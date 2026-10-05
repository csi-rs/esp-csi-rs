//! ESP-NOW PHY forcing: per-peer rate and HT bandwidth.
//!
//! Forced through esp-radio's per-peer rate config (`EspNow::set_peer_rate`),
//! the only API that actually forces the ESP-NOW frame PHY (rate, HT/HE mode
//! and bandwidth).
//!
//! This used to also carry controller-level bring-up helpers that forced the PHY
//! by restarting the STA interface and setting band/channel/bandwidth on the
//! controller. Those were superseded by the per-peer rate config below — which
//! carries the HT40 secondary channel too — and were removed once nothing called
//! them on any chip.

use esp_radio::esp_now::{EspNow, PhyMode, RateConfig, WifiPhyRate};
use esp_radio::wifi::SecondaryChannel;

use crate::log_ln;

/// Install this crate's static-pool ESP-NOW receive callback.
///
/// Call this immediately after building [`NodeHardware`](crate::NodeHardware) in
/// examples that may boot while another ESP-NOW node is already transmitting.
/// Creating `EspNow` briefly installs esp-radio's heap-backed receive queue;
/// replacing it early keeps startup traffic out of that queue.
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

/// The TX PHY a node forces on an ESP-NOW peer.
///
/// Built by [`EspNowConfig::peer_phy`](crate::EspNowConfig::peer_phy). The PHY mode is derived:
/// an MCS rate goes out at HT20, HT40 when `secondary` is set, or HE20 when `he20` is set; a
/// legacy rate goes out at 11b or 11g, whichever carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PeerPhy {
    /// The forced rate.
    pub rate: WifiPhyRate,
    /// HT40 secondary channel, or `None` for a 20 MHz PHY.
    pub secondary: Option<SecondaryChannel>,
    /// Send MCS rates as HE20 (802.11ax SU) instead of HT20. 20 MHz only, so it
    /// overrides `secondary`. Honoured on the ESP32-C5 and ESP32-C6, the parts
    /// with an 802.11ax PHY.
    pub he20: bool,
}

impl PeerPhy {
    fn phy_mode(&self) -> PhyMode {
        let c = self.rate as u32;
        // `WifiPhyRate` discriminants are ESP-IDF's `wifi_phy_rate_t` values: the MCS block
        // is 16..=35 (LGI then SGI), 0..=7 the 11b rates and 8..=15 the 11g ones.
        if (16..=35).contains(&c) {
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            if self.he20 {
                return PhyMode::He20;
            }
            if self.secondary.is_some() {
                PhyMode::Ht40
            } else {
                PhyMode::Ht20
            }
        } else if c <= 7 {
            PhyMode::_11b
        } else {
            PhyMode::_11g
        }
    }
}

/// Force a peer's ESP-NOW TX PHY. The peer must already be registered with `esp_now`.
pub fn set_peer_espnow_phy(esp_now: &EspNow, peer: &[u8; 6], phy: PeerPhy) {
    let phy_mode = phy.phy_mode();
    let cfg = RateConfig {
        phy_mode,
        rate: phy.rate,
        ersu: false,
        dcm: false,
    };
    if let Err(e) = esp_now.set_peer_rate(peer, cfg) {
        log_ln!(
            "ESP-NOW: set_peer_rate failed {:?} phymode={:?} rate={:?}",
            e,
            phy_mode,
            phy.rate
        );
    }
}

/// [`set_peer_espnow_phy`] with receive suspended during the driver call (C5-safe).
pub fn apply_peer_espnow_phy(esp_now: &EspNow, peer: &[u8; 6], phy: PeerPhy) {
    with_espnow_recv_suspended(|| {
        set_peer_espnow_phy(esp_now, peer, phy);
    });
}
