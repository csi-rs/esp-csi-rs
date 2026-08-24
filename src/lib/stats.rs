//! Global statistics counters and sequence-drop detection state.
//!
//! Everything here is gated behind the `statistics` feature except
//! `set_seq_drop_detection`, which is always compiled (the run loops call it
//! unconditionally) and simply no-ops without the feature.

#[cfg(feature = "statistics")]
use embassy_time::Instant;
#[cfg(all(feature = "statistics", not(feature = "esp32c5")))]
use portable_atomic::AtomicBool;
#[cfg(feature = "statistics")]
use portable_atomic::{AtomicU32, AtomicU64, Ordering};

#[cfg(feature = "statistics")]
use crate::logging::logging::reset_global_log_drops;

#[cfg(all(feature = "statistics", not(feature = "esp32c5")))]
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
pub(crate) static BB_FORMAT_HIST: [AtomicU32; 8] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

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
#[cfg(all(feature = "statistics", not(feature = "esp32c5")))]
pub(crate) static RESET_SEQ_TRACKER: AtomicBool = AtomicBool::new(false);
#[cfg(all(feature = "statistics", not(feature = "esp32c5")))]
static SEQ_DROP_DETECTION_ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_seq_drop_detection(enabled: bool) {
    #[cfg(all(feature = "statistics", not(feature = "esp32c5")))]
    {
        SEQ_DROP_DETECTION_ENABLED.store(enabled, Ordering::Relaxed);
    }

    #[cfg(not(all(feature = "statistics", not(feature = "esp32c5"))))]
    {
        let _ = enabled;
    }
}

#[cfg(all(feature = "statistics", not(feature = "esp32c5")))]
pub(crate) fn seq_drop_detection_enabled() -> bool {
    SEQ_DROP_DETECTION_ENABLED.load(Ordering::Relaxed)
}

/// Reset the statistics counters. Called only by `stats_begin_run`, at the start of a run.
///
/// Gated on `statistics` because its sole caller is: moving the reset out of `reset_globals` (which
/// is unconditional) left this with zero callers when the feature is off, and `cargo hack
/// --each-feature` builds exactly that combination.
#[cfg(feature = "statistics")]
pub(crate) fn reset() {
    #[cfg(feature = "statistics")]
    {
        STATS.tx_count.store(0, Ordering::Relaxed);
        STATS.rx_count.store(0, Ordering::Relaxed);
        STATS.rx_drop_count.store(0, Ordering::Relaxed);
        STATS.tx_rate_hz.store(0, Ordering::Relaxed);
        STATS.rx_rate_hz.store(0, Ordering::Relaxed);
        for slot in BB_FORMAT_HIST.iter() {
            slot.store(0, Ordering::Relaxed);
        }
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

/// Record one transmitted frame.
///
/// Called by the emitter's inject loop for each frame the driver accepted, so
/// TX counters reflect frames actually handed to the radio rather than loop
/// iterations.
#[cfg(feature = "statistics")]
pub(crate) fn record_tx() {
    STATS.tx_count.fetch_add(1, Ordering::Relaxed);
}

/// Record one transmitted frame from an **out-of-tree emitter**.
///
/// An emitter that owns the radio directly (rather than running through
/// [`crate::emitter::run_emitter`]) has no other way to reach the TX counters, so
/// its frames would be invisible to `get_total_tx_packets` / `get_pps_tx` — the
/// exact counters someone reaches for when a collector reports nothing. Call this
/// once per frame the driver accepted.
#[cfg(feature = "statistics")]
pub fn record_emitter_tx() {
    record_tx();
}

/// Record one received CSI report from an **out-of-tree collector**.
///
/// The HE20 path registers its own CSI callback and never enters
/// `capture_csi_info`, so until this existed an HE20 collector's `show-stats`
/// reported `RX Total Packets: 0` however many frames it forwarded — the
/// firmware's receive accounting was simply dead on the path production runs.
#[cfg(feature = "statistics")]
pub fn record_collector_rx() {
    STATS.rx_count.fetch_add(1, Ordering::Relaxed);
}

/// Record one CSI report an out-of-tree collector DROPPED (payload over the
/// wire cap, filtered, or otherwise not forwarded). Sibling of
/// [`record_collector_rx`], same reasoning.
#[cfg(feature = "statistics")]
pub fn record_collector_rx_drop() {
    STATS.rx_drop_count.fetch_add(1, Ordering::Relaxed);
}

/// Reset every counter and stamp the capture start, at the START of a run.
///
/// Called by `CSINode::run_inner` for the in-tree roles and directly by the out-of-tree HE20 run
/// loop, which enters neither `run_inner` nor `run_process_csi_packet` and so had no counter reset
/// and no start time to divide `pps` by.
///
/// This is the ONLY place counters are cleared. `reset_globals` used to also clear them, but it runs
/// at the end of a run, so it wiped the numbers a user was about to read — see the note there.
#[cfg(feature = "statistics")]
pub fn stats_begin_run() {
    reset();
    STATS
        .capture_start_time
        .store(Instant::now().as_ticks(), Ordering::Relaxed);
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

/// Snapshot `cur_bb_format` histogram (statistics feature).
#[cfg(feature = "statistics")]
pub fn snapshot_bb_format_histogram() -> [u32; 8] {
    let mut out = [0u32; 8];
    for (slot, count) in BB_FORMAT_HIST.iter().zip(out.iter_mut()) {
        *count = slot.load(Ordering::Relaxed);
    }
    out
}

// Only the newer MAC (C5/C6) exposes `cur_bb_format`; the classic esp32 / C3 /
// S3 radios never call this, so gate the definition to match the call site in
// `csi::delivery` and avoid a dead-code error there.
#[cfg(all(feature = "statistics", any(feature = "esp32c5", feature = "esp32c6")))]
pub(crate) fn record_cur_bb_format(fmt: u32) {
    if fmt < 8 {
        BB_FORMAT_HIST[fmt as usize].fetch_add(1, Ordering::Relaxed);
    }
}
