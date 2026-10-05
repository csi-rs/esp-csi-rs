# Experiments

Measurement and characterization harnesses for `esp-csi-rs`, kept out of `examples/` so the
canonical examples stay uncluttered.

These are **not** usage examples. To learn the API, start with the one-example-per-mode set in
[`../examples/`](../examples) (`sniffer`, `emitter`, `station`, `access_point`, `esp_now`,
`esp_now_simplex`, `csi_callback`, `runtime_config`).

## What's here

Three subjects, one per operational mode that the specs in `specs/` measure. Each subject is a
full harness plus a `_min` footprint floor.

| Target | Subject | What it reports | Needs |
|---|---|---|---|
| `esp_now_bench` | Symmetric ESP-NOW, either end (`NETWORK_ROLE`) | `MEASURE`: `Throughput`, `Heap`, `Drop`, `Quiet` (power), `Cpu` | `statistics`; `cpu-test-tx` for `Cpu` |
| `esp_now_bench_min` | ESP-NOW platform floor, no `CSINode` | nothing — built and measured, not run | — |
| `hw_check` | Any mode, configured by `HW_*` environment variables at build time | the `wire` format, decoded with `wire-tests`' `wire-decode` | `statistics` |
| `sniffer_bench` | Wi-Fi sniffer on `CHANNEL` | `MEASURE`: `Throughput`, `Heap`, `Quiet`; `LOG_MODE` is a measured variable too | `statistics` |
| `sniffer_bench_min` | Sniffer platform floor, no `CSINode` | nothing — built and measured, not run | — |
| `station_bench` | Wi-Fi station pinging its gateway at `PING_RATE_HZ` | `MEASURE`: `Throughput`, `Heap`, `Quiet` (power) | `statistics` |
| `station_bench_min` | Station platform floor, no `CSINode`, no IP stack | nothing — built and measured, not run | — |

Every harness is configured by the `const`s at the top of its file. `esp_now_bench` and
`station_bench` run their node as a **listener**, because a measurement of acquisition cost should
not be paying delivery cost; a sniffer is always a collector, so `sniffer_bench` makes the output
format (`LOG_MODE`) one of its variables instead.

`cpu_test_schedule.rs` is a **shared module**, not a target. `esp_now_bench` includes it with
`#[path]` under `cpu-test-tx`, so it and the device under test walk an identical phase schedule
with no control channel between them.

## Binary footprint

The footprint number is `full − min`: the `_min` half performs the same platform and radio
bring-up with no `CSINode` in it. **Build both halves with the same features**, or the difference
includes whatever the features linked, and record the feature set alongside the number.

## Running

Each file is registered in `Cargo.toml` as a Cargo example with an explicit `path` and its
`required-features`, so it builds through the same per-chip aliases as anything in `examples/`
(defined in `.cargo/config.toml`):

```bash
cargo esp32c6-build --example esp_now_bench_min                                  # build only
cargo esp32c6       --example esp_now_bench --features=statistics                # flash + monitor
cargo esp32c6-build --example esp_now_bench --features=statistics,cpu-test-tx    # MEASURE = Cpu
cargo esp32c6       --example sniffer_bench --features=statistics
cargo esp32c6       --example station_bench --features=statistics
```

Replace `esp32c6` with any of `esp32`, `esp32c3`, `esp32c5`, `esp32c6`, `esp32s3`; the `-defmt`
aliases work here too.

## Feature flags

| Feature | Used by |
|---|---|
| `statistics` | Required by the three full harnesses (`required-features`); they read the `get_total_*` / `get_pps_*` counters. |
| `cpu-test-tx` | `esp_now_bench` with `MEASURE = Cpu`: exposes the rate / payload / pause controls the schedule steers the real TX loop with. |
| `cpu-trace` | No in-tree target. It enables esp-rtos scheduler tracing for a CPU-utilization device under test that installs its own `rtos_trace` sink. |

## `hw_check` and `wire-decode`

`hw_check` is one firmware for checking the wire contract on hardware. Pick the mode and its
settings with `HW_MODE`, `HW_CH`, `HW_PHY`, `HW_REPORT`, `HW_CSI`, `HW_PROTO`, `HW_HZ`, `HW_SETUP`,
`HW_LOG` and `HW_EPOCH` (the table is at the top of the file). It logs `LogMode::Serialized` by
default and prints a framed `STATS` line every five seconds.

```bash
HW_MODE=emitter HW_PHY=he20 cargo esp32c5-build --example hw_check --features=statistics
HW_MODE=sniffer HW_CSI=he20 HW_PROTO=ax cargo esp32c5-build --example hw_check --features=statistics
```

Record the serial port raw, for example with pyserial, then summarise the capture on the host. The
summary covers frames, sessions, `stream_seq` gaps, timestamp monotonicity, PPDU formats, layouts,
stimulus, header digests and the device's `STATS` lines:

```bash
cargo +stable run --release --manifest-path wire-tests/Cargo.toml --bin wire-decode -- capture.bin
```
