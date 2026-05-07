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
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    config::CsiConfig, logging::logging::init_logger, CSINode, CollectionMode, CsiDeliveryMode,
    WifiSnifferConfig,
};
use esp_csi_rs::{
    get_dropped_packets_rx, log_ln, set_csi_callback, set_csi_delivery_mode,
    set_csi_logging_enabled, CSINodeClient, CSINodeHardware,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::{
    wifi::{PowerSaveMode, WifiController},
    Controller,
};
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

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

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
fn on_csi(packet: &CSIDataPacket) {
    LATEST_RSSI.store(packet.rssi as i32, Ordering::Relaxed);
    CSI_CB_COUNT.fetch_add(1, Ordering::Relaxed);

    // Demonstrate bounded inline math: sum |I| + |Q| across all CSI tones.
    // CSI data is interleaved I/Q pairs of `i8`; absolute-value sum is a
    // crude amplitude proxy good enough to show "callback can do real
    // work" without dragging in `f32` or heap.
    let mut energy: u64 = 0;
    let tones = packet.csi_data.len();
    for sample in packet.csi_data.iter() {
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
        for sample in packet.csi_data.iter() {
            energy = energy.wrapping_add((*sample as i32).unsigned_abs() as u64);
        }
        LATEST_DRAIN_ENERGY.store(energy, Ordering::Relaxed);
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
        log_ln!(
            "mode={:?} cb/sec: {}, drain/sec: {}, dropped: {}, RSSI: {} dBm, tones: {}, cb_energy: {}, drain_energy: {}",
            esp_csi_rs::csi_delivery_mode(),
            cb_delta,
            drain_delta,
            get_dropped_packets_rx(),
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
    #[cfg(any(feature = "esp32c6", feature = "esp32c3"))]
    {
        let sw_interrupt =
            esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
        esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);
    }
    #[cfg(not(any(feature = "esp32c6", feature = "esp32c3")))]
    esp_rtos::start(timg0.timer0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting CSI callback test (sniffer mode)");

    let radio_init = mk_static!(
        Controller<'static>,
        esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller")
    );

    let config_radio =
        esp_radio::wifi::Config::default().with_power_save_mode(PowerSaveMode::None);
    let (wifi_controller, mut interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, config_radio)
            .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::Node::Peripheral(esp_csi_rs::PeripheralOpMode::WifiSniffer(
            WifiSnifferConfig::default(),
        )),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(1000),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::P802D11BGNLR);
    node.set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi);

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
