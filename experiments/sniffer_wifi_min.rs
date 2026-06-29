//! Footprint **min** — Wi-Fi promiscuous-sniffer platform floor (no `CSINode`).
//!
//! Binary-footprint counterpart to `sniffer_wifi_exper`: identical platform
//! boilerplate (esp-hal + esp-rtos + esp-radio + embassy + alloc + the Wi-Fi
//! blob) and the same raw promiscuous bring-up the crate's sniffer arm performs
//! (`Sniffer::set_promiscuous_mode` + `set_channel`), but **without** the
//! `CSINode` state machine, `CsiConfig`, `set_csi`, `CSIDataPacket` build, or
//! `log_csi` pipeline. `full − min` isolates that crate machinery for the
//! sniffer role. Not meant to be run — it is built and measured (Test 3).
//!
//! Build: `cargo build --release --target xtensa-esp32-none-elf --example sniffer_wifi_min --features=esp32`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::log_ln;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::{SecondaryChannel, WifiController};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Wi-Fi channel — match `sniffer_wifi_exper`.
const CHANNEL: u8 = 1;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Footprint min: Wi-Fi sniffer platform floor (no CSINode)");

    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_static_rx_buf_num(25)
        .with_dynamic_rx_buf_num(128)
        .with_rx_queue_size(32);
    let (wifi_controller, interfaces) =
        esp_radio::wifi::new(peripherals.WIFI, config_radio).expect("Wi-Fi init failed");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    // Same esp-radio calls the CSINode sniffer arm makes — raw promiscuous +
    // channel lock — but no CSI capture/serialize/log machinery is linked.
    let sniffer = &interfaces.sniffer;
    let _ = sniffer.set_promiscuous_mode(true);
    let _ = controller.set_channel(CHANNEL, SecondaryChannel::None);

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
