//! **Minimal** CPU-utilization DUT — the fair, like-for-like counterpart to the
//! ESP-IDF reference `espnow_peer_cpu_test` (the "esp-now-idf" run).
//!
//! Identical to the standard DUT (`esp_now_peripheral_exper_cpu`) — same radio
//! bring-up via `node.run()`, same RX-buffer config, channel, protocol,
//! idle-time (rtos-trace) measurement, schedule and record format — except it
//! strips the two per-frame costs the IDF reference does **not** pay:
//!
//!   1. **CSI delivery** — uses [`set_csi_raw_callback`] instead of
//!      [`set_csi_callback`], so the WiFi callback self-times and returns
//!      *without* building the ~640 B `CSIDataPacket`. Matches the IDF `csi_cb`
//!      (`wifi_csi_info_t*` → time → count → return).
//!   2. **ESP-NOW ingest** — calls [`esp_csi_rs::set_raw_listen`]`(true)`, so the
//!      responder still drains received frames but skips
//!      `ingest_control_packet` (postcard deserialize, magic/source checks,
//!      sequence/timestamp bookkeeping, mode hysteresis — the esp-csi-rs
//!      "start-network" framing). Matches the IDF empty `espnow_recv_cb`.
//!
//! Because it reuses the library node, it inherits the exact, proven RX/CSI
//! engagement of the standard DUT — so busy% scales with offered rate (the
//! earlier hand-rolled bring-up did not receive at all). Running this against
//! `esp_now_peripheral_exper_cpu` isolates esp-csi-rs's framing/CSIDataPacket
//! cost; running it against `espnow_peer_cpu_test` isolates the platform RX
//! cost. Companion TX: `esp_now_central_min_cpu_tx`.
//!
//! Measurement, records, sync model and build flags match the standard DUT —
//! see `esp_now_peripheral_exper_cpu` for the spec details.
//!
//! Build: `cargo build --release --example esp_now_peripheral_min_cpu --features <xtensa-chip>,cpu-trace,async-print`.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    CSINode, EspNowConfig, IOTaskConfig, config::CsiConfig,
    logging::logging::init_logger, set_raw_listen,
};
use esp_csi_rs::{
    CSINodeClient, CSINodeHardware, log_ln, set_csi_logging_enabled, set_csi_raw_callback,
};
use esp_hal::clock::CpuClock;
use esp_hal::time::Instant;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::WifiController;
use portable_atomic::{AtomicBool, AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

#[path = "cpu_test_schedule.rs"]
mod cpu_test_schedule;
use cpu_test_schedule::{BASELINE_MAX_SAMPLES, BOOT_DELAY_S, PhaseKind, TEST_CHANNEL, phases_iter};

extern crate alloc;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// Must match `with_cpu_clock(CpuClock::max())` in main — 240 MHz on ESP32 / S3.
const CPU_FREQ_HZ: u32 = 240_000_000;

/// The measured / RX core. Single-core operation (second core never started).
const MEAS_CORE: u32 = 0;

/// Idle-measurement realisation declared in `MEAS` (spec §2.2).
const MEAS_METHOD: &str = "freertos_rtstats";

// --- Per-second CSI-callback accumulators, drained-and-reset by the emitter. ---
// 32-bit (not u64) to match the IDF reference and stay lock-free: ESP32 (xtensa
// LX6) has no native 64-bit atomics, so an AtomicU64 RMW compiles to a critical
// section (interrupts disabled). u32 is plenty — a 1 s window holds at most
// ~240 M cycles / ~500 frames.
static CB_CYCLES: AtomicU32 = AtomicU32::new(0);
static CB_COUNT: AtomicU32 = AtomicU32::new(0);
static CB_CORE: AtomicU32 = AtomicU32::new(0);

// --- Idle-time accounting, updated from the rtos-trace scheduler hooks. ---
// Timestamped with the CPU cycle counter (`ccount`, one `rsr` instruction) and
// accumulated in u32 — both lock-free on xtensa. This makes the per-switch
// observer cost a handful of instructions with NO critical section, comparable
// to FreeRTOS run-time stats' single TCB timestamp. (The previous version used
// `Instant::now()` + AtomicU64, i.e. a systimer read + a critical section on
// every context switch — heavy enough to bias the busy% upward vs the IDF DUT.)
// A 1 s window is < 2^32 cycles at 240 MHz, so u32 never overflows; ccount wraps
// every ~17.9 s but `wrapping_sub` over sub-second deltas is exact.
static IDLE_CYCLES: AtomicU32 = AtomicU32::new(0);
static LAST_TS_CYC: AtomicU32 = AtomicU32::new(0);
static IN_IDLE: AtomicBool = AtomicBool::new(false);
static LAST_BUSY_PPM: AtomicU32 = AtomicU32::new(0);
static LAST_CB_COUNT: AtomicU32 = AtomicU32::new(0);

mod cpu {
    use core::arch::asm;
    #[inline(always)]
    pub fn ccount() -> u32 {
        let v: u32;
        unsafe {
            asm!("rsr.ccount {0}", out(reg) v, options(nomem, nostack, preserves_flags));
        }
        v
    }

    /// PRID bit 13: PRO_CPU=0, APP_CPU=1 on ESP32 / S3.
    #[inline(always)]
    pub fn core_id() -> u32 {
        let p: u32;
        unsafe {
            asm!("rsr.prid {0}", out(reg) p, options(nomem, nostack, preserves_flags));
        }
        (p >> 13) & 1
    }
}

/// Record a context-switch transition for idle-time accounting (spec §2.2).
///
/// Cheap by construction: one `ccount` read + three lock-free u32 atomics, no
/// critical section. If the segment that just ended was the idle context, its
/// length (in CPU cycles) is added to `IDLE_CYCLES`. `LAST_TS_CYC` is seeded at
/// `t0`, so no first-sample guard is needed.
#[inline]
fn switch_transition(entering_idle: bool) {
    let now = cpu::ccount();
    let last = LAST_TS_CYC.swap(now, Ordering::Relaxed);
    let was_idle = IN_IDLE.swap(entering_idle, Ordering::Relaxed);
    if was_idle {
        IDLE_CYCLES.fetch_add(now.wrapping_sub(last), Ordering::Relaxed);
    }
}

/// rtos-trace sink — identical to the standard DUT.
struct CpuTrace;

impl rtos_trace::RtosTrace for CpuTrace {
    fn task_exec_begin(_id: u32) {
        switch_transition(false);
    }
    fn system_idle() {
        switch_transition(true);
    }

    fn start() {}
    fn stop() {}
    fn task_new(_id: u32) {}
    fn task_send_info(_id: u32, _info: rtos_trace::TaskInfo) {}
    fn task_new_stackless(_id: u32, _name: &'static str, _priority: u32) {}
    fn task_terminate(_id: u32) {}
    fn task_exec_end() {}
    fn task_ready_begin(_id: u32) {}
    fn task_ready_end(_id: u32) {}
    fn isr_enter() {}
    fn isr_exit() {}
    fn isr_exit_to_scheduler() {}
    fn name_marker(_id: u32, _name: &'static str) {}
    fn marker(_id: u32) {}
    fn marker_begin(_id: u32) {}
    fn marker_end(_id: u32) {}
}

rtos_trace::global_trace! {CpuTrace}

/// Minimal **raw** CSI callback — the like-for-like match to the IDF `csi_cb`.
/// Registered via `set_csi_raw_callback`, so it runs *before* any
/// `CSIDataPacket` is built and the WiFi callback returns straight after. It
/// takes no CSI data by design — that build is exactly the cost being elided.
fn csi_cb() {
    let s = cpu::ccount();
    let e = cpu::ccount();
    CB_CYCLES.fetch_add(e.wrapping_sub(s), Ordering::Relaxed);
    CB_COUNT.fetch_add(1, Ordering::Relaxed);
    CB_CORE.store(cpu::core_id(), Ordering::Relaxed);
}

/// `busy_ppm = round(1e6 × (1 − idle/window))`, clamped to `[0, 1_000_000]`.
/// `window` and `idle` are in the same unit (CPU cycles); the unit cancels.
fn busy_ppm(window: u64, idle: u64) -> u32 {
    if window == 0 {
        return 0;
    }
    let idle = idle.min(window);
    let busy = (window - idle) as u128;
    let w = window as u128;
    let ppm = (busy * 1_000_000 + w / 2) / w;
    ppm.min(1_000_000) as u32
}

/// 1 Hz emitter (spec §4.2). Busy fraction is idle_cycles / window_cycles, both
/// from `ccount` (the core cycle counter), so the ratio is exact regardless of
/// frequency. `t_ms` stays wall-clock (`Instant`) purely as a log label.
async fn per_second_emitter(t0: Instant) -> ! {
    let mut last_emit_cyc = cpu::ccount();
    loop {
        Timer::after(Duration::from_secs(1)).await;
        let t_ms = (Instant::now() - t0).as_millis();

        let now_cyc = cpu::ccount();
        let window_cyc = now_cyc.wrapping_sub(last_emit_cyc);
        last_emit_cyc = now_cyc;

        let idle_cyc = IDLE_CYCLES.swap(0, Ordering::Relaxed);
        let cycles = CB_CYCLES.swap(0, Ordering::Relaxed);
        let count = CB_COUNT.swap(0, Ordering::Relaxed);
        let core = CB_CORE.load(Ordering::Relaxed);

        let bp = busy_ppm(window_cyc as u64, idle_cyc as u64);
        LAST_CB_COUNT.store(count, Ordering::Relaxed);
        LAST_BUSY_PPM.store(bp, Ordering::Relaxed);

        log_ln!("CPU_BUSY,{},{},{}", t_ms, MEAS_CORE, bp);
        log_ln!("CPU_CB,{},{},{},{}", t_ms, core, cycles, count);
    }
}

/// Emit the informational `CPU_BASELINE` record (spec §5).
fn emit_baseline(t0: Instant, samples: &[u32]) {
    let t_ms = (Instant::now() - t0).as_millis();
    let n = samples.len() as u32;
    if n == 0 {
        log_ln!("CPU_BASELINE,{},{},0,0,0", t_ms, MEAS_CORE);
        return;
    }
    let mut sum: u64 = 0;
    let mut max: u32 = 0;
    for &s in samples {
        sum += s as u64;
        if s > max {
            max = s;
        }
    }
    let mean = (sum / n as u64) as u32;
    log_ln!("CPU_BASELINE,{},{},{},{},{}", t_ms, MEAS_CORE, mean, max, n);
}

/// Walks the shared schedule — identical to the standard DUT.
async fn schedule_driver(t0: Instant) -> ! {
    for (idx, p) in phases_iter() {
        let t_ms = (Instant::now() - t0).as_millis();
        log_ln!(
            "PHASE_BEGIN,{},{},{},{},{},{}",
            t_ms,
            idx,
            p.kind.as_str(),
            p.rate_hz,
            p.payload_b,
            p.rep
        );

        if matches!(p.kind, PhaseKind::BaselineCapture) {
            let mut samples: heapless::Vec<u32, BASELINE_MAX_SAMPLES> = heapless::Vec::new();
            for _ in 0..p.duration_s {
                Timer::after(Duration::from_secs(1)).await;
                let bp = LAST_BUSY_PPM.load(Ordering::Relaxed);
                let cb = LAST_CB_COUNT.load(Ordering::Relaxed);
                if cb == 0 {
                    let _ = samples.push(bp);
                }
            }
            emit_baseline(t0, &samples);
        } else {
            Timer::after(Duration::from_secs(p.duration_s as u64)).await;
        }

        let t_ms = (Instant::now() - t0).as_millis();
        log_ln!("PHASE_END,{},{}", t_ms, idx);
    }

    log_ln!("RUN_COMPLETE,{}", (Instant::now() - t0).as_millis());
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

async fn dut_workload() -> ! {
    log_ln!("CPU_FREQ,{}", CPU_FREQ_HZ);
    log_ln!("MEAS,{},{}", MEAS_CORE, MEAS_METHOD);
    for (idx, p) in phases_iter() {
        log_ln!(
            "SCHEDULE,{},{},{},{},{},{}",
            idx,
            p.kind.as_str(),
            p.rate_hz,
            p.payload_b,
            p.rep,
            p.duration_s
        );
    }

    Timer::after(Duration::from_secs(BOOT_DELAY_S as u64)).await;
    let t0 = Instant::now();
    CB_CYCLES.store(0, Ordering::Relaxed);
    CB_COUNT.store(0, Ordering::Relaxed);
    // Seed idle accounting cleanly at t0 so pre-t0 (boot) activity isn't counted.
    IDLE_CYCLES.store(0, Ordering::Relaxed);
    LAST_TS_CYC.store(cpu::ccount(), Ordering::Relaxed);
    IN_IDLE.store(false, Ordering::Relaxed);

    join(per_second_emitter(t0), schedule_driver(t0)).await;
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(_spawner, LogMode::Text);
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting MINIMAL ESP-NOW Peripheral CSI CPU-Utilization DUT (matched to esp-now-idf)");

    // RX buffering: 32 static / 128 dynamic. NOTE: esp-radio's controller init
    // OOMs at the 64 KiB heap if static_rx is lowered to 25 (the ESP-IDF max the
    // C builds use), so esp-csi-rs keeps 32 here. RX-buffer counts are therefore
    // "comparable", not identical, across the esp-radio vs ESP-IDF boundary —
    // their buffer semantics differ anyway, so exact parity isn't meaningful.
    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_static_rx_buf_num(32)
        .with_dynamic_rx_buf_num(128)
        .with_rx_queue_size(32);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut _node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::NodeRole::Peripheral(esp_csi_rs::PeripheralOpMode::EspNow(
            EspNowConfig::default()
                .with_channel(TEST_CHANNEL)
                .with_phy_rate(WifiPhyRate::RateMcs0Lgi),
        )),
        Some(CsiConfig::default()),
        Some(10000),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    // PHY parity with the esp-csi reference + exper DUT (MCS0-LGI + HT40, so the
    // platform comparison is at matched airtime / CSI buffer size) is set on the
    // EspNowConfig above.
    // Listen-only ESP-NOW peer: RX enabled, TX disabled (same as standard DUT).
    node.set_io_tasks(IOTaskConfig::new(false, true));

    // The two flags that make this the fair, minimal DUT — set BEFORE the node
    // starts so no frame is ever ingested and no CSIDataPacket is ever built:
    set_raw_listen(true); // skip ingest_control_packet (the "start-network" framing)
    set_csi_raw_callback(csi_cb); // skip the 640 B CSIDataPacket build

    join(node.run(), dut_workload()).await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
