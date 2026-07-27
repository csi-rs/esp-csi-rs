//! Low-level radio helpers shared by every node role.
//!
//! Band selection, HT40 channel setup, and the one piece of ESP-NOW handling
//! this crate still needs: silencing esp-radio's built-in ESP-NOW receive
//! dispatcher. See [`suppress_espnow_rx`].

use esp_radio::wifi::{SecondaryChannel, WifiController};

use crate::log_ln;

/// Permanently unregister esp-radio's ESP-NOW receive callback.
///
/// `esp_radio::wifi::new` eagerly builds `EspNow` (via `EspNow::new_internal`),
/// which calls `esp_now_init()`, registers esp-radio's heap-allocating `rcv_cb`,
/// and adds a broadcast peer — whether or not anything in this crate speaks
/// ESP-NOW. From that instant, and `esp_rtos` is already running, every
/// overheard ESP-NOW vendor action frame is `Box`ed and `push_back`ed into a
/// heap-backed `VecDeque<ReceivedData>` that nothing drains. Any blocking Wi-Fi
/// call during startup (`set_protocols`, `set_csi`) gives that callback time to
/// fire and grow the deque; on the small ESP32-S3 heap the next grow allocation
/// can fail, panicking in `handle_alloc_error` *inside esp-radio's `rcv_cb`*.
///
/// No role in this crate consumes ESP-NOW data, so the callback is dropped
/// outright at bring-up: overheard frames are then discarded at the C layer with
/// zero allocation. This also keeps Wi-Fi ISR work out of the way while a
/// dual-band ESP32-C5 radio is still being reconfigured, which was a recurring
/// source of interrupt-watchdog timeouts during `set_protocols` / `set_config` /
/// `set_csi`.
pub(crate) fn suppress_espnow_rx() {
    unsafe extern "C" {
        fn esp_now_unregister_recv_cb() -> i32;
    }
    unsafe {
        let _ = esp_now_unregister_recv_cb();
    }
}

/// Select the 2.4 / 5 GHz band from a primary channel number.
///
/// Only the dual-band ESP32-C5 has a band to choose; a no-op elsewhere. Without
/// this, a run on a 2.4 GHz channel can stay pinned to 5 GHz from a previous
/// application's radio state.
///
/// **The controller must already be configured and started** — `esp_wifi_set_band_mode`
/// requires it, and calling this earlier fails silently apart from the log line.
pub(crate) fn apply_band_for_channel(controller: &mut WifiController, primary: u8) {
    #[cfg(feature = "esp32c5")]
    {
        use esp_radio::wifi::BandMode;
        let band = if primary >= 36 {
            BandMode::_5G
        } else {
            BandMode::_2_4G
        };
        if controller.set_band_mode(band).is_err() {
            log_ln!("radio: set_band_mode failed for ch {}", primary);
        }
    }
    #[cfg(not(feature = "esp32c5"))]
    {
        let _ = (controller, primary);
    }
}

/// Select both bands (dual-band scan) on a 5 GHz-capable part.
///
/// For a station with no channel hint this is the only correct choice: pinning a
/// single band means an access point on the other one is simply invisible, and
/// leaving the band alone inherits whatever a previous run selected. `Auto` is the
/// platform default on these parts, so this restores it explicitly rather than
/// trusting that nothing has changed it.
///
/// Only exists on the dual-band part; single-band chips have no band to choose and
/// no caller, so the whole function is gated rather than left as a no-op stub.
#[cfg(feature = "esp32c5")]
pub(crate) fn apply_band_auto(controller: &mut WifiController) {
    use esp_radio::wifi::BandMode;
    if controller.set_band_mode(BandMode::Auto).is_err() {
        log_ln!("radio: set_band_mode(Auto) failed");
    }
}

/// Put the radio on an HT40 channel pair and raise the interface to 40 MHz.
///
/// `set_channel` only configures the secondary-channel offset; without also
/// widening the interface bandwidth the radio keeps a 20 MHz RX/TX path and
/// HT40 frames cannot be decoded. That applies on every chip, not just the C5.
pub(crate) fn apply_ht40_channel(
    controller: &mut WifiController,
    primary: u8,
    secondary: SecondaryChannel,
) {
    apply_band_for_channel(controller, primary);

    if controller.set_channel(primary, secondary).is_err() {
        log_ln!("HT40: set_channel failed");
    }

    use esp_radio::wifi::Bandwidth;
    match controller.bandwidths() {
        Ok(bw) => {
            // 5 GHz (ch >= 36) exists only on the dual-band C5; `with_5` is
            // absent from the single-band HAL, so gate that branch.
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
