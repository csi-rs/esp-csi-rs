//! **Station measurement harness** — heap and power on an associated link.
//!
//! A station associated to `SSID`, pinging its gateway at `PING_RATE_HZ` so the link carries
//! traffic, reporting one line a second. Replaces `wifi_station_heap` and `wifi_station_power`,
//! which differed only in what they printed.
//!
//! The footprint half is `station_bench_min` — same platform and radio bring-up, no `CSINode`.
//! Build both with the same features.
//!
//! Build:
//!   cargo esp32c6-build --example station_bench --features=statistics

#![no_std]
#![no_main]

#[cfg(not(feature = "statistics"))]
compile_error!("This harness requires the `statistics` feature.");

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeClient, ReportingPolicy, NodeHardware, WifiStationConfig, config::CsiConfig,
    log_ln, set_csi_logging_enabled,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::{get_pps_rx, get_total_rx_packets};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{PowerSaveMode, WifiController};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

// ── Knobs ───────────────────────────────────────────────────────────────────────────────────────

const SSID: &str = "esp-csi-ap";

#[allow(dead_code, reason = "selected by the `MEASURE` const")]
#[derive(PartialEq, Eq)]
enum Measure {
    /// RX rate and totals.
    Throughput,
    /// Heap-free, sampled once a second.
    Heap,
    /// Quiet — the power run is measured at the supply, not over the console, and a per-second
    /// console write would be part of what the meter sees.
    Quiet,
}
const MEASURE: Measure = Measure::Heap;

/// Gateway ping rate (Hz).
const PING_RATE_HZ: u16 = 10_000;

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
                    "RX: {}, RX PPS: {}",
                    rx_total.saturating_sub(last_rx_total),
                    get_pps_rx(),
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
    init_logger(spawner, LogMode::Text);
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 60000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let wifi_controller = esp_radio::wifi::WifiController::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);
    let _ = controller.set_power_saving(PowerSaveMode::None);

    let client_config = StationConfig::default()
        .with_ssid(SSID.try_into().expect("SSID longer than 32 bytes"))
        .with_authentication(esp_radio::wifi::AuthenticationMethodConfig::Open);

    log_ln!("Station bench — SSID {}", SSID);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = NodeHardware::new(controller);
    let mut node = CSINode::station(
        // Acquisition cost without delivery cost.
        WifiStationConfig::new(client_config).with_reporting(ReportingPolicy::Never),
        Some(CsiConfig::default()),
        Some(PING_RATE_HZ),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);

    join(node.run(), report_task(&mut node_handle)).await;

    loop {
        log_ln!("bench stopped");
        Timer::after(Duration::from_secs(5)).await;
    }
}
