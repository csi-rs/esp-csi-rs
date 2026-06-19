# Experiments

Measurement and experimental firmware for `esp-csi-rs` — benchmarking and
characterization harnesses kept out of `examples/` so the canonical examples
stay uncluttered.

These are **not** usage examples. For learning the API, start with the
canonical set in [`../examples/`](../examples) (`esp_now_central`,
`esp_now_peripheral`, `sniffer_wifi`, `wifi_station`, `runtime_config`,
`csi_callback_test`).

## Running

Each file is registered in the workspace `Cargo.toml` as a Cargo example with
an explicit `path`, so it builds and runs through the same per-chip aliases as
anything in `examples/` (defined in `.cargo/config.toml`):

```bash
cargo esp32c3        --example esp_now_central_min          # flash + monitor
cargo esp32c3-build  --example esp_now_central_min          # build only
cargo esp32s3        --example wifi_station_power            # other chips: esp32, esp32c3, esp32c5, esp32c6, esp32s3
cargo esp32c3-defmt  --example sniffer_wifi_exper_heap       # defmt logging variant
```

Some experiments need extra cargo features (see the table below):

```bash
cargo esp32c3 --example esp_now_central_exper        --features statistics
cargo esp32c3 --example esp_now_central_exper_cpu_tx --features cpu-test-tx,statistics
cargo esp32c3 --example esp_now_peripheral_exper_cpu --features cpu-trace,async-print
```

`cpu_test_schedule.rs` is a **shared module**, not a runnable example — the
CPU-utilization firmwares pull it in with `#[path = "cpu_test_schedule.rs"]`,
so it is intentionally not registered as a Cargo target.

## What's here

### Footprint floor (`*_min`)
Platform-floor builds that bring up the radio path **without** `CSINode`, to
measure baseline flash/RAM cost: `esp_now_central_min`,
`esp_now_peripheral_min`, `sniffer_wifi_min`, `wifi_station_min`.

### Power
- `esp_now_central_power` — power DUT via the full `CSINode` state machine.
- `esp_now_central_min_power` — power DUT via the raw `EspNow` path (no `CSINode`).
- `wifi_station_power` — STA→AP power DUT (`staap_active` scenario).

### Heap usage
`esp_now_central_exper_heap`, `esp_now_peripheral_exper_heap`,
`sniffer_wifi_exper_heap`, `wifi_station_heap` — track allocator high-water marks.

### Packet drop
- `esp_now_central_drop` / `esp_now_peripheral_drop` — TX / RX sides, full `CSINode`.
- `esp_now_central_min_drop` / `esp_now_peripheral_min_drop` — TX / RX sides, raw ESP-NOW.

### Bandwidth / PHY
- `esp_now_central_bw_tx` — continuous MCS0-LGI / HT40 broadcaster (companion TX).
- `esp_now_peripheral_bw_check` — bandwidth / PHY diagnostic for the ESP-NOW CSI path.

### CPU utilization (spec v2)
- `esp_now_peripheral_exper_cpu` — DUT, full-library variant.
- `esp_now_peripheral_min_cpu` — DUT, minimal like-for-like variant.
- `esp_now_central_exper_cpu_tx` — companion TX, full-library (steers the real
  `run_esp_now_central` loop via the `cpu-test-tx` hooks).
- `esp_now_central_min_cpu_tx` — companion TX, minimal raw-ESP-NOW variant.
- `cpu_test_schedule.rs` — shared phase schedule (module include, not a target).

### Experimental / scratch (`*_exper`)
Full-feature experimentation variants of the canonical examples:
`esp_now_central_exper`, `esp_now_peripheral_exper`, `sniffer_wifi_exper`.

Sniffer-specific variants:
- `sniffer_wifi_exper_esp_csi_tool` — emits CSI in ESP32-CSI-Tool CSV format.
- `sniffer_wifi_exper_logmode_cycle` — cycles through every `LogMode` once per minute.

## Feature flags

The aliases pass only the chip feature by default; add these where required:

| Experiment | Extra features |
|---|---|
| `esp_now_central_exper` | `statistics` |
| `esp_now_peripheral_exper` | `statistics` |
| `esp_now_central_drop` | `cpu-test-tx`, `statistics` |
| `esp_now_peripheral_drop` | `statistics` |
| `esp_now_central_exper_cpu_tx` | `cpu-test-tx`, `statistics` |
| `esp_now_peripheral_exper_cpu` | `cpu-trace` (and `async-print` for the timing path) |
| `esp_now_peripheral_min_cpu` | `cpu-trace` |

All other experiments run with just the chip feature. When in doubt, the
authoritative feature set for an experiment is whatever its header doc comment
and `use esp_csi_rs::…` imports require (e.g. anything pulling `get_total_*` /
`get_pps_*` needs `statistics`).

> Note: `esp_now_peripheral_min_cpu` (and the other `cpu-trace` firmware) target
> chips whose toolchain accepts the `cpu-trace` scheduler-hook asm; building it
> for `esp32c3` currently fails to assemble. This is a property of the
> experiment/feature, not of the directory layout.
