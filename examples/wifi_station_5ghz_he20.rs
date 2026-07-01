//! Wi-Fi 6 **HE20** station — serialized CSI output (RX side of the pair).
//!
//! Associates to SSID `esp-csi-ap` (open auth), runs DHCP, then just captures the
//! AP's **downlink** HE20 PPDUs as CSI — it does **not** run its own ICMP flood.
//! The AP ([`wifi_ap_5ghz_he20`]) already floods the leased station at
//! `PING_RATE_HZ`, and the embassy-net stack auto-replies to those echoes without
//! a dedicated flood task; adding a second, independent uplink flood here only
//! competes with the AP's downlink flood for airtime (halving effective CSI rate
//! and causing bursty, stuttering capture) without increasing this node's own
//! downlink CSI, which is driven entirely by the AP's flood. Pair with the AP on
//! the same band/channel: C5 AP → 5 GHz ch149; **C6 AP → 2.4 GHz ch6** (C6 cannot
//! use 5 GHz channels).
//!
//! Output is postcard + COBS serialized via [`LogMode::Serialized`] — **not**
//! human-readable. Record it with `tools/he20_csi_recorder.py --schema v1 --chip c5`
//! on a separate terminal (it prints a live rate to stderr).
//!
//! IMPORTANT — do **not** register `set_csi_callback` here, and do **not** print
//! text on this UART: a callback puts delivery in `Callback` mode (bypassing the
//! serialized `log_csi` path), and text bytes corrupt the binary COBS stream.
//! Delivery stays in `Off` mode so the WiFi callback routes each packet through
//! `log_csi`. The STA does not pin a channel — it scans and associates to the
//! 5 GHz AP once AX is enabled on the 5 GHz band (crate handles this for STA).
//!
//! For throughput (~490 B/packet) build with async logging over USB-Serial-JTAG:
//!   cargo esp32c5 --example wifi_station_5ghz_he20 --features async-print
//!   cargo esp32c6 --example wifi_station_5ghz_he20 --features async-print

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeHardware, CentralOpMode, CollectionMode, Node, WifiStationConfig,
    config::CsiConfig, log_ln,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{PowerSaveMode, Protocol, WifiController};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Must match this pair's AP. Override with `HE20_SSID` at build time so each
/// independent collector associates only to its own `wifi_ap_5ghz_he20`.
const SSID: &str = match option_env!("HE20_SSID") {
    Some(s) => s,
    None => "esp-csi-ap",
};

// Wi-Fi 6 parts request 802.11ax; Wi-Fi 4 parts fall back to 802.11n.
#[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
const PROTOCOL: Protocol = Protocol::AX;
#[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
const PROTOCOL: Protocol = Protocol::N;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    // Serialized CSI (COBS-framed postcard). `init_logger` opens the CSI gates.
    init_logger(spawner, LogMode::Serialized);

    // AX/HE20 association needs more headroom than the 61440 (60 KB) used by
    // the plain N-protocol examples — 60 KB panics with an allocation failure
    // right after WiFi controller start (CSI RX callback registration). 65536
    // is the max the `dram2_seg` region on C5 can give this heap; anything
    // higher overflows the link (98440 overflowed by exactly 32904 bytes).
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    #[cfg(feature = "esp32c6")]
    log_ln!(
        "HE20 station (serialized) — SSID {}, {:?}, pair with AP on 2.4 GHz ch6",
        SSID,
        PROTOCOL
    );
    #[cfg(not(feature = "esp32c6"))]
    log_ln!("HE20 station (serialized) — SSID {}, {:?}", SSID, PROTOCOL);

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);
    let _ = controller.set_power_saving(PowerSaveMode::None);

    let client_config = StationConfig::default()
        .with_ssid(SSID)
        .with_auth_method(esp_radio::wifi::AuthenticationMethod::None);
    let station_config = WifiStationConfig { client_config };

    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    let csi_cfg = CsiConfig::he20();
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    let csi_cfg = CsiConfig::default();

    let mut node = CSINode::new(
        Node::Central(CentralOpMode::WifiStation(station_config)),
        CollectionMode::Collector,
        Some(csi_cfg),
        None,
        csi_hardware,
    );
    node.set_protocol(PROTOCOL);
    // No self-generated uplink flood: the AP already drives downlink traffic at
    // PING_RATE_HZ, and a second independent flood here would only contend for
    // airtime with it (see module doc). DHCP + ICMP echo replies still run via
    // the net stack task, which stays enabled.
    node.set_tx_enabled(false);

    // No `set_csi_callback` — delivery stays `Off` so the WiFi callback routes each
    // packet through `log_csi`, which writes the COBS-framed postcard bytes.
    node.run().await;

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}
