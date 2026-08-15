//! Peripheral-node operating modes.
//!
//! A *peripheral* node is the responder in a CSI session. It either replies
//! to a central's ESP-NOW control frames ([`esp_now`](self::esp_now)) or
//! runs in promiscuous sniffer mode (handled directly in
//! [`crate::run_node`] via the Wi-Fi sniffer API rather than as a submodule
//! here).

/// Peripheral-side ESP-NOW driver: receives control packets from the
/// central and sends timestamped replies for latency telemetry.
pub mod esp_now;
/// Fast one-to-one ESP-NOW source (asymmetric simplex): discovers the collector,
/// then unicasts a continuous forced-PHY flood for CSI capture.
pub mod esp_now_fast;
