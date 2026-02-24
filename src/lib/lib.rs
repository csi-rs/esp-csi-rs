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
//!
//! ### High‑level flow (CSINode) -> Needs to be Basic Example
//! ## This example commentary should align highly with examples proviced
//! 1. Create a `CSINodeHardware` from `Interfaces` and `WifiController`.
//! 2. Choose `Node` + operation mode (Central/Peripheral + ESP‑NOW/Station/Sniffer).
//! 3. Choose `CollectionMode` (Collector/Listener).
//! 4. Optionally set CSI config, rate, protocol, and traffic frequency.
//! 5. Call `CSINode::run()` to start.
//!
//! ### Example for Collecting CSI with WIFI Station Mode
//!## I suggest to remove this whole section
//! There are more examples in the repository. The example below demonstrates how to collect CSI data with an ESP configured in WIFI Station mode.
//!
//! #### Step 1: Initialize Hardware and Logger
//! First, we need to initialize the hardware interfaces and the Wi-Fi controller. This involves setting up the radio, and preparing the CSI node hardware bundle. We also initialize a logger to print output to the console. This step is common across all modes of operation, but in this example we show it in the context of setting up a Wi-Fi Station node.
//!
//! Initalize Logger Pritning Options:
//! - **LogMode::ArrayList**: Print CSI Data as a list of arrays, where each array represents the CSI values for a received packet. This format is more compact and easier to read for large volumes of CSI data.
//! - **LogMode::Text**: Print CSI Data in a more verbose, human-readable format. This can include additional metadata and explanations alongside the raw CSI values, making it easier to understand the context of each packet's CSI data.
//! - **LogMode::Serialized**: Print CSI Data in a serialized COBS format. This is a compact binary format that can be easily parsed by external tools for further analysis. It is not human-readable but is efficient for logging large amounts of CSI data without overwhelming the console output.
//!
//!```rust, no_run
//! init_logger(spawner, LogMode::ArrayList);
//! let radio_init = mk_static!(
//!     Controller<'static>,
//!     esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller")
//! );
//!
//! let mut config_radio = esp_radio::wifi::Config::default();
//! config_radio = config_radio.with_power_save_mode(esp_radio::wifi::PowerSaveMode::None);
//! let (wifi_controller, mut interfaces) =
//!     esp_radio::wifi::new(radio_init, peripherals.WIFI, config_radio)
//!         .expect("Failed to initialize Wi-Fi controller");
//!
//! let controller = WIFI_CONTROLLER.init(wifi_controller);
//! let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
//!```
//!
//! #### Step 2: Create a CSI Collection Configuration/Profile
//! This configuration creates a Wi-Fi Station central node that connects to an AP with the specified SSID and password, and collects CSI as a Collector. You can customize the connection options and CSI configuration as needed.
//!
//! Connection Options include:
//! - **Option 1**: SSID/Password for a commercial router or ESP in AP mode
//! - **Option 2**: Auth method (e.g. WPA2 Personal, Open, etc.)
//! - **Option 3**: Hz for generating traffic (e.g. 100Hz = 10ms between packets)
//!```rust, no_run
//! let client_config = ClientConfig::default()
//!     .with_ssid("SSID".to_string())
//!     .with_password("PASS".to_string())
//!     .with_auth_method(esp_radio::wifi::AuthMethod::Wpa2Personal);
//!
//! let station_config = WifiStationConfig {
//!    client_config,  // Pass the config we created above
//! };
//! let mut node_handle = CSIClient::new(); // Create a client handle to receive CSI data and stop the node when needed
//! let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
//! let mut node = CSINode::new(
//!     esp_csi_rs::Node::Central(esp_csi_rs::CentralOpMode::WifiStation(station_config)),
//!     CollectionMode::Collector,
//!     Some(CsiConfig::default()),
//!     Some(100),
//!     csi_hardware,
//! );
//!```
//!
//! #### Step 3: Run CSI Collection
//! Finally, we call `run()` on the node to start the CSI collection process. This will connect to the Wi-Fi network, start generating traffic, and begin collecting CSI data according to the configuration we set up, we use join here to run the node and the client task concurrently. The client task can be used to receive and print CSI data while the node is running, the client can signal CSINode to stop using CSIClient handle.
//!```rust, no_run
//! // Async function to run concurrently with the node to receive and print CSI data, and stop the node after a certain duration
//! async fn node_task(client: &mut CSIClient) {
//!    let mut last_log_time = Instant::now();
//!
//!    // Print CSI data with metadata every 10 milliseconds for 1000 seconds
//!    with_timeout(Duration::from_secs(1000), async {
//!            loop {
//!                let _ = with_timeout(Duration::from_millis(10), client.print_csi_w_metadata()).await;
//!            }
//!        })
//!    .await
//!    .unwrap_err();
//!    client.send_stop().await; // Signal the node to stop after the timeout
//! }
//! join(node.run(), node_task(&mut node_handle)).await;
//!```
//!
#![no_std]

use portable_atomic::AtomicI64;

use zerocopy::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use embassy_futures::join::join;
use embassy_futures::select::{select3, Either3};
use embassy_sync::pubsub::{PubSubBehavior, Subscriber};

use embassy_time::Instant;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::{ClientConfig, CsiConfig, Interfaces, Protocol, WifiController};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

use embassy_sync::pubsub::PubSubChannel;
use embassy_sync::signal::Signal;

use heapless::Vec;
extern crate alloc;
use alloc::collections::BTreeMap;
use serde::{Deserialize, Serialize};

pub mod central;
pub mod config;
pub mod csi;
pub mod logging;
pub mod peripheral;
pub mod time;

use crate::central::esp_now::run_esp_now_central;
use crate::central::sta::{run_sta_connect, sta_init};
use crate::config::CsiConfig as CsiConfiguration;
use crate::csi::{CSIDataPacket, RxCSIFmt};
use crate::peripheral::esp_now::run_esp_now_peripheral;

const PROC_CSI_CH_CAPACITY: usize = 20;
const PROC_CSI_CH_SUBS: usize = 2;

// PubSub Channels
static CSI_PACKET: PubSubChannel<
    CriticalSectionRawMutex,
    CSIDataPacket,
    PROC_CSI_CH_CAPACITY,
    PROC_CSI_CH_SUBS,
    2,
> = PubSubChannel::new();

static IS_COLLECTOR: AtomicBool = AtomicBool::new(false);
static COLLECTION_MODE_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static CENTRAL_MAGIC_NUMBER: u32 = 0xA8912BF0;
static PERIPHERAL_MAGIC_NUMBER: u32 = !CENTRAL_MAGIC_NUMBER;

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
}

impl Default for WifiSnifferConfig {
    fn default() -> Self {
        Self { mac_filter: None }
    }
}

/// Configuration for Wi-Fi Station mode.
#[derive(Debug, Clone)]
pub struct WifiStationConfig {
    pub client_config: ClientConfig,
}

// Enum for Central modes, each wrapping its specific config.

/// Central node operational modes.
pub enum CentralOpMode {
    EspNow(EspNowConfig),
    WifiStation(WifiStationConfig),
}

// Enum for Peripheral modes, each wrapping its specific config.
/// Peripheral node operational modes.
pub enum PeripheralOpMode {
    EspNow(EspNowConfig),
    WifiSniffer(WifiSnifferConfig),
}

/// High-level node type and mode.
pub enum Node {
    Peripheral(PeripheralOpMode), // Mode is implicit (only EspNow), directly holds config.
    Central(CentralOpMode),       // Uses the sub-enum for mode selection.
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

type CSIRxSubscriber = Subscriber<
    'static,
    CriticalSectionRawMutex,
    CSIDataPacket,
    PROC_CSI_CH_CAPACITY,
    PROC_CSI_CH_SUBS,
    2,
>;

/// Client helper to receive CSI packets via a pub/sub channel.
pub struct CSIClient {
    csi_subscriber: CSIRxSubscriber,
}

impl CSIClient {
    /// Create a new CSI subscriber.
    pub fn new() -> Self {
        Self {
            csi_subscriber: CSI_PACKET.subscriber().unwrap(),
        }
    }

    /// Wait for the next CSI packet.
    pub async fn get_csi_data(&mut self) -> CSIDataPacket {
        self.csi_subscriber.next_message_pure().await
    }

    /// Receive and print CSI data with metadata (uses crate logging).
    pub async fn print_csi_w_metadata(&mut self) {
        let packet = self.get_csi_data().await;
        packet.print_csi_w_metadata();
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
    pub is_collector: bool,
    pub central_send_uptime: u64,
    pub latency_offset: i64,
}

impl ControlPacket {
    /// Create a new control packet with the provided collector flag and latency offset.
    pub fn new(is_collector: bool, latency_offset: i64) -> Self {
        Self {
            magic_number: CENTRAL_MAGIC_NUMBER.into(),
            is_collector,
            central_send_uptime: Instant::now().as_micros(),
            latency_offset,
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
        STATS.rx_drop_count.store(0, Ordering::Relaxed);
        STATS.tx_count.store(0, Ordering::Relaxed);
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
            csi_config,
            traffic_freq_hz,
            hardware,
            protocol: None,
            rate: Some(WifiPhyRate::RateMcs0Lgi),
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
            csi_config,
            traffic_freq_hz,
            hardware,
            protocol: None,
            rate: Some(WifiPhyRate::RateMcs0Lgi),
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

        // Set Peripheral/Central to Collect CSI
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
                    );
                    join(main_task, run_process_csi_packet()).await;
                }
                PeripheralOpMode::WifiSniffer(sniffer_config) => {
                    let sniffer = &interfaces.sniffer;
                    sniffer.set_promiscuous_mode(true).unwrap();
                    run_process_csi_packet().await;
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
                    );
                    join(main_task, run_process_csi_packet()).await;
                }
                CentralOpMode::WifiStation(sta_config) => {
                    // Initialize as Wifi Station Collector with WifiStationConfig
                    // 1. Connect to Wi-Fi network, etc.
                    // 2. Run DHCP, NTP sync if enabled in config, etc.
                    // 3. Spawn STA Connection Handling Task
                    // 4. Spawn STA Network Operation Task
                    let (sta_stack, sta_runner) = sta_interface.unwrap();

                    let main_task =
                        run_sta_connect(controller, self.traffic_freq_hz, sta_stack, sta_runner);
                    join(main_task, run_process_csi_packet()).await;
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
fn set_csi(controller: &mut WifiController, config: CsiConfig) {
    // Set CSI Configuration with callback
    controller
        .set_csi(config, |info: esp_radio::wifi::wifi_csi_info_t| {
            capture_csi_info(info);
        })
        .unwrap();
}

// Function to capture CSI info from callback and publish to channel
fn capture_csi_info(info: esp_radio::wifi::wifi_csi_info_t) {
    if IS_COLLECTOR.load(Ordering::Relaxed) == false {
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

    CSI_PACKET.publish_immediate(csi_packet);
    #[cfg(feature = "statistics")]
    STATS.rx_count.fetch_add(1, Ordering::Relaxed);
}

/// Internal task that processes CSI packets from the pub/sub channel.
pub async fn run_process_csi_packet() {
    // Initialize CSI process start time
    #[cfg(feature = "statistics")]
    STATS
        .capture_start_time
        .store(Instant::now().as_ticks(), Ordering::Relaxed);
    // Subscribe to CSI packet capture updates
    let mut csi_packet_sub = CSI_PACKET.subscriber().unwrap();
    // Map to track sequence numbers per MAC address
    let mut peer_tracker: BTreeMap<[u8; 6], u16> = BTreeMap::new();
    let mut is_collector = IS_COLLECTOR.load(Ordering::Relaxed);

    loop {
        match select3(
            STOP_SIGNAL.wait(),
            COLLECTION_MODE_CHANGED.wait(),
            csi_packet_sub.next_message_pure(),
        )
        .await
        {
            Either3::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either3::Second(_) => {
                COLLECTION_MODE_CHANGED.reset();
                is_collector = IS_COLLECTOR.load(Ordering::Relaxed);
                reset_globals();
                #[cfg(feature = "statistics")]
                STATS
                    .capture_start_time
                    .store(Instant::now().as_ticks(), Ordering::Relaxed);
            }
            Either3::Third(csi_packet) => {
                #[cfg(feature = "statistics")]
                {
                    if is_collector {
                        let current_seq = csi_packet.sequence_number;

                        // Check if we have seen this MAC before
                        if let Some(&last_seq) = peer_tracker.get(&csi_packet.mac) {
                            // Station Mode / Hardware Sequence Number Fix:
                            // WiFi hardware sequence numbers (802.11) are 12-bit (0-4095).
                            // We use '& 0x0FFF' to handle the wraparound from 4095 -> 0 correctly.
                            let diff = (current_seq.wrapping_sub(last_seq)) & 0x0FFF;

                            if diff > 1 {
                                let lost = (diff - 1) as u32;

                                // Sanity check for huge gaps (e.g. router reset)
                                if lost < 500 {
                                    STATS.rx_drop_count.fetch_add(lost, Ordering::Relaxed);
                                }
                            }
                        }

                        // Update tracker with new sequence
                        peer_tracker.insert(csi_packet.mac, current_seq);
                        // --- DROP DETECTION LOGIC END ---
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
