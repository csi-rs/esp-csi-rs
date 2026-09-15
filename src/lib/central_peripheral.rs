//! The **central / peripheral** node taxonomy, and the ESP-NOW configuration it carries.
//!
//! This vocabulary predates [`NodeRole`](crate::NodeRole) and was removed in "Replace ESP-NOW
//! central/peripheral with emitter/collector roles". It is restored here, **alongside** the newer
//! roles rather than in place of them, because the two describe different things and both are in
//! use:
//!
//! * [`NodeRole`](crate::NodeRole) says what a node is *for* — put energy in the channel, or measure
//!   it. That is the right axis for a raw-injection capture, where the transmitter is unassociated
//!   and there is no exchange at all.
//! * [`Node`] describes a *paired ESP-NOW exchange*, where one side drives and the other responds.
//!   The emitter/collector split cannot express it: an ESP-NOW central both transmits control frames
//!   and measures the replies, so it is neither a pure emitter nor a pure collector.
//!
//! The naming is admittedly backwards — the "central" is the receiver that aggregates CSI while its
//! "peripherals" transmit — and that was one of the reasons for the original removal. It is kept
//! as-is because it is the on-the-wire and on-the-CLI contract; renaming it would break every
//! configuration that uses it without making the exchange any easier to describe.
//!
//! `ap` and `sta` collection are NOT duplicated here: [`crate::central`] re-exports the live
//! [`crate::collector`] modules, so there is one copy of that code.

// `WifiPhyRate` moved from `wifi` to `esp_now` in esp-radio 0.18 — it only ever described the
// ESP-NOW peer rate, so the move is where it belongs.
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::SecondaryChannel;



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
    /// Which end of the exchange this node is. ESP-NOW is the one mode that admits both, because
    /// the exchange is symmetric: the central originates the control traffic and the peripheral
    /// answers it, and either end can measure.
    network_role: crate::NetworkRole,
    /// Whether this node reports the CSI it captures. A central announces its value on the wire so
    /// a peripheral paired with a listening central can promote itself.
    collection: crate::CollectionMode,
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
            network_role: crate::NetworkRole::Central,
            collection: crate::CollectionMode::Collector,
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

    /// Set which end of the exchange this node is. Defaults to
    /// [`Central`](crate::NetworkRole::Central).
    pub fn with_network_role(mut self, role: crate::NetworkRole) -> Self {
        self.network_role = role;
        self
    }

    /// Set whether this node reports its CSI. Defaults to
    /// [`Collector`](crate::CollectionMode::Collector).
    pub fn with_collection_mode(mut self, mode: crate::CollectionMode) -> Self {
        self.collection = mode;
        self
    }

    /// Which end of the exchange this node is.
    pub fn network_role(&self) -> crate::NetworkRole {
        self.network_role
    }

    /// Whether this node reports the CSI it captures.
    pub fn collection_mode(&self) -> crate::CollectionMode {
        self.collection
    }
}
/// Central node operational modes.
pub enum CentralOpMode {
    /// Drive an ESP-NOW exchange with a peripheral node.
    EspNow(EspNowConfig),
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
// `CollectionMode` moved to `crate::model`, where it sits beside the other three attributes of the
// node model instead of beside the ESP-NOW transport that happens to put it on the wire. It is
// re-exported from the crate root, so `esp_csi_rs::CollectionMode` is unchanged for callers.
pub use crate::model::CollectionMode;
