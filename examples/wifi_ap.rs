//! Self-contained softAP CSI collector — channel 6, open network `esp-csi-ap`.
//!
//! Starts a Wi-Fi access point with a built-in single-lease DHCP server
//! (`192.168.13.1` AP, `192.168.13.2` lease). Once a station associates, the AP
//! pings the leased client at 4 kHz; ICMP echo replies are uplink data frames
//! captured as CSI on this node. Pair with `wifi_station` on the same SSID for
//! bidirectional traffic (STA gateway ping + AP lease ping).
//!
//! Build / run:
//!   cargo esp32c6 --example wifi_ap
//!   cargo esp32s3 --example wifi_ap

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{CollectorMode, CSINode, log_ln, NodeHardware, set_csi_callback, WifiApConfig};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::{PowerSaveMode, WifiController};
use esp_radio::wifi::ap::AccessPointConfig;
use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

const CHANNEL: u8 = 6;
/// ICMP ping rate to the leased station (Hz) — each echo reply is uplink CSI.
const PING_RATE_HZ: u16 = 4000;

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
    loop {
        Timer::after_secs(1).await;
        log_ln!(
            "CSI Packets: {}, Latest RSSI: {}",
            CSI_PKT_COUNT.load(Ordering::Relaxed),
            LATEST_RSSI.load(Ordering::Relaxed),
        );
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
        "Starting softAP CSI collector — SSID esp-csi-ap, channel {}",
        CHANNEL
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);
    let _ = controller.set_power_saving(PowerSaveMode::None);

    let ap_radio_config = AccessPointConfig::default()
        .with_ssid("esp-csi-ap")
        .with_channel(CHANNEL);
    let ap_config = WifiApConfig::new(ap_radio_config, CHANNEL, None);

    let csi_hardware = NodeHardware::new(&mut interfaces, controller);

    let mut node = CSINode::new_collector(
        CollectorMode::AccessPoint(ap_config),
        Some(esp_csi_rs::config::CsiConfig::default()),
        Some(PING_RATE_HZ),
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
