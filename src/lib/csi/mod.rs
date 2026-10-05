//! CSI packets: what a node delivers for every channel measurement.
//!
//! A [`CsiPacket`] is the wire contract's [`Envelope`] and [`CsiFrame`] (see [`crate::wire`]),
//! plus the session announcement on the first packet of a run. The same value reaches every
//! consumer: a callback registered with [`set_csi_callback`](crate::set_csi_callback), the async
//! [`CSINodeClient`](crate::CSINodeClient), and the logger, which encodes it for the host.
//!
//! Before 0.12 the packet was `CSIDataPacket`, a field-for-field copy of ESP's receive-control
//! struct with one layout per chip family. Its fields now live, normalised, in [`RxMeta`]; the
//! chip-specific remainder is [`VendorRx`](crate::wire::VendorRx), and the CSI buffer is a tagged
//! [`CsiPayload`] that records its own subcarrier layout.

use crate::wire::{CsiFrame, CsiPayload, Envelope, RxMeta, SessionInfo};

/// CSI delivery state machine (callbacks, async queue, inline logging).
pub mod delivery;
pub(crate) mod esp;
pub mod session;
pub(crate) mod policy;
pub mod source;
pub(crate) mod text_row;

/// One delivered channel measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct CsiPacket {
    /// Who delivered it, in which session, and its place in the stream.
    pub envelope: Envelope,
    /// The measurement.
    pub frame: CsiFrame,
    /// The session announcement. Present on the first packet of each run only.
    pub session: Option<SessionInfo>,
}

/// The pre-0.12 name. The type is new: its fields are those of [`CsiPacket`].
#[deprecated(since = "0.12.0", note = "renamed to `CsiPacket`; see the 0.12 CHANGELOG for the field map")]
pub type CSIDataPacket = CsiPacket;

impl CsiPacket {
    /// Build a packet from a raw esp-radio CSI callback argument, stamped with this node's session
    /// context and the next stream sequence number.
    ///
    /// The single source of truth for the `WifiCsiInfo` → packet mapping, shared by the crate's
    /// own CSI callback and any out-of-tree node that owns the radio directly, so both emit
    /// identical frames. `None` if the buffer exceeds [`crate::wire::MAX_CSI_BYTES`].
    pub fn from_wifi_csi_info(info: &esp_radio::wifi::csi::WifiCsiInfo<'_>) -> Option<Self> {
        let frame = esp::frame_from_esp(info)?;
        Some(Self::new(frame, crate::wire::SourceKind::EspVendor))
    }

    /// Wrap a frame from any source in this node's envelope.
    pub(crate) fn new(frame: CsiFrame, source: crate::wire::SourceKind) -> Self {
        let session = session::take_announcement(frame.meta.timestamp_us);
        Self {
            envelope: session::next_envelope(source),
            frame,
            session,
        }
    }

    /// Normalised receive metadata.
    pub fn meta(&self) -> &RxMeta {
        &self.frame.meta
    }

    /// Transmitter of the measured PPDU.
    pub fn mac(&self) -> [u8; 6] {
        self.frame.transmitter().unwrap_or([0; 6])
    }

    /// Received signal strength, dBm.
    pub fn rssi(&self) -> i8 {
        self.frame.meta.rssi
    }

    /// Receive time, microseconds on the node's clock. Does not wrap.
    pub fn timestamp_us(&self) -> u64 {
        self.frame.meta.timestamp_us
    }

    /// The raw CSI buffer, for an Espressif payload: interleaved `(imag, real)` pairs. Empty for
    /// any other payload kind.
    pub fn csi_data(&self) -> &[i8] {
        match &self.frame.payload {
            CsiPayload::EspRaw { bytes, .. } => bytes,
            _ => &[],
        }
    }

    /// Subcarriers in the raw buffer (`csi_data().len() / 2`).
    pub fn subcarriers(&self) -> usize {
        self.csi_data().len() / 2
    }

    /// Emit this packet through the crate's logging backend in the configured `LogMode`.
    pub fn print_csi_w_metadata(self) {
        crate::logging::logging::log_csi(self);
    }
}
