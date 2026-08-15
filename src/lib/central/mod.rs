//! Central-node operating modes.
//!
//! A *central* node is the active driver of CSI collection. It either
//! orchestrates an ESP-NOW exchange with a peripheral
//! ([`esp_now`](self::esp_now)) or associates as a Wi-Fi station
//! ([`sta`](self::sta)) to extract CSI from regular 802.11 traffic. The
//! [`sniffer`](self::sniffer) module is a placeholder for future
//! central-side sniffer logic.

// `ap` and `sta` are RE-EXPORTED from `collector`, not duplicated here.
//
// The emitter/collector refactor moved both modules wholesale and edited them on the way. Restoring
// the pre-refactor copies alongside would leave two versions of the same softAP and station code to
// keep in step, and they would drift — so `central` is a compatibility facade over the live ones
// plus the ESP-NOW drivers, which have no counterpart under `collector`.
pub use crate::collector::{ap, sta};

/// Central-side ESP-NOW driver: latency-balanced control/reply exchange
/// with a peripheral that supplies the CSI source frames.
pub mod esp_now;
/// Fast one-to-one ESP-NOW collector (asymmetric simplex): sparse discovery
/// beacon, then RX-only capture of a source's continuous unicast flood.
pub mod esp_now_fast;
/// Reserved for future central-side promiscuous sniffer logic. Currently empty.
pub mod sniffer;
