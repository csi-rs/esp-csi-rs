//! Dedicated test for `set_csi_callback` AND `CSINodeClient::next_csi_packet`.
//!
//! The crate exposes two CSI delivery paths controlled by
//! [`esp_csi_rs::CsiDeliveryMode`] — **exactly one is active at a time**
//! so the WiFi callback never pays for both:
//!
//! 1. **Inline callback** (`set_csi_callback`, mode = `Callback`) —
//!    runs in the WiFi-task callback. Lowest possible latency, must be
//!    fast and non-blocking. Lowest per-packet overhead.
//!
//! 2. **Async drain** (`CSINodeClient::next_csi_packet().await`,
//!    mode = `Async`) — pops packets from a lock-free MPMC queue on a
//!    user-spawned task. No critical section on enqueue. Lets you do
//!    heavier work without landing it on the WiFi task. Pays a 640 B
//!    memcpy per packet for the queue copy.
//!
//! 3. **Off** — neither user path fires; only the inline-log path
//!    (gated by `set_csi_logging_enabled`) is reachable.
//!
//! Switch at runtime with [`esp_csi_rs::set_csi_delivery_mode`]. This
//! example demonstrates both paths by running two phases back-to-back
//! and reporting the per-mode rate.
//!
//! Runs in WiFi-sniffer mode so no peer is required.
//!
//! What a packet carries: `packet.envelope` names the node, the session and the frame's place in
//! the run (`stream_seq`, so a gap is a lost frame); `packet.meta()` is the normalised receive
//! metadata (`timestamp_us` never wraps); and `packet.frame.payload` is the CSI itself, tagged with
//! the `LayoutId` that maps each `(imag, real)` pair to a subcarrier index. With the `statistics`
//! feature the stats line also breaks drops down by cause and lists the transmitters heard.
//!
//! Constraints:
//!   - Inline callback runs on the WiFi task hot path. **Keep it fast.**
//!     No heap allocation, no `Mutex` locks, no UART writes.
//!   - Async drain has **one** consumer slot (`AtomicWaker`). Spawn
//!     exactly one drainer task per node.

#![no_std]
#![no_main]

use core::sync::atomic::Ordering;

use embassy_executor::Spawner;
use embassy_futures::join::{join, join3};
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CsiPacket;
use esp_csi_rs::wire::CsiPayload;
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{config::CsiConfig, CsiDeliveryMode, CSINode, logging::logging::init_logger, WifiSnifferConfig};
use esp_csi_rs::{CSINodeClient, log_ln, NodeHardware, set_csi_callback, set_csi_delivery_mode, set_csi_logging_enabled};
#[cfg(feature = "statistics")]
use esp_csi_rs::get_dropped_packets_rx;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use portable_atomic::{AtomicI32, AtomicU32, AtomicU64};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]


/// Atomics published from the inline CSI callback and read by `stats_task`.
/// Atomic-only access keeps the callback wait-free relative to the stats
/// task — no critical sections, no contention with the WiFi-task ISR path.
static CSI_CB_COUNT: AtomicU32 = AtomicU32::new(0);
static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
/// Sum of `csi_data` magnitudes for the most recent packet — a tiny
/// example aggregate. Stored as `u64` so even very long CSI payloads
/// don't wrap.
static LATEST_TONE_ENERGY: AtomicU64 = AtomicU64::new(0);
static LATEST_TONE_COUNT: AtomicU32 = AtomicU32::new(0);
/// The envelope's per-run frame counter and the 64-bit receive time of the latest packet.
static LATEST_STREAM_SEQ: AtomicU32 = AtomicU32::new(0);
static LATEST_TIMESTAMP_US: AtomicU64 = AtomicU64::new(0);
/// Subcarriers the latest drained packet's layout maps, and its lowest subcarrier index.
static LATEST_MAPPED: AtomicU32 = AtomicU32::new(0);
static LATEST_LOWEST_SC: AtomicI32 = AtomicI32::new(0);

/// Counter incremented from the async drainer task. The delta between
/// `CSI_CB_COUNT` (inline) and `CSI_DRAIN_COUNT` (async) measures
/// how many packets the drainer is dropping behind by — should be 0
/// in steady state for a fast drainer.
static CSI_DRAIN_COUNT: AtomicU32 = AtomicU32::new(0);
/// Sum of `csi_data` magnitudes computed in the **async drainer** —
/// same calculation as `LATEST_TONE_ENERGY` but on the user task, not
/// the WiFi callback. Demonstrates moving heavier processing off the
/// WiFi hot path.
static LATEST_DRAIN_ENERGY: AtomicU64 = AtomicU64::new(0);

/// On-device CSI processing hook.
///
/// Runs inline in the WiFi task callback. Must be fast and non-blocking
/// — no heap allocation, no locking, no UART writes. Reads/writes only
/// to atomics or stack memory.
fn on_csi(packet: &CsiPacket) {
    LATEST_RSSI.store(packet.rssi() as i32, Ordering::Relaxed);
    LATEST_STREAM_SEQ.store(packet.envelope.stream_seq, Ordering::Relaxed);
    LATEST_TIMESTAMP_US.store(packet.meta().timestamp_us, Ordering::Relaxed);
    CSI_CB_COUNT.fetch_add(1, Ordering::Relaxed);

    // Demonstrate bounded inline math: sum |I| + |Q| across all CSI tones.
    // CSI data is interleaved I/Q pairs of `i8`; absolute-value sum is a
    // crude amplitude proxy good enough to show "callback can do real
    // work" without dragging in `f32` or heap.
    let mut energy: u64 = 0;
    let tones = packet.csi_data().len();
    for sample in packet.csi_data().iter() {
        energy = energy.wrapping_add((*sample as i32).unsigned_abs() as u64);
    }
    LATEST_TONE_ENERGY.store(energy, Ordering::Relaxed);
    LATEST_TONE_COUNT.store(tones as u32, Ordering::Relaxed);
}

/// Async CSI drainer.
///
/// Pops packets from the lock-free CSI queue via `next_csi_packet().await`
/// — no critical section on the WiFi-callback enqueue, so this task can
/// be as slow as it wants without delaying the WiFi-task hot path.
/// The first `await` here lazily opens the master publish gate and the
/// async-delivery gate, so the WiFi callback starts enqueueing.
async fn csi_drainer(client: &mut CSINodeClient) {
    loop {
        let packet = client.next_csi_packet().await;
        CSI_DRAIN_COUNT.fetch_add(1, Ordering::Relaxed);
        // Mirror the same per-packet math the inline callback does, but
        // here it lives on the user task rather than the WiFi task. In a
        // real app this is where ML inference, file logging, or anything
        // that allocates / waits would go.
        let mut energy: u64 = 0;
        for sample in packet.csi_data().iter() {
            energy = energy.wrapping_add((*sample as i32).unsigned_abs() as u64);
        }
        LATEST_DRAIN_ENERGY.store(energy, Ordering::Relaxed);
        LATEST_STREAM_SEQ.store(packet.envelope.stream_seq, Ordering::Relaxed);
        LATEST_TIMESTAMP_US.store(packet.meta().timestamp_us, Ordering::Relaxed);

        // Map the buffer onto subcarriers. `Unknown` means the layout has no published table or
        // a training field was disabled; the bytes are still delivered, just not mapped.
        if let CsiPayload::EspRaw { layout, .. } = &packet.frame.payload {
            let mut mapped = 0u32;
            let mut lowest = i16::MAX;
            for (_, index) in layout.indices() {
                mapped += 1;
                lowest = lowest.min(index);
            }
            LATEST_MAPPED.store(mapped, Ordering::Relaxed);
            LATEST_LOWEST_SC.store(if mapped > 0 { lowest as i32 } else { 0 }, Ordering::Relaxed);
        }
    }
}

/// Reads the atomics published by `on_csi` and `csi_drainer` and logs
/// them once per second. `cb` is the inline-callback count, `drain` is
/// the async-drainer count. Exactly one is non-zero at a time —
/// they're mutually exclusive per [`CsiDeliveryMode`].
async fn stats_task() {
    let mut last_cb = CSI_CB_COUNT.load(Ordering::Relaxed);
    let mut last_drain = CSI_DRAIN_COUNT.load(Ordering::Relaxed);
    loop {
        Timer::after_secs(1).await;
        let cb = CSI_CB_COUNT.load(Ordering::Relaxed);
        let drain = CSI_DRAIN_COUNT.load(Ordering::Relaxed);
        let cb_delta = cb.wrapping_sub(last_cb);
        let drain_delta = drain.wrapping_sub(last_drain);
        last_cb = cb;
        last_drain = drain;
        #[cfg(feature = "statistics")]
        let dropped = get_dropped_packets_rx();
        #[cfg(not(feature = "statistics"))]
        let dropped = 0u32;
        log_ln!(
            "stream_seq: {}, timestamp_us: {}, layout maps {} subcarriers from {}",
            LATEST_STREAM_SEQ.load(Ordering::Relaxed),
            LATEST_TIMESTAMP_US.load(Ordering::Relaxed),
            LATEST_MAPPED.load(Ordering::Relaxed),
            LATEST_LOWEST_SC.load(Ordering::Relaxed),
        );
        #[cfg(feature = "statistics")]
        {
            let d = esp_csi_rs::get_drop_breakdown();
            log_ln!(
                "drops: queue full {}, oversize {}, on-air gaps {}; withheld: filtered {}, policy {}",
                d.queue_full,
                d.oversize,
                d.seq_gap,
                d.filtered,
                d.policy_suppressed,
            );
            let mut tx = [esp_csi_rs::TxStats::default(); 4];
            let n = esp_csi_rs::snapshot_tx_stats(&mut tx);
            for t in &tx[..n] {
                log_ln!(
                    "  tx {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} frames {} gaps {} retries {}",
                    t.mac[0], t.mac[1], t.mac[2], t.mac[3], t.mac[4], t.mac[5],
                    t.frames, t.seq_gaps, t.retries
                );
            }
        }
        log_ln!(
            "mode={:?} cb/sec: {}, drain/sec: {}, dropped: {}, RSSI: {} dBm, tones: {}, cb_energy: {}, drain_energy: {}",
            esp_csi_rs::csi_delivery_mode(),
            cb_delta,
            drain_delta,
            dropped,
            LATEST_RSSI.load(Ordering::Relaxed),
            LATEST_TONE_COUNT.load(Ordering::Relaxed),
            LATEST_TONE_ENERGY.load(Ordering::Relaxed),
            LATEST_DRAIN_ENERGY.load(Ordering::Relaxed),
        );
    }
}

/// Demonstrates runtime toggling between the two delivery modes.
/// Spends 10 s in `Callback` mode then 10 s in `Async` mode and
/// repeats. The stats line shows which counter ticks during each
/// window — a quick way to verify the toggle works.
async fn mode_switcher() {
    loop {
        log_ln!("--- switching to CsiDeliveryMode::Callback ---");
        set_csi_delivery_mode(CsiDeliveryMode::Callback);
        Timer::after_secs(10).await;
        log_ln!("--- switching to CsiDeliveryMode::Async ---");
        set_csi_delivery_mode(CsiDeliveryMode::Async);
        Timer::after_secs(10).await;
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);

    // Suppress the per-packet UART CSI dump that `init_logger` enables —
    // we only want our `log_ln!` output plus what the inline callback
    // does. `set_csi_callback` will re-enable the gate so the callback
    // still fires.
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting CSI callback test (sniffer mode)");

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let wifi_controller = esp_radio::wifi::WifiController::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = NodeHardware::new(controller);
    let mut node = CSINode::sniffer(
        WifiSnifferConfig::default(),
        Some(CsiConfig::default()),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::LR);

    // Register the inline CSI processing hook. `set_csi_callback`
    // sets the delivery mode to `Callback` automatically. The
    // `mode_switcher` task below flips it between `Callback` and
    // `Async` every 10 s to exercise both paths.
    set_csi_callback(on_csi);

    // We need the drainer task running so when the mode flips to
    // `Async` someone is dequeueing — otherwise the queue would just
    // fill and drop. The drainer's `next_csi_packet().await` parks on
    // the waker while the mode is `Callback` and resumes when packets
    // start landing in the queue.
    join(
        node.run(),
        join3(csi_drainer(&mut node_handle), mode_switcher(), stats_task()),
    )
    .await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
