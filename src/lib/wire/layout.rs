//! How an Espressif CSI buffer maps onto subcarriers.
//!
//! A raw CSI buffer is a run of `(imag, real)` `i8` pairs, one per subcarrier, grouped by training
//! field. Which fields are present, in what order and over which subcarrier indices depends on the
//! chip generation, the PPDU format, the bandwidth, the secondary-channel position and STBC — none
//! of which the buffer records. Before 0.12 a host had to guess them from the chip name and the
//! buffer length. The device now classifies the buffer as it captures it and ships a [`LayoutId`];
//! this module turns that id back into subcarrier indices.
//!
//! The tables are transcribed from ESP-IDF's *Wi-Fi Channel State Information* documentation
//! (`docs/en/api-guides/wifi-driver/wifi-vendor-features.rst`), one table for the classic MAC
//! (ESP32, ESP32-S2/S3, ESP32-C3) and one for the ESP32-C5. **They describe the buffer with every
//! training field enabled.** A configuration that disables one produces a shorter buffer, and a
//! buffer whose length does not match its table entry classifies as [`LayoutId::Unknown`] rather
//! than being mapped wrongly. The ESP32-C6 has no published table and always classifies as
//! `Unknown`.
//!
//! Like the rest of [`crate::wire`], this module has no `esp-*` imports.

use postcard::experimental::max_size::MaxSize;
use serde::{Deserialize, Serialize};

/// A training field within a CSI buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Ltf {
    /// Legacy long training field.
    Lltf,
    /// HT long training field (HT-LTF1 on the C5).
    HtLtf,
    /// Second HT long training field: STBC-HT-LTF on the classic MAC, HT-LTF2 on the C5.
    HtLtf2,
    /// HE long training field.
    HeLtf,
}

/// One training field's run of subcarriers within a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// The training field.
    pub ltf: Ltf,
    /// Inclusive subcarrier-index ranges, in buffer order.
    pub ranges: &'static [(i16, i16)],
}

impl Segment {
    /// Number of subcarriers in the segment.
    pub const fn len(&self) -> usize {
        let mut n = 0;
        let mut i = 0;
        while i < self.ranges.len() {
            let (a, b) = self.ranges[i];
            n += (b - a + 1) as usize;
            i += 1;
        }
        n
    }

    /// Whether the segment has no subcarriers.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Subcarrier indices in buffer order.
    pub fn indices(&self) -> impl Iterator<Item = i16> + '_ {
        self.ranges.iter().flat_map(|&(a, b)| a..=b)
    }
}

/// The layout of a raw Espressif CSI buffer. See the [module docs](self).
///
/// Encoded as one byte. New layouts are appended, so an older host decodes a newer layout as an
/// error rather than as the wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
#[repr(u8)]
pub enum LayoutId {
    /// Not classified: the chip has no published table, or the buffer length did not match the
    /// table entry its metadata selects (a training field was disabled in the CSI config).
    Unknown = 0,

    // Classic MAC: ESP32, ESP32-S2/S3, ESP32-C3. `None`/`Below`/`Above` is the secondary channel.
    /// Classic, no secondary, non-HT. 128 bytes.
    ClassicNoneNonHt,
    /// Classic, no secondary, HT20. 256 bytes.
    ClassicNoneHt20,
    /// Classic, no secondary, HT20 STBC. 384 bytes.
    ClassicNoneHt20Stbc,
    /// Classic, secondary below, non-HT. 128 bytes.
    ClassicBelowNonHt,
    /// Classic, secondary below, HT20. 256 bytes.
    ClassicBelowHt20,
    /// Classic, secondary below, HT20 STBC. 380 bytes.
    ClassicBelowHt20Stbc,
    /// Classic, secondary below, HT40. 384 bytes.
    ClassicBelowHt40,
    /// Classic, secondary below, HT40 STBC. 612 bytes.
    ClassicBelowHt40Stbc,
    /// Classic, secondary above, non-HT. 128 bytes.
    ClassicAboveNonHt,
    /// Classic, secondary above, HT20. 256 bytes.
    ClassicAboveHt20,
    /// Classic, secondary above, HT20 STBC. 376 bytes.
    ClassicAboveHt20Stbc,
    /// Classic, secondary above, HT40. 384 bytes.
    ClassicAboveHt40,
    /// Classic, secondary above, HT40 STBC. 612 bytes.
    ClassicAboveHt40Stbc,

    // ESP32-C5. The C5 reports one field per PPDU: L-LTF, or the HT/VHT/HE-LTF, chosen by
    // `acquire_csi_force_lltf`. VHT shares the HT layout.
    /// C5 L-LTF, no secondary. 106 bytes.
    C5LltfNone,
    /// C5 L-LTF, secondary below. 106 bytes.
    C5LltfBelow,
    /// C5 L-LTF, secondary above. 106 bytes.
    C5LltfAbove,
    /// C5 HT20/VHT20, no secondary. 114 bytes.
    C5Ht20None,
    /// C5 HT20/VHT20 STBC, no secondary. 228 bytes.
    C5Ht20NoneStbc,
    /// C5 HT20, secondary below. 114 bytes.
    C5Ht20Below,
    /// C5 HT20 STBC, secondary below. 228 bytes.
    C5Ht20BelowStbc,
    /// C5 HT20, secondary above. 114 bytes.
    C5Ht20Above,
    /// C5 HT20 STBC, secondary above. 228 bytes.
    C5Ht20AboveStbc,
    /// C5 HT40 (secondary above or below). 234 bytes.
    C5Ht40,
    /// C5 HT40 STBC (secondary above or below). 468 bytes.
    C5Ht40Stbc,
    /// C5 HE20 SU. 490 bytes. For an STBC PPDU the field sampled is chosen by
    /// `acquire_csi_he_stbc`; the index map is the same.
    C5He20Su,
}

// Subcarrier index runs, in buffer order.
const R_0_31_M32_M1: &[(i16, i16)] = &[(0, 31), (-32, -1)];
const R_0_63: &[(i16, i16)] = &[(0, 63)];
const R_0_62: &[(i16, i16)] = &[(0, 62)];
const R_M64_M1: &[(i16, i16)] = &[(-64, -1)];
const R_M62_M1: &[(i16, i16)] = &[(-62, -1)];
const R_0_63_M64_M1: &[(i16, i16)] = &[(0, 63), (-64, -1)];
const R_0_60_M60_M1: &[(i16, i16)] = &[(0, 60), (-60, -1)];

const R_0_26_M26_M1: &[(i16, i16)] = &[(0, 26), (-26, -1)];
const R_0_52: &[(i16, i16)] = &[(0, 52)];
const R_M53_M1: &[(i16, i16)] = &[(-53, -1)];
const R_0_28_M28_M1: &[(i16, i16)] = &[(0, 28), (-28, -1)];
const R_0_56: &[(i16, i16)] = &[(0, 56)];
const R_M57_M1: &[(i16, i16)] = &[(-57, -1)];
const R_0_58_M58_M1: &[(i16, i16)] = &[(0, 58), (-58, -1)];
const R_0_122_M122_M1: &[(i16, i16)] = &[(0, 122), (-122, -1)];

// A macro rather than a `const fn`: only a struct literal is promoted to a `'static` constant
// inside the `&[...]` tables below.
macro_rules! seg {
    ($ltf:ident, $ranges:expr) => {
        Segment { ltf: Ltf::$ltf, ranges: $ranges }
    };
}

impl LayoutId {
    /// The buffer's training-field segments, in order. Empty for [`LayoutId::Unknown`].
    pub const fn segments(self) -> &'static [Segment] {
        match self {
            Self::Unknown => &[],

            Self::ClassicNoneNonHt => &[seg!(Lltf, R_0_31_M32_M1)],
            Self::ClassicNoneHt20 => &[seg!(Lltf, R_0_31_M32_M1), seg!(HtLtf, R_0_31_M32_M1)],
            Self::ClassicNoneHt20Stbc => &[
                seg!(Lltf, R_0_31_M32_M1),
                seg!(HtLtf, R_0_31_M32_M1),
                seg!(HtLtf2, R_0_31_M32_M1),
            ],
            Self::ClassicBelowNonHt => &[seg!(Lltf, R_0_63)],
            Self::ClassicBelowHt20 => &[seg!(Lltf, R_0_63), seg!(HtLtf, R_0_63)],
            Self::ClassicBelowHt20Stbc => {
                &[seg!(Lltf, R_0_63), seg!(HtLtf, R_0_62), seg!(HtLtf2, R_0_62)]
            }
            Self::ClassicBelowHt40 => &[seg!(Lltf, R_0_63), seg!(HtLtf, R_0_63_M64_M1)],
            Self::ClassicBelowHt40Stbc => &[
                seg!(Lltf, R_0_63),
                seg!(HtLtf, R_0_60_M60_M1),
                seg!(HtLtf2, R_0_60_M60_M1),
            ],
            Self::ClassicAboveNonHt => &[seg!(Lltf, R_M64_M1)],
            Self::ClassicAboveHt20 => &[seg!(Lltf, R_M64_M1), seg!(HtLtf, R_M64_M1)],
            Self::ClassicAboveHt20Stbc => &[
                seg!(Lltf, R_M64_M1),
                seg!(HtLtf, R_M62_M1),
                seg!(HtLtf2, R_M62_M1),
            ],
            Self::ClassicAboveHt40 => &[seg!(Lltf, R_M64_M1), seg!(HtLtf, R_0_63_M64_M1)],
            Self::ClassicAboveHt40Stbc => &[
                seg!(Lltf, R_M64_M1),
                seg!(HtLtf, R_0_60_M60_M1),
                seg!(HtLtf2, R_0_60_M60_M1),
            ],

            Self::C5LltfNone => &[seg!(Lltf, R_0_26_M26_M1)],
            Self::C5LltfBelow => &[seg!(Lltf, R_0_52)],
            Self::C5LltfAbove => &[seg!(Lltf, R_M53_M1)],
            Self::C5Ht20None => &[seg!(HtLtf, R_0_28_M28_M1)],
            Self::C5Ht20NoneStbc => &[seg!(HtLtf, R_0_28_M28_M1), seg!(HtLtf2, R_0_28_M28_M1)],
            Self::C5Ht20Below => &[seg!(HtLtf, R_0_56)],
            Self::C5Ht20BelowStbc => &[seg!(HtLtf, R_0_56), seg!(HtLtf2, R_0_56)],
            Self::C5Ht20Above => &[seg!(HtLtf, R_M57_M1)],
            Self::C5Ht20AboveStbc => &[seg!(HtLtf, R_M57_M1), seg!(HtLtf2, R_M57_M1)],
            Self::C5Ht40 => &[seg!(HtLtf, R_0_58_M58_M1)],
            Self::C5Ht40Stbc => &[seg!(HtLtf, R_0_58_M58_M1), seg!(HtLtf2, R_0_58_M58_M1)],
            Self::C5He20Su => &[seg!(HeLtf, R_0_122_M122_M1)],
        }
    }

    /// Bytes a buffer of this layout occupies, or `None` for [`LayoutId::Unknown`].
    pub const fn byte_len(self) -> Option<usize> {
        let segs = self.segments();
        if segs.is_empty() {
            return None;
        }
        let mut n = 0;
        let mut i = 0;
        while i < segs.len() {
            n += segs[i].len();
            i += 1;
        }
        Some(n * 2)
    }

    /// `self` if `len` matches the table entry, else [`LayoutId::Unknown`].
    const fn checked(self, len: usize) -> Self {
        match self.byte_len() {
            Some(n) if n == len => self,
            _ => Self::Unknown,
        }
    }

    /// Classify a classic-MAC buffer (ESP32, ESP32-S2/S3, ESP32-C3) from its receive metadata.
    ///
    /// `secondary` is the raw `secondary_channel` (0 none, 1 above, 2 below), `sig_mode` the raw
    /// `sig_mode` (0 non-HT, 1 HT), `forty` the `cwb` bit, `stbc` the `stbc` field and `len` the
    /// buffer length in bytes.
    pub const fn classify_classic(secondary: u8, sig_mode: u8, forty: bool, stbc: bool, len: usize) -> Self {
        let id = match (secondary, sig_mode, forty, stbc) {
            (0, 0, _, _) => Self::ClassicNoneNonHt,
            (0, 1, false, false) => Self::ClassicNoneHt20,
            (0, 1, false, true) => Self::ClassicNoneHt20Stbc,
            (2, 0, _, _) => Self::ClassicBelowNonHt,
            (2, 1, false, false) => Self::ClassicBelowHt20,
            (2, 1, false, true) => Self::ClassicBelowHt20Stbc,
            (2, 1, true, false) => Self::ClassicBelowHt40,
            (2, 1, true, true) => Self::ClassicBelowHt40Stbc,
            (1, 0, _, _) => Self::ClassicAboveNonHt,
            (1, 1, false, false) => Self::ClassicAboveHt20,
            (1, 1, false, true) => Self::ClassicAboveHt20Stbc,
            (1, 1, true, false) => Self::ClassicAboveHt40,
            (1, 1, true, true) => Self::ClassicAboveHt40Stbc,
            _ => Self::Unknown,
        };
        id.checked(len)
    }

    /// Classify an ESP32-C5 buffer from its receive metadata.
    ///
    /// `bb_format` is the raw `cur_bb_format`, `secondary` the raw secondary-channel field (0 none,
    /// 1 above, 2 below) and `len` the buffer length in bytes. The C5 reports no bandwidth or STBC
    /// bit, so those are read off the length, which the table makes unambiguous.
    pub const fn classify_c5(bb_format: u8, secondary: u8, len: usize) -> Self {
        // L-LTF: every non-HT PPDU, and any PPDU when `acquire_csi_force_lltf` is set.
        if len == 106 {
            return match secondary {
                0 => Self::C5LltfNone,
                2 => Self::C5LltfBelow,
                1 => Self::C5LltfAbove,
                _ => Self::Unknown,
            };
        }
        match (bb_format, secondary, len) {
            // HT (2) and VHT (3) share a layout.
            (2 | 3, 0, 114) => Self::C5Ht20None,
            (2 | 3, 0, 228) => Self::C5Ht20NoneStbc,
            (2, 2, 114) => Self::C5Ht20Below,
            (2, 2, 228) => Self::C5Ht20BelowStbc,
            (2, 1, 114) => Self::C5Ht20Above,
            (2, 1, 228) => Self::C5Ht20AboveStbc,
            (2, 1 | 2, 234) => Self::C5Ht40,
            (2, 1 | 2, 468) => Self::C5Ht40Stbc,
            (4, 0, 490) => Self::C5He20Su,
            _ => Self::Unknown,
        }
    }

    /// Subcarrier indices of the whole buffer, in buffer order: one per `(imag, real)` pair.
    pub fn indices(self) -> impl Iterator<Item = (Ltf, i16)> {
        self.segments()
            .iter()
            .flat_map(|s| s.indices().map(move |i| (s.ltf, i)))
    }
}

/// Subcarrier spacing for the HT/VHT/legacy layouts: 312.5 kHz.
pub const SPACING_HZ_LEGACY: u32 = 312_500;
/// Subcarrier spacing for the HE layouts: 78.125 kHz.
pub const SPACING_HZ_HE: u32 = 78_125;

impl Ltf {
    /// Subcarrier spacing of this training field.
    pub const fn spacing_hz(self) -> u32 {
        match self {
            Self::HeLtf => SPACING_HZ_HE,
            _ => SPACING_HZ_LEGACY,
        }
    }
}
