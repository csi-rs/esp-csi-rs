//! A synthetic IEEE 802.11bf measurement source, for exercising the pipeline without the hardware.
//!
//! It emits what a sensing initiator would collect from a measurement setup: every sounding
//! instance produces one [`CsiPayload::Grouped`] report per responder, stamped with the setup and
//! instance ids, over an HE20 tone plan (subcarriers −122..=122, grouped by `ng`). The channel is a
//! fixed per-responder profile plus a slow sinusoidal "motion" term, so a threshold detector
//! downstream has something to find. Deterministic: the same seed gives the same stream.
//!
//! Enabled by the `synthetic` feature. Like the rest of [`crate::wire`], no `esp-*` imports.

use heapless::Vec;

use super::{
    Bandwidth, CsiFrame, CsiPayload, MAX_CSI_BYTES, PpduFormat, RxMeta, Secondary, Stimulus,
    VendorRx,
};

/// Lowest HE20 subcarrier index.
const SC_START: i16 = -122;
/// HE20 subcarriers spanned.
const SC_SPAN: u16 = 245;

/// Generates 802.11bf-shaped measurement reports.
pub struct SyntheticBf<const R: usize> {
    setup_id: u8,
    responders: [[u8; 6]; R],
    ng: u8,
    instance: u16,
    next_responder: usize,
    periodicity_us: u32,
    now_us: u64,
    rng: u32,
}

impl<const R: usize> SyntheticBf<R> {
    /// A source for `setup_id`, sounding `responders` every `periodicity_us`, grouping
    /// subcarriers by `ng` (1, 2, 4 or 8 is typical).
    pub const fn new(setup_id: u8, responders: [[u8; 6]; R], ng: u8, periodicity_us: u32, seed: u32) -> Self {
        Self {
            setup_id,
            responders,
            ng: if ng == 0 { 1 } else { ng },
            instance: 0,
            next_responder: 0,
            periodicity_us,
            now_us: 0,
            rng: if seed == 0 { 0x9E37_79B9 } else { seed },
        }
    }

    /// Reported subcarriers per report: `ceil(245 / ng)`.
    pub const fn n_sc(&self) -> u16 {
        SC_SPAN.div_ceil(self.ng as u16)
    }

    fn rand(&mut self) -> u32 {
        // xorshift32
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// The next report. Cycles through the responders; after the last one, the next sounding
    /// instance begins `periodicity_us` later.
    pub fn next_frame(&mut self) -> CsiFrame {
        let r = self.next_responder;
        let ta = self.responders[r];
        let n_sc = self.n_sc();
        // Motion: a slow triangle wave over instances, scaled per responder.
        let phase = (self.instance % 64) as i32;
        let motion = if phase < 32 { phase } else { 64 - phase };

        let mut data = Vec::<u8, MAX_CSI_BYTES>::new();
        for k in 0..n_sc {
            let base = 96 + ((k as i32 * 7 + r as i32 * 29) % 64);
            let noise = (self.rand() % 5) as i32 - 2;
            let v = (base + motion * (r as i32 + 1) / 2 + noise).clamp(0, 255);
            let _ = data.push(v as u8);
        }

        let frame = CsiFrame::new(
            RxMeta {
                timestamp_us: self.now_us + r as u64 * 50,
                rssi: -40 - 3 * r as i8,
                noise_floor: -95,
                channel: 36,
                secondary: Secondary::None,
                bandwidth: Some(Bandwidth::Mhz20),
                ppdu: PpduFormat::HeSu,
                mcs: None,
                stbc: Some(false),
                sgi: None,
                n_rx: 1,
                n_ss: Some(1),
                antenna: None,
                sig_len: 0,
                rx_state: 0,
                // A sensing NDP is a sounding PPDU.
                not_sounding: Some(false),
                aggregation: Some(false),
                frame_seq: None,
                vendor: VendorRx::None,
            },
            Stimulus::Controlled {
                setup_id: self.setup_id,
                instance_id: self.instance,
                ta,
            },
            None,
            CsiPayload::Grouped {
                ng: self.ng,
                nb: 8,
                sc_start: SC_START,
                n_sc,
                n_rx: 1,
                n_tx: 1,
                data,
            },
        );

        self.next_responder += 1;
        if self.next_responder == R {
            self.next_responder = 0;
            self.instance = self.instance.wrapping_add(1);
            self.now_us += self.periodicity_us as u64;
        }
        frame
    }
}
