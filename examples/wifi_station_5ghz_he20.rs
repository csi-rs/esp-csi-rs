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
//! Pair with the AP on the same band/channel: C5 AP → 5 GHz ch157 (or
//! `HE20_CHANNEL=6` for 2.4 GHz); **C6 AP → 2.4 GHz ch6** (C6 cannot use
//! 5 GHz channels). Flash the station with the same `HE20_CHANNEL` as the AP.
//!
//! Output is postcard + COBS serialized via [`LogMode::Serialized`] — **not**
//! human-readable. Record it with `tools/he20_csi_recorder.py --schema v1 --chip c5`
//! on a separate terminal (it prints a live rate to stderr).
//!
//! IMPORTANT — do **not** register `set_csi_callback` here: a callback puts
//! delivery in `Callback` mode, bypassing the serialized `log_csi` path.
//! Delivery stays in `Off` mode so the WiFi callback routes each packet through
//! `log_csi`. Avoid printing text on this UART — text bytes corrupt whichever
//! COBS frame they land in. The one exception is the 1 Hz `diag_task` line
//! below: it opens with a `\0` that closes any partial frame, so it lands as a
//! single undecodable frame the recorder skips and counts under `errors`
//! (≤2 frames/s). On C5 dual-band, pass `HE20_CHANNEL` so the crate selects
//! the correct 2.4 / 5 GHz band before association.
//!
//! For throughput (~490 B/packet) build with async logging over USB-Serial-JTAG:
//!   cargo esp32c5 --example wifi_station_5ghz_he20 --features async-print
//!   cargo esp32c6 --example wifi_station_5ghz_he20 --features async-print

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::set_csi_logging_enabled;
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

/// Compile-time `u8` from an optional env string (decimal), else `default`.
#[cfg(not(feature = "esp32c6"))]
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

// Must match this pair's AP channel. C5 defaults to 5 GHz ch157; set
// `HE20_CHANNEL=6` for 2.4 GHz HE20 on C5 (band selection uses this hint).
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

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// 1 Hz diagnostic line for the burstiness investigation. The leading `\0`
/// closes any partial COBS frame so the text lands as one isolated junk frame
/// for the recorder (counted in its `errors`) instead of corrupting two.
///
/// Reading it: `csi_cb/s=0` while associated → the radio itself went quiet
/// (traffic/driver stall); `csi_cb/s` healthy but `log_drop/s` large → the
/// serial output path is the bottleneck; `heap_free` sawtoothing toward 0 →
/// driver buffer starvation. `csi_cb/s`/`rx_drop/s` need
/// `--features statistics`; the heap numbers are always available.
#[embassy_executor::task]
async fn diag_task() {
    #[cfg(feature = "statistics")]
    let mut last_cb = 0u64;
    #[cfg(feature = "statistics")]
    let mut last_rx_drop = 0u32;
    let mut last_log_drop = 0u32;
    loop {
        Timer::after(Duration::from_secs(1)).await;
        #[cfg(feature = "statistics")]
        let cb = esp_csi_rs::stats::get_total_rx_packets();
        #[cfg(feature = "statistics")]
        let cb_delta = cb.wrapping_sub(last_cb);
        #[cfg(not(feature = "statistics"))]
        let cb_delta = 0u64;
        #[cfg(feature = "statistics")]
        let rx_drop = esp_csi_rs::stats::get_dropped_packets_rx();
        #[cfg(feature = "statistics")]
        let rx_drop_delta = rx_drop.wrapping_sub(last_rx_drop);
        #[cfg(not(feature = "statistics"))]
        let rx_drop_delta = 0u32;
        let log_drop = esp_csi_rs::logging::logging::get_log_packet_drops();
        log_ln!(
            "\u{0}STA-DIAG csi_cb/s={} log_drop/s={} rx_drop/s={} heap_used={} heap_free={}",
            cb_delta,
            log_drop.wrapping_sub(last_log_drop),
            rx_drop_delta,
            esp_alloc::HEAP.used(),
            esp_alloc::HEAP.free(),
        );
        last_log_drop = log_drop;
        #[cfg(feature = "statistics")]
        {
            last_cb = cb;
            last_rx_drop = rx_drop;
        }
    }
}

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
        "HE20 station (serialized) — SSID {}, ch {}, {:?}, pair with AP on 2.4 GHz ch6",
        SSID,
        CHANNEL,
        PROTOCOL
    );
    #[cfg(not(feature = "esp32c6"))]
    log_ln!(
        "HE20 station (serialized) — SSID {}, ch {}, {:?}",
        SSID,
        CHANNEL,
        PROTOCOL
    );

    // Match the AP: AMPDU off so every downlink echo is its own PPDU (one CSI
    // event per ping, no A-MPDU clumping / Block-Ack recovery stalls). TX
    // buffers stay halved (16) — this side only TXes one echo reply per ping.
    // RX buffers stay at the full 32: this station absorbs the AP's whole
    // flood (~700 fps) while also serializing CSI and replying to pings, and
    // a frame that finds no free RX buffer is dropped in the blob BEFORE the
    // CSI callback fires — with 16 buffers that starvation showed up as
    // ~340 pps CSI from a 700 fps flood, with zero drops visible anywhere.
    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_ampdu_rx_enable(false)
        .with_ampdu_tx_enable(false)
        .with_dynamic_rx_buf_num(32)
        .with_dynamic_tx_buf_num(16)
        .with_rx_ba_win(4);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);
    let _ = controller.set_power_saving(PowerSaveMode::None);

    let client_config = StationConfig::default()
        .with_ssid(SSID)
        .with_auth_method(esp_radio::wifi::AuthenticationMethod::None);
    let station_config = WifiStationConfig::new(client_config).with_channel_hint(CHANNEL);

    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    // Set `HE20_CSI_ALL=1` at build time to acquire CSI for every PPDU format
    // (legacy/HT/HE) instead of HE20-only. Diagnostic for rate-control
    // fallback: frames the AP transmits at non-HE rates produce NO CSI event
    // under the he20 preset — they are invisible, not counted anywhere. With
    // this on, `data_format` in the recorded parquet shows the real mix.
    const CSI_ALL_FORMATS: bool = matches!(option_env!("HE20_CSI_ALL"), Some(_));
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    let csi_cfg = if CSI_ALL_FORMATS {
        CsiConfig::default()
    } else {
        CsiConfig::he20()
    };
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
    // spawner.spawn(diag_task().unwrap());
    node.run().await;

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}
