//! ESP-NOW transport for the HT emitter.
//!
//! An emitter has to put known RF energy into the channel without associating. Raw injection
//! (`esp_wifi_80211_tx`) is one way to do that and it works on the newer MACs, but on the classic
//! parts it does not radiate: measured on an ESP32-S3, the driver returns `ESP_OK` for every
//! frame, the forced-TX call reports `rc = 0` before and after start, the emitter logs
//! "Emitter running" — and three independent collectors see nothing, while the same board beacons
//! normally as a softAP. That was reproduced against four different configurations (the AX-era
//! struct API, the legacy per-interface rate API, a management probe-request instead of a data
//! frame, and no forced rate at all), so it is not a rate or frame-type problem.
//!
//! ESP-NOW does not have that problem, and never did — it is how this crate transmitted from
//! classic chips before the emitter/collector rework. It is connectionless in exactly the way an
//! emitter needs (no association, no handshake, no reply expected) and it reaches the air through
//! the MAC's ordinary vendor-action-frame path rather than the raw-TX hook.
//!
//! **There is no ping-pong here.** This module only *sends*: a broadcast peer, a fixed payload, one
//! frame per period, nothing received and nothing awaited. A collector sees these frames
//! promiscuously like any other traffic and derives CSI from the PPDU preamble, so nothing on the
//! receive side needs to know ESP-NOW is involved.
//!
//! HE20 keeps using raw injection — it is C5/C6-only, lives in the proprietary crate, and ESP-NOW
//! cannot carry an HE PPDU.

use esp_radio::esp_now::{EspNow, EspNowWifiInterface, PeerInfo};
use esp_radio::wifi::{Interfaces, WifiController};

use crate::log_ln;

/// Broadcast peer address — the emitter's default destination.
pub const BROADCAST: [u8; 6] = [0xFF; 6];

/// `wifi_phy_mode_t` values used for per-peer PHY forcing.
const WIFI_PHY_MODE_HT20: u32 = 4;
const WIFI_PHY_MODE_HT40: u32 = 5;
/// `WIFI_PHY_RATE_MCS0_LGI` — the slowest HT rate, and the most robust.
const WIFI_PHY_RATE_MCS0_LGI: u32 = 16;

/// Mirror of ESP-IDF's `esp_now_rate_config_t` / `wifi_tx_rate_config_t`.
#[repr(C)]
struct WifiTxRateConfig {
    phymode: u32,
    rate: u32,
    ersu: bool,
    dcm: bool,
}

unsafe extern "C" {
    /// esp-radio does not expose this, and it is the only API that forces the PHY of an ESP-NOW
    /// frame. Note it is **per peer**, unlike the interface-wide `esp_wifi_config_80211_tx*`.
    fn esp_now_set_peer_rate_config(peer_addr: *const u8, config: *mut WifiTxRateConfig) -> i32;
}

/// Force a peer's ESP-NOW frames to HT20 or HT40 at MCS0 long-GI.
///
/// Returns the driver status; non-zero means the frames go out at whatever rate ESP-NOW picks,
/// which is a legacy rate. Surfaced rather than discarded, because an unforced PHY looks identical
/// to a working emitter until the receiver is inspected.
fn force_peer_ht(peer: &[u8; 6], forty: bool) -> i32 {
    let mut cfg = WifiTxRateConfig {
        phymode: if forty { WIFI_PHY_MODE_HT40 } else { WIFI_PHY_MODE_HT20 },
        rate: WIFI_PHY_RATE_MCS0_LGI,
        ersu: false,
        dcm: false,
    };
    unsafe { esp_now_set_peer_rate_config(peer.as_ptr(), &mut cfg) }
}

/// Bring the STA interface up and register the destination as an ESP-NOW peer with a forced HT PHY.
///
/// The interface must be *started* before any of this: per-peer rate config is silently ignored on
/// a stopped interface, and `add_peer` rejects a Station-interface peer outright.
pub fn bringup(
    controller: &mut WifiController<'_>,
    esp_now: &EspNow<'_>,
    dst: &[u8; 6],
    channel: u8,
    forty: bool,
) {
    use esp_radio::wifi::{Config, sta::StationConfig};
    if controller
        .set_config(&Config::Station(StationConfig::default()))
        .is_err()
    {
        log_ln!("emitter: ESP-NOW STA bring-up failed; frames may not radiate");
    }
    if esp_now.set_channel(channel).is_err() {
        log_ln!("emitter: ESP-NOW set_channel({}) failed", channel);
    }
    if !esp_now.peer_exists(dst) {
        if let Err(e) = esp_now.add_peer(PeerInfo {
            interface: EspNowWifiInterface::Station,
            peer_address: *dst,
            lmk: None,
            channel: Some(channel),
            encrypt: false,
        }) {
            log_ln!("emitter: ESP-NOW add_peer failed: {:?}", e);
        }
    }
    let rc = force_peer_ht(dst, forty);
    log_ln!(
        "Emitter (ESP-NOW): ch {}, {} MHz, peer PHY rc={}",
        channel,
        if forty { 40 } else { 20 },
        rc
    );
}

/// Send one broadcast sounding frame and wait for the MAC to finish with it. The payload is fixed
/// and meaningless — CSI comes from the PPDU preamble, so only the frame's existence and PHY matter.
///
/// The completion **must** be awaited. `EspNow::send` hands back a `SendWaiter` borrowing the
/// sender, and dropping it without polling abandons the transmission: measured on an ESP32-S3, a
/// fire-and-forget loop at a 2 ms period delivered 1 frame in 8 seconds. Awaiting each send yields
/// the full rate instead, and it is not the throughput limit — ESP-NOW completion is fast relative
/// to the inter-frame periods this emitter offers.
pub async fn send_once(esp_now: &mut EspNow<'_>, dst: &[u8; 6], payload: &[u8]) -> bool {
    esp_now.send_async(dst, payload).await.is_ok()
}

/// Whether this build should emit over ESP-NOW rather than raw injection.
///
/// Every chip does: ESP-NOW carries HT20/HT40 everywhere, and keeping one transport for the open
/// HT emitter avoids a per-chip split whose classic half was silently broken. Raw injection remains
/// the HE20 path in the proprietary crate.
pub const fn is_espnow_transport() -> bool {
    true
}

/// Access the ESP-NOW interface from the shared [`Interfaces`] handle.
pub fn interface<'a, 'd>(interfaces: &'a mut Interfaces<'d>) -> &'a mut EspNow<'d> {
    &mut interfaces.esp_now
}
