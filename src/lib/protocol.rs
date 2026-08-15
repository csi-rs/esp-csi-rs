//! ESP-NOW wire-format packets and the magic-prefix framing helpers.
//!
//! The central transmits [`ControlPacket`]s and the peripheral replies with
//! [`PeripheralPacket`] presence beacons. In auto-pairing mode each frame is
//! prefixed with a 4-byte little-endian magic (see [`serialize_with_magic`] /
//! [`parse_with_magic`]); in manual-pairing mode no magic is sent and the
//! source-MAC filter is the discriminator.

use serde::{Deserialize, Serialize};

pub(crate) static CENTRAL_MAGIC_NUMBER: u32 = 0xA8912BF0;
pub(crate) static PERIPHERAL_MAGIC_NUMBER: u32 = !CENTRAL_MAGIC_NUMBER;

/// Control packet sent from Central to Peripheral.
///
/// In auto-pairing mode the serialized frame is prefixed with a 4-byte
/// little-endian magic (see [`serialize_with_magic`] / [`parse_with_magic`]);
/// in manual-pairing mode no magic is sent and the source-MAC filter is the
/// discriminator. Both nodes must agree on the pairing mode (and on the
/// `statistics` feature, which gates `sequence_number`) for frames to parse.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ControlPacket {
    /// Whether the central is currently in collector mode; the peripheral
    /// mirrors this flag to keep the pair in sync.
    pub is_collector: bool,
    /// Monotonic sequence number used to detect drops/reordering. Only present
    /// when the `statistics` feature is enabled, to keep the frame small.
    #[cfg(feature = "statistics")]
    pub sequence_number: u32,
}

impl ControlPacket {
    /// Create a new control packet with the collector flag (and, under the
    /// `statistics` feature, a sequence number).
    pub fn new(is_collector: bool, #[cfg(feature = "statistics")] sequence_number: u32) -> Self {
        Self {
            is_collector,
            #[cfg(feature = "statistics")]
            sequence_number,
        }
    }
}

/// Peripheral reply packet — a pure presence beacon.
///
/// Carries no payload fields; in auto-pairing mode it is sent as the 4-byte
/// magic prefix alone, and in manual-pairing mode as a single sentinel byte
/// (ESP-NOW frames should not be zero-length).
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct PeripheralPacket;

impl PeripheralPacket {
    /// Create a new peripheral presence beacon.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PeripheralPacket {
    fn default() -> Self {
        Self::new()
    }
}

/// One-byte sentinel used for manual-mode peripheral replies so the frame is
/// never zero-length.
pub(crate) const PERIPHERAL_BEACON_SENTINEL: u8 = 0;

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
