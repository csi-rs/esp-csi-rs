//! The **collector** role: nodes that capture CSI and deliver it.
//!
//! A collector exists to measure the channel's response and hand the result to a
//! consumer. How it gets frames to measure is a separate question from what it is
//! for, which is why the capture paths are variants of one role rather than
//! separate roles:
//!
//! - [`sta`] — associate to an access point and measure the frames it receives.
//! - [`ap`] — run an access point (with a minimal DHCP server) so an associated
//!   station generates steady uplink traffic to measure.
//! - promiscuous sniffer — lock a channel and measure every frame overheard,
//!   including the raw frames from an [`crate::emitter`]. The sniffer needs no
//!   engine of its own; it is driven directly from the [`crate::node`] dispatch.

/// Self-contained softAP CSI collector: start an access point + minimal DHCP
/// server so a Wi-Fi station node can associate and generate CSI-bearing traffic.
pub mod ap;
/// Wi-Fi station mode: associate to an AP and process CSI from received frames.
pub mod sta;
