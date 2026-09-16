//! **ESP-NOW** — the symmetric connectionless exchange.
//!
//! | Attribute | Value |
//! |---|---|
//! | Operational mode | ESP-NOW |
//! | Network role | `NETWORK_ROLE` below — both are meaningful |
//! | Collection mode | `COLLECTION_MODE` below — both are meaningful |
//! | Session role | Responder |
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
//! That promotion does not rewrite this node's configuration, so `node.collection_mode()` keeps
//! reporting what you set here while `esp_csi_rs::runtime_collection_mode()` reports what is in
//! force.
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
//! Build / run (both ends on the same channel):
//!   cargo esp32c6 --example esp_now
//!   cargo esp32c5 --example esp_now   # set CHANNEL = 149 for the 5 GHz 149+153 HT40 pair

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::{LogMode, auto_log_backend_label, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeClient, CollectionMode, EspNowConfig, IOTaskConfig, NetworkRole, NodeHardware,
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

/// `Listener` keeps the exchange running without reporting. A listening *central* tells its
/// peripheral so, and the peripheral promotes itself.
const COLLECTION_MODE: CollectionMode = CollectionMode::Collector;

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

fn on_csi(packet: &CSIDataPacket) {
    LATEST_RSSI.store(packet.rssi as i32, Ordering::Relaxed);
    SUBCARRIERS.store((packet.csi_data_len / 2) as u32, Ordering::Relaxed);
    CSI_PKT_COUNT.fetch_add(1, Ordering::Relaxed);
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
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    // Replace esp-radio's heap recv queue before any peer traffic can arrive.
    install_static_espnow_recv();

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut espnow_cfg = EspNowConfig::default()
        .with_channel(CHANNEL)
        .with_network_role(NETWORK_ROLE)
        .with_collection_mode(COLLECTION_MODE);
    if let Some(rate) = PHY_RATE {
        espnow_cfg = espnow_cfg.with_phy_rate(rate);
    }
    if let Some(secondary) = SECONDARY {
        espnow_cfg = espnow_cfg.with_ht40(secondary);
    }
    if let Some(mac) = PEER_MAC {
        espnow_cfg = espnow_cfg.with_peer_mac(mac);
    }

    log_ln!(
        "Starting ESP-NOW node on channel {} — role {}, collection {}",
        CHANNEL,
        match NETWORK_ROLE {
            NetworkRole::Central => "central",
            NetworkRole::Peripheral => "peripheral",
        },
        match COLLECTION_MODE {
            CollectionMode::Collector => "collector",
            CollectionMode::Listener => "listener",
        },
    );

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = NodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::esp_now(
        espnow_cfg,
        Some(CsiConfig::default()),
        Some(TRAFFIC_HZ),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
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
