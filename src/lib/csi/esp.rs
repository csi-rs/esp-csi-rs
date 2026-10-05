//! The Espressif vendor source: maps esp-radio's `WifiCsiInfo` onto the wire contract.
//!
//! This is the only place that knows the shape of ESP's receive-control struct. Everything
//! downstream — delivery, statistics, encoding — sees [`CsiFrame`]s.

use esp_radio::wifi::SecondaryChannel;
use esp_radio::wifi::csi::WifiCsiInfo;
use heapless::Vec;

use crate::wire::{
    Bandwidth, Chip, CsiFrame, CsiPayload, HeaderDigest, LayoutId, MAX_CSI_BYTES, PpduFormat,
    RxMeta, Secondary, VendorRx,
};

/// The chip this build targets.
#[cfg(feature = "esp32")]
pub(crate) const CHIP: Chip = Chip::Esp32;
#[cfg(feature = "esp32s3")]
pub(crate) const CHIP: Chip = Chip::Esp32S3;
#[cfg(feature = "esp32c3")]
pub(crate) const CHIP: Chip = Chip::Esp32C3;
#[cfg(feature = "esp32c5")]
pub(crate) const CHIP: Chip = Chip::Esp32C5;
#[cfg(feature = "esp32c6")]
pub(crate) const CHIP: Chip = Chip::Esp32C6;

/// The raw secondary-channel code ESP uses (0 none, 1 above, 2 below), and its wire form.
fn secondary(info: &WifiCsiInfo<'_>) -> (u8, Secondary) {
    match info.secondary_channel() {
        SecondaryChannel::None => (0, Secondary::None),
        SecondaryChannel::Above => (1, Secondary::Above),
        SecondaryChannel::Below => (2, Secondary::Below),
    }
}

/// Build a [`CsiFrame`] from a CSI callback argument.
///
/// `None` if the buffer exceeds [`MAX_CSI_BYTES`]. The stimulus comes from the session context;
/// the header digest is captured only when enabled.
pub(crate) fn frame_from_esp(info: &WifiCsiInfo<'_>) -> Option<CsiFrame> {
    let buf = info.buf();
    let mut bytes = Vec::<i8, MAX_CSI_BYTES>::new();
    bytes.extend_from_slice(buf).ok()?;

    let raw_ts = info.timestamp().duration_since_epoch().as_micros() as u32;
    let timestamp_us = super::session::widen_timestamp(raw_ts);
    let (sec_raw, sec) = secondary(info);

    let (meta, layout) = rx_meta(info, timestamp_us, sec_raw, sec, buf.len());

    let header = if super::session::header_digest_enabled() {
        HeaderDigest::parse(info.header())
    } else {
        None
    };

    Some(CsiFrame::new(
        meta,
        super::session::stimulus(*info.mac(), instance_in(info)),
        header,
        CsiPayload::EspRaw {
            chip: CHIP,
            layout,
            first_word_invalid: info.first_word_invalid(),
            bytes,
        },
    ))
}

/// The sounding number carried by the measured frame, for a controlled run: an ESP-NOW control
/// frame's own sequence number.
fn instance_in(info: &WifiCsiInfo<'_>) -> Option<u32> {
    if !super::session::controlled() {
        return None;
    }
    crate::protocol::control_sequence_in(info.payload())
}

#[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
fn rx_meta(
    info: &WifiCsiInfo<'_>,
    timestamp_us: u64,
    sec_raw: u8,
    secondary: Secondary,
    len: usize,
) -> (RxMeta, LayoutId) {
    let sig_mode = info.packet_mode();
    let rate = info.rate();
    let forty = info.cwb();
    let stbc = info.space_time_block_code() != 0;
    let mcs = info.modulation_coding_scheme();
    let ppdu = match sig_mode {
        // `wifi_phy_rate_t`: 0..=7 are the 802.11b rates, 8..=15 the OFDM ones.
        0 if rate < 8 => PpduFormat::Dsss,
        0 => PpduFormat::NonHt,
        1 => PpduFormat::Ht,
        3 => PpduFormat::Vht,
        _ => PpduFormat::Unknown,
    };
    let ht_like = matches!(ppdu, PpduFormat::Ht | PpduFormat::Vht);
    let meta = RxMeta {
        timestamp_us,
        rssi: info.rssi(),
        noise_floor: info.noise_floor(),
        channel: info.channel(),
        secondary,
        bandwidth: Some(if forty { Bandwidth::Mhz40 } else { Bandwidth::Mhz20 }),
        ppdu,
        mcs: ht_like.then_some(mcs),
        stbc: Some(stbc),
        sgi: Some(info.short_guide_interval()),
        n_rx: 1,
        // HT MCS 8n..8n+7 use n+1 spatial streams.
        n_ss: Some(if ppdu == PpduFormat::Ht { mcs / 8 + 1 } else { 1 }),
        antenna: Some(info.antenna()),
        sig_len: info.signal_length(),
        rx_state: info.rx_state(),
        not_sounding: Some(info.not_sounding()),
        aggregation: Some(info.aggregation()),
        frame_seq: Some(info.rx_sequence()),
        vendor: VendorRx::EspClassic {
            rate,
            sig_mode,
            smoothing: info.smoothing(),
            fec_ldpc: info.forward_error_correction_coding(),
            ampdu_cnt: info.ampdu_count(),
        },
    };
    let layout = LayoutId::classify_classic(sec_raw, sig_mode, forty, stbc, len);
    (meta, layout)
}

#[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
fn rx_meta(
    info: &WifiCsiInfo<'_>,
    timestamp_us: u64,
    sec_raw: u8,
    secondary: Secondary,
    len: usize,
) -> (RxMeta, LayoutId) {
    let bb = info.cur_bb_format();
    let ppdu = match bb {
        0 => PpduFormat::Dsss,
        1 => PpduFormat::NonHt,
        2 => PpduFormat::Ht,
        3 => PpduFormat::Vht,
        4 => PpduFormat::HeSu,
        5 => PpduFormat::HeMu,
        6 => PpduFormat::HeErSu,
        7 => PpduFormat::HeTb,
        11 => PpduFormat::VhtMu,
        _ => PpduFormat::Unknown,
    };

    // Only the C5 has a published buffer table; the C6 is reported unclassified.
    #[cfg(feature = "esp32c5")]
    let layout = LayoutId::classify_c5(bb, sec_raw, len);
    #[cfg(feature = "esp32c6")]
    let layout = {
        let _ = (sec_raw, len);
        LayoutId::Unknown
    };

    // The 802.11ax-generation MAC reports neither a bandwidth nor an STBC bit. Where the layout is
    // known both follow from it; HE is a 20 MHz PHY on these parts.
    let (bandwidth, stbc) = match layout {
        LayoutId::C5Ht40 => (Some(Bandwidth::Mhz40), Some(false)),
        LayoutId::C5Ht40Stbc => (Some(Bandwidth::Mhz40), Some(true)),
        LayoutId::C5Ht20NoneStbc | LayoutId::C5Ht20BelowStbc | LayoutId::C5Ht20AboveStbc => {
            (Some(Bandwidth::Mhz20), Some(true))
        }
        LayoutId::Unknown => match ppdu {
            PpduFormat::HeSu | PpduFormat::HeMu | PpduFormat::HeErSu | PpduFormat::HeTb => {
                (Some(Bandwidth::Mhz20), None)
            }
            _ if secondary == Secondary::None => (Some(Bandwidth::Mhz20), None),
            _ => (None, None),
        },
        // L-LTF layouts describe the legacy field of whatever was received; its width is 20 MHz
        // but the PPDU's own STBC is not visible.
        LayoutId::C5LltfNone | LayoutId::C5LltfBelow | LayoutId::C5LltfAbove => {
            (Some(Bandwidth::Mhz20), None)
        }
        _ => (Some(Bandwidth::Mhz20), Some(false)),
    };

    #[allow(unused_mut)]
    let mut rxmatch = (info.rx_match1() as u8) << 1
        | (info.rx_match2() as u8) << 2
        | (info.rx_match3() as u8) << 3;
    #[cfg(feature = "esp32c6")]
    {
        rxmatch |= info.rx_match0() as u8;
    }
    #[cfg(feature = "esp32c6")]
    let (sigb_len, single_mpdu) = (info.he_sigb_length(), info.cur_single_mpdu());
    #[cfg(not(feature = "esp32c6"))]
    let (sigb_len, single_mpdu) = (0u8, false);

    let meta = RxMeta {
        timestamp_us,
        rssi: info.rssi(),
        noise_floor: info.noise_floor(),
        channel: info.channel(),
        secondary,
        bandwidth,
        ppdu,
        mcs: None,
        stbc,
        sgi: None,
        n_rx: 1,
        n_ss: None,
        antenna: None,
        sig_len: info.signal_length(),
        rx_state: info.rx_state(),
        not_sounding: None,
        aggregation: None,
        frame_seq: Some(info.rx_sequence()),
        vendor: VendorRx::EspHe {
            rate: info.rate(),
            cur_bb_format: bb,
            estimate_valid: info.rx_channel_estimate_info_valid(),
            estimate_len: info.rx_channel_estimate_length() as u16,
            dump_len: info.dump_length() as u16,
            is_group: info.is_group(),
            rxend_state: info.rx_end_state(),
            rxmatch,
            he_siga1: info.he_siga1(),
            he_siga2: info.he_siga2(),
            sigb_len,
            single_mpdu,
        },
    };
    (meta, layout)
}
