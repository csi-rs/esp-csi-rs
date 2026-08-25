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
//! - ESP32-C6
//! - ESP32-S3
//!
//! In terms of project and software toolchain setup, you will need to specify the hardware you will be using. To minimize headache, it is recommended that you generate a project using `esp-generate` as explained next.
//!
//! ### Creating a project
//! To use this crate you would need to create and setup a project for your ESP device then import the crate. This crate is compatible with the `no-std` ESP development framework. You should also select the corresponding device by activating it in the crate features.
//!
//! To create a projects it is highly recommended to refer the to instructions in [The Rust on ESP Book](https://docs.espressif.com/projects/rust/book/) before proceeding. The book explains the full esp-rs ecosystem, how to get started, and how to generate projects for both `std` and `no-std`.
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
//! cargo esp32c3 --example sniffer_wifi # println
//! cargo esp32c3-defmt --example sniffer_wifi # defmt
//! ```
//!
//! Replace `esp32c3` with any of: `esp32`, `esp32c3`, `esp32c5`, `esp32c6`, `esp32s3`. `-build` and `-build-defmt` variants compile without flashing.
//!
//! ## Using the Crate
//!
//! Each ESP device is a node with one job. A CSI measurement needs energy in the channel and
//! something to measure the channel's response to it, so there are exactly two roles:
//!
//! ### Node Roles
//! 1) **Emitter** ([`NodeRole::Emitter`]) — puts known RF energy into the channel and never captures.
//!    It forces its transmit PHY to a fixed format and loop-injects a raw sounding frame without
//!    associating to anything. Configured with [`EmitterConfig`].
//! 2) **Collector** ([`NodeRole::Collector`]) — captures the channel's response and delivers it.
//!
//! Because the emitter's frames carry no meaning, it needs no peer, no handshake, and no protocol.
//! That is what lets the two roles compose into any arrangement you like.
//!
//! ### Collector Capture Paths
//! *How* a collector gets frames to measure is a separate question from what it is for, so these are
//! variants of the collector role rather than roles of their own ([`CollectorMode`]):
//!
//! 1) **Sniffer** — lock a channel in promiscuous mode and measure every frame overheard. This is the
//!    path that pairs with an emitter.
//! 2) **Station** — associate to an access point and measure the frames received from it.
//! 3) **Access Point** — run a softAP (with a built-in DHCP server) so an associated station
//!    generates steady uplink traffic to measure.
//!
//! ### CSI Output
//! A collector delivers its CSI by default. [`CSINode::set_csi_output_enabled`] turns delivery off
//! while leaving capture running, so the RX path and its timing stay identical but nothing is
//! decoded, logged, or handed to a callback.
//!
//! ## Bandwidth
//! An emitter transmits HT20 or HT40 ([`HtBandwidth`]) — plain 802.11n, supported on every chip
//! listed above. 40 MHz needs a secondary channel above or below the primary, and every node in a
//! capture set must agree on the primary channel.
//!
//! ## Collection Setups
//! Roles compose, so the useful arrangements are just combinations rather than fixed topologies:
//!
//! 1. ***Single node:*** one sniffer collector, measuring whatever ambient traffic exists. The only
//!    setup that needs no second device.
//! 2. ***Emitter + collector:*** the controlled pairing. The emitter sounds the channel at a known
//!    rate and bandwidth; one or more sniffer collectors measure it. Adding collectors costs the
//!    emitter nothing, and several emitters can share one collector — each frame carries its
//!    transmitter's MAC, so a collector attributes measurements by source.
//! 3. ***Associated link:*** a station collector against any access point (an ESP softAP collector or
//!    a commercial router), measuring the CSI of ordinary traffic on that link.
//!
//! ## Output Formats & Logging Modes
//! `esp-csi-rs` is able to print CSI data in several formats. The output format can be configured when initializing the logger. The supported formats include:
//! - **LogMode::ArrayList**: This prints CSI data as an array, where the array represents the CSI values for a received packet. This format is more compact and easier to read for large volumes of CSI data.
//!
//! Example output:
//! ```
//! [3916,-93,11,157,1,1815804,256,0,260,2,0,1,1,128,0,1,1,0,1,0,0,0,256,128,[...]]
//! ```
//! The array fields map to the [`csi::CSIDataPacket`] struct fields in the following order:
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
//! ```rust,ignore
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
//! sigb len: 2
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
//! // your processing — keep it fast
//! }
//!
//! set_csi_callback(on_csi);
//! ```
//!
//! ### Example for creating WiFi Station Central Collector
//! There are more examples in the repository. The example below demonstrates how to collect CSI data with an ESP configured in WIFI Station mode.
//!
//! #### Step 1: Initialize Logger
//! ```rust,ignore
//! init_logger(spawner, LogMode::ArrayList);
//! ```
//! #### Step 2: Create a Hardware Instance for the CSI Node
//! ```rust,ignore
//! let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
//! ```
//! #### Step 3: Create a Station Configuration
//! ```rust,ignore
//! use esp_radio::wifi::sta::StationConfig;
//! use esp_radio::wifi::AuthenticationMethod;
//!
//! let client_config = StationConfig::default()
//! .with_ssid("SSID")
//! .with_password("PASS".to_string())
//! .with_auth_method(AuthenticationMethod::Wpa2Personal);
//!
//! let station_config = WifiStationConfig {
//! client_config, // Pass the config we created above
//! };
//! ```
//!
//! `StationConfig` was renamed from `ClientConfig`, and `AuthMethod` was renamed to `AuthenticationMethod` in `esp-radio` 0.18. `with_ssid` now takes `impl Into<Ssid>`, so a `&str` literal works directly without `.to_string()`.
//! #### Step 4: Create a CSI Collection Node Instance with the Desired Configuration
//! ```rust,ignore
//! let mut node = CSINode::new(
//!     esp_csi_rs::NodeRole::Central(esp_csi_rs::CentralOpMode::WifiStation(station_config)),
//!     Some(CsiConfig::default()),
//!     Some(100),
//!     csi_hardware,
//! );
//! // A node delivers its CSI by default. `CollectionMode::Listener` used to be the
//! // way to keep the radio capturing without delivering; that is now:
//! // node.set_csi_output_enabled(false);
//! ```
//!
//! Changed in 0.10: `CSINode::new` takes a [`NodeRole`] and no longer takes a
//! `CollectionMode`. `NodeRole` is the four-variant role — `Emitter`, `Collector`,
//! `Central`, `Peripheral` — where the ESP-NOW pair keeps the central/peripheral
//! spelling it always had. There are also `CSINode::new_collector` and
//! `CSINode::new_emitter` shorthands for the two most common cases.
//! #### Step 5: (Optional) Register an On-Device CSI Callback
//! ```rust,ignore
//! set_csi_callback(|packet| {
//! // process `packet` inline — keep it fast
//! });
//! ```
//! #### Step 6: Create a CSI Node Client to Control the Node
//! ```rust,ignore
//! let mut node_handle = CSINodeClient::new();
//! ```
//! #### Step 7: Run the Node for a Fixed Duration
//! ```rust,ignore
//! node.run_duration(1000, &mut node_handle).await;
//! ```
//!
//! ## Examples
//!
//! The repository ships runnable firmware for every supported topology in the
//! [examples directory](https://github.com/csi-rs/esp-csi-rs/tree/main/examples).
//! Build one with the per-chip cargo aliases, e.g.
//! `cargo esp32c6 --example esp_now_central`:
//!
//! | Example | What it does |
//! |---|---|
//! | `sniffer_wifi` | Promiscuous collector — locks a channel and measures every frame overheard |
//! | `wifi_station` / `wifi_ap` | Associated collector, station side / self-contained softAP collector |
//! | `ht20_emitter` / `ht40_emitter` | Raw 802.11n injection at 20 or 40 MHz; pair with a sniffer |
//! | `collector_sniffer` | The collector half of the emitter/collector pairing |
//! | `esp_now_central` / `esp_now_peripheral` | Connectionless ESP-NOW pair; both sides capture |
//! | `esp_now_fast_collector` / `esp_now_fast_source` | Asymmetric simplex ESP-NOW — the highest CSI rate of any pairing |
//! | `esp_now_*_ht40` | The ESP-NOW pair with a forced HT40 per-peer TX PHY |
//! | `csi_callback_test` | The two CSI delivery paths — inline callback vs. queued |
//! | `runtime_config` | Changing collection settings between runs without reflashing |
//!
//! Measurement and characterization harnesses live separately under
//! `experiments/`, documented in `experiments/README.md`.
//!
//! ## Architecture
//!
//! Everything is in this crate. Between 0.9.0 and 0.10.0 the engine lived in a
//! separate `esp-csi-rs-core` crate that this one re-exported wholesale; that split
//! has been undone, because it moved the implementation and the documentation away
//! from the name people actually depend on while buying nothing a module boundary
//! does not already give.
//!
//! `esp-csi-rs-core` 0.1.x stays published and is **not** yanked: `esp-csi-rs`
//! 0.9.0 depends on it, so removing it would retroactively break that release. It
//! receives no further versions. Anything written against
//! `esp_csi_rs_core::` should move to `esp_csi_rs::` — the paths are otherwise
//! unchanged.
//!
//! One seam is deliberate and worth knowing about if you extend this crate:
//! [`RadioProfile`] is the hook for driving a PHY this crate does not implement
//! itself, and [`esp_radio`] is re-exported so an out-of-tree profile is built
//! against the same `WifiController` / `CsiConfig` types the engine uses. Resolving
//! a different `esp-radio` would otherwise make `impl RadioProfile` silently fail
//! to satisfy the trait.

#![no_std]

extern crate alloc;

// Crate modules. `lib.rs` is intentionally thin: it declares the module tree
// and re-exports the public API (and the crate-internal items that submodules
// reach by crate-root path) from their new homes. The actual implementations
// live in the modules below.
// Restored alongside `collector` / `emitter`, not instead of them — see `central_peripheral`.
pub mod central;
pub mod central_peripheral;
pub mod collector;
pub mod config;
pub mod csi;
pub mod emitter;
pub mod logging;
pub mod esp_now_pool;
pub mod espnow_phy;
pub mod node;
pub mod peripheral;
pub mod profile;
pub mod protocol;
pub(crate) mod radio;

// Re-export `esp-radio` so the open and proprietary consumer crates build the
// `RadioProfile` trait against the *same* `WifiController` / `Protocol(s)` /
// `CsiConfig` types. Resolving a different `esp-radio` patch in a consumer would
// otherwise make `impl RadioProfile` silently fail to satisfy the trait.
pub use esp_radio;
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
    CSINodeClient, CsiDeliveryMode, clear_csi_callback, csi_delivery_mode, csi_logging_enabled,
    csi_min_sig_mode, csi_output_enabled, csi_peer_filter, run_process_csi_packet,
    set_csi_callback, set_csi_delivery_mode, set_csi_logging_enabled, set_csi_min_sig_mode,
    set_csi_output_enabled, set_csi_peer_filter, set_csi_raw_callback,
};
pub use crate::emitter::frame::{
    BROADCAST, PROBE_FRAME_LEN, build_probe_frame, inject_probe_once,
};
pub use crate::emitter::{EmitterConfig, HtBandwidth};
pub use crate::node::{
    CSINode, CollectorMode, IOTaskConfig, NodeHardware, NodeRole, WifiApConfig, WifiSnifferConfig,
    WifiStationConfig,
};
/// The restored central/peripheral taxonomy. Re-exported at the crate root because that is where
/// every existing caller and every ESP-NOW driver in this crate expects to find it.
pub use crate::central_peripheral::{
    CentralOpMode, CollectionMode, EspNowConfig, Node, PeripheralOpMode,
};
/// Pre-refactor name for [`NodeHardware`]. It served only the central/peripheral pair before the
/// emitter/collector split gave it a second caller, and the rename was the whole change — so this is
/// an alias, not a second type to keep in step.
pub type CSINodeHardware<'a> = crate::node::NodeHardware<'a>;
pub use crate::esp_now_pool::set_raw_recv_callback;
pub use crate::espnow_phy::{
    apply_peer_espnow_phy, install_static_espnow_recv, set_peer_espnow_phy,
};
pub use crate::peripheral::esp_now::set_raw_listen;
pub use crate::protocol::{ControlPacket, PeripheralPacket};
// The wire constants and codec are crate-private: they are an implementation detail of the ESP-NOW
// exchange, and `pub use` on a `pub(crate)` item is an error rather than a widening.
pub(crate) use crate::csi::delivery::{IS_COLLECTOR, set_runtime_collection_mode};
/// Feature-gated exactly as before: the ESP-NOW drivers only touch the counters when `statistics`
/// is on, so re-exporting unconditionally would make the symbol dead in every other build.
#[cfg(feature = "statistics")]
pub(crate) use crate::stats::STATS;
pub(crate) use crate::protocol::{
    CENTRAL_MAGIC_NUMBER, PERIPHERAL_BEACON_SENTINEL, PERIPHERAL_MAGIC_NUMBER, parse_with_magic,
    serialize_with_magic,
};
pub use crate::logging::logging::{
    get_log_packet_drops, is_async_logging_active, log_line,
};
pub use crate::profile::{RadioProfile, StandardProfile};

// Additive root re-exports. These types were reachable only by their full module path, while the
// crate documentation and every example wrote them unqualified — `init_logger(spawner, mode)` and
// `CsiConfig::default()`. Both paths now resolve, so the docs are correct as written and callers
// stop having to spell out `logging::logging::`.
//
// Nothing is removed: `logging::logging::{init_logger, LogMode}` and `config::CsiConfig` keep
// working, which matters because the pro CLI imports them by those paths today.
pub use crate::config::CsiConfig;
pub use crate::logging::logging::{LogMode, init_logger};

#[cfg(feature = "statistics")]
pub use crate::stats::{
    get_dropped_packets_rx, get_pps_rx, get_pps_tx, get_rx_rate_hz, get_total_rx_packets,
    get_total_tx_packets, get_tx_rate_hz, record_collector_rx, record_collector_rx_drop,
    record_emitter_tx, snapshot_bb_format_histogram, stats_begin_run,
};

#[cfg(feature = "cpu-test-tx")]
pub use crate::cpu_test::{set_test_tx_paused, set_test_tx_payload_b, set_test_tx_rate_hz};

// ---------------------------------------------------------------------------
// Crate-internal re-exports — these items are referenced by `crate::<Item>`
// path from the `collector` / `emitter` / `csi::delivery` modules. Keeping the
// flat crate-root paths means those modules need no edits after the split.
// ---------------------------------------------------------------------------
pub(crate) use crate::csi::delivery::set_csi;
pub(crate) use crate::node::STOP_SIGNAL;

#[cfg(feature = "cpu-test-tx")]
pub(crate) use crate::cpu_test::{TEST_TX_PAUSED, TEST_TX_PAYLOAD_B, TEST_TX_RATE_HZ};
