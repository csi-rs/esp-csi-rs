# Experiments

Measurement and experimental firmware for `esp-csi-rs` — benchmarking and
characterization harnesses kept out of `examples/` so the canonical examples
stay uncluttered.

These are **not** usage examples. For learning the API, start with the
canonical set in [`../examples/`](../examples) (`ht20_emitter`,
`collector_sniffer`, `sniffer_wifi`, `wifi_station`, `wifi_ap`,
`runtime_config`, `csi_callback_test`).

## Running

Each file is registered in the workspace `Cargo.toml` as a Cargo example with
an explicit `path`, so it builds and runs through the same per-chip aliases as
anything in `examples/` (defined in `.cargo/config.toml`):

```bash
cargo esp32c3        --example sniffer_wifi_min             # flash + monitor
cargo esp32c3-build  --example sniffer_wifi_min             # build only
cargo esp32s3        --example wifi_station_power            # other chips: esp32, esp32c3, esp32c5, esp32c6, esp32s3
cargo esp32c3-defmt  --example sniffer_wifi_exper_heap       # defmt logging variant
```

Some experiments need extra cargo features (see the table below):

```bash
cargo esp32c3 --example sniffer_wifi_exper       --features statistics
cargo esp32c3 --example wifi_station_heap        --features statistics
```

`cpu_test_schedule.rs` is a **shared module**, not a runnable example — the
CPU-utilization firmwares pull it in with `#[path = "cpu_test_schedule.rs"]`,
so it is intentionally not registered as a Cargo target.

## What's here

The ESP-NOW harnesses that used to live here went away with the ESP-NOW transport
(see the emitter/collector migration). What remains is the transport-agnostic set;
the emitter-side equivalents still need rebuilding against `NodeRole::Emitter`.

### Footprint floor (`*_min`)
Platform-floor builds that bring up the radio path **without** `CSINode`, to
measure baseline flash/RAM cost: `sniffer_wifi_min`, `wifi_station_min`.

### Power
- `wifi_station_power` — STA→AP power DUT (`staap_active` scenario).

### Heap usage
`sniffer_wifi_exper_heap`, `wifi_station_heap` — track allocator high-water marks.

### CPU utilization (spec v2)
- `cpu_test_schedule.rs` — shared phase schedule (module include, not a target).
- The `cpu-test-tx` hooks now steer the emitter inject loop
  (`esp_csi_rs::emitter::run_emitter`) rather than the removed ESP-NOW central TX
  loop, so a companion TX experiment can be rebuilt on top of an emitter node.

### Experimental / scratch (`*_exper`)
Full-feature experimentation variant of the canonical sniffer example:
`sniffer_wifi_exper`.

Sniffer-specific variants:
- `sniffer_wifi_exper_esp_csi_tool` — emits CSI in ESP32-CSI-Tool CSV format.
- `sniffer_wifi_exper_logmode_cycle` — cycles through every `LogMode` once per minute.

## Feature flags

The aliases pass only the chip feature by default; add these where required:

| Experiment | Extra features |
|---|---|
| `sniffer_wifi_exper` | `statistics` |
| `sniffer_wifi_exper_heap` | `statistics` |
| `wifi_station_heap` | `statistics` |
| `wifi_station_power` | `statistics` |

All other experiments run with just the chip feature. When in doubt, the
authoritative feature set for an experiment is whatever its header doc comment
and `use esp_csi_rs::…` imports require (e.g. anything pulling `get_total_*` /
`get_pps_*` needs `statistics`).

> Note: `cpu-trace` firmware only builds for chips whose toolchain accepts the
> scheduler-hook asm; `esp32c3` currently fails to assemble it. That is a property
> of the feature, not of the directory layout.
