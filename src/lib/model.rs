//! The node model: the four attributes that describe a node in a CSI collection network.
//!
//! The full model, including the deployment shapes it admits and its relation to IEEE 802.11bf,
//! is in [`crate::model`]'s companion document, `docs/network-model.md`. This module is that
//! document's type-level image. Three rules hold it together:
//!
//! 1. The four attributes are **independent**. What a node contributes to the network
//!    ([`NetworkRole`]), whether its measurements leave it ([`CollectionMode`]), how it reaches the
//!    channel ([`OperationalMode`]) and who starts the measurement ([`SessionRole`]) vary
//!    separately, and collapsing any two of them loses a configuration somebody deploys.
//! 2. **Illegal combinations are not offered.** A sniffer never transmits, so it cannot be a
//!    central; an access point beacons, so it cannot be a peripheral. Rather than document those as
//!    rules to observe, each [`OperationalMode`] exposes only the attributes it admits — the fixed
//!    ones are `const` accessors with no setter to call, so an illegal node cannot be built.
//! 3. The role and the collection mode are **computed from the mode**, never stored beside it.
//!    A second home for the value is a second place to get it wrong.

/// What a node contributes to the network: the traffic, or nothing.
///
/// This is the attribute that separates a node that *originates* the sounding traffic from one that
/// merely answers it. A peripheral's transmissions — the unicast replies of a symmetric ESP-NOW
/// exchange, say — answer the central's traffic rather than constituting an independent source,
/// which is why both directions can yield CSI without the arrangement becoming peer-to-peer.
///
/// A network whose traffic comes from outside it — a commercial access point, or ambient activity
/// on the channel — simply has no central. That is the sniffer deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkRole {
    /// Generates the network's traffic. One or more peripherals may connect to it.
    Central,
    /// Generates no traffic. May optionally connect to one central, at most.
    Peripheral,
}

/// Whether a node's measurements leave it.
///
/// Both values capture CSI; they differ in whether it is reported. A listener is not an idle node —
/// it is one that must exist for the measurement to happen but contributes no data: the peripheral
/// a central needs something to transmit to, a node that keeps traffic on the channel while another
/// node collects, or a collector with its output switched off to separate acquisition cost from
/// delivery cost.
///
/// This is also an ESP-NOW protocol field. A central announces its value in
/// [`ControlPacket::is_collector`](crate::ControlPacket), and a peripheral that hears a listening
/// central promotes itself to [`Collector`](Self::Collector) so the pair still produces a dataset.
/// That promotion happens at runtime and does not rewrite the node's configuration, so
/// [`CSINode::collection_mode`](crate::CSINode::collection_mode) reports what was configured while
/// [`runtime_collection_mode`](crate::runtime_collection_mode) reports what is in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionMode {
    /// Captures CSI and reports it — its own, and any reported to it by peers. Produces the dataset.
    Collector,
    /// Captures CSI and does not report it. Participates in the channel and in capture without
    /// contributing data or paying the delivery cost.
    Listener,
}

/// Who starts and stops the measurement session.
///
/// The names are IEEE 802.11bf's, which defines a sensing session as an agreement between a sensing
/// initiator and a sensing responder to take part in a sensing procedure. This is the one attribute
/// where this model and the standard describe the same thing, so it takes the standard's terms
/// rather than inventing its own.
///
/// **Every node built by this crate is a [`Responder`](Self::Responder).** Initiating a session is
/// not originating traffic: the central's traffic is what a measurement is made *of*, while
/// initiation is the decision that a run starts, stops, and measures something — and that decision
/// is always made by whatever calls [`CSINode::run`](crate::CSINode::run) and
/// [`CSINodeClient::send_stop`](crate::CSINodeClient::send_stop). In practice that is the host tier:
/// the serial console for `esp-csi-cli-rs`, the on-device UI task for `esp-csi-litetui-rs`, the HTTP
/// control route for `csi-webserver`, or your own application when you use this crate as a library.
///
/// The type is exported so those crates can name the role instead of inventing a word for it.
/// [`CSINode`](crate::CSINode) deliberately has no field of this type: a constant with a setter in
/// front of it is exactly the shape this refactor removed elsewhere.
///
/// Note that discovery is not initiation. An ESP-NOW central broadcasting for peers, or a simplex
/// peer beaconing to be found, is pairing — it settles *who* is in the network, not *when* the
/// measurement runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRole {
    /// Starts and stops the session and requests the measurements that constitute it. Exactly one
    /// per session.
    Initiator,
    /// Takes part in a session started by an initiator.
    Responder,
}

/// Which end of an asymmetric ESP-NOW simplex exchange a node is.
///
/// Private: the two ends are reached through [`SimplexConfig::source`] and [`SimplexConfig::peer`],
/// which take different arguments because the two ends consume different settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimplexEnd {
    Source,
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
    /// rate and HT40 all apply. Any network role or collection mode set on that config is ignored:
    /// this end is fixed.
    pub fn source(config: crate::EspNowConfig) -> Self {
        Self {
            end: SimplexEnd::Source,
            inner: config
                .with_network_role(NetworkRole::Central)
                .with_collection_mode(CollectionMode::Listener),
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
                .with_collection_mode(CollectionMode::Collector),
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
    pub fn collection_mode(&self) -> CollectionMode {
        self.inner.collection_mode()
    }

    /// Whether this is the flooding end.
    pub fn is_source(&self) -> bool {
        self.end == SimplexEnd::Source
    }

    pub(crate) fn inner(&self) -> &crate::EspNowConfig {
        &self.inner
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
/// | Mode | Network role | Collection mode |
/// |---|---|---|
/// | [`EspNow`](Self::EspNow) | either | either |
/// | [`EspNowSimplex`](Self::EspNowSimplex) | fixed by the end | fixed by the end |
/// | [`Sniffer`](Self::Sniffer) | peripheral | collector |
/// | [`Station`](Self::Station) | either | either |
/// | [`AccessPoint`](Self::AccessPoint) | central | either |
/// | [`Emitter`](Self::Emitter) | central | listener |
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

    /// Whether this node's measurements leave it, **as configured**. See
    /// [`runtime_collection_mode`](crate::runtime_collection_mode) for the value in force.
    pub fn collection_mode(&self) -> CollectionMode {
        match self {
            Self::EspNow(c) => c.collection_mode(),
            Self::EspNowSimplex(c) => c.collection_mode(),
            Self::Sniffer(_) => crate::WifiSnifferConfig::collection_mode(),
            Self::Station(c) => c.collection_mode(),
            Self::AccessPoint(c) => c.collection_mode(),
            Self::Emitter(_) => crate::EmitterConfig::collection_mode(),
        }
    }

    /// Every node this crate builds is a [`Responder`](SessionRole::Responder). See [`SessionRole`]
    /// for why the session's initiator is never a node.
    pub const fn session_role(&self) -> SessionRole {
        SessionRole::Responder
    }
}

/// What a [`RadioProfile`](crate::RadioProfile) is told about the node it is configuring.
///
/// A view rather than the mode itself, and `#[non_exhaustive]` on purpose. Profiles live
/// out-of-tree, so every change to what they are handed is a breaking change for someone; passing a
/// struct means the node can start offering a profile its network role, its collection mode or its
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
    pub fn collection_mode(&self) -> CollectionMode {
        self.mode.collection_mode()
    }
}
