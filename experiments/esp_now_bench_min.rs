//! **ESP-NOW footprint floor** — the platform baseline, with no `CSINode` in it.
//!
//! The `min` half of the binary-footprint measurement. Performs the same platform boilerplate and
//! the same raw ESP-NOW bring-up the crate's ESP-NOW arm relies on (STA config, channel, per-peer
//! MCS0 PHY) and nothing else: no node state machine, no `ControlPacket` serialization, no TX
//! scheduler and no TX loop.
//!
//! `esp_now_bench − esp_now_bench_min` is the crate's ESP-NOW footprint. **Build both halves with
//! the same feature set**, or the difference includes whatever the features linked. Built and
//! measured, not run. The radio floor is shared between the central and peripheral ends, which is
//! why there is one floor rather than two.
//!
//! Build: `cargo esp32c6-build --example esp_now_bench_min`

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{log_ln, set_peer_espnow_phy};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{BROADCAST_ADDRESS, WifiPhyRate};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, SecondaryChannel, WifiController};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Wi-Fi channel — match `esp_now_central`.
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

    log_ln!("Footprint min: ESP-NOW central platform floor (no CSINode)");

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, interfaces) =
        esp_radio::wifi::new(peripherals.WIFI, config_radio).expect("Wi-Fi init failed");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    // Raw ESP-NOW bring-up (STA started, channel, per-peer MCS0) — no CSINode,
    // no ControlPacket, no TX loop. Keeps the ESP-NOW interface alive/linked.
    let _esp_now = interfaces.esp_now;
    let _ = controller.set_config(&Config::Station(StationConfig::default()));
    let _ = controller.set_channel(CHANNEL, SecondaryChannel::None);
    set_peer_espnow_phy(&BROADCAST_ADDRESS, WifiPhyRate::RateMcs0Lgi, None);

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
