//! The pre-0.12 packet fields, rebuilt from a [`CsiPacket`] for the text log formats.
//!
//! `LogMode::Text`, `ArrayList` and `EspCsiTool` are parsed by existing host tools column by
//! column, so their output must not move when the packet type does. The formatters read this view,
//! which carries the old field names, types and per-chip layout, instead of the wire types
//! directly. `LogMode::Serialized` does not use it; it encodes the wire contract itself.
//!
//! The receive timestamp is truncated to its low 32 bits here, exactly as the old `u32` field
//! was. The full 64-bit value is only in the serialized format.

use super::CsiPacket;
#[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
use crate::wire::Bandwidth;
use crate::wire::{Secondary, VendorRx};

const fn secondary_code(s: Secondary) -> u32 {
    match s {
        Secondary::None => 0,
        Secondary::Above => 1,
        Secondary::Below => 2,
    }
}

pub(crate) struct TextRow<'a> {
    pub mac: [u8; 6],
    pub rssi: i32,
    pub timestamp: u32,
    pub rate: u32,
    pub noise_floor: i32,
    pub sig_len: u32,
    pub rx_state: u32,
    pub channel: u32,
    /// Never set by this crate; kept so the text formats print the same "real time" columns.
    pub date_time: Option<crate::time::DateTime>,
    pub sequence_number: u16,
    pub csi_data_len: u16,
    pub csi_data: &'a [i8],

    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub sgi: u32,
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub secondary_channel: u32,
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub bandwidth: u32,
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub antenna: u32,
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub sig_mode: u32,
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub mcs: u32,
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub smoothing: u32,
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub not_sounding: u32,
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub aggregation: u32,
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub stbc: u32,
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub fec_coding: u32,
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    pub ampdu_cnt: u32,

    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub dump_len: u32,
    #[cfg(feature = "esp32c6")]
    pub sigb_len: u32,
    #[cfg(feature = "esp32c6")]
    pub cur_single_mpdu: u32,
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub cur_bb_format: u32,
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub rx_channel_estimate_info_vld: u32,
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub rx_channel_estimate_len: u32,
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub second: u32,
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub is_group: u32,
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub rxend_state: u32,
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub rxmatch3: u32,
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub rxmatch2: u32,
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub rxmatch1: u32,
    #[cfg(feature = "esp32c6")]
    pub rxmatch0: u32,
}

impl<'a> TextRow<'a> {
    pub(crate) fn new(p: &'a CsiPacket) -> Self {
        let m = &p.frame.meta;
        let csi_data = p.csi_data();
        #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
        let (rate, sig_mode, smoothing, fec, ampdu) = match m.vendor {
            VendorRx::EspClassic {
                rate,
                sig_mode,
                smoothing,
                fec_ldpc,
                ampdu_cnt,
            } => (rate, sig_mode, smoothing, fec_ldpc, ampdu_cnt),
            _ => (0, 0, false, false, 0),
        };
        #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
        let (rate, bb, est_vld, est_len, dump_len, is_group, rxend, rxmatch, sigb, single) =
            match m.vendor {
                VendorRx::EspHe {
                    rate,
                    cur_bb_format,
                    estimate_valid,
                    estimate_len,
                    dump_len,
                    is_group,
                    rxend_state,
                    rxmatch,
                    sigb_len,
                    single_mpdu,
                    ..
                } => (
                    rate,
                    cur_bb_format,
                    estimate_valid,
                    estimate_len,
                    dump_len,
                    is_group,
                    rxend_state,
                    rxmatch,
                    sigb_len,
                    single_mpdu,
                ),
                _ => (0, 0, false, 0, 0, false, 0, 0, 0, false),
            };
        #[cfg(feature = "esp32c5")]
        let _ = (sigb, single);

        Self {
            mac: p.mac(),
            rssi: m.rssi as i32,
            timestamp: m.timestamp_us as u32,
            rate: rate as u32,
            noise_floor: m.noise_floor as i32,
            sig_len: m.sig_len as u32,
            rx_state: m.rx_state as u32,
            channel: m.channel as u32,
            date_time: None,
            sequence_number: m.frame_seq.unwrap_or(0),
            csi_data_len: csi_data.len() as u16,
            csi_data,

            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            sgi: m.sgi.unwrap_or(false) as u32,
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            secondary_channel: secondary_code(m.secondary),
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            bandwidth: matches!(m.bandwidth, Some(Bandwidth::Mhz40)) as u32,
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            antenna: m.antenna.unwrap_or(0) as u32,
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            sig_mode: sig_mode as u32,
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            mcs: m.mcs.unwrap_or(0) as u32,
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            smoothing: smoothing as u32,
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            not_sounding: m.not_sounding.unwrap_or(true) as u32,
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            aggregation: m.aggregation.unwrap_or(false) as u32,
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            stbc: m.stbc.unwrap_or(false) as u32,
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            fec_coding: fec as u32,
            #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
            ampdu_cnt: ampdu as u32,

            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            dump_len: dump_len as u32,
            #[cfg(feature = "esp32c6")]
            sigb_len: sigb as u32,
            #[cfg(feature = "esp32c6")]
            cur_single_mpdu: single as u32,
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            cur_bb_format: bb as u32,
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            rx_channel_estimate_info_vld: est_vld as u32,
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            rx_channel_estimate_len: est_len as u32,
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            second: secondary_code(m.secondary),
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            is_group: is_group as u32,
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            rxend_state: rxend as u32,
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            rxmatch3: ((rxmatch >> 3) & 1) as u32,
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            rxmatch2: ((rxmatch >> 2) & 1) as u32,
            #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
            rxmatch1: ((rxmatch >> 1) & 1) as u32,
            #[cfg(feature = "esp32c6")]
            rxmatch0: (rxmatch & 1) as u32,
        }
    }
}
