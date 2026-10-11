//! [`MeasurementSetup`]: how often a node sounds the channel, at what width, and how it acquires
//! the estimate.
//!
//! IEEE 802.11bf negotiates a *measurement setup* before any sounding. This is the node-level form
//! of the same idea: the parameters a [`CsiNode`](crate::CsiNode) is built with, beside its
//! [`Mode`](crate::Mode).
//!
//! **Two types share the name.** This one, at the crate root, is the node's own configuration and
//! holds a [`CsiConfig`] — a chip-specific type that never leaves the device. The serialized form a
//! controller sends over a link is [`wire::MeasurementSetup`](crate::wire::MeasurementSetup),
//! always addressed through the `wire` module; it has no `esp-*` types in it, and its encoding is a
//! wire contract. [`From<&MeasurementSetup>`](#impl-From%3C%26MeasurementSetup%3E-for-MeasurementSetup)
//! produces the wire form from this one, and
//! [`CsiNode::apply_measurement_setup`](crate::CsiNode::apply_measurement_setup) takes the wire
//! form back onto a node.

use crate::CsiConfig;
use crate::wire::Bandwidth;

/// The node-level measurement setup: periodicity, bandwidth and acquisition.
///
/// Build with [`MeasurementSetup::new`] (or `Default`) and chain the setters; every parameter is
/// optional and an unset one leaves the mode's own behaviour in place.
///
/// ```rust,ignore
/// let setup = MeasurementSetup::new()
///     .periodicity_hz(100)
///     .acquisition(CsiConfig::default());
/// let node = CsiNode::esp_now(EspNowConfig::default(), setup, hardware);
/// ```
///
/// Not to be confused with the serialized [`wire::MeasurementSetup`](crate::wire::MeasurementSetup);
/// see the [module documentation](self).
#[derive(Debug, Clone, Default)]
pub struct MeasurementSetup {
    id: u8,
    periodicity_hz: Option<u16>,
    bandwidth: Option<Bandwidth>,
    acquisition: Option<CsiConfig>,
}

impl MeasurementSetup {
    /// A setup that changes nothing: no periodicity, the mode's own bandwidth, the default
    /// acquisition, id 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// How often the node sounds the channel, in soundings per second.
    ///
    /// Read by the modes that sound it: the ESP-NOW initiator's control-packet rate and the
    /// responder's reply pacing, the simplex source's flood, the station's gateway ping and the
    /// access point's downlink flood. The sniffer and the simplex peer sound nothing and ignore it;
    /// the emitter takes its period from [`EmitterConfig`](crate::EmitterConfig) instead.
    ///
    /// Unset, a mode that sounds the channel uses its own default rate (as fast as the link allows
    /// for the ESP-NOW and simplex modes; no ping traffic for the station and access point).
    pub fn periodicity_hz(mut self, hz: u16) -> Self {
        self.periodicity_hz = Some(hz);
        self
    }

    /// The channel width the setup is for.
    ///
    /// The modes that fix their width in their own config (ESP-NOW and simplex through
    /// `with_ht40`, the access point through its secondary channel, the emitter through its PHY)
    /// keep that width; a different value here is reported with a warning when the node starts
    /// rather than silently overriding the config. A station's width is whatever it associates at,
    /// and a sniffer's follows each PPDU. The value is carried into the
    /// [wire form](crate::wire::MeasurementSetup) as the acquisition and stimulus bandwidth.
    pub fn bandwidth(mut self, bandwidth: Bandwidth) -> Self {
        self.bandwidth = Some(bandwidth);
        self
    }

    /// How the radio acquires the channel estimate. Unset, the chip's default [`CsiConfig`] is used.
    /// An emitter and a simplex source measure nothing and ignore it.
    pub fn acquisition(mut self, acquisition: CsiConfig) -> Self {
        self.acquisition = Some(acquisition);
        self
    }

    /// The setup id stamped on every controlled sounding taken under it, as
    /// [`Stimulus::Controlled::setup_id`](crate::wire::Stimulus::Controlled). Defaults to 0.
    pub fn id(mut self, id: u8) -> Self {
        self.id = id;
        self
    }

    /// The configured periodicity, in Hz, if one was set.
    pub fn get_periodicity_hz(&self) -> Option<u16> {
        self.periodicity_hz
    }

    /// The configured bandwidth, if one was set.
    pub fn get_bandwidth(&self) -> Option<Bandwidth> {
        self.bandwidth
    }

    /// The configured acquisition, if one was set.
    pub fn get_acquisition(&self) -> Option<&CsiConfig> {
        self.acquisition.as_ref()
    }

    /// The setup id.
    pub fn get_id(&self) -> u8 {
        self.id
    }

    pub(crate) fn set_periodicity_hz(&mut self, hz: Option<u16>) {
        self.periodicity_hz = hz;
    }

    pub(crate) fn set_acquisition(&mut self, acquisition: Option<CsiConfig>) {
        self.acquisition = acquisition;
    }

    pub(crate) fn set_id(&mut self, id: u8) {
        self.id = id;
    }
}

/// The serialized form of a node-level setup.
///
/// `id` and `bandwidth` carry over directly. A periodicity becomes a
/// [`StimulusParams`](crate::wire::StimulusParams) with raw reports, no grouping and non-TB
/// sounding — what the radio produces — at the setup's bandwidth (20 MHz if none was set). The
/// acquisition is chip-specific and has no wire counterpart, so it does not travel; the receiving
/// node keeps its own.
impl From<&MeasurementSetup> for crate::wire::MeasurementSetup {
    fn from(setup: &MeasurementSetup) -> Self {
        let mut acquisition = crate::wire::AcquisitionFilter::default();
        acquisition.bandwidth = setup.bandwidth;
        let mut wire = crate::wire::MeasurementSetup::new(setup.id).with_acquisition(acquisition);
        if let Some(hz) = setup.periodicity_hz {
            wire = wire.with_stimulus(crate::wire::StimulusParams::new(
                1_000_000 / hz.max(1) as u32,
                setup.bandwidth.unwrap_or(Bandwidth::Mhz20),
            ));
        }
        wire
    }
}
