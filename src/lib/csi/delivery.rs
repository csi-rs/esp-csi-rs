//! CSI delivery state machine.
//!
//! The WiFi-task callback (`capture_csi_info`) dispatches each captured CSI
//! report down exactly one user-facing path — inline callback, async queue, or
//! inline logging — selected by a single relaxed atomic load. This module owns
//! that dispatch state, the public registration functions, the lock-free CSI
//! queue + waker, the [`crate::CSINodeClient`] consumer handle, and the
//! `WifiController` CSI wiring (`set_csi` / `build_csi_config`).

use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_sync::waitqueue::AtomicWaker;
#[cfg(feature = "statistics")]
use embassy_time::Instant;
use embassy_time::Timer;
use heapless::Vec;
use portable_atomic::{AtomicBool, Ordering};

use esp_radio::wifi::WifiController;
use esp_radio::wifi::csi::CsiConfig;

use super::{CSIDataPacket, RxCSIFmt};
use crate::STOP_SIGNAL;
use crate::config::CsiConfig as CsiConfiguration;
#[cfg(feature = "statistics")]
use crate::stats::STATS;
#[cfg(all(feature = "statistics", not(feature = "esp32c5")))]
use crate::stats::{MAX_TRACKED_PEERS, RESET_SEQ_TRACKER, seq_drop_detection_enabled};
#[cfg(all(feature = "statistics", not(feature = "esp32c5")))]
use heapless::LinearMap;

/// Lock-free 32-slot MPMC ring used by the WiFi callback to deliver
/// captured `CSIDataPacket`s to user code via
/// [`CSINodeClient::next_csi_packet`]. The producer is the WiFi-task
/// callback, the consumer is one async task, and the queue is
/// **lock-free** — no critical section on enqueue, so the WiFi-task
/// hot path is never delayed.
///
/// 32 × `sizeof(CSIDataPacket)` ≈ 20 KB BSS. Drop-on-full; drops are
/// counted via `STATS.rx_drop_count`.
static CSI_QUEUE: heapless::mpmc::Q32<CSIDataPacket> = heapless::mpmc::Q32::new();
/// Single-slot waker for the CSI consumer. Registered by
/// [`CSINodeClient::next_csi_packet`] and woken from the WiFi callback
/// after a successful `CSI_QUEUE.enqueue`.
static CSI_WAKER: AtomicWaker = AtomicWaker::new();

/// Whether this node is currently acting as a CSI collector.
///
/// Restored with the central/peripheral roles: the ESP-NOW exchange lets a central tell a peripheral
/// to start or stop collecting mid-run, which the emitter/collector roles have no equivalent for —
/// there, the role is fixed when the node starts.
///
/// Deliberately NOT the same gate as `CSI_PUBLISH_ENABLED`. That one decides whether the WiFi
/// callback builds a packet at all; this one drives the ESP-NOW responder/initiator behaviour.
/// Conflating them silently breaks sniffer + Listener setups, where the user wants a passive node
/// that still reads CSI.
pub(crate) static IS_COLLECTOR: AtomicBool = AtomicBool::new(false);

/// Signalled whenever [`set_runtime_collection_mode`] changes the gate, so an ESP-NOW task waiting
/// on a mode change wakes immediately instead of polling.
pub(crate) static COLLECTION_MODE_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Whether captured CSI is delivered off-device. See
/// [`CSINode::set_csi_output_enabled`](crate::CSINode::set_csi_output_enabled).
pub(crate) static CSI_OUTPUT_ENABLED: AtomicBool = AtomicBool::new(false);
// CSI publish gate. The WiFi callback checks this in a single relaxed load
// to decide whether to build and emit a CSIDataPacket.
//
// Deliberately separate from `CSI_OUTPUT_ENABLED`: that flag is the user's
// output preference, while this one tracks whether any consumer is actually
// installed. Conflating them would let an output-disabled node also block a
// `CSINodeClient` that wants to read CSI directly.
static CSI_PUBLISH_ENABLED: AtomicBool = AtomicBool::new(false);
pub(crate) static CSI_OUTPUT_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// CSI delivery mode — single-atomic dispatch in the WiFi callback.
///
/// Per-packet, the callback loads `CSI_DELIVERY_MODE` once and branches
/// on it. Exactly one of the user-facing delivery paths runs, so users
/// pay only for what they asked for:
/// - `Off`: nothing past the publish gate (apart from seq-drop tracking).
/// - `Callback`: dispatch to the `fn` stored in `CSI_CALLBACK` with a
///   `&CSIDataPacket` borrow. Lowest latency, runs on the WiFi-task
///   hot path. **Picked by [`set_csi_callback`].**
/// - `Async`: move the packet into the lock-free `CSI_QUEUE` and wake
///   the consumer registered via `CSI_WAKER`. Doesn't block the WiFi
///   task. **Picked lazily by the first
///   [`CSINodeClient::next_csi_packet`].`**
///
/// The two are **mutually exclusive** so the WiFi callback never pays
/// for both a callback dispatch and a 640 B memcpy on the same packet.
/// Toggle explicitly with [`set_csi_delivery_mode`].
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum CsiDeliveryMode {
    /// No user delivery. Inline `log_csi` may still run if its gate is
    /// open (controlled by [`set_csi_logging_enabled`]).
    Off = 0,
    /// Dispatch to the `fn` registered with [`set_csi_callback`] inline
    /// in the WiFi callback context.
    Callback = 1,
    /// Move the packet into the lock-free `CSI_QUEUE` and wake the
    /// async consumer awaiting [`CSINodeClient::next_csi_packet`].
    Async = 2,
}

/// Single-atomic dispatch select for the WiFi callback. Read once per
/// CSI event in `capture_csi_info`. See [`CsiDeliveryMode`] for the
/// branch semantics.
static CSI_DELIVERY_MODE: portable_atomic::AtomicU8 = portable_atomic::AtomicU8::new(0);

/// User CSI callback registered via [`set_csi_callback`]. Loaded only
/// when `CSI_DELIVERY_MODE == Callback`, so callers in `Off` / `Async`
/// modes don't pay for an extra atomic load.
static CSI_CALLBACK: core::sync::atomic::AtomicPtr<()> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Raw CSI fast-path callback registered via [`set_csi_raw_callback`]. When
/// non-null, `capture_csi_info` invokes it and returns **before** building the
/// ~640 B [`CSIDataPacket`] — the cheapest possible delivery path, used by the
/// CPU-comparison DUT to match the ESP-IDF reference's do-nothing `csi_cb`.
/// Checked before [`CSI_DELIVERY_MODE`], so it overrides callback/async/off.
static CSI_RAW_CALLBACK: core::sync::atomic::AtomicPtr<()> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Inline-logging gate. Independent of [`CSI_DELIVERY_MODE`] so the
/// per-packet UART/JTAG `log_csi` path is controlled separately
/// (toggle with [`set_csi_logging_enabled`]).
static CSI_INLINE_LOG_ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable or disable inline CSI **logging** (per-packet UART/JTAG output).
///
/// This controls only the inline `log_csi` path inside the WiFi callback.
/// It does **not** disable a [`set_csi_callback`] hook — registering a
/// callback opens an independent publish gate, and that gate stays open
/// regardless of this flag. So a typical "process inline, no UART flood"
/// setup is:
///
/// ```ignore
/// init_logger(spawner, LogMode::Text);   // publish gate + log gate ON
/// set_csi_logging_enabled(false);        // log gate OFF (callback still fires)
/// set_csi_callback(on_csi);              // publish gate ON, log gate untouched
/// ```
///
/// Defaults / who flips this for you:
/// - `init_logger` enables it automatically in sync mode (the WiFi
///   callback writes CSI lines inline).
/// - In `async-print` mode `CSINodeClient::get_csi_data` enables the
///   publish gate (separate from the log gate) lazily on first await.
/// - [`set_csi_callback`] enables only the publish gate — it does not
///   touch this log-output flag.
pub fn set_csi_logging_enabled(enabled: bool) {
    CSI_INLINE_LOG_ENABLED.store(enabled, Ordering::Release);
    // Keep the master publish gate paired with logging by default so
    // existing callers that only flip `set_csi_logging_enabled(true)` to
    // get UART output still work. A registered `set_csi_callback` keeps
    // the publish gate open independently when this is later disabled.
    CSI_PUBLISH_ENABLED.store(enabled, Ordering::Release);
}

/// Returns whether inline CSI logging is currently enabled (i.e. whether
/// the per-packet UART/JTAG `log_csi` path will run).
pub fn csi_logging_enabled() -> bool {
    CSI_INLINE_LOG_ENABLED.load(Ordering::Relaxed)
}

/// Set the active CSI delivery mode (callback / async / off).
///
/// The WiFi callback dispatches to **exactly one** path per packet —
/// callers pay no overhead for the path they didn't pick. Switching
/// with this fn is a single relaxed atomic store; the next CSI event
/// follows the new mode.
///
/// You normally don't call this directly:
/// - [`set_csi_callback`] sets the mode to [`CsiDeliveryMode::Callback`].
/// - First await of [`CSINodeClient::next_csi_packet`] sets it to
///   [`CsiDeliveryMode::Async`].
/// - [`clear_csi_callback`] sets it to [`CsiDeliveryMode::Off`].
///
/// Use this fn when you want to **switch** between paths at runtime
/// without re-registering, or to fully disable user delivery while
/// leaving inline logging running.
pub fn set_csi_delivery_mode(mode: CsiDeliveryMode) {
    CSI_DELIVERY_MODE.store(mode as u8, Ordering::Release);
}

/// Returns the active CSI delivery mode.
pub fn csi_delivery_mode() -> CsiDeliveryMode {
    match CSI_DELIVERY_MODE.load(Ordering::Relaxed) {
        1 => CsiDeliveryMode::Callback,
        2 => CsiDeliveryMode::Async,
        _ => CsiDeliveryMode::Off,
    }
}

/// Register a user callback invoked inline for every captured CSI packet.
///
/// The callback runs in the WiFi task context (the same context that
/// formats and writes CSI lines), with a borrow of the [`CSIDataPacket`]
/// *before* it is consumed by the logging path. This is the supported
/// path for **on-device CSI processing** — zero channel hops, lowest
/// possible latency.
///
/// **Constraints**: the callback runs on the WiFi task hot path and MUST
/// be fast and non-blocking. Avoid heap allocation, locking, and long
/// format/write work. For heavier processing, copy what you need out of
/// the packet and post to your own task.
///
/// Registering opens the master publish gate and switches the
/// delivery mode to [`CsiDeliveryMode::Callback`]. Any prior async
/// drain mode is replaced — the WiFi callback only runs the inline
/// callback path from this point. The inline-logging gate
/// ([`set_csi_logging_enabled`]) is left untouched so `init_logger`'s
/// UART output (or its absence) is preserved.
///
/// Call [`clear_csi_callback`] to remove the hook and return to
/// [`CsiDeliveryMode::Off`].
pub fn set_csi_callback(cb: fn(&CSIDataPacket)) {
    // Store the fn pointer first so the WiFi callback never sees the
    // mode flipped to `Callback` while `CSI_CALLBACK` is still null.
    CSI_CALLBACK.store(cb as *mut (), core::sync::atomic::Ordering::Release);
    CSI_DELIVERY_MODE.store(CsiDeliveryMode::Callback as u8, Ordering::Release);
    CSI_PUBLISH_ENABLED.store(true, Ordering::Release);
}

/// Remove the user CSI callback registered via [`set_csi_callback`]
/// and switch to [`CsiDeliveryMode::Off`].
///
/// The publish gate and inline-logging gate are left untouched — call
/// `set_csi_logging_enabled(false)` if you also want to suppress
/// logging output, or `set_csi_delivery_mode(CsiDeliveryMode::Async)`
/// to swap to async drain without re-driving lazy initialization.
pub fn clear_csi_callback() {
    CSI_DELIVERY_MODE.store(CsiDeliveryMode::Off as u8, Ordering::Release);
    CSI_CALLBACK.store(core::ptr::null_mut(), core::sync::atomic::Ordering::Release);
}

/// Register a **raw** CSI fast-path callback (CPU-benchmark use).
///
/// Unlike [`set_csi_callback`], the WiFi callback invokes `cb` and returns
/// immediately, **without** building a [`CSIDataPacket`] — so the per-frame
/// CSI cost is just the callback dispatch, matching the ESP-IDF reference's
/// self-timing `csi_cb`. Intended for the like-for-like CPU comparison DUT;
/// the callback receives no CSI data (it cannot, by design — that is the cost
/// being elided). Pass a `fn()` that does only minimal bookkeeping.
pub fn set_csi_raw_callback(cb: fn()) {
    CSI_RAW_CALLBACK.store(cb as *mut (), core::sync::atomic::Ordering::Release);
    CSI_PUBLISH_ENABLED.store(true, Ordering::Release);
}

/// Toggle CSI output delivery at runtime.
pub fn set_csi_output_enabled(enabled: bool) {
    CSI_OUTPUT_ENABLED.store(enabled, Ordering::Relaxed);
    CSI_OUTPUT_CHANGED.signal(());
}

/// Whether CSI output delivery is currently enabled.
pub fn csi_output_enabled() -> bool {
    CSI_OUTPUT_ENABLED.load(Ordering::Relaxed)
}

/// Source-MAC allowlist for captured CSI. All-zero (the default) means "accept any source".
///
/// A collector is promiscuous by nature: it reports CSI for every frame its radio decodes, which on
/// a busy channel means the AP's own beacons and ACKs, the association exchange, and any THIRD-PARTY
/// device on the channel. That is correct behaviour and the reports are real, but it is not what
/// someone measuring their own link asked for, and it reads as corrupt data — the leading field of a
/// row is the frame's own 802.11 sequence number, which is per-transmitter and per-TID, so foreign
/// rows carry arbitrary sequence numbers and a legacy-rate PHY's shorter CSI length.
///
/// Filtering on the device rather than on the host is not just convenience. On a console-bound
/// collector every rejected frame is console bandwidth handed back to the traffic the user actually
/// configured, and the rejection happens before the 612-byte packet copy and before any formatting.
/// Held as a packed `u64` (big-endian in the low 48 bits) rather than a mutex-wrapped `[u8; 6]`,
/// because this is read from inside the Wi-Fi CSI callback. On `riscv32imc` every atomic op takes a
/// critical section, and this file's own dispatch comments note that extra gate atomics in that ISR
/// path delay the Embassy timer ISR — a `CriticalSectionRawMutex` per captured frame is exactly the
/// cost the fast path is written to avoid. One relaxed 64-bit load has no such problem.
static CSI_PEER_FILTER: portable_atomic::AtomicU64 = portable_atomic::AtomicU64::new(0);

/// Minimum `sig_mode` (PHY class) to accept: `0` accepts everything, `1` accepts HT (802.11n) and
/// better, dropping the legacy-rate mgmt/control frames that produce the short L-LTF-only rows.
static CSI_MIN_SIG_MODE: portable_atomic::AtomicU8 = portable_atomic::AtomicU8::new(0);

/// Single gate so the DEFAULT (unfiltered) path costs one relaxed bool load rather than a 64-bit
/// load plus a `u8` load on every captured frame.
static CSI_FILTER_ACTIVE: AtomicBool = AtomicBool::new(false);

#[inline(always)]
fn pack_mac(mac: [u8; 6]) -> u64 {
    let mut v = 0u64;
    let mut i = 0;
    while i < 6 {
        v = (v << 8) | mac[i] as u64;
        i += 1;
    }
    v
}

fn refresh_filter_active() {
    let active = CSI_PEER_FILTER.load(Ordering::Relaxed) != 0
        || CSI_MIN_SIG_MODE.load(Ordering::Relaxed) != 0;
    CSI_FILTER_ACTIVE.store(active, Ordering::Relaxed);
}

/// Restrict captured CSI to one source MAC. All-zero clears the filter.
pub fn set_csi_peer_filter(mac: [u8; 6]) {
    CSI_PEER_FILTER.store(pack_mac(mac), Ordering::Relaxed);
    refresh_filter_active();
}

/// The current source-MAC filter (all-zero = no filter).
pub fn csi_peer_filter() -> [u8; 6] {
    let v = CSI_PEER_FILTER.load(Ordering::Relaxed);
    let mut mac = [0u8; 6];
    let mut i = 0;
    while i < 6 {
        mac[i] = (v >> (8 * (5 - i))) as u8;
        i += 1;
    }
    mac
}

/// Restrict captured CSI to frames whose `sig_mode` is at least `min`.
///
/// `0` accepts every PHY (default); `1` keeps HT and better.
pub fn set_csi_min_sig_mode(min: u8) {
    CSI_MIN_SIG_MODE.store(min, Ordering::Relaxed);
    refresh_filter_active();
}

/// The current minimum-`sig_mode` filter.
pub fn csi_min_sig_mode() -> u8 {
    CSI_MIN_SIG_MODE.load(Ordering::Relaxed)
}

/// Whether a frame passes the attribution filters, judged from the raw `WifiCsiInfo`.
///
/// Deliberately reads `info` rather than a built `CSIDataPacket`: both fields are available on the
/// raw callback argument, so a rejected frame costs one MAC compare instead of a 612-byte copy plus
/// a format pass. On the classic MACs `packet_mode()` is the `sig_mode` field; on C5/C6 there is no
/// such field, so the PHY half of the filter is a no-op there and only the MAC filter applies.
#[inline(always)]
fn csi_passes_filter(info: &esp_radio::wifi::csi::WifiCsiInfo<'_>) -> bool {
    // One relaxed load on the unfiltered default, which is what almost every run uses.
    if !CSI_FILTER_ACTIVE.load(Ordering::Relaxed) {
        return true;
    }
    let want = CSI_PEER_FILTER.load(Ordering::Relaxed);
    if want != 0 && pack_mac(*info.mac()) != want {
        return false;
    }
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    {
        let min = CSI_MIN_SIG_MODE.load(Ordering::Relaxed);
        if min != 0 && (info.packet_mode() as u8) < min {
            return false;
        }
    }
    true
}

/// Change collection mode at runtime — e.g. a central signalling a peripheral to start or stop.
pub(crate) fn set_runtime_collection_mode(is_collector: bool) {
    IS_COLLECTOR.store(is_collector, Ordering::Relaxed);
    COLLECTION_MODE_CHANGED.signal(());
}

/// Reset the CSI delivery gates. Called by `reset_globals` between runs.
///
/// Closes all CSI delivery gates so any late-firing WiFi callback runs
/// are no-ops. The CSI callback stays registered with esp-radio after
/// stop (the radio itself is still up), but with the gates closed the
/// callback short-circuits before it touches the log channel or the
/// user's callback.
pub(crate) fn reset() {
    CSI_INLINE_LOG_ENABLED.store(false, Ordering::Release);
    CSI_PUBLISH_ENABLED.store(false, Ordering::Release);
    CSI_DELIVERY_MODE.store(CsiDeliveryMode::Off as u8, Ordering::Release);
    CSI_CALLBACK.store(core::ptr::null_mut(), core::sync::atomic::Ordering::Release);
    // The attribution filters are deliberately NOT cleared: they are user configuration set from
    // the CLI before `start`, exactly like the log mode, not per-run state. Clearing them here
    // would silently un-filter the next run.
}

/// Handle for controlling a running [`CSINode`](crate::CSINode) from user code.
///
/// CSI packets are delivered to user code via [`set_csi_callback`] (the
/// preferred path: zero channel hops, lowest latency) or — under the
/// `async-print` feature — by awaiting [`Self::get_csi_data`] /
/// [`Self::print_csi_w_metadata`]. The client also signals the running
/// node to stop early via [`Self::send_stop`].
pub struct CSINodeClient {
    _private: (),
}

impl CSINodeClient {
    /// Create a new CSI node client.
    ///
    /// Constructing a client does not by itself open the publish gate.
    /// In async-print mode the gate is opened lazily on the first
    /// `get_csi_data()` await; in sync mode it is opened by
    /// `init_logger` or `set_csi_callback`. Use `set_csi_logging_enabled`
    /// to override.
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Await the next CSI packet captured by the WiFi callback.
    ///
    /// Drains the lock-free `CSI_QUEUE`. Available in **both** sync and
    /// `async-print` modes — same API, same delivery path. Dequeue →
    /// register waker → re-check, which closes the lost-wakeup window.
    ///
    /// The first call lazily switches [`CsiDeliveryMode`] to
    /// [`CsiDeliveryMode::Async`] and opens the master publish gate so
    /// the WiFi callback starts enqueueing. **This replaces any prior
    /// `set_csi_callback`** — the two delivery paths are mutually
    /// exclusive so the WiFi callback only ever runs one of them per
    /// packet (no double-dispatch overhead).
    ///
    /// **Single consumer**: the underlying `AtomicWaker` is single-slot.
    /// Awaiting `next_csi_packet` from two different tasks at once will
    /// cause one of them to miss wake-ups — register exactly one
    /// drainer task per node.
    pub async fn next_csi_packet(&mut self) -> CSIDataPacket {
        // Conservative lazy init: only flip into `Async` mode if no
        // delivery path is currently active (`Off`). If the user has
        // already set `Callback` mode, we don't disrupt it — the
        // drainer just parks on the waker until the user explicitly
        // switches via `set_csi_delivery_mode(CsiDeliveryMode::Async)`.
        // This lets the two APIs coexist as runtime-toggleable choices
        // without one clobbering the other.
        if CSI_DELIVERY_MODE.load(Ordering::Relaxed) == CsiDeliveryMode::Off as u8 {
            CSI_DELIVERY_MODE.store(CsiDeliveryMode::Async as u8, Ordering::Release);
            CSI_PUBLISH_ENABLED.store(true, Ordering::Release);
        }
        core::future::poll_fn(|cx| {
            if let Some(p) = CSI_QUEUE.dequeue() {
                return core::task::Poll::Ready(p);
            }
            CSI_WAKER.register(cx.waker());
            // Re-check after register to close the lost-wakeup window:
            // the WiFi callback could have enqueued + woken between our
            // first dequeue and `register` if we hadn't checked again.
            if let Some(p) = CSI_QUEUE.dequeue() {
                core::task::Poll::Ready(p)
            } else {
                core::task::Poll::Pending
            }
        })
        .await
    }

    /// Back-compat alias for [`Self::next_csi_packet`]. Older code paths
    /// (and the `async-print` feature) referred to this name.
    pub async fn get_csi_data(&mut self) -> CSIDataPacket {
        self.next_csi_packet().await
    }

    /// Receive the next CSI packet and emit it via the crate logging
    /// backend (`log_csi`). Convenience wrapper for "drain + log to
    /// UART/JTAG" loops:
    /// ```ignore
    /// loop { client.print_csi_w_metadata().await; }
    /// ```
    pub async fn print_csi_w_metadata(&mut self) {
        let packet = self.next_csi_packet().await;
        crate::logging::logging::log_csi(packet);
        embassy_futures::yield_now().await;
    }

    /// Signal the running node to stop.
    pub async fn send_stop(&self) {
        STOP_SIGNAL.signal(());
    }
}

impl Default for CSINodeClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "esp32c5")]
pub(crate) fn build_csi_config(csi_config: &CsiConfiguration) -> CsiConfig {
    CsiConfig {
        enable: csi_config.enable,
        acquire_csi_legacy: csi_config.acquire_csi_legacy,
        acquire_csi_force_lltf: csi_config.acquire_csi_force_lltf,
        acquire_csi_ht20: csi_config.acquire_csi_ht20,
        acquire_csi_ht40: csi_config.acquire_csi_ht40,
        acquire_csi_vht: csi_config.acquire_csi_vht,
        // Extended-acquisition modes default off; a radio profile may enable
        // them via `RadioProfile::tune_csi_acquisition`.
        acquire_csi_su: 0,
        acquire_csi_mu: 0,
        acquire_csi_dcm: 0,
        acquire_csi_beamformed: 0,
        acquire_csi_he_stbc: 0,
        val_scale_cfg: csi_config.val_scale_cfg,
        dump_ack_en: csi_config.dump_ack_en,
        reserved: csi_config.reserved,
    }
}

#[cfg(feature = "esp32c6")]
pub(crate) fn build_csi_config(csi_config: &CsiConfiguration) -> CsiConfig {
    CsiConfig {
        enable: csi_config.enable,
        acquire_csi_legacy: csi_config.acquire_csi_legacy,
        acquire_csi_ht20: csi_config.acquire_csi_ht20,
        acquire_csi_ht40: csi_config.acquire_csi_ht40,
        // Extended-acquisition modes default off; a radio profile may enable
        // them via `RadioProfile::tune_csi_acquisition`.
        acquire_csi_su: 0,
        acquire_csi_mu: 0,
        acquire_csi_dcm: 0,
        acquire_csi_beamformed: 0,
        acquire_csi_he_stbc: 0,
        val_scale_cfg: csi_config.val_scale_cfg,
        dump_ack_en: csi_config.dump_ack_en,
        reserved: csi_config.reserved,
    }
}

#[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
pub(crate) fn build_csi_config(csi_config: &CsiConfiguration) -> CsiConfig {
    CsiConfig {
        lltf_en: csi_config.lltf_en,
        htltf_en: csi_config.htltf_en,
        stbc_htltf2_en: csi_config.stbc_htltf2_en,
        ltf_merge_en: csi_config.ltf_merge_en,
        channel_filter_en: csi_config.channel_filter_en,
        manu_scale: csi_config.manu_scale,
        shift: csi_config.shift,
        dump_ack_en: csi_config.dump_ack_en,
    }
}

/// Sets CSI Configuration.
pub(crate) fn set_csi(controller: &mut WifiController, config: CsiConfig) {
    // Set CSI Configuration with callback
    controller
        .set_csi(config, |info: esp_radio::wifi::csi::WifiCsiInfo<'_>| {
            capture_csi_info(info);
        })
        .unwrap();
}

impl CSIDataPacket {
    /// Build a `CSIDataPacket` from a raw `WifiCsiInfo`, copying the CSI byte
    /// buffer and classifying the format via [`Self::csi_fmt_from_params`].
    ///
    /// Returns `None` if the CSI payload exceeds the fixed 612-byte capacity;
    /// the caller does its own drop accounting. This is the single source of
    /// truth for the `WifiCsiInfo` → `CSIDataPacket` mapping, shared by the
    /// normal CSI callback (`capture_csi_info`) and any out-of-tree collector
    /// that owns the radio directly, so both emit byte-identical frames.
    pub fn from_wifi_csi_info(info: &esp_radio::wifi::csi::WifiCsiInfo<'_>) -> Option<Self> {
        let mut csi_data = Vec::<i8, 612>::new();
        let csi_slice = info.buf();
        let csi_buf_len = csi_slice.len() as u16;
        csi_data.extend_from_slice(csi_slice).ok()?;

        let mac_arr = *info.mac();
        let timestamp_us = info.timestamp().duration_since_epoch().as_micros() as u32;

        #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
        let mut csi_packet = CSIDataPacket {
            sequence_number: info.rx_sequence(),
            data_format: RxCSIFmt::Undefined,
            date_time: None,
            mac: mac_arr,
            rssi: info.rssi() as i32,
            bandwidth: info.cwb() as u32,
            antenna: info.antenna() as u32,
            rate: info.rate() as u32,
            sig_mode: info.packet_mode() as u32,
            mcs: info.modulation_coding_scheme() as u32,
            smoothing: info.smoothing() as u32,
            not_sounding: info.not_sounding() as u32,
            aggregation: info.aggregation() as u32,
            stbc: info.space_time_block_code() as u32,
            fec_coding: info.forward_error_correction_coding() as u32,
            sgi: info.short_guide_interval() as u32,
            noise_floor: info.noise_floor() as i32,
            ampdu_cnt: info.ampdu_count() as u32,
            channel: info.channel() as u32,
            secondary_channel: info.secondary_channel() as u32,
            timestamp: timestamp_us,
            rx_state: info.rx_state() as u32,
            sig_len: info.signal_length() as u32,
            csi_data_len: csi_buf_len,
            csi_data,
        };

        #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
        let mut csi_packet = CSIDataPacket {
            mac: mac_arr,
            rssi: info.rssi() as i32,
            timestamp: timestamp_us,
            rate: info.rate() as u32,
            noise_floor: info.noise_floor() as i32,
            sig_len: info.signal_length() as u32,
            rx_state: info.rx_state() as u32,
            dump_len: info.dump_length(),
            #[cfg(feature = "esp32c6")]
            sigb_len: info.he_sigb_length() as u32,
            #[cfg(feature = "esp32c6")]
            cur_single_mpdu: info.cur_single_mpdu() as u32,
            cur_bb_format: info.cur_bb_format() as u32,
            rx_channel_estimate_info_vld: info.rx_channel_estimate_info_valid() as u32,
            rx_channel_estimate_len: info.rx_channel_estimate_length(),
            second: info.secondary_channel() as u32,
            channel: info.channel() as u32,
            is_group: info.is_group() as u32,
            rxend_state: info.rx_end_state() as u32,
            rxmatch3: info.rx_match3() as u32,
            rxmatch2: info.rx_match2() as u32,
            rxmatch1: info.rx_match1() as u32,
            #[cfg(feature = "esp32c6")]
            rxmatch0: info.rx_match0() as u32,
            date_time: None,
            sequence_number: info.rx_sequence(),
            data_format: RxCSIFmt::Undefined,
            csi_data_len: csi_buf_len,
            csi_data,
        };

        // Classify the frame format from captured metadata so consumers can tell
        // a high-resolution capture from a fallback frame.
        csi_packet.csi_fmt_from_params();
        Some(csi_packet)
    }
}

// Function to capture CSI info from callback and publish to channel
fn capture_csi_info(info: esp_radio::wifi::csi::WifiCsiInfo<'_>) {
    // Count every CSI report regardless of mode so `rx_count` / `rx_rate_hz`
    // / `pps_rx` reflect actual radio CSI throughput. This is the only path
    // that fires for every collector capture path — counting here keeps the
    // metric consistent across all of them.
    #[cfg(feature = "statistics")]
    STATS.rx_count.fetch_add(1, Ordering::Relaxed);
    // `cur_bb_format()` only exists on the newer MAC (C5/C6). The classic
    // esp32 / C3 / S3 (`wifi_mac_version = "1"`) radios have no baseband-format
    // field, so the histogram simply stays empty there.
    #[cfg(all(feature = "statistics", any(feature = "esp32c5", feature = "esp32c6")))]
    crate::stats::record_cur_bb_format(info.cur_bb_format() as u32);

    // Raw fast-path (CPU-benchmark DUT): if a raw callback is registered,
    // invoke it and return before the ~640 B CSIDataPacket build. This makes
    // the per-frame CSI cost just the callback dispatch — the fair match to
    // the ESP-IDF reference's do-nothing `csi_cb`. See `set_csi_raw_callback`.
    let raw_cb_ptr = CSI_RAW_CALLBACK.load(Ordering::Relaxed);
    if !raw_cb_ptr.is_null() {
        let raw_cb: fn() = unsafe { core::mem::transmute::<*mut (), fn()>(raw_cb_ptr) };
        raw_cb();
        return;
    }

    // Single-atomic fast path: returns immediately when no consumer is
    // installed (no logger, callback, or CSINodeClient subscriber). Building the
    // CSIDataPacket and calling publish_immediate acquires CriticalSectionRawMutex
    // and on `riscv32imc` every other atomic op also takes a critical section,
    // so additional gate atomics in the hot ISR path delay the Embassy timer ISR.
    if !CSI_PUBLISH_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    // Attribution filter, applied BEFORE the 612-byte packet build and before any formatting, so a
    // rejected frame costs one relaxed load (see `csi_passes_filter`). Counted as an rx drop: the
    // radio did capture it, we chose not to deliver it, and a user comparing `RX Total Packets`
    // against delivered rows needs that difference to be visible rather than unexplained.
    if !csi_passes_filter(&info) {
        #[cfg(feature = "statistics")]
        STATS.rx_drop_count.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // No CS-locked early-drop pre-check: the lock-free `CSI_QUEUE`
    // returns `Err` from `enqueue` when full, so we do drop accounting at
    // the enqueue site below. The 640 B `CSIDataPacket` build still has
    // to run unconditionally — there's no cheaper way to know if the
    // packet is interesting until it's parsed.

    // Build the packet (copies the CSI buffer, classifies the format). `None`
    // means the payload overflowed the 612-byte cap — do drop accounting here.
    let csi_packet = match CSIDataPacket::from_wifi_csi_info(&info) {
        Some(p) => p,
        None => {
            #[cfg(feature = "statistics")]
            STATS.rx_drop_count.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    #[cfg(all(feature = "statistics", not(feature = "esp32c5")))]
    #[allow(static_mut_refs)] // single writer (WiFi callback) by construction
    {
        if seq_drop_detection_enabled() {
            static mut PEER_SEQ_TRACKER: LinearMap<[u8; 6], u16, MAX_TRACKED_PEERS> =
                LinearMap::new();
            unsafe {
                if RESET_SEQ_TRACKER.swap(false, Ordering::Relaxed) {
                    PEER_SEQ_TRACKER.clear();
                }
                let current_seq = csi_packet.sequence_number;
                if let Some(&last_seq) = PEER_SEQ_TRACKER.get(&csi_packet.mac) {
                    let diff = (current_seq.wrapping_sub(last_seq)) & 0x0FFF;
                    if diff > 1 {
                        let lost = (diff - 1) as u32;
                        if lost < 500 {
                            STATS.rx_drop_count.fetch_add(lost, Ordering::Relaxed);
                        }
                    }
                }
                if PEER_SEQ_TRACKER
                    .insert(csi_packet.mac, current_seq)
                    .is_err()
                {
                    PEER_SEQ_TRACKER.clear();
                    let _ = PEER_SEQ_TRACKER.insert(csi_packet.mac, current_seq);
                }
            }
        }
    }

    // Single-atomic delivery dispatch. One relaxed load, one branch.
    // Exactly one of Callback / Async / Off runs — the WiFi callback
    // never pays for both a fn-pointer dispatch and a 640 B memcpy on
    // the same packet. See `CsiDeliveryMode` for semantics.
    match CSI_DELIVERY_MODE.load(Ordering::Relaxed) {
        m if m == CsiDeliveryMode::Callback as u8 => {
            // Inline callback: zero-copy `&CSIDataPacket` borrow.
            let cb_ptr = CSI_CALLBACK.load(core::sync::atomic::Ordering::Relaxed);
            if !cb_ptr.is_null() {
                let cb: fn(&CSIDataPacket) =
                    unsafe { core::mem::transmute::<*mut (), fn(&CSIDataPacket)>(cb_ptr) };
                cb(&csi_packet);
            }
            return;
        }
        m if m == CsiDeliveryMode::Async as u8 => {
            // Lock-free MPMC enqueue + wake. No critical section, no
            // IRQ disable — the WiFi-task hot path is never blocked by
            // the user's async drainer.
            if CSI_QUEUE.enqueue(csi_packet).is_err() {
                #[cfg(feature = "statistics")]
                STATS.rx_drop_count.fetch_add(1, Ordering::Relaxed);
            } else {
                CSI_WAKER.wake();
            }
            return;
        }
        _ => {}
    }

    // Off mode: fall through to the inline-log path. In sync mode
    // `log_csi` writes the CSI line directly to UART/JTAG here in the
    // WiFi callback (matches ESP32-CSI-Tool's `_wifi_csi_cb`); in
    // async-print mode it enqueues to the logger backend's own channel
    // (`logging::logging::CSI_CHANNEL`, drained by `logger_backend`).
    // Either way the packet is consumed.
    if CSI_INLINE_LOG_ENABLED.load(Ordering::Relaxed) {
        crate::logging::logging::log_csi(csi_packet);
    }
}

/// Internal task that handles collection-mode changes and rate statistics.
///
/// Seq drop detection runs inside `capture_csi_info` (ISR context) so this task
/// never drains `CSI_PACKET`, leaving the channel exclusively for `CSINodeClient`.
pub async fn run_process_csi_packet() {
    #[cfg(feature = "statistics")]
    STATS
        .capture_start_time
        .store(Instant::now().as_ticks(), Ordering::Relaxed);
    #[cfg(feature = "statistics")]
    let mut last_rate_update = Instant::now();
    #[cfg(feature = "statistics")]
    let mut last_rx_count = STATS.rx_count.load(Ordering::Relaxed);
    #[cfg(feature = "statistics")]
    let mut last_tx_count = STATS.tx_count.load(Ordering::Relaxed);

    loop {
        match select3(
            STOP_SIGNAL.wait(),
            CSI_OUTPUT_CHANGED.wait(),
            Timer::after_millis(500),
        )
        .await
        {
            Either3::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either3::Second(_) => {
                CSI_OUTPUT_CHANGED.reset();
                // A runtime Collector/Listener switch is not a collection
                // teardown. Keep CSI delivery gates and callbacks intact; closing
                // them here disables output mid-run until the next CLI `start`.
                #[cfg(feature = "statistics")]
                {
                    STATS
                        .capture_start_time
                        .store(Instant::now().as_ticks(), Ordering::Relaxed);
                    last_rate_update = Instant::now();
                    last_rx_count = STATS.rx_count.load(Ordering::Relaxed);
                    last_tx_count = STATS.tx_count.load(Ordering::Relaxed);
                    #[cfg(not(feature = "esp32c5"))]
                    RESET_SEQ_TRACKER.store(true, Ordering::Relaxed);
                }
            }
            Either3::Third(_) => {
                #[cfg(feature = "statistics")]
                {
                    let elapsed_secs = last_rate_update.elapsed().as_secs();
                    if elapsed_secs >= 1 {
                        let current_rx = STATS.rx_count.load(Ordering::Relaxed);
                        let current_tx = STATS.tx_count.load(Ordering::Relaxed);

                        let rx_rate =
                            ((current_rx.saturating_sub(last_rx_count)) / elapsed_secs) as u32;
                        let tx_rate =
                            ((current_tx.saturating_sub(last_tx_count)) / elapsed_secs) as u32;

                        STATS.rx_rate_hz.store(rx_rate, Ordering::Relaxed);
                        STATS.tx_rate_hz.store(tx_rate, Ordering::Relaxed);

                        last_rx_count = current_rx;
                        last_tx_count = current_tx;
                        last_rate_update = Instant::now();
                    }
                }
            }
        }
    }
}
