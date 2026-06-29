//! Central-node operating modes.
//!
//! A *central* node is the active driver of CSI collection. It either
//! orchestrates an ESP-NOW exchange with a peripheral
//! ([`esp_now`](self::esp_now)) or associates as a Wi-Fi station
//! ([`sta`](self::sta)) to extract CSI from regular 802.11 traffic. The
//! [`sniffer`](self::sniffer) module is a placeholder for future
//! central-side sniffer logic.

/// Self-contained softAP CSI collector: start an access point + minimal DHCP
/// server so a Wi-Fi station node can associate and generate CSI-bearing traffic.
pub mod ap;
/// Central-side ESP-NOW driver: latency-balanced control/reply exchange
/// with a peripheral that supplies the CSI source frames.
pub mod esp_now;
/// Fast one-to-one ESP-NOW collector (asymmetric simplex): sparse discovery
/// beacon, then RX-only capture of a source's continuous unicast flood.
pub mod esp_now_fast;
/// Reserved for future central-side promiscuous sniffer logic. Currently empty.
pub mod sniffer;
/// Wi-Fi station mode: associate to an AP and process CSI from received frames.
pub mod sta;
