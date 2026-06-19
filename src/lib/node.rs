//! Node topology/configuration types and the [`CSINode`] orchestrator.
//!
//! This module owns the user-facing description of a CSI node — its role
//! ([`Node`] / [`CentralOpMode`] / [`PeripheralOpMode`]), the per-mode configs
//! ([`EspNowConfig`], [`WifiSnifferConfig`], [`WifiStationConfig`]), the
//! collection and TX/RX toggles — and [`CSINode`], whose `run` / `run_duration`
//! wire up Wi-Fi, CSI, and the mode-specific tasks. It also holds the shared
//! stop signal and the per-run lifecycle helpers.

#[cfg(feature = "async-print")]
use embassy_time::with_timeout;

use embassy_futures::join::{join, join3};
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Timer};
use enumset::EnumSet;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Interfaces, Protocol, Protocols, SecondaryChannel, WifiController};
#[cfg(feature = "esp32c5")]
use esp_radio::wifi::BandMode;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use portable_atomic::Ordering;

use crate::central::esp_now::run_esp_now_central;
use crate::central::sta::{run_sta_connect, sta_init};
use crate::config::CsiConfig as CsiConfiguration;
use crate::peripheral::esp_now::run_esp_now_peripheral;

use crate::csi::delivery::{build_csi_config, run_process_csi_packet, set_csi, CSINodeClient, IS_COLLECTOR};
use crate::espnow_phy::{apply_espnow_ht40, bring_up_espnow_sta, takeover_esp_now_recv};
use crate::log_ln;
use crate::stats::set_seq_drop_detection;

// Signals
pub(crate) static STOP_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

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
            | Node::Peripheral(PeripheralOpMode::EspNow(cfg)) => {
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

        // A forced ESP-NOW PHY (rate/bandwidth via per-peer config) needs the
        // radio in started STA mode (see `bring_up_espnow_sta`). Do it before
        // `set_protocols`/`set_csi` so they apply on the started STA interface.
        if matches!(
            &self.kind,
            Node::Peripheral(PeripheralOpMode::EspNow(c)) | Node::Central(CentralOpMode::EspNow(c))
                if c.force_phy()
        ) {
            bring_up_espnow_sta(controller);
        }

        // Tasks Necessary for Central Station & Sniffer
        let sta_interface = if let Node::Central(CentralOpMode::WifiStation(config)) = &self.kind {
            Some(sta_init(&mut interfaces.station, config, controller))
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

        // Apply Protocol if specified
        if let Some(protocol) = self.protocol.take() {
            let old_protocol = reconstruct_protocol(&protocol);
            let protocols = Protocols::default().with_2_4(EnumSet::only(protocol));
            controller.set_protocols(protocols).unwrap();
            self.protocol = Some(old_protocol);
        }

        log_ln!("Wi-Fi Controller Started");
        let is_collector = self.collection_mode == CollectionMode::Collector;
        IS_COLLECTOR.store(is_collector, Ordering::Relaxed);
        set_seq_drop_detection(matches!(
            &self.kind,
            Node::Peripheral(PeripheralOpMode::EspNow(_))
                | Node::Central(CentralOpMode::EspNow(_))
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
        if self.io_tasks.rx_enabled && !is_sniffer {
            set_csi(controller, config.clone());
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
                    // HT40: set the secondary channel before the run loop (which
                    // then skips its own `esp_now.set_channel`). The TX rate/PHY
                    // is forced per-peer inside the run loops (see
                    // `set_peer_espnow_phy`); `esp_now.set_rate` is unused — it
                    // routes to the deprecated `esp_wifi_config_espnow_rate`.
                    if let Some(secondary) = esp_now_config.secondary_channel() {
                        apply_espnow_ht40(controller, esp_now_config.channel(), secondary);
                    }

                    let main_task = run_esp_now_peripheral(
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
                    // HT40 handling mirrors the peripheral ESP-NOW arm above.
                    if let Some(secondary) = esp_now_config.secondary_channel() {
                        apply_espnow_ht40(controller, esp_now_config.channel(), secondary);
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
