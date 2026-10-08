//! **ESP-NOW** — the symmetric connectionless exchange.
//!
//! | Attribute | Value |
//! |---|---|
//! | Operational mode | ESP-NOW |
//! | Network role | `NETWORK_ROLE` below — both are meaningful |
//! | Reporting policy | `REPORTING` below — `Always`, `Never`, `Threshold` or `Decimate` |
//! | Session role | none — the run's controller is whatever calls `run()` |
//!
//! This is the one mode where **every** combination of the two attributes is meaningful, because
//! the exchange is symmetric: the central originates the control traffic, the peripheral answers
//! it, and either end can measure. It is also the only mode that supports a star — one central
//! against several peripherals. Flash this same example onto both boards and change `NETWORK_ROLE`.
//!
//! Pairing is automatic: the two ends find each other by magic prefix, so there are no hardcoded
//! MACs. Set `PEER_MAC` on both if you would rather pin it, in which case the source-MAC filter
//! becomes the discriminator and the magic prefix is dropped.
//!
//! A listening central announces itself on the wire (`ControlPacket::is_collector`), and a
//! peripheral that hears one **promotes itself to collector** so the pair still produces a dataset.
//! That promotion does not rewrite this node's configuration, so `node.reporting()` keeps
//! reporting what you set here while `esp_csi_rs::runtime_reporting()` reports what is in force.
//!
//! ## Forcing the PHY
//!
//! Without `with_phy_rate`, ESP-NOW frames fall back to a legacy 11b/g rate. With it, the per-peer
//! TX PHY is forced and the node brings the radio up in started STA mode to do it.
//!
//! **HT40 only applies to a unicast peer.** The central broadcasts; the peripheral learns its MAC
//! and replies by unicast, and the forced HT40 rate applies to that learned peer — so the wide CSI
//! is collected by the *central*, from the peripheral's replies. On the dual-band ESP32-C5, forcing
//! the PHY on the broadcast peer is additionally unsafe (it wedges the Wi-Fi ISR), so a C5 pair
//! must use the unicast path. See `docs/bandwidth.md`.
//!
//! Verify HT40 engaged by the subcarrier count in the stats line: >= 100 (commonly ~117–128) is
//! HT40; ~53/~56 means it fell back. 2.4 GHz HT40 is finicky on some chips — if it stays narrow,
//! try a different primary/secondary pair (ch 1 Above, ch 11 Below) or accept HT20.
//!
//! ## Sessions and measurement setups
//!
//! Every measurement here is a *controlled* stimulus: `Stimulus::Controlled` carries the
//! measurement-setup id and the sounding instance — the number of the central's control frame that
//! produced it — so the two ends' measurements of one sounding line up. `SESSION_ID` names the run
//! on every frame's envelope. `SETUP_ID` applies an IEEE 802.11bf-style `MeasurementSetup` to the
//! central: its periodicity becomes the control-packet rate and its id is stamped on each frame.
//!
//! `HE20` (ESP32-C5 / C6) forces HE20 instead of HT20 and captures the full HE-LTF.
//!
//! Build / run (both ends on the same channel):
//!   cargo esp32c6 --example esp_now
//!   cargo esp32c5 --example esp_now   # set CHANNEL = 149 for the 5 GHz 149+153 HT40 pair

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CsiPacket;
use esp_csi_rs::wire::{Bandwidth, MeasurementSetup, Stimulus, StimulusParams};
use esp_csi_rs::logging::logging::{LogMode, auto_log_backend_label, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeClient, ReportingPolicy, EspNowConfig, IOTaskConfig, NetworkRole, NodeHardware,
    config::CsiConfig, install_static_espnow_recv, log_ln, set_csi_callback,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::{get_dropped_packets_rx, get_pps_rx, get_pps_tx};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::{SecondaryChannel, WifiController};
use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

// ── Knobs ───────────────────────────────────────────────────────────────────────────────────────

/// Which end of the exchange this board is. Flash the same binary to both, changing only this.
const NETWORK_ROLE: NetworkRole = NetworkRole::Central;

/// `Never` keeps the exchange running without reporting. A central that reports nothing tells its
/// peripheral so, and the peripheral promotes itself to `Always`.
const REPORTING: ReportingPolicy = ReportingPolicy::Always;

/// Both ends must agree. On the ESP32-C5, `>= 36` selects 5 GHz.
const CHANNEL: u8 = 6;

/// Forced TX PHY. `None` leaves ESP-NOW on the driver's default legacy rate.
const PHY_RATE: Option<WifiPhyRate> = Some(WifiPhyRate::RateMcs7Lgi);

/// HT40 secondary channel. Only meaningful with `PHY_RATE` set, and only on the unicast leg — see
/// the module docs.
const SECONDARY: Option<SecondaryChannel> = None;

/// Pin the peer instead of discovering it. Both ends need the other's address.
const PEER_MAC: Option<[u8; 6]> = None;

/// Control-packet rate (Hz) on the central.
const TRAFFIC_HZ: u16 = 1000;

/// Force HE20 (802.11ax) on the ESP32-C5 / C6 instead of HT20. Both ends must agree.
const HE20: bool = false;

/// Name the run on every frame's envelope. `None` draws a random id per run.
const SESSION_ID: Option<u32> = None;

/// Apply a `MeasurementSetup` with this id to the central, sounding at `TRAFFIC_HZ`.
const SETUP_ID: Option<u8> = None;

/// Prune a task subtree entirely: a TX-only central plus an RX-only peripheral is the lowest-
/// overhead one-directional pairing the mode offers.
const TX_ENABLED: bool = true;
const RX_ENABLED: bool = true;

/// `LogMode::Serialized` emits postcard/COBS frames instead of text — compact, not human-readable,
/// and the right choice when a host is parsing the stream.
const LOG_MODE: LogMode = LogMode::Text;

// ────────────────────────────────────────────────────────────────────────────────────────────────

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
static CSI_PKT_COUNT: AtomicU32 = AtomicU32::new(0);
static SUBCARRIERS: AtomicU32 = AtomicU32::new(0);
/// Setup id and sounding instance of the latest measurement.
static LATEST_SETUP: AtomicU32 = AtomicU32::new(0);
static LATEST_INSTANCE: AtomicU32 = AtomicU32::new(0);

fn on_csi(packet: &CsiPacket) {
    LATEST_RSSI.store(packet.rssi() as i32, Ordering::Relaxed);
    SUBCARRIERS.store(packet.subcarriers() as u32, Ordering::Relaxed);
    if let Stimulus::Controlled { setup_id, instance_id, .. } = packet.frame.stimulus {
        LATEST_SETUP.store(setup_id as u32, Ordering::Relaxed);
        LATEST_INSTANCE.store(instance_id as u32, Ordering::Relaxed);
    }
    CSI_PKT_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// HE20 needs an 802.11ax PHY.
fn with_he20(cfg: EspNowConfig) -> EspNowConfig {
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    if HE20 {
        return cfg.with_he20();
    }
    cfg
}

fn csi_config() -> CsiConfig {
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    if HE20 {
        return CsiConfig::he20();
    }
    CsiConfig::default()
}

#[embassy_executor::task]
async fn stats_task() {
    loop {
        Timer::after_secs(1).await;
        #[cfg(feature = "statistics")]
        log_ln!(
            "RX PPS: {}, TX PPS: {}, RX Dropped: {}, CSI Packets: {}, Subcarriers: {}, RSSI: {}",
            get_pps_rx(),
            get_pps_tx(),
            get_dropped_packets_rx(),
            CSI_PKT_COUNT.load(Ordering::Relaxed),
            SUBCARRIERS.load(Ordering::Relaxed),
            LATEST_RSSI.load(Ordering::Relaxed),
        );
        #[cfg(not(feature = "statistics"))]
        log_ln!(
            "CSI Packets: {}, Subcarriers: {}, RSSI: {}",
            CSI_PKT_COUNT.load(Ordering::Relaxed),
            SUBCARRIERS.load(Ordering::Relaxed),
            LATEST_RSSI.load(Ordering::Relaxed),
        );
        log_ln!(
            "session {:#010x}, setup {}, sounding instance {}",
            esp_csi_rs::session_id(),
            LATEST_SETUP.load(Ordering::Relaxed),
            LATEST_INSTANCE.load(Ordering::Relaxed),
        );
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LOG_MODE);
    log_ln!("Log backend: {}", auto_log_backend_label());

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let wifi_controller = esp_radio::wifi::WifiController::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    // Replace esp-radio's heap recv queue before any peer traffic can arrive.
    install_static_espnow_recv();

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut espnow_cfg = EspNowConfig::default()
        .with_channel(CHANNEL)
        .with_network_role(NETWORK_ROLE)
        .with_reporting(REPORTING);
    if let Some(rate) = PHY_RATE {
        espnow_cfg = espnow_cfg.with_phy_rate(rate);
    }
    if let Some(secondary) = SECONDARY {
        espnow_cfg = espnow_cfg.with_ht40(secondary);
    }
    if let Some(mac) = PEER_MAC {
        espnow_cfg = espnow_cfg.with_peer_mac(mac);
    }
    let espnow_cfg = with_he20(espnow_cfg);
    if let Some(id) = SESSION_ID {
        esp_csi_rs::set_session(id, None);
    }

    log_ln!(
        "Starting ESP-NOW node on channel {} — role {}, collection {}",
        CHANNEL,
        match NETWORK_ROLE {
            NetworkRole::Central => "central",
            _ => "peripheral",
        },
        match REPORTING {
            ReportingPolicy::Never => "listener",
            _ => "collector",
        },
    );

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = NodeHardware::new(controller);
    let mut node = CSINode::esp_now(espnow_cfg, Some(csi_config()), Some(TRAFFIC_HZ), csi_hardware);
    node.set_protocol(if HE20 {
        esp_radio::wifi::Protocol::AX
    } else {
        esp_radio::wifi::Protocol::N
    });
    if let (Some(id), NetworkRole::Central) = (SETUP_ID, NETWORK_ROLE) {
        let setup = MeasurementSetup::new(id).with_stimulus(StimulusParams::new(
            1_000_000 / TRAFFIC_HZ as u32,
            Bandwidth::Mhz20,
        ));
        if node.apply_measurement_setup(&setup).is_err() {
            log_ln!("Measurement setup refused by this mode");
        }
    }
    node.set_io_tasks(IOTaskConfig {
        tx_enabled: TX_ENABLED,
        rx_enabled: RX_ENABLED,
    });

    set_csi_callback(on_csi);
    let _ = &mut node_handle;
    spawner.spawn(stats_task().unwrap());
    node.run().await;

    loop {
        log_ln!("ESP-NOW node stopped");
        Timer::after(Duration::from_secs(5)).await;
    }
}
