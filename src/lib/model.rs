//! The node model: the attributes that describe a node in a CSI sensing network, named in IEEE
//! 802.11bf's terms wherever the standard has a term.
//!
//! The normative description is `docs/network-model.md`, included below so that the document and
//! the types it describes cannot drift: there is one copy, and it reaches docs.rs, GitHub and every
//! README link from the same file. No other document in the ecosystem restates it.
//!
//! The types in this module are that document's image. Three rules connect them:
//!
//! 1. The attributes are **independent** ([`Mode`], [`SessionRole`], [`SensingRole`],
//!    [`Reporting`]). Collapsing any two loses a configuration somebody deploys.
//! 2. **Illegal combinations are not offered.** Each mode exposes only the attributes it admits;
//!    the fixed ones are `const` accessors with no setter to call.
//! 3. The roles and the reporting are **computed from the mode**, never stored beside it. A second
//!    home for a value is a second place to get it wrong.
//!
//! Every public enum here is `#[non_exhaustive]`, so a new mode — a negotiated IEEE 802.11bf
//! sensing mode, say — or a new kind of reporting is a minor release, not a breaking one. Inside
//! this crate the matches stay exhaustive, so a new variant is still a compile error everywhere it
//! has to be handled.
//!
#![doc = include_str!("../../docs/network-model.md")]

/// A node's part in a sensing session: IEEE 802.11bf's *sensing initiator* or *sensing
/// responder*.
///
/// The initiator is the node that originates the soundings a session measures; a responder takes
/// part in soundings an initiator set up. A node that takes part in no session — a sniffer
/// observing traffic it neither originates nor answers, or an emitter sounding for whoever listens
/// — has no session role, which the accessors report as `None` (an *observer*).
///
/// The host that starts and stops a run is **not** an initiator; it is the [`Controller`], which is
/// never a node.
///
/// | Mode | Session role |
/// |---|---|
/// | ESP-NOW | initiator or responder (configurable) |
/// | ESP-NOW simplex | source: initiator; peer: responder |
/// | Wi-Fi station | initiator when it drives the measured traffic, otherwise `None` |
/// | Wi-Fi access point | initiator |
/// | Wi-Fi sniffer | `None` |
/// | Emitter | `None` |
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionRole {
    /// IEEE 802.11bf sensing initiator: originates the soundings that constitute a session.
    Initiator,
    /// IEEE 802.11bf sensing responder: takes part in soundings an initiator set up.
    Responder,
}

/// What starts and stops a run: the host tier, never a node.
///
/// Whatever calls [`CsiNode::run`](crate::CsiNode::run) and
/// [`CsiNodeClient::send_stop`](crate::CsiNodeClient::send_stop) is the run's controller. In
/// practice that is the serial console of `esp-csi-cli-rs`, the HTTP control route of
/// `csi-webserver`, the on-device UI task of `esp-csi-litetui-rs`, or your own application.
///
/// Controlling a run is not initiating a session: a [`SessionRole::Initiator`] is a node that
/// originates soundings inside the WLAN. In IEEE 802.11bf terms the controller is the layer above
/// the MAC that requests measurements through its service interface. No node reports this role;
/// the type exists so the crates that drive a node can name it instead of inventing a word.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Controller;

/// Which end of a sounding a node is: IEEE 802.11bf's *sensing transmitter* and *sensing
/// receiver*.
///
/// The sensing transmitter sends the PPDUs a measurement is taken from; the sensing receiver
/// measures them. A node in a symmetric exchange does both.
///
/// | Mode | Sensing role |
/// |---|---|
/// | ESP-NOW | transmitter-receiver |
/// | ESP-NOW simplex | source: transmitter; peer: receiver |
/// | Wi-Fi station | transmitter-receiver when it drives the traffic, otherwise receiver |
/// | Wi-Fi access point | receiver |
/// | Wi-Fi sniffer | receiver |
/// | Emitter | transmitter |
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SensingRole {
    /// Sends the PPDUs used for sensing and measures nothing.
    Transmitter,
    /// Measures PPDUs another node sends.
    Receiver,
    /// Both sends the PPDUs used for sensing and measures the ones it receives.
    TransmitterReceiver,
}

impl SensingRole {
    /// Whether this node measures the channel.
    pub const fn receives(self) -> bool {
        !matches!(self, Self::Transmitter)
    }

    /// Whether this node sends the PPDUs used for sensing.
    pub const fn transmits(self) -> bool {
        !matches!(self, Self::Receiver)
    }
}

/// Where a reported measurement goes.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ReportTo {
    /// Emitted to the host output (the serial log in the configured [`LogMode`](crate::LogMode)),
    /// and delivered to any on-device consumer as well.
    #[default]
    Host,
    /// Delivered only to on-device consumers — the callback registered with
    /// [`set_csi_callback`](crate::set_csi_callback) or the queue behind
    /// [`CsiNodeClient::next_csi_packet`](crate::CsiNodeClient::next_csi_packet) — and never
    /// written to the host output.
    Local,
}

/// Whether, and how often, a node's measurements leave it.
///
/// Every variant except [`None`](Self::None) delivers measurements; they differ in which ones. A
/// node that reports nothing is not an idle node — it is one that must exist for the measurement to
/// happen but contributes no data: the responder an initiator needs something to sound, a node that
/// keeps traffic on the channel while another measures, or a receiver with its output switched off
/// to separate acquisition cost from delivery cost.
///
/// [`ThresholdBased`](Self::ThresholdBased) is IEEE 802.11bf's threshold-based reporting: the node
/// reports only while the channel is changing. It is useful well before 802.11bf hardware exists,
/// because gating delivery on motion **on the device** takes the load off a USB or UART link.
/// [`Decimate`](Self::Decimate) thins a steady stream to every *n*th measurement.
///
/// Whether a node reports at all is also an ESP-NOW protocol field. An initiator announces it in
/// [`ControlPacket::initiator_reports`](crate::ControlPacket), and a responder that hears an
/// initiator reporting [`None`](Self::None) promotes itself to `Report { to: Host }` so the pair
/// still produces a dataset. That promotion happens at runtime and does not rewrite the node's
/// configuration, so [`CsiNode::reporting`](crate::CsiNode::reporting) reports what was configured
/// while [`runtime_reporting`](crate::runtime_reporting) reports what is in force.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reporting {
    /// Report every measurement — its own, and any reported to it by peers — to `to`.
    Report {
        /// Where the measurements go.
        to: ReportTo,
    },
    /// Report only while the channel is moving: IEEE 802.11bf threshold-based reporting. See
    /// [`Threshold`].
    ThresholdBased(Threshold),
    /// Report every *n*th measurement. `Decimate(1)` reports everything; `0` is treated as 1.
    Decimate(u16),
    /// Report nothing. Participates in the channel and in capture without contributing data or
    /// paying the delivery cost.
    None,
}

impl Reporting {
    /// `Report { to: ReportTo::Host }`: report every measurement to the host. The default.
    pub const HOST: Self = Self::Report { to: ReportTo::Host };

    /// Whether this reports anything at all — everything except [`None`](Self::None).
    pub const fn reports(self) -> bool {
        !matches!(self, Self::None)
    }
}

impl Default for Reporting {
    fn default() -> Self {
        Self::HOST
    }
}

/// Parameters of [`Reporting::ThresholdBased`].
///
/// The node keeps a slow running average of the channel's amplitude profile over up to 32 sampled
/// subcarriers, and scores every measurement by how far its profile's **shape** departs from that
/// average, on a `0..=65535` scale. Both profiles are normalised by their own totals first, so the
/// receiver's per-frame gain changes score nothing; `65535` is a profile with no overlap at all. A
/// measurement scoring at least `level` is reported and opens a `hold_ms` window in which every
/// measurement is reported, so a movement is captured whole rather than as its loudest frames.
///
/// **Calibrate `level` on your own link.** Raw 8-bit estimates carry per-frame quantisation noise,
/// so a still room does not score zero. Measured on two ESP32-C5s at HE20 (245 subcarriers,
/// 100 frames/s, an empty room), single-frame scores had a median of ~2700 and a 99th percentile
/// of ~4300. Run the node with [`variation_only`](Self::variation_only) first, read the scores it
/// reports while nothing moves, and set `level` comfortably above their high percentile; the level
/// that separates motion from stillness has not been measured here and depends on the room.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Threshold {
    /// Score at or above which a measurement is reported.
    pub level: u16,
    /// How long to keep reporting after a measurement crosses `level`, in milliseconds.
    pub hold_ms: u16,
    /// Report only the score, as a [`CsiPayload::Variation`](crate::wire::CsiPayload::Variation),
    /// instead of the full measurement. The lightest report a node can send.
    pub variation_only: bool,
}

impl Threshold {
    /// Report full measurements scoring at least `level`, holding for `hold_ms`.
    pub const fn new(level: u16, hold_ms: u16) -> Self {
        Self {
            level,
            hold_ms,
            variation_only: false,
        }
    }

    /// Report only the score, not the measurement.
    pub const fn variation_only(mut self) -> Self {
        self.variation_only = true;
        self
    }
}

/// Which end of an asymmetric ESP-NOW simplex exchange a node is.
///
/// The ends are built through [`SimplexConfig::source`] and [`SimplexConfig::peer`], which take
/// different arguments because the two ends consume different settings.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SimplexEnd {
    /// The flooding end: an initiator and sensing transmitter that reports nothing.
    Source,
    /// The measuring end: a receive-only responder and sensing receiver that reports everything.
    Peer,
}

/// Configuration for the asymmetric ESP-NOW simplex exchange.
///
/// One node owns all the transmit airtime and floods; the other goes receive-only after discovery
/// and measures the flood. Leaving the channel to a single transmitter is what makes this the
/// highest-rate pairing the crate offers.
///
/// **The two ends are not interchangeable, so they are not built the same way.** The source needs a
/// PHY rate, a bandwidth and a periodicity; the peer forces no TX PHY at all — it only receives —
/// so handing it those settings would silently ignore them. Each constructor takes only what its
/// end consumes.
///
/// The session role follows the soundings, as it does everywhere else in this model: the node that
/// floods is the [`Initiator`](SessionRole::Initiator), and the node that beacons to be found and
/// then falls silent is the [`Responder`](SessionRole::Responder).
pub struct SimplexConfig {
    end: SimplexEnd,
    inner: crate::EspNowConfig,
}

impl SimplexConfig {
    /// The flooding end: an initiator and sensing transmitter that reports nothing.
    ///
    /// Listens for the peer's discovery beacon, learns its MAC, registers it as a unicast peer with
    /// a forced PHY, then unicasts a continuous max-rate flood. It measures nothing, so its receive
    /// path is disabled regardless of how the node is otherwise configured — a CSI rate task here
    /// would compete for the airtime the flood exists to fill.
    ///
    /// Takes the full [`EspNowConfig`](crate::EspNowConfig) because this end is where channel, PHY
    /// rate and HT40 all apply. Any session role or reporting set on that config is ignored: this
    /// end is fixed.
    pub fn source(config: crate::EspNowConfig) -> Self {
        Self {
            end: SimplexEnd::Source,
            inner: config
                .session(SessionRole::Initiator)
                .reporting(Reporting::None),
        }
    }

    /// The measuring end: a responder and sensing receiver that reports every measurement.
    ///
    /// Broadcasts a sparse (~1 Hz) discovery beacon until it hears a [`source`](Self::source), then
    /// stops beaconing entirely and goes receive-only, capturing CSI from the source's flood.
    ///
    /// Takes only a channel, because that is all this end uses. There is no PHY rate to force — the
    /// peer never transmits once paired — and no periodicity, because it sounds nothing.
    pub fn peer(channel: u8) -> Self {
        Self {
            end: SimplexEnd::Peer,
            inner: crate::EspNowConfig::default()
                .with_channel(channel)
                .session(SessionRole::Responder)
                .reporting(Reporting::HOST),
        }
    }

    /// Pair with a known MAC instead of discovering one. Both ends must be given the other's.
    pub fn with_peer_mac(mut self, mac: [u8; 6]) -> Self {
        self.inner = self.inner.with_peer_mac(mac);
        self
    }

    /// `Initiator` for the source, `Responder` for the peer. Fixed by the end.
    pub fn session_role(&self) -> SessionRole {
        match self.end {
            SimplexEnd::Source => SessionRole::Initiator,
            SimplexEnd::Peer => SessionRole::Responder,
        }
    }

    /// `Transmitter` for the source, `Receiver` for the peer. Fixed by the end.
    pub fn sensing_role(&self) -> SensingRole {
        match self.end {
            SimplexEnd::Source => SensingRole::Transmitter,
            SimplexEnd::Peer => SensingRole::Receiver,
        }
    }

    /// `None` for the source, `Report { to: Host }` for the peer. Fixed by the end.
    pub fn get_reporting(&self) -> Reporting {
        self.inner.get_reporting()
    }

    /// Which end this is.
    pub fn end(&self) -> SimplexEnd {
        self.end
    }

    /// Whether this is the flooding end.
    pub fn is_source(&self) -> bool {
        self.end == SimplexEnd::Source
    }

    pub(crate) fn inner(&self) -> &crate::EspNowConfig {
        &self.inner
    }

    pub(crate) fn set_channel(&mut self, ch: u8) {
        self.inner.channel = ch;
    }
}

/// How a node reaches the channel.
///
/// This is the model's extension point and it is deliberately abstract: it names the link a node
/// uses to take part in the network, not a property of CSI measurement itself. Any link that can
/// excite a channel and yield a channel measurement can supply a mode.
///
/// Each variant admits only the attribute combinations that are meaningful for that link, and it
/// admits them through its config type rather than through a rule written down somewhere:
///
/// | Mode | Session role | Sensing role | Reporting |
/// |---|---|---|---|
/// | [`EspNow`](Self::EspNow) | initiator or responder | transmitter-receiver | any |
/// | [`EspNowSimplex`](Self::EspNowSimplex) | source: initiator; peer: responder | source: transmitter; peer: receiver | source: none; peer: report |
/// | [`Sniffer`](Self::Sniffer) | none (observer) | receiver | report, threshold-based or decimate — never `None` |
/// | [`Station`](Self::Station) | initiator or none | transmitter-receiver or receiver | any |
/// | [`AccessPoint`](Self::AccessPoint) | initiator | receiver | any |
/// | [`Emitter`](Self::Emitter) | none | transmitter | none |
#[non_exhaustive]
pub enum Mode {
    /// Connectionless symmetric exchange: auto-pairing, optional forced-PHY unicast replies, no AP
    /// and no association. The only mode that admits a star — one initiator against many
    /// responders.
    EspNow(crate::EspNowConfig),
    /// Connectionless asymmetric exchange: the source owns all transmit airtime and the peer is
    /// receive-only after discovery. The highest CSI rate of any pairing here.
    EspNowSimplex(SimplexConfig),
    /// Promiscuous capture on a locked channel. The traffic is whatever is already on air, so this
    /// is the one mode that needs no second device — and the one that pairs with an emitter.
    Sniffer(crate::WifiSnifferConfig),
    /// Associates to an ESP softAP or a commercial router and measures the frames it receives.
    Station(crate::WifiStationConfig),
    /// Self-contained softAP with DHCP; associated stations generate the uplink that is measured.
    AccessPoint(crate::WifiApConfig),
    /// Unassociated transmit-only sounding: force a TX PHY and loop-inject frames. Needs no peer,
    /// no handshake and no protocol, which is what lets it compose with anything.
    Emitter(crate::EmitterConfig),
}

impl Mode {
    /// This node's IEEE 802.11bf session role, or `None` for a node that takes part in no session
    /// (an observer).
    ///
    /// Computed, never stored: the modes that fix this attribute return a constant, and the modes
    /// that admit a choice delegate to their config. There is no second copy of the value on
    /// [`CsiNode`](crate::CsiNode) that could disagree with the mode it was built from.
    pub fn session_role(&self) -> Option<SessionRole> {
        match self {
            Self::EspNow(c) => Some(c.session_role()),
            Self::EspNowSimplex(c) => Some(c.session_role()),
            Self::Sniffer(_) => crate::WifiSnifferConfig::session_role(),
            Self::Station(c) => c.session_role(),
            Self::AccessPoint(_) => crate::WifiApConfig::session_role(),
            Self::Emitter(_) => crate::EmitterConfig::session_role(),
        }
    }

    /// This node's IEEE 802.11bf sensing role: whether it sends the sensing PPDUs, measures them,
    /// or both.
    pub fn sensing_role(&self) -> SensingRole {
        match self {
            Self::EspNow(c) => c.sensing_role(),
            Self::EspNowSimplex(c) => c.sensing_role(),
            Self::Sniffer(_) => crate::WifiSnifferConfig::sensing_role(),
            Self::Station(c) => c.sensing_role(),
            Self::AccessPoint(_) => crate::WifiApConfig::sensing_role(),
            Self::Emitter(_) => crate::EmitterConfig::sensing_role(),
        }
    }

    /// Whether, and how often, this node's measurements leave it, **as configured**. See
    /// [`runtime_reporting`](crate::runtime_reporting) for the value in force.
    pub fn reporting(&self) -> Reporting {
        match self {
            Self::EspNow(c) => c.get_reporting(),
            Self::EspNowSimplex(c) => c.get_reporting(),
            Self::Sniffer(c) => c.get_reporting(),
            Self::Station(c) => c.get_reporting(),
            Self::AccessPoint(c) => c.get_reporting(),
            Self::Emitter(_) => crate::EmitterConfig::get_reporting(),
        }
    }
}

/// What a [`RadioProfile`](crate::RadioProfile) is told about the node it is configuring.
///
/// A view rather than the mode itself, and `#[non_exhaustive]` on purpose. Profiles live
/// out-of-tree, so every change to what they are handed is a breaking change for someone; passing a
/// struct means the node can start offering a profile more of its configuration or its hardware
/// without changing the trait's signature again.
///
/// [`Mode`] stays exhaustive inside this crate, because there a new mode *should* be a compile
/// error in the dispatch that has to handle it.
#[non_exhaustive]
pub struct NodeView<'a> {
    mode: &'a Mode,
}

impl<'a> NodeView<'a> {
    pub(crate) fn new(mode: &'a Mode) -> Self {
        Self { mode }
    }

    /// How the node reaches the channel.
    pub fn mode(&self) -> &Mode {
        self.mode
    }

    /// The node's session role, or `None` for an observer.
    pub fn session_role(&self) -> Option<SessionRole> {
        self.mode.session_role()
    }

    /// The node's sensing role.
    pub fn sensing_role(&self) -> SensingRole {
        self.mode.sensing_role()
    }

    /// Whether the node's measurements leave it, as configured.
    pub fn reporting(&self) -> Reporting {
        self.mode.reporting()
    }
}
