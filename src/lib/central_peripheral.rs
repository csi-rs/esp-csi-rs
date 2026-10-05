//! [`EspNowConfig`], the configuration of both ESP-NOW operational modes.
//!
//! `EspNowConfig` carries what the symmetric exchange ([`OperationalMode::EspNow`]) admits —
//! channel, forced TX PHY (HT20, HT40 or HE20), an explicit peer MAC, and the node's network role
//! and reporting policy — and is the inner config of both ends of the asymmetric one
//! ([`OperationalMode::EspNowSimplex`], built through [`SimplexConfig`]). What those attributes
//! mean is in [`crate::model`].
//!
//! The pre-0.11 `CentralOpMode`, `PeripheralOpMode` and `Node` enums that used to wrap this config
//! were removed in 0.12.
//!
//! Station and access-point collection are not here: they live in [`crate::collector`], which
//! [`crate::central`] re-exports rather than duplicates.
//!
//! [`OperationalMode::EspNow`]: crate::OperationalMode::EspNow
//! [`OperationalMode::EspNowSimplex`]: crate::OperationalMode::EspNowSimplex
//! [`SimplexConfig`]: crate::SimplexConfig

// `WifiPhyRate` moved from `wifi` to `esp_now` in esp-radio 0.18 — it only ever described the
// ESP-NOW peer rate, so the move is where it belongs.
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::SecondaryChannel;

/// Configuration for the ESP-NOW operational modes.
///
/// Both ends of the symmetric exchange take one, and it is where their network role and collection
/// mode are set; the simplex source end takes one through
/// [`SimplexConfig::source`](crate::SimplexConfig::source). Construct with
/// `EspNowConfig::default()` then chain `with_channel` / `with_phy_rate` to override defaults —
/// both nodes must agree on the channel for ESP-NOW frames to be received.
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
    /// Send forced MCS rates as HE20 instead of HT20. Set by `with_he20`.
    he20: bool,
    /// Which end of the exchange this node is. ESP-NOW is the one mode that admits both, because
    /// the exchange is symmetric: the central originates the control traffic and the peripheral
    /// answers it, and either end can measure.
    network_role: crate::NetworkRole,
    /// Whether, and how often, this node reports the CSI it captures. A central announces whether
    /// it reports at all on the wire, so a peripheral paired with a silent central can promote
    /// itself.
    reporting: crate::ReportingPolicy,
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
            he20: false,
            network_role: crate::NetworkRole::Central,
            reporting: crate::ReportingPolicy::Always,
        }
    }
}

impl EspNowConfig {
    /// Recommended base config for the source end of the asymmetric ESP-NOW simplex exchange:
    /// forces HT20 at MCS7 Long-GI for maximum CSI packets/sec at the peer. Chain `with_channel` /
    /// `with_ht40` to override. Pass it to [`SimplexConfig::source`](crate::SimplexConfig::source)
    /// or [`CSINode::esp_now_simplex_source`](crate::CSINode::esp_now_simplex_source); the peer end
    /// takes only a channel.
    pub fn fast_default() -> Self {
        Self::default().with_phy_rate(WifiPhyRate::RateMcs7Lgi)
    }

    /// Override the channel. Both nodes must be configured with the same one.
    ///
    /// The ESP-NOW modes are built and measured on 2.4 GHz (`1`–`14`). Unlike the sniffer,
    /// station and access-point modes they do not select the band themselves on the dual-band
    /// ESP32-C5, so a 5 GHz primary channel here is untested.
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

    /// Configured channel.
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

    /// Send the forced MCS rate as HE20 (802.11ax SU, 20 MHz) instead of HT20, using the configured
    /// [`with_phy_rate`] (default `RateMcs0Lgi`). Implies `force_phy` and overrides
    /// [`with_ht40`]: HE20 is a 20 MHz PHY. Only the ESP32-C5 and ESP32-C6 have an 802.11ax PHY.
    ///
    /// [`with_phy_rate`]: EspNowConfig::with_phy_rate
    /// [`with_ht40`]: EspNowConfig::with_ht40
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub fn with_he20(mut self) -> Self {
        self.he20 = true;
        self.secondary_channel = None;
        self.force_phy = true;
        self
    }

    /// Whether forced MCS rates go out as HE20.
    pub fn he20(&self) -> bool {
        self.he20
    }

    /// The PHY this node forces on its ESP-NOW peers.
    pub fn peer_phy(&self) -> crate::espnow_phy::PeerPhy {
        crate::espnow_phy::PeerPhy {
            rate: self.phy_rate,
            secondary: self.secondary_channel,
            he20: self.he20,
        }
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

    /// Set whether, and how often, this node reports its CSI. Defaults to
    /// [`Always`](crate::ReportingPolicy::Always).
    pub fn with_reporting(mut self, policy: crate::ReportingPolicy) -> Self {
        self.reporting = policy;
        self
    }

    /// The pre-0.12 name of [`with_reporting`](Self::with_reporting).
    #[deprecated(since = "0.12.0", note = "renamed to `with_reporting`")]
    pub fn with_collection_mode(self, policy: crate::ReportingPolicy) -> Self {
        self.with_reporting(policy)
    }

    /// Which end of the exchange this node is.
    pub fn network_role(&self) -> crate::NetworkRole {
        self.network_role
    }

    /// Whether, and how often, this node reports the CSI it captures.
    pub fn reporting(&self) -> crate::ReportingPolicy {
        self.reporting
    }

    /// The pre-0.12 name of [`reporting`](Self::reporting).
    #[deprecated(since = "0.12.0", note = "renamed to `reporting`")]
    pub fn collection_mode(&self) -> crate::ReportingPolicy {
        self.reporting
    }
}
// `ReportingPolicy` lives in `crate::model`, beside the other attributes of the node model instead of
// beside the ESP-NOW transport that happens to put it on the wire.
pub use crate::model::ReportingPolicy;
