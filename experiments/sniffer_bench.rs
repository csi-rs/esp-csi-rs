//! **Sniffer measurement harness** — heap, throughput and log-format cost.
//!
//! A peripheral collector locked to `CHANNEL`, reporting one line a second. Replaces the four
//! single-purpose sniffer harnesses (`sniffer_wifi_exper{,_heap,_esp_csi_tool,_logmode_cycle}`),
//! which differed only in `MEASURE` and `LOG_MODE`.
//!
//! `LOG_MODE` is a measurement variable in its own right: the log format decides how many bytes
//! leave the device per captured frame, and on a busy channel the console, not the radio, is what
//! saturates first. `EspCsiTool` emits the ESP32-CSI-Tool CSV layout for comparison against that
//! tool's own numbers; `Serialized` is the compact postcard/COBS framing.
//!
//! The footprint half is `sniffer_bench_min` — same platform and radio bring-up, no `CSINode`.
//! Build both with the same features.
//!
//! Build:
//!   cargo esp32c6-build --example sniffer_bench --features=statistics

#![no_std]
#![no_main]

#[cfg(not(feature = "statistics"))]
compile_error!("This harness requires the `statistics` feature.");

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeClient, NodeHardware, WifiSnifferConfig, config::CsiConfig, log_ln,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::{get_dropped_packets_rx, get_pps_rx, get_total_rx_packets};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

// ── Knobs ───────────────────────────────────────────────────────────────────────────────────────

const CHANNEL: u8 = 11;

#[allow(dead_code, reason = "selected by the `MEASURE` const")]
#[derive(PartialEq, Eq)]
enum Measure {
    /// RX rate, totals and the sequence-gap drop count.
    Throughput,
    /// Heap-free, sampled once a second.
    Heap,
    /// Quiet — for runs where the instrument is external.
    Quiet,
}
const MEASURE: Measure = Measure::Throughput;

/// The output format under test. See the module docs — this is a measured variable.
const LOG_MODE: LogMode = LogMode::Text;

// ────────────────────────────────────────────────────────────────────────────────────────────────

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

async fn report_task(_client: &mut CSINodeClient) {
    let mut last_rx_total = get_total_rx_packets();
    loop {
        Timer::after_secs(1).await;
        match MEASURE {
            Measure::Throughput => {
                let rx_total = get_total_rx_packets();
                log_ln!(
                    "RX: {}, RX PPS: {}, dropped: {}",
                    rx_total.saturating_sub(last_rx_total),
                    get_pps_rx(),
                    get_dropped_packets_rx(),
                );
                last_rx_total = rx_total;
            }
            Measure::Heap => log_ln!("heap free: {}", esp_alloc::HEAP.free()),
            Measure::Quiet => {}
        }
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LOG_MODE);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 60000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let wifi_controller = esp_radio::wifi::WifiController::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    log_ln!("Sniffer bench — channel {}", CHANNEL);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = NodeHardware::new(controller);
    let mut node = CSINode::sniffer(
        WifiSnifferConfig::default().with_channel(CHANNEL),
        Some(CsiConfig::default()),
        csi_hardware,
    );
    // Deliberately no `set_protocol`: on the C5 the broadcast PHY is not forced, so frames can be
    // legacy/11g, and pinning N here would filter out exactly what a sniffer is meant to overhear.
    let _ = &mut node;

    join(node.run(), report_task(&mut node_handle)).await;

    loop {
        log_ln!("bench stopped");
        Timer::after(Duration::from_secs(5)).await;
    }
}
