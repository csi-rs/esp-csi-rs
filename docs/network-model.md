# A role model for CSI collection networks

Deployments described in one vocabulary, so two of them can be compared. This is the normative
description; the crate's types are its image, and no other document in the ecosystem restates it.

## 1. Why a model is needed

Existing ESP-class collectors describe deployments with a single binary — *active/passive*,
*transmitter/receiver*, *sender/receiver*. That binary collapses two attributes that vary
independently: **who sources the traffic** and **who reports the measurement**. Four things a
deployment needs to say then cannot be said. A node that both sources traffic and measures it has no
name. A node that measures without reporting — the node that keeps the channel occupied, and the
instrument that separates acquisition cost from delivery cost — has no name. Many-to-one collection
has no name, because "receiver" does not distinguish a node reporting its own CSI from one reporting
several nodes'. And the binary is usually inherited from whichever link the tool implements, so the
vocabulary changes when the link does.

**And the standard?** IEEE 802.11bf standardises Wi-Fi sensing, so a second vocabulary looks
redundant. It is not, for three reasons developed in §4: the standard's subject is the sensing
session between stations rather than the configuration of a measurement deployment; no collector the
field uses implements its procedures, since CSI reaches these tools through vendor-specific paths;
and it stops at the device boundary, saying nothing about whether a node's measurements leave it.
Where the two describe the same thing — who starts a measurement session — this model takes the
standard's terms unchanged rather than inventing its own.

## 2. Definition

A **CSI collection network** is a set of nodes sharing a channel and a measurement session in which
traffic excites the channel and at least one node reports CSI. A node is described by four
attributes: what it contributes to the network, whether its measurements leave it, how it reaches
the channel, and what part it plays in the session.

### 2.1 Network role — who sources the traffic

| Role | Definition |
|---|---|
| **Central** | Generates the network's traffic. One or more peripherals may connect to it. |
| **Peripheral** | Generates no traffic. May optionally connect to one central, at most. |

What the central supplies is *origination*. Where an operational mode has peripherals transmit — the
unicast replies of a symmetric ESP-NOW exchange, for instance — those transmissions answer the
central's traffic rather than constituting an independent source, which is why both directions can
yield CSI without the arrangement becoming peer-to-peer. A peripheral that originated its own
sounding traffic would simply be a second central, and the model says so rather than tolerating it.

The traffic need not come from inside the network: a commercial access point or ambient activity on
the channel excites it just as well, and such a network simply has no central. That is the sniffer
deployment, and admitting it is what keeps ambient measurement and controlled sounding in one model.

### 2.2 Collection mode — who reports

| Mode | Captures CSI | Reports CSI | Purpose |
|---|---|---|---|
| **Collector** | yes | yes — its own, and any reported to it by peers | Produces the dataset |
| **Listener** | yes | no | Participates in channel and capture without contributing data or delivery cost |

A listener is not an idle node: it is one that must exist for the measurement to happen but
contributes no data — the peripheral a central needs something to transmit to, a node that keeps
traffic on the channel while another node collects, or a collector with its output switched off to
separate acquisition cost from delivery cost.

Two values are enough. A node that only generates traffic is a **central listener**; whether its
radio has capture enabled internally is invisible to every other node, so it is not a network-level
attribute. The test an attribute has to pass is whether another node can tell the difference.

### 2.3 Operational mode — how the node reaches the channel

The operational mode is the model's extension point, and it is deliberately abstract: it names the
link a node uses to take part in the network, not a property of CSI collection itself. Any link that
can excite a channel and yield a channel measurement can supply a mode — the collection network is
defined the same way whether the measurement comes from Wi-Fi, a connectionless protocol, or another
radio entirely. `esp-csi-rs` supplies six:

| Mode | How the node participates |
|---|---|
| **ESP-NOW** | connectionless, symmetric exchange; auto-pairing, optional forced-PHY unicast replies |
| **ESP-NOW simplex** | connectionless, asymmetric; the source owns all transmit airtime and the peer is receive-only after discovery |
| **Wi-Fi sniffer** | promiscuous capture on a locked channel; the traffic is whatever is already on air |
| **Wi-Fi station** | associates to an ESP softAP or a commercial router |
| **Wi-Fi access point** | self-contained softAP with DHCP; associated stations generate the uplink that is measured |
| **Emitter** | transmit-only sounding (raw injection or ESP-NOW broadcast, by chip): unassociated, no peer and no handshake |

### 2.4 Session role — who starts and stops the measurement

| Session role | Definition |
|---|---|
| **Initiator** | Starts and stops the measurement session and requests the measurements that constitute it. Exactly one per session. |
| **Responder** | Takes part in a session started by an initiator. |

The names are taken from IEEE 802.11bf, which defines a sensing session as an agreement between a
sensing initiator and a sensing responder to take part in a sensing procedure. Borrowing the
standard's terms rather than inventing new ones is deliberate: this attribute is the one place the
model and the standard describe the same thing.

**Initiating a session is not originating traffic.** The two verbs are distinct and only one of them
defines a role: traffic origination *is* the central (§2.1), while session initiation is control —
deciding when a run starts, stops and what it measures.

In this stack the initiator is **always the host tier**, and no node ever initiates a session on
another node. Every node `esp-csi-rs` builds is a responder: the run begins when something calls
`CSINode::run` and ends when something calls `CSINodeClient::send_stop`, and that something is the
serial console for `esp-csi-cli-rs`, the on-device UI task for `esp-csi-litetui-rs`, the HTTP control
route for `csi-webserver`, or your own application when the crate is used as a library.

Discovery is not initiation. An ESP-NOW central broadcasting for peers, or a simplex peer beaconing
to be found, settles *who* is in the network, not *when* the measurement runs.

## 3. What each mode admits

The role and the collection mode are independent attributes, but not every mode can express every
combination — a sniffer never transmits, so it cannot be a central. **Rather than document the
invalid combinations as a rule to observe, the crate does not offer them**: each operational mode
exposes only the configurations it admits, so an illegal node cannot be built.

| Operational mode | Network role | Collection mode | Why the rest is not offered |
|---|---|---|---|
| ESP-NOW | either | either | symmetric exchange: every combination is meaningful |
| ESP-NOW simplex | fixed by the end | fixed by the end | the asymmetry fixes the assignment |
| Wi-Fi sniffer | peripheral | collector | it never transmits, so it cannot be central; and a sniffer that does not report observes nothing |
| Wi-Fi station | either | either | central when the uplink it generates is the traffic being measured |
| Wi-Fi access point | central | either | beacons and DHCP make it a traffic source by construction |
| Emitter | central | listener | the sounding frames are the network's traffic, and it captures nothing |

In the code this is structural rather than documentary. `EspNowConfig` and `WifiStationConfig` carry
`with_network_role` and `with_collection_mode`; `WifiApConfig` carries only the second; the sniffer
and emitter configs expose `const` accessors and no setter at all. There is nothing to call that
would build a central sniffer.

Cardinality: at most one central per peripheral, any number of peripherals per central, no
peripheral-to-peripheral association. The familiar deployment shapes follow from the cardinality
rather than from a separate list — **single node** (one node, traffic already on the channel),
**point-to-point** (one central, one peripheral) and **star** (one central, n peripherals) — so a new
arrangement needs no new word.

### 3.1 What the firmware cannot enforce

Two of the model's rules are properties of a *network*, and this is firmware for one node with no
network-wide view. They are conventions the host tier can check and the node cannot:

- **Cardinality.** ESP-NOW pairing is magic-prefix or MAC-filter based and an access point hands out
  N leases with no role attached. Two centrals on one channel is physically constructible, and no
  node can notice.
- **Exactly one initiator (§2.4).** A node only knows that something called `run()`.

The type system delivers the per-node table in §3. It does not, and cannot, deliver these.

## 4. Relation to IEEE 802.11bf

IEEE Std 802.11bf-2025 was published in September 2025, and it answers a different question.

**Different layer.** 802.11bf amends the MAC and the PHY service interface so that stations can
negotiate sensing capability, set up a measurement, sound the channel and return feedback. Its
subject is the sensing procedure and the sensing session between stations. The subject here is how a
measurement *deployment* is configured — which node supplies the traffic, which node reports data,
and how the arrangement scales. A deployment built entirely from 802.11bf-capable hardware would
still have to answer those questions.

**No deployed collector implements it.** The amendment's PHY targets are broad, but a target is not
an implementation. Every CSI dataset the field works with is produced by hardware that exposes
channel estimates through a vendor-specific path rather than a standardised sensing procedure: a
patched driver on the Intel 5300, patched firmware on Broadcom parts, a driver callback on ESP-IDF.
No ESP-class part today negotiates a sensing session.

**Reporting stops at the device boundary.** 802.11bf does define measurement delivery: feedback to
the sensing initiator, a sensing-by-proxy procedure in which an AP senses on a station's behalf, and
a MAC service interface through which layers above the MAC request and retrieve sensing
measurements. What it does not describe — because it is not a WLAN concern — is what happens after
that: whether a node's measurements leave it at all, and over what link, encoding and storage. That
is what the collector/listener distinction settles, and why the listener has no counterpart in the
standard's vocabulary.

### 4.1 Mapping

802.11bf separates two role axes of its own — *initiator/responder*, by which station initiates the
procedure, and *transmitter/receiver*, by which station sends the PPDU used for sensing and which
performs the measurement. That separation is the same instinct as this model's, applied to different
attributes, and the two vocabularies line up without conflict:

| This model | IEEE 802.11bf |
|---|---|
| Central (sources the traffic) | sensing transmitter — transmits the PPDUs used for sensing measurements |
| Peripheral (sources none) | a station that is not a sensing transmitter in the procedure; a sensing receiver where it measures |
| Collector (measures, reports) | sensing receiver that performs the measurement, plus feedback or sensing-by-proxy reporting |
| Listener (measures, no report) | sensing receiver that performs the measurement; no reporting counterpart |
| Initiator / Responder (§2.4) | sensing initiator / sensing responder — same attribute, same names |

Three of the four attributes describe something the standard does not: which node supplies the
traffic for a deployment, whether a node's measurements leave the device, and over which link it
participates. The fourth is the standard's own, and the model adopts its names unchanged.

When 802.11bf-capable parts arrive, an 802.11bf sensing session becomes another operational mode and
the roles, collection modes and cardinality rules are unchanged. That is the test of whether a model
was worth writing down: the standard arriving should extend it, not replace it.
