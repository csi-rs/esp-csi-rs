//! # A crate for CSI collection on ESP devices
//! ## Overview
//! This crate builds on the low level Espressif abstractions to enable the collection of Channel State Information (CSI) on ESP devices with ease.
//! Currently this crate supports only the ESP `no-std` development framework.
//!
//! ### Choosing a device
//! In terms of hardware, you need to make sure that the device you choose supports WiFi and CSI collection.
//! Currently supported devices include:
//! - ESP32
//! - ESP32-C3
//! - ESP32-C5 (dual-band 2.4/5 GHz)
//! - ESP32-C6 (WiFi 6)
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
//! ## Logging Backends
//!
//! Two logging backends are supported and they are mutually exclusive:
//!
//! - **`println` (default)** — plain text via `esp-println`. Decoded by any serial monitor.
//! - **`defmt`** — compact binary frames via `esp-println`'s `defmt-espflash` backend, decoded by `espflash --monitor --log-format defmt`. The `build.rs` adds `-Tdefmt.x` automatically when this feature is on, so no manual linker-script edits are needed.
//!
//! Per-chip cargo aliases ship in `.cargo/config.toml` for both flavors:
//!
//! ```bash
//! cargo esp32c3 --example sniffer_wifi          # println
//! cargo esp32c3-defmt --example sniffer_wifi    # defmt
//! ```
//!
//! Replace `esp32c3` with any of: `esp32`, `esp32c3`, `esp32c5`, `esp32c6`, `esp32s3`. `-build` and `-build-defmt` variants compile without flashing.
//!
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
//! ### On-Device CSI Processing
//!
//! Register a `fn(&CSIDataPacket)` with [`set_csi_callback`] to process
//! every captured CSI packet inline in the WiFi-task callback. Zero
//! channel hops, lowest possible latency. The callback runs on the WiFi
//! hot path so it must be fast and non-blocking — no heap allocation,
//! no locking, no UART I/O. Heavier work belongs in your own task; copy
//! what you need out of the borrowed packet and post it via atomics or
//! a queue. See `examples/csi_callback_test.rs` for a working demo.
//!
//! ```rust,ignore
//! use esp_csi_rs::{set_csi_callback, csi::CSIDataPacket};
//!
//! fn on_csi(packet: &CSIDataPacket) {
//!     // your processing — keep it fast
//! }
//!
//! set_csi_callback(on_csi);
//! ```
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
//! use esp_radio::wifi::sta::StationConfig;
//! use esp_radio::wifi::AuthenticationMethod;
//!
//! let client_config = StationConfig::default()
//!     .with_ssid("SSID")
//!     .with_password("PASS".to_string())
//!     .with_auth_method(AuthenticationMethod::Wpa2Personal);
//!
//! let station_config = WifiStationConfig {
//!    client_config,  // Pass the config we created above
//! };
//! ```
//!
//! `StationConfig` was renamed from `ClientConfig`, and `AuthMethod` was renamed to `AuthenticationMethod` in `esp-radio` 0.18. `with_ssid` now takes `impl Into<Ssid>`, so a `&str` literal works directly without `.to_string()`.
//! #### Step 4: Create a CSI Collection Node Instance with the Desired Configuration
//! ```rust
//! let mut node = CSINode::new(
//!     esp_csi_rs::Node::Central(esp_csi_rs::CentralOpMode::WifiStation(station_config)),
//!     CollectionMode::Collector,
//!     Some(CsiConfig::default()),
//!     Some(100),
//!     csi_hardware,
//! );
//! ```
//! #### Step 5: (Optional) Register an On-Device CSI Callback
//! ```rust
//! set_csi_callback(|packet| {
//!     // process `packet` inline — keep it fast
//! });
//! ```
//! #### Step 6: Create a CSI Node Client to Control the Node
//! ```rust
//! let mut node_handle = CSINodeClient::new();
//! ```
//! #### Step 7: Run the Node for a Fixed Duration
//! ```rust
//! node.run_duration(1000, &mut node_handle).await;
//! ```
//!


#![no_std]

extern crate alloc;

// Crate modules. `lib.rs` is intentionally thin: it declares the module tree
// and re-exports the public API (and the crate-internal items that submodules
// reach by crate-root path) from their new homes. The actual implementations
// live in the modules below.
pub mod central;
pub mod config;
pub mod csi;
pub mod esp_now_pool;
pub mod espnow_phy;
pub mod logging;
pub mod node;
pub mod peripheral;
pub mod protocol;
pub mod stats;
pub mod time;

#[cfg(feature = "cpu-test-tx")]
pub mod cpu_test;

// ---------------------------------------------------------------------------
// Public API re-exports — kept at the crate root so existing user code and the
// in-tree examples (`esp_csi_rs::CSINode`, `esp_csi_rs::set_csi_callback`, …)
// continue to resolve unchanged after the split.
// ---------------------------------------------------------------------------
pub use crate::csi::delivery::{
    clear_csi_callback, csi_delivery_mode, csi_logging_enabled, run_process_csi_packet,
    set_csi_callback, set_csi_delivery_mode, set_csi_logging_enabled, set_csi_raw_callback,
    CSINodeClient, CsiDeliveryMode,
};
pub use crate::espnow_phy::set_peer_espnow_phy;
pub use crate::node::{
    CSINode, CSINodeHardware, CentralOpMode, CollectionMode, EspNowConfig, IOTaskConfig, Node,
    PeripheralOpMode, WifiSnifferConfig, WifiStationConfig,
};
pub use crate::protocol::{ControlPacket, PeripheralPacket};

pub use crate::esp_now_pool::set_raw_recv_callback;
pub use crate::peripheral::esp_now::set_raw_listen;

#[cfg(feature = "statistics")]
pub use crate::stats::{
    get_dropped_packets_rx, get_pps_rx, get_pps_tx, get_rx_rate_hz, get_total_rx_packets,
    get_total_tx_packets, get_tx_rate_hz,
};

#[cfg(feature = "cpu-test-tx")]
pub use crate::cpu_test::{set_test_tx_paused, set_test_tx_payload_b, set_test_tx_rate_hz};

// ---------------------------------------------------------------------------
// Crate-internal re-exports — these items are referenced by `crate::<Item>`
// path from the `central` / `peripheral` / `sta` / `csi::delivery` modules. Keeping
// the flat crate-root paths means those modules need no edits after the split.
// ---------------------------------------------------------------------------
pub(crate) use crate::csi::delivery::{set_csi, set_runtime_collection_mode, IS_COLLECTOR};
pub(crate) use crate::node::{reset_globals, STOP_SIGNAL};
pub(crate) use crate::protocol::{
    parse_with_magic, serialize_with_magic, CENTRAL_MAGIC_NUMBER, PERIPHERAL_BEACON_SENTINEL,
    PERIPHERAL_MAGIC_NUMBER,
};

#[cfg(feature = "statistics")]
pub(crate) use crate::stats::STATS;

#[cfg(feature = "cpu-test-tx")]
pub(crate) use crate::cpu_test::{TEST_TX_PAUSED, TEST_TX_PAYLOAD_B, TEST_TX_RATE_HZ};
