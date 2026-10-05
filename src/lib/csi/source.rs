//! Measurement sources: anything that produces channel measurements for this node to deliver.
//!
//! Delivery, statistics and encoding consume [`CsiFrame`]s, not a radio's callback struct. The
//! Espressif vendor CSI callback is one source ([`EspVendorSource`]); a synthetic generator, an
//! out-of-tree driver, or — once a radio exposes one — an IEEE 802.11bf sensing procedure is
//! another. Each hands its frames to [`emit`], which applies the same publish gate, reporting
//! policy, envelope, per-transmitter statistics and delivery path as the built-in callback, so a
//! host cannot tell a source's frames from the callback's except by [`SourceKind`].

use super::CsiPacket;
use crate::wire::{CsiFrame, SourceKind};

/// A producer of channel measurements.
///
/// Object-safe, so a node can hold sources of different kinds side by side.
pub trait MeasurementSource {
    /// What kind of source this is. Stamped on every frame's envelope.
    fn kind(&self) -> SourceKind;

    /// A short name for diagnostics.
    fn name(&self) -> &'static str {
        match self.kind() {
            SourceKind::EspVendor => "esp-vendor",
            SourceKind::Ieee80211bf => "ieee80211bf",
            SourceKind::Synthetic => "synthetic",
        }
    }
}

/// The Espressif vendor CSI callback: the source every built-in operational mode uses.
///
/// Its frames do not pass through [`emit`] — the callback runs the same pipeline inline, with its
/// attribution filter and policy judged on the raw buffer before a packet is built — but they are
/// stamped with this source's kind.
pub struct EspVendorSource;

impl MeasurementSource for EspVendorSource {
    fn kind(&self) -> SourceKind {
        SourceKind::EspVendor
    }
}

/// Deliver a measurement from `source` through this node's pipeline.
///
/// Returns whether it was delivered: `false` when no consumer is installed, the node reports
/// `Never` or output is switched off, or the reporting policy withheld it. A delivered frame gets
/// this node's envelope (node id, session id, the next stream sequence number) and goes to exactly
/// one of the callback, the async queue or the logger, like any captured frame.
///
/// Callable from any task. It must not race the vendor CSI callback's own reporting-policy state, so
/// do not emit from a second source while a [`Threshold`](crate::ReportingPolicy::Threshold) or
/// [`Decimate`](crate::ReportingPolicy::Decimate) policy is in force on a running ESP mode.
pub fn emit(source: &dyn MeasurementSource, mut frame: CsiFrame) -> bool {
    if !super::delivery::publish_open() {
        return false;
    }
    if !super::delivery::policy_admits_frame(&mut frame) {
        return false;
    }
    #[cfg(feature = "statistics")]
    crate::stats::record_collector_rx();
    super::delivery::deliver(CsiPacket::new(frame, source.kind()));
    true
}

#[cfg(feature = "synthetic")]
impl<const R: usize> MeasurementSource for crate::wire::synthetic::SyntheticBf<R> {
    fn kind(&self) -> SourceKind {
        SourceKind::Synthetic
    }
}
