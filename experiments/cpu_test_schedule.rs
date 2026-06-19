//! Shared schedule for the CPU-utilization experiment (spec v2).
//!
//! Both `esp_now_peripheral_exper_cpu` (DUT) and `esp_now_central_exper_cpu_tx`
//! (TX traffic generator) iterate this same sequence so they stay
//! lockstep without a control channel. After a fixed `BOOT_DELAY_S`
//! handshake delay both firmwares march phase-by-phase.
//!
//! Phase ordering: baseline_warmup, baseline_capture, then for each
//! (rate, payload, rep) in `RATES_HZ × PAYLOADS_B × 0..REPS` order:
//! cell_warmup, cell_capture. The ordering MUST be bit-for-bit identical
//! across DUT and TX — that is what keeps them synchronised without a
//! handshake (spec §6.1, §10).

/// No-load (TX silent) baseline phases. These characterise each build's
/// fixed overhead; they are informational and never gate the run (spec §5).
pub const BASELINE_WARMUP_S: u32 = 10;
pub const BASELINE_CAPTURE_S: u32 = 20;
pub const CELL_WARMUP_S: u32 = 15;
pub const CELL_CAPTURE_S: u32 = 40;

pub const RATES_HZ: [u32; 5] = [10, 50, 100, 250, 500];
pub const PAYLOADS_B: [u32; 3] = [32, 128, 512];
pub const REPS: u32 = 2;

pub const BOOT_DELAY_S: u32 = 10;
pub const TEST_CHANNEL: u8 = 11;

/// Upper bound on per-window `busy_ppm` samples the DUT accumulates during
/// `baseline_capture` before emitting `CPU_BASELINE` (spec §5). Consumed only
/// by the DUT; the shared module is `#[path]`-included by the TX example too.
#[allow(dead_code)]
pub const BASELINE_MAX_SAMPLES: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PhaseKind {
    BaselineWarmup,
    BaselineCapture,
    CellWarmup,
    CellCapture,
}

impl PhaseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PhaseKind::BaselineWarmup => "baseline_warmup",
            PhaseKind::BaselineCapture => "baseline_capture",
            PhaseKind::CellWarmup => "cell_warmup",
            PhaseKind::CellCapture => "cell_capture",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Phase {
    pub kind: PhaseKind,
    pub rate_hz: u32,
    pub payload_b: u32,
    pub rep: u32,
    pub duration_s: u32,
}

pub const fn total_phases() -> usize {
    2 + (RATES_HZ.len() * PAYLOADS_B.len() * REPS as usize) * 2
}

pub struct PhaseIter {
    pos: usize,
}

impl Iterator for PhaseIter {
    type Item = (usize, Phase);
    fn next(&mut self) -> Option<Self::Item> {
        let total = total_phases();
        if self.pos >= total {
            return None;
        }
        let idx = self.pos;
        let phase = if self.pos == 0 {
            Phase {
                kind: PhaseKind::BaselineWarmup,
                rate_hz: 0,
                payload_b: 0,
                rep: 0,
                duration_s: BASELINE_WARMUP_S,
            }
        } else if self.pos == 1 {
            Phase {
                kind: PhaseKind::BaselineCapture,
                rate_hz: 0,
                payload_b: 0,
                rep: 0,
                duration_s: BASELINE_CAPTURE_S,
            }
        } else {
            let cell_idx = (self.pos - 2) / 2;
            let is_capture = (self.pos - 2) % 2 == 1;
            let rep = (cell_idx as u32) % REPS;
            let pi = (cell_idx / REPS as usize) % PAYLOADS_B.len();
            let ri = (cell_idx / (REPS as usize * PAYLOADS_B.len())) % RATES_HZ.len();
            let rate = RATES_HZ[ri];
            let payload = PAYLOADS_B[pi];
            let (kind, duration_s) = if is_capture {
                (PhaseKind::CellCapture, CELL_CAPTURE_S)
            } else {
                (PhaseKind::CellWarmup, CELL_WARMUP_S)
            };
            Phase {
                kind,
                rate_hz: rate,
                payload_b: payload,
                rep,
                duration_s,
            }
        };
        self.pos += 1;
        Some((idx, phase))
    }
}

pub fn phases_iter() -> PhaseIter {
    PhaseIter { pos: 0 }
}
