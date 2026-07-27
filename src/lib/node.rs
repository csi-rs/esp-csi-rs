//! Node role/configuration types and the [`CSINode`] orchestrator.
//!
//! This module owns the user-facing description of a CSI node — its role
//! ([`NodeRole`], and for a collector its capture path [`CollectorMode`]), the
//! per-mode configs ([`EmitterConfig`], [`WifiSnifferConfig`],
//! [`WifiStationConfig`], [`WifiApConfig`]), and the TX/RX toggles — plus
//! [`CSINode`], whose `run` / `run_duration` wire up Wi-Fi, CSI, and the
//! role-specific tasks. It also holds the shared stop signal and the per-run
//! lifecycle helpers.
//!
//! There are exactly two roles. An **emitter** puts known RF energy into the
//! channel and never captures; a **collector** captures the channel's response
//! and delivers it. Everything else — station, softAP, promiscuous sniffer — is a
//! *way of collecting*, not a role of its own.

#[cfg(any(feature = "async-print", feature = "auto"))]
use embassy_time::with_timeout;

use embassy_futures::join::{join, join3};
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use enumset::EnumSet;
#[cfg(feature = "esp32c5")]
use esp_radio::wifi::BandMode;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Interfaces, Protocol, Protocols, SecondaryChannel, WifiController};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use portable_atomic::Ordering;

use crate::collector::ap::{ap_init, run_ap};
use crate::collector::sta::{run_sta_connect, sta_init};
use crate::config::CsiConfig as CsiConfiguration;
use crate::emitter::{EmitterConfig, run_emitter};
use crate::profile::{RadioProfile, StandardProfile};

use crate::csi::delivery::{
    CSINodeClient, CSI_OUTPUT_ENABLED, build_csi_config, run_process_csi_packet, set_csi,
};
use crate::log_ln;
use crate::radio::{apply_ht40_channel, suppress_espnow_rx};
#[cfg(feature = "esp32c5")]
use crate::radio::apply_band_for_channel;
use crate::stats::set_seq_drop_detection;

// Signals
pub(crate) static STOP_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Per-mutation radio-quiesce delay on C5 dual-band bring-up.
///
/// The C5 Wi-Fi ISR can wedge if a MAC interrupt fires mid-reconfiguration
/// (`set_protocols` / `set_config` STA restart / `set_csi` / `set_channel`),
/// tripping the interrupt watchdog (`handle_interrupts` backtrace at boot) or
/// hard-freezing before any task runs. Dropping the ESP-NOW receive callback at
/// bring-up (see [`crate::radio::suppress_espnow_rx`]) already shrinks that
/// window; inserting a short settle *between* the mutations lets the MAC drain any
/// pending interrupt before the next driver call, shrinking it further. This is a
/// probabilistic mitigation, not a guarantee — the radio restart still races the
/// MAC IRQ — so keeping the air quiet during a node's bring-up remains the most
/// effective measure.
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
            // Channel 1 is typically less congested than 11 in dense
            // residential / office environments.
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
    /// Primary channel of the target AP. On dual-band ESP32-C5 this selects
    /// 2.4 vs 5 GHz (`set_band_mode`) before scan/association.
    pub channel_hint: Option<u8>,
}

impl WifiStationConfig {
    /// Build a station config from esp-radio's [`StationConfig`].
    pub fn new(client_config: StationConfig) -> Self {
        Self {
            client_config,
            channel_hint: None,
        }
    }

    /// Pin the radio band from the AP's primary channel (C5 dual-band only).
    pub fn with_channel_hint(mut self, channel: u8) -> Self {
        self.channel_hint = Some(channel);
        self
    }
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
/// channel) and the static IPv4 addressing used by the built-in DHCP server.
/// The AP hands associating stations addresses from a lease pool in the AP's /24
/// subnet with the gateway set to the AP itself.
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
    /// First IPv4 address in the DHCP lease pool (typically `.2`).
    pub lease_ipv4: core::net::Ipv4Addr,
    /// Number of consecutive lease addresses starting at [`Self::lease_ipv4`]
    /// (e.g. `3` → `.2`, `.3`, `.4`). Default `1` preserves the original
    /// single-client behaviour.
    pub lease_count: u8,
    /// Whether to run the built-in DHCP server. When `false`, the AP only starts
    /// + collects CSI (clients must self-assign IPs).
    pub serve_dhcp: bool,
    /// When `true`, every flood tick fires one unicast frame back-to-back to
    /// **all** active leases instead of advancing one lease per tick (round-robin).
    /// All associated stations then receive their downlink PPDU within tens of
    /// microseconds of each other — temporally-synchronized multi-receiver CSI —
    /// instead of being spread across the whole tick interval.
    ///
    /// This is the workable path to synchronized multi-receiver CSI. A single
    /// group-addressed broadcast frame does *not* work on an ESP32 softAP:
    /// broadcast/multicast is DTIM-buffered, dropped under a high-rate flood, and
    /// only ever sent at the legacy basic rate — so it mostly never leaves the
    /// radio and never honours a forced high-throughput TX rate. Only unicast
    /// transmits immediately and honours the configured TX rate, so N unicast
    /// frames per tick keep near-simultaneous arrival across receivers. Stations
    /// must be **associated** — an unassociated receiver does not reliably
    /// produce CSI from overheard frames.
    ///
    /// Per-receiver rate is the configured ping rate; total offered rate is
    /// `rate * lease_count`, so lower the rate if airtime saturates. Default
    /// `false` preserves per-lease round-robin. Set by [`Self::with_sync_burst`].
    pub sync_burst: bool,
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
            lease_count: 1,
            serve_dhcp: true,
            sync_burst: false,
        }
    }

    /// Override the AP/lease IPv4 addresses (must share a /24).
    pub fn with_ipv4(mut self, ap: core::net::Ipv4Addr, lease: core::net::Ipv4Addr) -> Self {
        self.ap_ipv4 = ap;
        self.lease_ipv4 = lease;
        self
    }

    /// Set the DHCP lease pool size (consecutive addresses from `lease_ipv4`).
    pub fn with_lease_pool(mut self, count: u8) -> Self {
        self.lease_count = count.max(1);
        self
    }

    /// Lease address at `index` (`0` = `lease_ipv4`, `1` = next host, …).
    pub fn lease_ip_at(&self, index: u8) -> core::net::Ipv4Addr {
        let idx = index.min(self.lease_count.saturating_sub(1));
        let mut oct = self.lease_ipv4.octets();
        oct[3] = oct[3].saturating_add(idx);
        core::net::Ipv4Addr::from(oct)
    }

    /// All configured pool addresses (up to [`Self::lease_count`]).
    pub fn lease_pool(&self) -> heapless::Vec<core::net::Ipv4Addr, 8> {
        let mut v = heapless::Vec::new();
        for i in 0..self.lease_count.min(8) {
            let _ = v.push(self.lease_ip_at(i));
        }
        v
    }

    /// Enable or disable the built-in DHCP server (default enabled).
    pub fn with_dhcp_server(mut self, enabled: bool) -> Self {
        self.serve_dhcp = enabled;
        self
    }

    /// Fire one unicast frame back-to-back to every active lease per flood tick,
    /// instead of unicasting round-robin (one lease per tick).
    ///
    /// All associated stations then receive their downlink PPDU within
    /// microseconds of each other — synchronized multi-receiver CSI without the
    /// round-robin spread. This is the workable substitute for a single broadcast
    /// PPDU, which an ESP32 softAP can't reliably deliver (see [`Self::sync_burst`]).
    /// Keep the DHCP server / lease pool enabled so stations associate as genuine
    /// BSS members; only the per-tick transmit pattern changes.
    pub fn with_sync_burst(mut self, enabled: bool) -> Self {
        self.sync_burst = enabled;
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

/// How a collector obtains the frames it measures.
///
/// These are capture paths, not roles: each one ends with this node holding CSI.
/// Which one to use depends on what traffic is available to measure.
pub enum CollectorMode {
    /// Lock a channel in promiscuous mode and measure every frame overheard.
    ///
    /// This is the capture path that pairs with an [`NodeRole::Emitter`]: the
    /// emitter injects unassociated frames and the sniffer measures them, with no
    /// association or handshake between the two.
    Sniffer(WifiSnifferConfig),
    /// Associate as a Wi-Fi station and measure CSI from received frames.
    Station(WifiStationConfig),
    /// Run a self-contained softAP: start an access point (plus a minimal DHCP
    /// server) so a [`CollectorMode::Station`] node can associate and generate
    /// steady uplink traffic, measured as CSI here.
    AccessPoint(WifiApConfig),
}

/// What this node is for.
///
/// A CSI measurement needs energy in the channel and something to measure the
/// channel's response. Those are the two roles, and they are exhaustive.
pub enum NodeRole {
    /// Transmit-only: force a TX PHY and loop-inject sounding frames. Never
    /// captures CSI. See [`crate::emitter`].
    Emitter(EmitterConfig),
    /// Capture the channel response and deliver it, via the chosen capture path.
    /// See [`crate::collector`].
    Collector(CollectorMode),
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

/// Hardware handles required to operate a node in either role.
pub struct NodeHardware<'a> {
    interfaces: &'a mut Interfaces<'static>,
    controller: &'a mut WifiController<'static>,
}

impl<'a> NodeHardware<'a> {
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
    // the log channel or the user's callback. Without this, a collector keeps
    // emitting CSI lines on the serial port well after `send_stop()`.
    crate::csi::delivery::reset();
    crate::stats::reset();
}

/// Primary orchestration object for a CSI node.
///
/// Construct with [`CSINode::new`] (or [`CSINode::new_collector`] for the common
/// case), configure optional protocol / traffic frequency, then call `run()`.
pub struct CSINode<'a> {
    role: NodeRole,
    /// Whether captured CSI is delivered off-device. See
    /// [`CSINode::set_csi_output_enabled`].
    csi_output_enabled: bool,
    io_tasks: IOTaskConfig,
    /// CSI Configuration
    csi_config: Option<CsiConfiguration>,
    /// Traffic Generation Frequency
    traffic_freq_hz: Option<u16>,
    hardware: NodeHardware<'a>,
    protocol: Option<Protocol>,
    /// ICMP flood sends unsolicited echo replies (one-directional traffic)
    /// instead of echo requests. See [`CSINode::set_flood_unsolicited_reply`].
    flood_unsolicited_reply: bool,
    /// Pluggable Wi-Fi bring-up back-end. Defaults to [`StandardProfile`];
    /// override with [`CSINode::set_radio_profile`].
    profile: &'static dyn RadioProfile,
}

impl<'a> CSINode<'a> {
    /// Create a node in the given role.
    ///
    /// CSI output is enabled by default. An [`NodeRole::Emitter`] captures no CSI,
    /// so the setting has no effect there.
    pub fn new(
        role: NodeRole,
        csi_config: Option<CsiConfiguration>,
        traffic_freq_hz: Option<u16>,
        hardware: NodeHardware<'a>,
    ) -> Self {
        Self {
            role,
            csi_output_enabled: true,
            io_tasks: IOTaskConfig::default(),
            csi_config,
            traffic_freq_hz,
            hardware,
            protocol: None,
            flood_unsolicited_reply: false,
            profile: &StandardProfile,
        }
    }

    /// Convenience constructor for a collector node.
    pub fn new_collector(
        mode: CollectorMode,
        csi_config: Option<CsiConfiguration>,
        traffic_freq_hz: Option<u16>,
        hardware: NodeHardware<'a>,
    ) -> Self {
        Self::new(
            NodeRole::Collector(mode),
            csi_config,
            traffic_freq_hz,
            hardware,
        )
    }

    /// Convenience constructor for an emitter node.
    pub fn new_emitter(config: EmitterConfig, hardware: NodeHardware<'a>) -> Self {
        Self::new(NodeRole::Emitter(config), None, None, hardware)
    }

    /// Get the node's role.
    pub fn get_role(&self) -> &NodeRole {
        &self.role
    }

    /// Whether captured CSI is currently delivered off-device.
    pub fn csi_output_enabled(&self) -> bool {
        self.csi_output_enabled
    }

    /// If this is a collector, return its capture mode.
    pub fn get_collector_mode(&self) -> Option<&CollectorMode> {
        match &self.role {
            NodeRole::Collector(mode) => Some(mode),
            NodeRole::Emitter(_) => None,
        }
    }

    /// If this is an emitter, return its configuration.
    pub fn get_emitter_config(&self) -> Option<&EmitterConfig> {
        match &self.role {
            NodeRole::Emitter(config) => Some(config),
            NodeRole::Collector(_) => None,
        }
    }

    /// Update CSI configuration.
    pub fn set_csi_config(&mut self, config: CsiConfiguration) {
        self.csi_config = Some(config);
    }

    /// Update Wi-Fi Station configuration (only applies to a station collector).
    pub fn set_station_config(&mut self, config: WifiStationConfig) {
        if let NodeRole::Collector(CollectorMode::Station(_)) = &mut self.role {
            self.role = NodeRole::Collector(CollectorMode::Station(config));
        }
    }

    /// Set traffic generation frequency in Hz (station / softAP collectors).
    pub fn set_traffic_frequency(&mut self, freq_hz: u16) {
        self.traffic_freq_hz = Some(freq_hz);
    }

    /// Enable or disable delivery of captured CSI off-device.
    ///
    /// When disabled the radio still captures CSI — keeping the RX path and its
    /// timing identical — but nothing is decoded, logged, or handed to a callback.
    /// Useful for a node whose only job is to keep traffic on air, or for
    /// measuring capture overhead without the delivery cost.
    ///
    /// Has no effect on an [`NodeRole::Emitter`], which captures nothing.
    pub fn set_csi_output_enabled(&mut self, enabled: bool) {
        self.csi_output_enabled = enabled;
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

    /// Replace the node's role.
    pub fn set_role(&mut self, role: NodeRole) {
        self.role = role;
    }

    /// Set Wi-Fi protocol (overrides default).
    pub fn set_protocol(&mut self, protocol: Protocol) {
        self.protocol = Some(protocol);
    }

    /// Install a Wi-Fi bring-up profile (overrides the default
    /// [`StandardProfile`]). Pass a reference to a zero-sized profile value,
    /// e.g. `node.set_radio_profile(&MyProfile);`.
    pub fn set_radio_profile(&mut self, profile: &'static dyn RadioProfile) {
        self.profile = profile;
    }

    /// Make the ICMP traffic flood send unsolicited echo **replies** instead
    /// of echo requests.
    ///
    /// The peer's IP stack silently ignores an unsolicited reply, so the
    /// generated traffic becomes strictly one-directional: the peer still
    /// hardware-ACKs every data frame (rate control stays fed) and captures
    /// CSI per frame, but never transmits an IP-level response. This halves
    /// the on-air frame count versus request/reply and stabilizes the offered
    /// rate under CSMA contention. Trade-off: this node receives no CSI back
    /// from the peer's replies.
    pub fn set_flood_unsolicited_reply(&mut self, enabled: bool) {
        self.flood_unsolicited_reply = enabled;
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

        // Applied every run (not only when set) so the process-wide flood-kind
        // flag never leaks from a previous, differently-configured run.
        crate::collector::sta::set_icmp_flood_unsolicited(self.flood_unsolicited_reply);

        // Silence esp-radio's built-in ESP-NOW receive dispatcher before any other
        // Wi-Fi reconfiguration runs — see `suppress_espnow_rx` for why this must
        // happen this early even though no role here speaks ESP-NOW.
        suppress_espnow_rx();
        // Let the freshly-constructed radio state settle before the first C5
        // reconfiguration mutation (no-op off C5).
        c5_radio_settle().await;

        let is_ap = matches!(
            &self.role,
            NodeRole::Collector(CollectorMode::AccessPoint(_))
        );
        let is_sniffer = matches!(&self.role, NodeRole::Collector(CollectorMode::Sniffer(_)));
        let is_emitter = matches!(&self.role, NodeRole::Emitter(_));

        // An emitter never captures, so CSI is only ever armed for a collector.
        // Everything downstream keys off this rather than re-testing the role.
        let rx_enabled = self.io_tasks.rx_enabled && !is_emitter;

        // Radio-profile back-end (Copy handle; does not alias `self.hardware`).
        // `bringup` decides whether the profile takes over the extended Wi-Fi
        // bring-up sequence for this role/protocol.
        let profile = self.profile;
        let bringup = profile.wants_bringup(&self.role, self.protocol);

        // Apply protocol before STA bring-up / CSI. Generic chip-level tuning
        // lives in the radio profile; specialised back-ends may rebuild the set
        // entirely. Skipped for an emitter, which pins its own protocol set during
        // bring-up to match its forced TX PHY.
        if let Some(protocol) = self.protocol.take() {
            if !is_emitter {
                let base = Protocols::default().with_2_4(EnumSet::only(protocol));
                let protocols = profile.tune_protocols(&self.role, protocol, base);
                controller.set_protocols(protocols).unwrap();
                c5_radio_settle().await;
            }
            self.protocol = Some(protocol);
        }

        if bringup && !is_emitter {
            profile.apply_bandwidth(controller);
            c5_radio_settle().await;
        }

        // Tasks necessary for a station collector.
        let sta_interface =
            if let NodeRole::Collector(CollectorMode::Station(config)) = &self.role {
                #[cfg(feature = "esp32c5")]
                if let Some(channel) = config.channel_hint {
                    apply_band_for_channel(controller, channel);
                    c5_radio_settle().await;
                }
                Some(sta_init(
                    &mut interfaces.station,
                    config,
                    controller,
                    profile,
                    bringup,
                ))
            } else {
                None
            };
        if bringup && sta_interface.is_some() {
            profile.apply_protocols_post(controller);
            c5_radio_settle().await;
        }

        // Self-contained softAP: bring up the AP-side embassy-net stack (static
        // IP) and apply the AP config to the controller. `interfaces.access_point`
        // is disjoint from `.station`/`.sniffer`, so this borrow is fine.
        let ap_interface = if let NodeRole::Collector(CollectorMode::AccessPoint(config)) =
            &self.role
        {
            #[cfg(feature = "esp32c5")]
            if config.secondary_channel().is_none() {
                apply_band_for_channel(controller, config.channel());
            }
            if let Some(secondary) = config.secondary_channel() {
                apply_ht40_channel(controller, config.channel(), secondary);
                c5_radio_settle().await;
            }
            let ifaces = ap_init(
                &mut interfaces.access_point,
                config,
                controller,
                profile,
                bringup,
            );
            if bringup {
                profile.apply_protocols_post(controller);
            }
            // The AP `set_config` restarts the radio; settle before `set_csi`.
            c5_radio_settle().await;
            Some(ifaces)
        } else {
            None
        };

        // Build CSI Configuration. An emitter captures nothing, so this is only
        // meaningful for a collector — but it is cheap and keeps the flow linear.
        let mut config = match self.csi_config {
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
        // Let the radio profile enable any extra acquisition modes it needs
        // (default is a no-op) before the config is registered/cloned.
        profile.tune_csi_acquisition(&mut config);

        log_ln!("Wi-Fi Controller Started");
        CSI_OUTPUT_ENABLED.store(self.csi_output_enabled, Ordering::Relaxed);
        // Sequence-drop detection tracks per-source-MAC sequence numbers, so it
        // works for any collector: the emitter's driver-assigned incrementing
        // sequence numbers make gaps in a capture measurable.
        set_seq_drop_detection(!is_emitter);

        // Keep a clone so the STA recovery path in `run_sta_connect` can re-apply
        // after a stop/start cycle (stop clears the CSI filter/callback).
        //
        // Only register the CSI callback when RX is actually enabled — otherwise
        // the radio fires `capture_csi_info` for every overheard 802.11 frame on
        // the WiFi task hot path for no purpose.
        let csi_config_for_recovery = config.clone();
        // The sniffer arm sets CSI after locking its channel; the AP arm sets it
        // inside `run_ap`, because `set_config(AccessPoint)` restarts the radio and
        // clears the CSI filter.
        if rx_enabled && !is_sniffer && !is_ap {
            set_csi(controller, config.clone());
            // Settle after enabling CSI before the role task issues its first
            // set_channel / TX so the run loop doesn't start into a pending IRQ.
            c5_radio_settle().await;
        }
        // Immutable borrow of a *different* `interfaces` field than the station
        // arm touches, so this disjoint borrow is fine. Used by the sniffer arm and
        // to clear promiscuous mode on station shutdown.
        let sniffer = &interfaces.sniffer;

        match &self.role {
            NodeRole::Emitter(emitter_config) => {
                // The emitter owns its whole bring-up (forced TX PHY, unassociated
                // interface start, channel lock) inside `run_emitter`, because the
                // forced rate has to be applied before the interface starts.
                let main_task = run_emitter(controller, interfaces, emitter_config);
                drive_main(main_task, false, duration, client).await;
            }
            NodeRole::Collector(mode) => match mode {
                CollectorMode::Sniffer(sniffer_config) => {
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
                    if bringup {
                        profile.apply_sniffer_radio(controller);
                        c5_radio_settle().await;
                    }
                    if rx_enabled {
                        set_csi(controller, config.clone());
                    }
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
                CollectorMode::AccessPoint(ap_config) => {
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
                CollectorMode::Station(_sta_config) => {
                    // 1. Connect to the Wi-Fi network.
                    // 2. Run DHCP / NTP sync if enabled in config.
                    // 3. Drive STA connection handling and network operations.
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
