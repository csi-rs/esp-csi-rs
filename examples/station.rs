//! **Station** — associate to an access point and measure the link.
//!
//! | Attribute | Value |
//! |---|---|
//! | Operational mode | Wi-Fi station |
//! | Network role | `NETWORK_ROLE` below — both are meaningful |
//! | Reporting policy | `REPORTING` below — `Always`, `Never`, `Threshold` or `Decimate` |
//! | Session role | none — the run's controller is whatever calls `run()` |
//!
//! A station is **central** when the uplink it generates is the traffic being measured — the usual
//! case here, where it pings its gateway at `PING_RATE_HZ` so the AP has something to measure. It
//! is a **peripheral** when it only measures an already-busy link and sources nothing itself; set
//! that with `with_network_role` and drop the ping rate to `None`.
//!
//! Associates to an ESP softAP (`access_point`) or to any commercial router. On the dual-band
//! ESP32-C5, `with_channel_hint` pins the band before the scan — without it the node scans both,
//! which is slower but finds an AP on either.
//!
//! Build / run (pair with `access_point` on the same SSID):
//!   cargo esp32c6 --example station

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CsiPacket;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, ReportingPolicy, NetworkRole, NodeHardware, WifiStationConfig, config::CsiConfig,
    log_ln, set_csi_callback,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{PowerSaveMode, WifiController};
use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

// ── Knobs ───────────────────────────────────────────────────────────────────────────────────────

/// Must match `access_point` (or your own AP).
const SSID: &str = "esp-csi-ap";

/// Central if this node's uplink is the traffic being measured; peripheral if it only listens to a
/// link someone else keeps busy.
const NETWORK_ROLE: NetworkRole = NetworkRole::Central;

/// `Never` keeps the link busy without reporting anything — useful when the AP is the collector
/// and you want this node's delivery cost out of the measurement.
const REPORTING: ReportingPolicy = ReportingPolicy::Always;

/// Gateway ping rate (Hz) — the uplink an AP collector measures. Set `None` for a peripheral that
/// generates no traffic of its own.
const PING_RATE_HZ: Option<u16> = Some(4000);

/// Pin the radio band from the AP's primary channel (ESP32-C5 dual-band only).
const CHANNEL_HINT: Option<u8> = None;

// ────────────────────────────────────────────────────────────────────────────────────────────────

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
static CSI_PKT_COUNT: AtomicU32 = AtomicU32::new(0);

fn on_csi(packet: &CsiPacket) {
    LATEST_RSSI.store(packet.rssi() as i32, Ordering::Relaxed);
    CSI_PKT_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[embassy_executor::task]
async fn stats_task() {
    loop {
        Timer::after_secs(1).await;
        log_ln!(
            "CSI Packets: {}, Latest RSSI: {}",
            CSI_PKT_COUNT.load(Ordering::Relaxed),
            LATEST_RSSI.load(Ordering::Relaxed),
        );
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

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

    let mut station_config = WifiStationConfig::new(client_config)
        .with_network_role(NETWORK_ROLE)
        .with_reporting(REPORTING);
    if let Some(channel) = CHANNEL_HINT {
        station_config = station_config.with_channel_hint(channel);
    }

    log_ln!("Starting station on SSID {}", SSID);

    let csi_hardware = NodeHardware::new(controller);
    let mut node = CSINode::station(
        station_config,
        Some(CsiConfig::default()),
        PING_RATE_HZ,
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);

    set_csi_callback(on_csi);
    spawner.spawn(stats_task().unwrap());
    node.run().await;

    loop {
        log_ln!("Station stopped");
        Timer::after(Duration::from_secs(5)).await;
    }
}
