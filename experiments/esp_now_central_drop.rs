//! Packet drop rate test — transmitter (central side).
//!
//! ESP-NOW adaptation of `docs/packet_drop_rate_test_spec.md` (§2), using the
//! library's central/peripheral system instead of the spec's STA→AP UDP
//! link. The transport protocol details (UDP, port 2223, 4-byte datagrams)
//! are dropped; the measurement contract is kept:
//!
//! - Every control packet the central sends already carries a 32-bit
//!   `sequence_number`, incremented only when the packet was actually queued
//!   to the driver — a failed send retries the *same* number, so local send
//!   failures never inflate the receiver's drop count (§2.2).
//! - The counter is monotonic for the life of the program; only a reboot
//!   restarts it at 0, which the peripheral treats as a resync.
//! - Pacing at `PACKET_RATE` Hz is handled by the central TX loop's
//!   µs-deadline schedule (§2.3).
//! - Once per second the achieved rate is printed as `TX: <n>` — a sanity
//!   channel only; the drop computation never consumes it (§2.4).
//!
//! PHY rate is pinned to MCS0 long GI and the channel is fixed; run the
//! matching `esp_now_peripheral_drop` example on the receiver.
//!
//! Build with the `statistics` feature enabled.

#![no_std]
#![no_main]

#[cfg(not(feature = "statistics"))]
compile_error!("This experiment requires the `statistics` feature.");

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    CSINode, EspNowConfig, config::CsiConfig, logging::logging::init_logger,
};
use esp_csi_rs::{
    CSINodeHardware, log_ln, set_csi_logging_enabled, set_test_tx_paused,
    set_test_tx_payload_b, set_test_tx_rate_hz,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::get_total_tx_packets;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Wi-Fi channel — must match `esp_now_peripheral_drop` (spec §1).
const CHANNEL: u8 = 1;
/// Initial/cap control-packet rate; the actual offered rate is ramped at runtime
/// via the `cpu-test-tx` hook (see `ramp_and_report_task`), so this is just the
/// node-construction seed.
const PACKET_RATE: u16 = 10000;
/// On-air payload size, synced with the other implementations (the `cpu-test-tx`
/// path pads the `ControlPacket` frame up to this length).
const PAYLOAD_B: u16 = 8;

/// Offered-rate ramp: 500 → 5000 pps in 500-pps steps, 30 s dwell each
/// (~300 s) — traces the drop-vs-offered-rate knee in one run. Identical
/// schedule across all four drop transmitters.
const RAMP_STEP_HZ: u64 = 500;
const RAMP_MAX_HZ: u64 = 5000;
const RAMP_DWELL_S: u64 = 30;

/// Offered rate (Hz) for a given elapsed time: `500 * (elapsed/30 + 1)`, capped
/// at 5000 (held thereafter).
fn ramp_target_hz(elapsed_s: u64) -> u64 {
    (RAMP_STEP_HZ * (elapsed_s / RAMP_DWELL_S + 1)).min(RAMP_MAX_HZ)
}

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// Drives the offered-rate ramp via the `cpu-test-tx` runtime hooks (the CSINode
/// central loop reads `TEST_TX_RATE_HZ` each iteration) and, once per second,
/// prints the achieved send rate + current setpoint: `TX: <n> <target_hz>`.
async fn ramp_and_report_task() {
    // Fixed 8-byte on-air payload, unpaused; rate is stepped below.
    set_test_tx_payload_b(PAYLOAD_B);
    set_test_tx_paused(false);
    let mut last_tx_total = get_total_tx_packets();
    let mut sec: u64 = 0;
    loop {
        let target = ramp_target_hz(sec);
        set_test_tx_rate_hz(target as u16);
        Timer::after_secs(1).await;
        let tx_total = get_total_tx_packets();
        let tx_delta = tx_total.saturating_sub(last_tx_total);
        last_tx_total = tx_total;
        log_ln!("TX: {} {}", tx_delta, target);
        sec += 1;
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
    // TX-only node: keep the CSI publish gate closed so received frames
    // don't CPU-spin the UART and disturb the pacing.
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Starting drop-rate transmitter (EspNow Central)");

    // Raise the Wi-Fi TX buffer budget for sustained paced ESP-NOW traffic.
    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_static_tx_buf_num(25)
        .with_dynamic_tx_buf_num(128)
        .with_ampdu_tx_enable(false)
        .with_tx_queue_size(32);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::NodeRole::Central(esp_csi_rs::CentralOpMode::EspNow(
            EspNowConfig::default().with_channel(CHANNEL),
        )),
        Some(CsiConfig::default()),
        Some(PACKET_RATE),
        csi_hardware,
    );
    // `CollectionMode::Listener` became this: keep the radio capturing, and its
    // timing intact, but deliver nothing off-device.
    node.set_csi_output_enabled(false);
    node.set_protocol(esp_radio::wifi::Protocol::N);
    // One-way link: no replies expected from the drop-test peripheral, so
    // skip the RX side. The offered rate is driven by the cpu-test-tx ramp.
    node.set_rx_enabled(false);
    // Pin the PHY rate so link conditions stay controlled and comparable
    // across runs (spec §1).
    node.set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi);

    log_ln!("sending numbered frames.");

    join(node.run(), ramp_and_report_task()).await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
