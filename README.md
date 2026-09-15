# `esp-csi-rs`

A Rust crate for collecting **Channel State Information (CSI)** on **ESP32** series devices using
the `no-std` embedded framework.

[![crates.io](https://img.shields.io/crates/v/esp_csi_rs.svg)](https://crates.io/crates/esp_csi_rs)
[![docs.rs](https://docs.rs/esp-csi-rs/badge.svg)](https://docs.rs/esp-csi-rs)

> **Want CSI without writing code?** [`esp-csi-cli-rs`](https://github.com/csi-rs/esp-csi-cli-rs) is
> a CLI wrapper with pre-built binaries that exposes everything this crate does — flash it and talk
> to the board over serial.

## The node model

A deployment is a **CSI collection network**: nodes sharing a channel and a measurement session, in
which traffic excites the channel and at least one node reports CSI. Each node is described by four
independent attributes.

| Attribute | Values | Question it answers |
|---|---|---|
| Network role | Central, Peripheral | Who sources the traffic? |
| Collection mode | Collector, Listener | Do this node's measurements leave it? |
| Operational mode | six, below | How does it reach the channel? |
| Session role | Initiator, Responder | Who starts and stops the run? |

They are independent because they vary independently — the node that keeps a channel busy and the
node that produces the dataset need not be the same one, and usually are not. Not every mode admits
every combination, and **the ones it does not admit are not offered**: where an attribute is fixed
there is no setter to call, so a central sniffer or a collecting emitter cannot be built.

| Constructor | Operational mode | Network role | Collection mode |
|---|---|---|---|
| `CSINode::sniffer` | Wi-Fi sniffer | peripheral | collector |
| `CSINode::station` | Wi-Fi station | either | either |
| `CSINode::access_point` | Wi-Fi access point | central | either |
| `CSINode::emitter` | Emitter (raw sounding) | central | listener |
| `CSINode::esp_now` | ESP-NOW | either | either |
| `CSINode::esp_now_simplex_source` | ESP-NOW simplex | central | listener |
| `CSINode::esp_now_simplex_peer` | ESP-NOW simplex | peripheral | collector |

The deployment shapes follow from the cardinality rule — at most one central per peripheral, any
number of peripherals per central — rather than from a separate list of topologies:

<div align="center">

![CSI collection network shapes](https://raw.githubusercontent.com/csi-rs/esp-csi-rs/main/assets/net-arch.png)

</div>

**→ [`docs/network-model.md`](docs/network-model.md)** is the normative description, including how
the model relates to IEEE 802.11bf and what the firmware cannot enforce. Every repository in this
ecosystem links to that one file rather than restating it.

## Features

- **Devices:** ESP32, ESP32-C3, ESP32-C5 (dual-band 2.4/5 GHz), ESP32-C6, ESP32-S3.
- **Host interface:** USB-Serial-JTAG on every part except the original ESP32, for higher baud rates
  than UART.
- **Output:** plain text, a compact array format, postcard/COBS serialized frames, the
  ESP32-CSI-Tool CSV layout, or `defmt` binary frames.
- **Traffic generation:** control the rate at which a node puts frames on the channel.
- **Sequence-number tags:** collected CSI carries the sequence number of the frame that triggered
  it, so a collector can measure the gaps — how much of the sounding traffic it actually captured —
  per source MAC when several emitters share a channel.
- **On-device processing:** register a `fn(&CSIDataPacket)` to process CSI inline in the Wi-Fi
  callback, with no host round-trip.

## Getting started

Create an ESP `no-std` project with `esp-generate` (adjust the chip):

```sh
cargo install esp-generate
esp-generate --chip=esp32c3 your-project
```

Add the crate, selecting a device and a logging backend:

```toml
[dependencies]
esp-csi-rs = { version = "0.11", features = ["esp32c3", "println"] }
```

The crate uses Rust **edition 2024** and tracks the latest Espressif Rust ecosystem (`esp-hal` 1.1,
`esp-radio` 0.18, `esp-rtos` 0.3). Using `defmt` instead needs three extra steps in your own project
— see [`docs/defmt.md`](docs/defmt.md).

A sniffer collector, start to finish:

```rust
use esp_csi_rs::{CSINode, CsiConfig, NodeHardware, WifiSnifferConfig};

let mut node = CSINode::sniffer(
    WifiSnifferConfig::default().with_channel(7),
    Some(CsiConfig::default()),
    NodeHardware::new(&mut interfaces, controller),
);
node.run().await;
```

And the emitter to give it something to measure:

```rust
use esp_csi_rs::{CSINode, EmitterConfig, HtBandwidth};

let emitter = EmitterConfig::new(7, HtBandwidth::Ht20)   // same primary channel
    .with_period(embassy_time::Duration::from_millis(20)); // ~50 frames/s
let mut node = CSINode::emitter(emitter, hardware);
```

## Examples

One example per operational mode, each opening with its node's four attributes. The variants that
used to be separate files are `const`s at the top.

| Example | What it does |
|---|---|
| `sniffer` | Peripheral collector — locks a channel and measures every frame overheard |
| `emitter` | Central listener — unassociated sounding at HT20 or HT40; pair with `sniffer` |
| `station` | Associates to an ESP softAP or a commercial router |
| `access_point` | Self-contained softAP with DHCP; associated stations generate the uplink |
| `esp_now` | The symmetric connectionless pair — both roles, both collection modes |
| `esp_now_simplex` | The asymmetric pair — the highest CSI rate of any pairing |
| `csi_callback` | The two CSI delivery paths — inline callback vs. queued |
| `runtime_config` | Reconfiguring one node between runs without reflashing |

Two flavours of cargo alias ship in `.cargo/config.toml`:

| Logging | Run | Build only |
|---|---|---|
| `println` (default) | `cargo esp32c3 --example <name>` | `cargo esp32c3-build --example <name>` |
| `defmt` | `cargo esp32c3-defmt --example <name>` | `cargo esp32c3-build-defmt --example <name>` |

Replace `esp32c3` with any of `esp32`, `esp32c3`, `esp32c5`, `esp32c6`, `esp32s3`. The `-defmt`
aliases add the feature, override the espflash runner and let `build.rs` add the `-Tdefmt.x` linker
script — no config edits needed to switch backends.

Measurement harnesses live under `experiments/`, documented in `experiments/README.md`.

## Further reading

| Document | What is in it |
|---|---|
| [`docs/network-model.md`](docs/network-model.md) | The node model, normatively; relation to IEEE 802.11bf |
| [`docs/bandwidth.md`](docs/bandwidth.md) | HT20 vs HT40, verifying HT40 engaged, filtering legacy/ACK CSI |
| [`docs/emitter-support.md`](docs/emitter-support.md) | Which transport each chip uses, and why raw injection is not offered everywhere |
| [`docs/defmt.md`](docs/defmt.md) | Logging backends, and the three steps to use `defmt` from your own app |
| [docs.rs](https://docs.rs/esp_csi_rs) | Full API documentation |

## Development

Early development, `no-std` only. Contributions and suggestions are welcome.

## License

Copyright 2026 The csi-rs Team. Licensed under the Apache License, Version 2.0 — see
[`LICENSE`](LICENSE).

---

Made with 🦀 for ESP chips
