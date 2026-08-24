//! ESP-NOW PHY forcing: per-peer rate and HT bandwidth.
//!
//! esp-radio doesn't expose `esp_now_set_peer_rate_config`, the only API that
//! actually forces the ESP-NOW frame PHY (rate + HT bandwidth), so it is bound
//! directly here.
//!
//! This used to also carry controller-level bring-up helpers that forced the PHY
//! by restarting the STA interface and setting band/channel/bandwidth on the
//! controller. Those were superseded by the per-peer rate config below — which
//! carries the HT40 secondary channel too — and were removed once nothing called
//! them on any chip.

use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::SecondaryChannel;

use crate::log_ln;

/// Install this crate's static-pool ESP-NOW receive callback.
///
/// Call this immediately after `esp_radio::wifi::new()` in examples that may
/// boot while another ESP-NOW node is already transmitting. `wifi::new()`
/// constructs `EspNow` internally and briefly installs esp-radio's heap-backed
/// receive queue; replacing it early keeps startup traffic out of that queue.
/// On ESP32-C5 this also avoids Wi-Fi ISR work while the dual-band radio is
/// still being reconfigured (a common source of interrupt watchdog timeouts).
pub fn install_static_espnow_recv() {
    crate::esp_now_pool::install();
}

/// Temporarily stop ESP-NOW receive dispatch at the C layer.
///
/// Dual-band bring-up (band switch, channel, bandwidth, STA restart) on
/// ESP32-C5 must not deliver ESP-NOW frames into callbacks mid-transition.
///
/// C5-only, because that is the only chip whose `with_espnow_recv_suspended`
/// arm calls it — the other chips need no such window.
#[cfg(feature = "esp32c5")]
pub(crate) fn suspend_esp_now_recv() {
    unsafe extern "C" {
        fn esp_now_unregister_recv_cb() -> i32;
    }
    unsafe {
        let _ = esp_now_unregister_recv_cb();
    }
}

/// Run a Wi-Fi controller mutation with ESP-NOW recv suspended on C5.
///
/// On dual-band C5, recv callbacks firing during `set_protocols`,
/// `set_config`, or `set_csi` can wedge the Wi-Fi ISR and trip the
/// interrupt watchdog (`handle_interrupts` backtrace at boot).
#[cfg(feature = "esp32c5")]
pub(crate) fn with_espnow_recv_suspended<F: FnOnce()>(f: F) {
    suspend_esp_now_recv();
    f();
    install_static_espnow_recv();
}

#[cfg(not(feature = "esp32c5"))]
pub(crate) fn with_espnow_recv_suspended<F: FnOnce()>(f: F) {
    f();
}

// ESP-NOW per-peer TX rate config (ESP-IDF `esp_now_set_peer_rate_config`).
#[repr(C)]
struct WifiTxRateConfig {
    phymode: u32,
    rate: u32,
    ersu: bool,
    dcm: bool,
}

const WIFI_PHY_MODE_11B: u32 = 1;
const WIFI_PHY_MODE_11G: u32 = 2;
const WIFI_PHY_MODE_HT20: u32 = 4;
const WIFI_PHY_MODE_HT40: u32 = 5;

unsafe extern "C" {
    fn esp_now_set_peer_rate_config(peer_addr: *const u8, config: *mut WifiTxRateConfig) -> i32;
}

fn wifi_phy_rate_to_c(rate: WifiPhyRate) -> u32 {
    match rate {
        WifiPhyRate::RateLora250k => 41,
        WifiPhyRate::RateLora500k => 42,
        WifiPhyRate::RateMax => 43,
        // `esp-radio::WifiPhyRate` is a contiguous Rust enum, but ESP-IDF's
        // `wifi_phy_rate_t` has a gap at value 4 (there is no *_4M symbol).
        // Shift all non-LoRa values >= 4 to preserve the C ABI mapping.
        other => {
            let idx = other as u32;
            if idx < 4 { idx } else { idx + 1 }
        }
    }
}

fn espnow_phymode(rate: WifiPhyRate, secondary: Option<SecondaryChannel>) -> u32 {
    let c = wifi_phy_rate_to_c(rate);
    if (16..=31).contains(&c) {
        if secondary.is_some() {
            WIFI_PHY_MODE_HT40
        } else {
            WIFI_PHY_MODE_HT20
        }
    } else if c <= 7 {
        WIFI_PHY_MODE_11B
    } else {
        WIFI_PHY_MODE_11G
    }
}

/// Force a peer's ESP-NOW TX PHY to the configured `rate` and bandwidth.
pub fn set_peer_espnow_phy(peer: &[u8; 6], rate: WifiPhyRate, secondary: Option<SecondaryChannel>) {
    let mut cfg = WifiTxRateConfig {
        phymode: espnow_phymode(rate, secondary),
        rate: wifi_phy_rate_to_c(rate),
        ersu: false,
        dcm: false,
    };
    let rc = unsafe { esp_now_set_peer_rate_config(peer.as_ptr(), &mut cfg) };
    if rc != 0 {
        log_ln!(
            "ESP-NOW: set_peer_rate_config rc={} phymode={} rate={}",
            rc,
            cfg.phymode,
            cfg.rate
        );
    }
}

/// Apply per-peer ESP-NOW PHY with recv suspended during the driver call (C5-safe).
pub fn apply_peer_espnow_phy(
    peer: &[u8; 6],
    rate: WifiPhyRate,
    secondary: Option<SecondaryChannel>,
) {
    with_espnow_recv_suspended(|| {
        set_peer_espnow_phy(peer, rate, secondary);
    });
}
