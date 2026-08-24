//! Radio-profile seam.
//!
//! The node orchestrator ([`crate::node::CSINode`]) drives Wi-Fi bring-up
//! through a small set of hooks so that alternative PHY back-ends can be
//! supplied out-of-tree without forking the engine. The crate ships a no-op
//! [`StandardProfile`] that performs only the generic, chip-level radio tuning;
//! specialised profiles override the hooks they need.

use crate::node::NodeRole;
#[cfg(feature = "esp32c5")]
use crate::node::CollectorMode;
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
    /// given role and requested protocol. `false` keeps the plain path.
    fn wants_bringup(&self, _role: &NodeRole, _protocol: Option<Protocol>) -> bool {
        false
    }

    /// Adjust the protocol set before it is applied. `base` is
    /// `Protocols::default().with_2_4(only(protocol))`; return it unchanged to
    /// keep the default, or rebuild it entirely.
    fn tune_protocols(&self, _role: &NodeRole, _protocol: Protocol, base: Protocols) -> Protocols {
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

/// Default profile: generic chip-level radio tuning only, no extended bring-up.
pub struct StandardProfile;

impl RadioProfile for StandardProfile {
    fn tune_protocols(&self, role: &NodeRole, _protocol: Protocol, base: Protocols) -> Protocols {
        // Generic, chip-level tuning shared by every deployment. Kept free of
        // any 5 GHz high-throughput advertising the plain path does not need.
        #[cfg(feature = "esp32c5")]
        {
            // The plain associated AP/STA paths advertise A/N on 5 GHz. A sniffer
            // collector is excluded: it locks a channel rather than associating, and
            // an emitter pins its own protocol set to match its forced TX PHY.
            if matches!(
                role,
                NodeRole::Collector(CollectorMode::Station(_))
                    | NodeRole::Collector(CollectorMode::AccessPoint(_))
            ) {
                return base.with_5(Protocol::A | Protocol::N);
            }
        }
        let _ = role;
        base
    }
}
