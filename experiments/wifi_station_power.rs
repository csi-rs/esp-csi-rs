//! Power-test STA→AP DUT (esp-csi-rs side of the `staap_active` scenario).
//!
//! Associates to the AP and, when active, sends app traffic at a fixed rate to
//! elicit CSI — **silent CSI** (no per-packet serial), so the UM34C reads the
//! stack's transmit power, not the UART. Pair it with ESP32-CSI-Tool
//! `active_sta_power` on the **same AP / channel / rate**.
//!
//! One binary, two captures via `TX_ENABLED` (drives `IOTaskConfig.tx_enabled`,
//! which gates `sta_network_ops` — the STA's app-traffic sender):
//!   * `true`  → **active**: associated + sending at PACKET_RATE_HZ.
//!   * `false` → **idle baseline**: associated (connection + net + DHCP up), but
//!     no app traffic — the fair "associated, not sending" floor.
//!
//! Unlike the stock `wifi_station`, this never sends a stop — it stays associated
//! for the whole power window. Flash with `TX_ENABLED = true`, capture →
//! `UM34C_staap_esp32_active.csv`; flip to `false`, re-flash →
//! `UM34C_staap_esp32_idle.csv`. Build: `--features=esp32,async-print`.

#![no_std]
#![no_main]

use crate::alloc::string::ToString;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{CollectorMode, config::CsiConfig, CSINode, CSINodeClient, IOTaskConfig, log_ln, NodeHardware, set_csi_logging_enabled, WifiStationConfig};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use esp_radio::wifi::sta::StationConfig;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// AP credentials — must be the SAME AP the ESP32-CSI-Tool DUT associates to.
const WIFI_SSID: &str = "myssid";
const WIFI_PASS: &str = "mypassword";
/// Fixed offered app-traffic rate (Hz) — must match `active_sta_power`'s
/// `CONFIG_PACKET_RATE` so the on-air load is identical across the pair.
const PACKET_RATE_HZ: u16 = 1000;
/// `true` = active (associated + sending); `false` = idle baseline (associated,
/// no app traffic).
const TX_ENABLED: bool = true;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// Slow liveness heartbeat only — no per-packet serial (silent CSI).
async fn heartbeat_task() -> ! {
    loop {
        Timer::after(Duration::from_secs(30)).await;
        log_ln!("alive (tx_enabled={})", TX_ENABLED);
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);
    // Silent CSI: init_logger turns the inline per-packet log gate ON; the STA
    // *receives* CSI (collector), so without this every CSI packet would print
    // to serial. Close the gate — the power meter must see the radio, not UART.
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!(
        "Power test — STA->AP DUT, tx_enabled={}, rate={} Hz, ssid={}",
        TX_ENABLED,
        PACKET_RATE_HZ,
        WIFI_SSID
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let client_config = StationConfig::default()
        .with_ssid(WIFI_SSID)
        .with_password(WIFI_PASS.to_string())
        .with_auth_method(esp_radio::wifi::AuthenticationMethod::Wpa2Personal);
    let station_config = WifiStationConfig::new(client_config);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = NodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new_collector(
        CollectorMode::Station(station_config),
        Some(CsiConfig::default()),
        Some(PACKET_RATE_HZ),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);

    // Active = associated + app traffic (tx_enabled gates `sta_network_ops`);
    // idle = associated, no app traffic. RX stays up either way.
    node.set_io_tasks(IOTaskConfig::new(TX_ENABLED, true));

    // CSI capture stays on but nothing prints it (silent) — measure the radio.
    // Never send a stop: run associated for the whole power window.
    let _ = &mut node_handle;
    join(node.run(), heartbeat_task()).await;
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
