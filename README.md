# esp csi rs

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
With exception to the ESP32 and the ESP32-C2, `esp-csi-rs` leverages the `USB-JTAG-SERIAL` peripheral available on many recent ESP development boards. This allows for higher baud rates compared to using the traditional UART interface.

### ✅ `defmt`
`esp-csi-rs` reduces device to host transfer overhead further by supporting `defmt`. `defmt` is a highly efficient logging framework introduced by Ferrous Systems that targets resource-constrained devices. More detail about `defmt` can be found [here](https://defmt.ferrous-systems.com/).

### ✅ Traffic Generation
When setting up a CSI collection system, dummy traffic on the network is needed to exchange packets that encapsulate the CSI data. `esp-csi-rs` in turn allows you to generate ICMP traffic. The crate also allows you to control the intervals at which traffic is generated. The crate also supports external traffic triggers. The resulting CSI from an external trigger is sent back as a UDP packet.

### ✅ Port Configuration
Destination and source port numbers can be configured for UDP packets carrying CSI data. This is useful in cases where seperate destination port numbers are required at the CSI recieving end (Ex. Star Topology).

### ✅ Sequence Numbers Tags
External trigger traffic are ICMP echo requests with sequence numbers. As such, UDP traffic carrying collected CSI data are tagged with sequence numbers that triggered the collection. This is useful in star topologies where the traffic generator wants to track the CSI generated with a single broadcast across several stations.


### ✅ NTP Timestamp
The crate supports synchronization with an NTP time server. Afterward, the acquired timestamp is associated with every recieved CSI packet.


## Network Architechtures
`esp-csi-rs` allows you to configure a device to one several modes including access point, station, or sniffer. You would need at least `esp-csi-rs` supports several network setups allowing for flexibility in collection of CSI. Possible architechtures including the following:

1. <span style="color:red">***ESP Sniffer***</span>: This is the simplest setup where only one ESP device is needed. The device is configured to "sniff" packets on surrounding networks and extract CSI data.
2. <span style="color:red">***Commercial Router Connected to an ESP Station***</span>: In this setup only one ESP device is needed as well and is configrued as a Station. The ESP station then connects to a commercial router. The station sends traffic to the commercial router to acquire CSI data. Additionally, setups including a commercial router have the advantage of syncronizing with an NTP time server if needed. 
3. <span style="color:red">***ESP Access Point Connected to an ESP Station***</span>: This setup requires the use of at least two ESP devices, one configured as a Station and one as an Access Point. The station sends traffic to the access point to acquire CSI data. This architechure is also expandable where additional stations can be introduces to connect to the central Access point.
4. <span style="color:red">***Commercial Router Connected to an ESP Access Point/Station Connected to an ESP Station***</span>: This setup is similar to the Access Point setup with the difference that the Access Point + Station can also connect to a commercial router for internet access. This architechure is also expandable where additional stations can be introduces to connect to the central Access point.
5. <span style="color:red">***ESP Access Point Connected to Several ESP Stations***</span>: This setup is the same as 3 except that several stations connect to the access point.
6. <span style="color:red">***Commercial Router Connected to an ESP Access Point/Station Connected to Several ESP Stations***</span>: This setup is the same as 4 except that several stations connect to the access point.


<div align="center">

![Network Architechtures 1](/assets/NetArch1.png)
![Network Architechtures 2](/assets/NetArch2.png)

</div>

## Traffic Generation

Stimulating CSI data requires WiFi channel traffic. This traffic can be artifically generated by `esp-csi-rs`. In non-sniffer networks, CSI in `esp-csi-rs` is also always stimulated at a Station device but can be collected at a Station or Access Point triggering the traffic. Traffic can be generated in two different ways, either locally by the station itself, or remotely by an access point the station is connected to. Traffic roles in `esp-csi-rs` are defined as follows:
1. <span style="color:red">***Trigger***</span>: In this role, the access point or station is the source of traffic and where the CSI data output is collected. In the case of an Access Point trigger, the collected CSI is the CSI recieved from the Station where it was stimulated. Additionally, the traffic generated by the Access Point is ICMP Echo Request broadcasts with an incrementing sequence number.
2. <span style="color:red">***Monitor***</span>: In this role, the access point or station monitor incoming traffic to stimulate CSI collection. In the case of the Station, stimulated CSI is sent back to the access point alongside a sequence number tag in a UDP message.

<div align="center">

![Traffic Generation](/assets/traffic-diagram.png)

</div>

## Getting Started

To use `esp_csi_rs` in your project, create an ESP `no-std` project set up using the `esp-generate` tool (modify the chip/device accordingly):

```sh
cargo install esp-generate
esp-generate --chip=esp32c3 your-project
```

Add the crate to your `Cargo.toml`. At a minimum, you would need to specify the device and the desired logging framework (`println` or `defmt`):

```toml
esp-csi-rs = { version = "0.1.0", features = ["esp32c3", "println"] }
```

> ‼️ The selected logging framework needs to align with the selected framework for the `esp-backtrace` dependency

## Usage Example
This is the simplest example of how this crate can be used. This example follows a sniffer architechture where only one ESP device is needed. This example sets up the ESP to sniff packets of the surrounding networks and print out CSI data to the console.

For more details refer to the crate [documentation](https://docs.rs/esp_csi_rs).

```rust
use esp_csi_rs::{collector::CSISniffer, config::CsiConfig};

// Create a Collector Instance
let mut csi_coll_snif = CSISniffer::new(CsiConfig::default(), controller).await;

// Initialize CSI Collector
csi_coll_snif.init(interfaces, &spawner).await.unwrap();

// Start Collection
csi_coll_snif.start_collection().await;

// Collect for 2 Seconds
with_timeout(Duration::from_secs(2), async {
    loop {
        csi_coll_snif.print_csi_w_metadata().await;
    }
})
.await
.unwrap_err();

// Stop Collection
csi_coll_snif.stop_collection().await;
```

Everytime CSI data is captured, the resulting output looks like this:
```bash
mac: D6:62:A7:DC:DF:7C
rssi: -79
rate: 9
sig_mode: 0
mcs: 0
cwb: 0
smoothing: 0
not sounding: 0
aggregation: 0
stbc: 0
fec coding: 0
sgi: 0
noise floor: 160
ampdu cnt: 0
channel: 1
secondary channel: 1
timestamp: 26123538
ant: 0
sig len: 28
rx state: 0
data length: 128
csi raw data:
[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -12, 9, -13, 8, -13, 6, -13, 4, -12, 2, -10, 2, -7, 2, -6, 2, -3, 3, -1, 4, 1, 6, 2, 8, 2, 10, 3, 11, 6, 13, 6, 14, 4, 14, 2, 15, 1, 14, 1, 13, 2, 11, 2, 8, 3, 4, 4, 0, 6, -4, 6, -5, 0, 0, 10, -11, 12, -11, 13, -12, 13, -12, 10, -11, 7, -12, 5, -12, 4, -11, 1, -11, -2, -11, -2, -11, -3, -11, -3, -10, -3, -8, -4, -5, -6, -3, -7, -1, -8, 0, -12, 2, -14, 4, -16, 3, -18, 1, -20, 0, -18, -2, -15, -4, -13, -5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
```

## More Examples
The repository contains an example folder that contains examples for various device configurations. To run any of the examples enter the following to your command line:
```bash
cargo run --example <example-name>
```
Just replace `example-name` with the file name of any of the examples.

## Documentation

You can find full documentation on [docs.rs](https://docs.rs/esp_csi_rs).

## Development

This crate is still in early development and currently supports `no-std` only. Contributions and suggestions are welcome!

## License
Copyright 2025 The Embedded Rustacean

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
