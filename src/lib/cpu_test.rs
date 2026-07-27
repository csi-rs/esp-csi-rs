//! CPU-utilization test TX hooks (`cpu-test-tx` feature).
//!
//! The CPU-utilization experiment drives the emitter's TX loop from a schedule
//! that changes the injection rate and on-air frame size every phase, and goes
//! silent during baseline phases. [`crate::emitter::run_emitter`] reads these
//! atomics each iteration so the experiment's schedule driver can steer the
//! *real* library TX path without re-running `node.run()` per phase. The whole
//! module is gated so production builds carry none of this.

use portable_atomic::{AtomicBool, AtomicU32, Ordering};

/// Runtime injection rate (Hz) for the CPU-test emitter TX loop.
pub(crate) static TEST_TX_RATE_HZ: AtomicU32 = AtomicU32::new(100);
/// Runtime on-air frame size (bytes) for the CPU-test emitter TX loop. The
/// injected frame is padded up to this length, capped at the buffer size.
pub(crate) static TEST_TX_PAYLOAD_B: AtomicU32 = AtomicU32::new(0);
/// When true, the CPU-test emitter TX loop sends nothing (baseline phases).
pub(crate) static TEST_TX_PAUSED: AtomicBool = AtomicBool::new(true);

/// Set the CPU-test emitter injection rate (Hz). See [`TEST_TX_RATE_HZ`].
pub fn set_test_tx_rate_hz(rate_hz: u16) {
    TEST_TX_RATE_HZ.store(rate_hz.max(1) as u32, Ordering::Relaxed);
}

/// Set the CPU-test emitter on-air frame size (bytes). See [`TEST_TX_PAYLOAD_B`].
pub fn set_test_tx_payload_b(payload_b: u16) {
    TEST_TX_PAYLOAD_B.store(payload_b as u32, Ordering::Relaxed);
}

/// Pause/unpause the CPU-test emitter TX loop (silent baseline phases).
pub fn set_test_tx_paused(paused: bool) {
    TEST_TX_PAUSED.store(paused, Ordering::Relaxed);
}
