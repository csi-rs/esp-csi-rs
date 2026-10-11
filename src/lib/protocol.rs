//! ESP-NOW wire-format packets and the magic-prefix framing helpers.
//!
//! The initiator transmits [`ControlPacket`]s and the responder replies with
//! [`ResponderPacket`] presence beacons. In auto-pairing mode each frame is
//! prefixed with a 4-byte little-endian magic (see `serialize_with_magic` /
//! `parse_with_magic`); in manual-pairing mode no magic is sent and the
//! source-MAC filter is the discriminator.

use serde::{Deserialize, Serialize};

// The values are part of the over-the-air protocol and are unchanged since they were introduced;
// only the names moved to the IEEE 802.11bf roles in 0.13.
pub(crate) static INITIATOR_MAGIC_NUMBER: u32 = 0xA8912BF0;
pub(crate) static RESPONDER_MAGIC_NUMBER: u32 = !INITIATOR_MAGIC_NUMBER;

/// Control packet sent from the ESP-NOW initiator to its responders.
///
/// In auto-pairing mode the serialized frame is prefixed with a 4-byte
/// little-endian magic (see `serialize_with_magic` / `parse_with_magic`);
/// in manual-pairing mode no magic is sent and the source-MAC filter is the
/// discriminator. Both nodes must agree on the pairing mode for frames to parse.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ControlPacket {
    /// Whether the initiator's reporting in force reports anything at all (anything but
    /// [`Reporting::None`](crate::Reporting::None)). A responder that hears an initiator reporting
    /// nothing promotes itself to `Report { to: Host }` so the pair still produces a dataset; a
    /// reporting initiator leaves the responder's own reporting alone.
    ///
    /// Before 0.13 this field was `is_collector`. Its position and type are unchanged, so the
    /// postcard encoding on the air is identical and 0.12 and 0.13 nodes interoperate.
    pub initiator_reports: bool,
    /// Monotonic sounding counter. It numbers the initiator's soundings, which is what a measurement
    /// frame's `Stimulus::Controlled { instance_id }` is taken from, and lets a responder detect
    /// dropped or reordered control frames.
    ///
    /// Before 0.12 the field existed only under the `statistics` feature, so two nodes built with
    /// different feature sets could not parse each other's frames.
    pub sequence_number: u32,
}

impl ControlPacket {
    /// Create a new control packet with the initiator's reporting flag and sounding counter.
    pub fn new(initiator_reports: bool, sequence_number: u32) -> Self {
        Self {
            initiator_reports,
            sequence_number,
        }
    }
}

/// Responder reply packet — a pure presence beacon.
///
/// Carries no payload fields; in auto-pairing mode it is sent as the 4-byte
/// magic prefix alone, and in manual-pairing mode as a single sentinel byte
/// (ESP-NOW frames should not be zero-length).
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ResponderPacket;

impl ResponderPacket {
    /// Create a new responder presence beacon.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ResponderPacket {
    fn default() -> Self {
        Self::new()
    }
}

/// One-byte sentinel used for manual-mode responder replies so the frame is
/// never zero-length.
pub(crate) const RESPONDER_BEACON_SENTINEL: u8 = 0;

/// Serialize `packet` into `buf`, optionally prefixed with a 4-byte
/// little-endian `magic`. Returns the populated slice. When `send_magic` is
/// false only the postcard body is written (manual-pairing mode).
pub(crate) fn serialize_with_magic<'a, T: Serialize>(
    packet: &T,
    magic: u32,
    send_magic: bool,
    buf: &'a mut [u8],
) -> Result<&'a [u8], postcard::Error> {
    if send_magic {
        buf[0..4].copy_from_slice(&magic.to_le_bytes());
        let body = postcard::to_slice(packet, &mut buf[4..])?;
        let len = 4 + body.len();
        Ok(&buf[..len])
    } else {
        let body = postcard::to_slice(packet, buf)?;
        let len = body.len();
        Ok(&buf[..len])
    }
}

/// The sounding number of an initiator's control frame, found in a received frame's payload.
///
/// Called from the CSI callback, so the measurement of a control frame is labelled with that
/// frame's own number instead of whichever one the ESP-NOW task processed last — the CSI callback
/// fires before the ESP-NOW receive path has run. Finds the magic prefix anywhere in the payload
/// (the ESP-NOW vendor element sits a few bytes into the action frame), so it only works in
/// auto-pairing mode; manual pairing sends no magic and returns `None`.
pub(crate) fn control_sequence_in(payload: &[u8]) -> Option<u32> {
    let magic = INITIATOR_MAGIC_NUMBER.to_le_bytes();
    let at = payload.windows(4).position(|w| w == magic)?;
    postcard::take_from_bytes::<ControlPacket>(&payload[at + 4..])
        .ok()
        .map(|(p, _)| p.sequence_number)
}

/// Parse a frame produced by [`serialize_with_magic`]. When `expect_magic` is
/// true the leading 4 bytes must equal `magic`, otherwise `None` is returned
/// (caller bumps the magic-drop counter). The remaining bytes are postcard
/// decoded into `T`.
///
/// Uses `take_from_bytes` rather than `from_bytes` so any trailing bytes after
/// the encoded `T` are ignored — the CPU-test TX pads `ControlPacket` frames up
/// to the cell payload size, and those pad bytes must not fail the decode.
pub(crate) fn parse_with_magic<T: for<'de> Deserialize<'de>>(
    data: &[u8],
    magic: u32,
    expect_magic: bool,
) -> Option<T> {
    let body = if expect_magic {
        if data.len() < 4 || u32::from_le_bytes(data[0..4].try_into().ok()?) != magic {
            return None;
        }
        &data[4..]
    } else {
        data
    };
    postcard::take_from_bytes::<T>(body)
        .ok()
        .map(|(v, _rest)| v)
}
