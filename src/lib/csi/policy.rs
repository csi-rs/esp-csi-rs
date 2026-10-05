//! The state behind [`ReportingPolicy::Threshold`](crate::ReportingPolicy::Threshold) and
//! [`ReportingPolicy::Decimate`](crate::ReportingPolicy::Decimate).
//!
//! Pure integer arithmetic with no `esp-*` imports: it runs inside the Wi-Fi CSI callback, and the
//! host test crate compiles this file directly to test it.

/// Subcarriers sampled per measurement.
pub(crate) const SAMPLED: usize = 32;

/// Fixed-point fraction bits of the running average.
const FRAC: u32 = 4;
/// Running-average weight: each measurement moves the average 1/16 of the way.
const EWMA_SHIFT: u32 = 4;

/// How far measurements depart from a slow running average of the channel.
pub(crate) struct ThresholdState {
    base: [u32; SAMPLED],
    /// Buffer length the average was built from. A different length is a different layout, so the
    /// average restarts rather than compare unrelated subcarriers.
    buf_len: usize,
    hold_until_us: u64,
}

impl ThresholdState {
    pub(crate) const fn new() -> Self {
        Self {
            base: [0; SAMPLED],
            buf_len: 0,
            hold_until_us: 0,
        }
    }

    /// Forget the average and any open hold window.
    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    /// Score `buf` (interleaved `(imag, real)` pairs) against the running average, on a
    /// `0..=65535` scale, and fold it into the average. The first buffer of a layout scores 0.
    ///
    /// The score compares the **shape** of the channel, not its level: each side's per-subcarrier
    /// amplitudes are normalised by their own total before they are compared, so the receiver's
    /// per-frame gain changes score nothing. It is half the L1 distance between the two normalised
    /// profiles — `0` for the same shape, `65535` for profiles with no overlap at all.
    pub(crate) fn score(&mut self, buf: &[i8]) -> u16 {
        let pairs = buf.len() / 2;
        if pairs == 0 {
            return 0;
        }
        let n = pairs.min(SAMPLED);
        let step = pairs / n;
        let restart = buf.len() != self.buf_len;
        self.buf_len = buf.len();

        let mut amp = [0u32; SAMPLED];
        let mut a_tot: u64 = 0;
        let mut b_tot: u64 = 0;
        for (k, slot) in amp.iter_mut().enumerate().take(n) {
            let i = 2 * (k * step);
            *slot = ((buf[i] as i32).unsigned_abs() + (buf[i + 1] as i32).unsigned_abs()) << FRAC;
            a_tot += *slot as u64;
            b_tot += self.base[k] as u64;
        }

        let mut dist: u64 = 0;
        for (k, &a) in amp.iter().enumerate().take(n) {
            let b = &mut self.base[k];
            if restart {
                *b = a;
                continue;
            }
            dist += (a as u64 * b_tot).abs_diff(*b as u64 * a_tot);
            // b += (a - b) / 16, in unsigned arithmetic.
            if a >= *b {
                *b += (a - *b) >> EWMA_SHIFT;
            } else {
                *b -= (*b - a) >> EWMA_SHIFT;
            }
        }
        if restart || a_tot == 0 || b_tot == 0 {
            return 0;
        }
        // dist / (a_tot * b_tot) is the L1 distance in 0..=2; scale its half to 0..=65535.
        let denom = a_tot * b_tot;
        (dist.saturating_mul(u16::MAX as u64 / 2) / denom).min(u16::MAX as u64) as u16
    }

    /// Whether to report a measurement that scored `score` at time `now_us`. Crossing `level`
    /// reports and opens a `hold_ms` window in which every measurement is reported.
    pub(crate) fn decide(&mut self, score: u16, level: u16, hold_ms: u16, now_us: u64) -> bool {
        if score >= level {
            self.hold_until_us = now_us + hold_ms as u64 * 1000;
            return true;
        }
        now_us < self.hold_until_us
    }
}

/// Passes every `n`th measurement.
pub(crate) struct Decimator {
    count: u16,
}

impl Decimator {
    pub(crate) const fn new() -> Self {
        Self { count: 0 }
    }

    pub(crate) fn reset(&mut self) {
        self.count = 0;
    }

    /// Whether this measurement is one to keep. The first of every `n` is kept.
    pub(crate) fn tick(&mut self, n: u16) -> bool {
        let keep = self.count == 0;
        self.count += 1;
        if self.count >= n.max(1) {
            self.count = 0;
        }
        keep
    }
}
