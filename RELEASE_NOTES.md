# Release Notes

## v0.7.3

**HT40 CSI collection** over ESP-NOW across chips — ESP32-C5 on 5 GHz and
ESP32 / ESP32-S3 / ESP32-C3 / ESP32-C6 on 2.4 GHz — **automatic unicast
forced-PHY pairing** that makes forced-rate CSI work on C5, a
**runtime-selectable logging backend**, and several CSI/PHY correctness fixes.

No public API was removed — this is a backward-compatible patch release.

### Highlights

- **HT40 (40 MHz) CSI** end-to-end: ~117–128 subcarriers vs ~56 (HT20) / ~53
  (legacy 20 MHz).
- **Automatic pairing → unicast replies**: the peripheral learns the central's
  MAC from its broadcast and unicasts forced-PHY replies back — no hardcoded
  MAC addresses. This is what enables forced-rate CSI on C5 (where forcing the
  PHY on the broadcast peer is unsafe).
- **Runtime logging backend**: auto-selects async (USB-Serial-JTAG) vs inline
  sync (UART) at boot, with per-format CSI output modes.

### Added

- **HT40 CSI collection** (`with_ht40(SecondaryChannel)` ⇒ forces the per-peer
  PHY): 40 MHz capture on C5 (5 GHz, 149 + 153) and ESP32 / S3 / C3 / C6
  (2.4 GHz, 6 + 10).
- **Automatic unicast forced-PHY replies** on the peripheral: the responder
  learns the central peer from auto-pairing and unicasts forced-rate replies.
  Applies to HT40 on all chips, and to HT20 with a forced rate on C5.
- **Per-peer ESP-NOW PHY forcing** (`esp_now_set_peer_rate_config` binding):
  `set_peer_espnow_phy` / `apply_peer_espnow_phy`, plus dual-band bring-up
  helpers (band selection, HT40 channel/bandwidth, started-STA bring-up).
- **Runtime-selectable logging backend** with `auto` (USB SOF ⇒ async drain,
  UART ⇒ inline sync) and explicit `async-print` / `jtag-serial` overrides;
  async `LogMode` variants (`Serialized`, `Text`, `ArrayList`, `EspCsiTool`);
  `is_async_logging_active()`; `external-defmt-logger` feature.
- **ESP-NOW diagnostics** (`statistics` feature): TX queued/confirmed/failed
  counters; peripheral RX control-packet / magic-drop / parse-fail / sequence-
  miss counters; RX/TX rate and PPS getters.
- **New examples**: `esp_now_{central,peripheral}_ht40` — multi-chip HT40 with
  auto-pairing unicast replies. C5 runs 5 GHz (149+153); ESP32 / S3 / C3 / C6
  run 2.4 GHz (6+10), band/channel and per-chip `CsiConfig` selected at build time.
- **Subcarrier-count histogram** output in the `esp_now_central_bw_tx`
  experiment (bins CSI reports by `csi_data_len / 2` instead of dumping packets).
- **Per-chip `CsiConfig`** (C5 adds `acquire_csi_force_lltf` / `acquire_csi_vht`;
  C5/C6 expose the `acquire_csi_*` acquisition fields).
- Host-side tooling under `python/` (serial → parquet decode) and `scripts/`.

### Changed

- **C5 dual-band boot hardening**: ESP-NOW receive dispatch is suspended across
  every controller mutation, and short radio-settle delays are inserted between
  C5 reconfiguration steps to reduce interrupt-watchdog (`handle_interrupts`)
  wedges at startup. Settle is a no-op on every other chip.
- Expanded crate re-exports (`install_static_espnow_recv`,
  `apply_peer_espnow_phy` alongside `set_peer_espnow_phy`).
- `rust-toolchain.toml` adds `rust-src` and the `riscv32imac` target; new
  `.cargo` aliases for the `defmt` logging flavor.
- Expanded README "HT40 CSI Collection" guide (per-peer PHY, unicast topology,
  channel/secondary selection, per-chip notes, verification).

### Fixed

- **CSI requires an OFDM PHY**: bandwidth/diagnostic experiments transmitted at
  an 802.11b DSSS rate (`Rate11mL`), which carries no training fields and
  produces *no CSI*. Switched to OFDM HT rates (`RateMcs0Lgi`) so the receiver
  actually produces CSI reports.
- **`traffic_freq_hz` is Hz, not an interval**: the `bw_tx` experiment used
  `10000` (clamped to 8000 Hz), which pegged the C5 CPU on the synchronous TX
  wait path and saturated the TX queue (frames failed to go out). Corrected to
  100 Hz.
- Documented/avoided the `with_ht40(SecondaryChannel::None)` pitfall — it still
  flags a node as HT40 and forces the C5 interface to 40 MHz, breaking RX. Use
  no `with_ht40` for HT20.
- **Logging backend now builds on chips without USB-Serial-JTAG** (classic
  ESP32): the `auto` USB-SOF check referenced a chip-specific register
  unconditionally, breaking the build on ESP32. The register read is now gated
  to the USB-JTAG chips (C3/C5/C6/S3); others always report "no USB SOF" and use
  the UART/sync path.

### Known issues & notes

- **C5 boot ISR-wedge is mitigated, not eliminated.** Intermittent
  `handle_interrupts` watchdog resets / silent freezes can still occur during
  dual-band radio bring-up. Most effective workaround: keep ESP-NOW traffic off
  the air during a node's bring-up — power the collector up first, then the peer.
- **2.4 GHz HT40 is experimental** (ESP32 / S3 / C3 / C6). HT40 on channel 11
  did not bring up the central's CSI; the `_ht40` examples use the 6 + 10 pair
  instead. Verify HT40 engaged via the central's `Subcarriers` field (≥ 100
  confirms it). If it won't engage, try another pair or fall back to HT20.
- The `statistics` feature is **not** enabled by default; enable it (or use a
  `-defmt` alias) for the throughput/diagnostic counters.
