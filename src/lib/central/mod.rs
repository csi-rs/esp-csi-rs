//! Central-node operating modes.
//!
//! A *central* node is the active driver of CSI collection. It either
//! orchestrates an ESP-NOW exchange with a peripheral
//! ([`esp_now`](self::esp_now)) or associates as a Wi-Fi station
//! ([`sta`](self::sta)) to extract CSI from regular 802.11 traffic. The
//! [`sniffer`](self::sniffer) module is a placeholder for future
//! central-side sniffer logic.

/// Central-side ESP-NOW driver: latency-balanced control/reply exchange
/// with a peripheral that supplies the CSI source frames.
pub mod esp_now;
/// Reserved for future central-side promiscuous sniffer logic. Currently empty.
pub mod sniffer;
/// Wi-Fi station mode: associate to an AP and process CSI from received frames.
pub mod sta;
