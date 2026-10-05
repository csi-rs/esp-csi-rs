//! Footprint **min** — Wi-Fi STA platform floor (no `CSINode`, no net stack).
//!
//! Binary-footprint counterpart to `station_bench`: identical platform
//! boilerplate and a raw STA bring-up (config + associate at the controller
//! level), but **without** the `CSINode` state machine, the embassy-net/smoltcp
//! IP stack, DHCP, `sta_network_ops`, `set_csi`, or `CsiPacket` pipeline that
//! the full station mode pulls in. `full − min` exposes that whole stack; the
//! per-library breakdown attributes it (smoltcp / embassy-net / esp_csi_rs).
//! Built and measured, not run (Test 3).
//!
//! Build: `cargo build --release --target xtensa-esp32-none-elf --example station_bench_min --features=esp32`.

#![no_std]
#![no_main]

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
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    log_ln!("Footprint min: Wi-Fi STA platform floor (no CSINode / net stack)");

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let wifi_controller = esp_radio::wifi::WifiController::new(peripherals.WIFI, config_radio).expect("Wi-Fi init failed");
    let controller = WIFI_CONTROLLER.init(wifi_controller);
    // Claim the same radio handles `NodeHardware::new` claims, so the footprint difference
    // against the full harness is only the CSINode machinery.
    let _station = esp_radio::wifi::Interface::station();
    let _access_point = esp_radio::wifi::Interface::access_point();
    let _sniffer = controller.sniffer();
    let _esp_now = controller.esp_now();

    // Raw STA bring-up: configure + associate at the controller level. No
    // embassy-net/smoltcp/DHCP, no CSINode — those are what `full − min` reveals.
    let client_config = StationConfig::default()
        .with_ssid(WIFI_SSID.try_into().expect("SSID longer than 32 bytes"))
        .with_authentication(esp_radio::wifi::AuthenticationMethodConfig::Wpa2Personal(
            WIFI_PASS.try_into().expect("password longer than 64 bytes"),
        ));
    let _ = controller.set_config(&Config::Station(client_config));
    let _ = controller.connect_async().await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
