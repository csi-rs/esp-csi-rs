# `esp-csi-rs`

A Rust crate for collecting **Channel State Information (CSI)** on **ESP32** series devices using
the `no-std` embedded framework.

[![crates.io](https://img.shields.io/crates/v/esp-csi-rs.svg)](https://crates.io/crates/esp-csi-rs)
[![docs.rs](https://docs.rs/esp-csi-rs/badge.svg)](https://docs.rs/esp-csi-rs)

> **Want CSI without writing code?** [`esp-csi-cli-rs`](https://github.com/csi-rs/esp-csi-cli-rs) is
> a CLI wrapper with pre-built binaries that exposes everything this crate does — flash it and talk
> to the board over serial.

## The node model

A deployment is a **CSI sensing network**: nodes sharing a channel, in which soundings excite the
channel and at least one node reports a measurement of it. Each node is described by four
independent attributes, named in IEEE 802.11bf's terms wherever the standard has one.

| Attribute | Values | Question it answers |
|---|---|---|
| Mode | six, below | How does the node reach the channel? |
| Session role | IEEE 802.11bf Initiator / Responder, or none (observer) | Who originates the soundings? |
| Sensing role | IEEE 802.11bf Transmitter / Receiver / both | Who sends the sensing PPDUs, and who measures them? |
| Reporting | Report (to host or local), ThresholdBased, Decimate, None | Whether, and how often, measurements leave the node |

The run is started and stopped by the **controller** — the host tier, never a node and never the
initiator. Not every mode admits every combination, and **the ones it does not admit are not
offered**: where an attribute is fixed there is no setter to call.

| Constructor | Mode | Session role | Sensing role | Reporting |
|---|---|---|---|---|
| `CsiNode::sniffer` | Wi-Fi sniffer | none | receiver | report, threshold-based or decimate |
| `CsiNode::station` | Wi-Fi station | initiator or none | transmitter-receiver or receiver | any |
| `CsiNode::access_point` | Wi-Fi access point | initiator | receiver | any |
| `CsiNode::emitter` | Emitter (transmit-only sounding) | none | transmitter | none |
| `CsiNode::esp_now` | ESP-NOW | initiator or responder | transmitter-receiver | any |
| `CsiNode::esp_now_simplex_source` | ESP-NOW simplex | initiator | transmitter | none |
| `CsiNode::esp_now_simplex_peer` | ESP-NOW simplex | responder | receiver | report |

<div align="center">

![CSI sensing network shapes](https://raw.githubusercontent.com/csi-rs/esp-csi-rs/main/assets/net-arch.png)

</div>

[`docs/network-model.md`](docs/network-model.md) is the normative description, including how the
model maps onto IEEE 802.11bf.

## Architecture

<div align="center">

![Data path from the radio to the host](https://raw.githubusercontent.com/csi-rs/esp-csi-rs/main/assets/data-path.png)

</div>

| Abstraction | Role |
|---|---|
| `Mode`, `CsiNode` | How the node reaches the channel; one constructor per mode |
| `SessionRole`, `SensingRole` | The node's IEEE 802.11bf roles, read back from the mode |
| `Reporting`, `ReportTo` | Whether, and how often, measurements leave the node, and where to |
| `CsiPacket` → `Envelope` | Format version, node id, session id, source kind, per-run frame counter |
| `CsiPacket` → `CsiFrame` | Normalised `RxMeta`, the `Stimulus` that excited the channel, an optional `HeaderDigest`, and a `CsiPayload` (`EspRaw` tagged with its `LayoutId`, `Grouped`, `Variation`) |
| `wire` | The frame format itself: `no_std`, radio-neutral and free of `esp-*` imports, so hosts decode with the same types |
| `MeasurementSource`, `emit` | Feed frames from any source through the same policy, statistics and delivery |
| `MeasurementSetup` | The node's measurement setup: periodicity, bandwidth, acquisition |
| `wire::MeasurementSetup` | The serialized IEEE 802.11bf-style setup a controller sends (periodicity, bandwidth, responders), mapped onto the mode |
| `RadioProfile` | Seam for custom Wi-Fi bring-up |
| `get_drop_breakdown`, `snapshot_tx_stats` | Drops per cause and per-transmitter counters (`statistics` feature) |

## Features

- **Devices:** ESP32, ESP32-C3, ESP32-C5 (dual-band 2.4/5 GHz), ESP32-C6, ESP32-S3.
- **PHYs:** HT20 and HT40 on every chip; HE20 (802.11ax) on the ESP32-C5 and C6.
- **Output:** postcard/COBS frames, plain text, a compact array format, the ESP32-CSI-Tool CSV
  layout, or `defmt`.
- **On-device processing:** register a `fn(&CsiPacket)` to process CSI inline in the Wi-Fi
  callback.

## Getting started

```toml
[dependencies]
esp-csi-rs = { version = "0.13", features = ["esp32c3", "println"] }
```

The crate uses Rust **edition 2024** and tracks `esp-hal` 1.2, `esp-radio` 1.0 and `esp-rtos` 0.4.
For `defmt` see [`docs/defmt.md`](docs/defmt.md).

A sniffer:

```rust
use esp_csi_rs::{CsiConfig, CsiNode, MeasurementSetup, NodeHardware, WifiSnifferConfig};

let mut node = CsiNode::sniffer(
    WifiSnifferConfig::default().with_channel(7),
    MeasurementSetup::new().acquisition(CsiConfig::default()),
    NodeHardware::new(controller),
);
node.run().await;
```

`controller` is the `WifiController` from `esp_radio::wifi::WifiController::new`; `NodeHardware`
claims its interfaces, sniffer and ESP-NOW handle.

Reading measurements, naming the session and reporting only on change:

```rust
use esp_csi_rs::csi::CsiPacket;
use esp_csi_rs::wire::{self, Bandwidth, StimulusParams};
use esp_csi_rs::{Threshold, WifiSnifferConfig, set_csi_callback, set_session};

fn on_csi(packet: &CsiPacket) {
    let meta = packet.meta();
    let _ = (packet.mac(), meta.timestamp_us, meta.ppdu, packet.csi_data());
}

set_csi_callback(on_csi);
set_session(0x1013, Some(unix_time_us));
let sniffer = WifiSnifferConfig::default().with_threshold(Threshold::new(6000, 200));

// A controller's serialized setup, applied to an ESP-NOW initiator.
let setup = wire::MeasurementSetup::new(1)
    .with_stimulus(StimulusParams::new(10_000, Bandwidth::Mhz20));
initiator_node.apply_measurement_setup(&setup)?;
```

And an emitter for it to measure:

```rust
use esp_csi_rs::{CsiNode, EmitterConfig, EmitterPhy};

let emitter = EmitterConfig::new(7, EmitterPhy::Ht20)
    .with_period(embassy_time::Duration::from_millis(20));
let mut node = CsiNode::emitter(emitter, hardware);
```

## Examples

| Example | What it does |
|---|---|
| `sniffer` | Locks a channel and measures every frame overheard |
| `emitter` | Unassociated sounding at HT20, HT40 or HE20; pair with `sniffer` |
| `station` | Associates to an ESP softAP or a commercial router |
| `access_point` | Self-contained softAP with DHCP; associated stations generate the uplink |
| `esp_now` | The symmetric connectionless pair |
| `esp_now_simplex` | The asymmetric pair — the highest CSI rate of any pairing |
| `csi_callback` | Inline callback vs. queued delivery |
| `runtime_config` | Reconfiguring one node between runs |
| `synthetic_source` | A non-radio `MeasurementSource` feeding 802.11bf-shaped reports through `emit` (`synthetic` feature) |

```sh
cargo esp32c3 --example sniffer          # println
cargo esp32c3-defmt --example sniffer    # defmt
```

Replace `esp32c3` with `esp32`, `esp32c5`, `esp32c6` or `esp32s3`. Measurement harnesses are in
[`experiments/`](experiments/README.md).

## Further reading

| Document | What is in it |
|---|---|
| [`docs/network-model.md`](docs/network-model.md) | The node model and its relation to IEEE 802.11bf |
| [`docs/bandwidth.md`](docs/bandwidth.md) | HT20, HT40 and HE20, and filtering legacy/ACK CSI |
| [`docs/emitter-support.md`](docs/emitter-support.md) | Which transport each chip's emitter uses |
| [`docs/defmt.md`](docs/defmt.md) | Logging backends and using `defmt` |
| [`wire` on docs.rs](https://docs.rs/esp-csi-rs/latest/esp_csi_rs/wire/) | The frame format, subcarrier layouts and measurement setup |
| [docs.rs](https://docs.rs/esp-csi-rs) | Full API documentation |

## License

Copyright 2026 The csi-rs Team. Licensed under either of

- the Apache License, Version 2.0 ([`LICENSE`](LICENSE)), or
- the MIT license ([`LICENSE-MIT`](LICENSE-MIT)),

at your option.
