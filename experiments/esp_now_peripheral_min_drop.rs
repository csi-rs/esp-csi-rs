//! Packet drop rate test — **minimal** receiver (raw ESP-NOW ingest, no
//! `CSINode` framing), the platform-floor counterpart to esp-csi's bare
//! `espnow_rx_drop`.
//!
//! It reuses the library node only to bring up the radio and engage CSI capture
//! (so the CSI workload is present and fair), but strips the two per-frame costs
//! the C reference does not pay:
//!
//!   1. **ESP-NOW ingest** — [`set_raw_listen`]`(true)` discards frames inline in
//!      the receive callback (no `ingest_control_packet`: no postcard parse,
//!      magic/source checks, mode hysteresis). A [`set_raw_recv_callback`] hook
//!      hands the raw payload to `raw_recv_cb` *inline* so we read the 4-byte
//!      sequence number and do gap accounting — the match to the C `recv_cb`.
//!   2. **CSI delivery** — [`set_csi_raw_callback`] counts each CSI record and
//!      returns without building the ~640 B `CSIDataPacket` — the match to the C
//!      `csi_cb`.
//!
//! Each received broadcast frame is exactly one CSI opportunity, so the
//! sequence-gap tally **is** the CSI drop. Once per second:
//!   `DROP: <rate>% (received <r>, dropped <m> of <e> sent)`
//!   `CSI: <n>`   (CSI-record count this window; cross-check, ~= received)
//!
//! Running this against `esp_now_peripheral_drop` isolates the `CSINode`
//! state-machine cost; running it against esp-csi's `espnow_rx_drop` is the fair
//! platform comparison. Companion TX: `esp_now_central_min_drop`.
//!
//! Build: `cargo build --release --example esp_now_peripheral_min_drop --features <chip>,async-print`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    CSINode, CollectionMode, EspNowConfig, IOTaskConfig, config::CsiConfig,
    logging::logging::init_logger, set_raw_listen, set_raw_recv_callback,
};
use esp_csi_rs::{CSINodeHardware, log_ln, set_csi_logging_enabled, set_csi_raw_callback};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::WifiController;
use portable_atomic::{AtomicBool, AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Wi-Fi channel — must match `esp_now_central_min_drop`.
const CHANNEL: u8 = 1;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

// --- Window counters (lock-free; touched from the WiFi-task receive/CSI cbs). ---
static RX_RECV: AtomicU32 = AtomicU32::new(0); // frames with a valid 4-byte seq
static RX_MISS: AtomicU32 = AtomicU32::new(0); // inferred CSI drops (seq gaps)
static CSI_COUNT: AtomicU32 = AtomicU32::new(0); // CSI records (cross-check)
// Last seen seq + first-frame guard. Only the (single) receive callback touches
// these, so plain relaxed u32/bool are sufficient and lock-free on xtensa.
static LAST_SEQ: AtomicU32 = AtomicU32::new(0);
static HAVE_LAST: AtomicBool = AtomicBool::new(false);

/// Inline raw-recv callback (matches the C `recv_cb`): read the big-endian
/// sequence from the first 4 bytes and gap-account. Runs in WiFi-task context.
fn raw_recv_cb(data: &[u8]) {
    if data.len() < 4 {
        return; // too short to carry a sequence number
    }
    let seq = ((data[0] as u32) << 24)
        | ((data[1] as u32) << 16)
        | ((data[2] as u32) << 8)
        | (data[3] as u32);

    RX_RECV.fetch_add(1, Ordering::Relaxed);

    if HAVE_LAST.load(Ordering::Relaxed) {
        // 64-bit signed gap so a sender reboot (seq → 0) or reorder resyncs
        // without registering ~2^32 drops.
        let gap = seq as i64 - LAST_SEQ.load(Ordering::Relaxed) as i64;
        if gap > 1 {
            RX_MISS.fetch_add((gap - 1) as u32, Ordering::Relaxed);
        }
    } else {
        HAVE_LAST.store(true, Ordering::Relaxed);
    }
    LAST_SEQ.store(seq, Ordering::Relaxed);
}

/// Minimal raw CSI callback (matches the C `csi_cb`): count only, no build.
fn csi_cb() {
    CSI_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Once per second, snapshot the window tallies and print the `DROP:` + `CSI:`
/// lines (same contract as the deployed receiver, spec §3.3).
async fn drop_report_task() -> ! {
    log_ln!("drop receiver listening (minimal raw esp-now peripheral).");
    loop {
        Timer::after(Duration::from_secs(1)).await;
        let r = RX_RECV.swap(0, Ordering::Relaxed);
        let m = RX_MISS.swap(0, Ordering::Relaxed);
        let c = CSI_COUNT.swap(0, Ordering::Relaxed);

        // e = r + m: how many the central must have sent across the span.
        let e = r + m;
        // rate = 100 × m / e, two decimals (integer hundredths); 0.00 when e==0.
        let rate_x100 = if e == 0 { 0 } else { (m * 10_000 + e / 2) / e };
        log_ln!(
            "DROP: {}.{:02}% (received {}, dropped {} of {} sent)",
            rate_x100 / 100,
            rate_x100 % 100,
            r,
            m,
            e
        );
        log_ln!("CSI: {}", c);
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Starting MINIMAL drop-rate receiver (raw ESP-NOW ingest, no CSINode framing)");

    // Same RX-buffer budget as the deployed `esp_now_peripheral_drop`, so the
    // min-vs-deployed comparison differs only in the per-frame path.
    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_static_rx_buf_num(25)
        .with_dynamic_rx_buf_num(128)
        .with_ampdu_rx_enable(false)
        .with_rx_queue_size(32);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::Node::Peripheral(esp_csi_rs::PeripheralOpMode::EspNow(
            EspNowConfig::default()
                .with_channel(CHANNEL)
                .with_phy_rate(WifiPhyRate::RateMcs0Lgi),
        )),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(10000),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    // Listen-only ESP-NOW peer: RX enabled, TX disabled.
    node.set_io_tasks(IOTaskConfig::new(false, true));

    // The flags that make this the fair, minimal receiver — set BEFORE the node
    // starts so no frame is ever ingested and no CSIDataPacket is ever built:
    set_raw_listen(true); // discard frames inline, skip ingest_control_packet
    set_raw_recv_callback(raw_recv_cb); // but read the seq inline (gap accounting)
    set_csi_raw_callback(csi_cb); // count CSI, skip the 640 B CSIDataPacket build

    join(node.run(), drop_report_task()).await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
