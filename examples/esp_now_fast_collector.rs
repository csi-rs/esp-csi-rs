//! ESP-NOW fast one-to-one **collector** — asymmetric simplex, channel 6, HT20.
//!
//! Companion to `esp_now_fast_source`. This node broadcasts a sparse ~1 Hz
//! discovery beacon until it hears the source, then **stops beaconing** and goes
//! RX-only, capturing CSI from the source's continuous unicast flood. Leaving all
//! airtime to the single transmitter maximizes CSI packets/sec versus the
//! balanced `esp_now_central`/`esp_now_peripheral` pair.
//!
//! Pairing is automatic (magic-prefix; no hardcoded MACs). Flash this on one
//! board and `esp_now_fast_source` on another, both on channel 6.
//!
//! Build / run (optional throughput counters need `--features statistics`):
//!   cargo esp32c6 --example esp_now_fast_collector
//!   cargo esp32s3 --example esp_now_fast_collector

#![no_std]
#![no_main]

use embassy_executor::Spawner;
#[cfg(feature = "statistics")]
use embassy_time::Instant;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeHardware, CollectionMode, EspNowConfig, install_static_espnow_recv, log_ln,
    set_csi_callback,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::{get_dropped_packets_rx, get_pps_rx, get_total_rx_packets};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

const CHANNEL: u8 = 6;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
static CSI_PKT_COUNT: AtomicU32 = AtomicU32::new(0);

fn on_csi(packet: &CSIDataPacket) {
    LATEST_RSSI.store(packet.rssi as i32, Ordering::Relaxed);
    CSI_PKT_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[embassy_executor::task]
async fn stats_task() {
    #[cfg(feature = "statistics")]
    let mut last_sample = Instant::now();
    #[cfg(feature = "statistics")]
    let mut last_rx_total = get_total_rx_packets();

    loop {
        Timer::after_secs(1).await;

        #[cfg(feature = "statistics")]
        {
            let elapsed_us = last_sample.elapsed().as_micros() as u64;
            let rx_total = get_total_rx_packets();
            let rx_rate_hz = if elapsed_us == 0 {
                0
            } else {
                (rx_total.saturating_sub(last_rx_total) * 1_000_000 / elapsed_us) as u32
            };
            last_sample = Instant::now();
            last_rx_total = rx_total;

            log_ln!(
                "RX PPS(avg): {}, RX Hz(inst): {}, RX Total: {}, RX Dropped: {}, CSI Packets: {}, Latest RSSI: {}",
                get_pps_rx(),
                rx_rate_hz,
                rx_total,
                get_dropped_packets_rx(),
                CSI_PKT_COUNT.load(Ordering::Relaxed),
                LATEST_RSSI.load(Ordering::Relaxed),
            );
        }

        #[cfg(not(feature = "statistics"))]
        {
            log_ln!(
                "CSI Packets: {}, Latest RSSI: {}",
                CSI_PKT_COUNT.load(Ordering::Relaxed),
                LATEST_RSSI.load(Ordering::Relaxed),
            );
        }
    }
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
        "Starting ESP-NOW fast collector — channel {}, HT20, sparse beacon then RX-only flood capture",
        CHANNEL
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    install_static_espnow_recv();

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    // HT20 MCS7 base. The collector only forces its own RX bandwidth; the source
    // forces the actual unicast TX PHY.
    let espnow_cfg = EspNowConfig::fast_default().with_channel(CHANNEL);

    let mut node = CSINode::new(
        esp_csi_rs::Node::Central(esp_csi_rs::CentralOpMode::EspNowFastCollector(espnow_cfg)),
        CollectionMode::Collector,
        Some(esp_csi_rs::config::CsiConfig::default()),
        None,
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);

    set_csi_callback(on_csi);
    spawner.spawn(stats_task().unwrap());
    node.run().await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
