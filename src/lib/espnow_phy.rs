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
    install_static_espnow_recv();
    if is_sniffer {
        suspend_esp_now_recv();
    }
}

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

/// Bring the radio up in **started STA mode** for the ESP-NOW forced-PHY path.
///
/// ESP-NOW PHY configuration only takes effect on a started STA interface.
/// Best-effort: on error we log and continue. `set_config` restarts the radio;
/// reinstall the static recv callback afterward when `resume_recv` is true.
pub(crate) fn bring_up_espnow_sta(controller: &mut WifiController, resume_recv: bool) {
    if controller
        .set_config(&Config::Station(StationConfig::default()))
        .is_err()
    {
        log_ln!("ESP-NOW: STA bring-up failed; PHY may stay legacy/20 MHz");
    }
    if resume_recv {
        install_static_espnow_recv();
    }
}

/// HT40 ESP-NOW bring-up: on C5, switch from AP bootstrap to started STA
/// (avoids `InterfaceMismatch` on Station-interface peers), then apply band,
/// channel, and 40 MHz bandwidth.
pub(crate) fn apply_espnow_ht40_mode(
    controller: &mut WifiController,
    primary: u8,
    secondary: SecondaryChannel,
) {
    #[cfg(feature = "esp32c5")]
    bring_up_espnow_sta(controller, false);
    apply_espnow_ht40(controller, primary, secondary);
}

/// Select 2.4 / 5 GHz band from the primary channel (ESP32-C5 dual-band only).
pub(crate) fn apply_espnow_band_for_channel(controller: &mut WifiController, primary: u8) {
    #[cfg(feature = "esp32c5")]
    {
        use esp_radio::wifi::BandMode;
        let band = if primary >= 36 {
            BandMode::_5G
        } else {
            BandMode::_2_4G
        };
        if controller.set_band_mode(band).is_err() {
            log_ln!("ESP-NOW: set_band_mode failed for ch {}", primary);
        }
    }
    #[cfg(not(feature = "esp32c5"))]
    {
        let _ = (controller, primary);
    }
}

/// Put the radio on an HT40-capable channel (primary + secondary). On C5 this
/// also switches band, caps 5 GHz protocols to A/N (higher-throughput modes
/// break peer rate config), and sets interface bandwidth to 40 MHz before
/// per-peer HT40 applies.
pub(crate) fn apply_espnow_ht40(
    controller: &mut WifiController,
    primary: u8,
    secondary: SecondaryChannel,
) {
    apply_espnow_band_for_channel(controller, primary);

    #[cfg(feature = "esp32c5")]
    {
        use esp_radio::wifi::{Protocol, Protocols};
        if primary >= 36 {
            let protocols = Protocols::default().with_5(Protocol::A | Protocol::N);
            if controller.set_protocols(protocols).is_err() {
                log_ln!("HT40: set_protocols (A/N on 5G) failed");
            }
        }
    }

    if controller.set_channel(primary, secondary).is_err() {
        log_ln!("HT40: set_channel failed");
    }

    // Raise the interface bandwidth to 40 MHz. `set_channel` only configures the
    // secondary-channel offset; without this the radio keeps a 20 MHz RX/TX path
    // and HT40 frames can't be decoded. Required on every chip, not just C5 —
    // single-band 2.4 GHz parts (C6/C3/S3/ESP32) were previously left at 20 MHz,
    // so a central could never receive the peripheral's 40 MHz unicast replies.
    {
        use esp_radio::wifi::Bandwidth;
        match controller.bandwidths() {
            Ok(bw) => {
                // 5 GHz (ch >= 36) only exists on the dual-band C5; `with_5` is
                // not present in the single-band HAL, so gate that branch.
                #[cfg(feature = "esp32c5")]
                let bw = if primary >= 36 {
                    bw.with_5(Bandwidth::_40MHz)
                } else {
                    bw.with_2_4(Bandwidth::_40MHz)
                };
                #[cfg(not(feature = "esp32c5"))]
                let bw = bw.with_2_4(Bandwidth::_40MHz);
                if let Err(e) = controller.set_bandwidths(bw) {
                    log_ln!("HT40: set_bandwidths failed: {:?}", e);
                }
            }
            Err(_) => log_ln!("HT40: read bandwidths failed"),
        }
    }
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
