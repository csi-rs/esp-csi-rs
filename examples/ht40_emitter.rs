//! HT40 emitter — 40 MHz 802.11n channel sounding.
//!
//! Same as `ht20_emitter` but bonds a second 20 MHz channel, so a collector sees
//! roughly twice the subcarriers per measurement. The secondary channel sits
//! above or below the primary; both nodes must agree on the primary, and the
//! collector's radio has to be on a 40 MHz path to decode the frames.
//!
//! Note that 40 MHz needs room in the band: with the secondary above channel 7,
//! the occupied span reaches channel 11. Pair with `collector_sniffer` on the
//! same primary channel.

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

/// Primary channel. The secondary sits above it, so the 40 MHz block spans 7–11.
const CHANNEL: u8 = 7;
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

    log_ln!("Starting HT40 emitter on primary channel {} (+40)", CHANNEL);

    let hardware = NodeHardware::new(&mut interfaces, controller);
    let emitter = EmitterConfig::new(CHANNEL, HtBandwidth::Ht40Above)
        .with_period(Duration::from_millis(PERIOD_MS));
    let mut node = CSINode::new_emitter(emitter, hardware);

    node.run().await;

    loop {
        log_ln!("Emitter stopped");
        embassy_time::Timer::after(Duration::from_secs(5)).await;
    }
}
