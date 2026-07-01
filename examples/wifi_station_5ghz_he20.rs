//! Wi-Fi 6 **HE20** station — serialized CSI output (RX side of the pair).
//!
//! Associates to SSID `esp-csi-ap` (open auth), runs DHCP, then pings the gateway
//! so the AP ([`wifi_ap_5ghz_he20`]) stays busy; this node captures the AP's
//! **downlink** HE20 PPDUs as CSI.
//!
//! Output is postcard + COBS serialized via [`LogMode::Serialized`] — **not**
//! human-readable. Record it with `tools/he20_csi_recorder.py --schema v1 --chip c5`
//! on a separate terminal (it prints a live rate to stderr).
//!
//! IMPORTANT — do **not** register `set_csi_callback` here, and do **not** print
//! text on this UART: a callback puts delivery in `Callback` mode (bypassing the
//! serialized `log_csi` path), and text bytes corrupt the binary COBS stream.
//! Delivery stays in `Off` mode so the WiFi callback routes each packet through
//! `log_csi`. The STA does not pin a channel — it scans and associates to the
//! 5 GHz AP once AX is enabled on the 5 GHz band (crate handles this for STA).
//!
//! For throughput (~490 B/packet) build with async logging over USB-Serial-JTAG:
//!   cargo esp32c5 --example wifi_station_5ghz_he20 --features async-print

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeHardware, CentralOpMode, CollectionMode, Node, WifiStationConfig,
    config::CsiConfig, log_ln,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{PowerSaveMode, Protocol, WifiController};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Must match `wifi_ap_5ghz_he20` (or your own Wi-Fi 6 AP).
const SSID: &str = "esp-csi-ap";
/// Gateway ping rate (Hz) — keeps uplink traffic flowing for the AP collector.
const PING_RATE_HZ: u16 = 4000;

// Wi-Fi 6 parts request 802.11ax; Wi-Fi 4 parts fall back to 802.11n.
#[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
const PROTOCOL: Protocol = Protocol::AX;
#[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
const PROTOCOL: Protocol = Protocol::N;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    // Serialized CSI (COBS-framed postcard). `init_logger` opens the CSI gates.
    init_logger(spawner, LogMode::Serialized);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("HE20 station (serialized) — SSID {}, {:?}", SSID, PROTOCOL);

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);
    let _ = controller.set_power_saving(PowerSaveMode::None);

    let client_config = StationConfig::default()
        .with_ssid(SSID)
        .with_auth_method(esp_radio::wifi::AuthenticationMethod::None);
    let station_config = WifiStationConfig { client_config };

    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    let mut node = CSINode::new(
        Node::Central(CentralOpMode::WifiStation(station_config)),
        CollectionMode::Collector,
        // Default acquires legacy + HT + HE-SU/MU → CSI flows for any PPDU format.
        Some(CsiConfig::default()),
        Some(PING_RATE_HZ),
        csi_hardware,
    );
    node.set_protocol(PROTOCOL);

    // No `set_csi_callback` — delivery stays `Off` so the WiFi callback routes each
    // packet through `log_csi`, which writes the COBS-framed postcard bytes.
    node.run().await;

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}
