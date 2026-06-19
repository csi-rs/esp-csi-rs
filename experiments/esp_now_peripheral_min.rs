//! Footprint **min** — ESP-NOW peripheral platform floor (no `CSINode`).
//!
//! Binary-footprint counterpart to `esp_now_peripheral`: identical platform
//! boilerplate and the same raw ESP-NOW bring-up, but **without** the `CSINode`
//! state machine, control-packet ingest, `CSIDataPacket` build, or `set_csi`
//! pipeline. `full − min` isolates the peripheral (RX) state-machine footprint.
//! Built and measured, not run (Test 3). Shares the ESP-NOW radio floor with
//! `esp_now_central_min`.
//!
//! Build: `cargo build --release --target xtensa-esp32-none-elf --example esp_now_peripheral_min --features=esp32`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{init_logger, LogMode};
use esp_csi_rs::{log_ln, set_peer_espnow_phy};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{WifiPhyRate, BROADCAST_ADDRESS};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, SecondaryChannel, WifiController};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Wi-Fi channel — match `esp_now_peripheral`.
const CHANNEL: u8 = 1;

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

    log_ln!("Footprint min: ESP-NOW peripheral platform floor (no CSINode)");

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, interfaces) =
        esp_radio::wifi::new(peripherals.WIFI, config_radio).expect("Wi-Fi init failed");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    // Raw ESP-NOW bring-up — no CSINode, no ingest/CSIDataPacket/set_csi pipeline.
    let _esp_now = interfaces.esp_now;
    let _ = controller.set_config(&Config::Station(StationConfig::default()));
    let _ = controller.set_channel(CHANNEL, SecondaryChannel::None);
    set_peer_espnow_phy(&BROADCAST_ADDRESS, WifiPhyRate::RateMcs0Lgi, None);

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
