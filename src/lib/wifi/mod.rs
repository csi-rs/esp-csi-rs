//! The Wi-Fi station and access-point engines: the association-based way of reaching the channel.
//!
//! - [`sta`] — associate to an access point or router and measure the frames it receives
//!   ([`CsiNode::station`](crate::CsiNode::station)).
//! - [`ap`] — run an access point (with a minimal DHCP server) so associated stations generate
//!   steady uplink traffic to measure ([`CsiNode::access_point`](crate::CsiNode::access_point)).
//!
//! Both admit any [`Reporting`](crate::Reporting). The promiscuous sniffer, the third Wi-Fi mode,
//! needs no engine of its own; it is driven directly from the [`crate::node`] dispatch.

/// Self-contained softAP: start an access point + minimal DHCP server so Wi-Fi station nodes can
/// associate and generate CSI-bearing traffic.
pub mod ap;
/// Wi-Fi station mode: associate to an AP and process CSI from received frames.
pub mod sta;
