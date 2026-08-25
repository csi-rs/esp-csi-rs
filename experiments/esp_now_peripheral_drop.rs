//! Packet drop rate test — receiver (peripheral side).
//!
//! ESP-NOW adaptation of `docs/packet_drop_rate_test_spec.md` (§3), using the
//! library's central/peripheral system instead of the spec's STA→AP UDP
//! link. The transport protocol details (UDP server, port 2223) are dropped;
//! the measurement contract is kept:
//!
//! - Gap accounting runs in the library's control-packet ingest path: for
//!   every valid control packet the gap to the previous `sequence_number` is
//!   computed in 64-bit signed arithmetic. `gap == 1` is in-order, `gap > 1`
//!   adds `gap − 1` to the missing tally, and `gap <= 0` (central reboot or
//!   reorder) resyncs silently — never registering ~2^32 drops (§3.1, §3.2).
//! - Invalid frames (parse/magic/source-filter failures) are ignored
//!   entirely: neither received nor missing — the analog of the spec's
//!   "datagrams shorter than 4 bytes".
//! - Once per second this example snapshots window deltas of the cumulative
//!   counters and prints exactly one line (§3.3):
//!   `DROP: <rate>% (received <r>, dropped <m> of <e> sent)` with
//!   `e = r + m`, so the metric stays correct even when the central's
//!   transmit rate drifts.
//! - CSI is not written to serial during a measurement run (§3.4); the drop
//!   figure comes entirely from the control-packet sequence numbers.
//!
//! Misses are attributed to the window where the gap is *observed* (when the
//! next packet finally arrives), so a burst loss lands in one window; during
//! a total outage windows report `0 of 0 sent` (spec §5).
//!
//! Run the matching `esp_now_central_drop` example on the transmitter, on
//! the same channel. Build with the `statistics` feature enabled.

#![no_std]
#![no_main]

#[cfg(not(feature = "statistics"))]
compile_error!("This experiment requires the `statistics` feature.");

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::peripheral::esp_now::{get_rx_control_packets, get_rx_sequence_miss_packets};
use esp_csi_rs::{
    CSINode, EspNowConfig, config::CsiConfig, logging::logging::init_logger,
};
use esp_csi_rs::{CSINodeHardware, log_ln, set_csi_logging_enabled};
#[cfg(feature = "statistics")]
use esp_csi_rs::get_total_rx_packets;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Wi-Fi channel — must match `esp_now_central_drop` (spec §1).
const CHANNEL: u8 = 1;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// Once per second, snapshot window deltas of the cumulative tallies and
/// print exactly one `DROP:` line (spec §3.3), followed by a `CSI:` sanity
/// line.
///
/// The drop figure is the *CSI drop*: `received` counts valid control packets,
/// each of which rode in a broadcast frame that also produced exactly one CSI
/// record, so the received tally is the unique-CSI count and the sequence gaps
/// are CSI losses. The separate `CSI: <n>` line reports the window delta of the
/// independent CSI-record counter (`get_total_rx_packets`) so post-processing
/// can confirm CSI count ≈ `received` (any divergence would be parse/magic
/// failures — frames that produced CSI but no valid control packet). It is a
/// distinct line so the `$`-anchored `DROP:` parser is unaffected.
async fn drop_report_task() {
    let mut last_rx = get_rx_control_packets();
    let mut last_miss = get_rx_sequence_miss_packets();
    let mut last_csi = get_total_rx_packets();
    log_ln!("drop receiver listening (esp-now peripheral).");
    loop {
        Timer::after_secs(1).await;
        let rx = get_rx_control_packets();
        let miss = get_rx_sequence_miss_packets();
        let csi = get_total_rx_packets();
        let r = rx.saturating_sub(last_rx);
        let m = miss.saturating_sub(last_miss);
        let c = csi.saturating_sub(last_csi);
        last_rx = rx;
        last_miss = miss;
        last_csi = csi;

        // e = r + m: how many the central must have sent across the span.
        let e = r + m;
        // rate = 100 × m / e, two decimal places; 0.00 when e == 0.
        // Computed in integer hundredths to stay float-free.
        let rate_x100 = if e == 0 { 0 } else { (m * 10_000 + e / 2) / e };
        log_ln!(
            "DROP: {}.{:02}% (received {}, dropped {} of {} sent)",
            rate_x100 / 100,
            rate_x100 % 100,
            r,
            m,
            e
        );
        // CSI cross-check: unique CSI records captured this window (≈ received).
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
    // No CSI on serial during a measurement run (spec §3.4) — the drop
    // figure comes entirely from the sequence numbers.
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Starting drop-rate receiver (EspNow Peripheral)");

    // Raise Wi-Fi buffer budget for sustained ESP-NOW RX traffic.
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
        esp_csi_rs::NodeRole::Peripheral(esp_csi_rs::PeripheralOpMode::EspNow(
            EspNowConfig::default().with_channel(CHANNEL),
        )),
        Some(CsiConfig::default()),
        Some(10000),
        csi_hardware,
    );
    // `CollectionMode::Listener` became this: keep the radio capturing, and its
    // timing intact, but deliver nothing off-device.
    node.set_csi_output_enabled(false);
    node.set_protocol(esp_radio::wifi::Protocol::N);
    // One-way link: receive and tally only, no replies back to the central.
    node.set_tx_enabled(false);

    join(node.run(), drop_report_task()).await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
