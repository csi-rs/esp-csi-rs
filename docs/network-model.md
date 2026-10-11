# A role model for CSI sensing networks

Deployments described in one vocabulary, so two of them can be compared. This is the normative
description; the crate's types are its image, and no other document in the ecosystem restates it.

**The naming rule.** Where IEEE 802.11bf has a word for a concept, this model uses that word with
the standard's meaning. Where the standard has none, it uses a plain word of its own — and never
borrows one of the standard's for something the standard means differently.

## 1. Why a model is needed

Existing ESP-class tools describe deployments with a single binary — *active/passive*,
*transmitter/receiver*, *sender/receiver*. That binary collapses attributes that vary
independently: **who originates the soundings**, **who sends and who measures them**, and **who
reports the measurement**. Several things a deployment needs to say then cannot be said. A node that
both sends sounding frames and measures them has no name. A node that measures without reporting —
the node that keeps the channel occupied, and the instrument that separates acquisition cost from
delivery cost — has no name. And the binary is usually inherited from whichever link the tool
implements, so the vocabulary changes when the link does.

IEEE 802.11bf standardises Wi-Fi sensing and already separates two of these axes: *initiator /
responder* (which station originates a sensing procedure) and *transmitter / receiver* (which
station sends the PPDU a measurement is taken from, and which measures it). This model adopts both
unchanged. What the standard does not describe — because it is not a WLAN concern — is how a node
reaches the channel in hardware that implements none of its procedures, and whether a node's
measurements leave the device at all. Those two attributes get plain words of their own: **mode**
and **reporting**.

## 2. Definition

A **CSI sensing network** is a set of nodes sharing a channel in which soundings excite the channel
and at least one node reports a measurement of it. A node is described by four attributes:

| Attribute | Question | Values | Term from |
|---|---|---|---|
| **Mode** | How does the node reach the channel? | ESP-NOW, ESP-NOW simplex, sniffer, station, access point, emitter | this model |
| **Session role** | Does it originate the soundings, take part in them, or neither? | initiator, responder, none (observer) | IEEE 802.11bf |
| **Sensing role** | Does it send the sensing PPDUs, measure them, or both? | transmitter, receiver, transmitter-receiver | IEEE 802.11bf |
| **Reporting** | Do its measurements leave it, and how often? | report (to host or local), threshold-based, decimate, none | this model, plus 802.11bf's threshold-based reporting |

A fifth role belongs to no node: the **controller**, which starts and stops the run (§2.5).

### 2.1 Mode — how the node reaches the channel

The mode is the model's extension point, and it is deliberately abstract: it names the link a node
uses to take part in the network, not a property of CSI measurement itself. Any link that can
excite a channel and yield a channel measurement can supply a mode. `esp-csi-rs` supplies six:

| Mode | How the node participates |
|---|---|
| **ESP-NOW** | connectionless, symmetric exchange; auto-pairing, optional forced-PHY unicast replies |
| **ESP-NOW simplex** | connectionless, asymmetric; the source owns all transmit airtime and the peer is receive-only after discovery |
| **Wi-Fi sniffer** | promiscuous capture on a locked channel; the traffic is whatever is already on air |
| **Wi-Fi station** | associates to an ESP softAP or a commercial router |
| **Wi-Fi access point** | self-contained softAP with DHCP; associated stations generate the uplink that is measured |
| **Emitter** | transmit-only sounding (raw injection or ESP-NOW broadcast, by chip): unassociated, no peer and no handshake |

A negotiated 802.11bf sensing session, when ESP-class hardware can run one, becomes another mode;
the other three attributes are unchanged by it.

### 2.2 Session role — who originates the soundings

| Session role | Definition |
|---|---|
| **Initiator** | IEEE 802.11bf sensing initiator: originates the soundings that a session measures. |
| **Responder** | IEEE 802.11bf sensing responder: takes part in soundings an initiator set up. |
| *none* | An **observer**: takes part in no session. It measures traffic it neither originates nor answers (a sniffer), or sounds for whoever listens (an emitter). |

The ESP-NOW initiator originates the control traffic and the responders answer it. The responders'
transmissions answer the initiator's soundings rather than constituting an independent source,
which is why both directions can yield CSI without the arrangement becoming peer-to-peer. A
responder that originated its own sounding traffic would simply be a second initiator.

The soundings need not come from inside the network: a commercial access point or ambient activity
on the channel excites it just as well, and such a network simply has no initiator. That is the
sniffer deployment, and admitting it is what keeps ambient measurement and controlled sounding in
one model.

**Discovery is not initiation.** An ESP-NOW initiator broadcasting for peers, or a simplex peer
beaconing to be found, settles *who* is in the network, not *when* the measurement runs.

### 2.3 Sensing role — who sends and who measures

| Sensing role | Definition |
|---|---|
| **Transmitter** | IEEE 802.11bf sensing transmitter: sends the PPDUs used for sensing, and measures nothing. |
| **Receiver** | IEEE 802.11bf sensing receiver: measures PPDUs another node sends. |
| **Transmitter-receiver** | Both: sends sensing PPDUs and measures the ones it receives — each end of a symmetric exchange. |

The session role and the sensing role are independent, as they are in the standard. An ESP-NOW
responder is a transmitter-receiver; an access point is an initiator that only measures (a
receiver); an emitter is a transmitter with no session role.

### 2.4 Reporting — whether, and how often, a node's measurements leave it

| Reporting | Measures | Reports | Purpose |
|---|---|---|---|
| **Report** to **host** | yes | every measurement — its own, and any reported to it by peers — to the host output | Produces the dataset |
| **Report** to **local** | yes | every measurement, to on-device consumers (a callback or a queue) only; nothing on the host output | On-device processing without paying for the host link |
| **Threshold-based** | yes | only while the channel is moving | IEEE 802.11bf's threshold-based reporting, applied at the device boundary |
| **Decimate(n)** | yes | every *n*th measurement | Thins a steady stream to what the link can carry |
| **None** | yes | nothing | Takes part in the channel and in capture without contributing data or delivery cost |

A node that reports nothing is not an idle node: it is one that must exist for the measurement to
happen but contributes no data — the responder an initiator needs something to sound, a node that
keeps traffic on the channel while another measures, or a receiver with its output switched off to
separate acquisition cost from delivery cost.

The test an attribute has to pass is whether another node can tell the difference. Whether a
transmitter's radio has capture enabled internally is invisible to every other node, so it is not an
attribute. A node reporting only on motion, or one frame in ten, is one another node can tell
apart.

Reporting is also an ESP-NOW protocol field: an initiator announces whether it reports at all, and a
responder that hears an initiator reporting nothing **promotes itself** to report-to-host, so the
pair still produces a dataset. The promotion changes the reporting *in force* and leaves the
configured value alone.

### 2.5 Controller — who starts and stops the run

The **controller** starts and stops a run and decides what it measures. It is never a node, and in
particular it is **never the initiator**: an 802.11bf initiator is a station that originates
soundings inside the WLAN, while the controller sits outside it. In 802.11bf terms the controller is
the layer above the MAC that requests measurements through its service interface.

In this stack the controller is **always the host tier**: the run begins when something calls
`CsiNode::run` and ends when something calls `CsiNodeClient::send_stop`, and that something is the
serial console for `esp-csi-cli-rs`, the on-device UI task for `esp-csi-litetui-rs`, the HTTP
control route for `csi-webserver`, or your own application when the crate is used as a library.

## 3. What each mode admits

The attributes are independent, but not every mode can express every combination — a sniffer never
transmits, so it cannot be a sensing transmitter. **Rather than document the invalid combinations as
a rule to observe, the crate does not offer them**: each mode exposes only the configurations it
admits, so an illegal node cannot be built.

| Mode | Session role | Sensing role | Reporting | Why the rest is not offered |
|---|---|---|---|---|
| ESP-NOW | initiator or responder | transmitter-receiver | any | symmetric exchange: every combination is meaningful |
| ESP-NOW simplex, source | initiator | transmitter | none | the asymmetry fixes the assignment: the source floods and measures nothing |
| ESP-NOW simplex, peer | responder | receiver | report | the peer goes receive-only and exists to produce the dataset |
| Wi-Fi sniffer | none (observer) | receiver | report, threshold-based or decimate | it never transmits; and a sniffer that reports nothing observes nothing |
| Wi-Fi station | initiator or none | transmitter-receiver or receiver | any | initiator when the uplink it drives is the traffic being measured |
| Wi-Fi access point | initiator | receiver | any | it drives the downlink its stations answer, and measures their uplink |
| Emitter | none | transmitter | none | the sounding frames are the network's traffic, and it measures nothing |

In the code this is structural rather than documentary. `EspNowConfig` carries `session` and
`reporting`; `WifiStationConfig` carries `session` (`Some(Initiator)` or `None`) and `reporting`;
`WifiApConfig` carries only `reporting`; the sniffer offers `with_report_to`, `with_threshold` and
`with_decimation` and no way to say `None`; the emitter exposes `const` accessors and no setter at
all. There is nothing to call that would build a transmitting sniffer or a silent one.

How often a node sounds the channel, at what width, and how it acquires the estimate is its
**measurement setup** (`MeasurementSetup`: periodicity, bandwidth, acquisition) — 802.11bf's term for
the parameters agreed before any sounding. The serialized form a controller sends over a link is
`wire::MeasurementSetup`; the two carry the same idea at different layers.

Cardinality: at most one initiator per responder, any number of responders per initiator, no
responder-to-responder association. The familiar deployment shapes follow from the cardinality
rather than from a separate list — **single node** (one node, traffic already on the channel),
**point-to-point** (one initiator, one responder) and **star** (one initiator, n responders) — so a
new arrangement needs no new word.

### 3.1 What the firmware cannot enforce

Two of the model's rules are properties of a *network*, and this is firmware for one node with no
network-wide view. They are conventions the host tier can check and the node cannot:

- **Cardinality.** ESP-NOW pairing is magic-prefix or MAC-filter based and an access point hands out
  N leases with no role attached. Two initiators on one channel is physically constructible, and no
  node can notice.
- **Exactly one controller (§2.5).** A node only knows that something called `run()`.

The type system delivers the per-node table in §3. It does not, and cannot, deliver these.

## 4. Relation to IEEE 802.11bf

IEEE Std 802.11bf-2025 was published in September 2025. It amends the MAC and the PHY service
interface so that stations can negotiate sensing capability, set up a measurement, sound the channel
and return feedback. Its subject is the sensing procedure between stations.

**No deployed CSI tool implements it.** Every CSI dataset the field works with is produced by
hardware that exposes channel estimates through a vendor-specific path rather than a standardised
sensing procedure: a patched driver on the Intel 5300, patched firmware on Broadcom parts, a driver
callback on ESP-IDF. No ESP-class part today negotiates a sensing session. The modes in §2.1 are how
nodes reach the channel in the meantime, and each one is described with the standard's roles so
that an 802.11bf mode, when it exists, slots in beside them.

**Reporting stops at the device boundary.** 802.11bf does define measurement delivery: feedback to
the sensing initiator, a sensing-by-proxy procedure in which an AP senses on a station's behalf, and
a MAC service interface through which layers above the MAC request and retrieve measurements. What
it does not describe is what happens after that — whether a node's measurements leave it at all,
and over what link, encoding and storage. That is what reporting settles, and why `None` and the
`host`/`local` destinations have no counterpart in the standard. Threshold-based reporting does: the
standard lets a responder report only when the channel has changed by more than a threshold, and
`ThresholdBased` is the same idea applied at the device boundary.

### 4.1 Mapping

| This model | IEEE 802.11bf |
|---|---|
| Session role: initiator / responder | sensing initiator / sensing responder — same attribute, same names |
| Sensing role: transmitter / receiver | sensing transmitter / sensing receiver — same attribute, same names |
| Measurement setup | measurement setup — the parameters agreed before sounding |
| Threshold-based reporting | threshold-based sensing measurement reporting |
| Report (host or local), decimate, none | no counterpart: delivery beyond the device boundary |
| Mode | no counterpart: how hardware without 802.11bf reaches the channel |
| Controller | the layer above the MAC that requests measurements through its service interface; not a WLAN role |

The measurement records follow the same mapping. A frame's *stimulus* says what excited the channel:
a **controlled** sounding carries a measurement-setup id and an instance id, as an 802.11bf
measurement does (an ESP-NOW exchange numbers its soundings the same way today); **ambient** traffic
carries only its transmitter; and an **observed** sounding is someone else's — the NDP that followed
an NDPA, overheard by a sniffer that never joined the session.

### 4.2 Earlier vocabulary

Before 0.13 the crate used a vocabulary of its own for the roles. It maps onto this model as
follows; the CHANGELOG carries the full API migration table.

| Before 0.13 | Now |
|---|---|
| network role *central* (originates the traffic) | session role **initiator** |
| network role *peripheral* (originates none) | session role **responder** (ESP-NOW), or **none** (sniffer, passive station) |
| reporting policy *always* (earlier: *collector*) | reporting **report**, to the host |
| reporting policy *never* (earlier: *listener*) | reporting **none** |
| reporting policy *threshold* | reporting **threshold-based** |
| operational mode | **mode** |
| mode roles *source / peer / observer / sounder* | session role plus sensing role (§3) |
| session role *controller* | the **controller**, no longer a session role (§2.5) |
| traffic frequency | measurement-setup **periodicity** |

When 802.11bf-capable parts arrive, an 802.11bf sensing session becomes another mode and the roles,
the reporting and the cardinality rules are unchanged. That is the test of whether a model was worth
writing down: the standard arriving should extend it, not replace it.
