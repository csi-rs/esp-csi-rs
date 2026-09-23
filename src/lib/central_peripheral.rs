//! [`EspNowConfig`], the configuration of both ESP-NOW operational modes, and the retired enums
//! that used to wrap it.
//!
//! `EspNowConfig` carries what the symmetric exchange ([`OperationalMode::EspNow`]) admits —
//! channel, forced TX PHY, HT40, an explicit peer MAC, and the node's network role and collection
//! mode — and is the inner config of both ends of the asymmetric one
//! ([`OperationalMode::EspNowSimplex`], built through [`SimplexConfig`]). What those attributes
//! mean is in [`crate::model`].
//!
//! [`CentralOpMode`], [`PeripheralOpMode`] and [`Node`] are the pre-0.11 spelling of the same two
//! modes, kept so callers written against 0.10 compile with warnings. They are deprecated and are
//! removed in 0.12. That spelling also put the simplex ends on the wrong sides: the flooding end
//! was a "peripheral" and the receive-only end a "central". `From<NodeRole> for OperationalMode`
//! corrects the assignment for callers that have not migrated.
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

    /// In-place form of [`with_collection_mode`](Self::with_collection_mode). Exists for the
    /// deprecated `CSINode::set_collection_mode` shim, which mutates a config it does not own;
    /// `EspNowConfig` is deliberately not `Clone`, so the builder form cannot serve there.
    pub(crate) fn set_collection_mode(&mut self, mode: crate::CollectionMode) {
        self.collection = mode;
    }
}
/// The pre-0.11 "central" ESP-NOW modes.
///
/// **Deprecated in 0.11, removed in 0.12.** Use [`OperationalMode::EspNow`](crate::OperationalMode)
/// with [`NetworkRole::Central`](crate::NetworkRole::Central) on the config, or
/// [`SimplexConfig::peer`](crate::SimplexConfig::peer) for what was `EspNowFastCollector`. This
/// spelling put the simplex ends the wrong way round: the receive-only end is a peripheral.
#[deprecated(
    since = "0.11.0",
    note = "removed in 0.12; use `OperationalMode::EspNow` / `OperationalMode::EspNowSimplex`. \
            This spelling put the simplex ends the wrong way round: `EspNowFastCollector` is the \
            peripheral collector, `SimplexConfig::peer`"
)]
pub enum CentralOpMode {
    /// The central end of the symmetric exchange. Now `OperationalMode::EspNow` with
    /// `NetworkRole::Central`.
    EspNow(EspNowConfig),
    /// The receive-only simplex end. Now `SimplexConfig::peer`: a **peripheral** collector.
    EspNowFastCollector(EspNowConfig),
}

/// The pre-0.11 "peripheral" ESP-NOW modes.
///
/// **Deprecated in 0.11, removed in 0.12.** Use [`OperationalMode::EspNow`](crate::OperationalMode)
/// with [`NetworkRole::Peripheral`](crate::NetworkRole::Peripheral) on the config, or
/// [`SimplexConfig::source`](crate::SimplexConfig::source) for what was `EspNowFastSource`. This
/// spelling put the simplex ends the wrong way round: the flooding end is a central.
#[deprecated(
    since = "0.11.0",
    note = "removed in 0.12; use `OperationalMode::EspNow` / `OperationalMode::EspNowSimplex`. \
            This spelling put the simplex ends the wrong way round: `EspNowFastSource` is the \
            central listener, `SimplexConfig::source`"
)]
pub enum PeripheralOpMode {
    /// The peripheral end of the symmetric exchange. Now `OperationalMode::EspNow` with
    /// `NetworkRole::Peripheral`.
    EspNow(EspNowConfig),
    /// The flooding simplex end. Now `SimplexConfig::source`: a **central** listener.
    EspNowFastSource(EspNowConfig),
}

/// The pre-0.11 wrapper around [`CentralOpMode`] and [`PeripheralOpMode`]. Nothing in the crate
/// consumes it.
///
/// **Deprecated in 0.11, removed in 0.12.** Use [`OperationalMode`](crate::OperationalMode).
#[deprecated(
    since = "0.11.0",
    note = "removed in 0.12; use `OperationalMode`. This spelling put the simplex ends the wrong \
            way round — see `esp_csi_rs::model`"
)]
#[allow(deprecated)]
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
