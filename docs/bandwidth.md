# Bandwidth: HT20 and HT40

40 MHz gives roughly twice the subcarriers of 20 MHz — typically **~117–128** (`csi_data_len / 2`)
versus **~56** for HT20 HT-LTF or **~53** for legacy 20 MHz L-LTF.

HT40 works on **every supported chip**: it is plain 802.11n, not a C5/C6 feature. On the dual-band
C5 the band follows the primary channel number automatically (`>= 36` selects 5 GHz).

Select it on the emitter:

```rust
// Secondary channel above the primary: the 40 MHz block spans channels 7–11.
let emitter = EmitterConfig::new(7, HtBandwidth::Ht40Above);
```

## Two things that are easy to get wrong

- **Leave room in the band.** `Ht40Above` on channel 7 occupies up to channel 11; `Ht40Below`
  occupies down to channel 3. A primary too close to the band edge silently falls back.
- **The collector needs a 40 MHz RX path.** Setting the secondary channel only configures the
  offset; the interface bandwidth has to be widened too, or 40 MHz frames cannot be decoded. The
  library does both together.

## HT40 over ESP-NOW applies to the unicast leg only

A per-peer HT40 rate only applies to a **unicast** peer. In the symmetric pairing the central
broadcasts, the peripheral learns its MAC and replies by unicast, and the forced HT40 rate applies to
that learned peer — so the wide CSI is collected by the *central*, from the peripheral's replies.

On the dual-band ESP32-C5, forcing the PHY on the *broadcast* peer is additionally unsafe: it wedges
the Wi-Fi ISR. A C5 pair must use the unicast path.

2.4 GHz HT40 is finicky on some chips. If the subcarrier count stays narrow, try a different
primary/secondary pair (ch 1 Above, ch 11 Below) or accept HT20.

## Verify it actually engaged

Check the **collector's** captured CSI. A subcarrier count `>= 100` (commonly ~117) confirms HT40;
~53/~56 means it fell back to legacy or HT20. The `sniffer` and `esp_now` examples print it.

## Filter out legacy / ACK CSI

With the default `CsiConfig`, the radio also reports legacy and control-path CSI (including ACKs),
which can dominate the statistics and look "stuck at ~53 subcarriers" even though HT40 is configured.
Symptoms: the subcarrier count stays ~53, the reported rate stays legacy, and the CSI count tracks
ambient traffic rather than the emitter's rate.

`emitter::phy::ht_csi_acquisition` sets an HT-only acquisition for you. By hand on the newer PHY
(C5/C6):

```rust
let csi_cfg = CsiConfig {
    acquire_csi_legacy: 0,
    acquire_csi_ht20: 0,
    acquire_csi_ht40: 1,
    dump_ack_en: 0,
    ..CsiConfig::default()
};
```

The classic ESP32 / C3 / S3 parts expose different controls — `lltf_en` / `htltf_en` /
`ltf_merge_en` rather than `acquire_csi_*` — so use `ht_csi_acquisition` for one call that works
everywhere.
