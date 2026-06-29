//! ESP-NOW receive pool that bypasses esp-radio's heap-allocating dispatcher.
//!
//! ## Why
//!
//! `esp_radio::esp_now::rcv_cb` (the C-level dispatcher esp-radio registers
//! during `EspNow::new_internal`) does two heap operations per ESP-NOW vendor
//! action frame: `Box::from(slice)` for the payload (~250 B) and
//! `push_back` into a `VecDeque<ReceivedData>` that grows from 0 → 4 → 8 → 16
//! capacity (384 B / 768 B / 1536 B grow allocations) on demand.
//!
//! Our sync CSI logger CPU-spins UART for ~11 ms per line inside the WiFi
//! task. While that spin runs, no other code on the same core can run —
//! including `rcv_cb`. ESP-NOW vendor frames pile up at lower layers and
//! `rcv_cb` then fires for them in burst once the spin returns. That burst
//! does many `Box::from`s in rapid succession, fragmenting the heap. By the
//! time the VecDeque needs to grow, the allocator can no longer find a
//! contiguous chunk of the requested size → `handle_alloc_error` → panic.
//!
//! ## What this module does
//!
//! Re-registers a custom `rcv_cb` *over* esp-radio's via the C FFI
//! `esp_now_register_recv_cb`. Our callback copies the payload into one of
//! [`POOL_CAPACITY`] fixed-size BSS slots and pushes the slot to a lock-free
//! `MpMcQueue`. **No heap allocation, ever**, regardless of how many frames
//! arrive while the CSI callback is spinning UART.
//!
//! User code (`peripheral::esp_now`, `central::esp_now`) calls [`receive`]
//! and [`receive_async`] in place of `EspNow::receive` / `receive_async`.
//! The returned [`PoolFrame`] mirrors the small subset of `ReceivedData`'s
//! API the rest of the crate actually consumes (`info.src_address`,
//! `data()`).
//!
//! Once [`install`] is called, esp-radio's `EspNow::receive*` methods
//! return `None` — they read a queue that's no longer being written to.
//! That's intentional: any code path that still calls them is broken and
//! should switch to this module.

use core::future::poll_fn;
use core::task::Poll;

use embassy_sync::waitqueue::AtomicWaker;
use heapless::mpmc::Q16;
use portable_atomic::{AtomicBool, AtomicPtr, Ordering};

/// ESP-NOW maximum data payload (matches `esp_radio::esp_now::ESP_NOW_MAX_DATA_LEN`).
const ESP_NOW_MAX_DATA_LEN: usize = 250;

/// Fixed pool capacity. Mirrors esp-radio's `RECEIVE_QUEUE_SIZE` so behavior
/// matches drop-front-on-full. Heapless `Q16` provides 16-slot MPMC.
pub const POOL_CAPACITY: usize = 16;

/// Subset of `esp_radio::esp_now::ReceiveInfo` that the rest of the crate
/// actually reads. Keeping this minimal avoids copying the ~80 B
/// `RxControlInfo` per frame in the C callback.
#[derive(Clone, Copy)]
pub struct PoolInfo {
    /// Source MAC of the received ESP-NOW frame.
    pub src_address: [u8; 6],
}

/// Drop-in replacement for `ReceivedData` carrying only the fields used by
/// `peripheral::esp_now` and `central::esp_now`.
#[derive(Clone, Copy)]
pub struct PoolFrame {
    /// Receive metadata extracted from the original esp-radio frame.
    pub info: PoolInfo,
    data_buf: [u8; ESP_NOW_MAX_DATA_LEN],
    data_len: u16,
}

impl PoolFrame {
    /// Returns the received payload (matches `ReceivedData::data`).
    pub fn data(&self) -> &[u8] {
        &self.data_buf[..self.data_len as usize]
    }
}

/// FFI mirror of `esp_now_recv_info_t` (matches the C-side layout). We only
/// dereference `src_addr` — `des_addr` and `rx_ctrl` are unused so they can
/// stay opaque.
#[repr(C)]
struct EspNowRecvInfo {
    src_addr: *mut u8,
    des_addr: *mut u8,
    rx_ctrl: *mut u8,
}

unsafe extern "C" {
    fn esp_now_register_recv_cb(
        cb: Option<unsafe extern "C" fn(*const EspNowRecvInfo, *const u8, i32)>,
    ) -> i32;
}

/// Lock-free 16-slot MPMC queue holding pre-formatted frames. `Q16` stores
/// `PoolFrame`s by value, so enqueue/dequeue copy ~256 B — negligible vs.
/// the UART time the original Box-allocating path costs.
static QUEUE: Q16<PoolFrame> = Q16::new();
/// Single waker for `receive_async` consumers. We only have one consumer in
/// the codebase (the responder/handler task), so a single-slot waker is fine.
static WAKER: AtomicWaker = AtomicWaker::new();

/// Raw-drop mode (CPU-benchmark use). When enabled, `pool_rcv_cb` discards the
/// frame immediately — no payload copy, no enqueue, no waker wake — so no
/// separate responder task is woken per frame. This makes the per-frame
/// ESP-NOW receive path "callback fires → returns", matching the ESP-IDF
/// reference's empty inline `recv_cb` (no app-task hop, no extra context
/// switch). CSI still fires via the independent `capture_csi_info` path.
/// Driven by [`crate::set_raw_listen`] (which also enables raw CSI delivery).
static RAW_DROP: AtomicBool = AtomicBool::new(false);

/// Enable/disable raw-drop mode — see [`RAW_DROP`]. Set before the node starts.
pub fn set_raw_drop(enabled: bool) {
    RAW_DROP.store(enabled, Ordering::Relaxed);
}

/// Optional raw-recv callback (min/drop-test use). When set **and** raw-drop is
/// on, `pool_rcv_cb` hands each frame's payload slice to this callback inline,
/// before discarding the frame — so a minimal receiver can read e.g. a sequence
/// number with no responder-task hop or `ControlPacket` ingest, matching the
/// ESP-IDF reference's inline `recv_cb`. Stored as an erased `fn(&[u8])`.
static RAW_RECV_CALLBACK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// Register a raw-recv callback — see [`RAW_RECV_CALLBACK`]. Pair with
/// [`crate::set_raw_listen`]`(true)`. The callback runs in the WiFi task's
/// C-FFI context, so it must return quickly and do only lock-free bookkeeping.
pub fn set_raw_recv_callback(cb: fn(&[u8])) {
    RAW_RECV_CALLBACK.store(cb as *mut (), Ordering::Release);
}

/// Custom receive callback installed via `esp_now_register_recv_cb`. Runs on
/// the WiFi task in C-FFI context; must do no heap operations and finish
/// quickly so the WiFi RX path keeps moving.
unsafe extern "C" fn pool_rcv_cb(info: *const EspNowRecvInfo, data: *const u8, data_len: i32) {
    // Raw-drop fast path (CPU-benchmark / min-drop): discard before any
    // copy/enqueue/wake, so no responder task is woken — the fair match to the
    // IDF empty recv_cb. If a raw-recv callback is registered (min drop test),
    // hand it the payload slice inline first so it can read the sequence number.
    if RAW_DROP.load(Ordering::Relaxed) {
        let cb_ptr = RAW_RECV_CALLBACK.load(Ordering::Acquire);
        if !cb_ptr.is_null() && !data.is_null() && data_len > 0 {
            let len = (data_len as usize).min(ESP_NOW_MAX_DATA_LEN);
            // SAFETY: cb_ptr was stored from a `fn(&[u8])` in set_raw_recv_callback;
            // `data`/`len` describe a valid frame buffer for the call's duration.
            let cb: fn(&[u8]) = unsafe { core::mem::transmute(cb_ptr) };
            let slice = unsafe { core::slice::from_raw_parts(data, len) };
            cb(slice);
        }
        return;
    }
    if info.is_null() || data.is_null() || data_len <= 0 {
        return;
    }
    let len = (data_len as usize).min(ESP_NOW_MAX_DATA_LEN);

    let mut frame = PoolFrame {
        info: PoolInfo {
            src_address: [0; 6],
        },
        data_buf: [0; ESP_NOW_MAX_DATA_LEN],
        data_len: len as u16,
    };

    let src_ptr = unsafe { (*info).src_addr };
    if !src_ptr.is_null() {
        for i in 0..6 {
            frame.info.src_address[i] = unsafe { *src_ptr.add(i) };
        }
    }

    for i in 0..len {
        frame.data_buf[i] = unsafe { *data.add(i) };
    }

    // Drop frame on full. Mirrors esp-radio's "drop oldest" semantics in
    // spirit (we drop newest instead — easier with MPMC and equivalent under
    // sustained overload), avoids ever allocating, never blocks the WiFi
    // task. Worst case under burst: 16 newest frames retained, anything past
    // that is silently lost — same outcome as esp-radio's queue past
    // capacity.
    let _ = QUEUE.enqueue(frame);
    WAKER.wake();
}

/// Install the static-pool `rcv_cb`, replacing esp-radio's heap-allocating
/// dispatcher. Must be called *after* `wifi::new` has constructed the
/// `EspNow` (which performs the initial registration) and *before* any
/// real ESP-NOW traffic begins. Idempotent: subsequent calls just re-bind
/// to the same callback.
pub fn install() {
    unsafe {
        let _ = esp_now_register_recv_cb(Some(pool_rcv_cb));
    }
}

/// Non-blocking receive. Returns `Some(frame)` if a frame is queued,
/// `None` otherwise. Drop-in replacement for `EspNow::receive`.
pub fn receive() -> Option<PoolFrame> {
    QUEUE.dequeue()
}

/// Async receive. Resolves when the next frame arrives. Drop-in replacement
/// for `EspNow::receive_async`.
pub async fn receive_async() -> PoolFrame {
    poll_fn(|cx| {
        if let Some(f) = QUEUE.dequeue() {
            return Poll::Ready(f);
        }
        WAKER.register(cx.waker());
        // Re-check after registering to close the lost-wake-up window: a
        // frame could have been enqueued and woken between our first
        // dequeue and `register` if we hadn't checked again.
        if let Some(f) = QUEUE.dequeue() {
            Poll::Ready(f)
        } else {
            Poll::Pending
        }
    })
    .await
}
