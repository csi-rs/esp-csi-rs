//! CSI delivery state machine.
//!
//! The WiFi-task callback ([`capture_csi_info`]) dispatches each captured CSI
//! report down exactly one user-facing path — inline callback, async queue, or
//! inline logging — selected by a single relaxed atomic load. This module owns
//! that dispatch state, the public registration functions, the lock-free CSI
//! queue + waker, the [`CSINodeClient`] consumer handle, and the
//! `WifiController` CSI wiring (`set_csi` / [`build_csi_config`]).

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
/// [`CSINodeClient::next_csi_packet`]. Mirrors the `esp_now_pool`
/// pattern (`src/lib/esp_now_pool.rs`): the producer is the WiFi-task
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

pub(crate) static IS_COLLECTOR: AtomicBool = AtomicBool::new(false);
// CSI publish gate. The WiFi callback checks this in a single relaxed load
// to decide whether to build and emit a CSIDataPacket.
//
// Decoupled from `IS_COLLECTOR` on purpose: `CollectionMode` controls the
// ESP-NOW responder/initiator behavior (Listener stays passive on TX), but
// it must NOT block a `CSINodeClient` from reading CSI — that conflation
// silently breaks sniffer + Listener configurations where the user wants
// to passively read CSI without participating in any control protocol.
static CSI_PUBLISH_ENABLED: AtomicBool = AtomicBool::new(false);
pub(crate) static COLLECTION_MODE_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

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
///   the consumer registered via [`CSI_WAKER`]. Doesn't block the WiFi
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
/// being elided). Pass a `fn()` that does only minimal bookkeeping. Pair with
/// [`set_raw_listen`](crate::set_raw_listen) to also skip the ESP-NOW
/// control-packet ingest.
pub fn set_csi_raw_callback(cb: fn()) {
    CSI_RAW_CALLBACK.store(cb as *mut (), core::sync::atomic::Ordering::Release);
    CSI_PUBLISH_ENABLED.store(true, Ordering::Release);
}

/// Internal function to change collection mode at runtime (e.g. Central can
/// signal Peripheral to start/stop collecting CSI).
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
    /// `async-print` modes — same API, same delivery path. Mirrors
    /// `crate::esp_now_pool::receive_async`: dequeue → register waker
    /// → re-check (closes the lost-wakeup window).
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
        acquire_csi_su: csi_config.acquire_csi_su,
        acquire_csi_mu: csi_config.acquire_csi_mu,
        acquire_csi_dcm: csi_config.acquire_csi_dcm,
        acquire_csi_beamformed: csi_config.acquire_csi_beamformed,
        acquire_csi_he_stbc: csi_config.acquire_csi_he_stbc,
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
        acquire_csi_su: csi_config.acquire_csi_su,
        acquire_csi_mu: csi_config.acquire_csi_mu,
        acquire_csi_dcm: csi_config.acquire_csi_dcm,
        acquire_csi_beamformed: csi_config.acquire_csi_beamformed,
        acquire_csi_he_stbc: csi_config.acquire_csi_he_stbc,
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

// Function to capture CSI info from callback and publish to channel
fn capture_csi_info(info: esp_radio::wifi::csi::WifiCsiInfo<'_>) {
    // Count every CSI report regardless of mode so `rx_count` / `rx_rate_hz`
    // / `pps_rx` reflect actual radio CSI throughput. This is the only path
    // that fires for sniffer / STA / ESP-NOW collection — counting here keeps
    // the metric consistent across all node modes.
    #[cfg(feature = "statistics")]
    STATS.rx_count.fetch_add(1, Ordering::Relaxed);

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

    // Single-atomic fast path: returns immediately in Listener mode and in
    // Collector mode when no CSINodeClient subscriber exists. Building the
    // CSIDataPacket and calling publish_immediate acquires CriticalSectionRawMutex
    // and on `riscv32imc` every other atomic op also takes a critical section,
    // so additional gate atomics in the hot ISR path delay the Embassy timer ISR.
    if !CSI_PUBLISH_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    // No CS-locked early-drop pre-check: the lock-free `CSI_QUEUE`
    // returns `Err` from `enqueue` when full, so we do drop accounting at
    // the enqueue site below. The 640 B `CSIDataPacket` build still has
    // to run unconditionally — there's no cheaper way to know if the
    // packet is interesting until it's parsed.

    let rssi = info.rssi();

    let mut csi_data = Vec::<i8, 612>::new();
    let csi_slice = info.buf();
    let csi_buf_len = csi_slice.len() as u16;
    match csi_data.extend_from_slice(csi_slice) {
        Ok(_) => {}
        Err(_) => {
            #[cfg(feature = "statistics")]
            STATS.rx_drop_count.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }

    let mac_arr = *info.mac();
    let timestamp_us = info.timestamp().duration_since_epoch().as_micros() as u32;

    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    let mut csi_packet = CSIDataPacket {
        sequence_number: info.rx_sequence(),
        data_format: RxCSIFmt::Undefined,
        date_time: None,
        mac: mac_arr,
        rssi: rssi as i32,
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
        rssi: rssi as i32,
        timestamp: timestamp_us,
        rate: info.rate() as u32,
        noise_floor: info.noise_floor() as i32,
        sig_len: info.signal_length() as u32,
        rx_state: info.rx_state() as u32,
        dump_len: info.dump_length(),
        #[cfg(feature = "esp32c6")]
        he_sigb_len: info.he_sigb_length() as u32,
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

    // Classify the frame format (HE20 / VHT / legacy-HT) from captured metadata so
    // consumers can tell a high-resolution HE20 capture from a fallback frame.
    csi_packet.csi_fmt_from_params();

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
            COLLECTION_MODE_CHANGED.wait(),
            Timer::after_millis(500),
        )
        .await
        {
            Either3::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either3::Second(_) => {
                COLLECTION_MODE_CHANGED.reset();
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
