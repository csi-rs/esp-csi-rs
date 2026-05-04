//! # A crate for CSI collection on ESP devices
//! ## Overview
//! This crate builds on the low level Espressif abstractions to enable the collection of Channel State Information (CSI) on ESP devices with ease.
//! Currently this crate supports only the ESP `no-std` development framework.
//!
//! ### Choosing a device
//! In terms of hardware, you need to make sure that the device you choose supports WiFi and CSI collection.
//! Currently supported devices include:
//! - ESP32
//! - ESP32-C2
//! - ESP32-C3
//! - ESP32-C6
//! - ESP32-S3
//!
//! In terms of project and software toolchain setup, you will need to specify the hardware you will be using. To minimize headache, it is recommended that you generate a project using `esp-generate` as explained next.
//!
//! ### Creating a project
//! To use this crate you would need to create and setup a project for your ESP device then import the crate. This crate is compatible with the `no-std` ESP development framework. You should also select the corresponding device by activating it in the crate features.
//!
//! To create a projects it is highly recommended to refer the to instructions in [The Rust on ESP Book](https://docs.esp-rs.org/book/) before proceeding. The book explains the full esp-rs ecosystem, how to get started, and how to generate projects for both `std` and `no-std`.
//!
//! Espressif has developed a project generation tool, `esp-generate`, to ease this process and is recommended for new projects. As an example, you can create a `no-std` project for the ESP32-C3 device as follows:
//!
//! ```bash
//! cargo install esp-generate
//! esp-generate --chip=esp32c3 [project-name]
//! ```
//!
//! ## Feature Flags
#![doc = document_features::document_features!()]
//! ## Using the Crate
//!
//! Each ESP device is represented as a node in a collection network. For each node, we need to configure its role in the network, the mode of operation, and the CSI collection behavior. The node role determines how the node participates in the network and interacts with other nodes, while the collection mode determines how the node handles CSI data.
//!
//! ### Node Roles
//! 1) **Central Node**: This type of node is one that generates traffic, also can connect to one or more peripheral nodes.
//! 2) **Peripheral Node**: This type of node does not generate traffic, also can optionally connect to one central node at most.
//!
//! ### Node Operation Modes
//! The operation mode determines how the node operates in terms of Wi-Fi features and interactions with other nodes. The supported operation modes are:
//! 1) **ESP-NOW**
//! 2) **Wi-Fi Station** (Central only)
//! 3) **Wi-Fi Sniffer** (Peripheral only)
//!
//! ### Collection Modes
//! 1) **Collector**: A collector node collects and provides CSI data output from one or more devices.
//! 2) **Listener**: A listener is a passive node. It only enables CSI collection and does not provide any CSI output.
//!
//! A collector node typically is the one that actively processes CSI data. A listener on the other hand typically keeps CSI traffic flowing but does not process CSI data.
//!
//! ## Collection Network Architechtures
//! As ahown earlier, `esp-csi-rs` allows you to configure a device to one several operational modes including ESP-NOW, WiFi station, or WiFi sniffer. As such, `esp-csi-rs` supports several network setups allowing for flexibility in collecting CSI data. Some possible setups including the following:
//!
//! 1. ***Single Node:***  This is the simplest setup where only one ESP device (CSI Node) is needed. The node is configured to "sniff" packets in surrounding networks and collect CSI data. The WiFi Sniffer Peripheral Collector is the only configuration that supports this topology.
//! 2. ***Point-to-Point:*** This set up uses two CSI Nodes, a central and a peripheral. One of them can be a collector and the other a listener. Alternatively, both can be collectors as well. Some configuration examples include
//!     - **WiFi Station Central Collector <-> Access Point/Commercial Router**: In this configuration the CSI node can connect to any WiFi Access Point like an ESP AP or a commercial router. The node in turn sends traffic to the Access Point to acquire CSI data.
//!     - **ESP-NOW Central Listener/Collector <-> ESP-NOW Peripheral Listener/Collector**: In this configuration a CSI central node connects to one other ESP-NOW peripheral node. Both ESP-NOW peripheral and central nodes can operate either as listeners or collectors.
//! 3. ***Star:*** In this architechture a central node connects to several peripheral nodes. The central node triggers traffic and aggregates CSI sent back from peripheral nodes. Alternatively, CSI can be collected by the individual peripherals. Only the ESP-NOW operation mode supports this architechture. The ESP-NOW peripheral and central nodes can also operate either as listeners or collectors.
//!
//! ## Output Formats & Logging Modes
//! `esp-csi-rs` is able to print CSI data in several formats. The output format can be configured when initializing the logger. The supported formats include:
//! - **LogMode::ArrayList**: This prints CSI data as an array, where the array represents the CSI values for a received packet. This format is more compact and easier to read for large volumes of CSI data.
//!
//! Example output:
//! ```
//! [3916,-93,11,157,1,1815804,256,0,260,2,0,1,1,128,0,1,1,0,1,0,0,0,256,128,[...]]
//! ```
//! The array fields map to the [`CSIDataPacket`] struct fields in the following order:
//!
//! | Index | Field | Description |
//! |-------|-------|-------------|
//! | 0 | `sequence_number` | Sequence number of the packet that triggered the CSI capture |
//! | 1 | `rssi` | Received Signal Strength Indicator (dBm) |
//! | 2 | `rate` | PHY rate encoding (valid for non-HT / 802.11b/g packets) |
//! | 3 | `noise_floor` | Noise floor of the RF module (dBm) |
//! | 4 | `channel` | Primary channel on which the packet was received |
//! | 5 | `timestamp` | Local timestamp when the packet was received (microseconds) |
//! | 6 | `sig_len` | Length of the packet including Frame Check Sequence (FCS) |
//! | 7 | `rx_state` | Reception state: `0` = no error, non-zero = error code |
//! | 8 | `secondary_channel` | Secondary channel: `0` = none, `1` = above, `2` = below *(non-ESP32-C6 only)* |
//! | 9 | `sgi` | Short Guard Interval: `0` = Long GI, `1` = Short GI *(non-ESP32-C6 only)* |
//! | 10 | `antenna` | Antenna number: `0` = antenna 0, `1` = antenna 1 *(non-ESP32-C6 only)* |
//! | 11 | `ampdu_cnt` | Number of subframes aggregated in AMPDU *(non-ESP32-C6 only)* |
//! | 12 | `sig_mode` | Protocol: `0` = non-HT (11b/g), `1` = HT (11n), `3` = VHT (11ac) *(non-ESP32-C6 only)* |
//! | 13 | `mcs` | Modulation Coding Scheme; for HT packets ranges from 0 (MCS0) to 76 (MCS76) *(non-ESP32-C6 only)* |
//! | 14 | `bandwidth` | Channel bandwidth: `0` = 20 MHz, `1` = 40 MHz *(non-ESP32-C6 only)* |
//! | 15 | `smoothing` | Channel estimate smoothing: `0` = unsmoothed, `1` = smoothing recommended *(non-ESP32-C6 only)* |
//! | 16 | `not_sounding` | Sounding PPDU flag: `0` = sounding PPDU, `1` = not a sounding PPDU *(non-ESP32-C6 only)* |
//! | 17 | `aggregation` | Aggregation type: `0` = MPDU, `1` = AMPDU *(non-ESP32-C6 only)* |
//! | 18 | `stbc` | Space-Time Block Code: `0` = non-STBC, `1` = STBC *(non-ESP32-C6 only)* |
//! | 19 | `fec_coding` | Forward Error Correction / LDPC flag; set for 11n LDPC packets *(non-ESP32-C6 only)* |
//! | 20 | `sig_len` | Packet length including FCS (repeated) |
//! | 21 | `csi_data_len` | Length of the raw CSI data (number of `i8` samples) |
//! | 22 | `[csi_data]` | Inner array of raw CSI `i8` samples |
//!
//! - **LogMode::Text**: This output prints CSI data in a more verbose, human-readable format. This includes additional metadata and explanations alongside the raw CSI values, making it easier to understand the context of each packet's CSI data.
//!
//! Example output:
//! ```rust
//! mac: 56:6C:EB:6F:BC:3D
//! sequence number: 426
//! rssi: -82
//! rate: 11
//! noise floor: 165
//! channel: 1
//! timestamp: 2424915
//! sig len: 332
//! rx state: 0
//! dump len: 336
//! he sigb len: 2
//! cur single mpdu: 0
//! cur bb format: 1
//! rx channel estimate info vld: 1
//! rx channel estimate len: 128
//! time seconds: 0
//! channel: 1
//! is group: 1
//! rxend state: 0
//! rxmatch3: 1
//! rxmatch2: 0
//! rxmatch1: 0
//! rxmatch0: 0
//! sig_len: 332
//! data length: 128
//! csi raw data: [0, 0, 0, 0, 0, 0, 0, 0, -6, 0, 6, 0, -24, 10, -23, 9, -23, 8, -23, 7, -22, 6, -22, 5, -22, 6, -23, 5, -22, 6, -22, 6, -22, 7, -20, 7, -19, 9, -19, 10, -19, 12, -19, 12, -18, 14, -19, 14, -19, 16, -20, 17, -21, 18, -20, 18, -19, 18, -16, 18, -14, 19, -13, 18, 0, 0, -19, 22, -20, 22, -20, 22, -20, 21, -21, 19, -22, 18, -20, 16, -18, 16, -17, 15, -16, 15, -14, 15, -13, 13, -12, 13, -9, 13, -7, 14, -6, 14, -5, 13, -3, 12, 0, 13, 2, 12, 3, 12, 5, 12, 7, 13, 8, 13, 10, 13, 12, 14, 9, 1, -5, -4, 0, 0, 0, 0, 0, 0]
//! ```
//! - **LogMode::Serialized**: This mode serializes the `CSIDataPacket` structure and prints it in a serialized COBS format. This is a compact binary format that can be parsed by and serde compatible crate like [postcard](https://crates.io/crates/postcard). It is not human-readable but is efficient for logging large amounts of CSI data on the host without overwhelming the console output.
//!
//!
//!
//! ### Example for creating WiFi Station Central Collector
//! There are more examples in the repository. The example below demonstrates how to collect CSI data with an ESP configured in WIFI Station mode.
//!
//! #### Step 1: Initialize Logger
//! ```rust
//! init_logger(spawner, LogMode::ArrayList);
//! ```
//! #### Step 2: Create a Hardware Instance for the CSI Node
//! ```rust
//! let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
//! ```
//! #### Step 3: Create a Station Configuration
//! ```rust
//! let client_config = ClientConfig::default()
//!     .with_ssid("SSID".to_string())
//!     .with_password("PASS".to_string())
//!     .with_auth_method(esp_radio::wifi::AuthMethod::Wpa2Personal);
//!
//! let station_config = WifiStationConfig {
//!    client_config,  // Pass the config we created above
//! };
//! ```
//! #### Step 3: Create a CSI Collection Node Instance with the Desired Configuration
//! ```rust
//! let mut node = CSINode::new(
//!     esp_csi_rs::Node::Central(esp_csi_rs::CentralOpMode::WifiStation(station_config)),
//!     CollectionMode::Collector,
//!     Some(CsiConfig::default()),
//!     Some(100),
//!     csi_hardware,
//! );
//! ```
//! #### Step 4: Create a CSI Node Client to Control the Node and Receive CSI Data
//! ```rust
//! let mut node_handle = CSINodeClient::new();
//! ```
//! #### Step 5: Create Handler for Printing for Certain Duration
//! ```rust
//! node.run_duration(1000, &mut node_handle).await;
//! ```
//!

#![no_std]

use portable_atomic::AtomicI64;

use zerocopy::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use embassy_futures::join::{join, join3};
use embassy_futures::select::{select, select3, Either, Either3};

use embassy_time::{with_timeout, Duration, Instant, Timer};
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::{ClientConfig, CsiConfig, Interfaces, Protocol, WifiController};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, TrySendError};
use embassy_sync::signal::Signal;

use heapless::{LinearMap, Vec};
extern crate alloc;
use serde::{Deserialize, Serialize};

pub mod central;
pub mod config;
pub mod csi;
pub mod esp_now_pool;
pub mod logging;
pub mod peripheral;
pub mod time;

use crate::central::esp_now::run_esp_now_central;
use crate::central::sta::{run_sta_connect, sta_init};
use crate::config::CsiConfig as CsiConfiguration;
use crate::csi::{CSIDataPacket, RxCSIFmt};
use crate::peripheral::esp_now::run_esp_now_peripheral;

const PROC_CSI_CH_CAPACITY: usize = 32;
const MAX_TRACKED_PEERS: usize = 16;

// Single SPSC channel: ISR (capture_csi_info) is the only producer,
// CSINodeClient is the only consumer.
static CSI_PACKET: Channel<CriticalSectionRawMutex, CSIDataPacket, PROC_CSI_CH_CAPACITY> =
    Channel::new();

static IS_COLLECTOR: AtomicBool = AtomicBool::new(false);
// CSI publish gate. The WiFi callback checks this in a single relaxed load
// to decide whether to build and emit a CSIDataPacket.
//
// Decoupled from `IS_COLLECTOR` on purpose: `CollectionMode` controls the
// ESP-NOW responder/initiator behavior (Listener stays passive on TX), but
// it must NOT block a `CSINodeClient` from reading CSI — that conflation
// silently breaks sniffer + Listener configurations where the user wants
// to passively read CSI without participating in any control protocol.
static CSI_PUBLISH_ENABLED: AtomicBool = AtomicBool::new(false);
static COLLECTION_MODE_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Enable or disable CSI publishing.
///
/// When disabled, the WiFi CSI callback returns immediately at the gate check
/// — no formatting, no enqueue, no print. Use this to keep `log_ln!` output
/// visible without the per-packet CSI line flood, while leaving the WiFi
/// stack and stats counters running normally.
///
/// `init_logger` enables this automatically in sync mode (no `async-print`
/// feature) because the WiFi callback formats and writes CSI lines directly
/// in that mode. In async-print mode `CSINodeClient::get_csi_data` enables
/// it lazily on first await.
///
/// Call this AFTER `init_logger` to override either default.
pub fn set_csi_logging_enabled(enabled: bool) {
    CSI_PUBLISH_ENABLED.store(enabled, Ordering::Release);
}

/// Returns whether CSI publishing is currently enabled.
pub fn csi_logging_enabled() -> bool {
    CSI_PUBLISH_ENABLED.load(Ordering::Relaxed)
}
// Signals run_process_csi_packet to clear PEER_SEQ_TRACKER on the next ISR entry.
#[cfg(feature = "statistics")]
static RESET_SEQ_TRACKER: AtomicBool = AtomicBool::new(false);
static CENTRAL_MAGIC_NUMBER: u32 = 0xA8912BF0;
static PERIPHERAL_MAGIC_NUMBER: u32 = !CENTRAL_MAGIC_NUMBER;
#[cfg(feature = "statistics")]
static SEQ_DROP_DETECTION_ENABLED: AtomicBool = AtomicBool::new(false);

use portable_atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
/// Global statistics counters (enabled with the `statistics` feature).
#[cfg(feature = "statistics")]
struct GlobalStats {
    /// Total transmitted packets.
    tx_count: AtomicU64,
    /// Total received packets.
    rx_count: AtomicU64,
    /// Estimated number of dropped RX packets.
    rx_drop_count: AtomicU32,
    /// Capture start time (ticks).
    capture_start_time: AtomicU64,
    /// Current TX packet rate (Hz).
    tx_rate_hz: AtomicU32,
    /// Current RX packet rate (Hz).
    rx_rate_hz: AtomicU32,
    /// One-way latency (microseconds).
    one_way_latency: AtomicI64,
    /// Two-way latency (microseconds).
    two_way_latency: AtomicI64,
}

#[cfg(feature = "statistics")]
static STATS: GlobalStats = GlobalStats {
    tx_count: AtomicU64::new(0),
    rx_count: AtomicU64::new(0),
    rx_drop_count: AtomicU32::new(0),
    capture_start_time: AtomicU64::new(0),
    tx_rate_hz: AtomicU32::new(0),
    rx_rate_hz: AtomicU32::new(0),
    one_way_latency: AtomicI64::new(0),
    two_way_latency: AtomicI64::new(0),
};
// static GLOBAL_PACKET_RX_DROP_COUNT: AtomicU32 = AtomicU32::new(0);
// static GLOBAL_PACKET_TX_COUNT: AtomicU64 = AtomicU64::new(0);
// static GLOBAL_PACKET_RX_COUNT: AtomicU64 = AtomicU64::new(0);
// static GLOBAL_CAPTURE_START_TIME: AtomicU64 = AtomicU64::new(0);
// static TX_RATE_HZ: AtomicU32 = AtomicU32::new(0);
// static RX_RATE_HZ: AtomicU32 = AtomicU32::new(0);
// static TWO_WAY_LATENCY: AtomicI64 = AtomicI64::new(0);
// static ONE_WAY_LATENCY: AtomicI64 = AtomicI64::new(0);

// Signals
static STOP_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Internal fucntion to change collection mode at runtime (e.g. Central can signal Peripheral to start/stop collecting CSI).
fn set_runtime_collection_mode(is_collector: bool) {
    IS_COLLECTOR.store(is_collector, Ordering::Relaxed);
    COLLECTION_MODE_CHANGED.signal(());
}

fn set_seq_drop_detection(enabled: bool) {
    #[cfg(feature = "statistics")]
    {
        SEQ_DROP_DETECTION_ENABLED.store(enabled, Ordering::Relaxed);
    }

    #[cfg(not(feature = "statistics"))]
    {
        let _ = enabled;
    }
}

fn seq_drop_detection_enabled() -> bool {
    #[cfg(feature = "statistics")]
    {
        SEQ_DROP_DETECTION_ENABLED.load(Ordering::Relaxed)
    }

    #[cfg(not(feature = "statistics"))]
    {
        false
    }
}

// Drives the per-`run_duration` lifecycle. In async-print mode this is the
// loop that pulls packets out of `CSI_PACKET` and forwards them to the
// logger task — without it, the channel would fill and packets would drop.
// In sync mode the WiFi callback writes inline, so we just wait for the
// duration and then signal stop.
#[cfg(feature = "async-print")]
async fn csi_data_collection(client: &mut CSINodeClient, duration: u64) {
    with_timeout(Duration::from_secs(duration), async {
        loop {
            client.print_csi_w_metadata().await;
        }
    })
    .await
    .unwrap_err();
    client.send_stop().await;
}

#[cfg(not(feature = "async-print"))]
async fn csi_data_collection(client: &mut CSINodeClient, duration: u64) {
    Timer::after(Duration::from_secs(duration)).await;
    client.send_stop().await;
}

async fn wait_for_stop() {
    STOP_SIGNAL.wait().await;
    STOP_SIGNAL.signal(());
}

async fn stop_after_duration(duration: u64) {
    match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_secs(duration))).await {
        Either::First(_) | Either::Second(_) => STOP_SIGNAL.signal(()),
    }
}

/// Configuration for ESP-NOW traffic generation.
///
/// Used by both Central and Peripheral nodes when operating in ESP-NOW mode.
pub struct EspNowConfig {
    phy_rate: WifiPhyRate,
    channel: u8,
}

impl Default for EspNowConfig {
    fn default() -> Self {
        Self {
            phy_rate: WifiPhyRate::RateMcs0Lgi,
            channel: 11,
        }
    }
}

/// Configuration for Wi-Fi Promiscuous Sniffer mode.
#[derive(Debug, Clone)]
pub struct WifiSnifferConfig {
    mac_filter: Option<[u8; 6]>,
    /// 2.4 GHz channel (1–14) the sniffer should lock to.
    pub channel: u8,
}

impl Default for WifiSnifferConfig {
    fn default() -> Self {
        Self {
            mac_filter: None,
            channel: 11,
        }
    }
}

/// Configuration for Wi-Fi Station mode.
#[derive(Debug, Clone)]
pub struct WifiStationConfig {
    /// Underlying esp-radio client configuration (SSID, auth, etc.).
    pub client_config: ClientConfig,
}

// Enum for Central modes, each wrapping its specific config.

/// Central node operational modes.
pub enum CentralOpMode {
    /// Drive an ESP-NOW exchange with a peripheral node.
    EspNow(EspNowConfig),
    /// Associate as a Wi-Fi station to harvest CSI from received frames.
    WifiStation(WifiStationConfig),
}

// Enum for Peripheral modes, each wrapping its specific config.
/// Peripheral node operational modes.
pub enum PeripheralOpMode {
    /// Reply to a central's ESP-NOW control frames.
    EspNow(EspNowConfig),
    /// Run as a Wi-Fi promiscuous sniffer; CSI is captured from every
    /// frame received on the locked channel.
    WifiSniffer(WifiSnifferConfig),
}

/// High-level node type and mode.
pub enum Node {
    /// Run as the peripheral side of the chosen [`PeripheralOpMode`].
    Peripheral(PeripheralOpMode),
    /// Run as the central side of the chosen [`CentralOpMode`].
    Central(CentralOpMode),
}

/// CSI collection behavior for the node.
///
/// Use `Listener` to keep CSI traffic flowing without processing packets,
/// or `Collector` to actively process CSI data. Note: `Listener` combined with
/// a sniffer node makes the sniffer effectively useless because no CSI data is
/// processed.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum CollectionMode {
    /// Enables CSI collection and processes CSI data.
    Collector,
    /// Enables CSI collection but does not process CSI data.
    Listener,
}

/// Controls whether TX and RX tasks are active for a node.
///
/// Defaults to both TX and RX enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IOTaskConfig {
    /// Enable transmit-side task work for the selected operation mode.
    pub tx_enabled: bool,
    /// Enable receive/process-side task work for the selected operation mode.
    pub rx_enabled: bool,
}

impl IOTaskConfig {
    /// Create a task configuration with explicit TX/RX state.
    pub const fn new(tx_enabled: bool, rx_enabled: bool) -> Self {
        Self {
            tx_enabled,
            rx_enabled,
        }
    }
}

impl Default for IOTaskConfig {
    fn default() -> Self {
        Self::new(true, true)
    }
}

/// Hardware handles required to operate a CSI node.
pub struct CSINodeHardware<'a> {
    interfaces: &'a mut Interfaces<'static>,
    controller: &'a mut WifiController<'static>,
}

impl<'a> CSINodeHardware<'a> {
    /// Create a hardware bundle from the Wi-Fi `Interfaces` and `WifiController`.
    pub fn new(
        interfaces: &'a mut Interfaces<'static>,
        controller: &'a mut WifiController<'static>,
    ) -> Self {
        Self {
            interfaces,
            controller,
        }
    }
}

/// Client helper to receive CSI packets via the global SPSC channel.
pub struct CSINodeClient {
    _private: (),
}

impl CSINodeClient {
    /// Create a new CSI node client.
    ///
    /// In async-print mode the publish gate is opened lazily on the first
    /// `get_csi_data()` await, so constructing a client purely for
    /// `send_stop()` does not cause the WiFi callback to enqueue packets.
    /// In sync mode CSI publishing is controlled by `init_logger` /
    /// `set_csi_logging_enabled` — `CSINodeClient` only owns the stop signal.
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Wait for the next CSI packet.
    ///
    /// Only available with the `async-print` feature: in sync mode the WiFi
    /// callback formats and writes CSI lines directly without enqueueing,
    /// so there is no channel to drain.
    #[cfg(feature = "async-print")]
    pub async fn get_csi_data(&mut self) -> CSIDataPacket {
        // Lazily open the publish gate on first wait so the WiFi callback
        // starts enqueueing. Idempotent for subsequent calls.
        set_csi_logging_enabled(true);
        CSI_PACKET.receive().await
    }

    /// Receive and print CSI data with metadata (uses crate logging).
    ///
    /// Only available with the `async-print` feature: in sync mode the CSI
    /// line is emitted directly from the WiFi callback (`capture_csi_info`)
    /// — there is nothing for user code to drive. Use
    /// `set_csi_logging_enabled(false)` to suppress sync-mode output.
    #[cfg(feature = "async-print")]
    pub async fn print_csi_w_metadata(&mut self) {
        let packet = self.get_csi_data().await;
        packet.print_csi_w_metadata();
        embassy_futures::yield_now().await;
    }

    /// Signal the running node to stop.
    pub async fn send_stop(&self) {
        STOP_SIGNAL.signal(());
    }
}

/// Control packet sent from Central to Peripheral.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ControlPacket {
    magic_number: u32,
    /// Whether the central is currently in collector mode; the peripheral
    /// mirrors this flag to keep the pair in sync.
    pub is_collector: bool,
    /// Microseconds-since-boot timestamp captured when the central queued
    /// this packet for transmit.
    pub central_send_uptime: u64,
    /// Latency offset (μs) the central has observed for this peer; sent
    /// so the peripheral can compensate when stamping its reply.
    pub latency_offset: i64,
    /// Monotonic sequence number used to detect drops/reordering.
    pub sequence_number: u32,
}

impl ControlPacket {
    /// Create a new control packet with collector flag, latency offset, and sequence number.
    pub fn new(is_collector: bool, latency_offset: i64, sequence_number: u32) -> Self {
        Self {
            magic_number: CENTRAL_MAGIC_NUMBER.into(),
            is_collector,
            central_send_uptime: Instant::now().as_micros(),
            latency_offset,
            sequence_number,
        }
    }
}

/// Peripheral reply packet for latency/telemetry exchange.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct PeripheralPacket {
    magic_number: u32,        // Magic number to identify packet type
    recv_uptime: u64,         // When Peripheral received the Control Packet
    send_uptime: u64, // When Peripheral sent the Peripheral Packet (after receiving Control Packet)
    central_send_uptime: u64, // When Central sent the Control Packet
}

impl PeripheralPacket {
    /// Create a new peripheral packet using timestamps captured locally.
    pub fn new(recv_uptime: u64, central_send_uptime: u64) -> Self {
        Self {
            magic_number: PERIPHERAL_MAGIC_NUMBER,
            recv_uptime,
            send_uptime: Instant::now().as_micros(),
            central_send_uptime,
        }
    }
}

fn reset_globals() {
    #[cfg(feature = "statistics")]
    {
        STATS.tx_count.store(0, Ordering::Relaxed);
        STATS.rx_count.store(0, Ordering::Relaxed);
        STATS.rx_drop_count.store(0, Ordering::Relaxed);
        STATS.tx_rate_hz.store(0, Ordering::Relaxed);
        STATS.rx_rate_hz.store(0, Ordering::Relaxed);
        STATS.one_way_latency.store(0, Ordering::Relaxed);
        STATS.two_way_latency.store(0, Ordering::Relaxed);
    }
    #[cfg(feature = "statistics")]
    reset_global_log_drops();
}

/// Primary orchestration object for CSI collection.
///
/// Construct a node with `CSINode::new` or `CSINode::new_central_node`, configure
/// optional protocol/rate/traffic frequency, then call `run()`.
pub struct CSINode<'a> {
    kind: Node,
    collection_mode: CollectionMode,
    io_tasks: IOTaskConfig,
    /// CSI Configuration
    csi_config: Option<CsiConfiguration>,
    /// Traffic Generation Frequency
    traffic_freq_hz: Option<u16>,
    hardware: CSINodeHardware<'a>,
    protocol: Option<Protocol>,
    rate: Option<WifiPhyRate>,
}

impl<'a> CSINode<'a> {
    /// Create a new node with explicit `Node` kind.
    pub fn new(
        kind: Node,
        collection_mode: CollectionMode,
        csi_config: Option<CsiConfiguration>,
        traffic_freq_hz: Option<u16>,
        hardware: CSINodeHardware<'a>,
    ) -> Self {
        Self {
            kind,
            collection_mode,
            io_tasks: IOTaskConfig::default(),
            csi_config,
            traffic_freq_hz,
            hardware,
            protocol: None,
            rate: Some(WifiPhyRate::RateMcs7Lgi),
        }
    }

    /// Convenience constructor for a central node.
    pub fn new_central_node(
        op_mode: CentralOpMode,
        collection_mode: CollectionMode,
        csi_config: Option<CsiConfiguration>,
        traffic_freq_hz: Option<u16>,
        hardware: CSINodeHardware<'a>,
    ) -> Self {
        Self {
            kind: Node::Central(op_mode),
            collection_mode,
            io_tasks: IOTaskConfig::default(),
            csi_config,
            traffic_freq_hz,
            hardware,
            protocol: None,
            rate: Some(WifiPhyRate::RateMcs7Lgi),
        }
    }

    /// Get the node type and operation mode.
    pub fn get_node_type(&self) -> &Node {
        &self.kind
    }

    /// Get the current collection mode.
    pub fn get_collection_mode(&self) -> CollectionMode {
        self.collection_mode
    }

    /// If central, return the active central op mode.
    pub fn get_central_op_mode(&self) -> Option<&CentralOpMode> {
        match &self.kind {
            Node::Central(mode) => Some(mode),
            Node::Peripheral(_) => None,
        }
    }

    /// If peripheral, return the active peripheral op mode.
    pub fn get_peripheral_op_mode(&self) -> Option<&PeripheralOpMode> {
        match &self.kind {
            Node::Peripheral(mode) => Some(mode),
            Node::Central(_) => None,
        }
    }

    /// Update CSI configuration.
    pub fn set_csi_config(&mut self, config: CsiConfiguration) {
        self.csi_config = Some(config);
    }

    /// Update Wi-Fi Station configuration (only applies to central station mode).
    pub fn set_station_config(&mut self, config: WifiStationConfig) {
        if let Node::Central(CentralOpMode::WifiStation(_)) = &mut self.kind {
            self.kind = Node::Central(CentralOpMode::WifiStation(config));
        }
    }

    /// Set traffic generation frequency in Hz (ESP-NOW modes).
    pub fn set_traffic_frequency(&mut self, freq_hz: u16) {
        self.traffic_freq_hz = Some(freq_hz);
    }

    /// Set collection mode for the node.
    pub fn set_collection_mode(&mut self, mode: CollectionMode) {
        self.collection_mode = mode;
    }

    /// Set TX/RX task enablement for the node.
    pub fn set_io_tasks(&mut self, io_tasks: IOTaskConfig) {
        self.io_tasks = io_tasks;
    }

    /// Enable or disable TX task work.
    pub fn set_tx_enabled(&mut self, enabled: bool) {
        self.io_tasks.tx_enabled = enabled;
    }

    /// Enable or disable RX task work.
    pub fn set_rx_enabled(&mut self, enabled: bool) {
        self.io_tasks.rx_enabled = enabled;
    }

    /// Get current TX/RX task configuration.
    pub fn get_io_tasks(&self) -> IOTaskConfig {
        self.io_tasks
    }

    /// Replace the node kind/mode.
    pub fn set_op_mode(&mut self, mode: Node) {
        self.kind = mode;
    }

    /// Set Wi-Fi protocol (overrides default).
    pub fn set_protocol(&mut self, protocol: Protocol) {
        self.protocol = Some(protocol);
    }

    /// Set Wi-Fi PHY data rate for ESP-NOW traffic.
    pub fn set_rate(&mut self, rate: WifiPhyRate) {
        self.rate = Some(rate);
    }

    /// Run the node until duration in seconds with internal collection.
    ///
    /// This initializes Wi-Fi, configures CSI, and starts mode-specific tasks.
    pub async fn run_duration(&mut self, duration: u64, mut client: &mut CSINodeClient) {
        let interfaces = &mut self.hardware.interfaces;
        let controller = &mut self.hardware.controller;

        // Tasks Necessary for Central Station & Sniffer
        let sta_interface = if let Node::Central(CentralOpMode::WifiStation(config)) = &self.kind {
            Some(sta_init(&mut interfaces.sta, config, controller))
        } else {
            None
        };

        // Set Wi-Fi mode to Station for all node types
        controller.set_mode(esp_radio::wifi::WifiMode::Sta).unwrap();

        // Build CSI Configuration
        let config = match self.csi_config {
            Some(ref config) => {
                log_ln!("CSI Configuration Set: {:?}", config);
                build_csi_config(config)
            }
            None => {
                let default_config = CsiConfiguration::default();
                log_ln!(
                    "No CSI Configuration Provided. Going with defaults: {:?}",
                    default_config
                );
                build_csi_config(&default_config)
            }
        };

        // Apply Protocol if specified
        if let Some(protocol) = self.protocol.take() {
            let old_protocol = reconstruct_protocol(&protocol);
            controller.set_protocol(protocol.into()).unwrap();
            self.protocol = Some(old_protocol);
        }

        // Start the controller
        controller.start_async().await.unwrap();
        log_ln!("Wi-Fi Controller Started");
        let is_collector = self.collection_mode == CollectionMode::Collector;
        IS_COLLECTOR.store(is_collector, Ordering::Relaxed);
        set_seq_drop_detection(matches!(
            &self.kind,
            Node::Peripheral(PeripheralOpMode::EspNow(_))
                | Node::Central(CentralOpMode::EspNow(_))
        ));

        // Replace esp-radio's heap-allocating ESP-NOW receive dispatcher with
        // our static-pool variant *before* CSI starts. If we waited until the
        // mode-specific arm runs (after `set_csi`), the CSI callback could
        // already have begun its UART spin and a vendor frame arriving in
        // that window would still hit esp-radio's `rcv_cb` and risk the
        // 384 B grow panic. Doing it here covers all peripheral/central
        // EspNow modes; sniffer modes also call `esp_now_unregister_recv_cb`
        // later which overrides this — also fine.
        crate::esp_now_pool::install();

        // Set Peripheral/Central to Collect CSI. Keep a clone so the STA
        // recovery path in run_sta_connect can re-apply after a stop/start
        // cycle (stop clears the CSI filter/callback).
        let csi_config_for_recovery = config.clone();
        set_csi(controller, config);
        let sniffer: &esp_radio::wifi::Sniffer<'_> = &interfaces.sniffer;

        // Initialize Nodes based on type
        match &self.kind {
            Node::Peripheral(op_mode) => match op_mode {
                PeripheralOpMode::EspNow(esp_now_config) => {
                    // Initialize as Peripheral node with EspNowConfig
                    if let Some(rate) = self.rate.take() {
                        let old_rate = reconstruct_wifi_rate(&rate);
                        let _ = interfaces.esp_now.set_rate(rate);
                        self.rate = Some(old_rate);
                    }

                    let main_task = run_esp_now_peripheral(
                        &mut interfaces.esp_now,
                        esp_now_config,
                        self.traffic_freq_hz,
                        self.io_tasks,
                    );
                    if self.io_tasks.rx_enabled {
                        join3(
                            main_task,
                            run_process_csi_packet(),
                            csi_data_collection(client, duration),
                        )
                        .await;
                    } else {
                        join3(main_task, wait_for_stop(), stop_after_duration(duration)).await;
                    }
                }
                PeripheralOpMode::WifiSniffer(sniffer_config) => {
                    interfaces
                        .esp_now
                        .set_channel(sniffer_config.channel)
                        .unwrap();
                    // Drop the ESP-NOW receive callback at the C layer.
                    // Rationale: in Rust esp-radio, `rcv_cb` runs for every
                    // ESP-NOW vendor action frame the 802.11 MAC sees and
                    // unconditionally `Box::new`s the payload + push_back's
                    // to a heap-backed VecDeque. While the sync CSI callback
                    // CPU-spins UART for ~11 ms per line, those frames pile
                    // up inside the WiFi task; once the spin returns, rcv_cb
                    // burst-fires hundreds of allocs back-to-back, fragmenting
                    // the heap until a 384 B VecDeque grow fails → panic.
                    // Hernandez never hits this because in C, no recv_cb is
                    // registered → frames are silently dropped at the C layer
                    // with zero allocation. Replicate that here for sniffer
                    // mode (we don't consume ESP-NOW data anyway).
                    extern "C" {
                        fn esp_now_unregister_recv_cb() -> i32;
                    }
                    unsafe {
                        let _ = esp_now_unregister_recv_cb();
                    }
                    let sniffer = &interfaces.sniffer;
                    sniffer.set_promiscuous_mode(true).unwrap();
                    if self.io_tasks.rx_enabled {
                        join(
                            run_process_csi_packet(),
                            csi_data_collection(client, duration),
                        )
                        .await;
                        run_process_csi_packet().await;
                    } else {
                        stop_after_duration(duration).await;
                    }
                    sniffer.set_promiscuous_mode(false).unwrap();
                }
            },
            Node::Central(op_mode) => match op_mode {
                CentralOpMode::EspNow(esp_now_config) => {
                    // Initialize as Central node with EspNowConfig
                    if let Some(rate) = self.rate.take() {
                        let old_rate = reconstruct_wifi_rate(&rate);
                        let _ = interfaces.esp_now.set_rate(rate);
                        self.rate = Some(old_rate);
                    }

                    let main_task = run_esp_now_central(
                        &mut interfaces.esp_now,
                        interfaces.sta.mac_address(),
                        esp_now_config,
                        self.traffic_freq_hz,
                        is_collector,
                        self.io_tasks,
                    );
                    if self.io_tasks.rx_enabled {
                        join3(
                            main_task,
                            run_process_csi_packet(),
                            csi_data_collection(client, duration),
                        )
                        .await;
                    } else {
                        join3(main_task, wait_for_stop(), stop_after_duration(duration)).await;
                    }
                }
                CentralOpMode::WifiStation(sta_config) => {
                    // Initialize as Wifi Station Collector with WifiStationConfig
                    // 1. Connect to Wi-Fi network, etc.
                    // 2. Run DHCP, NTP sync if enabled in config, etc.
                    // 3. Spawn STA Connection Handling Task
                    // 4. Spawn STA Network Operation Task
                    let (sta_stack, sta_runner) = sta_interface.unwrap();

                    let main_task = run_sta_connect(
                        controller,
                        self.traffic_freq_hz,
                        sta_stack,
                        sta_runner,
                        csi_config_for_recovery,
                        self.io_tasks,
                    );
                    if self.io_tasks.rx_enabled {
                        join3(
                            main_task,
                            run_process_csi_packet(),
                            csi_data_collection(client, duration),
                        )
                        .await;
                    } else {
                        join3(main_task, wait_for_stop(), stop_after_duration(duration)).await;
                    }
                }
            },
        }

        STOP_SIGNAL.reset();
        let _ = controller.stop_async().await;
        reset_globals();
    }

    /// Run the node until stopped.
    ///
    /// This initializes Wi-Fi, configures CSI, and starts mode-specific tasks.
    pub async fn run(&mut self) {
        let interfaces = &mut self.hardware.interfaces;
        let controller = &mut self.hardware.controller;

        // Tasks Necessary for Central Station & Sniffer
        let sta_interface = if let Node::Central(CentralOpMode::WifiStation(config)) = &self.kind {
            Some(sta_init(&mut interfaces.sta, config, controller))
        } else {
            None
        };

        // Set Wi-Fi mode to Station for all node types
        controller.set_mode(esp_radio::wifi::WifiMode::Sta).unwrap();

        // Build CSI Configuration
        let config = match self.csi_config {
            Some(ref config) => {
                log_ln!("CSI Configuration Set: {:?}", config);
                build_csi_config(config)
            }
            None => {
                let default_config = CsiConfiguration::default();
                log_ln!(
                    "No CSI Configuration Provided. Going with defaults: {:?}",
                    default_config
                );
                build_csi_config(&default_config)
            }
        };

        // Apply Protocol if specified
        if let Some(protocol) = self.protocol.take() {
            let old_protocol = reconstruct_protocol(&protocol);
            controller.set_protocol(protocol.into()).unwrap();
            self.protocol = Some(old_protocol);
        }

        // Start the controller
        controller.start_async().await.unwrap();
        log_ln!("Wi-Fi Controller Started");
        let is_collector = self.collection_mode == CollectionMode::Collector;
        IS_COLLECTOR.store(is_collector, Ordering::Relaxed);
        set_seq_drop_detection(matches!(
            &self.kind,
            Node::Peripheral(PeripheralOpMode::EspNow(_))
                | Node::Central(CentralOpMode::EspNow(_))
        ));

        // Replace esp-radio's heap-allocating ESP-NOW receive dispatcher with
        // our static-pool variant *before* CSI starts. If we waited until the
        // mode-specific arm runs (after `set_csi`), the CSI callback could
        // already have begun its UART spin and a vendor frame arriving in
        // that window would still hit esp-radio's `rcv_cb` and risk the
        // 384 B grow panic. Doing it here covers all peripheral/central
        // EspNow modes; sniffer modes also call `esp_now_unregister_recv_cb`
        // later which overrides this — also fine.
        crate::esp_now_pool::install();

        // Set Peripheral/Central to Collect CSI. Keep a clone so the STA
        // recovery path in run_sta_connect can re-apply after a stop/start
        // cycle (stop clears the CSI filter/callback).
        let csi_config_for_recovery = config.clone();
        set_csi(controller, config);
        let sniffer: &esp_radio::wifi::Sniffer<'_> = &interfaces.sniffer;

        // Initialize Nodes based on type
        match &self.kind {
            Node::Peripheral(op_mode) => match op_mode {
                PeripheralOpMode::EspNow(esp_now_config) => {
                    // Initialize as Peripheral node with EspNowConfig
                    if let Some(rate) = self.rate.take() {
                        let old_rate = reconstruct_wifi_rate(&rate);
                        let _ = interfaces.esp_now.set_rate(rate);
                        self.rate = Some(old_rate);
                    }

                    let main_task = run_esp_now_peripheral(
                        &mut interfaces.esp_now,
                        esp_now_config,
                        self.traffic_freq_hz,
                        self.io_tasks,
                    );
                    if self.io_tasks.rx_enabled {
                        join(main_task, run_process_csi_packet()).await;
                    } else {
                        join(main_task, wait_for_stop()).await;
                    }
                }
                PeripheralOpMode::WifiSniffer(sniffer_config) => {
                    interfaces
                        .esp_now
                        .set_channel(sniffer_config.channel)
                        .unwrap();
                    // See the sniffer-Collector arm above for rationale.
                    extern "C" {
                        fn esp_now_unregister_recv_cb() -> i32;
                    }
                    unsafe {
                        let _ = esp_now_unregister_recv_cb();
                    }
                    let sniffer = &interfaces.sniffer;
                    sniffer.set_promiscuous_mode(true).unwrap();
                    if self.io_tasks.rx_enabled {
                        run_process_csi_packet().await;
                    } else {
                        wait_for_stop().await;
                    }
                    sniffer.set_promiscuous_mode(false).unwrap();
                }
            },
            Node::Central(op_mode) => match op_mode {
                CentralOpMode::EspNow(esp_now_config) => {
                    // Initialize as Central node with EspNowConfig
                    if let Some(rate) = self.rate.take() {
                        let old_rate = reconstruct_wifi_rate(&rate);
                        let _ = interfaces.esp_now.set_rate(rate);
                        self.rate = Some(old_rate);
                    }

                    let main_task = run_esp_now_central(
                        &mut interfaces.esp_now,
                        interfaces.sta.mac_address(),
                        esp_now_config,
                        self.traffic_freq_hz,
                        is_collector,
                        self.io_tasks,
                    );
                    if self.io_tasks.rx_enabled {
                        join(main_task, run_process_csi_packet()).await;
                    } else {
                        join(main_task, wait_for_stop()).await;
                    }
                }
                CentralOpMode::WifiStation(sta_config) => {
                    // Initialize as Wifi Station Collector with WifiStationConfig
                    // 1. Connect to Wi-Fi network, etc.
                    // 2. Run DHCP, NTP sync if enabled in config, etc.
                    // 3. Spawn STA Connection Handling Task
                    // 4. Spawn STA Network Operation Task
                    let (sta_stack, sta_runner) = sta_interface.unwrap();

                    let main_task = run_sta_connect(
                        controller,
                        self.traffic_freq_hz,
                        sta_stack,
                        sta_runner,
                        csi_config_for_recovery,
                        self.io_tasks,
                    );
                    if self.io_tasks.rx_enabled {
                        join(main_task, run_process_csi_packet()).await;
                    } else {
                        join(main_task, wait_for_stop()).await;
                    }
                    sniffer.set_promiscuous_mode(false).unwrap();
                }
            },
        }

        STOP_SIGNAL.reset();
        let _ = controller.stop_async().await;
        reset_globals();
    }
}

#[cfg(feature = "esp32c6")]
fn build_csi_config(csi_config: &CsiConfiguration) -> CsiConfig {
    CsiConfig {
        enable: csi_config.enable,
        acquire_csi_legacy: csi_config.acquire_csi_legacy,
        acquire_csi_ht20: csi_config.acquire_csi_ht20,
        acquire_csi_ht40: csi_config.acquire_csi_ht40,
        acquire_csi_su: csi_config.acquire_csi_su,
        acquire_csi_mu: csi_config.acquire_csi_mu,
        acquire_csi_dcm: csi_config.acquire_csi_dcm,
        acquire_csi_beamformed: csi_config.acquire_csi_beamformed,
        acquire_csi_he_stbc: csi_config.acquire_csi_he_stbc,
        val_scale_cfg: csi_config.val_scale_cfg,
        dump_ack_en: csi_config.dump_ack_en,
        reserved: csi_config.reserved,
    }
}

#[cfg(not(feature = "esp32c6"))]
fn build_csi_config(csi_config: &CsiConfiguration) -> CsiConfig {
    CsiConfig {
        lltf_en: csi_config.lltf_en,
        htltf_en: csi_config.htltf_en,
        stbc_htltf2_en: csi_config.stbc_htltf2_en,
        ltf_merge_en: csi_config.ltf_merge_en,
        channel_filter_en: csi_config.channel_filter_en,
        manu_scale: csi_config.manu_scale,
        shift: csi_config.shift,
        dump_ack_en: csi_config.dump_ack_en,
    }
}

/// Total received CSI packets (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_total_rx_packets() -> u64 {
    STATS.rx_count.load(Ordering::Relaxed)
}

/// Total transmitted packets (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_total_tx_packets() -> u64 {
    STATS.tx_count.load(Ordering::Relaxed)
}

/// Current RX packet rate in Hz (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_rx_rate_hz() -> u32 {
    STATS.rx_rate_hz.load(Ordering::Relaxed)
}

/// Current TX packet rate in Hz (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_tx_rate_hz() -> u32 {
    STATS.tx_rate_hz.load(Ordering::Relaxed)
}

/// Packets per second received since capture start (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_pps_rx() -> u64 {
    let start_time = Instant::from_ticks(STATS.capture_start_time.load(Ordering::Relaxed));
    let elapsed_secs = start_time.elapsed().as_secs() as u64;
    let total_packets = STATS.rx_count.load(Ordering::Relaxed);
    if elapsed_secs == 0 {
        return total_packets;
    }
    total_packets / elapsed_secs
}

/// Packets per second transmitted since capture start (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_pps_tx() -> u64 {
    let start_time = Instant::from_ticks(STATS.capture_start_time.load(Ordering::Relaxed));
    let elapsed_secs = start_time.elapsed().as_secs() as u64;
    let total_packets = STATS.tx_count.load(Ordering::Relaxed);
    if elapsed_secs == 0 {
        return total_packets;
    }
    total_packets / elapsed_secs
}

/// Dropped RX packets estimate (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_dropped_packets_rx() -> u32 {
    STATS.rx_drop_count.load(Ordering::Relaxed)
}

/// One-way latency (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_one_way_latency() -> i64 {
    STATS.one_way_latency.load(Ordering::Relaxed)
}

/// Two-way latency (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_two_way_latency() -> i64 {
    STATS.two_way_latency.load(Ordering::Relaxed)
}

/// Sets CSI Configuration.
pub(crate) fn set_csi(controller: &mut WifiController, config: CsiConfig) {
    // Set CSI Configuration with callback
    controller
        .set_csi(config, |info: esp_radio::wifi::wifi_csi_info_t| {
            capture_csi_info(info);
        })
        .unwrap();
}

// Function to capture CSI info from callback and publish to channel
fn capture_csi_info(info: esp_radio::wifi::wifi_csi_info_t) {
    // Count every CSI report regardless of mode so `rx_count` / `rx_rate_hz`
    // / `pps_rx` reflect actual radio CSI throughput. This is the only path
    // that fires for sniffer / STA / ESP-NOW collection — counting here keeps
    // the metric consistent across all node modes.
    #[cfg(feature = "statistics")]
    STATS.rx_count.fetch_add(1, Ordering::Relaxed);

    // Single-atomic fast path: returns immediately in Listener mode and in
    // Collector mode when no CSINodeClient subscriber exists. Building the
    // CSIDataPacket and calling publish_immediate acquires CriticalSectionRawMutex
    // and on `riscv32imc` every other atomic op also takes a critical section,
    // so additional gate atomics in the hot ISR path delay the Embassy timer ISR.
    if !CSI_PUBLISH_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    // In async-print mode the consumer is a separate logger task that
    // drains `CSI_PACKET`. If that channel is already full there's no
    // point constructing the heavy ~700-byte CSIDataPacket — fast-drop
    // and return. In sync mode this gate is irrelevant because the
    // emission happens inline below (no channel involved), so we skip
    // the CS-locked is_full check entirely.
    #[cfg(feature = "async-print")]
    if CSI_PACKET.is_full() {
        #[cfg(feature = "statistics")]
        STATS.rx_drop_count.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let rssi = if info.rx_ctrl.rssi() > 127 {
        info.rx_ctrl.rssi() - 256
    } else {
        info.rx_ctrl.rssi()
    };

    let mut csi_data = Vec::<i8, 612>::new();
    // let csi_buf = info.buf;
    let csi_buf_len = info.len;
    let csi_slice =
        unsafe { core::slice::from_raw_parts(info.buf as *const i8, csi_buf_len as usize) };
    match csi_data.extend_from_slice(csi_slice) {
        Ok(_) => {}
        Err(_) => {
            #[cfg(feature = "statistics")]
            STATS.rx_drop_count.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }

    #[cfg(not(feature = "esp32c6"))]
    let csi_packet = CSIDataPacket {
        sequence_number: info.rx_seq,
        data_format: RxCSIFmt::Undefined,
        date_time: None,
        mac: [
            info.mac[0],
            info.mac[1],
            info.mac[2],
            info.mac[3],
            info.mac[4],
            info.mac[5],
        ],
        rssi,
        bandwidth: info.rx_ctrl.cwb(),
        antenna: info.rx_ctrl.ant(),
        rate: info.rx_ctrl.rate(),
        sig_mode: info.rx_ctrl.sig_mode(),
        mcs: info.rx_ctrl.mcs(),
        smoothing: info.rx_ctrl.smoothing(),
        not_sounding: info.rx_ctrl.not_sounding(),
        aggregation: info.rx_ctrl.aggregation(),
        stbc: info.rx_ctrl.stbc(),
        fec_coding: info.rx_ctrl.fec_coding(),
        sgi: info.rx_ctrl.sgi(),
        noise_floor: info.rx_ctrl.noise_floor(),
        ampdu_cnt: info.rx_ctrl.ampdu_cnt(),
        channel: info.rx_ctrl.channel(),
        secondary_channel: info.rx_ctrl.secondary_channel(),
        timestamp: info.rx_ctrl.timestamp(),
        rx_state: info.rx_ctrl.rx_state(),
        sig_len: info.rx_ctrl.sig_len(),
        csi_data_len: csi_buf_len,
        csi_data: csi_data,
    };

    #[cfg(feature = "esp32c6")]
    let csi_packet = CSIDataPacket {
        mac: [
            info.mac[0],
            info.mac[1],
            info.mac[2],
            info.mac[3],
            info.mac[4],
            info.mac[5],
        ],
        rssi,
        timestamp: info.rx_ctrl.timestamp(),
        rate: info.rx_ctrl.rate(),
        noise_floor: info.rx_ctrl.noise_floor(),
        sig_len: info.rx_ctrl.sig_len(),
        rx_state: info.rx_ctrl.rx_state(),
        dump_len: info.rx_ctrl.dump_len(),
        he_sigb_len: info.rx_ctrl.he_sigb_len(),
        cur_single_mpdu: info.rx_ctrl.cur_single_mpdu(),
        cur_bb_format: info.rx_ctrl.cur_bb_format(),
        rx_channel_estimate_info_vld: info.rx_ctrl.rx_channel_estimate_info_vld(),
        rx_channel_estimate_len: info.rx_ctrl.rx_channel_estimate_len(),
        second: info.rx_ctrl.second(),
        channel: info.rx_ctrl.channel(),
        is_group: info.rx_ctrl.is_group(),
        rxend_state: info.rx_ctrl.rxend_state(),
        rxmatch3: info.rx_ctrl.rxmatch3(),
        rxmatch2: info.rx_ctrl.rxmatch2(),
        rxmatch1: info.rx_ctrl.rxmatch1(),
        rxmatch0: info.rx_ctrl.rxmatch0(),
        date_time: None,
        sequence_number: info.rx_seq,
        data_format: RxCSIFmt::Undefined,
        csi_data_len: info.len as u16,
        csi_data: csi_data,
    };

    #[cfg(feature = "statistics")]
    {
        if seq_drop_detection_enabled() {
            static mut PEER_SEQ_TRACKER: LinearMap<[u8; 6], u16, MAX_TRACKED_PEERS> =
                LinearMap::new();
            unsafe {
                if RESET_SEQ_TRACKER.swap(false, Ordering::Relaxed) {
                    PEER_SEQ_TRACKER.clear();
                }
                let current_seq = csi_packet.sequence_number;
                if let Some(&last_seq) = PEER_SEQ_TRACKER.get(&csi_packet.mac) {
                    let diff = (current_seq.wrapping_sub(last_seq)) & 0x0FFF;
                    if diff > 1 {
                        let lost = (diff - 1) as u32;
                        if lost < 500 {
                            STATS.rx_drop_count.fetch_add(lost, Ordering::Relaxed);
                        }
                    }
                }
                if PEER_SEQ_TRACKER.insert(csi_packet.mac, current_seq).is_err() {
                    PEER_SEQ_TRACKER.clear();
                    let _ = PEER_SEQ_TRACKER.insert(csi_packet.mac, current_seq);
                }
            }
        }
    }

    // Sync mode: format and write the CSI line directly here, in the WiFi
    // task callback context — exactly like ESP32-CSI-Tool's `_wifi_csi_cb`.
    // No channel hop, no separate task, no executor scheduling round-trip.
    // This is the architectural change that closes the gap to Hernandez:
    // they format inline in the WiFi callback, we used to round-trip
    // through CSI_PACKET → node_task → log_csi → format. With the
    // async-print feature *off*, we now match Hernandez 1:1.
    #[cfg(not(feature = "async-print"))]
    {
        crate::logging::logging::log_csi(csi_packet);
        return;
    }

    // Async-print mode: enqueue for the spawned `logger_backend` task
    // to drain. Channel had space when we checked is_full() above. The
    // only way this try_send fails is if the consumer drained between
    // then and now — harmless.
    #[cfg(feature = "async-print")]
    let _ = CSI_PACKET.try_send(csi_packet);
}

/// Internal task that handles collection-mode changes and rate statistics.
///
/// Seq drop detection runs inside `capture_csi_info` (ISR context) so this task
/// never drains `CSI_PACKET`, leaving the channel exclusively for `CSINodeClient`.
pub async fn run_process_csi_packet() {
    #[cfg(feature = "statistics")]
    STATS
        .capture_start_time
        .store(Instant::now().as_ticks(), Ordering::Relaxed);
    #[cfg(feature = "statistics")]
    let mut last_rate_update = Instant::now();
    #[cfg(feature = "statistics")]
    let mut last_rx_count = STATS.rx_count.load(Ordering::Relaxed);
    #[cfg(feature = "statistics")]
    let mut last_tx_count = STATS.tx_count.load(Ordering::Relaxed);

    loop {
        match select3(
            STOP_SIGNAL.wait(),
            COLLECTION_MODE_CHANGED.wait(),
            Timer::after_millis(500),
        )
        .await
        {
            Either3::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either3::Second(_) => {
                COLLECTION_MODE_CHANGED.reset();
                reset_globals();
                #[cfg(feature = "statistics")]
                {
                    STATS
                        .capture_start_time
                        .store(Instant::now().as_ticks(), Ordering::Relaxed);
                    last_rate_update = Instant::now();
                    last_rx_count = STATS.rx_count.load(Ordering::Relaxed);
                    last_tx_count = STATS.tx_count.load(Ordering::Relaxed);
                    RESET_SEQ_TRACKER.store(true, Ordering::Relaxed);
                }
            }
            Either3::Third(_) => {
                #[cfg(feature = "statistics")]
                {
                    let elapsed_secs = last_rate_update.elapsed().as_secs() as u64;
                    if elapsed_secs >= 1 {
                        let current_rx = STATS.rx_count.load(Ordering::Relaxed);
                        let current_tx = STATS.tx_count.load(Ordering::Relaxed);

                        let rx_rate = ((current_rx.saturating_sub(last_rx_count))
                            / elapsed_secs) as u32;
                        let tx_rate = ((current_tx.saturating_sub(last_tx_count))
                            / elapsed_secs) as u32;

                        STATS.rx_rate_hz.store(rx_rate, Ordering::Relaxed);
                        STATS.tx_rate_hz.store(tx_rate, Ordering::Relaxed);

                        last_rx_count = current_rx;
                        last_tx_count = current_tx;
                        last_rate_update = Instant::now();
                    }
                }
            }
        }
    }
}

#[cfg(feature = "statistics")]
use crate::logging::logging::{get_log_packet_drops, reset_global_log_drops};

fn reconstruct_wifi_rate(rate: &WifiPhyRate) -> WifiPhyRate {
    match rate {
        WifiPhyRate::Rate1mL => WifiPhyRate::Rate1mL,
        WifiPhyRate::Rate2m => WifiPhyRate::Rate2m,
        WifiPhyRate::Rate5mL => WifiPhyRate::Rate5mL,
        WifiPhyRate::Rate11mL => WifiPhyRate::Rate11mL,
        WifiPhyRate::Rate2mS => WifiPhyRate::Rate2mS,
        WifiPhyRate::Rate5mS => WifiPhyRate::Rate5mS,
        WifiPhyRate::Rate11mS => WifiPhyRate::Rate11mS,
        WifiPhyRate::Rate48m => WifiPhyRate::Rate48m,
        WifiPhyRate::Rate24m => WifiPhyRate::Rate24m,
        WifiPhyRate::Rate12m => WifiPhyRate::Rate12m,
        WifiPhyRate::Rate6m => WifiPhyRate::Rate6m,
        WifiPhyRate::Rate54m => WifiPhyRate::Rate54m,
        WifiPhyRate::Rate36m => WifiPhyRate::Rate36m,
        WifiPhyRate::Rate18m => WifiPhyRate::Rate18m,
        WifiPhyRate::Rate9m => WifiPhyRate::Rate9m,
        WifiPhyRate::RateMcs0Lgi => WifiPhyRate::RateMcs0Lgi,
        WifiPhyRate::RateMcs1Lgi => WifiPhyRate::RateMcs1Lgi,
        WifiPhyRate::RateMcs2Lgi => WifiPhyRate::RateMcs2Lgi,
        WifiPhyRate::RateMcs3Lgi => WifiPhyRate::RateMcs3Lgi,
        WifiPhyRate::RateMcs4Lgi => WifiPhyRate::RateMcs4Lgi,
        WifiPhyRate::RateMcs5Lgi => WifiPhyRate::RateMcs5Lgi,
        WifiPhyRate::RateMcs6Lgi => WifiPhyRate::RateMcs6Lgi,
        WifiPhyRate::RateMcs7Lgi => WifiPhyRate::RateMcs7Lgi,
        WifiPhyRate::RateMcs0Sgi => WifiPhyRate::RateMcs0Sgi,
        WifiPhyRate::RateMcs1Sgi => WifiPhyRate::RateMcs1Sgi,
        WifiPhyRate::RateMcs2Sgi => WifiPhyRate::RateMcs2Sgi,
        WifiPhyRate::RateMcs3Sgi => WifiPhyRate::RateMcs3Sgi,
        WifiPhyRate::RateMcs4Sgi => WifiPhyRate::RateMcs4Sgi,
        WifiPhyRate::RateMcs5Sgi => WifiPhyRate::RateMcs5Sgi,
        WifiPhyRate::RateMcs6Sgi => WifiPhyRate::RateMcs6Sgi,
        WifiPhyRate::RateMcs7Sgi => WifiPhyRate::RateMcs7Sgi,
        WifiPhyRate::RateLora250k => WifiPhyRate::RateLora250k,
        WifiPhyRate::RateLora500k => WifiPhyRate::RateLora500k,
        WifiPhyRate::RateMax => WifiPhyRate::RateMax,
    }
}

fn reconstruct_protocol(protocol: &Protocol) -> Protocol {
    match protocol {
        Protocol::P802D11B => Protocol::P802D11B,
        Protocol::P802D11BG => Protocol::P802D11BG,
        Protocol::P802D11BGN => Protocol::P802D11BGN,
        Protocol::P802D11BGNLR => Protocol::P802D11BGNLR,
        Protocol::P802D11LR => Protocol::P802D11LR,
        Protocol::P802D11BGNAX => Protocol::P802D11BGNAX,
        _ => Protocol::P802D11BGNLR,
    }
}
