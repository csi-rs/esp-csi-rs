# `esp-csi-rs`

A Rust crate for collecting **Channel State Information (CSI)** on **ESP32** series devices using the `no-std` embedded framework.

[![crates.io](https://img.shields.io/crates/v/esp_csi_rs.svg)](https://crates.io/crates/esp_csi_rs)
[![docs.rs](https://docs.rs/esp-csi-rs/badge.svg)](https://docs.rs/esp-csi-rs)


> ‼️ **Command Line Interface (CLI) Option**: If you'd like to extract CSI without having to code your own application, there is the CLI wrapper that was created for that purpose. The CLI also gives access to all the features available in this crate. Check out the [`esp-csi-cli-rs`](https://github.com/theembeddedrustacean/esp-csi-cli-rs) repository where you can flash a pre-built binary. This allows you to interact with your board/device immediately wihtout the need to code your own application.


## Overview

`esp_csi_rs` builds on top of Espressif's low-level abstractions to enable easy CSI collection on embedded ESP devices. The crate supports various WiFi modes and network configurations and integrates with the `esp-wifi` and `embassy` async ecosystems.

## Features
### ✅ Device Support
`esp-csi-rs` supports both 2.4 GHz and dual-band ESP devices, including ESP32-C5 (dual-band 2.4/5 GHz). The current list of supported devices is:
- ESP32
- ESP32-C3
- ESP32-C5 (2.4/5 GHz)
- ESP32-C6
- ESP32-S3

### ✅ Host Interface
With exception to the ESP32, `esp-csi-rs` leverages the `USB-JTAG-SERIAL` peripheral available on most recent ESP development boards. This allows for higher baud rates compared to using the UART interface.

### ✅ `defmt` & Serialized Output
`esp-csi-rs` reduces device-to-host transfer overhead by supporting both serialized output and `defmt`. The defmt frames are emitted directly over USB-Serial-JTAG via `esp-println`'s `defmt-espflash` backend — `espflash --monitor --log-format defmt` decodes them inline. `defmt` is a highly efficient logging framework introduced by Ferrous Systems that targets resource-constrained devices. More detail about `defmt` can be found [here](https://defmt.ferrous-systems.com/).

### ✅ Async Logging
The crate supports both sync and async logging paths:

- `async-print` **forces async logging** (override mode).
- With `auto` (and without `async-print`), runtime backend selection applies:
  - USB-Serial-JTAG detected -> async logging
  - UART path -> sync logging

This keeps JTAG throughput benefits while preserving UART's low-overhead sync path.

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

`esp-csi-rs` supports five operational modes:

1. **ESP-NOW** — balanced central/peripheral control exchange (auto-pairing, optional forced-PHY unicast replies).
2. **ESP-NOW Fast** — asymmetric one-to-one simplex: collector beacons, source floods; maximizes CSI packets/sec.
3. **WiFi Sniffer** — promiscuous capture on a locked channel (single-node topology).
4. **WiFi Station** — associate to an AP or router and harvest CSI from received frames.
5. **WiFi Access Point** — self-contained softAP collector with built-in DHCP; a `WifiStation` peer associates and generates uplink traffic captured as CSI on the AP.


## Network Architechtures
`esp-csi-rs` allows you to configure a device to one several operational modes including ESP-NOW, wifi station, or sniffer. As such, `esp-csi-rs` supports several network setups allowing for flexibility in collecting of CSI. Some possible setups including the following:

1. ***Single Node:***  This is the simplest setup where only one ESP device (CSI Node) is needed. The node is configured to "sniff" packets in surrounding networks and collect CSI data. The WiFi Sniffer Peripheral Collector is the only possible configuration that supports this topology. 
2. ***Point-to-Point:*** This set up uses two CSI Nodes, a central and a peripheral. One of them can be a collector and the other a listener. Alternatively, both can be collectors as well. Some configuration examples include
    - **WiFi Station Central Collector <-> Access Point/Commercial Router**: In this configuration the CSI node can connect to any WiFi Access Point like an ESP AP or a commercial router. The node in turn sends traffic to the Access Point to acquire CSI data.
    - **WiFi Access Point Central Collector <-> WiFi Station Peripheral Collector**: A self-contained softAP node (`WifiAccessPoint`) runs DHCP and ICMP flood to a paired `WifiStation` node — no external router required. See `wifi_ap` / `wifi_station`.
    - **ESP-NOW Central Listener/Collector <-> ESP-NOW Peripheral Listener/Collector**: In this configuration a CSI central node connects to one other ESP-NOW peripheral node. Both ESP-NOW peripheral and central nodes can operate either as listeners or collectors.
    - **ESP-NOW Fast Collector <-> ESP-NOW Fast Source**: Asymmetric simplex — the source owns all TX airtime while the collector goes RX-only after discovery. See `esp_now_fast_collector` / `esp_now_fast_source`.
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
esp-csi-rs = { version = "0.8.1", features = ["esp32c3", "println"] }
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
esp-csi-rs = { version = "0.8.1", features = ["esp32c3", "defmt"] }
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

Replace `<name>` with the file name of any example, e.g. `sniffer_wifi`, `esp_now_central`, `wifi_station`, `wifi_ap`, `esp_now_fast_collector`.

## WiFi Access Point CSI Collection

Run a **self-contained softAP collector** so a standard `WifiStation` node can
associate without an external router. The AP hands out DHCP leases from a
configurable pool and pings associated clients at a configurable rate; uplink
ICMP replies become CSI on the AP.

```rust
use esp_csi_rs::{CentralOpMode, WifiApConfig, /* ... */};
use esp_radio::wifi::ap::AccessPointConfig;

let ap = AccessPointConfig::default()
    .with_ssid("esp-csi-ap".into())
    .with_auth_method(AuthMethod::None);
let ap_cfg = WifiApConfig::new(ap, 6, None).with_lease_pool(4); // .2–.5
// CSINode::Central(CentralOpMode::WifiAccessPoint(ap_cfg))
```

Defaults: AP `192.168.13.1`, single lease `192.168.13.2`, DHCP enabled. Use
[`WifiApConfig::with_lease_pool`] to support multiple associated stations — each
client gets a distinct address (MAC→IP binding) and the AP round-robins ICMP
downlink across the pool. Tune uplink traffic with `node.set_traffic_freq_hz(...)`
(or the example's ping rate). Pair with `examples/wifi_station.rs` on the same
SSID. Expect tens to low hundreds of CSI pps depending on bidirectional
contention and filter settings — WiFi airtime is the limit, not CPU.

## ESP-NOW Fast Simplex (High Throughput)

For **maximum CSI packets/sec** in a one-to-one link, use the asymmetric fast
modes instead of balanced `EspNow` central/peripheral:

1. **Collector** (`EspNowFastCollector`) broadcasts a sparse ~1 Hz discovery beacon.
2. **Source** (`EspNowFastSource`) learns the collector MAC, registers forced-PHY
   unicast, then floods continuously.
3. Collector **stops beaconing** and goes RX-only — all airtime goes to the source.

```rust
let espnow_cfg = EspNowConfig::fast_default().with_channel(6); // HT20 MCS7-LGI
// Central: CentralOpMode::EspNowFastCollector(espnow_cfg)
// Peripheral: PeripheralOpMode::EspNowFastSource(espnow_cfg)
```

Auto-pairing uses the same magic-prefix protocol as standard ESP-NOW. Chain
`with_ht40` for 40 MHz capture where supported. See `esp_now_fast_collector` /
`esp_now_fast_source`.

## HT40 CSI Collection (ESP32-C5 / C6)

Wide-bandwidth (40 MHz) CSI gives ~2× the subcarriers of HT20 — typically
**~117–128** subcarriers (`csi_data_len / 2`) versus **~56** for HT20 HT-LTF or
**~53** for legacy 20 MHz L-LTF. This section covers how to collect it.

### Key concept: bandwidth is a *per-peer PHY property*

For ESP-NOW, the on-air bandwidth/rate of a frame is set **per peer**
(`esp_now_set_peer_rate_config`), not by the interface bandwidth. So HT40 only
engages when a PHY rate is **forced** on the peer the frame is sent to. Enable it
on the config:

```rust
let espnow_cfg = EspNowConfig::default()
    .with_channel(CHANNEL)
    .with_phy_rate(WifiPhyRate::RateMcs7Lgi) // forced OFDM HT rate (carries HT-LTF)
    .with_ht40(SecondaryChannel::Above);     // 40 MHz; implies force_phy
// node.set_protocol(Protocol::N); node.set_rate(WifiPhyRate::RateMcs7Lgi);
```

`with_ht40` implies `force_phy`. CSI is derived from OFDM training fields, so the
rate **must** be OFDM (an `RateMcsN*` HT rate for HT-LTF, or a legacy-OFDM
6–54 Mbps rate for L-LTF). An 802.11b DSSS rate (`Rate1mL`, `Rate11mL`, …) carries
no training fields and produces **no CSI at all**.

### Topology: Collector + Listener with unicast replies

The proven setup is **central = `CollectionMode::Collector`**, **peripheral =
`CollectionMode::Listener`** with both RX and TX enabled
(`IOTaskConfig::new(true, true)`):

1. The central broadcasts control frames (auto-pairing, no hardcoded MACs).
2. The peripheral receives them, learns the central's MAC, and sends **unicast**
   replies back — applying the forced MCS/HT40 PHY to that learned peer.
3. The central captures wide CSI from those unicast replies.

> **Why unicast (especially on C5):** a per-peer HT40 rate config only applies to
> a *unicast* peer, never the broadcast peer. On ESP32-C5,
> `esp_now_set_peer_rate_config` on the broadcast address wedges the dual-band
> Wi-Fi ISR, so broadcast PHY forcing is skipped there entirely. The peripheral
> therefore unicasts its forced-PHY replies (HT40 always; HT20 too when a PHY rate
> is forced on C5). The central's discovery broadcasts stay at the driver default
> so a peer can boot safely while the central is already running.

### Channel / secondary selection

`with_ht40` takes the HT40 **secondary** channel offset. Pass the IEEE
**primary** channel to `with_channel` (not the wide-channel center label):

| Band | Primary | Secondary | Pair |
|---|---|---|---|
| 5 GHz (C5) | `149` | `SecondaryChannel::Above` | 149 + 153 |
| 2.4 GHz | e.g. `6` | `Above` / `Below` | 6 + 10 / 6 + 2 |

### Per-chip notes

- **ESP32-C5 (dual-band):** validated on **5 GHz channel 149 + 153 HT40**. This
  is the most reliable HT40 path.
- **ESP32-C6 (2.4 GHz only):** HT40 on 2.4 GHz **channel 11 did not bring up the
  central's CSI** in testing, so the C5/C6 examples fall back to **HT20** on C6.
  Other 2.4 GHz HT40 channel pairs may work — verify on-air.
- **C5 boot stability:** dual-band radio bring-up can intermittently wedge the
  Wi-Fi ISR (`handle_interrupts` watchdog reset, or a silent freeze). The library
  inserts short radio-settle delays between C5 reconfiguration steps to reduce
  this, but the most effective measure is to **keep ESP-NOW traffic off the air
  during a node's bring-up** — power the collector up first, then release the peer.

### Filter out legacy / ACK CSI

With the default `CsiConfig`, the radio also reports legacy and control-path CSI
(including ACKs), which in a collector setup can dominate the stats and look
"stuck at ~53 subcarriers" even though HT40 is configured (symptoms: `Subcarriers`
stays ~53, `LastRate` stays legacy, CSI count tracks the control TX rate). For
HT40-focused collection, use an HT40-only CSI filter:

```rust
let csi_cfg = CsiConfig {
    acquire_csi_legacy: 0,
    acquire_csi_ht20: 0,
    acquire_csi_ht40: 1,
    dump_ack_en: 0,
    ..CsiConfig::default()
};
```

### Verify HT40 actually engaged

Check the **central's** captured CSI: a subcarrier count `≥ 100` (commonly ~117)
confirms HT40; ~53/~56 means it fell back to legacy/HT20. The
`esp_now_central_bw_tx` experiment prints a live subcarrier-count histogram for
exactly this check.

### Example matrix

| Example pair | Chip(s) | Band / channel | Bandwidth |
|---|---|---|---|
| `wifi_ap` / `wifi_station` | all supported | 2.4 GHz 6 | HT20 |
| `esp_now_fast_collector` / `esp_now_fast_source` | all supported | 2.4 GHz 6 | HT20 (MCS7) |
| `esp_now_central_ht40` / `esp_now_peripheral_ht40` | C5 / C6 / C3 / S3 / ESP32 | C5: 5 GHz 149+153 · others: 2.4 GHz 6+10 | HT40 |

See `examples/esp_now_central_ht40.rs` for a full working configuration.

## Documentation

You can find full documentation on [docs.rs](https://docs.rs/esp_csi_rs).

## Development

This crate is still in early development and currently supports `no-std` only. Contributions and suggestions are welcome!

## License
Copyright 2026 The csi-rs Team

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
