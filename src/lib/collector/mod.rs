//! The Wi-Fi station and access-point engines.
//!
//! The module name is historical: "collector" is a collection mode, not a role (see
//! [`crate::model`]), and both modes here admit either collection mode —
//! [`WifiStationConfig`](crate::WifiStationConfig) and [`WifiApConfig`](crate::WifiApConfig)
//! carry `with_collection_mode`. What this module holds is the association-based way of reaching
//! the channel:
//!
//! - [`sta`] — associate to an access point or router and measure the frames it receives
//!   ([`CSINode::station`](crate::CSINode::station)).
//! - [`ap`] — run an access point (with a minimal DHCP server) so associated stations generate
//!   steady uplink traffic to measure ([`CSINode::access_point`](crate::CSINode::access_point)).
//!
//! The promiscuous sniffer, the third Wi-Fi mode, needs no engine of its own; it is driven
//! directly from the [`crate::node`] dispatch.

/// Self-contained softAP: start an access point + minimal DHCP server so Wi-Fi station nodes can
/// associate and generate CSI-bearing traffic.
pub mod ap;
/// Wi-Fi station mode: associate to an AP and process CSI from received frames.
pub mod sta;
