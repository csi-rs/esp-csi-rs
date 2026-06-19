//! Global statistics counters and sequence-drop detection state.
//!
//! Everything here is gated behind the `statistics` feature except
//! [`set_seq_drop_detection`], which is always compiled (the run loops call it
//! unconditionally) and simply no-ops without the feature.

#[cfg(feature = "statistics")]
use embassy_time::Instant;
#[cfg(feature = "statistics")]
use portable_atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

#[cfg(feature = "statistics")]
use crate::logging::logging::reset_global_log_drops;

#[cfg(feature = "statistics")]
pub(crate) const MAX_TRACKED_PEERS: usize = 16;

/// Global statistics counters (enabled with the `statistics` feature).
#[cfg(feature = "statistics")]
pub(crate) struct GlobalStats {
    /// Total transmitted packets.
    pub tx_count: AtomicU64,
    /// Total received packets.
    pub rx_count: AtomicU64,
    /// Estimated number of dropped RX packets.
    pub rx_drop_count: AtomicU32,
    /// Capture start time (ticks).
    pub capture_start_time: AtomicU64,
    /// Current TX packet rate (Hz).
    pub tx_rate_hz: AtomicU32,
    /// Current RX packet rate (Hz).
    pub rx_rate_hz: AtomicU32,
}

#[cfg(feature = "statistics")]
pub(crate) static STATS: GlobalStats = GlobalStats {
    tx_count: AtomicU64::new(0),
    rx_count: AtomicU64::new(0),
    rx_drop_count: AtomicU32::new(0),
    capture_start_time: AtomicU64::new(0),
    tx_rate_hz: AtomicU32::new(0),
    rx_rate_hz: AtomicU32::new(0),
};

// Signals run_process_csi_packet to clear PEER_SEQ_TRACKER on the next ISR entry.
#[cfg(feature = "statistics")]
pub(crate) static RESET_SEQ_TRACKER: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "statistics")]
static SEQ_DROP_DETECTION_ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_seq_drop_detection(enabled: bool) {
    #[cfg(feature = "statistics")]
    {
        SEQ_DROP_DETECTION_ENABLED.store(enabled, Ordering::Relaxed);
    }

    #[cfg(not(feature = "statistics"))]
    {
        let _ = enabled;
    }
}

#[cfg(feature = "statistics")]
pub(crate) fn seq_drop_detection_enabled() -> bool {
    SEQ_DROP_DETECTION_ENABLED.load(Ordering::Relaxed)
}

/// Reset the statistics counters. Called by `reset_globals` between runs.
pub(crate) fn reset() {
    #[cfg(feature = "statistics")]
    {
        STATS.tx_count.store(0, Ordering::Relaxed);
        STATS.rx_count.store(0, Ordering::Relaxed);
        STATS.rx_drop_count.store(0, Ordering::Relaxed);
        STATS.tx_rate_hz.store(0, Ordering::Relaxed);
        STATS.rx_rate_hz.store(0, Ordering::Relaxed);
        reset_global_log_drops();
    }
}

/// Total received CSI packets (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_total_rx_packets() -> u64 {
    STATS.rx_count.load(Ordering::Relaxed)
}

/// Total transmitted packets (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_total_tx_packets() -> u64 {
    STATS.tx_count.load(Ordering::Relaxed)
}

/// Current RX packet rate in Hz (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_rx_rate_hz() -> u32 {
    STATS.rx_rate_hz.load(Ordering::Relaxed)
}

/// Current TX packet rate in Hz (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_tx_rate_hz() -> u32 {
    STATS.tx_rate_hz.load(Ordering::Relaxed)
}

/// Packets per second received since capture start (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_pps_rx() -> u64 {
    let start_time = Instant::from_ticks(STATS.capture_start_time.load(Ordering::Relaxed));
    let elapsed_secs = start_time.elapsed().as_secs();
    let total_packets = STATS.rx_count.load(Ordering::Relaxed);
    if elapsed_secs == 0 {
        return total_packets;
    }
    total_packets / elapsed_secs
}

/// Packets per second transmitted since capture start (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_pps_tx() -> u64 {
    let start_time = Instant::from_ticks(STATS.capture_start_time.load(Ordering::Relaxed));
    let elapsed_secs = start_time.elapsed().as_secs();
    let total_packets = STATS.tx_count.load(Ordering::Relaxed);
    if elapsed_secs == 0 {
        return total_packets;
    }
    total_packets / elapsed_secs
}

/// Dropped RX packets estimate (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_dropped_packets_rx() -> u32 {
    STATS.rx_drop_count.load(Ordering::Relaxed)
}
