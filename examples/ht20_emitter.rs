//! HT20 emitter — 20 MHz 802.11n channel sounding.
//!
//! Forces the radio to an HT20 PPDU and loop-injects a raw sounding frame on a
//! fixed channel without associating to anything. This node captures no CSI; it
//! exists so that a collector can measure the channel's response to it.
//!
//! Pair with `collector_sniffer` on the same channel. Works on every supported
//! chip — HT20 is plain 802.11n.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::Duration;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{CSINode, EmitterConfig, HtBandwidth, NodeHardware, log_ln};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Channel to sound. Every node in the capture set must agree on this.
const CHANNEL: u8 = 7;
/// 20 ms between frames ≈ 50 sounding frames per second.
const PERIOD_MS: u64 = 20;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(_spawner, LogMode::Text);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    log_ln!("Starting HT20 emitter on channel {}", CHANNEL);

    let hardware = NodeHardware::new(&mut interfaces, controller);
    let emitter = EmitterConfig::new(CHANNEL, HtBandwidth::Ht20)
        .with_period(Duration::from_millis(PERIOD_MS));
    let mut node = CSINode::new_emitter(emitter, hardware);

    node.run().await;

    // `run` only returns if something signalled a stop.
    loop {
        log_ln!("Emitter stopped");
        embassy_time::Timer::after(Duration::from_secs(5)).await;
    }
}
