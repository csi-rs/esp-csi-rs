//! The node model: the four attributes that describe a node in a CSI collection network.
//!
//! The normative description is `docs/network-model.md`, included below so that the document and
//! the types it describes cannot drift: there is one copy, and it reaches docs.rs, GitHub and every
//! README link from the same file. No other document in the ecosystem restates it.
//!
//! The types in this module are that document's image. Three rules connect them:
//!
//! 1. The four attributes are **independent** ([`NetworkRole`], [`ReportingPolicy`],
//!    [`OperationalMode`], [`SessionRole`]). Collapsing any two loses a configuration somebody
//!    deploys.
//! 2. **Illegal combinations are not offered.** Each mode exposes only the attributes it admits;
//!    the fixed ones are `const` accessors with no setter to call.
//! 3. The role and the reporting policy are **computed from the mode**, never stored beside it. A
//!    second home for a value is a second place to get it wrong.
//!
//! Every public enum here is `#[non_exhaustive]`, so a new operational mode — an IEEE 802.11bf
//! sensing mode, say — or a new reporting policy is a minor release, not a breaking one. Inside
//! this crate the matches stay exhaustive, so a new variant is still a compile error everywhere it
//! has to be handled.
//!
#![doc = include_str!("../../docs/network-model.md")]

/// What a node contributes to the network: the traffic, or nothing.
///
/// This is the attribute that separates a node that *originates* the sounding traffic from one that
/// merely answers it. A peripheral's transmissions — the unicast replies of a symmetric ESP-NOW
/// exchange, say — answer the central's traffic rather than constituting an independent source,
/// which is why both directions can yield CSI without the arrangement becoming peer-to-peer.
///
/// A network whose traffic comes from outside it — a commercial access point, or ambient activity
/// on the channel — simply has no central. That is the sniffer deployment.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NetworkRole {
    /// Generates the network's traffic. One or more peripherals may connect to it.
    Central,
    /// Generates no traffic. May optionally connect to one central, at most.
    Peripheral,
}

/// Whether, and how often, a node's measurements leave it.
///
/// Every policy captures CSI; they differ in what is reported. A node that never reports is not an
/// idle node — it is one that must exist for the measurement to happen but contributes no data: the
/// peripheral a central needs something to transmit to, a node that keeps traffic on the channel
/// while another node collects, or a collector with its output switched off to separate
/// acquisition cost from delivery cost.
///
/// [`Threshold`](Self::Threshold) mirrors IEEE 802.11bf's threshold-based reporting: the node
/// reports only while the channel is changing. It is useful well before 802.11bf hardware exists,
/// because gating delivery on motion **on the device** takes the load off a USB or UART link.
/// [`Decimate`](Self::Decimate) thins a steady stream to every *n*th measurement.
///
/// Whether a node reports at all is also an ESP-NOW protocol field. A central announces it in
/// [`ControlPacket::is_collector`](crate::ControlPacket), and a peripheral that hears a central
/// reporting [`Never`](Self::Never) promotes itself to [`Always`](Self::Always) so the pair still
/// produces a dataset. That promotion happens at runtime and does not rewrite the node's
/// configuration, so [`CSINode::reporting`](crate::CSINode::reporting) reports what was configured
/// while [`runtime_reporting`](crate::runtime_reporting) reports what is in force.
///
/// Before 0.12 this was `CollectionMode { Collector, Listener }`: `Collector` is
/// [`Always`](Self::Always) and `Listener` is [`Never`](Self::Never).
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReportingPolicy {
    /// Report every measurement — its own, and any reported to it by peers. Produces the dataset.
    Always,
    /// Report nothing. Participates in the channel and in capture without contributing data or
    /// paying the delivery cost. (Was `CollectionMode::Listener`.)
    Never,
    /// Report only while the channel is moving. See [`Threshold`].
    Threshold(Threshold),
    /// Report every *n*th measurement. `Decimate(1)` is [`Always`](Self::Always); `0` is treated as
    /// 1.
    Decimate(u16),
}

#[allow(non_upper_case_globals)]
impl ReportingPolicy {
    /// The pre-0.12 `CollectionMode::Collector`.
    #[deprecated(since = "0.12.0", note = "use `ReportingPolicy::Always`")]
    pub const Collector: Self = Self::Always;
    /// The pre-0.12 `CollectionMode::Listener`.
    #[deprecated(since = "0.12.0", note = "use `ReportingPolicy::Never`")]
    pub const Listener: Self = Self::Never;

    /// Whether this policy reports anything at all — everything except [`Never`](Self::Never).
    pub const fn reports(self) -> bool {
        !matches!(self, Self::Never)
    }
}

/// The pre-0.12 name of [`ReportingPolicy`]. `Collector` and `Listener` remain as deprecated
/// associated constants, so `CollectionMode::Listener` still compiles.
#[deprecated(since = "0.12.0", note = "renamed to `ReportingPolicy`: `Collector` is `Always`, `Listener` is `Never`")]
pub type CollectionMode = ReportingPolicy;

/// Parameters of [`ReportingPolicy::Threshold`].
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

/// Who starts, stops and takes part in a measurement session.
///
/// Two different things were called "initiator" before 0.12, and only one of them is IEEE 802.11bf's.
/// The names are now split:
///
/// - [`Controller`](Self::Controller) is whatever starts and stops a run: whatever calls
///   [`CSINode::run`](crate::CSINode::run) and
///   [`CSINodeClient::send_stop`](crate::CSINodeClient::send_stop). In practice that is the host
///   tier — the serial console of `esp-csi-cli-rs`, the HTTP control route of `csi-webserver`, the
///   on-device UI task of `esp-csi-litetui-rs` — or your own application. It is never a node, so no
///   node reports it; the variant exists so those crates can name the role instead of inventing a
///   word for it.
/// - [`Initiator`](Self::Initiator) and [`Responder`](Self::Responder) keep **802.11bf's**
///   meaning: the two ends of a negotiated sensing session, in which the initiator requests
///   measurements and a responder takes part. No operational mode this crate builds today runs
///   such a session, so [`OperationalMode::session_role`] is `None` for all of them. An 802.11bf
///   mode would report one of these, and could change it per session.
///
/// Discovery is not initiation. An ESP-NOW central broadcasting for peers, or a simplex peer
/// beaconing to be found, is pairing — it settles *who* is in the network, not *when* the
/// measurement runs.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionRole {
    /// Starts and stops the run. Not a node.
    Controller,
    /// IEEE 802.11bf sensing initiator: requests the measurements that constitute a session.
    Initiator,
    /// IEEE 802.11bf sensing responder: takes part in a session an initiator set up.
    Responder,
}

/// Which end of an asymmetric ESP-NOW simplex exchange a node is: the simplex mode's role.
///
/// The ends are built through [`SimplexConfig::source`] and [`SimplexConfig::peer`], which take
/// different arguments because the two ends consume different settings.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SimplexEnd {
    /// The flooding end: a central that reports nothing.
    Source,
    /// The measuring end: a receive-only peripheral that reports everything.
    Peer,
}

/// Configuration for the asymmetric ESP-NOW simplex exchange.
///
/// One node owns all the transmit airtime and floods; the other goes receive-only after discovery
/// and measures the flood. Leaving the channel to a single transmitter is what makes this the
/// highest-rate pairing the crate offers.
///
/// **The two ends are not interchangeable, so they are not built the same way.** The source needs a
/// PHY rate, a bandwidth and a traffic frequency; the peer forces no TX PHY at all — it only
/// receives — so handing it those settings would silently ignore them. Each constructor takes only
/// what its end consumes.
///
/// The network role follows the traffic, as it does everywhere else in this model: the node that
/// floods is the [`Central`](NetworkRole::Central), and the node that beacons to be found and then
/// falls silent is the [`Peripheral`](NetworkRole::Peripheral). Before 0.11 the crate had these the
/// other way around — it assigned the roles by who initiated *discovery* rather than by who
/// originated the *traffic* — which made a "peripheral" the only node transmitting.
pub struct SimplexConfig {
    end: SimplexEnd,
    inner: crate::EspNowConfig,
}

impl SimplexConfig {
    /// The flooding end: a **central listener**.
    ///
    /// Listens for the peer's discovery beacon, learns its MAC, registers it as a unicast peer with
    /// a forced PHY, then unicasts a continuous max-rate flood. It captures nothing, so its receive
    /// path is disabled regardless of how the node is otherwise configured — a CSI rate task here
    /// would compete for the airtime the flood exists to fill.
    ///
    /// Takes the full [`EspNowConfig`](crate::EspNowConfig) because this end is where channel, PHY
    /// rate and HT40 all apply. Any network role or reporting policy set on that config is ignored:
    /// this end is fixed.
    pub fn source(config: crate::EspNowConfig) -> Self {
        Self {
            end: SimplexEnd::Source,
            inner: config
                .with_network_role(NetworkRole::Central)
                .with_reporting(ReportingPolicy::Never),
        }
    }

    /// The measuring end: a **peripheral collector**.
    ///
    /// Broadcasts a sparse (~1 Hz) discovery beacon until it hears a [`source`](Self::source), then
    /// stops beaconing entirely and goes receive-only, capturing CSI from the source's flood.
    ///
    /// Takes only a channel, because that is all this end uses. There is no PHY rate to force — the
    /// peer never transmits once paired — and no traffic frequency, because it generates no traffic.
    pub fn peer(channel: u8) -> Self {
        Self {
            end: SimplexEnd::Peer,
            inner: crate::EspNowConfig::default()
                .with_channel(channel)
                .with_network_role(NetworkRole::Peripheral)
                .with_reporting(ReportingPolicy::Always),
        }
    }

    /// Pair with a known MAC instead of discovering one. Both ends must be given the other's.
    pub fn with_peer_mac(mut self, mac: [u8; 6]) -> Self {
        self.inner = self.inner.with_peer_mac(mac);
        self
    }

    /// Whether this node floods (`Central`) or measures (`Peripheral`).
    pub fn network_role(&self) -> NetworkRole {
        self.inner.network_role()
    }

    /// Whether this node reports the CSI it captures. Fixed by the end.
    pub fn reporting(&self) -> ReportingPolicy {
        self.inner.reporting()
    }

    /// Which end this is: the simplex mode's role.
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
/// uses to take part in the network, not a property of CSI collection itself. Any link that can
/// excite a channel and yield a channel measurement can supply a mode.
///
/// Each variant admits only the attribute combinations that are meaningful for that link, and it
/// admits them through its config type rather than through a rule written down somewhere:
///
/// | Mode | Role ([`ModeRole`]) | Network role | Reporting policy |
/// |---|---|---|---|
/// | [`EspNow`](Self::EspNow) | central or peripheral | either | any |
/// | [`EspNowSimplex`](Self::EspNowSimplex) | source or peer | fixed by the end | fixed by the end |
/// | [`Sniffer`](Self::Sniffer) | observer | peripheral | always, threshold or decimate — never `Never` |
/// | [`Station`](Self::Station) | central or peripheral | either | any |
/// | [`AccessPoint`](Self::AccessPoint) | central | central | any |
/// | [`Emitter`](Self::Emitter) | sounder | central | never |
///
/// The role column is the mode's own vocabulary, defined by the mode rather than imposed on it, so
/// a future IEEE 802.11bf mode can name its ends initiator and responder — and let a receiver
/// initiate, or change roles per session — without touching the ESP-NOW modes.
#[non_exhaustive]
pub enum OperationalMode {
    /// Connectionless symmetric exchange: auto-pairing, optional forced-PHY unicast replies, no AP
    /// and no association. The only mode that admits a star — one central against many peripherals.
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

impl OperationalMode {
    /// What this node contributes to the network.
    ///
    /// Computed, never stored: the modes that fix this attribute return a constant, and the modes
    /// that admit a choice delegate to their config. There is no second copy of the value on
    /// [`CSINode`](crate::CSINode) that could disagree with the mode it was built from.
    pub fn network_role(&self) -> NetworkRole {
        match self {
            Self::EspNow(c) => c.network_role(),
            Self::EspNowSimplex(c) => c.network_role(),
            Self::Sniffer(_) => crate::WifiSnifferConfig::network_role(),
            Self::Station(c) => c.network_role(),
            Self::AccessPoint(_) => crate::WifiApConfig::network_role(),
            Self::Emitter(_) => crate::EmitterConfig::network_role(),
        }
    }

    /// Whether, and how often, this node's measurements leave it, **as configured**. See
    /// [`runtime_reporting`](crate::runtime_reporting) for the policy in force.
    pub fn reporting(&self) -> ReportingPolicy {
        match self {
            Self::EspNow(c) => c.reporting(),
            Self::EspNowSimplex(c) => c.reporting(),
            Self::Sniffer(c) => c.reporting(),
            Self::Station(c) => c.reporting(),
            Self::AccessPoint(c) => c.reporting(),
            Self::Emitter(_) => crate::EmitterConfig::reporting(),
        }
    }

    /// The pre-0.12 name of [`reporting`](Self::reporting).
    #[deprecated(since = "0.12.0", note = "renamed to `reporting`")]
    pub fn collection_mode(&self) -> ReportingPolicy {
        self.reporting()
    }

    /// This mode's role, in the mode's own vocabulary. See the table on [`OperationalMode`].
    pub fn role(&self) -> ModeRole {
        match self {
            Self::EspNow(c) => c.network_role().into(),
            Self::EspNowSimplex(c) => match c.end() {
                SimplexEnd::Source => ModeRole::Source,
                SimplexEnd::Peer => ModeRole::Peer,
            },
            Self::Sniffer(_) => ModeRole::Observer,
            Self::Station(c) => c.network_role().into(),
            Self::AccessPoint(_) => ModeRole::Central,
            Self::Emitter(_) => ModeRole::Sounder,
        }
    }

    /// This node's role in an IEEE 802.11bf sensing session, if the mode runs one. `None` for every
    /// mode this crate builds today; see [`SessionRole`]. The run's
    /// [`Controller`](SessionRole::Controller) is never a node.
    pub const fn session_role(&self) -> Option<SessionRole> {
        None
    }
}

/// A node's role within its operational mode, named in that mode's vocabulary.
///
/// Each mode defines which of these it admits — see the table on [`OperationalMode`]. A sniffer is
/// only ever an [`Observer`](Self::Observer): it never transmits, so "central" or "peripheral"
/// means nothing for it.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModeRole {
    /// Originates the network's traffic (ESP-NOW, station, access point).
    Central,
    /// Answers or measures traffic another node originates (ESP-NOW, station).
    Peripheral,
    /// The flooding end of an ESP-NOW simplex exchange.
    Source,
    /// The measuring end of an ESP-NOW simplex exchange.
    Peer,
    /// Observes traffic it neither originates nor answers (sniffer).
    Observer,
    /// Transmits soundings and measures nothing (emitter).
    Sounder,
}

impl From<NetworkRole> for ModeRole {
    fn from(role: NetworkRole) -> Self {
        match role {
            NetworkRole::Central => Self::Central,
            NetworkRole::Peripheral => Self::Peripheral,
        }
    }
}

/// What a [`RadioProfile`](crate::RadioProfile) is told about the node it is configuring.
///
/// A view rather than the mode itself, and `#[non_exhaustive]` on purpose. Profiles live
/// out-of-tree, so every change to what they are handed is a breaking change for someone; passing a
/// struct means the node can start offering a profile its network role, its reporting policy or its
/// hardware without changing the trait's signature again.
///
/// [`OperationalMode`] stays exhaustive, because inside this crate a new mode *should* be a compile
/// error in the dispatch that has to handle it.
#[non_exhaustive]
pub struct NodeView<'a> {
    mode: &'a OperationalMode,
}

impl<'a> NodeView<'a> {
    pub(crate) fn new(mode: &'a OperationalMode) -> Self {
        Self { mode }
    }

    /// How the node reaches the channel.
    pub fn mode(&self) -> &OperationalMode {
        self.mode
    }

    /// What the node contributes to the network.
    pub fn network_role(&self) -> NetworkRole {
        self.mode.network_role()
    }

    /// Whether the node's measurements leave it, as configured.
    pub fn reporting(&self) -> ReportingPolicy {
        self.mode.reporting()
    }

    /// The node's role within its mode.
    pub fn role(&self) -> ModeRole {
        self.mode.role()
    }
}
