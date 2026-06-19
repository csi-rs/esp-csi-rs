//! Continuous MCS0-LGI / HT40 broadcaster — companion TX for
//! `esp_now_peripheral_bw_check`.
//!
//! Unlike the CPU-test TXs (schedule-gated, silent for the first ~40 s), this
//! drives the library central in plain TX-only mode at a fixed rate from boot,
//! so the paired `bw_check` peripheral starts logging `BW_CHECK` lines within a
//! second. Same PHY request as the CPU-test exper TX: `set_rate(RateMcs0Lgi)` +
//! `with_ht40(Below)` on channel 11 — so the bandwidth the RX reports tells you
//! whether HT40 actually engaged end-to-end.
//!
//! Build: `cargo build --release --example esp_now_central_bw_tx --features <chip>,async-print`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    config::CsiConfig, log_ln, logging::logging::init_logger, CSINode, CSINodeClient,
    CSINodeHardware, CentralOpMode, CollectionMode, EspNowConfig, IOTaskConfig, Node,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

const TEST_CHANNEL: u8 = 11;

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(_spawner, LogMode::Text);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Continuous MCS0-LGI/HT20 broadcaster @100 Hz on channel {}", TEST_CHANNEL);

    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_static_tx_buf_num(25)
        .with_dynamic_tx_buf_num(128)
        .with_ampdu_tx_enable(false)
        .with_tx_queue_size(32);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut _node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        Node::Central(CentralOpMode::EspNow(
            EspNowConfig::default()
                .with_channel(TEST_CHANNEL),
        )),
        CollectionMode::Listener,
        Some(CsiConfig::default()),
        Some(10000), // ~100 Hz broadcast, from boot, no schedule gating
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    node.set_rate(WifiPhyRate::RateMcs0Lgi);
    node.set_io_tasks(IOTaskConfig::new(true, false)); // TX only

    node.run().await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
