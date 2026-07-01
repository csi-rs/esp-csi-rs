//! Wi-Fi 6 **HE20** softAP CSI collector + traffic generator (TX side of the pair).
//!
//! Starts an open softAP; once the station ([`wifi_station_5ghz_he20`]) associates,
//! the AP floods ICMP echo requests to the leased client at `PING_RATE_HZ`. With
//! 802.11ax negotiated on the link, those are HE20 PPDUs, so the station captures
//! the dense ~242-subcarrier HE-LTF; this node captures the HE echo replies. This
//! is the pair's only traffic generator — the station side is receive-only, so
//! there is no competing uplink flood eating into the same airtime.
//!
//! Band by chip: **C5** → 5 GHz ch149 (HE20); **C6** → 2.4 GHz ch6 (HE20);
//! Wi-Fi 4 chips → 802.11n fallback (build/runs, no HE). CSI config is the crate
//! default (acquires legacy + HT + HE-SU/MU), so CSI flows for every PPDU format
//! and the station's `data_format` reveals which frames are actually HE20.
//!
//! Run:
//!   cargo esp32c5 --example wifi_ap_5ghz_he20          # 5 GHz ch149
//!   cargo esp32c6 --example wifi_ap_5ghz_he20          # 2.4 GHz ch6 (fixed)

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeHardware, CentralOpMode, CollectionMode, Node, WifiApConfig, log_ln,
    config::CsiConfig, set_csi_callback,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::{PowerSaveMode, Protocol, WifiController};
use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Compile-time `u8` from an optional env string (decimal), else `default`.
/// Lets you flash N distinct pairs without editing source:
///   `HE20_CHANNEL=153 cargo esp32c5 --example wifi_ap_5ghz_he20`
const fn parse_u8_or(s: Option<&str>, default: u8) -> u8 {
    match s {
        None => default,
        Some(s) => {
            let bytes = s.as_bytes();
            let mut i = 0;
            let mut acc: u32 = 0;
            while i < bytes.len() {
                let b = bytes[i];
                if b < b'0' || b > b'9' {
                    return default;
                }
                acc = acc * 10 + (b - b'0') as u32;
                i += 1;
            }
            if i == 0 || acc > 255 { default } else { acc as u8 }
        }
    }
}

/// SSID (override with `HE20_SSID` at build time). Each independent pair should
/// use a unique SSID so its STA associates only to its own AP.
const SSID: &str = match option_env!("HE20_SSID") {
    Some(s) => s,
    None => "esp-csi-ap",
};

// Channel (override with `HE20_CHANNEL` on C5 only). Use distinct non-overlapping
// 5 GHz channels per C5 pair (e.g. 149/153/157/161). C6 is 2.4 GHz-only → ch6.
#[cfg(feature = "esp32c5")]
const CHANNEL: u8 = parse_u8_or(option_env!("HE20_CHANNEL"), 157);
#[cfg(feature = "esp32c6")]
const CHANNEL: u8 = 6;
#[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
const CHANNEL: u8 = parse_u8_or(option_env!("HE20_CHANNEL"), 6);

// Wi-Fi 6 parts request 802.11ax; Wi-Fi 4 parts fall back to 802.11n.
#[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
const PROTOCOL: Protocol = Protocol::AX;
#[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
const PROTOCOL: Protocol = Protocol::N;

/// ICMP ping rate to the leased station (Hz) — each is a downlink CSI trigger.
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
            "AP HE20 ch{}: CSI packets {}, latest RSSI {}",
            CHANNEL,
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

    // AX/HE20 association needs more headroom than the 61440 (60 KB) used by
    // the plain N-protocol examples — 65536 is the max `dram2_seg` allows on
    // C5. See wifi_station_5ghz_he20.rs.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!(
        "HE20 softAP — SSID {}, channel {}, {:?}",
        SSID,
        CHANNEL,
        PROTOCOL
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);
    let _ = controller.set_power_saving(PowerSaveMode::None);

    let ap_radio_config = AccessPointConfig::default()
        .with_ssid(SSID)
        .with_channel(CHANNEL);
    // secondary = None → 20 MHz (HE20), no HT40.
    let ap_config = WifiApConfig::new(ap_radio_config, CHANNEL, None);

    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    let csi_cfg = CsiConfig::he20();
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    let csi_cfg = CsiConfig::default();

    let mut node = CSINode::new(
        Node::Central(CentralOpMode::WifiAccessPoint(ap_config)),
        CollectionMode::Collector,
        Some(csi_cfg),
        Some(PING_RATE_HZ),
        csi_hardware,
    );
    node.set_protocol(PROTOCOL);

    set_csi_callback(on_csi);
    spawner.spawn(stats_task().unwrap());
    node.run().await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
