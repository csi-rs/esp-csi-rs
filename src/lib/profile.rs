//! Radio-profile seam.
//!
//! The node orchestrator ([`crate::node::CSINode`]) drives Wi-Fi bring-up
//! through a small set of hooks so that alternative PHY back-ends can be
//! supplied out-of-tree without forking the engine. The crate ships
//! [`StandardProfile`], which performs the generic chip-level radio tuning and,
//! on the ESP32-C5 / C6, the 802.11ax HE20 bring-up a node asks for with
//! [`CSINode::set_protocol`](crate::CSINode::set_protocol)`(Protocol::AX)`;
//! specialised profiles override the hooks they need.

use crate::model::NodeView;
#[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
use crate::model::OperationalMode;
use esp_radio::wifi::csi::CsiConfig as RadioCsiConfig;
use esp_radio::wifi::{Protocol, Protocols, WifiController};

/// Pluggable Wi-Fi bring-up back-end.
///
/// Every method has a default so profiles only override what they need and new
/// hooks can be added without breaking existing implementors. Object-safe: the
/// node stores it as `&'static dyn RadioProfile`.
pub trait RadioProfile: Sync {
    /// Whether this profile takes over the extended bring-up sequence
    /// (bandwidth lock / pre-config forcing / post-config re-apply) for the
    /// given node and requested protocol. `false` keeps the plain path.
    fn wants_bringup(&self, _node: NodeView<'_>, _protocol: Option<Protocol>) -> bool {
        false
    }

    /// Adjust the protocol set before it is applied. `base` is
    /// `Protocols::default()` with its 2.4 GHz set replaced by the cumulative ladder for
    /// `protocol` — `N` becomes `B | G | N`, `G` becomes `B | G`, and so on; `LR` and the
    /// 5 GHz-only protocols stay single. Return it unchanged to keep the default, or rebuild it
    /// entirely (a profile owns the 5 GHz set).
    fn tune_protocols(
        &self,
        _node: NodeView<'_>,
        _protocol: Protocol,
        base: Protocols,
    ) -> Protocols {
        base
    }

    /// Lock/adjust bandwidth just before station bring-up (only called when
    /// [`Self::wants_bringup`] is `true`).
    fn apply_bandwidth(&self, _controller: &mut WifiController<'_>) {}

    /// Fired inside `sta_init`, immediately before `set_config(Station)`.
    fn before_sta_config(&self) {}

    /// Fired inside `ap_init`, immediately before `set_config(AccessPoint)`.
    fn before_ap_config(&self) {}

    /// Re-apply protocols after a `set_config` restart (station and AP paths).
    fn apply_protocols_post(&self, _controller: &mut WifiController<'_>) {}

    /// Radio setup for the promiscuous sniffer path after the channel lock.
    fn apply_sniffer_radio(&self, _controller: &mut WifiController<'_>) {}

    /// Mutate the raw esp-radio CSI config before it is applied, e.g. to enable
    /// additional acquisition modes. Default leaves it untouched.
    fn tune_csi_acquisition(&self, _raw: &mut RadioCsiConfig) {}
}

/// Default profile: generic chip-level radio tuning, plus 802.11ax HE20 bring-up on the parts
/// with an HE PHY.
///
/// **HE20 (ESP32-C5 / C6).** A station, access point or sniffer whose node was given
/// `Protocol::AX` gets the extended bring-up: the interface is locked to 20 MHz, its TX PHY is
/// forced to HE20 before `set_config` starts it, and the protocol set — which `set_config`
/// overwrites with its B|G|N default — is re-applied with AX afterwards. Ask for HE-LTF
/// acquisition with [`CsiConfig::he20`](crate::config::CsiConfig) to receive the full HE
/// estimate rather than the legacy L-LTF one. An HE20 *emitter* forces its PHY itself
/// ([`EmitterPhy::He20`](crate::EmitterPhy)), and the ESP-NOW modes force theirs per peer
/// ([`EspNowConfig::with_he20`](crate::EspNowConfig)), so neither goes through this path.
pub struct StandardProfile;

#[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
mod he20 {
    use esp_radio::wifi::{Bandwidth, Protocol, Protocols, WifiController};

    use crate::emitter::phy::{WIFI_PHY_MODE_HE20, force_tx_ap, force_tx_sta_before_start};

    /// The protocol set an HE20 station or AP advertises: N|AX on 2.4 GHz (not AX alone —
    /// beacons and association need HT), and A|N|AX on 5 GHz on the dual-band C5.
    pub(super) fn protocols() -> Protocols {
        let p = Protocols::default().with_2_4(Protocol::N | Protocol::AX);
        #[cfg(feature = "esp32c5")]
        {
            return p.with_5(Protocol::A | Protocol::N | Protocol::AX);
        }
        #[cfg(not(feature = "esp32c5"))]
        p
    }

    /// The protocol set re-applied after `set_config`: B|G|N|AX (A|N|AX on 5 GHz).
    pub(super) fn protocols_post() -> Protocols {
        let p = Protocols::default().with_2_4(Protocol::B | Protocol::G | Protocol::N | Protocol::AX);
        #[cfg(feature = "esp32c5")]
        {
            return p.with_5(Protocol::A | Protocol::N | Protocol::AX);
        }
        #[cfg(not(feature = "esp32c5"))]
        p
    }

    pub(super) fn lock_20mhz(controller: &mut WifiController<'_>) {
        if let Ok(bw) = controller.bandwidths() {
            let _ = controller.set_bandwidths(bw.with_2_4(Bandwidth::_20MHz));
        }
    }

    pub(super) fn force_sta() {
        let _ = force_tx_sta_before_start(WIFI_PHY_MODE_HE20);
    }

    pub(super) fn force_ap() {
        let _ = force_tx_ap(WIFI_PHY_MODE_HE20);
    }
}

impl RadioProfile for StandardProfile {
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    fn wants_bringup(&self, node: NodeView<'_>, protocol: Option<Protocol>) -> bool {
        protocol == Some(Protocol::AX)
            && match node.mode() {
                // HT40 softAP is out: HE20 locks 20 MHz.
                OperationalMode::AccessPoint(c) => c.secondary_channel().is_none(),
                OperationalMode::Station(_) | OperationalMode::Sniffer(_) => true,
                // The emitter and the ESP-NOW modes force their own PHY; taking over their
                // bring-up would undo it.
                OperationalMode::Emitter(_)
                | OperationalMode::EspNow(_)
                | OperationalMode::EspNowSimplex(_) => false,
            }
    }

    fn tune_protocols(
        &self,
        node: NodeView<'_>,
        _protocol: Protocol,
        base: Protocols,
    ) -> Protocols {
        #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
        if _protocol == Protocol::AX
            && matches!(
                node.mode(),
                OperationalMode::Station(_) | OperationalMode::AccessPoint(_)
            )
        {
            return he20::protocols();
        }
        // Generic, chip-level tuning shared by every deployment.
        #[cfg(feature = "esp32c5")]
        {
            // The plain associated AP/STA paths advertise A/N on 5 GHz. A sniffer
            // is excluded: it locks a channel rather than associating, and an
            // emitter pins its own protocol set to match its forced TX PHY.
            if matches!(
                node.mode(),
                OperationalMode::Station(_) | OperationalMode::AccessPoint(_)
            ) {
                return base.with_5(Protocol::A | Protocol::N);
            }
        }
        let _ = node;
        base
    }

    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    fn apply_bandwidth(&self, controller: &mut WifiController<'_>) {
        he20::lock_20mhz(controller);
    }

    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    fn before_sta_config(&self) {
        he20::force_sta();
    }

    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    fn before_ap_config(&self) {
        he20::force_ap();
    }

    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    fn apply_protocols_post(&self, controller: &mut WifiController<'_>) {
        let _ = controller.set_protocols(he20::protocols_post());
    }

    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    fn apply_sniffer_radio(&self, controller: &mut WifiController<'_>) {
        he20::lock_20mhz(controller);
        let _ = controller.set_protocols(he20::protocols_post());
    }
}
