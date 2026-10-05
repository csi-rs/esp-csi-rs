//! The CSI wire contract: what a node emits, independent of the radio that measured it.
//!
//! Every frame on the serialized stream is an [`Envelope`] followed by a [`Body`], postcard-encoded
//! back to back inside one COBS frame. The envelope comes first so a decoder can read the format
//! version and the source **before** it commits to decoding the body — postcard is not
//! self-describing, so a decoder that guessed the layout would mis-read a newer body silently
//! instead of rejecting it.
//!
//! The contract is deliberately radio-neutral. ESP's receive-control struct used to *be* the data
//! contract; here it is one [`SourceKind`] among several. Its normalised fields live in [`RxMeta`],
//! whatever is specific to one chip family lives in [`VendorRx`], and the CSI itself is a tagged
//! [`CsiPayload`] that records the layout it was captured in rather than leaving the host to guess
//! it from the chip name. An IEEE 802.11bf source, or a synthetic one, fills the same types.
//!
//! **This module has no `esp-*` imports**, only `serde`, `heapless` and `postcard`, so host tooling
//! can depend on the identical definitions instead of mirroring them by hand. Keep it that way.

use heapless::Vec;
use postcard::experimental::max_size::MaxSize;
use serde::{Deserialize, Serialize};

pub mod layout;
pub mod setup;
#[cfg(feature = "synthetic")]
pub mod synthetic;
pub use layout::LayoutId;
pub use setup::{AcquisitionFilter, MeasurementSetup, PpduSet, ReportType, SetupError, StimulusParams};

/// Version of the wire format described by this module.
///
/// Bump it on any change that alters the encoding of [`Envelope`] or [`Body`]: a field added,
/// removed or reordered anywhere inside them, or a variant inserted before an existing one. Adding a
/// variant at the **end** of a `#[non_exhaustive]` enum does not need a bump — an older decoder
/// rejects that frame cleanly instead of mis-reading it.
pub const WIRE_VERSION: u8 = 1;

/// Largest raw CSI buffer any supported chip produces, in bytes.
pub const MAX_CSI_BYTES: usize = 612;

/// Who emitted a frame, when, and from which measurement source. Encoded ahead of every [`Body`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub struct Envelope {
    /// [`WIRE_VERSION`] of the encoder.
    pub version: u8,
    /// Identity of the node that delivered the frame: its base MAC address.
    pub node_id: [u8; 6],
    /// The measurement session this frame belongs to. Set by the controller with
    /// `set_session`, or drawn at random when a run starts without one.
    pub session_id: u32,
    /// The kind of source that produced the measurement.
    pub source: SourceKind,
    /// Per-source frame counter assigned by the delivering node, incremented once per frame it
    /// emits. A gap is a frame lost between capture and the host. Unlike an 802.11 sequence
    /// number it never repeats on a retry and does not depend on who transmitted the sounding.
    pub stream_seq: u32,
}

impl Envelope {
    /// An envelope at the current [`WIRE_VERSION`].
    pub const fn new(node_id: [u8; 6], session_id: u32, source: SourceKind, stream_seq: u32) -> Self {
        Self {
            version: WIRE_VERSION,
            node_id,
            session_id,
            source,
            stream_seq,
        }
    }
}

/// The kind of measurement source behind a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub enum SourceKind {
    /// The vendor CSI callback of an Espressif radio.
    EspVendor,
    /// An IEEE 802.11bf sensing measurement.
    Ieee80211bf,
    /// A synthetic source, for testing the pipeline without hardware.
    Synthetic,
}

/// What follows an [`Envelope`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub enum Body {
    /// One channel measurement.
    Csi(CsiFrame),
    /// Session announcement, emitted when a run starts. Anchors [`RxMeta::timestamp_us`] to wall
    /// time when the controller supplied one.
    Session(SessionInfo),
}

/// Session announcement: what a host needs to interpret the frames that follow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub struct SessionInfo {
    /// The chip that delivers the frames.
    pub chip: Chip,
    /// Version of the crate that encoded the frames: major, minor, patch.
    pub crate_version: [u8; 3],
    /// The node's receive clock at the moment of the announcement, on the same time base as
    /// [`RxMeta::timestamp_us`].
    pub timestamp_us: u64,
    /// Wall-clock time (UNIX epoch, microseconds) corresponding to `timestamp_us`, when the
    /// controller supplied one.
    pub epoch_unix_us: Option<u64>,
}

impl SessionInfo {
    /// A session announcement.
    pub const fn new(
        chip: Chip,
        crate_version: [u8; 3],
        timestamp_us: u64,
        epoch_unix_us: Option<u64>,
    ) -> Self {
        Self {
            chip,
            crate_version,
            timestamp_us,
            epoch_unix_us,
        }
    }
}

/// One channel measurement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub struct CsiFrame {
    /// Normalised receive metadata.
    pub meta: RxMeta,
    /// What excited the channel.
    pub stimulus: Stimulus,
    /// The measured PPDU's MAC header, when the source captured it.
    pub header: Option<HeaderDigest>,
    /// The channel measurement itself.
    pub payload: CsiPayload,
}

impl CsiFrame {
    /// A measurement frame.
    pub fn new(
        meta: RxMeta,
        stimulus: Stimulus,
        header: Option<HeaderDigest>,
        payload: CsiPayload,
    ) -> Self {
        Self {
            meta,
            stimulus,
            header,
            payload,
        }
    }

    /// The transmitter of the measured PPDU: the header's `addr2` when present, else the
    /// stimulus' transmitter address.
    pub fn transmitter(&self) -> Option<[u8; 6]> {
        if let Some(h) = &self.header {
            return Some(h.addr2);
        }
        match self.stimulus {
            Stimulus::Ambient { ta }
            | Stimulus::Observed { ta, .. }
            | Stimulus::Controlled { ta, .. } => Some(ta),
        }
    }
}

/// Normalised receive metadata: the fields every source can state in the same units.
///
/// A field is an `Option` when some source cannot state it. The Espressif 802.11ax-generation
/// MAC (C5/C6), for instance, reports no MCS, STBC, guard-interval or antenna bit, and an absent
/// value is `None` rather than a guess.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub struct RxMeta {
    /// Receive time on the node's clock, microseconds. 64-bit: it does not wrap within a session.
    pub timestamp_us: u64,
    /// Received signal strength, dBm.
    pub rssi: i8,
    /// Noise floor, dBm.
    pub noise_floor: i8,
    /// Primary channel number.
    pub channel: u8,
    /// Position of the secondary channel, for a 40 MHz-capable channel.
    pub secondary: Secondary,
    /// Bandwidth of the measured PPDU, when the source reports or implies it.
    pub bandwidth: Option<Bandwidth>,
    /// Format of the measured PPDU.
    pub ppdu: PpduFormat,
    /// MCS index, for HT/VHT/HE PPDUs where the source reports it.
    pub mcs: Option<u8>,
    /// Whether the PPDU used space-time block coding.
    pub stbc: Option<bool>,
    /// Whether the PPDU used a short guard interval.
    pub sgi: Option<bool>,
    /// Receive chains the measurement covers.
    pub n_rx: u8,
    /// Spatial streams in the measured PPDU, when known.
    pub n_ss: Option<u8>,
    /// Index of the antenna the PPDU was received on, when reported.
    pub antenna: Option<u8>,
    /// Length of the received PPDU including its FCS, bytes.
    pub sig_len: u16,
    /// Receive state: 0 = no error, others are source-specific error codes.
    pub rx_state: u8,
    /// Whether the PPDU was *not* a sounding PPDU (`Some(false)` = sounding, i.e. an NDP), when
    /// the source distinguishes them.
    pub not_sounding: Option<bool>,
    /// Whether the MPDU arrived inside an A-MPDU, when reported.
    pub aggregation: Option<bool>,
    /// The 12-bit 802.11 sequence number of the measured MPDU, when the source reports it.
    /// Assigned by the *transmitter*, per TID, and repeated on retries, so it measures loss on the
    /// air per transmitter. Use [`Envelope::stream_seq`] to count loss between node and host.
    pub frame_seq: Option<u16>,
    /// Fields specific to the source's chip family.
    pub vendor: VendorRx,
}

/// Position of the secondary channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub enum Secondary {
    /// No secondary channel.
    None,
    /// Secondary channel above the primary.
    Above,
    /// Secondary channel below the primary.
    Below,
}

/// Bandwidth of a measured PPDU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub enum Bandwidth {
    /// 20 MHz.
    Mhz20,
    /// 40 MHz.
    Mhz40,
    /// 80 MHz.
    Mhz80,
    /// 160 MHz.
    Mhz160,
    /// 320 MHz.
    Mhz320,
}

impl Bandwidth {
    /// Width in MHz.
    pub const fn mhz(self) -> u16 {
        match self {
            Self::Mhz20 => 20,
            Self::Mhz40 => 40,
            Self::Mhz80 => 80,
            Self::Mhz160 => 160,
            Self::Mhz320 => 320,
        }
    }
}

/// Format of a measured PPDU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub enum PpduFormat {
    /// The source did not report a format it could classify.
    Unknown,
    /// DSSS/CCK (802.11b).
    Dsss,
    /// Non-HT OFDM (802.11a/g).
    NonHt,
    /// High throughput (802.11n).
    Ht,
    /// Very high throughput (802.11ac), single user.
    Vht,
    /// Very high throughput (802.11ac), multi user.
    VhtMu,
    /// High efficiency (802.11ax), single user.
    HeSu,
    /// High efficiency (802.11ax), multi user.
    HeMu,
    /// High efficiency (802.11ax), extended-range single user.
    HeErSu,
    /// High efficiency (802.11ax), trigger based.
    HeTb,
    /// Extremely high throughput (802.11be).
    Eht,
}

/// What excited the channel that was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub enum Stimulus {
    /// A sounding this network controls: an ESP-NOW exchange today, a negotiated 802.11bf
    /// session later. `setup_id` names the measurement setup and `instance_id` the sounding
    /// sequence within it.
    Controlled {
        /// Measurement setup the sounding belongs to.
        setup_id: u8,
        /// Sounding sequence within the setup.
        instance_id: u16,
        /// Transmitter of the measured PPDU. With several responders in one setup, this is what
        /// tells their measurements apart.
        ta: [u8; 6],
    },
    /// Ordinary traffic overheard on the channel, from transmitter `ta`.
    Ambient {
        /// Transmitter address of the measured PPDU.
        ta: [u8; 6],
    },
    /// Someone else's sounding, observed without taking part: the NDP that followed an NDPA from
    /// `ta` carrying `dialog_token`.
    Observed {
        /// Transmitter of the sounding.
        ta: [u8; 6],
        /// Sounding dialog token from the NDPA.
        dialog_token: u8,
    },
}

/// The fixed part of the measured MPDU's MAC header: enough to drop retries, tell uplink from
/// downlink and attribute the frame to a transmitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
pub struct HeaderDigest {
    /// Frame control field.
    pub frame_control: u16,
    /// Receiver address.
    pub addr1: [u8; 6],
    /// Transmitter address.
    pub addr2: [u8; 6],
    /// Third address (BSSID, or source/destination depending on To/From DS).
    pub addr3: [u8; 6],
    /// Sequence control field.
    pub seq_ctrl: u16,
}

impl HeaderDigest {
    /// Length of the header prefix the digest is read from: FC, duration, three addresses and
    /// sequence control.
    pub const HEADER_LEN: usize = 24;

    /// Parse the first [`Self::HEADER_LEN`] bytes of an 802.11 MAC header. `None` if `hdr` is
    /// shorter, or the frame type carries no transmitter address (ACK, CTS).
    pub fn parse(hdr: &[u8]) -> Option<Self> {
        if hdr.len() < Self::HEADER_LEN {
            return None;
        }
        let frame_control = u16::from_le_bytes([hdr[0], hdr[1]]);
        let mut a = [[0u8; 6]; 3];
        for (i, addr) in a.iter_mut().enumerate() {
            addr.copy_from_slice(&hdr[4 + 6 * i..10 + 6 * i]);
        }
        let digest = Self {
            frame_control,
            addr1: a[0],
            addr2: a[1],
            addr3: a[2],
            seq_ctrl: u16::from_le_bytes([hdr[22], hdr[23]]),
        };
        if digest.has_transmitter() { Some(digest) } else { None }
    }

    /// Frame type (bits 2..4 of frame control): 0 management, 1 control, 2 data.
    pub const fn frame_type(&self) -> u8 {
        ((self.frame_control >> 2) & 0b11) as u8
    }

    /// Frame subtype (bits 4..8 of frame control).
    pub const fn subtype(&self) -> u8 {
        ((self.frame_control >> 4) & 0b1111) as u8
    }

    /// Whether the Retry bit is set: the MPDU is a retransmission.
    pub const fn is_retry(&self) -> bool {
        self.frame_control & (1 << 11) != 0
    }

    /// Whether the frame carries a transmitter address. ACK and CTS carry only a receiver.
    pub const fn has_transmitter(&self) -> bool {
        // Control subtypes 12 (CTS) and 13 (ACK) have no addr2.
        !(self.frame_type() == 1 && matches!(self.subtype(), 12 | 13))
    }

    /// The 12-bit sequence number.
    pub const fn sequence_number(&self) -> u16 {
        self.seq_ctrl >> 4
    }
}

/// The channel measurement itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub enum CsiPayload {
    /// An Espressif CSI buffer exactly as the radio produced it: interleaved `(imag, real)` `i8`
    /// pairs, segmented per training field as `layout` describes. Lossless, and costs the device
    /// nothing to produce — the host owns the unpacking, through [`LayoutId`].
    EspRaw {
        /// Chip that produced the buffer.
        chip: Chip,
        /// How the buffer maps onto subcarriers.
        layout: LayoutId,
        /// The first four bytes are invalid (a hardware limitation of some parts).
        first_word_invalid: bool,
        /// The raw buffer.
        bytes: Vec<i8, MAX_CSI_BYTES>,
    },
    /// A grouped, quantised channel report, the shape an IEEE 802.11bf sensing measurement
    /// report takes.
    Grouped {
        /// Subcarrier grouping.
        ng: u8,
        /// Bits per quantised value.
        nb: u8,
        /// Index of the first reported subcarrier.
        sc_start: i16,
        /// Number of reported subcarriers.
        n_sc: u16,
        /// Receive chains.
        n_rx: u8,
        /// Transmit chains.
        n_tx: u8,
        /// Packed report.
        data: Vec<u8, MAX_CSI_BYTES>,
    },
    /// A threshold-based report: only how far the channel moved, not the channel itself.
    Variation {
        /// Variation metric, scaled to the full `u16` range.
        value: u16,
    },
}

/// Chip-family-specific receive fields that have no normalised home in [`RxMeta`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub enum VendorRx {
    /// No vendor fields.
    None,
    /// Espressif classic MAC (ESP32, ESP32-S2/S3, ESP32-C3).
    EspClassic {
        /// PHY rate encoding (non-HT PPDUs).
        rate: u8,
        /// Raw `sig_mode`: 0 non-HT, 1 HT, 3 VHT.
        sig_mode: u8,
        /// Channel-estimate smoothing recommended.
        smoothing: bool,
        /// LDPC FEC coding.
        fec_ldpc: bool,
        /// Subframes in the A-MPDU.
        ampdu_cnt: u8,
    },
    /// Espressif 802.11ax-generation MAC (ESP32-C5, ESP32-C6).
    EspHe {
        /// PHY rate encoding.
        rate: u8,
        /// Raw `cur_bb_format` (`wifi_rx_bb_format_t`): 0 11b, 1 11a/g, 2 HT, 3 VHT, 4 HE SU,
        /// 5 HE MU, 6 HE ER-SU, 7 HE TB, 11 VHT MU.
        cur_bb_format: u8,
        /// The channel estimate is valid (`rx_channel_estimate_info_vld`).
        estimate_valid: bool,
        /// Length of the channel estimate the radio produced.
        estimate_len: u16,
        /// Length of the dump buffer.
        dump_len: u16,
        /// Group-addressed frame.
        is_group: bool,
        /// Receive-end state.
        rxend_state: u8,
        /// Interface-match bits: bit *n* is `rxmatch<n>`.
        rxmatch: u8,
        /// HE-SIG-A1, raw.
        he_siga1: u32,
        /// HE-SIG-A2, raw.
        he_siga2: u16,
        /// HE-SIG-B length (ESP32-C6 only; 0 elsewhere).
        sigb_len: u8,
        /// Single-MPDU reception (ESP32-C6 only; `false` elsewhere).
        single_mpdu: bool,
    },
}

/// Chips that can produce [`CsiPayload::EspRaw`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub enum Chip {
    /// ESP32.
    Esp32,
    /// ESP32-S2.
    Esp32S2,
    /// ESP32-S3.
    Esp32S3,
    /// ESP32-C3.
    Esp32C3,
    /// ESP32-C5.
    Esp32C5,
    /// ESP32-C6.
    Esp32C6,
}

/// Bytes a COBS-encoded frame can occupy at most: both postcard bodies plus COBS overhead and the
/// terminating zero.
pub const MAX_ENCODED_LEN: usize = {
    let raw = Envelope::POSTCARD_MAX_SIZE + Body::POSTCARD_MAX_SIZE;
    raw + raw / 254 + 2
};

/// Encode `envelope` and `body` as one COBS frame into `buf`, returning the used slice (including
/// the trailing `0x00` delimiter).
pub fn encode_cobs<'b>(
    envelope: &Envelope,
    body: &Body,
    buf: &'b mut [u8],
) -> Result<&'b mut [u8], postcard::Error> {
    postcard::to_slice_cobs(&(envelope, body), buf)
}

/// Why a frame could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The frame was written by a different [`WIRE_VERSION`]. The envelope still decoded, so the
    /// version is known; the body was not attempted.
    UnsupportedVersion(u8),
    /// The bytes are not a valid frame.
    Malformed(postcard::Error),
}

impl From<postcard::Error> for DecodeError {
    fn from(e: postcard::Error) -> Self {
        Self::Malformed(e)
    }
}

/// Decode one COBS frame (with or without its trailing delimiter). Decodes in place.
///
/// Like [`decode`], this rejects a frame from another [`WIRE_VERSION`].
///
/// The envelope is read first and its version checked before the body is touched, so a frame from a
/// newer encoder is reported as [`DecodeError::UnsupportedVersion`] rather than mis-read.
pub fn decode_cobs(frame: &mut [u8]) -> Result<(Envelope, Body), DecodeError> {
    let len = cobs::decode_in_place(frame)
        .map_err(|_| DecodeError::Malformed(postcard::Error::DeserializeBadEncoding))?;
    decode(&frame[..len])
}

/// Decode one frame that has already been COBS-decoded. See [`decode_cobs`].
pub fn decode(bytes: &[u8]) -> Result<(Envelope, Body), DecodeError> {
    let (envelope, rest) = postcard::take_from_bytes::<Envelope>(bytes)?;
    if envelope.version != WIRE_VERSION {
        return Err(DecodeError::UnsupportedVersion(envelope.version));
    }
    let (body, _) = postcard::take_from_bytes::<Body>(rest)?;
    Ok((envelope, body))
}
