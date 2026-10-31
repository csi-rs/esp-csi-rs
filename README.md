<p align="center">
  <a href="https://github.com/csi-rs">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/csi-rs/.github/main/brand/svg/csi-rs-lockup-dark-nocaption.svg">
      <img alt="csi-rs" src="https://raw.githubusercontent.com/csi-rs/.github/main/brand/svg/csi-rs-lockup-light-nocaption.svg" width="320">
    </picture>
  </a>
</p>

# `esp-csi-rs`

A Rust crate for collecting **Channel State Information (CSI)** on **ESP32** series devices using
the `no-std` embedded framework.

[![crates.io](https://img.shields.io/crates/v/esp-csi-rs.svg)](https://crates.io/crates/esp-csi-rs)
[![docs.rs](https://docs.rs/esp-csi-rs/badge.svg)](https://docs.rs/esp-csi-rs)

> **Want CSI without writing code?** [`esp-csi-cli-rs`](https://github.com/csi-rs/esp-csi-cli-rs) is
> a CLI wrapper with pre-built binaries that exposes everything this crate does — flash it and talk
> to the board over serial.

## Overview
`esp_csi_rs` builds on top of Espressif's Rust `esp-hal` and `esp-radio` crates' low-level abstractions to enable easy CSI collection on embedded ESP devices. The crate supports various modes and network configurations and integrates with the esp-wifi and embassy async ecosystems.

## Features
- **Device Support**: supports all ESP devices supported by the `esp-hal` and have WiFi or ESP-NOW radio support.
- **Host Support**: Where supported, `esp-csi-rs` leverages the `USB-JTAG-SERIAL` peripheral to allow higher baud rates compared to traditional UART.
- **Multi Output Support:** postcard/COBS frames, plain text, a compact array format, the ESP32-CSI-Tool CSV layout, or `defmt`.
- **Traffic Generation**: supports the generation of different types of traffic and control of traffic rate.
- **Network Architechtures**: supports the creation of several network architechtures & topologies introducing flexibility for different network setups.
- **Multi Mode Support**: supports multiple modes operational modes, including ESP-NOW, Wi-Fi, and raw 802.11 emitter packets. 
- **802.11bf Ready**: is ready for 802.11bf support. 802.11bf devices would integrate as an additional node mode.  
- **802.11bf Collection Model**: adopts the 802.11bf CSI collection and measurement terminology.



## The Collection Network Model

A CSI sensing network deployment is a set of nodes sharing a channel in which traffic (or soundings) excite the channel and at least one node reports a measurement of it. Measurments are taken in sessions and each node in a collection network has a role during that session. 

The measurement node in a collection network can report the CSI locally to the collecting node itself (ex. for extra processing) or deliver it using a transport to a host. Measurements are also captured in measurement sessions. During a session CSI is captured/measured under a certain set of configuration parameters until the session concludes.

The sensing architechture is split in two tiers:
- **The Host Tier (Controller)**: This is the external orchestrator that could be the ESP application, a PC, a Python script, a server, or a host application (like esp-csi-cli-rs over a serial/UART link or network). It sends measurement configurations and collects or analyzes the resulting data. It oversees the experiment or sensing deployment.
- **The Embedded Tier (The Node)**: This is the physical ESP32 device running the firmware. It exists on the channel to actually participate in Wi-Fi sensing by either transmitting sensing PPDUs, measuring incoming frames (calculating CSI), or both. A node is never the system-level controller because the node is the entity being orchestrated.

## The Node Model

Each node is described by four independent attributes, compatible with IEEE 802.11bf's terms wherever the standard has one:

1. **Mode**: defines how does it reach the channel.
2. **Session Role**: defines who is the initiator of a measurement session. A node can be an initiator or a responder. 
3. **Sensing Role**: defines who does what in a sensig session. A node can be a transmitter or a reciever.
4. **Reporting Policy**: defines how the collected measurements are reported.

`esp-csi-rs` defines the following contsrutors for the different modes:

| Constructor | Mode | Session role | Sensing role | Reporting Policy |
|---|---|---|---|---|
| `CsiNode::sniffer` | Wi-Fi sniffer | none | receiver | report, threshold-based or decimate |
| `CsiNode::station` | Wi-Fi station | initiator or none | transmitter-receiver or receiver | any |
| `CsiNode::access_point` | Wi-Fi access point | initiator | receiver | any |
| `CsiNode::emitter` | Raw 802.11 frames | none | transmitter | none |
| `CsiNode::esp_now` | ESP-NOW | initiator or responder | transmitter-receiver | any |
| `CsiNode::esp_now_simplex_source` | ESP-NOW simplex | initiator | transmitter | none |
| `CsiNode::esp_now_simplex_peer` | ESP-NOW simplex | responder | receiver | report |

Below are some of the different network shapes that could be deployed.
<div align="center">

![CSI sensing network shapes](https://raw.githubusercontent.com/csi-rs/esp-csi-rs/main/assets/net-arch.png)

</div>

[`docs/network-model.md`](docs/network-model.md) is the normative description, including how the
model maps onto IEEE 802.11bf.

## System Architecture

The architrecture of the system is split into two types of hardware:
- The collection hardware which is typically the ESP device.
- The processing hardware which can be the ESP itself or a host computer.

The processing hardware typically decodes the reported measurements, processes them and stores or analyzes them.

On the collection hardware the architechure is split into a controller and a data path. The controller lives in the ESP application and defines the measurement setup and parameters. The data path is mostly defined in `esp-csi-rs` and can live completely on the ESP device or split between the ESP and a host.

The data path is split into two parts.
1. **The Capture Path**: This is the callback inside the WiFi task that copies over the recieved CSI data, determines where it will be reported, and places it in a record. 
2. **The Delivery Path**: This is the path that defines how the CSI records will be delivered. 

The figure below visualizes the flow. 

<div align="center">

![Data path from the radio to the host](https://raw.githubusercontent.com/csi-rs/esp-csi-rs/main/assets/data-path.png)

</div>

Some of the crate abstractions are defined in the table below:

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



## Getting started

Make sure to add `esp-csi-rs` to the list of dependencies in `Cargo.toml`:

```toml
[dependencies]
esp-csi-rs = { version = "0.13", features = ["esp32c3", "println"] }
```

### Example: Creating a single node collection network (sniffer)

In this configuration we simply pass the WiFi controller hardware instance and measurement setup to create a sniffer that sniffs packets on channel 7 using the measurement defualts.

In the example, `controller` is the `WifiController` from `esp_radio::wifi::WifiController::new`. `NodeHardware` is an abstraction that claims its interfaces and handles.

```rust
use esp_csi_rs::{CsiConfig, CsiNode, MeasurementSetup, NodeHardware, WifiSnifferConfig};

// Device HAL and Controller instantiation

let mut node = CsiNode::sniffer(
    WifiSnifferConfig::default().with_channel(7),
    MeasurementSetup::new().acquisition(CsiConfig::default()),
    NodeHardware::new(controller),
);
node.run().await;
```


### Example: Creating a point to point collection network emitter

In this configuration we pass the WiFi controller hardware instance and measurement setup to create an emitter that emits packets on channel 7 using the measurement defaults.

And an emitter for it to measure:

```rust
use esp_csi_rs::{CsiNode, EmitterConfig, EmitterPhy};

// Device HAL and Controller instantiation

let emitter = EmitterConfig::new(7, EmitterPhy::Ht20)
    .with_period(embassy_time::Duration::from_millis(20));
let mut node = CsiNode::emitter(emitter,  NodeHardware::new(controller));
```

## Examples

For more examples Refer to the examples directory. Heres a table for the different examples that exist:

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

To build an example run the following:

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
