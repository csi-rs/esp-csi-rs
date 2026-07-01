//! Wi-Fi station — DHCP client that pings its gateway for steady traffic.
//!
//! Companion to `wifi_ap`. Associates to SSID `esp-csi-ap` (open auth), runs
//! DHCP, then pings the gateway at 4 kHz so the AP side (`wifi_ap`) captures
//! uplink CSI. Can also join any other AP by changing `SSID` / auth below.
//!
//! Build / run (optional throughput counters need `--features statistics`):
//!   cargo esp32c6 --example wifi_station
//!   cargo esp32s3 --example wifi_station

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeHardware, CentralOpMode, CollectionMode, Node, WifiStationConfig,
    config::CsiConfig, log_ln, set_csi_callback,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::{
    get_dropped_packets_rx, get_pps_rx, get_pps_tx, get_rx_rate_hz, get_tx_rate_hz,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::{PowerSaveMode, WifiController};
use esp_radio::wifi::sta::StationConfig;
use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Must match `wifi_ap` (or your own AP).
const SSID: &str = "esp-csi-ap";
/// Gateway ping rate (Hz) — drives uplink traffic for the AP CSI collector.
const PING_RATE_HZ: u16 = 4000;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
static CSI_PKT_COUNT: AtomicU32 = AtomicU32::new(0);

fn on_csi(packet: &CSIDataPacket) {
    LATEST_RSSI.store(packet.rssi as i32, Ordering::Relaxed);
    CSI_PKT_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[embassy_executor::task]
async fn stats_task() {
    loop {
        Timer::after_secs(1).await;

        #[cfg(feature = "statistics")]
        {
            log_ln!(
                "RX PPS(avg): {}, TX PPS(avg): {}, RX Rate(Hz): {}, TX Rate(Hz): {}, RX Dropped: {}, CSI Packets: {}, Latest RSSI: {}",
                get_pps_rx(),
                get_pps_tx(),
                get_rx_rate_hz(),
                get_tx_rate_hz(),
                get_dropped_packets_rx(),
                CSI_PKT_COUNT.load(Ordering::Relaxed),
                LATEST_RSSI.load(Ordering::Relaxed),
            );
        }

        #[cfg(not(feature = "statistics"))]
        {
            log_ln!(
                "CSI Packets: {}, Latest RSSI: {}",
                CSI_PKT_COUNT.load(Ordering::Relaxed),
                LATEST_RSSI.load(Ordering::Relaxed),
            );
        }
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!(
        "Starting Wi-Fi station — SSID {}, {} Hz gateway ping",
        SSID,
        PING_RATE_HZ
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);
    let _ = controller.set_power_saving(PowerSaveMode::None);

    let client_config = StationConfig::default()
        .with_ssid(SSID)
        .with_auth_method(esp_radio::wifi::AuthenticationMethod::None);

    let station_config = WifiStationConfig::new(client_config);
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    let mut node = CSINode::new(
        Node::Central(CentralOpMode::WifiStation(station_config)),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(PING_RATE_HZ),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);

    set_csi_callback(on_csi);
    spawner.spawn(stats_task().unwrap());
    node.run().await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
