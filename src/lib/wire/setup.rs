//! A measurement setup, in IEEE 802.11bf's terms.
//!
//! 802.11bf negotiates a *measurement setup* before any sounding: how often to sound, at what
//! bandwidth, which responders take part, and what form their reports take. Each sounding sequence
//! under it is a *measurement instance*. This type carries the same parameters, split the way a
//! sniffer forces them apart: an [`AcquisitionFilter`] (what a node accepts, which every mode has)
//! and optional [`StimulusParams`] (how the channel is sounded, which only a node that sounds it
//! has).
//!
//! A controller can speak these terms today. A node maps the parameters onto whatever its mode
//! actually does — an ESP-NOW central's periodicity becomes its packet rate — and refuses the ones
//! it cannot honour rather than ignoring them. When an 802.11bf mode exists, the same setup maps
//! onto a negotiated one, with no change for the controller.
//!
//! Like the rest of [`crate::wire`], this module has no `esp-*` imports.

use heapless::Vec;
use postcard::experimental::max_size::MaxSize;
use serde::{Deserialize, Serialize};

use super::{Bandwidth, PpduFormat};

/// Responders a setup can name.
pub const MAX_RESPONDERS: usize = 8;

/// A measurement setup: an id, what to accept, and — for a node that sounds the channel — how to
/// sound it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub struct MeasurementSetup {
    /// Setup id. Stamped on every measurement taken under it, as
    /// [`Stimulus::Controlled::setup_id`](super::Stimulus::Controlled).
    pub id: u8,
    /// What a node accepts.
    pub acquisition: AcquisitionFilter,
    /// How the channel is sounded. `None` for a node that does not sound it — a sniffer.
    pub stimulus: Option<StimulusParams>,
}

impl MeasurementSetup {
    /// A setup that accepts everything and sounds nothing.
    pub fn new(id: u8) -> Self {
        Self {
            id,
            acquisition: AcquisitionFilter::default(),
            stimulus: None,
        }
    }

    /// Set what to accept.
    pub fn with_acquisition(mut self, acquisition: AcquisitionFilter) -> Self {
        self.acquisition = acquisition;
        self
    }

    /// Set how to sound the channel.
    pub fn with_stimulus(mut self, stimulus: StimulusParams) -> Self {
        self.stimulus = Some(stimulus);
        self
    }
}

/// Which measurements a node accepts.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub struct AcquisitionFilter {
    /// Primary channel. `None` keeps the mode's own.
    pub channel: Option<u8>,
    /// Bandwidth. `None` accepts whatever arrives.
    pub bandwidth: Option<Bandwidth>,
    /// PPDU formats to acquire. Empty accepts every format.
    pub ppdu_formats: PpduSet,
    /// Accept only this transmitter. `None` accepts any.
    pub transmitter: Option<[u8; 6]>,
}

/// A set of [`PpduFormat`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, MaxSize)]
pub struct PpduSet(u16);

impl PpduSet {
    /// The empty set, which a filter reads as "every format".
    pub const ANY: Self = Self(0);

    const fn bit(f: PpduFormat) -> u16 {
        1 << match f {
            PpduFormat::Unknown => 0,
            PpduFormat::Dsss => 1,
            PpduFormat::NonHt => 2,
            PpduFormat::Ht => 3,
            PpduFormat::Vht => 4,
            PpduFormat::VhtMu => 5,
            PpduFormat::HeSu => 6,
            PpduFormat::HeMu => 7,
            PpduFormat::HeErSu => 8,
            PpduFormat::HeTb => 9,
            PpduFormat::Eht => 10,
        }
    }

    /// This set plus `f`.
    pub const fn with(self, f: PpduFormat) -> Self {
        Self(self.0 | Self::bit(f))
    }

    /// Whether `f` is in the set. The empty set contains every format.
    pub const fn contains(self, f: PpduFormat) -> bool {
        self.0 == 0 || self.0 & Self::bit(f) != 0
    }

    /// Whether the set is empty (every format accepted).
    pub const fn is_any(self) -> bool {
        self.0 == 0
    }

    /// Whether every format in the set is an HE (802.11ax) format.
    pub const fn only_he(self) -> bool {
        let he = Self::bit(PpduFormat::HeSu)
            | Self::bit(PpduFormat::HeMu)
            | Self::bit(PpduFormat::HeErSu)
            | Self::bit(PpduFormat::HeTb);
        self.0 != 0 && self.0 & !he == 0
    }

    /// Whether the set excludes the legacy formats (DSSS and non-HT OFDM).
    pub const fn excludes_legacy(self) -> bool {
        self.0 != 0 && self.0 & (Self::bit(PpduFormat::Dsss) | Self::bit(PpduFormat::NonHt)) == 0
    }
}

/// How the channel is sounded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub struct StimulusParams {
    /// Interval between soundings, microseconds.
    pub periodicity_us: u32,
    /// Sounding bandwidth.
    pub bandwidth: Bandwidth,
    /// The form a measurement report takes.
    pub report: ReportType,
    /// Subcarrier grouping of a [`ReportType::Grouped`] report (1 = no grouping).
    pub ng: u8,
    /// Bits per quantised value of a [`ReportType::Grouped`] report.
    pub nb: u8,
    /// Trigger-based sounding (the access point triggers responders) rather than non-TB.
    pub trigger_based: bool,
    /// Responders taking part. Empty means any that pairs.
    pub responders: Vec<[u8; 6], MAX_RESPONDERS>,
}

impl StimulusParams {
    /// Sound every `periodicity_us` at `bandwidth`, raw reports, no grouping, non-TB, any
    /// responder.
    pub fn new(periodicity_us: u32, bandwidth: Bandwidth) -> Self {
        Self {
            periodicity_us,
            bandwidth,
            report: ReportType::Raw,
            ng: 1,
            nb: 8,
            trigger_based: false,
            responders: Vec::new(),
        }
    }

    /// Sounding rate in Hz implied by the periodicity, saturated to `u16`.
    pub const fn rate_hz(&self) -> u16 {
        if self.periodicity_us == 0 {
            return u16::MAX;
        }
        let hz = 1_000_000 / self.periodicity_us;
        if hz > u16::MAX as u32 { u16::MAX } else if hz == 0 { 1 } else { hz as u16 }
    }
}

/// The form of a measurement report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
#[non_exhaustive]
pub enum ReportType {
    /// The full channel estimate as the receiver produced it.
    Raw,
    /// A grouped, quantised report ([`CsiPayload::Grouped`](super::CsiPayload::Grouped)).
    Grouped,
}

/// Why a node could not take on a measurement setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SetupError {
    /// The mode does not sound the channel, but the setup asked it to.
    NoStimulus,
    /// The mode sounds the channel, and the setup's stimulus asks for something it cannot do. The
    /// field names which parameter.
    UnsupportedStimulus(&'static str),
    /// The acquisition filter asks for something this node cannot select. The field names which
    /// parameter.
    UnsupportedAcquisition(&'static str),
}
