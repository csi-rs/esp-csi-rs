//! ESP-NOW fast one-to-one **source** — asymmetric simplex, channel 6, HT20.
//!
//! Companion to `esp_now_fast_collector`. This node listens for the collector's
//! sparse discovery beacon, learns its MAC, registers it as a forced-PHY unicast
//! peer, sends one magic hello, then floods unicast frames at max rate. With the
//! collector silent on TX, all airtime goes to this flood → maximum CSI
//! packets/sec on the collector.
//!
//! Pairing is automatic (magic-prefix; no hardcoded MACs). Flash this on one
//! board and `esp_now_fast_collector` on another, both on channel 6.
//!
//! Build / run (optional throughput counters need `--features statistics`):
//!   cargo esp32c6 --example esp_now_fast_source
//!   cargo esp32s3 --example esp_now_fast_source

#![no_std]
#![no_main]

use embassy_executor::Spawner;
#[cfg(feature = "statistics")]
use embassy_time::Instant;
use embassy_time::{Duration, Timer, with_timeout};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeClient, CSINodeHardware, EspNowConfig, IOTaskConfig,
    install_static_espnow_recv, log_ln,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::{get_pps_tx, get_total_tx_packets};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

const CHANNEL: u8 = 6;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

async fn stats_task(client: &mut CSINodeClient) {
    #[cfg(feature = "statistics")]
    let mut last_sample = Instant::now();
    #[cfg(feature = "statistics")]
    let mut last_tx_total = get_total_tx_packets();

    with_timeout(Duration::from_secs(1000), async {
        loop {
            Timer::after_secs(1).await;

            #[cfg(feature = "statistics")]
            {
                let elapsed_us = last_sample.elapsed().as_micros() as u64;
                let tx_total = get_total_tx_packets();
                let tx_rate_hz = if elapsed_us == 0 {
                    0
                } else {
                    (tx_total.saturating_sub(last_tx_total) * 1_000_000 / elapsed_us) as u32
                };
                last_sample = Instant::now();
                last_tx_total = tx_total;

                log_ln!(
                    "TX PPS(avg): {}, TX Hz(inst): {}, TX Total: {}",
                    get_pps_tx(),
                    tx_rate_hz,
                    tx_total,
                );
            }

            #[cfg(not(feature = "statistics"))]
            {
                log_ln!("ESP-NOW fast source flooding...");
            }
        }
    })
    .await
    .unwrap_err();
    client.send_stop().await;
}

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

    log_ln!("Embassy initialized!");
    log_ln!(
        "Starting ESP-NOW fast source — channel {}, HT20, discover then unicast flood",
        CHANNEL
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    install_static_espnow_recv();

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    let espnow_cfg = EspNowConfig::fast_default().with_channel(CHANNEL);

    let mut node = CSINode::new(
        esp_csi_rs::NodeRole::Peripheral(esp_csi_rs::PeripheralOpMode::EspNowFastSource(
            espnow_cfg,
        )),
        None,
        None,
        csi_hardware,
    );
    // `CollectionMode::Listener` became this: keep the radio capturing, and its
    // timing intact, but deliver nothing off-device.
    node.set_csi_output_enabled(false);
    node.set_protocol(esp_radio::wifi::Protocol::N);
    // TX-only flood; CSI is captured on the collector.
    node.set_io_tasks(IOTaskConfig::new(true, false));

    embassy_futures::join::join(node.run(), stats_task(&mut node_handle)).await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
