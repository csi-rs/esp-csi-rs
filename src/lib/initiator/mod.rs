//! Drivers for the **initiator** end of the ESP-NOW modes.
//!
//! An initiator originates the soundings that a session measures (see [`crate::model`]). This
//! module holds the initiator end of both ESP-NOW exchanges:
//!
//! - [`esp_now`] — the symmetric exchange: transmits control packets on a balanced TX/RX schedule
//!   and measures the responders' replies.
//! - [`esp_now_simplex`] — the simplex **source**: discovers the peer, then unicasts a continuous
//!   forced-PHY flood for it to measure, and measures nothing itself.

/// ESP-NOW initiator driver: originates the control traffic of the symmetric exchange and measures
/// the responders' replies.
pub mod esp_now;
/// ESP-NOW simplex source driver: discovers the peer, then unicasts a continuous forced-PHY flood.
pub mod esp_now_simplex;
