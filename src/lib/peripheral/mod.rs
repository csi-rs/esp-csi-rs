//! Drivers for the peripheral end of the ESP-NOW modes.
//!
//! A *peripheral* sources no traffic of its own (see [`crate::model`]). This module holds the
//! peripheral end of the symmetric ESP-NOW exchange ([`esp_now`]), which answers a central's
//! control frames. The sniffer, the other peripheral mode, needs no driver module: it is run
//! directly from [`crate::CSINode::run`] via the Wi-Fi sniffer API.
//!
//! [`esp_now_fast`] is here for historical reasons only: it drives the flooding **source** end of
//! the simplex exchange, which is a central listener. It sits under the module name it had when
//! the simplex ends were assigned the other way round.

/// ESP-NOW peripheral driver: receives control packets from the central and sends timestamped
/// replies for latency telemetry.
pub mod esp_now;
/// ESP-NOW simplex **source** driver (a central listener, despite the module's location):
/// discovers the peer, then unicasts a continuous forced-PHY flood for it to capture.
pub mod esp_now_fast;
