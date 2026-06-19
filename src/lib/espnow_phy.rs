//! ESP-NOW PHY forcing (per-peer rate / HT40) and radio bring-up helpers.
//!
//! esp-radio doesn't expose `esp_now_set_peer_rate_config`, the only API that
//! actually forces the ESP-NOW frame PHY (rate + HT bandwidth), so we bind it
//! directly here alongside the startup helpers that get the radio into the
//! state that binding requires.

use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, SecondaryChannel, WifiController};

use crate::log_ln;

/// Take over esp-radio's ESP-NOW receive dispatcher as early as possible.
///
/// `esp_radio::wifi::new` eagerly builds `EspNow` (via `EspNow::new_internal`),
/// which calls `esp_now_init()`, registers esp-radio's heap-allocating
/// `rcv_cb`, and adds a broadcast peer. From that instant — and `esp_rtos`
/// is already running — every overheard ESP-NOW vendor action frame is
/// `Box`ed and `push_back`ed into a heap-backed `VecDeque<ReceivedData>`
/// that nothing in this crate drains (our consumers read the static
/// [`crate::esp_now_pool`] queue instead). Any blocking Wi-Fi call during
/// startup (`set_protocols`, `set_csi`) gives that callback time to fire and
/// grow the VecDeque; on the small ESP32-S3 heap the next grow allocation
/// can't be satisfied → `handle_alloc_error` panic *inside esp-radio's
/// `rcv_cb`*, before our pool was ever installed.
///
/// Calling this *first* in `run`/`run_duration`, before any other Wi-Fi
/// reconfiguration, closes that startup window:
/// - non-sniffer modes get our static-pool `rcv_cb` (no heap, ever);
/// - sniffer mode drops the callback entirely so overheard frames are
///   discarded at the C layer with zero allocation (sniffers never consume
///   ESP-NOW data).
pub(crate) fn takeover_esp_now_recv(is_sniffer: bool) {
    crate::esp_now_pool::install();
    if is_sniffer {
        unsafe extern "C" {
            fn esp_now_unregister_recv_cb() -> i32;
        }
        unsafe {
            let _ = esp_now_unregister_recv_cb();
        }
    }
}

/// Bring the radio up in **started STA mode** for the ESP-NOW HT40 path.
///
/// ESP-NOW PHY configuration only takes effect on a started STA interface:
/// esp-radio applies the ESP-NOW rate to `WIFI_IF_STA`
/// (`esp_wifi_config_espnow_rate`) and gates `set_bandwidths` on STA/AP mode,
/// and `esp_wifi_start()` runs *only* inside `set_config`. Without this the
/// controller stays in `WIFI_MODE_NULL`, so `set_rate`/`set_bandwidths` are
/// silent no-ops and frames go out legacy / 20 MHz regardless of
/// `with_ht40()` + `set_rate()` (confirmed via the `bw_check` example).
///
/// We do not associate to an AP — an unassociated STA is all ESP-NOW needs
/// (this mirrors the ESP-IDF reference, which runs ESP-NOW in `WIFI_MODE_STA`).
/// Best-effort: on error we log and continue (the run still works, just at the
/// default PHY). `set_config` restarts the radio, so the static-pool ESP-NOW
/// receive callback is re-installed afterward.
pub(crate) fn bring_up_espnow_sta(controller: &mut WifiController) {
    if controller
        .set_config(&Config::Station(StationConfig::default()))
        .is_err()
    {
        log_ln!("ESP-NOW HT40: STA bring-up failed; PHY may stay legacy/20 MHz");
    }
    // `set_config` stop/starts the radio; re-take the ESP-NOW receive dispatcher
    // so frames keep landing in the BSS pool, not esp-radio's heap queue.
    crate::esp_now_pool::install();
}

/// Put the radio on an HT40-capable channel (primary + secondary). This is
/// necessary but **not sufficient** for HT40 ESP-NOW: the PPDU bandwidth/rate
/// is set per-peer via [`set_peer_espnow_phy`], not by the interface bandwidth.
pub(crate) fn apply_espnow_ht40(
    controller: &mut WifiController,
    primary: u8,
    secondary: SecondaryChannel,
) {
    if controller.set_channel(primary, secondary).is_err() {
        log_ln!("HT40: set_channel failed");
    }
}

// ESP-NOW per-peer TX rate config (ESP-IDF `esp_now_set_peer_rate_config`).
// This is the ONLY way to force the ESP-NOW frame PHY (rate + HT bandwidth):
// it's a per-peer property, not the interface bandwidth/rate (which esp-radio
// sets via the deprecated `esp_wifi_config_espnow_rate`, a no-op on IDF v5.5).
// esp-radio doesn't expose this, so we bind it directly (same pattern as the
// pool's `esp_now_register_recv_cb`). `esp_now_rate_config_t == wifi_tx_rate_config_t`.
#[repr(C)]
struct WifiTxRateConfig {
    phymode: u32, // wifi_phy_mode_t
    rate: u32,    // wifi_phy_rate_t
    ersu: bool,
    dcm: bool,
}
// wifi_phy_mode_t values (esp-wifi-sys).
const WIFI_PHY_MODE_11B: u32 = 1;
const WIFI_PHY_MODE_11G: u32 = 2;
const WIFI_PHY_MODE_HT20: u32 = 4;
const WIFI_PHY_MODE_HT40: u32 = 5;

unsafe extern "C" {
    fn esp_now_set_peer_rate_config(peer_addr: *const u8, config: *mut WifiTxRateConfig) -> i32;
}

/// Map esp-radio's contiguous [`WifiPhyRate`] discriminant to the C
/// `wifi_phy_rate_t` value. The C enum reserves `0x04`, so everything from
/// `Rate2mS` (esp-radio 4) up is shifted by one; the two LoRa rates jump to
/// 41/42. Without this fix-up, MCS rates would be off by one (e.g. esp-radio
/// `RateMcs0Lgi` = 15 vs C `WIFI_PHY_RATE_MCS0_LGI` = 16).
fn wifi_phy_rate_to_c(rate: WifiPhyRate) -> u32 {
    match rate {
        WifiPhyRate::RateLora250k => 41,
        WifiPhyRate::RateLora500k => 42,
        WifiPhyRate::RateMax => 43,
        other => {
            let idx = other as u32;
            if idx < 4 {
                idx
            } else {
                idx + 1
            }
        }
    }
}

/// Derive the ESP-NOW `phymode` from the rate and the HT40 secondary-channel
/// choice. MCS rates (C 16–31) are HT → HT40 if a secondary channel is set,
/// else HT20. Legacy DSSS rates (C 0–7) are 11b; legacy OFDM (8–15) is 11g.
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

/// Force a peer's ESP-NOW TX PHY to the configured `rate` and bandwidth
/// (HT40 when `secondary` is set, else HT20/legacy per the rate). Must be
/// called after `esp_wifi_start()`, `esp_now_init` (done in `wifi::new`), and
/// the peer being added. Best-effort: a non-zero return is logged, not fatal.
///
/// The library applies this automatically for `EspNowConfig`-driven nodes; it's
/// public so raw-ESP-NOW examples (e.g. the CPU-test TX) can match the same PHY
/// after bringing the radio up in started STA mode (`Config::Station`) and
/// setting an HT40 channel.
pub fn set_peer_espnow_phy(
    peer: &[u8; 6],
    rate: WifiPhyRate,
    secondary: Option<SecondaryChannel>,
) {
    let mut cfg = WifiTxRateConfig {
        phymode: espnow_phymode(rate, secondary),
        rate: wifi_phy_rate_to_c(rate),
        ersu: false,
        dcm: false,
    };
    let rc = unsafe { esp_now_set_peer_rate_config(peer.as_ptr(), &mut cfg) };
    if rc != 0 {
        log_ln!("ESP-NOW: set_peer_rate_config rc={} (PHY may stay default)", rc);
    }
}
