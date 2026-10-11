//! The [`CsiNode`] orchestrator and the Wi-Fi mode configs.
//!
//! [`CsiNode`] holds one [`Mode`] and one [`MeasurementSetup`](crate::MeasurementSetup), and its
//! `run` / `run_duration` wire up Wi-Fi, CSI and the mode's tasks. This module also owns the
//! configs of the three Wi-Fi modes ([`WifiSnifferConfig`], [`WifiStationConfig`],
//! [`WifiApConfig`]), the hardware bundle ([`NodeHardware`]), the TX/RX toggles ([`IOTaskConfig`]),
//! the shared stop signal and the per-run lifecycle helpers. The emitter's config is
//! [`EmitterConfig`] and the ESP-NOW modes' is [`EspNowConfig`](crate::EspNowConfig).
//!
//! A node is described by the attributes in [`crate::model`]: its session role, sensing role and
//! reporting are read back from the mode, never stored beside it. Each mode's config exposes only
//! the attributes that mode admits, so a node that the model rules out cannot be built.

#[cfg(any(feature = "async-print", feature = "auto"))]
use embassy_time::with_timeout;

use embassy_futures::join::{join, join3};
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use enumset::EnumSet;
#[cfg(feature = "esp32c5")]
use esp_radio::wifi::BandMode;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Protocol, Protocols, SecondaryChannel, WifiController};

use crate::radio::RadioInterfaces;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use portable_atomic::Ordering;

use crate::wifi::ap::{ap_init, run_ap};
use crate::wifi::sta::{run_sta_connect, sta_init};
use crate::CsiConfig;
use crate::MeasurementSetup;
use crate::emitter::{EmitterConfig, run_emitter};
use crate::initiator::esp_now::run_esp_now_initiator;
use crate::initiator::esp_now_simplex::run_esp_now_simplex_source;
use crate::model::{Mode, NodeView, SimplexConfig};
use crate::responder::esp_now::run_esp_now_responder;
use crate::responder::esp_now_simplex::run_esp_now_simplex_peer;
use crate::profile::{RadioProfile, StandardProfile};

use crate::csi::delivery::{
    CsiNodeClient, build_csi_config, run_process_csi_packet, set_csi,
};
use crate::log_ln;
use crate::radio::{apply_ht40_channel, idle_station_for_espnow, suppress_espnow_rx};
#[cfg(feature = "esp32c5")]
use crate::radio::{apply_band_auto, apply_band_for_channel};
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

async fn csi_data_collection(client: &mut CsiNodeClient, duration: u64) {
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
    reporting: crate::Reporting,
}

impl WifiSnifferConfig {
    /// A sniffer takes part in no sensing session: it observes traffic it neither originates nor
    /// answers, so its session role is `None` (an observer). There is no setter for this.
    pub const fn session_role() -> Option<crate::SessionRole> {
        None
    }

    /// A sniffer is always a [`Receiver`](crate::SensingRole::Receiver): it never transmits.
    /// There is no setter for this.
    pub const fn sensing_role() -> crate::SensingRole {
        crate::SensingRole::Receiver
    }

    /// How this sniffer reports: [`Reporting::HOST`](crate::Reporting::HOST) unless set with
    /// [`with_report_to`](Self::with_report_to), [`with_threshold`](Self::with_threshold) or
    /// [`with_decimation`](Self::with_decimation). Never [`None`](crate::Reporting::None): a sniffer
    /// that does not report observes nothing, so there is no way to configure one.
    pub fn get_reporting(&self) -> crate::Reporting {
        self.reporting
    }

    /// Report every measurement to `to`.
    pub fn with_report_to(mut self, to: crate::ReportTo) -> Self {
        self.reporting = crate::Reporting::Report { to };
        self
    }

    /// Report only while the channel is moving (threshold-based reporting). See
    /// [`Threshold`](crate::Threshold).
    pub fn with_threshold(mut self, threshold: crate::Threshold) -> Self {
        self.reporting = crate::Reporting::ThresholdBased(threshold);
        self
    }

    /// Report every `n`th measurement.
    pub fn with_decimation(mut self, n: u16) -> Self {
        self.reporting = crate::Reporting::Decimate(n.max(1));
        self
    }
}

impl Default for WifiSnifferConfig {
    fn default() -> Self {
        Self {
            mac_filter: None,
            reporting: crate::Reporting::HOST,
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
    /// Whether this station drives the measured traffic. A station that pings its gateway to keep
    /// the link busy originates the soundings and so is the session's
    /// [`Initiator`](crate::SessionRole::Initiator); one that merely measures an already-busy link
    /// takes part in no session it set up and has no session role (`None`).
    session: Option<crate::SessionRole>,
    /// Whether, and how often, this node reports the CSI it captures.
    reporting: crate::Reporting,
}

impl WifiStationConfig {
    /// Build a station config from esp-radio's [`StationConfig`].
    pub fn new(client_config: StationConfig) -> Self {
        Self {
            client_config,
            channel_hint: None,
            session: Some(crate::SessionRole::Initiator),
            reporting: crate::Reporting::HOST,
        }
    }

    /// Set whether this station drives the measured traffic: `Some(Initiator)` when its gateway
    /// ping (the [periodicity](crate::MeasurementSetup::periodicity_hz)) is the traffic being
    /// measured, `None` when it passively measures a link something else keeps busy. Defaults to
    /// `Some(Initiator)`.
    ///
    /// The value describes the node; the ping itself runs whenever a periodicity is set.
    pub fn session(mut self, role: Option<crate::SessionRole>) -> Self {
        self.session = role;
        self
    }

    /// Set whether, and how often, this node reports its CSI. Defaults to
    /// [`Reporting::HOST`](crate::Reporting::HOST).
    pub fn reporting(mut self, reporting: crate::Reporting) -> Self {
        self.reporting = reporting;
        self
    }

    /// Whether this station drives the measured traffic (`Some(Initiator)`) or not (`None`).
    pub fn session_role(&self) -> Option<crate::SessionRole> {
        self.session
    }

    /// [`TransmitterReceiver`](crate::SensingRole::TransmitterReceiver) for a station that drives
    /// the traffic it measures, [`Receiver`](crate::SensingRole::Receiver) otherwise.
    pub fn sensing_role(&self) -> crate::SensingRole {
        match self.session {
            Some(crate::SessionRole::Initiator) => crate::SensingRole::TransmitterReceiver,
            _ => crate::SensingRole::Receiver,
        }
    }

    /// Whether, and how often, this node reports the CSI it captures, as configured.
    pub fn get_reporting(&self) -> crate::Reporting {
        self.reporting
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

/// Configuration for the self-contained softAP mode.
///
/// Wraps esp-radio's [`AccessPointConfig`] (SSID, channel, auth, secondary
/// channel) and the static IPv4 addressing used by the built-in DHCP server.
/// The AP hands associating stations addresses from a lease pool in the AP's /24
/// subnet with the gateway set to the AP itself.
///
/// `channel`/`secondary_channel` are duplicated here because esp-radio's
/// `AccessPointConfig` fields are not externally readable; [`CsiNode`] needs them
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
    /// Whether this node reports the CSI it captures. The access point's roles are not
    /// configurable — it is the session's initiator by construction — but whether its measurements
    /// leave it is.
    reporting: crate::Reporting,
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
            reporting: crate::Reporting::HOST,
        }
    }

    /// An access point is always the session's [`Initiator`](crate::SessionRole::Initiator): its
    /// beacons and the downlink it drives to its stations originate the session, so there is no
    /// setter for this.
    pub const fn session_role() -> Option<crate::SessionRole> {
        Some(crate::SessionRole::Initiator)
    }

    /// An access point is a [`Receiver`](crate::SensingRole::Receiver): it measures the uplink its
    /// associated stations send. There is no setter for this.
    pub const fn sensing_role() -> crate::SensingRole {
        crate::SensingRole::Receiver
    }

    /// Set whether, and how often, this node reports its CSI. Defaults to
    /// [`Reporting::HOST`](crate::Reporting::HOST).
    pub fn reporting(mut self, reporting: crate::Reporting) -> Self {
        self.reporting = reporting;
        self
    }

    /// Whether, and how often, this node reports the CSI it captures, as configured.
    pub fn get_reporting(&self) -> crate::Reporting {
        self.reporting
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

/// Placeholder for the initiator driver's unused `_mac_addr` parameter. See its call site.
const UNSET_MAC: [u8; 6] = [0; 6];

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

/// Hardware handles required to operate a node in any mode.
pub struct NodeHardware<'a> {
    interfaces: RadioInterfaces,
    controller: &'a mut WifiController<'static>,
}

impl<'a> NodeHardware<'a> {
    /// Create a hardware bundle from the `WifiController`.
    ///
    /// Claims the station and access-point interfaces, the sniffer and ESP-NOW from the controller.
    /// Build one per controller: the interfaces are singletons, so a second bundle panics.
    pub fn new(controller: &'a mut WifiController<'static>) -> Self {
        Self {
            interfaces: RadioInterfaces::claim(controller),
            controller,
        }
    }
}

pub(crate) fn reset_globals() {
    // Close all CSI delivery gates so any late-firing WiFi callback runs
    // are no-ops. The CSI callback stays registered with esp-radio after stop
    // (the radio itself is still up), but with the gates closed the callback
    // short-circuits before it touches the log channel or the user's callback.
    // Without this, a receiver keeps emitting CSI lines on the serial port
    // well after `send_stop()`.
    //
    // The statistics counters are deliberately NOT cleared here. This runs at the END of
    // `run_inner`, so clearing them destroyed the run's numbers at the moment the run finished —
    // every post-collection `show-stats` on the classic AP/STA path reported `RX Total Packets: 0`
    // however many frames the callback had counted, which reads as "the radio received nothing"
    // when the truth was "the radio received plenty and the log path could not keep up". The
    // counters are the only evidence a user has that captured CSI was dropped rather than never
    // captured, and this was throwing that evidence away.
    //
    // `stats::reset` now runs at the START of a run instead (see `run_inner`), which is both what
    // the README already documents ("counters reset on the start of each new `start` collection")
    // and what an out-of-tree run loop does via `stats_begin_run`.
    crate::csi::delivery::reset();
}

/// Primary orchestration object for a CSI node.
///
/// Construct with a per-mode constructor — [`CsiNode::sniffer`], [`CsiNode::station`],
/// [`CsiNode::access_point`], [`CsiNode::emitter`], [`CsiNode::esp_now`],
/// [`CsiNode::esp_now_simplex_source`] or [`CsiNode::esp_now_simplex_peer`] — or with
/// [`CsiNode::new`] and a [`Mode`]. Every constructor except the emitter's takes a
/// [`MeasurementSetup`] (periodicity, bandwidth, acquisition); configure an optional protocol,
/// then call `run()`.
pub struct CsiNode<'a> {
    /// How this node reaches the channel. The node's roles and reporting are read back from it
    /// rather than stored alongside it — see [`Mode`].
    mode: Mode,
    io_tasks: IOTaskConfig,
    /// Periodicity, bandwidth, acquisition and setup id.
    setup: MeasurementSetup,
    hardware: NodeHardware<'a>,
    protocol: Option<Protocol>,
    /// ICMP flood sends unsolicited echo replies (one-directional traffic)
    /// instead of echo requests. See [`CsiNode::set_flood_unsolicited_reply`].
    flood_unsolicited_reply: bool,
    /// Pluggable Wi-Fi bring-up back-end. Defaults to [`StandardProfile`];
    /// override with [`CsiNode::set_radio_profile`].
    profile: &'static dyn RadioProfile,
}

impl<'a> CsiNode<'a> {
    /// Create a node that reaches the channel through `mode`, measuring under `setup`.
    ///
    /// The node's session role, sensing role and reporting come from the mode's config — see the
    /// per-mode constructors below, which are the ergonomic way in.
    pub fn new(mode: Mode, setup: MeasurementSetup, hardware: NodeHardware<'a>) -> Self {
        Self {
            mode,
            io_tasks: IOTaskConfig::default(),
            setup,
            hardware,
            protocol: None,
            flood_unsolicited_reply: false,
            profile: &StandardProfile,
        }
    }

    /// A node in the symmetric ESP-NOW exchange. Initiator or responder, reporting or not — set
    /// both on the [`EspNowConfig`](crate::EspNowConfig); every combination is meaningful. The
    /// setup's periodicity is the initiator's control-packet rate and the responder's reply
    /// pacing.
    pub fn esp_now(
        config: crate::EspNowConfig,
        setup: MeasurementSetup,
        hardware: NodeHardware<'a>,
    ) -> Self {
        Self::new(Mode::EspNow(config), setup, hardware)
    }

    /// The flooding end of the asymmetric ESP-NOW exchange: an **initiator and sensing transmitter
    /// that reports nothing**. It owns all the transmit airtime and measures nothing, so the
    /// setup's acquisition is ignored; its periodicity paces the flood.
    pub fn esp_now_simplex_source(
        config: crate::EspNowConfig,
        setup: MeasurementSetup,
        hardware: NodeHardware<'a>,
    ) -> Self {
        Self::new(
            Mode::EspNowSimplex(SimplexConfig::source(config)),
            setup,
            hardware,
        )
    }

    /// The measuring end of the asymmetric ESP-NOW exchange: a **responder and sensing receiver
    /// that reports every measurement**. It beacons until it is found, then goes receive-only; it
    /// sounds nothing, so the setup's periodicity is ignored.
    pub fn esp_now_simplex_peer(
        channel: u8,
        setup: MeasurementSetup,
        hardware: NodeHardware<'a>,
    ) -> Self {
        Self::new(
            Mode::EspNowSimplex(SimplexConfig::peer(channel)),
            setup,
            hardware,
        )
    }

    /// A promiscuous sniffer: an **observer** (no session role) and sensing receiver, always. It
    /// never transmits, and a sniffer that does not report observes nothing. It sounds nothing, so
    /// the setup's periodicity is ignored.
    pub fn sniffer(
        config: WifiSnifferConfig,
        setup: MeasurementSetup,
        hardware: NodeHardware<'a>,
    ) -> Self {
        Self::new(Mode::Sniffer(config), setup, hardware)
    }

    /// A Wi-Fi station associated to an access point or a commercial router. The setup's
    /// periodicity is its gateway ping rate; it is the session's initiator when that ping is the
    /// traffic being measured (see [`WifiStationConfig::session`]).
    pub fn station(
        config: WifiStationConfig,
        setup: MeasurementSetup,
        hardware: NodeHardware<'a>,
    ) -> Self {
        Self::new(Mode::Station(config), setup, hardware)
    }

    /// A self-contained softAP with DHCP. Always the session's **initiator** and a sensing
    /// receiver; reporting is configurable. The setup's periodicity is its downlink flood rate.
    pub fn access_point(
        config: WifiApConfig,
        setup: MeasurementSetup,
        hardware: NodeHardware<'a>,
    ) -> Self {
        Self::new(Mode::AccessPoint(config), setup, hardware)
    }

    /// An unassociated transmit-only sounding node: a **sensing transmitter that reports nothing**,
    /// with no session role, always. It measures nothing and takes its period from the
    /// [`EmitterConfig`], so it takes no measurement setup.
    pub fn emitter(config: EmitterConfig, hardware: NodeHardware<'a>) -> Self {
        Self::new(Mode::Emitter(config), MeasurementSetup::new(), hardware)
    }

    /// How this node reaches the channel.
    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// The node's IEEE 802.11bf session role, or `None` for an observer. Computed from the mode.
    pub fn session_role(&self) -> Option<crate::SessionRole> {
        self.mode.session_role()
    }

    /// The node's IEEE 802.11bf sensing role. Computed from the mode.
    pub fn sensing_role(&self) -> crate::SensingRole {
        self.mode.sensing_role()
    }

    /// Whether, and how often, this node's measurements leave it, **as configured**. A responder
    /// paired with a silent initiator promotes itself mid-run without rewriting its configuration,
    /// so this and [`runtime_reporting`](crate::runtime_reporting) can legitimately differ.
    pub fn reporting(&self) -> crate::Reporting {
        self.mode.reporting()
    }

    /// The node's measurement setup.
    pub fn measurement_setup(&self) -> &MeasurementSetup {
        &self.setup
    }

    /// If this is an emitter, return its configuration.
    pub fn get_emitter_config(&self) -> Option<&EmitterConfig> {
        match &self.mode {
            Mode::Emitter(config) => Some(config),
            _ => None,
        }
    }

    /// Take on an IEEE 802.11bf-style [`MeasurementSetup`](crate::wire::MeasurementSetup).
    ///
    /// The setup is mapped onto what this mode actually does, and refused — with nothing changed —
    /// if any parameter cannot be honoured:
    ///
    /// - `id` is stamped on every controlled sounding's
    ///   [`Stimulus`](crate::wire::Stimulus::Controlled); each ESP-NOW control frame is one
    ///   measurement instance.
    /// - `acquisition.channel` replaces the mode's channel (the station's channel hint).
    /// - `acquisition.transmitter` becomes the CSI peer filter ([`crate::set_csi_peer_filter`]).
    /// - `acquisition.ppdu_formats`: every format, HE only (ESP32-C5/C6 HE-LTF acquisition), or
    ///   no legacy formats. Other subsets are refused.
    /// - `acquisition.bandwidth` and `stimulus.bandwidth` must match the mode's configured width.
    /// - `stimulus.periodicity_us` becomes the node's
    ///   [periodicity](MeasurementSetup::periodicity_hz) — the ESP-NOW initiator's control-packet
    ///   rate, the station's or access point's ping rate — or the emitter's frame period. A mode
    ///   that does not sound the channel (a sniffer, an ESP-NOW responder or simplex peer) refuses
    ///   any stimulus.
    /// - `stimulus.responders`: none, or exactly one, which becomes the CSI peer filter.
    /// - Grouped reports, `ng`/`nb` other than 1/8, and trigger-based sounding are refused: the
    ///   radio produces raw estimates from non-TB soundings only.
    pub fn apply_measurement_setup(
        &mut self,
        setup: &crate::wire::MeasurementSetup,
    ) -> Result<(), crate::wire::SetupError> {
        use crate::wire::{ReportType, SetupError};

        let width = self.configured_bandwidth();
        let sounds = match &self.mode {
            Mode::EspNow(c) => c.session_role() == crate::SessionRole::Initiator,
            Mode::EspNowSimplex(c) => c.is_source(),
            Mode::Station(c) => c.session_role() == Some(crate::SessionRole::Initiator),
            Mode::AccessPoint(_) | Mode::Emitter(_) => true,
            Mode::Sniffer(_) => false,
        };

        // Validate everything before changing anything.
        let acq = &setup.acquisition;
        if let (Some(want), Some(have)) = (acq.bandwidth, width)
            && want != have
        {
            return Err(SetupError::UnsupportedAcquisition("bandwidth"));
        }
        let formats = acq.ppdu_formats;
        let he_only = formats.only_he();
        if he_only && !cfg!(any(feature = "esp32c5", feature = "esp32c6")) {
            return Err(SetupError::UnsupportedAcquisition("ppdu_formats"));
        }
        if !formats.is_any() && !he_only && !formats.excludes_legacy() {
            return Err(SetupError::UnsupportedAcquisition("ppdu_formats"));
        }
        let mut filter = acq.transmitter;
        let mut rate = None;
        if let Some(st) = &setup.stimulus {
            if !sounds {
                return Err(SetupError::NoStimulus);
            }
            if st.report != ReportType::Raw {
                return Err(SetupError::UnsupportedStimulus("report"));
            }
            if st.ng != 1 || st.nb != 8 {
                return Err(SetupError::UnsupportedStimulus("ng/nb"));
            }
            if st.trigger_based {
                return Err(SetupError::UnsupportedStimulus("trigger_based"));
            }
            if width.is_some_and(|w| w != st.bandwidth) {
                return Err(SetupError::UnsupportedStimulus("bandwidth"));
            }
            match st.responders.as_slice() {
                [] => {}
                [one] => match filter {
                    Some(t) if t != *one => {
                        return Err(SetupError::UnsupportedStimulus("responders"));
                    }
                    _ => filter = Some(*one),
                },
                _ => return Err(SetupError::UnsupportedStimulus("responders")),
            }
            rate = Some(st.rate_hz());
        }

        // Apply.
        if let Some(ch) = acq.channel {
            self.set_mode_channel(ch);
        }
        if let Some(mac) = filter {
            crate::set_csi_peer_filter(mac);
        }
        if he_only {
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            {
                self.setup.set_acquisition(Some(CsiConfig::he20()));
            }
        } else if formats.excludes_legacy() {
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            {
                let mut c = self.setup.get_acquisition().cloned().unwrap_or_default();
                c.acquire_csi_legacy = 0;
                #[cfg(feature = "esp32c5")]
                {
                    c.acquire_csi_force_lltf = false;
                }
                self.setup.set_acquisition(Some(c));
            }
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            crate::set_csi_min_sig_mode(1);
        }
        if let Some(hz) = rate {
            match &mut self.mode {
                Mode::Emitter(c) => {
                    c.period = Duration::from_micros(1_000_000 / hz.max(1) as u64);
                }
                _ => self.setup.set_periodicity_hz(Some(hz)),
            }
        }
        self.setup.set_id(setup.id);
        Ok(())
    }

    /// The channel width the mode is configured for, where it fixes one.
    fn configured_bandwidth(&self) -> Option<crate::wire::Bandwidth> {
        use crate::wire::Bandwidth;
        let forty = match &self.mode {
            Mode::EspNow(c) => c.secondary_channel().is_some(),
            Mode::EspNowSimplex(c) => c.inner().secondary_channel().is_some(),
            Mode::AccessPoint(c) => c.secondary_channel.is_some(),
            Mode::Emitter(c) => c.phy.is_forty(),
            // A station's width is whatever it associates at; a sniffer's follows the PPDU.
            Mode::Station(_) | Mode::Sniffer(_) => return None,
        };
        Some(if forty { Bandwidth::Mhz40 } else { Bandwidth::Mhz20 })
    }

    /// Replace the mode's primary channel.
    fn set_mode_channel(&mut self, ch: u8) {
        match &mut self.mode {
            Mode::EspNow(c) => c.channel = ch,
            Mode::EspNowSimplex(c) => c.set_channel(ch),
            Mode::Sniffer(c) => c.channel = ch,
            Mode::Station(c) => c.channel_hint = Some(ch),
            Mode::AccessPoint(c) => {
                c.channel = ch;
                c.ap_config = c.ap_config.clone().with_channel(ch);
            }
            Mode::Emitter(c) => c.channel = ch,
        }
    }

    /// Replace the measurement setup.
    pub fn set_measurement_setup(&mut self, setup: MeasurementSetup) {
        self.setup = setup;
    }

    /// Replace the acquisition (CSI configuration) of the measurement setup.
    pub fn set_csi_config(&mut self, config: CsiConfig) {
        self.setup.set_acquisition(Some(config));
    }

    /// Update Wi-Fi Station configuration (only applies to a station node).
    pub fn set_station_config(&mut self, config: WifiStationConfig) {
        if let Mode::Station(_) = &self.mode {
            self.mode = Mode::Station(config);
        }
    }

    /// Set the periodicity of the measurement setup, in Hz. See
    /// [`MeasurementSetup::periodicity_hz`] for which modes read it.
    pub fn set_periodicity_hz(&mut self, hz: u16) {
        self.setup.set_periodicity_hz(Some(hz));
    }

    /// Replace the TX/RX task configuration. See [`IOTaskConfig`]; the two fields can also be set
    /// one at a time with [`set_tx_enabled`](Self::set_tx_enabled) and
    /// [`set_rx_enabled`](Self::set_rx_enabled).
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

    /// Replace how this node reaches the channel, and with it the roles and reporting the new mode
    /// implies. Used to run one node through several configurations without reconstructing it —
    /// see `examples/runtime_config.rs`.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
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

    /// Run the node for `duration` seconds with internal draining of the measurements.
    ///
    /// This initializes Wi-Fi, configures CSI, and starts mode-specific tasks.
    pub async fn run_duration(&mut self, duration: u64, client: &mut CsiNodeClient) {
        self.run_inner(Some(duration), Some(client)).await;
    }

    /// Shared implementation behind [`run`](Self::run) and
    /// [`run_duration`](Self::run_duration).
    ///
    /// `duration`/`client` are `Some` only on the timed `run_duration` path:
    /// when set, each mode arm runs an extra concurrent future that stops the
    /// node after `duration` seconds (and, with RX enabled, drains CSI to the
    /// logger via `client`). When `None` the node runs until externally
    /// stopped via [`CsiNodeClient::send_stop`].
    async fn run_inner(&mut self, duration: Option<u64>, client: Option<&mut CsiNodeClient>) {
        // Zero the counters and stamp the capture start so `show-stats` describes THIS run, and
        // still describes it after the run ends. Deliberately here rather than in `reset_globals`,
        // which runs at stop — see the note there. Mirrors what an out-of-tree receiver path
        // does with `stats_begin_run`.
        #[cfg(feature = "statistics")]
        crate::stats::stats_begin_run();

        // A requested width the mode's own config contradicts is reported, not applied: the
        // config is where the width is set (see `MeasurementSetup::bandwidth`).
        if let (Some(want), Some(have)) = (self.setup.get_bandwidth(), self.configured_bandwidth())
            && want != have
        {
            let mhz = |b: crate::wire::Bandwidth| -> u16 {
                use crate::wire::Bandwidth::*;
                match b {
                    Mhz20 => 20,
                    Mhz40 => 40,
                    Mhz80 => 80,
                    Mhz160 => 160,
                    Mhz320 => 320,
                }
            };
            log_ln!(
                "Measurement setup asks for {} MHz but the mode is configured for {} MHz; running at {} MHz",
                mhz(want),
                mhz(have),
                mhz(have)
            );
        }

        let interfaces = &mut self.hardware.interfaces;
        let controller = &mut self.hardware.controller;

        // Applied every run (not only when set) so the process-wide flood-kind
        // flag never leaks from a previous, differently-configured run.
        crate::wifi::sta::set_icmp_flood_unsolicited(self.flood_unsolicited_reply);

        // Applied every run, for the same reason as the flood flag above: the gate is
        // process-wide, so a node left reporting `None` by a previous run would silently stop
        // processing CSI in this one. Read by the ESP-NOW initiator to announce whether it
        // reports.
        crate::set_runtime_reporting(self.mode.reporting());

        // Session context for this run's frames. The ESP-NOW modes sound the channel themselves,
        // so their measurements are of a controlled stimulus; every other mode measures traffic it
        // did not originate. A sniffer has no other way to attribute that traffic, so it captures
        // the MAC header digest by default.
        crate::csi::session::begin_run(
            matches!(
                self.mode,
                Mode::EspNow(_) | Mode::EspNowSimplex(_)
            ),
            matches!(self.mode, Mode::Sniffer(_)),
        );
        crate::csi::session::set_setup_id(self.setup.get_id());

        // Deal with esp-radio's built-in ESP-NOW receive dispatcher before any other Wi-Fi
        // reconfiguration runs — see `suppress_espnow_rx` for why this must happen this early.
        //
        // WHICH treatment depends on the role, and getting it wrong is silent. `suppress_espnow_rx`
        // permanently UNREGISTERS the callback, which is right for the 802.11 roles — none of them
        // reads ESP-NOW, and the stock dispatcher heap-allocates every overheard vendor action
        // frame into a deque nothing drains. It is exactly wrong for the ESP-NOW roles, which would
        // then be deaf: a responder would never see a control frame and a simplex source would never
        // hear the discovery beacon, both while looking perfectly healthy.
        //
        // So an ESP-NOW role installs the static-pool dispatcher instead, which replaces the
        // allocating one rather than removing it — same protection against the heap growth, and the
        // frames still arrive.
        if matches!(
            &self.mode,
            Mode::EspNow(_) | Mode::EspNowSimplex(_)
        ) {
            crate::esp_now_pool::install();
            idle_station_for_espnow(controller).await;
        } else {
            suppress_espnow_rx();
        }
        // Let the freshly-constructed radio state settle before the first C5
        // reconfiguration mutation (no-op off C5).
        c5_radio_settle().await;

        let is_ap = matches!(&self.mode, Mode::AccessPoint(_));
        let is_sniffer = matches!(&self.mode, Mode::Sniffer(_));
        let is_emitter = matches!(&self.mode, Mode::Emitter(_));

        // An emitter never captures, so CSI is only ever armed for a receiver.
        // Everything downstream keys off this rather than re-testing the role.
        let rx_enabled = self.io_tasks.rx_enabled && !is_emitter;

        // Radio-profile back-end (Copy handle; does not alias `self.hardware`).
        // `bringup` decides whether the profile takes over the extended Wi-Fi
        // bring-up sequence for this role/protocol.
        let profile = self.profile;
        // An HE20 ESP-NOW peer rate is refused (`InvalidArgument`) unless the interface advertises
        // 802.11ax, so `EspNowConfig::with_he20` implies the AX protocol set when none was asked
        // for. Measured on ESP32-C5: without it no frame rides HE20 and no CSI arrives at all; with
        // it both directions deliver full HE-LTF estimates.
        #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
        if self.protocol.is_none() {
            let he20 = match &self.mode {
                Mode::EspNow(c) => c.he20(),
                Mode::EspNowSimplex(c) => c.inner().he20(),
                _ => false,
            };
            if he20 {
                self.protocol = Some(Protocol::AX);
            }
        }

        let bringup = profile.wants_bringup(NodeView::new(&self.mode), self.protocol);

        // Apply protocol ladder before STA bring-up / CSI. Generic chip-level tuning
        // lives in the radio profile; specialised back-ends may rebuild the set
        // entirely. Skipped for an emitter, which pins its own protocol set during
        // bring-up to match its forced TX PHY.
        if let Some(protocol) = self.protocol.take() {
            if !is_emitter {
                let base = Protocols::default().with_2_4(protocol_ladder_2_4(protocol));
                let protocols =
                    profile.tune_protocols(NodeView::new(&self.mode), protocol, base);
                controller.set_protocols(protocols).unwrap();
                c5_radio_settle().await;
            }
            self.protocol = Some(protocol);
        }

        if bringup && !is_emitter {
            profile.apply_bandwidth(controller);
            c5_radio_settle().await;
        }

        // Tasks necessary for a station receiver.
        let sta_interface =
            if let Mode::Station(config) = &self.mode {
                let ifaces = sta_init(
                    &mut interfaces.station,
                    config,
                    controller,
                    profile,
                    bringup,
                );
                // Band selection comes *after* `sta_init`, which is what configures and
                // starts the interface: `esp_wifi_set_band_mode` requires a started
                // controller, so doing this first failed silently and left the station on
                // whatever band a previous run had selected.
                //
                // With a channel hint, pin the band it implies. Without one, select both
                // bands — pinning a single band would make an access point on the other
                // one invisible, which presents as a bare "no access point found".
                #[cfg(feature = "esp32c5")]
                {
                    match config.channel_hint {
                        Some(channel) => apply_band_for_channel(controller, channel),
                        None => apply_band_auto(controller),
                    }
                    c5_radio_settle().await;
                }
                Some(ifaces)
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
        let ap_interface = if let Mode::AccessPoint(config) = &self.mode {
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
        // meaningful for a receiver — but it is cheap and keeps the flow linear.
        let mut config = match self.setup.get_acquisition() {
            Some(config) => {
                log_ln!("CSI Configuration Set: {:?}", config);
                build_csi_config(config)
            }
            None => {
                let default_config = CsiConfig::default();
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
        // The reporting reaches the delivery gate through `set_runtime_reporting` above, not
        // through here: `CSI_OUTPUT_ENABLED` is the user's runtime override and writing the
        // configured reporting into it would make `set-csi-output --enabled=true` unable to
        // re-enable a node that had been configured to report nothing.
        // Sequence-drop detection tracks per-source-MAC sequence numbers, so it
        // works for any receiver: the emitter's driver-assigned incrementing
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

        match &self.mode {
            Mode::Emitter(emitter_config) => {
                // The emitter owns its whole bring-up (forced TX PHY, unassociated
                // interface start, channel lock) inside `run_emitter`, because the
                // forced rate has to be applied before the interface starts.
                let main_task = run_emitter(controller, interfaces, emitter_config);
                drive_main(main_task, false, duration, client).await;
            }
            Mode::Sniffer(sniffer_config) => {
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
            Mode::AccessPoint(ap_config) => {
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
                        self.setup.get_periodicity_hz(),
                    );
                    drive_main(main_task, rx_enabled, duration, client).await;
                    sniffer.set_promiscuous_mode(false).unwrap();
                }
            Mode::Station(_sta_config) => {
                    // 1. Connect to the Wi-Fi network.
                    // 2. Run DHCP / NTP sync if enabled in config.
                    // 3. Drive STA connection handling and network operations.
                    let (sta_stack, sta_runner) = sta_interface.unwrap();

                    let main_task = run_sta_connect(
                        controller,
                        self.setup.get_periodicity_hz(),
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

            // ── ESP-NOW ───────────────────────────────────────────────────────────────────────
            //
            // The four drivers all take `&mut EspNow`, and this tree already has one:
            // `interfaces.esp_now`, the same handle the emitter uses to transmit over ESP-NOW on
            // classic chips. The pre-refactor loop built its own and threaded it through twenty
            // integration points; none of that is needed here, and reproducing it would have been
            // a second bring-up path to keep in step with this one.
            //
            // Borrowing note: the `sniffer` binding above holds `&interfaces.sniffer`. These arms
            // take `&mut interfaces.esp_now`, a disjoint field, which the borrow checker accepts —
            // and they must not touch `sniffer`, which is why none of them clears promiscuous mode
            // on the way out. ESP-NOW never sets it.
            Mode::EspNow(cfg) => {
                // The symmetric exchange has two ends and the session role picks which one this
                // node drives: the initiator originates the control traffic, the responder answers
                // it. Both measure, which is why this is the one mode where every combination of
                // session role and reporting is meaningful.
                match cfg.session_role() {
                    crate::SessionRole::Initiator => {
                        // `reports` is announced to the responders in every control packet, so a
                        // responder paired with a silent initiator can promote itself. It is read
                        // from the same runtime flag the delivery path uses.
                        let main_task = run_esp_now_initiator(
                            &mut interfaces.esp_now,
                            // `run_esp_now_initiator` takes this as `_mac_addr` and does not read
                            // it — the peer MAC it actually uses comes from
                            // `EspNowConfig::peer_mac`. Passed as an explicit unset value rather
                            // than a plausible-looking address, so that if the driver ever starts
                            // reading it the result is obviously wrong rather than subtly wrong.
                            UNSET_MAC,
                            cfg,
                            self.setup.get_periodicity_hz(),
                            crate::REPORTS.load(Ordering::Relaxed),
                            self.io_tasks,
                        );
                        drive_main(main_task, rx_enabled, duration, client).await;
                    }
                    crate::SessionRole::Responder => {
                        let main_task = run_esp_now_responder(
                            &mut interfaces.esp_now,
                            cfg,
                            self.setup.get_periodicity_hz(),
                            self.io_tasks,
                        );
                        drive_main(main_task, rx_enabled, duration, client).await;
                    }
                }
            }
            Mode::EspNowSimplex(cfg) => {
                // Asymmetric simplex. The `source` end owns all the transmit airtime; the `peer`
                // end beacons until it is found and then goes receive-only. Both drivers take the
                // same `EspNowConfig`, so the end is what selects between them: the source is the
                // initiator (`initiator::esp_now_simplex`), the peer the responder
                // (`responder::esp_now_simplex`).
                if cfg.is_source() {
                    // It captures nothing, so RX is forced off regardless of `rx_enabled` — a CSI
                    // rate task here would compete for the airtime the flood exists to fill.
                    let main_task = run_esp_now_simplex_source(
                        &mut interfaces.esp_now,
                        cfg.inner(),
                        self.setup.get_periodicity_hz(),
                        self.io_tasks,
                    );
                    drive_main(main_task, false, duration, client).await;
                } else {
                    let main_task = run_esp_now_simplex_peer(
                        &mut interfaces.esp_now,
                        cfg.inner(),
                        self.io_tasks,
                    );
                    drive_main(main_task, rx_enabled, duration, client).await;
                }
            }
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
    client: Option<&mut CsiNodeClient>,
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

/// Expand a single requested 2.4 GHz protocol into the cumulative set the radio needs.
///
/// 802.11 protocol sets on 2.4 GHz are a **ladder**, not a choice: 11n is an extension of
/// 11b/11g, and the ESP32-C5/C6 rung above it extends all three, so a station must advertise
/// the rungs beneath the one it wants. Advertising a lone bit (the previous `EnumSet::only(protocol)`) produces a set
/// no real link can use.
///
/// This was measured, not theorised. With an N-only set, an ESP32-C6 `sniffer` collected
/// **zero** HT20 frames from a working emitter — reproduced on both C6 boards, in both role
/// assignments — while ESP32-S3 receivers on the same link and the same config collected
/// normally (their driver tolerates the degenerate set). The emitter never hit this because
/// `emitter::phy` builds `B | G | N` for itself; only the receiver path used `only()`.
///
/// `LR` is Espressif's proprietary long-range PHY rather than a rung on the ladder, so it
/// stays on its own. The 5 GHz-only rungs keep the previous one-bit behaviour instead of
/// being given an invented 2.4 GHz meaning — [`RadioProfile::tune_protocols`] owns the
/// 5 GHz set.
fn protocol_ladder_2_4(protocol: Protocol) -> EnumSet<Protocol> {
    match protocol {
        Protocol::B => EnumSet::only(Protocol::B),
        Protocol::G => Protocol::B | Protocol::G,
        Protocol::N => Protocol::B | Protocol::G | Protocol::N,
        // Proprietary long-range, and the 5 GHz-only rungs: not part of the 2.4 GHz ladder.
        Protocol::LR | Protocol::A | Protocol::AC => EnumSet::only(protocol),
        // The rung above N (ESP32-C5/C6 only) extends all three beneath it.
        higher => Protocol::B | Protocol::G | Protocol::N | higher,
    }
}

#[cfg(test)]
mod protocol_ladder_tests {
    use super::*;

    /// The rung under test must always be present, and every lower rung with it.
    #[test]
    fn each_rung_carries_the_ones_beneath_it() {
        assert_eq!(protocol_ladder_2_4(Protocol::B), EnumSet::only(Protocol::B));
        assert_eq!(protocol_ladder_2_4(Protocol::G), Protocol::B | Protocol::G);
        assert_eq!(
            protocol_ladder_2_4(Protocol::N),
            Protocol::B | Protocol::G | Protocol::N
        );
        // Every rung above N carries B | G | N beneath it.
        for p in EnumSet::<Protocol>::all().iter().filter(|p| {
            !matches!(
                p,
                Protocol::B | Protocol::G | Protocol::N | Protocol::LR | Protocol::A | Protocol::AC
            )
        }) {
            assert_eq!(protocol_ladder_2_4(p), Protocol::B | Protocol::G | Protocol::N | p);
        }
    }

    /// Regression for the measured failure: an N-only 2.4 GHz set made an ESP32-C6
    /// receiver capture zero HT20 frames. `N` must never be advertised alone.
    #[test]
    fn n_is_never_advertised_alone() {
        let set = protocol_ladder_2_4(Protocol::N);
        assert!(set.contains(Protocol::N));
        assert!(set.contains(Protocol::G), "11n needs 11g beneath it");
        assert!(set.contains(Protocol::B), "11n needs 11b beneath it");
        assert_ne!(set, EnumSet::only(Protocol::N));
    }

    /// The receiver ladder must match what the emitter already builds for itself in
    /// `emitter::phy` (`B | G | N`) — the two ends of an HT link have to agree, and the
    /// mismatch between them is exactly what this bug was.
    #[test]
    fn the_ht_rung_matches_the_emitters_own_set() {
        assert_eq!(
            protocol_ladder_2_4(Protocol::N),
            Protocol::B | Protocol::G | Protocol::N
        );
    }

    /// `LR` is Espressif's proprietary long-range PHY, not a rung: it must stay alone,
    /// or enabling it would silently also advertise b/g/n.
    #[test]
    fn lr_stays_on_its_own() {
        assert_eq!(protocol_ladder_2_4(Protocol::LR), EnumSet::only(Protocol::LR));
    }

    /// 5 GHz-only rungs keep the previous single-bit behaviour; the profile owns that band.
    #[test]
    fn five_ghz_rungs_are_untouched() {
        for p in [Protocol::A, Protocol::AC] {
            assert_eq!(protocol_ladder_2_4(p), EnumSet::only(p));
        }
    }
}
