//! Session context stamped onto every delivered frame: the node's identity, the session it
//! belongs to, a receive clock that does not wrap, and what excited the channel.
//!
//! All state is process-wide and read from the Wi-Fi CSI callback, so it is held in relaxed
//! atomics. The callback is the only writer of the timestamp-widening state, which is what makes
//! the load-compare-store there safe without a critical section.

use embassy_time::Instant;
use portable_atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use crate::wire::{Envelope, SessionInfo, SourceKind, Stimulus};

/// Base MAC, packed big-endian into the low 48 bits. Zero until first read.
static NODE_ID: AtomicU64 = AtomicU64::new(0);

static SESSION_ID: AtomicU32 = AtomicU32::new(0);
/// Set by [`set_session`]: keep `SESSION_ID` across runs instead of drawing a fresh one.
static SESSION_PINNED: AtomicBool = AtomicBool::new(false);

/// Wall-clock time supplied by the controller, and the monotonic instant it was supplied at.
static EPOCH_UNIX_US: AtomicU64 = AtomicU64::new(0);
static EPOCH_AT_TICKS: AtomicU64 = AtomicU64::new(0);
static EPOCH_SET: AtomicBool = AtomicBool::new(false);

/// Frames emitted this run. The envelope's `stream_seq`.
static STREAM_SEQ: AtomicU32 = AtomicU32::new(0);

/// Receive-timestamp widening: the last raw 32-bit value and how many times it has wrapped.
static TS_LAST_RAW: AtomicU32 = AtomicU32::new(0);
static TS_WRAPS: AtomicU32 = AtomicU32::new(0);

/// Whether the next frame should carry the session announcement.
static ANCHOR_PENDING: AtomicBool = AtomicBool::new(false);

/// 0 = ambient traffic, 1 = a controlled sounding (ESP-NOW).
static STIMULUS_CONTROLLED: AtomicBool = AtomicBool::new(false);
static SETUP_ID: AtomicU8 = AtomicU8::new(0);
static SOUNDING_INSTANCE: AtomicU32 = AtomicU32::new(0);

/// Whether to capture the measured MPDU's header digest.
static HEADER_DIGEST: AtomicBool = AtomicBool::new(false);

/// Name the measurement session, and optionally anchor it to wall-clock time.
///
/// Called by whatever starts the run — the controller. `session_id` is stamped on every frame's
/// envelope until changed, across runs. `epoch_unix_us`, the current UNIX time in microseconds, is
/// paired with the receive clock of the next run's first frame and announced in its
/// [`SessionInfo`], so a host can put every later frame on wall time.
///
/// Without a call, each run draws a random session id and announces no wall time.
pub fn set_session(session_id: u32, epoch_unix_us: Option<u64>) {
    SESSION_ID.store(session_id, Ordering::Relaxed);
    SESSION_PINNED.store(true, Ordering::Relaxed);
    match epoch_unix_us {
        Some(us) => {
            EPOCH_UNIX_US.store(us, Ordering::Relaxed);
            EPOCH_AT_TICKS.store(Instant::now().as_micros(), Ordering::Relaxed);
            EPOCH_SET.store(true, Ordering::Release);
        }
        None => EPOCH_SET.store(false, Ordering::Release),
    }
}

/// The session id currently stamped on frames.
pub fn session_id() -> u32 {
    SESSION_ID.load(Ordering::Relaxed)
}

/// This node's identity on the wire: its base MAC address.
pub fn node_id() -> [u8; 6] {
    let mut packed = NODE_ID.load(Ordering::Relaxed);
    if packed == 0 {
        let mac = esp_hal::efuse::base_mac_address();
        let mut v = 0u64;
        for &b in mac.as_bytes() {
            v = (v << 8) | b as u64;
        }
        NODE_ID.store(v, Ordering::Relaxed);
        packed = v;
    }
    let mut out = [0u8; 6];
    for (i, b) in out.iter_mut().enumerate() {
        *b = (packed >> (8 * (5 - i))) as u8;
    }
    out
}

/// Capture each measured MPDU's MAC header (frame control, three addresses, sequence control —
/// 24 bytes per frame) into the frame's `header`. Lets a host drop retries, tell uplink from
/// downlink and attribute every frame. On by default for a sniffer, off for the other modes.
pub fn set_header_digest_enabled(enabled: bool) {
    HEADER_DIGEST.store(enabled, Ordering::Relaxed);
}

/// Whether the header digest is captured.
pub fn header_digest_enabled() -> bool {
    HEADER_DIGEST.load(Ordering::Relaxed)
}

/// Prepare the context for a run: draw a session id if none is pinned, restart the frame counter
/// and the timestamp widening, and queue the session announcement for the first frame.
///
/// `controlled` says whether this mode sounds the channel itself (ESP-NOW) or measures ambient
/// traffic, and `header_digest` is the mode's default for [`set_header_digest_enabled`].
pub(crate) fn begin_run(controlled: bool, header_digest: bool) {
    let _ = node_id();
    if !SESSION_PINNED.load(Ordering::Relaxed) {
        SESSION_ID.store(esp_hal::rng::Rng::new().random(), Ordering::Relaxed);
    }
    STREAM_SEQ.store(0, Ordering::Relaxed);
    TS_LAST_RAW.store(0, Ordering::Relaxed);
    TS_WRAPS.store(0, Ordering::Relaxed);
    STIMULUS_CONTROLLED.store(controlled, Ordering::Relaxed);
    SETUP_ID.store(0, Ordering::Relaxed);
    SOUNDING_INSTANCE.store(0, Ordering::Relaxed);
    HEADER_DIGEST.store(header_digest, Ordering::Relaxed);
    ANCHOR_PENDING.store(true, Ordering::Release);
}

/// Record the sounding the next measurements belong to (ESP-NOW control-frame number).
pub(crate) fn set_sounding_instance(instance: u32) {
    SOUNDING_INSTANCE.store(instance, Ordering::Relaxed);
}

/// Name the measurement setup that controlled soundings belong to.
pub(crate) fn set_setup_id(id: u8) {
    SETUP_ID.store(id, Ordering::Relaxed);
}

/// Widen the radio's 32-bit microsecond receive timestamp to 64 bits.
///
/// The raw value wraps every ~71.6 minutes. A decrease of more than half the range is a wrap; a
/// smaller one is a frame delivered slightly out of order and is not counted. Called only from the
/// CSI callback.
pub(crate) fn widen_timestamp(raw: u32) -> u64 {
    const HALF: u32 = u32::MAX / 2;
    let last = TS_LAST_RAW.load(Ordering::Relaxed);
    let mut wraps = TS_WRAPS.load(Ordering::Relaxed);
    if raw < last {
        if last - raw > HALF {
            // Wrapped.
            wraps = wraps.wrapping_add(1);
            TS_WRAPS.store(wraps, Ordering::Relaxed);
            TS_LAST_RAW.store(raw, Ordering::Relaxed);
        }
        // Otherwise a slightly late frame from the current epoch: stamp it, keep `last`.
    } else if raw - last > HALF && wraps > 0 {
        // A straggler from before the most recent wrap, delivered after it.
        return (((wraps - 1) as u64) << 32) | raw as u64;
    } else {
        TS_LAST_RAW.store(raw, Ordering::Relaxed);
    }
    ((wraps as u64) << 32) | raw as u64
}

/// The envelope for the next emitted frame. Advances `stream_seq`.
pub(crate) fn next_envelope(source: SourceKind) -> Envelope {
    Envelope::new(
        node_id(),
        SESSION_ID.load(Ordering::Relaxed),
        source,
        STREAM_SEQ.fetch_add(1, Ordering::Relaxed),
    )
}

/// Whether this run measures a controlled stimulus.
pub(crate) fn controlled() -> bool {
    STIMULUS_CONTROLLED.load(Ordering::Relaxed)
}

/// What excited the channel for a frame from transmitter `ta`. `instance` is the sounding number
/// read off the measured frame itself, when it carries one; otherwise the last one recorded.
pub(crate) fn stimulus(ta: [u8; 6], instance: Option<u32>) -> Stimulus {
    if STIMULUS_CONTROLLED.load(Ordering::Relaxed) {
        Stimulus::Controlled {
            setup_id: SETUP_ID.load(Ordering::Relaxed),
            instance_id: instance.unwrap_or_else(|| SOUNDING_INSTANCE.load(Ordering::Relaxed)) as u16,
            ta,
        }
    } else {
        Stimulus::Ambient { ta }
    }
}

/// The session announcement, if the frame received at `timestamp_us` is the run's first.
///
/// The wall-clock anchor is the controller's epoch advanced by the time elapsed since it was
/// supplied, paired with this frame's receive time. Its error is the CSI callback's delivery
/// latency, normally well under a millisecond.
pub(crate) fn take_announcement(timestamp_us: u64) -> Option<SessionInfo> {
    if !ANCHOR_PENDING.swap(false, Ordering::AcqRel) {
        return None;
    }
    let epoch = if EPOCH_SET.load(Ordering::Acquire) {
        let elapsed = Instant::now()
            .as_micros()
            .saturating_sub(EPOCH_AT_TICKS.load(Ordering::Relaxed));
        Some(EPOCH_UNIX_US.load(Ordering::Relaxed) + elapsed)
    } else {
        None
    };
    Some(SessionInfo::new(
        super::esp::CHIP,
        crate_version(),
        timestamp_us,
        epoch,
    ))
}

/// This crate's version as `[major, minor, patch]`.
pub(crate) const fn crate_version() -> [u8; 3] {
    const fn parse(s: &str) -> u8 {
        let b = s.as_bytes();
        let mut v = 0u8;
        let mut i = 0;
        while i < b.len() {
            v = v * 10 + (b[i] - b'0');
            i += 1;
        }
        v
    }
    [
        parse(env!("CARGO_PKG_VERSION_MAJOR")),
        parse(env!("CARGO_PKG_VERSION_MINOR")),
        parse(env!("CARGO_PKG_VERSION_PATCH")),
    ]
}
