//! Node topology/configuration types and the [`CSINode`] orchestrator.
//!
//! This module owns the user-facing description of a CSI node — its role
//! ([`Node`] / [`CentralOpMode`] / [`PeripheralOpMode`]), the per-mode configs
//! ([`EspNowConfig`], [`WifiSnifferConfig`], [`WifiStationConfig`]), the
//! collection and TX/RX toggles — and [`CSINode`], whose `run` / `run_duration`
//! wire up Wi-Fi, CSI, and the mode-specific tasks. It also holds the shared
//! stop signal and the per-run lifecycle helpers.

#[cfg(any(feature = "async-print", feature = "auto"))]
use embassy_time::with_timeout;

use embassy_futures::join::{join, join3};
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use enumset::EnumSet;
use esp_radio::esp_now::WifiPhyRate;
#[cfg(feature = "esp32c5")]
use esp_radio::wifi::BandMode;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Interfaces, Protocol, Protocols, SecondaryChannel, WifiController};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use portable_atomic::Ordering;

use crate::central::ap::{ap_init, run_ap};
use crate::central::esp_now::run_esp_now_central;
use crate::central::esp_now_fast::run_esp_now_fast_collector;
use crate::central::sta::{run_sta_connect, sta_init};
use crate::config::CsiConfig as CsiConfiguration;
use crate::peripheral::esp_now::run_esp_now_peripheral;
use crate::peripheral::esp_now_fast::run_esp_now_fast_source;

use crate::csi::delivery::{
    CSINodeClient, IS_COLLECTOR, build_csi_config, run_process_csi_packet, set_csi,
};
use crate::espnow_phy::bring_up_espnow_sta;
use crate::espnow_phy::{
    apply_espnow_band_for_channel, apply_espnow_ht40_mode, install_static_espnow_recv,
    takeover_esp_now_recv, with_espnow_recv_suspended,
};
use crate::log_ln;
use crate::stats::set_seq_drop_detection;

// Signals
pub(crate) static STOP_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Per-mutation radio-quiesce delay on C5 dual-band bring-up.
///
/// The C5 Wi-Fi ISR can wedge if a MAC interrupt fires mid-reconfiguration
/// (`set_protocols` / `set_config` STA restart / `set_csi` / `set_channel`),
/// tripping the interrupt watchdog (`handle_interrupts` backtrace at boot) or
/// hard-freezing before any task runs. `with_espnow_recv_suspended` already
/// shrinks that window; inserting a short settle *between* the mutations lets
/// the MAC drain any pending interrupt before the next driver call, shrinking it
/// further. This is a probabilistic mitigation, not a guarantee — the radio
/// restart still races the MAC IRQ — so keeping no ESP-NOW traffic on air during
/// a node's bring-up remains the most effective measure.
#[cfg(feature = "esp32c5")]
const C5_RADIO_SETTLE_MS: u64 = 60;

/// Await a brief radio-settle delay on C5; no-op on every other chip.
/// See [`C5_RADIO_SETTLE_MS`].
async fn c5_radio_settle() {
    #[cfg(feature = "esp32c5")]
    Timer::after(Duration::from_millis(C5_RADIO_SETTLE_MS)).await;
}

async fn csi_data_collection(client: &mut CSINodeClient, duration: u64) {
    #[cfg(any(feature = "async-print", feature = "auto"))]
    if crate::logging::logging::is_async_logging_active() {
        with_timeout(Duration::from_secs(duration), async {
            loop {
                client.print_csi_w_metadata().await;
            }
        })
        .await
        .unwrap_err();
        client.send_stop().await;
        return;
    }

    #[cfg(not(any(feature = "async-print", feature = "auto")))]
    {
        let _ = client;
    }
    Timer::after(Duration::from_secs(duration)).await;
    client.send_stop().await;
}

async fn wait_for_stop() {
    STOP_SIGNAL.wait().await;
    STOP_SIGNAL.signal(());
}

async fn stop_after_duration(duration: u64) {
    match select(
        STOP_SIGNAL.wait(),
        Timer::after(Duration::from_secs(duration)),
    )
    .await
    {
        Either::First(_) | Either::Second(_) => STOP_SIGNAL.signal(()),
    }
}

/// Configuration for ESP-NOW traffic generation.
///
/// Used by both Central and Peripheral nodes when operating in ESP-NOW mode.
/// Construct with `EspNowConfig::default()` then chain `with_channel` /
/// `with_phy_rate` to override defaults — both nodes must agree on the
/// channel for ESP-NOW frames to be received.
pub struct EspNowConfig {
    phy_rate: WifiPhyRate,
    pub(crate) channel: u8,
    /// Optional pre-configured peer MAC. When `None` (default) the pair uses
    /// automatic, magic-prefix-based pairing. When `Some`, the magic prefix is
    /// dropped from every frame and the source-MAC filter is the discriminator
    /// from the first frame — both nodes must each be configured with the
    /// other's MAC.
    peer_mac: Option<[u8; 6]>,
    /// Optional HT40 secondary channel. When `Some`, the node runs HT40 (40 MHz)
    /// on `channel` + this secondary; when `None`, HT20. Only meaningful when
    /// `force_phy` is set.
    secondary_channel: Option<SecondaryChannel>,
    /// When set, the node forces the ESP-NOW TX PHY (`phy_rate` +
    /// HT20/HT40 from `secondary_channel`) via a per-peer rate config — which
    /// requires bringing the radio up in started STA mode. When clear (default),
    /// the radio is left in its default state and ESP-NOW frames go out at the
    /// driver's default (legacy) PHY. Set by `with_phy_rate` / `with_ht40`.
    force_phy: bool,
}

impl Default for EspNowConfig {
    fn default() -> Self {
        Self {
            phy_rate: WifiPhyRate::RateMcs0Lgi,
            // Channel 1 is empirically less congested than 11 in most
            // residential / office environments — APs on auto-select tend
            // to bias toward 11 because it's the upper bound in US/EU.
            // Override with `with_channel` if your environment differs.
            channel: 1,
            peer_mac: None,
            secondary_channel: None,
            force_phy: false,
        }
    }
}

impl EspNowConfig {
    /// Recommended base config for the fast one-to-one (asymmetric simplex)
    /// mode: forces HT20 at MCS7 Long-GI for maximum CSI packets/sec. Chain
    /// `with_channel` / `with_ht40` to override. Used by
    /// [`CentralOpMode::EspNowFastCollector`] / [`PeripheralOpMode::EspNowFastSource`].
    pub fn fast_default() -> Self {
        Self::default().with_phy_rate(WifiPhyRate::RateMcs7Lgi)
    }

    /// Override the 2.4 GHz channel (1–14). Both central and peripheral
    /// must be configured with the same channel.
    pub fn with_channel(mut self, channel: u8) -> Self {
        self.channel = channel;
        self
    }

    /// Force the ESP-NOW TX PHY rate (e.g. `RateMcs0Lgi` … `RateMcs7Lgi`, or a
    /// legacy rate). Applied per-peer via `esp_now_set_peer_rate_config`, which
    /// brings the radio up in started STA mode. Combine with [`with_ht40`] for
    /// a 40 MHz bandwidth; without it the rate is sent at HT20 (for MCS rates)
    /// or the matching legacy mode. Without calling this (or `with_ht40`) the
    /// PHY is left at the driver default.
    ///
    /// [`with_ht40`]: EspNowConfig::with_ht40
    pub fn with_phy_rate(mut self, phy_rate: WifiPhyRate) -> Self {
        self.phy_rate = phy_rate;
        self.force_phy = true;
        self
    }

    /// Pre-configure the peer's MAC address for manual pairing.
    ///
    /// Switches off automatic magic-prefix pairing: no magic is sent, and each
    /// node accepts frames only from the configured peer MAC (source-MAC
    /// filtering applies from the first frame). The central must be given the
    /// peripheral's MAC and vice-versa, and both nodes must use the same
    /// pairing mode for frames to parse.
    pub fn with_peer_mac(mut self, peer_mac: [u8; 6]) -> Self {
        self.peer_mac = Some(peer_mac);
        self
    }

    /// Configured 2.4 GHz channel.
    pub fn channel(&self) -> u8 {
        self.channel
    }

    /// Configured PHY rate.
    pub fn phy_rate(&self) -> &WifiPhyRate {
        &self.phy_rate
    }

    /// Configured peer MAC for manual pairing, or `None` for automatic
    /// magic-prefix pairing.
    pub fn peer_mac(&self) -> Option<[u8; 6]> {
        self.peer_mac
    }

    /// Run the ESP-NOW TX at HT40 (40 MHz) with `secondary` as the HT40
    /// secondary channel, using the configured [`with_phy_rate`] (default
    /// `RateMcs0Lgi`). Implies `force_phy`. Without this the PHY is HT20 (if a
    /// rate is forced) or the driver default. Verify on-air (CSI `bandwidth`
    /// field) that HT40 actually engaged.
    ///
    /// [`with_phy_rate`]: EspNowConfig::with_phy_rate
    pub fn with_ht40(mut self, secondary: SecondaryChannel) -> Self {
        self.secondary_channel = Some(secondary);
        self.force_phy = true;
        self
    }

    /// Configured HT40 secondary channel, or `None` for HT20.
    pub fn secondary_channel(&self) -> Option<SecondaryChannel> {
        self.secondary_channel
    }

    /// Whether the ESP-NOW TX PHY (rate + bandwidth) is forced via a per-peer
    /// rate config (set by [`with_phy_rate`] / [`with_ht40`]).
    ///
    /// [`with_phy_rate`]: EspNowConfig::with_phy_rate
    /// [`with_ht40`]: EspNowConfig::with_ht40
    pub fn force_phy(&self) -> bool {
        self.force_phy
    }
}

/// Configuration for Wi-Fi Promiscuous Sniffer mode.
///
/// Construct with `WifiSnifferConfig::default()` then chain `with_channel`
/// to override defaults.
#[derive(Debug, Clone)]
pub struct WifiSnifferConfig {
    /// Optional MAC source filter (reserved — not yet wired into the
    /// promiscuous filter setup).
    #[allow(dead_code)]
    mac_filter: Option<[u8; 6]>,
    channel: u8,
}

impl Default for WifiSnifferConfig {
    fn default() -> Self {
        Self {
            mac_filter: None,
            // Match `EspNowConfig` default — channel 1 is typically less
            // congested than 11 in dense residential / office environments.
            channel: 1,
        }
    }
}

impl WifiSnifferConfig {
    /// Override the channel the sniffer locks to.
    ///
    /// Must be a valid IEEE 802.11 **primary** channel number — pass the
    /// primary, not the wider-channel center notation that routers
    /// commonly display:
    ///
    /// - **2.4 GHz**: `1`–`14`
    /// - **5 GHz**: `36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112,
    ///   116, 120, 124, 128, 132, 136, 140, 144, 149, 153, 157, 161, 165`
    ///   (regulatory-domain dependent — some restricted by `country_info`)
    ///
    /// Center-channel labels (`38, 46, ...` for HT40; `42, 58, 106, ...`
    /// for VHT80; `50, 114` for VHT160; `154` for the 153/157 HT40 pair)
    /// are **not** accepted here — `esp_wifi_set_channel` panics with
    /// `InvalidArguments`. For example, a router showing "channel 154"
    /// is using primary `153` (or `157`); pass that primary and the chip
    /// will sniff the full 40 MHz block automatically per 802.11.
    ///
    /// On dual-band chips (currently ESP32-C5), the band is auto-selected
    /// from the channel number — channels `>= 36` switch the radio to
    /// `BandMode::_5G`, otherwise `BandMode::_2_4G`. On 2.4-GHz-only
    /// chips, passing any 5 GHz channel will fail at runtime.
    pub fn with_channel(mut self, channel: u8) -> Self {
        self.channel = channel;
        self
    }

    /// Configured channel (2.4 GHz: 1–14, 5 GHz: 36–165).
    pub fn channel(&self) -> u8 {
        self.channel
    }
}

/// Configuration for Wi-Fi Station mode.
#[derive(Debug, Clone)]
pub struct WifiStationConfig {
    /// Underlying esp-radio station configuration (SSID, auth, etc.).
    pub client_config: StationConfig,
}

#[cfg(feature = "defmt")]
impl defmt::Format for WifiStationConfig {
    fn format(&self, fmt: defmt::Formatter<'_>) {
        defmt::write!(fmt, "WifiStationConfig {{ client_config: <opaque> }}");
    }
}

/// Configuration for self-contained softAP CSI collector mode.
///
/// Wraps esp-radio's [`AccessPointConfig`] (SSID, channel, auth, secondary
/// channel) and the static IPv4 addressing used by the built-in single-lease
/// DHCP server. The AP hands the associating station an address in the AP's /24
/// subnet with the gateway set to the AP itself, so the station's pings target
/// the AP and land as CSI on the AP RX path.
///
/// `channel`/`secondary_channel` are duplicated here because esp-radio's
/// `AccessPointConfig` fields are not externally readable; [`CSINode`] needs them
/// for band/HT40 setup.
///
/// [`AccessPointConfig`]: esp_radio::wifi::ap::AccessPointConfig
pub struct WifiApConfig {
    /// Underlying esp-radio access-point configuration.
    pub ap_config: esp_radio::wifi::ap::AccessPointConfig,
    /// Primary channel the AP operates on (mirror of `ap_config`'s channel).
    pub channel: u8,
    /// Optional HT40 secondary channel (mirror of `ap_config`'s secondary).
    pub secondary_channel: Option<SecondaryChannel>,
    /// AP's static IPv4 address; also the gateway and DHCP server identifier.
    pub ap_ipv4: core::net::Ipv4Addr,
    /// IPv4 address leased to the single associating station.
    pub lease_ipv4: core::net::Ipv4Addr,
    /// Whether to run the built-in DHCP server. When `false`, the AP only starts
    /// + collects CSI (clients must self-assign IPs).
    pub serve_dhcp: bool,
}

impl WifiApConfig {
    /// Create a config from an [`AccessPointConfig`], its primary `channel`, and
    /// optional HT40 `secondary` channel. Defaults the AP to `192.168.13.1/24`,
    /// leases `192.168.13.2`, and enables the DHCP server.
    ///
    /// [`AccessPointConfig`]: esp_radio::wifi::ap::AccessPointConfig
    pub fn new(
        ap_config: esp_radio::wifi::ap::AccessPointConfig,
        channel: u8,
        secondary: Option<SecondaryChannel>,
    ) -> Self {
        Self {
            ap_config,
            channel,
            secondary_channel: secondary,
            ap_ipv4: core::net::Ipv4Addr::new(192, 168, 13, 1),
            lease_ipv4: core::net::Ipv4Addr::new(192, 168, 13, 2),
            serve_dhcp: true,
        }
    }

    /// Override the AP/lease IPv4 addresses (must share a /24).
    pub fn with_ipv4(mut self, ap: core::net::Ipv4Addr, lease: core::net::Ipv4Addr) -> Self {
        self.ap_ipv4 = ap;
        self.lease_ipv4 = lease;
        self
    }

    /// Enable or disable the built-in DHCP server (default enabled).
    pub fn with_dhcp_server(mut self, enabled: bool) -> Self {
        self.serve_dhcp = enabled;
        self
    }

    /// Configured primary channel.
    pub fn channel(&self) -> u8 {
        self.channel
    }

    /// Configured HT40 secondary channel, or `None` for HT20.
    pub fn secondary_channel(&self) -> Option<SecondaryChannel> {
        self.secondary_channel
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for WifiApConfig {
    fn format(&self, fmt: defmt::Formatter<'_>) {
        defmt::write!(fmt, "WifiApConfig {{ ap_config: <opaque> }}");
    }
}

// Enum for Central modes, each wrapping its specific config.

/// Central node operational modes.
pub enum CentralOpMode {
    /// Drive an ESP-NOW exchange with a peripheral node.
    EspNow(EspNowConfig),
    /// Associate as a Wi-Fi station to harvest CSI from received frames.
    WifiStation(WifiStationConfig),
    /// Run a self-contained softAP CSI collector: start an access point (plus a
    /// minimal DHCP server) so a [`CentralOpMode::WifiStation`] node can
    /// associate and generate steady uplink traffic, captured as CSI on this AP.
    WifiAccessPoint(WifiApConfig),
    /// Fast one-to-one ESP-NOW collector (asymmetric simplex): broadcast a
    /// sparse discovery beacon until a [`PeripheralOpMode::EspNowFastSource`] is
    /// heard, then stop beaconing and go RX-only, capturing CSI from the source's
    /// continuous unicast flood. Maximizes CSI packets/sec by leaving all airtime
    /// to the single transmitter.
    EspNowFastCollector(EspNowConfig),
}

// Enum for Peripheral modes, each wrapping its specific config.
/// Peripheral node operational modes.
pub enum PeripheralOpMode {
    /// Reply to a central's ESP-NOW control frames.
    EspNow(EspNowConfig),
    /// Run as a Wi-Fi promiscuous sniffer; CSI is captured from every
    /// frame received on the locked channel.
    WifiSniffer(WifiSnifferConfig),
    /// Fast one-to-one ESP-NOW source (asymmetric simplex): listen for a
    /// [`CentralOpMode::EspNowFastCollector`] beacon, learn its MAC, then unicast
    /// a continuous forced-PHY flood for the collector to capture as CSI.
    EspNowFastSource(EspNowConfig),
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

pub(crate) fn reset_globals() {
    // Close all CSI delivery gates so any late-firing WiFi callback runs
    // are no-ops, then clear the statistics counters. The CSI callback stays
    // registered with esp-radio after stop (the radio itself is still up),
    // but with the gates closed the callback short-circuits before it touches
    // the log channel or the user's callback. Without this, sniffer/ESP-NOW/STA
    // nodes keep emitting CSI lines on the serial port well after `send_stop()`.
    crate::csi::delivery::reset();
    crate::stats::reset();
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

    /// Set the ESP-NOW TX PHY rate after construction.
    ///
    /// Equivalent to [`EspNowConfig::with_phy_rate`]: forces the per-peer PHY
    /// (and brings the radio up in started STA mode). Combine with
    /// `EspNowConfig::with_ht40` for 40 MHz. No effect on non-ESP-NOW nodes —
    /// STA / sniffer rates are driven by their own configuration, not here.
    pub fn set_rate(&mut self, rate: WifiPhyRate) {
        match &mut self.kind {
            Node::Central(CentralOpMode::EspNow(cfg))
            | Node::Central(CentralOpMode::EspNowFastCollector(cfg))
            | Node::Peripheral(PeripheralOpMode::EspNow(cfg))
            | Node::Peripheral(PeripheralOpMode::EspNowFastSource(cfg)) => {
                cfg.phy_rate = rate;
                cfg.force_phy = true;
            }
            _ => {}
        }
    }

    /// Run the node for `duration` seconds with internal collection.
    ///
    /// This initializes Wi-Fi, configures CSI, and starts mode-specific tasks.
    pub async fn run_duration(&mut self, duration: u64, client: &mut CSINodeClient) {
        self.run_inner(Some(duration), Some(client)).await;
    }

    /// Shared implementation behind [`run`](Self::run) and
    /// [`run_duration`](Self::run_duration).
    ///
    /// `duration`/`client` are `Some` only on the timed `run_duration` path:
    /// when set, each mode arm runs an extra concurrent future that stops the
    /// node after `duration` seconds (and, with RX enabled, drains CSI to the
    /// logger via `client`). When `None` the node runs until externally
    /// stopped via [`CSINodeClient::send_stop`].
    async fn run_inner(&mut self, duration: Option<u64>, client: Option<&mut CSINodeClient>) {
        let interfaces = &mut self.hardware.interfaces;
        let controller = &mut self.hardware.controller;

        // Take over esp-radio's ESP-NOW receive dispatcher *first*, before any
        // other Wi-Fi reconfiguration runs (`set_protocols`, `set_csi`) — see
        // `takeover_esp_now_recv` for why this must happen this early.
        takeover_esp_now_recv(matches!(
            &self.kind,
            Node::Peripheral(PeripheralOpMode::WifiSniffer(_))
        ));
        // Let the freshly-constructed radio/ESP-NOW state settle before the
        // first C5 reconfiguration mutation (no-op off C5).
        c5_radio_settle().await;

        let espnow_ht40 = matches!(
            &self.kind,
            Node::Peripheral(PeripheralOpMode::EspNow(c))
                | Node::Peripheral(PeripheralOpMode::EspNowFastSource(c))
                | Node::Central(CentralOpMode::EspNow(c))
                | Node::Central(CentralOpMode::EspNowFastCollector(c))
                if c.secondary_channel().is_some()
        );

        let is_ap = matches!(&self.kind, Node::Central(CentralOpMode::WifiAccessPoint(_)));

        // Apply protocol before STA bring-up / CSI — on C5, recv must stay
        // suspended across every controller mutation to avoid ISR WDT trips.
        if let Some(protocol) = self.protocol.take() {
            let old_protocol = reconstruct_protocol(&protocol);
            let mut protocols = Protocols::default().with_2_4(EnumSet::only(protocol));
            #[cfg(feature = "esp32c5")]
            {
                if matches!(
                    &self.kind,
                    Node::Peripheral(PeripheralOpMode::EspNow(_))
                        | Node::Peripheral(PeripheralOpMode::EspNowFastSource(_))
                        | Node::Central(CentralOpMode::EspNow(_))
                        | Node::Central(CentralOpMode::EspNowFastCollector(_))
                ) {
                    // ESP-NOW peer rate config misbehaves with 802.11ax on 5G — cap to A/N.
                    protocols = protocols.with_5(Protocol::A | Protocol::N);
                } else if matches!(
                    &self.kind,
                    Node::Central(CentralOpMode::WifiAccessPoint(_))
                        | Node::Central(CentralOpMode::WifiStation(_))
                ) {
                    // AP / STA: enable 802.11ax on 5 GHz so the link negotiates HE20
                    // (~242-subcarrier CSI). Cumulative A|N|AX, NOT AX-only — HE can't
                    // operate standalone (beacons/assoc/base PHY are legacy a/n), and
                    // AX-only silently blocks association.
                    protocols = protocols.with_5(Protocol::A | Protocol::N | Protocol::AX);
                }
            }
            #[cfg(feature = "esp32c6")]
            {
                if matches!(
                    &self.kind,
                    Node::Central(CentralOpMode::WifiAccessPoint(_))
                        | Node::Central(CentralOpMode::WifiStation(_))
                ) {
                    // C6 is 2.4 GHz-only — enable N|AX (not AX-only) so HE20 AP/STA
                    // on channel 6 can associate and negotiate HE20 PPDUs.
                    protocols = Protocols::default().with_2_4(Protocol::N | Protocol::AX);
                }
            }
            with_espnow_recv_suspended(|| {
                controller.set_protocols(protocols).unwrap();
            });
            self.protocol = Some(old_protocol);
            c5_radio_settle().await;
        }

        // Started STA mode is required for ESP-NOW CSI capture (RX path) and for
        // forced-PHY / manual-unicast TX. On C5 dual-band, skip STA for TX-only
        // broadcast (no peer_mac, no RX) — restarting STA there can wedge the
        // Wi-Fi ISR when the TX loop starts immediately afterward.
        // The fast simplex roles always need started STA: the collector for its
        // RX/CSI path, the source for forced per-peer unicast PHY (which it
        // applies after discovery, before any `peer_mac` is known).
        let needs_sta_bringup = matches!(
            &self.kind,
            Node::Peripheral(PeripheralOpMode::EspNow(c)) | Node::Central(CentralOpMode::EspNow(c))
                if self.io_tasks.rx_enabled
                    || {
                        #[cfg(not(feature = "esp32c5"))]
                        {
                            c.force_phy()
                        }
                        #[cfg(feature = "esp32c5")]
                        {
                            c.peer_mac().is_some()
                        }
                    }
        ) || matches!(
            &self.kind,
            Node::Peripheral(PeripheralOpMode::EspNowFastSource(_))
                | Node::Central(CentralOpMode::EspNowFastCollector(_))
        );
        if needs_sta_bringup {
            with_espnow_recv_suspended(|| {
                bring_up_espnow_sta(controller, false);
            });
            // The STA restart is the riskiest C5 op — settle before the next
            // mutation (set_csi) so a post-restart MAC interrupt can drain.
            c5_radio_settle().await;
        }

        // Tasks Necessary for Central Station & Sniffer
        let sta_interface = if let Node::Central(CentralOpMode::WifiStation(config)) = &self.kind {
            Some(sta_init(&mut interfaces.station, config, controller))
        } else {
            None
        };

        // Self-contained softAP: bring up the AP-side embassy-net stack (static
        // IP) and apply the AP config to the controller. `interfaces.access_point`
        // is disjoint from `.station`/`.esp_now`/`.sniffer`, so this borrow is fine.
        let ap_interface = if let Node::Central(CentralOpMode::WifiAccessPoint(config)) = &self.kind {
            #[cfg(feature = "esp32c5")]
            if config.secondary_channel().is_none() {
                with_espnow_recv_suspended(|| {
                    apply_espnow_band_for_channel(controller, config.channel());
                });
            }
            if let Some(secondary) = config.secondary_channel() {
                with_espnow_recv_suspended(|| {
                    apply_espnow_ht40_mode(controller, config.channel(), secondary);
                });
                c5_radio_settle().await;
            }
            let ifaces = ap_init(&mut interfaces.access_point, config, controller);
            // The AP `set_config` restarts the radio; settle before `set_csi`.
            c5_radio_settle().await;
            Some(ifaces)
        } else {
            None
        };

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

        // Apply Protocol if specified — handled above (before STA bring-up).

        log_ln!("Wi-Fi Controller Started");
        let is_collector = self.collection_mode == CollectionMode::Collector;
        IS_COLLECTOR.store(is_collector, Ordering::Relaxed);
        set_seq_drop_detection(matches!(
            &self.kind,
            Node::Peripheral(PeripheralOpMode::EspNow(_))
                | Node::Peripheral(PeripheralOpMode::EspNowFastSource(_))
                | Node::Central(CentralOpMode::EspNow(_))
                | Node::Central(CentralOpMode::EspNowFastCollector(_))
        ));

        // Set Peripheral/Central to Collect CSI. Keep a clone so the STA
        // recovery path in run_sta_connect can re-apply after a stop/start
        // cycle (stop clears the CSI filter/callback).
        //
        // Only register the CSI callback when RX is actually enabled —
        // otherwise the radio fires `capture_csi_info` for every overheard
        // 802.11 frame (beacons, neighbour ESP-NOW, retries) on the WiFi
        // task hot path, stealing cycles from the central TX-completion
        // ISR for no purpose.
        let csi_config_for_recovery = config.clone();
        let is_sniffer = matches!(
            &self.kind,
            Node::Peripheral(PeripheralOpMode::WifiSniffer(_))
        );
        // AP is excluded here: `set_config(AccessPoint)` in `ap_init` restarts the
        // radio and clears the CSI filter, so the AP arm registers CSI itself
        // after start.
        if self.io_tasks.rx_enabled && !is_sniffer && !espnow_ht40 && !is_ap {
            with_espnow_recv_suspended(|| {
                set_csi(controller, config.clone());
            });
            // Settle after enabling CSI before the mode task issues its first
            // set_channel / TX so the run loop doesn't start into a pending IRQ.
            c5_radio_settle().await;
        }
        let rx_enabled = self.io_tasks.rx_enabled;
        // Immutable borrow of a *different* `interfaces` field than the ESP-NOW
        // arms touch (`esp_now` / `station`), so this disjoint borrow is fine.
        // Used by the sniffer arm and to clear promiscuous mode on WifiStation
        // shutdown.
        let sniffer = &interfaces.sniffer;

        // Initialize Nodes based on type
        match &self.kind {
            Node::Peripheral(op_mode) => match op_mode {
                PeripheralOpMode::EspNow(esp_now_config) => {
                    // Initialize as Peripheral node with EspNowConfig
                    // Non-HT40 path on dual-band C5: select band from primary
                    // channel as well, so a prior 5 GHz app doesn't leave this
                    // run pinned to 5 GHz when channel is 2.4 GHz (e.g. ch 11).
                    #[cfg(feature = "esp32c5")]
                    if esp_now_config.secondary_channel().is_none() {
                        with_espnow_recv_suspended(|| {
                            apply_espnow_band_for_channel(controller, esp_now_config.channel());
                        });
                    }
                    // HT40: set the secondary channel before the run loop (which
                    // then skips its own `esp_now.set_channel`). The TX rate/PHY
                    // is forced per-peer inside the run loops (see
                    // `set_peer_espnow_phy`); `esp_now.set_rate` is unused — it
                    // routes to the deprecated `esp_wifi_config_espnow_rate`.
                    if let Some(secondary) = esp_now_config.secondary_channel() {
                        with_espnow_recv_suspended(|| {
                            apply_espnow_ht40_mode(controller, esp_now_config.channel(), secondary);
                        });
                        install_static_espnow_recv();
                        c5_radio_settle().await;
                        if rx_enabled {
                            with_espnow_recv_suspended(|| {
                                set_csi(controller, config.clone());
                            });
                            c5_radio_settle().await;
                        }
                    }

                    let main_task = run_esp_now_peripheral(
                        &mut interfaces.esp_now,
                        esp_now_config,
                        self.traffic_freq_hz,
                        self.io_tasks,
                    );
                    drive_main(main_task, rx_enabled, duration, client).await;
                }
                PeripheralOpMode::EspNowFastSource(esp_now_config) => {
                    // Pre-setup mirrors the ESP-NOW peripheral arm above.
                    #[cfg(feature = "esp32c5")]
                    if esp_now_config.secondary_channel().is_none() {
                        with_espnow_recv_suspended(|| {
                            apply_espnow_band_for_channel(controller, esp_now_config.channel());
                        });
                    }
                    if let Some(secondary) = esp_now_config.secondary_channel() {
                        with_espnow_recv_suspended(|| {
                            apply_espnow_ht40_mode(controller, esp_now_config.channel(), secondary);
                        });
                        install_static_espnow_recv();
                        c5_radio_settle().await;
                        if rx_enabled {
                            with_espnow_recv_suspended(|| {
                                set_csi(controller, config.clone());
                            });
                            c5_radio_settle().await;
                        }
                    }

                    let main_task = run_esp_now_fast_source(
                        &mut interfaces.esp_now,
                        esp_now_config,
                        self.traffic_freq_hz,
                        self.io_tasks,
                    );
                    drive_main(main_task, rx_enabled, duration, client).await;
                }
                PeripheralOpMode::WifiSniffer(sniffer_config) => {
                    #[cfg(feature = "esp32c5")]
                    {
                        let band = if sniffer_config.channel() >= 36 {
                            BandMode::_5G
                        } else {
                            BandMode::_2_4G
                        };
                        controller.set_band_mode(band).unwrap();
                    }
                    sniffer.set_promiscuous_mode(true).unwrap();
                    controller
                        .set_channel(sniffer_config.channel(), SecondaryChannel::None)
                        .unwrap();
                    if rx_enabled {
                        set_csi(controller, config.clone());
                    }
                    // ESP-NOW's heap-allocating `rcv_cb` was already dropped at
                    // the top of `run_inner` via `takeover_esp_now_recv`, so
                    // overheard vendor frames are discarded at the C layer.
                    //
                    // The sniffer arm has no `main_task`, so it drives CSI
                    // collection directly rather than through `drive_main`.
                    match (duration, rx_enabled) {
                        (Some(d), true) => {
                            join(
                                run_process_csi_packet(),
                                csi_data_collection(client.unwrap(), d),
                            )
                            .await;
                            // `csi_data_collection` signals stop, so the join
                            // returns; this trailing await lets the rate task
                            // observe the stop and exit (preserves prior behavior).
                            run_process_csi_packet().await;
                        }
                        (Some(d), false) => stop_after_duration(d).await,
                        (None, true) => run_process_csi_packet().await,
                        (None, false) => wait_for_stop().await,
                    }
                    sniffer.set_promiscuous_mode(false).unwrap();
                }
            },
            Node::Central(op_mode) => match op_mode {
                CentralOpMode::EspNow(esp_now_config) => {
                    // Initialize as Central node with EspNowConfig.
                    // Non-HT40 path on dual-band C5: select band from primary
                    // channel as well, so a prior 5 GHz app doesn't leave this
                    // run pinned to 5 GHz when channel is 2.4 GHz (e.g. ch 11).
                    #[cfg(feature = "esp32c5")]
                    if esp_now_config.secondary_channel().is_none() {
                        with_espnow_recv_suspended(|| {
                            apply_espnow_band_for_channel(controller, esp_now_config.channel());
                        });
                    }
                    // HT40 handling mirrors the peripheral ESP-NOW arm above.
                    if let Some(secondary) = esp_now_config.secondary_channel() {
                        with_espnow_recv_suspended(|| {
                            apply_espnow_ht40_mode(controller, esp_now_config.channel(), secondary);
                        });
                        install_static_espnow_recv();
                        c5_radio_settle().await;
                        if rx_enabled {
                            with_espnow_recv_suspended(|| {
                                set_csi(controller, config.clone());
                            });
                            c5_radio_settle().await;
                        }
                    }

                    let main_task = run_esp_now_central(
                        &mut interfaces.esp_now,
                        interfaces.station.mac_address(),
                        esp_now_config,
                        self.traffic_freq_hz,
                        is_collector,
                        self.io_tasks,
                    );
                    drive_main(main_task, rx_enabled, duration, client).await;
                }
                CentralOpMode::EspNowFastCollector(esp_now_config) => {
                    // Pre-setup mirrors the ESP-NOW central arm above.
                    #[cfg(feature = "esp32c5")]
                    if esp_now_config.secondary_channel().is_none() {
                        with_espnow_recv_suspended(|| {
                            apply_espnow_band_for_channel(controller, esp_now_config.channel());
                        });
                    }
                    if let Some(secondary) = esp_now_config.secondary_channel() {
                        with_espnow_recv_suspended(|| {
                            apply_espnow_ht40_mode(controller, esp_now_config.channel(), secondary);
                        });
                        install_static_espnow_recv();
                        c5_radio_settle().await;
                        if rx_enabled {
                            with_espnow_recv_suspended(|| {
                                set_csi(controller, config.clone());
                            });
                            c5_radio_settle().await;
                        }
                    }

                    let main_task = run_esp_now_fast_collector(
                        &mut interfaces.esp_now,
                        esp_now_config,
                        self.io_tasks,
                    );
                    drive_main(main_task, rx_enabled, duration, client).await;
                }
                CentralOpMode::WifiAccessPoint(ap_config) => {
                    // Start the AP, run the net stack + optional DHCP server, and
                    // collect CSI from associated stations' uplink frames. CSI is
                    // registered inside `run_ap` (after the AP-start radio restart).
                    let (ap_stack, ap_runner) = ap_interface.unwrap();
                    let main_task = run_ap(
                        controller,
                        ap_stack,
                        ap_runner,
                        ap_config,
                        csi_config_for_recovery,
                        self.io_tasks,
                        self.traffic_freq_hz,
                    );
                    drive_main(main_task, rx_enabled, duration, client).await;
                    sniffer.set_promiscuous_mode(false).unwrap();
                }
                CentralOpMode::WifiStation(_sta_config) => {
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
                    drive_main(main_task, rx_enabled, duration, client).await;
                    // Clear promiscuous mode on shutdown. It is never enabled on
                    // a STA interface, so this is a no-op — kept to match the
                    // unconditional shutdown path the untimed `run()` always took.
                    sniffer.set_promiscuous_mode(false).unwrap();
                }
            },
        }

        STOP_SIGNAL.reset();
        reset_globals();
    }

    /// Run the node until stopped.
    ///
    /// This initializes Wi-Fi, configures CSI, and starts mode-specific tasks.
    pub async fn run(&mut self) {
        self.run_inner(None, None).await;
    }
}

/// Concurrent driver for a mode's `main_task`.
///
/// Joins `main_task` with the CSI rate task (RX enabled) or a stop waiter, and
/// — on the timed `run_duration` path (`duration`/`client` are `Some`) — a
/// third future that ends the run after `duration` seconds, draining CSI to the
/// logger via `client` when RX is enabled.
async fn drive_main(
    main_task: impl core::future::Future,
    rx_enabled: bool,
    duration: Option<u64>,
    client: Option<&mut CSINodeClient>,
) {
    match (duration, rx_enabled) {
        (Some(d), true) => {
            join3(
                main_task,
                run_process_csi_packet(),
                csi_data_collection(client.unwrap(), d),
            )
            .await;
        }
        (Some(d), false) => {
            join3(main_task, wait_for_stop(), stop_after_duration(d)).await;
        }
        (None, true) => {
            join(main_task, run_process_csi_packet()).await;
        }
        (None, false) => {
            join(main_task, wait_for_stop()).await;
        }
    }
}

fn reconstruct_protocol(protocol: &Protocol) -> Protocol {
    match protocol {
        Protocol::B => Protocol::B,
        Protocol::G => Protocol::G,
        Protocol::N => Protocol::N,
        Protocol::LR => Protocol::LR,
        Protocol::A => Protocol::A,
        Protocol::AC => Protocol::AC,
        Protocol::AX => Protocol::AX,
        _ => Protocol::N,
    }
}
