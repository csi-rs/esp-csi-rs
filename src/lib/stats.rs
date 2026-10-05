//! Global statistics counters and sequence-drop detection state.
//!
//! Everything here is gated behind the `statistics` feature except
//! `set_seq_drop_detection`, which is always compiled (the run loops call it
//! unconditionally) and simply no-ops without the feature.

#[cfg(feature = "statistics")]
use embassy_time::Instant;
#[cfg(feature = "statistics")]
use portable_atomic::AtomicBool;
#[cfg(feature = "statistics")]
use portable_atomic::{AtomicU32, AtomicU64, Ordering};

#[cfg(feature = "statistics")]
use crate::logging::logging::reset_global_log_drops;

/// Global statistics counters (enabled with the `statistics` feature).
#[cfg(feature = "statistics")]
pub(crate) struct GlobalStats {
    /// Total transmitted packets.
    pub tx_count: AtomicU64,
    /// Total received packets.
    pub rx_count: AtomicU64,
    /// Captured measurements that were lost: the sum of `rx_oversize`, `rx_queue_full` and
    /// `rx_seq_gap`. Frames a filter or policy chose not to deliver are not losses and are not in
    /// it.
    pub rx_drop_count: AtomicU32,
    /// Rejected by the attribution filter (`set_csi_peer_filter` / `set_csi_min_sig_mode`).
    pub rx_filtered: AtomicU32,
    /// Buffer larger than the packet can carry.
    pub rx_oversize: AtomicU32,
    /// Delivery queue (async consumer or log channel) full.
    pub rx_queue_full: AtomicU32,
    /// Frames missing on the air, estimated from per-transmitter sequence-number gaps.
    pub rx_seq_gap: AtomicU32,
    /// Withheld by the reporting policy (threshold or decimation). Deliberate, not a loss.
    pub rx_policy_suppressed: AtomicU32,
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
    rx_filtered: AtomicU32::new(0),
    rx_oversize: AtomicU32::new(0),
    rx_queue_full: AtomicU32::new(0),
    rx_seq_gap: AtomicU32::new(0),
    rx_policy_suppressed: AtomicU32::new(0),
    capture_start_time: AtomicU64::new(0),
    tx_rate_hz: AtomicU32::new(0),
    rx_rate_hz: AtomicU32::new(0),
};

// Set when CSI output is toggled: the next frame from each transmitter resyncs its sequence instead
// of counting the frames that went undelivered while output was off as lost.
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
        STATS.rx_filtered.store(0, Ordering::Relaxed);
        STATS.rx_oversize.store(0, Ordering::Relaxed);
        STATS.rx_queue_full.store(0, Ordering::Relaxed);
        STATS.rx_seq_gap.store(0, Ordering::Relaxed);
        STATS.rx_policy_suppressed.store(0, Ordering::Relaxed);
        PER_TX.lock(|t| t.borrow_mut().clear());
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
/// [`CSINode::emitter`](crate::CSINode::emitter)) has no other way to reach the TX counters, so
/// its frames would be invisible to `get_total_tx_packets` / `get_pps_tx` — the
/// exact counters someone reaches for when a collector reports nothing. Call this
/// once per frame the driver accepted.
#[cfg(feature = "statistics")]
pub fn record_emitter_tx() {
    record_tx();
}

/// Record one received CSI report from an **out-of-tree collector**.
///
/// A radio profile that registers its own CSI callback never enters
/// `capture_csi_info`, so until this existed such a collector's `show-stats`
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
    record_loss(&STATS.rx_queue_full, 1);
}

/// Reset every counter and stamp the capture start, at the START of a run.
///
/// Called by `CSINode::run_inner` for the in-tree modes and directly by an out-of-tree run
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

/// Captured measurements lost before delivery: buffer overflow, a full delivery queue, and frames
/// missing on the air (statistics feature). See [`get_drop_breakdown`] for each cause.
///
/// Before 0.12 this also counted frames the attribution filter rejected, which are a choice, not a
/// loss. Those are now [`DropBreakdown::filtered`].
#[cfg(feature = "statistics")]
pub fn get_dropped_packets_rx() -> u32 {
    STATS.rx_drop_count.load(Ordering::Relaxed)
}

/// Why captured measurements did not reach the consumer, one counter per cause.
#[cfg(feature = "statistics")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DropBreakdown {
    /// Rejected by the attribution filter. Deliberate.
    pub filtered: u32,
    /// Withheld by the reporting policy (threshold or decimation). Deliberate.
    pub policy_suppressed: u32,
    /// Buffer larger than the packet can carry. A loss.
    pub oversize: u32,
    /// Delivery queue full. A loss, and the one that says the consumer is too slow.
    pub queue_full: u32,
    /// Frames missing on the air, from per-transmitter sequence gaps. A loss upstream of the node.
    pub seq_gap: u32,
    /// Lines the logger refused or could not queue. Counted separately from the above by the
    /// logging backend.
    pub log_dropped: u32,
}

/// Snapshot of every drop counter (statistics feature).
#[cfg(feature = "statistics")]
pub fn get_drop_breakdown() -> DropBreakdown {
    DropBreakdown {
        filtered: STATS.rx_filtered.load(Ordering::Relaxed),
        policy_suppressed: STATS.rx_policy_suppressed.load(Ordering::Relaxed),
        oversize: STATS.rx_oversize.load(Ordering::Relaxed),
        queue_full: STATS.rx_queue_full.load(Ordering::Relaxed),
        seq_gap: STATS.rx_seq_gap.load(Ordering::Relaxed),
        log_dropped: crate::logging::logging::get_log_packet_drops(),
    }
}

/// Record a loss of `n` frames under one cause, keeping `rx_drop_count` the sum of the losses.
#[cfg(feature = "statistics")]
pub(crate) fn record_loss(counter: &AtomicU32, n: u32) {
    counter.fetch_add(n, Ordering::Relaxed);
    STATS.rx_drop_count.fetch_add(n, Ordering::Relaxed);
}

/// Per-transmitter receive statistics: what one sender's frames looked like at this node.
///
/// An 802.11 sequence number is assigned per transmitter, so loss on the air is only meaningful per
/// transmitter — and an IEEE 802.11bf initiator combines measurements from several responders, so
/// one global counter cannot say whose frames went missing.
#[cfg(feature = "statistics")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TxStats {
    /// Transmitter address.
    pub mac: [u8; 6],
    /// Measurements captured from it.
    pub frames: u32,
    /// Frames missing from its sequence-number stream.
    pub seq_gaps: u32,
    /// Retransmissions seen (needs the header digest; see `set_header_digest_enabled`). Retries
    /// are skipped by the gap count, since a retry repeats its sequence number.
    pub retries: u32,
    last_seq: u16,
    last_used: u32,
    resync: bool,
}

/// Transmitters tracked at once. The least recently heard is evicted to make room, so a busy
/// channel rotates through its transmitters instead of clearing everyone's history.
#[cfg(feature = "statistics")]
pub const MAX_TRACKED_TX: usize = 16;

#[cfg(feature = "statistics")]
type PerTxTable = heapless::Vec<TxStats, MAX_TRACKED_TX>;

#[cfg(feature = "statistics")]
static PER_TX: embassy_sync::blocking_mutex::Mutex<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    core::cell::RefCell<PerTxTable>,
> = embassy_sync::blocking_mutex::Mutex::new(core::cell::RefCell::new(heapless::Vec::new()));

#[cfg(feature = "statistics")]
static PER_TX_CLOCK: AtomicU32 = AtomicU32::new(0);

/// Fold one captured frame into its transmitter's statistics. `seq` is the 802.11 sequence number,
/// `retry` the header's retry bit when known. Gaps of 500 or more are treated as a restart rather
/// than counted.
#[cfg(feature = "statistics")]
pub(crate) fn record_tx_frame(mac: [u8; 6], seq: Option<u16>, retry: Option<bool>) {
    let now = PER_TX_CLOCK.fetch_add(1, Ordering::Relaxed);
    let resync_all = RESET_SEQ_TRACKER.swap(false, Ordering::Relaxed);
    let mut gap = 0u32;
    PER_TX.lock(|t| {
        let mut t = t.borrow_mut();
        if resync_all {
            for e in t.iter_mut() {
                e.resync = true;
            }
        }
        let idx = match t.iter().position(|e| e.mac == mac) {
            Some(i) => i,
            None => {
                let fresh = TxStats {
                    mac,
                    last_seq: seq.unwrap_or(0),
                    ..TxStats::default()
                };
                if t.push(fresh).is_err() {
                    // Evict the least recently heard transmitter.
                    let lru = t
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, e)| e.last_used)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    t[lru] = fresh;
                    lru
                } else {
                    t.len() - 1
                }
            }
        };
        let e = &mut t[idx];
        let first = e.frames == 0 || core::mem::take(&mut e.resync);
        e.frames += 1;
        e.last_used = now;
        if retry == Some(true) {
            e.retries += 1;
            return;
        }
        if let Some(seq) = seq {
            if !first {
                let diff = seq.wrapping_sub(e.last_seq) & 0x0FFF;
                if diff > 1 && diff < 500 {
                    gap = (diff - 1) as u32;
                    e.seq_gaps += gap;
                }
            }
            e.last_seq = seq;
        }
    });
    if gap > 0 {
        record_loss(&STATS.rx_seq_gap, gap);
    }
}

/// Copy the per-transmitter table into `out`, returning how many entries were written (statistics
/// feature).
#[cfg(feature = "statistics")]
pub fn snapshot_tx_stats(out: &mut [TxStats]) -> usize {
    PER_TX.lock(|t| {
        let t = t.borrow();
        let n = t.len().min(out.len());
        out[..n].copy_from_slice(&t[..n]);
        n
    })
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
