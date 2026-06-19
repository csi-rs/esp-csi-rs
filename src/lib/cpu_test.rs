//! CPU-utilization test TX hooks (`cpu-test-tx` feature).
//!
//! The CPU-utilization experiment drives the central TX loop from a schedule
//! that changes the broadcast rate and on-air payload every phase, and goes
//! silent during baseline phases. `run_esp_now_central` reads these atomics each
//! iteration so the example's schedule driver can steer the *real* library TX
//! path without re-running `node.run()` per phase. The whole module is gated so
//! production builds carry none of this.

use portable_atomic::{AtomicBool, AtomicU32, Ordering};

/// Runtime broadcast rate (Hz) for the CPU-test central TX loop.
pub(crate) static TEST_TX_RATE_HZ: AtomicU32 = AtomicU32::new(100);
/// Runtime on-air payload size (bytes) for the CPU-test central TX loop. The
/// `ControlPacket` frame is padded up to this length (capped at the ESP-NOW max).
pub(crate) static TEST_TX_PAYLOAD_B: AtomicU32 = AtomicU32::new(0);
/// When true, the CPU-test central TX loop sends nothing (baseline phases).
pub(crate) static TEST_TX_PAUSED: AtomicBool = AtomicBool::new(true);

/// Set the CPU-test central broadcast rate (Hz). See [`TEST_TX_RATE_HZ`].
pub fn set_test_tx_rate_hz(rate_hz: u16) {
    TEST_TX_RATE_HZ.store(rate_hz.max(1) as u32, Ordering::Relaxed);
}

/// Set the CPU-test central on-air payload size (bytes). See [`TEST_TX_PAYLOAD_B`].
pub fn set_test_tx_payload_b(payload_b: u16) {
    TEST_TX_PAYLOAD_B.store(payload_b as u32, Ordering::Relaxed);
}

/// Pause/unpause the CPU-test central TX loop (silent baseline phases).
pub fn set_test_tx_paused(paused: bool) {
    TEST_TX_PAUSED.store(paused, Ordering::Relaxed);
}
