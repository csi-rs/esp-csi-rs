# Changelog

All notable changes to `esp-csi-rs` are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) (pre-1.0: a minor bump may break).

## [0.12.1]

### Fixed

- **ESP32-C5: the CSI acquisition config now reaches the radio as configured.** esp-radio
  1.0.0-beta.1 packs the C5's acquisition bitfield in the C6's field order. That shifted every flag
  after `acquire_csi_legacy`, ignored `acquire_csi_force_lltf` / `acquire_csi_vht`, and zeroed
  `val_scale_cfg`. An HT-only capture fell back to L-LTF; default and HE-only captures only behaved
  by coincidence. The crate now pre-arranges the fields to compensate. Measured on four C5s:
  HT-only gives HT-LTF (114 bytes), HE-only gives HE-LTF (490 bytes), and forced L-LTF gives L-LTF
  (106 bytes). `dump_ack_en` cannot be set on the C5 until esp-radio is fixed.

## [0.12.0] — unreleased

0.12 makes the crate ready for IEEE 802.11bf without waiting for the hardware. The ESP callback
struct stops being the data contract: frames now follow a versioned, radio-neutral wire format, and
the ESP vendor path becomes one measurement source among several. 0.12 also moves to esp-radio 1.0
and opens 802.11ax HE20 on the ESP32-C5 and C6. See the field map at the end of this entry.

**The serialized wire format changes.** Host decoders written for 0.11 (`csi-webserver-core` 0.2,
`csi-webclient`, `csi-pipeline`) cannot read 0.12 firmware's `LogMode::Serialized` output until they
move to the `wire` types. The text formats are unchanged.

### Added

- **`wire`: the CSI wire contract.** It has no `esp-*` imports, so host tooling can share it.
  - Each frame is an `Envelope` followed by a `Body`.
  - The `Envelope` carries the format version, node id (base MAC), session id, `SourceKind` and a
    per-run `stream_seq`.
  - A `Body` is a `CsiFrame` or a `SessionInfo` announcement.
  - A `CsiFrame` holds:
    - `RxMeta`: normalised metadata, with a 64-bit `timestamp_us`, bandwidth in MHz and a
      `PpduFormat` that includes HE.
    - A `Stimulus`: `Controlled`, `Ambient` or `Observed`.
    - An optional `HeaderDigest`.
    - A tagged `CsiPayload`: `EspRaw`, `Grouped` or `Variation`.
  - `wire::decode_cobs` checks the version before it decodes the body. Every public enum is
    `#[non_exhaustive]`.
- **`wire::layout`: subcarrier layout tables**, transcribed from ESP-IDF's CSI documentation for
  the classic MAC and the ESP32-C5.
  - The device classifies each buffer, and `CsiPayload::EspRaw` carries its `LayoutId`.
  - `LayoutId::segments` / `indices` map a buffer back onto subcarrier indices.
  - A buffer whose length does not match its table entry is `LayoutId::Unknown` rather than
    mis-mapped. The C6 has no published table and is always `Unknown`.
- **Sessions.**
  - `set_session(id, epoch)` names the session and anchors it to wall time. Without it, each run
    draws a random id.
  - The first frame of each run carries a `SessionInfo`.
  - Also added: `session_id()` and `node_id()`.
- **MAC header digest:** frame control, three addresses and sequence control for each measured
  MPDU. It is on by default for the sniffer; toggle it with `set_header_digest_enabled`.
- **`ReportingPolicy`**: `Always`, `Never`, `Threshold(Threshold)` and `Decimate(n)`. Threshold
  reporting gates delivery on channel change, on the device. The sniffer offers `with_threshold` /
  `with_decimation` and has no way to say `Never`.
- **`MeasurementSetup`** (`wire::setup`), in 802.11bf's terms: an acquisition filter, plus optional
  stimulus parameters (periodicity, bandwidth, report type, Ng/Nb, TB/non-TB, responders).
  `CSINode::apply_measurement_setup` maps it onto the mode and refuses what the mode cannot honour.
  Each ESP-NOW control frame is a measurement instance.
- **`MeasurementSource` and `emit`**: any source (synthetic, out-of-tree, a future 802.11bf one)
  delivers through the same gates, policy, envelope, statistics and encoding as the vendor
  callback. `EspVendorSource` names the built-in one.
- **`synthetic` feature:** `wire::synthetic::SyntheticBf`, a deterministic 802.11bf-shaped source
  that emits `Grouped` reports with setup and instance ids.
- **Statistics:**
  - `get_drop_breakdown()` gives one counter per cause.
  - `snapshot_tx_stats()` gives per-transmitter frames, sequence gaps and retries, with LRU
    eviction instead of clearing everyone's history.
- `set_csi_queue_overflow(OverflowPolicy::{DropNewest, DropOldest})` for the async queue.
- Model:
  - `ModeRole` (central, peripheral, source, peer, observer, sounder) and `OperationalMode::role`.
  - `SimplexEnd` is now public, with `SimplexConfig::end`.
  - `SessionRole::Controller`.
- `experiments/hw_check.rs`, a build-time-configured hardware check that emits the `wire` format,
  and `wire-tests`' `wire-decode` binary, which decodes and summarises a raw serial capture.
- `wire-tests/`: host tests that compile the wire and policy sources by path. They run in the new
  `wire-tests.yml` workflow.
- **HE20 (802.11ax SU) on the ESP32-C5 and C6:**
  - `EmitterPhy::He20` for emitters.
  - `EspNowConfig::with_he20` for ESP-NOW. It implies the 802.11ax protocol set, without which the
    driver refuses an HE20 peer rate. Measured on two C5s: full HE-LTF estimates (245 subcarriers)
    in both directions at 100 Hz.
  - `CSINode::set_protocol(Protocol::AX)` for stations, access points and sniffers. `StandardProfile`
    now runs the HE20 bring-up itself.
  - `CsiConfig::he20()`, an HE-LTF-only acquisition preset.
  - `emitter::phy::he20_csi_acquisition`.
- The C5/C6 `CsiConfig` has the HE-LTF acquisition fields again.
- `PeerPhy`, the per-peer ESP-NOW PHY, and `EspNowConfig::peer_phy`.

### Changed

- **Breaking:** `CSIDataPacket` is replaced by `CsiPacket { envelope, frame, session }`. A
  deprecated alias remains, but the fields are new; see the map below. `set_csi_callback` takes
  `fn(&CsiPacket)` and `next_csi_packet` returns `CsiPacket`.
- **Breaking:** `LogMode::Serialized` emits the `wire` format.
- **Breaking:** `RxCSIFmt` is removed. Use `RxMeta::{ppdu, bandwidth, secondary}`, plus the
  payload's `LayoutId`.
- **Breaking:** `CollectionMode` becomes `ReportingPolicy`:
  - `Collector` is `Always` and `Listener` is `Never`. The old name and both constants remain as
    deprecated aliases.
  - `with_collection_mode` / `collection_mode()` are now `with_reporting` / `reporting()`.
  - `runtime_collection_mode` / `set_collection_mode` are now `runtime_reporting` /
    `set_reporting`.
  - Every old name is a deprecated forwarder.
- **Breaking:** `SessionRole` gains `Controller`, the host that starts and stops a run.
  `Initiator` and `Responder` now mean only their 802.11bf roles, and `session_role()` returns
  `Option<SessionRole>`, which is `None` for every mode.
- **Breaking:** `NetworkRole`, `OperationalMode` and the new model enums are `#[non_exhaustive]`.
  Matches outside the crate need a wildcard arm.
- **Breaking:** `ControlPacket.sequence_number` is always on the wire. It was gated on
  `statistics`, so nodes built with different feature sets could not parse each other's frames.
- **Behaviour:** `get_dropped_packets_rx` counts only losses: oversize, queue-full and on-air
  sequence gaps. Filter rejects and policy suppression are no longer drops.
- **Breaking:** esp-radio `=1.0.0-beta.1` (was 0.18), esp-hal 1.2, esp-rtos 0.4, esp-alloc 0.11,
  esp-backtrace 0.20, esp-println 0.18, esp-bootloader-esp-idf 0.6. Applications have to follow:
  - `esp_rtos::start(timer, peripherals.FROM_CPU_INTR0)`.
  - `WifiController::new(peripherals.WIFI, cfg)` replaces `esp_radio::wifi::new`.
  - `StationConfig::with_ssid` takes an `Ssid`, and the password goes inside
    `AuthenticationMethodConfig`.
- **Breaking:** `NodeHardware::new(controller)` claims the interfaces, the sniffer and ESP-NOW
  itself.
- **Breaking:** `HtBandwidth` is renamed to `EmitterPhy` and is `#[non_exhaustive]` (a deprecated
  alias remains). `EmitterConfig::bandwidth` is renamed to `phy`.
- **Breaking:** `set_peer_espnow_phy` / `apply_peer_espnow_phy` take the `EspNow` handle and a
  `PeerPhy`. They now use esp-radio's safe `set_peer_rate`.
- **Behaviour:** the C5/C6 `CsiConfig::default()` enables HE-LTF acquisition, as ESP-IDF does.
- The text log formats print the same columns as 0.11. `timestamp` there is the low 32 bits of
  `timestamp_us`.
- `emitter::run_emitter` is crate-private.

### Fixed

- An ESP-NOW measurement is labelled with the sounding that produced it. The CSI callback fires
  before the ESP-NOW receive path runs, so the instance number is read from the measured frame's
  own payload rather than from the last control frame processed.

- On the C5/C6 the old packet's `second` field held the secondary channel, not a time. The field is
  gone; the secondary channel is `RxMeta::secondary`.
- The receive timestamp no longer wraps after ~71.6 minutes.

### Removed

These were deprecated in 0.11:
- `CentralOpMode`, `PeripheralOpMode`, the root `Node`, `NodeRole`, `CollectorMode` and
  `From<NodeRole> for OperationalMode`.
- `CSINode::{new_role, new_collector, new_emitter, set_role, set_collection_mode,
  set_csi_output_enabled, set_rate}`.

### Field map: `CSIDataPacket` (0.11) to `CsiPacket` (0.12)

| 0.11 | 0.12 |
|---|---|
| `mac` | `frame.transmitter()` / `CsiPacket::mac()` |
| `rssi`, `noise_floor`, `channel`, `sig_len`, `rx_state` | `frame.meta.*` (narrower integer types) |
| `timestamp: u32` | `frame.meta.timestamp_us: u64` |
| `sequence_number` | `frame.meta.frame_seq` (802.11 sequence number); `envelope.stream_seq` counts delivered frames |
| `bandwidth`, `secondary_channel`, `sig_mode`, `data_format` | `frame.meta.bandwidth` / `secondary` / `ppdu`, plus `LayoutId` |
| `mcs`, `stbc`, `sgi`, `antenna`, `not_sounding`, `aggregation` | `frame.meta.*` as `Option`s (`None` on C5/C6) |
| `rate`, `smoothing`, `fec_coding`, `ampdu_cnt` | `VendorRx::EspClassic` |
| `cur_bb_format`, `rx_channel_estimate_*`, `dump_len`, `is_group`, `rxend_state`, `rxmatch*`, `sigb_len`, `cur_single_mpdu` | `VendorRx::EspHe` |
| `csi_data`, `csi_data_len` | `CsiPayload::EspRaw { bytes, layout, chip, first_word_invalid }`, or `CsiPacket::csi_data()` |
| `date_time` (never set) | removed; `SessionInfo::epoch_unix_us` |

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

[0.12.1]: https://github.com/csi-rs/esp-csi-rs/compare/0.12.0...HEAD
[0.12.0]: https://github.com/csi-rs/esp-csi-rs/compare/d860363...0.12.0
[0.11.0]: https://github.com/csi-rs/esp-csi-rs/compare/d3dbd70...d860363
[0.10.1]: https://github.com/csi-rs/esp-csi-rs/commit/d3dbd70
