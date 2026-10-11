//! Drivers for the **responder** end of the ESP-NOW modes.
//!
//! A responder takes part in soundings an initiator originates (see [`crate::model`]). This module
//! holds the responder end of both ESP-NOW exchanges:
//!
//! - [`esp_now`] — the symmetric exchange: answers the initiator's control frames with presence
//!   beacons, and measures them.
//! - [`esp_now_simplex`] — the simplex **peer**: beacons sparsely until the source finds it, then
//!   goes receive-only and measures the source's flood.

/// ESP-NOW responder driver: receives control packets from the initiator and sends presence
/// replies.
pub mod esp_now;
/// ESP-NOW simplex peer driver: sparse discovery beacon, then receive-only capture of the source's
/// continuous unicast flood.
pub mod esp_now_simplex;
