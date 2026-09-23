# Changelog

All notable changes to `esp-csi-rs` are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) (pre-1.0: a minor bump may break).

## [0.11.0]

0.11 replaces the node taxonomy with the four-attribute node model: network role
(central/peripheral), collection mode (collector/listener), operational mode, and session role
(initiator/responder). [`docs/network-model.md`](docs/network-model.md) is the normative
description. The 0.10 surface is kept as a deprecated shim, so code written against 0.10 compiles
with warnings. The shim is removed in 0.12.

### Added

- The model's types: `NetworkRole`, `CollectionMode`, `OperationalMode`, `SessionRole`,
  `SimplexConfig` and `NodeView`, in `esp_csi_rs::model` and at the crate root.
- One constructor per operational mode: `CSINode::sniffer`, `::station`, `::access_point`,
  `::emitter`, `::esp_now`, `::esp_now_simplex_source` and `::esp_now_simplex_peer`. Each mode
  offers only the attributes it admits, so a configuration the model rules out, such as a central
  sniffer or a collecting emitter, cannot be built.
- `with_network_role` / `with_collection_mode` on `EspNowConfig` and `WifiStationConfig`, and
  `with_collection_mode` on `WifiApConfig`. The sniffer and emitter configs expose `const`
  accessors and no setters.
- `CSINode::set_operational_mode`, plus read-backs `operational_mode`, `network_role`,
  `collection_mode` and `session_role`.
- `runtime_collection_mode()` returns the collection mode currently in force. It can differ from
  the configured one: a peripheral paired with a listening central promotes itself to collector.
- `set_collection_mode()`, a free function for out-of-tree nodes that drive the radio themselves
  and never pass through `CSINode::run`. They must state their collection mode here, or the
  process-wide gate keeps whatever the previous run left.
- `LICENSE-MIT`, so the `MIT OR Apache-2.0` licence declared in `Cargo.toml` ships both texts.

### Changed

- **Breaking:** `CSINode::new` takes an `OperationalMode`. The old `NodeRole` form is
  `CSINode::new_role`, which is deprecated.
- **Breaking:** the ESP-NOW simplex ends swap sides. The flooding end (was
  `PeripheralOpMode::EspNowFastSource`) is now the **central listener**, and the receive-only end
  (was `CentralOpMode::EspNowFastCollector`) is now the **peripheral collector**. The network role
  follows the traffic, as it does in every other mode. What either node does on air is unchanged.
  `From<NodeRole> for OperationalMode` applies the correction, so unmigrated callers get the fixed
  roles.
- **Breaking:** `RadioProfile::wants_bringup` and `::tune_protocols` take a `NodeView` instead of
  `&NodeRole`. `NodeView` is `#[non_exhaustive]`, so later additions are not breaking.
- **Breaking:** a node configured as `CollectionMode::Listener` now stops delivering CSI. It keeps
  capturing. Before 0.11 a listener kept delivering. See "Fixed".
- **Breaking:** the examples are consolidated from 18 to 8, one per operational mode plus
  `csi_callback` and `runtime_config`. `experiments/` goes from 27 files to 7: six harnesses plus
  the shared `cpu_test_schedule.rs` module. In all, 45 files become 15 (14 build targets). Old
  names such as `ht20_emitter`, `wifi_station` and `esp_now_central` are gone; see
  [`experiments/README.md`](experiments/README.md) for the harness map.
- The ESP-NOW central unicasts its control traffic to the peripheral once it has learned exactly
  one. With several peripherals it stays on broadcast. See "Fixed".

### Deprecated

All of these are removed in 0.12.

- `NodeRole` and `CollectorMode`. Use `OperationalMode` with `NetworkRole` / `CollectionMode`.
- `CentralOpMode`, `PeripheralOpMode` and `esp_csi_rs::Node`. Use `OperationalMode::EspNow` /
  `OperationalMode::EspNowSimplex`. This spelling put the simplex ends the wrong way round.
- `node::Node`, an alias of `NodeRole`.
- The `CSINode` shims `new_role`, `new_collector`, `new_emitter`, `set_role` and
  `set_collection_mode`. Use the per-mode constructors, `set_operational_mode`, and the mode
  config's `with_collection_mode`.
- `CSINode::set_csi_output_enabled` and `CSINode::set_rate`. Both are now documented as the no-ops
  they always were. For the first, use the free function `esp_csi_rs::set_csi_output_enabled` at
  runtime, or `CollectionMode::Listener` on the config. For the second, use
  `EspNowConfig::with_phy_rate`.

### Removed

- **Breaking:** `CentralOpMode::WifiStation`, `CentralOpMode::WifiAccessPoint` and
  `PeripheralOpMode::WifiSniffer`. They could be constructed but did nothing, since `run()` only
  logged a message for them. Use `CSINode::station` / `::access_point` / `::sniffer`.

### Fixed

- `CollectionMode::Listener` now gates delivery. Before, the free function
  `set_csi_output_enabled(false)` stored a flag that no CSI path read, so `set-csi-output
  --enabled=false` on the CLI and `POST /config/csi-output` on the HTTP API reported success and
  kept delivering. The publish gate now requires three things: a consumer is installed, the node is
  a collector, and output is enabled. The free function is the runtime delivery gate. It defaults
  to on and is restored between runs.
- The symmetric ESP-NOW peripheral now captures CSI. The central used to broadcast forever, and
  broadcast frames never reach the Wi-Fi CSI callback. So the peripheral decoded and answered every
  control packet while measuring nothing. Measured on two ESP32-C5s, the peripheral's RX went from
  0 to 1744.
- In `LogMode::Serialized`, log lines no longer destroy the CSI frame that follows them. Text
  written into the COBS stream is now delimited into a frame of its own, instead of merging with
  the next packet. This covers `log_ln!`, boot banners and `log_line`.
- `cargo check --examples` passes on every feature set. The harness registrations now carry
  `required-features`.

## [0.10.1]

### Fixed

- The sync UART writer compiles under `auto` + `async-print`, and the docs CI job is fixed.

[0.11.0]: https://github.com/csi-rs/esp-csi-rs/compare/d3dbd70...HEAD
[0.10.1]: https://github.com/csi-rs/esp-csi-rs/commit/d3dbd70
