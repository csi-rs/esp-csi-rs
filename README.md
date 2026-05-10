# `esp-csi-rs`

A Rust crate for collecting **Channel State Information (CSI)** on **ESP32** series devices using the `no-std` embedded framework.

[![crates.io](https://img.shields.io/crates/v/esp_csi_rs.svg)](https://crates.io/crates/esp_csi_rs)
[![docs.rs](https://docs.rs/esp-csi-rs/badge.svg)](https://docs.rs/esp-csi-rs)


> ‼️ **Command Line Interface (CLI) Option**: If you'd like to extract CSI without having to code your own application, there is the CLI wrapper that was created for that purpose. The CLI also gives access to all the features available in this crate. Check out the [`esp-csi-cli-rs`](https://github.com/theembeddedrustacean/esp-csi-cli-rs) repository where you can flash a pre-built binary. This allows you to interact with your board/device immediately wihtout the need to code your own application.


## Overview

`esp_csi_rs` builds on top of Espressif's low-level abstractions to enable easy CSI collection on embedded ESP devices. The crate supports various WiFi modes and network configurations and integrates with the `esp-wifi` and `embassy` async ecosystems.

## Features
### ✅ Device Support
`esp-csi-rs` supports both 2.4 GHz and dual-band ESP devices, including ESP32-C6 (WiFi 6) and ESP32-C5 (WiFi 6 dual-band 2.4/5 GHz). The current list of supported devices is:
- ESP32
- ESP32-C3
- ESP32-C5 (2.4/5 GHz)
- ESP32-C6 (WiFi 6)
- ESP32-S3

### ✅ Host Interface
With exception to the ESP32, `esp-csi-rs` leverages the `USB-JTAG-SERIAL` peripheral available on most recent ESP development boards. This allows for higher baud rates compared to using the UART interface.

### ✅ `defmt` & Serialized Output
`esp-csi-rs` reduces device-to-host transfer overhead by supporting both serialized output and `defmt`. The defmt frames are emitted directly over USB-Serial-JTAG via `esp-println`'s `defmt-espflash` backend — `espflash --monitor --log-format defmt` decodes them inline. `defmt` is a highly efficient logging framework introduced by Ferrous Systems that targets resource-constrained devices. More detail about `defmt` can be found [here](https://defmt.ferrous-systems.com/).

### ✅ Async Logging
By enabling the optional async-print feature, the crate delegates packet serialization and output to an asynchronous driver. This ensures that heavy I/O operations won't block the async executor. Keeping logging non-blocking is critical for maintaining higher throughput and preventing dropped CSI packets.

### ✅ Traffic Generation
When setting up a CSI collection system, dummy traffic on the network is needed to exchange packets that encapsulate the CSI data. `esp-csi-rs` allows you to control the intervals at which traffic is generated.

### ✅ Sequence Number Tags
Traffic carrying collected CSI data are tagged with sequence numbers that triggered the collection. This is useful in star topologies where the traffic generator wants to track the CSI generated with a single broadcast across several stations.

## Node Roles

`esp-csi-rs` defines two types of roles that a node can take in a collection network:

1. **Central Node**: This type of node is one that generates traffic, also can connect to one or more peripheral nodes.
2. **Peripheral Node**: This type of node does not generate traffic, also can optionally connect to one central node at most.

## Node CSI Collection Modes

`esp-csi-rs` defines two types of collection modes:

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
[dependencies]
esp-csi-rs = { version = "0.5.2", features = ["esp32c3", "println"] }
```

The crate uses Rust **edition 2024** and tracks the latest Espressif Rust ecosystem (`esp-hal` 1.1, `esp-radio` 0.18, `esp-rtos` 0.3).

> ‼️ The selected logging framework needs to align with the selected framework for the `esp-backtrace` dependency. The `defmt` feature already pulls the matching `esp-backtrace/defmt`, `esp-hal/defmt`, and `esp-radio/defmt` flags for you.

### Using `defmt` from your application

When enabling the `defmt` feature, the user app needs three additional things on top of the crate dep:

1. **Add `defmt` as a direct dependency** in your own `Cargo.toml`. Our `log_ln!` macro expands to `defmt::println!(...)` at the call site, so the `defmt` crate must be resolvable from your code. Plain `defmt = "1.0"` is enough — do **not** add `defmt-rtt` or any other logger; we already provide one via `esp-println/defmt-espflash`.
2. **Add `-Tdefmt.x` to your linker flags** in your own `.cargo/config.toml` (Cargo doesn't propagate linker args from dependencies' build scripts):
   ```toml
   [target.'cfg(target_arch = "riscv32")']
   rustflags = ["-C", "link-arg=-Tlinkall.x", "-C", "link-arg=-Tdefmt.x"]
   ```
3. **Decode with espflash**: `espflash flash --monitor --log-format defmt <elf>`. No probe-rs / J-Link needed — frames stream over the same USB-Serial-JTAG channel as `println!`.

```toml
[dependencies]
esp-csi-rs = { version = "0.5.2", features = ["esp32c3", "defmt"] }
defmt = "1.0"
```

If you're cribbing from this repo's examples, you don't need any of the above — the in-repo `.cargo/config.toml` aliases (`cargo esp32c3-defmt`, etc.) and `build.rs` handle all three steps automatically.

## Usage Examples

The repository contains an `examples/` folder with configurations for each supported topology. Two flavors of cargo aliases ship in `.cargo/config.toml`:

| Logging | Run alias | Build alias |
|---|---|---|
| `println` (default) | `cargo esp32c3 --example <name>` | `cargo esp32c3-build --example <name>` |
| `defmt` | `cargo esp32c3-defmt --example <name>` | `cargo esp32c3-build-defmt --example <name>` |

Replace `esp32c3` with any of: `esp32`, `esp32c3`, `esp32c5`, `esp32c6`, `esp32s3`. The `-defmt` aliases inject `--features=defmt`, override the espflash runner with `--log-format defmt`, and `build.rs` adds the `-Tdefmt.x` linker script automatically — no manual config edits required to switch between logging backends.

Replace `<name>` with the file name of any example, e.g. `sniffer_wifi`, `esp_now_central`, `wifi_station`.

## Documentation

You can find full documentation on [docs.rs](https://docs.rs/esp_csi_rs).

## Development

This crate is still in early development and currently supports `no-std` only. Contributions and suggestions are welcome!

## License
Copyright 2026 The esp-csi Team

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
