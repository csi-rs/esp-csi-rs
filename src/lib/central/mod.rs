//! Drivers for the central end of the ESP-NOW modes, plus the Wi-Fi collection modules.
//!
//! A *central* originates the network's traffic (see [`crate::model`]). This module holds the
//! central end of the symmetric ESP-NOW exchange ([`esp_now`]), and re-exports the station
//! ([`sta`]) and access-point ([`ap`]) engines from [`crate::collector`] so their pre-0.11 paths
//! keep resolving.
//!
//! [`esp_now_fast`] is here for historical reasons only: it drives the receive-only **peer** end
//! of the simplex exchange, which is a peripheral collector. It sits under the module name it had
//! when the simplex ends were assigned the other way round.

// `ap` and `sta` are RE-EXPORTED from `collector`, not duplicated here, so there is one copy of
// the softAP and station code to keep in step. `central` is a compatibility facade over them plus
// the ESP-NOW drivers, which have no counterpart under `collector`.
pub use crate::collector::{ap, sta};

/// ESP-NOW central driver: originates the control traffic of the symmetric exchange and measures
/// the peripherals' replies.
pub mod esp_now;
/// ESP-NOW simplex **peer** driver (a peripheral collector, despite the module's location): sparse
/// discovery beacon, then receive-only capture of the source's continuous unicast flood.
pub mod esp_now_fast;
/// Empty. Kept only so the pre-0.11 path still resolves; a sniffer is always a peripheral and is
/// driven from [`crate::CSINode::run`].
#[doc(hidden)]
pub mod sniffer;
