# Emitter chip support

An emitter is a **central listener**: it originates the network's traffic and captures nothing. How
its frames leave the radio is chosen by the chip, not by configuration, because the transport that
works is not the same on every part.

| Chip | Transport | Status |
|---|---|---|
| ESP32-C5, ESP32-C6 | Raw injection (`esp_wifi_80211_tx`) | **Verified** — roughly 90 CSI reports/s at a 10 ms period, measured at a paired collector |
| ESP32, ESP32-C3, ESP32-S3 | ESP-NOW broadcast | Works; broadcast frames are never ACKed, so the offered rate stays flat whether or not anyone is listening |

## Why raw injection is not offered everywhere

**On the ESP32-S3 raw injection does not radiate.** `esp_wifi_80211_tx` returns success for every
frame and the forced TX PHY is accepted, but nothing reaches a collector. This is not a board fault
— the same S3 associates to an access point and sustains ~290 reports/s as a station — and not a
library bug, since the identical code path radiates on C5/C6. It appears to be raw-TX behaviour in
esp-radio / ESP-IDF on that part.

Rather than let a user select a transport the crate knows will not radiate on their chip, the
selection is made at compile time from the chip feature. That is the same principle the node model
applies to roles: a configuration that cannot work is not offered.

ESP-NOW frames carry the node's station MAC as the transmitter address, so a collector attributes
CSI to the emitter exactly as it would for an injected sounding frame, and several emitters can
share one collector.

## If you need more reports per second from a classic chip

An ESP32-S3 (or ESP32, ESP32-C3) emitter works, over ESP-NOW broadcast. When an unassociated
sounding source is not a requirement, pairing a **station** against a **softAP**
(`station` + `access_point`) is the alternative: an associated link rather than blind sounding, but
unicast, ACKed traffic that yields more reports per second — ~290/s measured on an S3 station.

Any chip can measure an emitter's frames as a **sniffer**.
