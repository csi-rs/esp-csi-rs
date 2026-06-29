//! Footprint **min** — Wi-Fi STA platform floor (no `CSINode`, no net stack).
//!
//! Binary-footprint counterpart to `wifi_station`: identical platform
//! boilerplate and a raw STA bring-up (config + associate at the controller
//! level), but **without** the `CSINode` state machine, the embassy-net/smoltcp
//! IP stack, DHCP, `sta_network_ops`, `set_csi`, or `CSIDataPacket` pipeline that
//! the full STA CSI role pulls in. `full − min` exposes that whole stack; the
//! per-library breakdown attributes it (smoltcp / embassy-net / esp_csi_rs).
//! Built and measured, not run (Test 3).
//!
//! Build: `cargo build --release --target xtensa-esp32-none-elf --example wifi_station_min --features=esp32`.

#![no_std]
#![no_main]

use crate::alloc::string::ToString;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::log_ln;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, WifiController};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

const WIFI_SSID: &str = "myssid";
const WIFI_PASS: &str = "mypassword";

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

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

    log_ln!("Footprint min: Wi-Fi STA platform floor (no CSINode / net stack)");

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, _interfaces) =
        esp_radio::wifi::new(peripherals.WIFI, config_radio).expect("Wi-Fi init failed");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    // Raw STA bring-up: configure + associate at the controller level. No
    // embassy-net/smoltcp/DHCP, no CSINode — those are what `full − min` reveals.
    let client_config = StationConfig::default()
        .with_ssid(WIFI_SSID)
        .with_password(WIFI_PASS.to_string())
        .with_auth_method(esp_radio::wifi::AuthenticationMethod::Wpa2Personal);
    let _ = controller.set_config(&Config::Station(client_config));
    let _ = controller.connect_async().await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
