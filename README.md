# `esp-csi-rs`

A Rust crate for collecting **Channel State Information (CSI)** on **ESP32** series devices using the `no-std` embedded framework.

[![crates.io](https://img.shields.io/crates/v/esp_csi_rs.svg)](https://crates.io/crates/esp_csi_rs)
[![docs.rs](https://docs.rs/esp-csi-rs/badge.svg)](https://docs.rs/esp-csi-rs)


> ‼️ **Command Line Interface (CLI) Option**: If you'd like to extract CSI without having to code your own application, there is the CLI wrapper that was created for that purpose. The CLI also gives access to all the features available in this crate. Check out the [`esp-csi-cli-rs`](https://github.com/theembeddedrustacean/esp-csi-cli-rs) repository where you can flash a pre-built binary. This allows you to interact with your board/device immediately wihtout the need to code your own application.


## Overview

`esp_csi_rs` builds on top of Espressif's low-level abstractions to enable easy CSI collection on embedded ESP devices. The crate supports various WiFi modes and network configurations and integrates with the `esp-wifi` and `embassy` async ecosystems.

## Features
### ✅ Device Support
`esp-csi-rs` supports several ESP devices including the ESP32-C6 which supports WiFi 6. The current list of supported devices are:
- ESP32
- ESP32-C2
- ESP32-C3
- ESP32-C6
- ESP32-S3

### ✅ Host Interface
With exception to the ESP32 and the ESP32-C2, `esp-csi-rs` leverages the `USB-JTAG-SERIAL` peripheral available on many recent ESP development boards. This allows for higher baud rates compared to using the UART interface.

### ✅ `defmt` & Serialized Output
`esp-csi-rs` reduces device to host transfer overhead further by supporting both serialized output and `defmt`. This allows for better CSI throughput when communicating the output to a host device. `defmt` is a highly efficient logging framework introduced by Ferrous Systems that targets resource-constrained devices. More detail about `defmt` can be found [here](https://defmt.ferrous-systems.com/).

### ✅ Async Logging
By enabling the optional async-print feature, the crate delegates packet serialization and output to an asynchronous driver. This ensures that heavy I/O operations won't block the async executor. Keeping logging non-blocking is critical for maintaining higher throughput and preventing dropped CSI packets.

### ✅ Traffic Generation
When setting up a CSI collection system, dummy traffic on the network is needed to exchange packets that encapsulate the CSI data. `esp-csi-rs` allows you to control the intervals at which traffic is generated.

### ✅ Sequence Number Tags
Traffic carrying collected CSI data are tagged with sequence numbers that triggered the collection. This is useful in star topologies where the traffic generator wants to track the CSI generated with a single broadcast across several stations.

## Node Roles

`esp-cs-rs` defines two types of roles that a node can take in a collection network:

1. **Central Node**: This type of node is one that generates traffic, also can connect to one or more peripheral nodes.
2. **Peripheral Node**: This type of node does not generate traffic, also can optionally connect to one central node at most.

## Node CSI Collection Modes

`esp-cs-rs` defines two types of collection modes:

1. **Collector**: A collector node collects and provides CSI data output from one or more devices.
2. **Listener**: A listener is a passive node. It only enables CSI collection and does not provide any CSI output.

## Node Operation Modes

`esp-csi-rs` supports three operational modes:

1. ESP-NOW
2. WiFi Sniffer
3. WiFi Station


## Network Architechtures
`esp-csi-rs` allows you to configure a device to one several operational modes including ESP-NOW, wifi station, or sniffer. As such, `esp-csi-rs` supports several network setups allowing for flexibility in collecting of CSI. Some possible setups including the following:

1. ***Single Node:***  This is the simplest setup where only one ESP device (CSI Node) is needed. The node is configured to "sniff" packets in surrounding networks and collect CSI data. The WiFi Sniffer Peripheral Collector is the only possible configuration that supports this topology. 
2. ***Point-to-Point:*** This set up uses two CSI Nodes, a central and a peripheral. One of them can be a collector and the other a listener. Alternatively, both can be collectors as well. Some configuration examples include
    - **WiFi Station Central Collector <-> Access Point/Commercial Router**: In this configuration the CSI node can connect to any WiFi Access Point like an ESP AP or a commercial router. The node in turn sends traffic to the Access Point to acquire CSI data.
    - **ESP-NOW Central Listener/Collector <-> ESP-NOW Peripheral Listener/Collector**: In this configuration a CSI central node connects to one other ESP-NOW peripheral node. Both ESP-NOW peripheral and central nodes can operate either as listeners or collectors.
3. ***Star:*** In this architechture a central node connects to several peripheral nodes. The central node triggers traffic and aggregates CSI sent back from peripheral nodes. Alternatively, CSI can be collected by the individual peripherals. Only the ESP-NOW operation mode supports this architechture. The ESP-NOW peripheral and central nodes can also operate either as listeners or collectors. 

<div align="center">

![Network Architechtures](/assets/net-arch.png)

</div>

## Getting Started

To use `esp_csi_rs` in your project, create an ESP `no-std` project set up using the `esp-generate` tool (modify the chip/device accordingly):

```sh
cargo install esp-generate
esp-generate --chip=esp32c3 your-project
```

Add the crate to your `Cargo.toml`. At a minimum, you would need to specify the device and the desired logging framework (`println` or `defmt`):

```toml
esp-csi-rs = { version = "0.3.0", features = ["esp32c3", "println"] }
```

> ‼️ The selected logging framework needs to align with the selected framework for the `esp-backtrace` dependency

## Usage Examples
The repository contains an example folder that contains examples for various device configurations. To run any of the examples enter the following to your command line:
```bash
cargo esp32s3 --example <example-name>
```
Just replace `example-name` with the file name of any of the examples.

## Documentation

You can find full documentation on [docs.rs](https://docs.rs/esp_csi_rs).

## Development

This crate is still in early development and currently supports `no-std` only. Contributions and suggestions are welcome!

## License
Copyright 2026 The Embedded Rustacean

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at
http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.

---

Made with 🦀 for ESP chips
