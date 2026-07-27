//! Forced TX PHY for the 802.11n high-throughput formats (HT20 / HT40).
//!
//! An emitter transmits without associating, so it cannot negotiate a rate. It
//! instead forces the interface's TX PHY mode outright via ESP-IDF's
//! `esp_wifi_config_80211_tx`, which esp-radio does not expose. Every frame the
//! interface subsequently sends — including the raw frames from
//! [`super::frame::inject_probe_once`] — goes out in that format.
//!
//! HT20/HT40 are plain 802.11n, supported by every chip this crate targets.

use esp_radio::wifi::csi::CsiConfig as RadioCsiConfig;
use esp_radio::wifi::{Bandwidth, Protocol, Protocols, WifiController};

const WIFI_IF_STA: u32 = 0;
const WIFI_IF_AP: u32 = 1;
const WIFI_MODE_AP: u32 = 2;
const WIFI_PHY_MODE_HT20: u32 = 4;
const WIFI_PHY_MODE_HT40: u32 = 5;
const WIFI_PHY_RATE_MCS0_LGI: u32 = 16;

#[repr(C)]
struct WifiTxRateConfig {
    phymode: u32,
    rate: u32,
    ersu: bool,
    dcm: bool,
}

unsafe extern "C" {
    fn esp_wifi_config_80211_tx(ifx: u32, config: *mut WifiTxRateConfig) -> i32;
    fn esp_wifi_get_mode(mode: *mut u32) -> i32;
    fn esp_wifi_set_mode(mode: u32) -> i32;
}

/// Force an HT (802.11n) TX PHY mode at MCS0 / long GI on interface `ifx`.
///
/// Returns the driver's status code; non-zero means the rate was not applied and
/// frames will go out in whatever format the interface defaults to. Callers should
/// surface that rather than discard it — a silently unforced PHY looks identical to
/// a working emitter until you inspect the receiver.
fn force_ht_tx(ifx: u32, phymode: u32) -> i32 {
    let mut cfg = WifiTxRateConfig {
        phymode,
        rate: WIFI_PHY_RATE_MCS0_LGI,
        ersu: false,
        dcm: false,
    };
    unsafe { esp_wifi_config_80211_tx(ifx, &mut cfg) }
}

/// Force a TX PHY mode on the AP interface.
///
/// Forcing AP TX requires `WIFI_MODE_AP` to be set first, so the current mode is
/// saved, switched, and restored around the call.
fn force_tx_ap_before_start(phymode: u32) -> i32 {
    let mut prev_mode = 0u32;
    unsafe {
        let _ = esp_wifi_get_mode(&mut prev_mode);
        if prev_mode != WIFI_MODE_AP {
            let _ = esp_wifi_set_mode(WIFI_MODE_AP);
        }
        let rc = force_ht_tx(WIFI_IF_AP, phymode);
        if prev_mode != WIFI_MODE_AP {
            let _ = esp_wifi_set_mode(prev_mode);
        }
        rc
    }
}

/// Legacy HT protocols (802.11 B|G|N — no AX). The C5 also advertises A|N on 5 GHz.
fn ht_protocols() -> Protocols {
    let protocols = Protocols::default().with_2_4(Protocol::B | Protocol::G | Protocol::N);
    #[cfg(feature = "esp32c5")]
    {
        return protocols.with_5(Protocol::A | Protocol::N);
    }
    #[cfg(not(feature = "esp32c5"))]
    protocols
}

/// Lock the 2.4 GHz bandwidth for an emitter: 40 MHz for HT40, else 20 MHz.
pub fn apply_ht_bandwidth(controller: &mut WifiController<'_>, forty: bool) {
    if let Ok(bw) = controller.bandwidths() {
        let width = if forty {
            Bandwidth::_40MHz
        } else {
            Bandwidth::_20MHz
        };
        let _ = controller.set_bandwidths(bw.with_2_4(width));
    }
}

/// Re-apply B|G|N after `set_config`, which embeds its own default protocol set.
pub fn apply_ht_protocols(controller: &mut WifiController<'_>) {
    let _ = controller.set_protocols(ht_protocols());
}

/// Request HT-LTF CSI acquisition on a raw CSI config, disabling the other
/// acquisition paths so the HT channel estimate is the only one reported.
///
/// This is the collector-side counterpart to the emitter's forced TX PHY: a
/// collector paired with an HT emitter wants HT acquisition and nothing else,
/// otherwise the stream is diluted by short legacy and ACK frames.
///
/// The two PHY generations expose completely different acquisition controls, so
/// this is gated rather than shared. `forty` only means something on the newer
/// PHY (C5/C6), which selects HT20 vs HT40 acquisition explicitly; on the older
/// parts the reported CSI width simply follows the received PPDU.
pub fn ht_csi_acquisition(raw: &mut RadioCsiConfig, forty: bool) {
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    {
        raw.acquire_csi_legacy = 0;
        raw.acquire_csi_ht20 = 1;
        raw.acquire_csi_ht40 = if forty { 1 } else { 0 };
        raw.acquire_csi_su = 0;
        raw.acquire_csi_mu = 0;
        raw.acquire_csi_dcm = 0;
        raw.acquire_csi_beamformed = 0;
        raw.dump_ack_en = 0;
        #[cfg(feature = "esp32c5")]
        {
            raw.acquire_csi_force_lltf = false;
            raw.acquire_csi_vht = false;
        }
    }

    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    {
        // This PHY reports whatever training field the received PPDU carried, so
        // there is no HT20/HT40 switch to set.
        let _ = forty;
        // HT-LTF is what we want, but `lltf_en` must stay ON: measured on an
        // ESP32-S3, clearing it silences CSI reporting altogether rather than
        // just dropping the legacy field, so an HT-only filter here produces a
        // collector that captures nothing. Turning `ltf_merge_en` off is what
        // keeps the HT estimate from being averaged with the L-LTF one, which is
        // the part that actually mattered.
        raw.lltf_en = true;
        raw.htltf_en = true;
        raw.stbc_htltf2_en = true;
        raw.ltf_merge_en = false;
        raw.dump_ack_en = false;
    }
}

/// Force HT20 TX on STA; call immediately before `set_config(Station)`.
pub fn force_ht20_tx_sta_before_start() -> i32 {
    force_ht_tx(WIFI_IF_STA, WIFI_PHY_MODE_HT20)
}

/// Force HT40 TX on STA; call immediately before `set_config(Station)`.
pub fn force_ht40_tx_sta_before_start() -> i32 {
    force_ht_tx(WIFI_IF_STA, WIFI_PHY_MODE_HT40)
}

/// Force HT20 TX on AP; call immediately before `set_config(AccessPoint)`.
pub fn force_ht20_tx_ap_before_start() -> i32 {
    force_tx_ap_before_start(WIFI_PHY_MODE_HT20)
}

/// Force HT40 TX on AP; call immediately before `set_config(AccessPoint)`.
pub fn force_ht40_tx_ap_before_start() -> i32 {
    force_tx_ap_before_start(WIFI_PHY_MODE_HT40)
}
