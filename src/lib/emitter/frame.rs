//! The raw sounding frame an emitter injects, and the injection call itself.
//!
//! The frame is deliberately **rate-agnostic**: it is a plain, well-formed
//! non-QoS data frame, and the interface's forced TX rate decides which PPDU
//! format carries it. That is what lets one frame builder serve every
//! bandwidth/format an emitter can be configured for.

use esp_radio::wifi::WifiError;
use esp_radio::wifi::sniffer::Sniffer;

/// Broadcast address — the default injection destination.
pub const BROADCAST: [u8; 6] = [0xFF; 6];

/// Header + payload length of the frame built by [`build_probe_frame`].
///
/// Non-QoS Data MAC header (24 bytes) plus a short payload. `esp_wifi_80211_tx`
/// accepts only beacon / probe req/resp / action / **non-QoS** data frames — QoS
/// Data (`0x88`) is rejected with `ESP_ERR_INVALID_ARG`. The CSI engine reports
/// the channel estimate from the PPDU preamble, so the body only has to make the
/// frame well-formed; its contents are irrelevant.
pub const PROBE_FRAME_LEN: usize = 24 + 8;

/// Build a minimal well-formed 802.11 **non-QoS Data** frame for injection.
///
/// No DS bits are set, so the address layout is Addr1 = destination, Addr2 =
/// source (the transmitter address a receiver's CSI callback reports as
/// [`crate::csi::CSIDataPacket::mac`]), Addr3 = BSSID. Give each emitter a
/// distinct `src_mac` — its own interface MAC is the obvious choice — and a
/// single collector can separate emitters by that field.
///
/// The sequence-control field is left zero: pair this with the driver's internal
/// sequence numbering in [`inject_probe_once`] so each frame gets an
/// incrementing sequence number, which is what lets a collector de-duplicate
/// per-MAC.
///
/// Returns the number of bytes written, or `0` if `buf` is shorter than
/// [`PROBE_FRAME_LEN`].
pub fn build_probe_frame(src_mac: &[u8; 6], dst_mac: &[u8; 6], buf: &mut [u8]) -> usize {
    if buf.len() < PROBE_FRAME_LEN {
        return 0;
    }
    const HDR: usize = 24;
    buf[0] = 0x08; // Frame Control: type Data, subtype Data (non-QoS — required by esp_wifi_80211_tx)
    buf[1] = 0x00; // no ToDS / FromDS
    buf[2] = 0x00; // Duration/ID
    buf[3] = 0x00;
    buf[4..10].copy_from_slice(dst_mac); // Addr1 = DA
    buf[10..16].copy_from_slice(src_mac); // Addr2 = SA / transmitter address
    buf[16..22].copy_from_slice(src_mac); // Addr3 = BSSID (reuse SA)
    buf[22] = 0x00; // Sequence Control (driver overwrites when using internal seq numbers)
    buf[23] = 0x00;
    for (i, b) in buf[HDR..PROBE_FRAME_LEN].iter_mut().enumerate() {
        *b = i as u8;
    }
    PROBE_FRAME_LEN
}

/// Inject one raw frame at the interface's currently-forced TX rate.
///
/// Once the interface has been forced to a PHY mode (see
/// [`super::phy`]), every frame sent here goes out in that PPDU format with no
/// association required. `use_sta_if` selects the STA (`true`) or AP (`false`)
/// interface and must match the interface the forced rate was applied to.
pub fn inject_probe_once(
    sniffer: &mut Sniffer<'_>,
    use_sta_if: bool,
    frame: &[u8],
) -> Result<(), WifiError> {
    sniffer.send_raw_frame(use_sta_if, frame, true)
}
