//! CPU-utilization DUT firmware per CPU Utilization Test Specification v2.
//!
//! The DUT receives the offered traffic as an **ESP-NOW peripheral in
//! listen-only mode** (`PeripheralOpMode::EspNow`, RX enabled / TX disabled):
//! it brings up ESP-NOW on `TEST_CHANNEL` and produces CSI for the broadcast
//! frames emitted by the TX (`esp_now_central_exper_cpu_tx`). In this
//! full-library (exper) pairing the TX sends real magic-prefixed
//! `ControlPacket`s (padded to the cell payload), so the DUT's **full ingest
//! path actually runs** — magic validation, sequence tracking, mode hysteresis
//! — on top of the radio's CSI generation and the library's full
//! `CSIDataPacket` delivery. That is the esp-csi-rs full-stack cost under test
//! (the min pair, by contrast, strips ingest + CSIDataPacket to match the
//! esp-csi reference floor). The spec allows any receive path that produces CSI
//! (§1, §6.2).
//!
//! PHY parity: MCS0-LGI + HT40 (secondary below) on `TEST_CHANNEL`, matching the
//! TX and the esp-csi reference. HT40 for ESP-NOW is best-effort in esp-radio —
//! confirm on-air via the CSI `bandwidth` field that HT40 actually engaged.
//!
//! No `statistics` feature: this is the measured device, so it deliberately
//! omits `statistics` — those per-frame RX/ingest atomics are overhead the
//! esp-csi C++ reference doesn't have and would bias the comparison. With both
//! sides statistics-free, `ControlPacket` is `{is_collector}` on the wire.
//!
//! Reports **absolute** core utilisation as an idle-*time* fraction
//! (`busy_ppm = 1e6 × (1 − idle_time/window)`, spec §2.1) — there is no
//! calibration, no no-load baseline normalisation, and no in-firmware
//! PASS/FAIL verdict (v2 removed all of those). The baseline phase is
//! captured and reported as `CPU_BASELINE` purely as informational fixed
//! overhead (spec §5).
//!
//! ## How `idle_time` is measured here (spec §2.2)
//!
//! esp-csi-rs runs on **esp-rtos**, a *preemptive* multi-task RTOS — the
//! WiFi RX path and the CSI callback run in their own RTOS task, not in the
//! embassy executor thread. So the spec's "cooperative executor / WFI
//! bracket" realisation does not apply; the correct realisation for a
//! preemptive RTOS is run-time stats over the idle task (spec §2.2,
//! `freertos_rtstats`). We obtain that from esp-rtos's `rtos-trace`
//! scheduler hooks: every context switch emits `system_idle()` (switching
//! into the idle context) or `task_exec_begin()` (switching into a task).
//! We timestamp those transitions and sum the wall-clock time the core
//! spent in the idle context — that is `idle_time`. Requires building with
//! `--features cpu-trace` (enables `esp-rtos/rtos-trace`).
//!
//! Single measured core: we never start the second core, so the embassy
//! main task, the WiFi RX task, and the CSI callback all run on PRO_CPU
//! (core 0). `MEAS` declares core 0. If a build ever starts the second
//! core, the idle accounting below must become per-core.
//!
//! Known limitation: esp-rtos does not call rtos-trace's `isr_enter`/
//! `isr_exit`, so time spent in raw ISRs (e.g. the radio ISR before it
//! hands off to the WiFi task) is not separated out — if an ISR fires
//! while the core is in the idle context, that time is attributed to idle.
//! The bulk of CSI RX work runs in the WiFi *task* and is captured. The
//! per-frame `CPU_CB` cycle count (spec §2.3) is unaffected by this.
//!
//! - CPU_FREQ_HZ is a compile-time const; must match with_cpu_clock(...) in main.
//! - Sync model: fixed BOOT_DELAY_S boot-delay only (no runtime handshake);
//!   TX runs the same shared schedule.
//! - Build with `--features async-print` to avoid UART I/O contaminating the
//!   measurement (spec §4.2 — no I/O on the measurement path).
//! - Records: CPU_FREQ/MEAS/SCHEDULE (headers), CPU_BUSY/CPU_CB (per-second),
//!   PHASE_BEGIN/PHASE_END/CPU_BASELINE/RUN_COMPLETE (lifecycle) — spec §4.3.
//!
//! Build: `cargo build --release --example esp_now_peripheral_exper_cpu --features <xtensa-chip>,cpu-trace,async-print`.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    CSINode, CollectionMode, EspNowConfig, IOTaskConfig, config::CsiConfig,
    logging::logging::init_logger,
};
use esp_csi_rs::{
    CSINodeClient, CSINodeHardware, log_ln, set_csi_callback, set_csi_logging_enabled,
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

/// The measured / RX core. Single-core operation (second core never started),
/// so everything runs on PRO_CPU. Declared in the `MEAS` header (spec §4.3).
const MEAS_CORE: u32 = 0;

/// Idle-measurement realisation declared in `MEAS` (spec §2.2). esp-rtos is a
/// preemptive RTOS and we accumulate idle-task (idle-context) run-time, so the
/// conforming label is `freertos_rtstats`, not `executor_idle`.
const MEAS_METHOD: &str = "freertos_rtstats";

// --- Per-second CSI-callback accumulators, drained-and-reset by the emitter. ---
// All u32 (not u64): ESP32 (xtensa LX6) has no native 64-bit atomics, so an
// AtomicU64 RMW compiles to an interrupt-disabling critical section. Keeping the
// per-context-switch idle hook lock-free is essential for an UNBIASED busy%
// measurement — the full-library DUT wakes an extra task per frame, so any
// per-switch cost is multiplied. (This mirrors the de-biased min DUT.)
static CB_CYCLES: AtomicU32 = AtomicU32::new(0);
static CB_COUNT: AtomicU32 = AtomicU32::new(0);
static CB_CORE: AtomicU32 = AtomicU32::new(0);

// --- Idle-time accounting, updated from the rtos-trace scheduler hooks. ---
/// Total CPU cycles the measured core has spent in its idle context this window.
/// Accumulated lock-free (u32, `ccount`); the emitter reads and resets it. A 1 s
/// window is < 2^32 cycles at 240 MHz, so u32 never overflows.
static IDLE_CYCLES: AtomicU32 = AtomicU32::new(0);
/// `ccount` timestamp of the most recent context-switch transition. Seeded at
/// `t0`, so no first-sample guard is needed.
static LAST_TS_CYC: AtomicU32 = AtomicU32::new(0);
/// Whether the segment that started at `LAST_TS_CYC` is the idle context.
static IN_IDLE: AtomicBool = AtomicBool::new(false);
/// Most recent window's `busy_ppm`, published for the baseline sampler.
static LAST_BUSY_PPM: AtomicU32 = AtomicU32::new(0);
/// Most recent window's CSI frame count, published alongside `LAST_BUSY_PPM`
/// so the baseline sampler folds in only no-load windows (spec §5).
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
/// Called from the rtos-trace hooks, which fire on every task switch. Cheap by
/// construction: one `ccount` read + three lock-free u32 atomics, NO critical
/// section and NO systimer read. The segment that just ended ran from
/// `LAST_TS_CYC` until now; if it was the idle context, its length (CPU cycles)
/// is added to `IDLE_CYCLES`. `entering_idle` is whether the *next* segment is
/// idle. `LAST_TS_CYC` is seeded at `t0`, so no first-sample guard is needed.
#[inline]
fn switch_transition(entering_idle: bool) {
    let now = cpu::ccount();
    let last = LAST_TS_CYC.swap(now, Ordering::Relaxed);
    let was_idle = IN_IDLE.swap(entering_idle, Ordering::Relaxed);
    if was_idle {
        IDLE_CYCLES.fetch_add(now.wrapping_sub(last), Ordering::Relaxed);
    }
}

/// rtos-trace sink. All bodies are minimal (atomics only) per spec §4.2; only
/// the idle/task transitions feed the idle-time accumulator.
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

/// Instrumented CSI callback. Runs in the WiFi task context — keep it minimal
/// (no allocation/blocking/I/O, spec §4.2). Times only dispatch + atomic
/// bookkeeping; add real processing here and re-label the run if a different
/// comparison point is wanted (spec §2.3).
fn csi_cb(_p: &CSIDataPacket) {
    let s = cpu::ccount();
    let e = cpu::ccount();
    CB_CYCLES.fetch_add(e.wrapping_sub(s), Ordering::Relaxed);
    CB_COUNT.fetch_add(1, Ordering::Relaxed);
    CB_CORE.store(cpu::core_id(), Ordering::Relaxed);
}

/// `busy_ppm = round(1e6 × (1 − idle/window))`, clamped to `[0, 1_000_000]`
/// (spec §2.1). `window` and `idle` are in the same unit (CPU cycles); the unit
/// cancels.
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
/// from `ccount`, so the ratio is exact. `t_ms` stays wall-clock as a log label.
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
        // Publish for the baseline sampler. Store the count first so a reader
        // that sees this window's fresh busy_ppm also sees its matching
        // cb_count (spec §5 folds in only windows with no CSI received).
        LAST_CB_COUNT.store(count, Ordering::Relaxed);
        LAST_BUSY_PPM.store(bp, Ordering::Relaxed);

        log_ln!("CPU_BUSY,{},{},{}", t_ms, MEAS_CORE, bp);
        log_ln!("CPU_CB,{},{},{},{}", t_ms, core, cycles, count);
    }
}

/// Emit the informational `CPU_BASELINE` record (spec §5). No verdict, no gate.
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

/// Walks the shared schedule, emitting PHASE_BEGIN/PHASE_END per phase and one
/// CPU_BASELINE after baseline_capture. Shares `t0` with the emitter so all
/// `t_ms` fields are on one timeline (spec §4.1).
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
                // Fold in only no-load windows: skip any window where CSI was
                // received (TX/DUT phase-sync slop or ambient traffic), spec §5.
                // Read busy_ppm before cb_count to match the emitter's store
                // order. Drops samples past the cap (BASELINE_MAX_SAMPLES).
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
    // Header records (spec §4.3), emitted once before any per-second record.
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

    // Synchronisation: delay BOOT_DELAY_S, latch t0, zero accumulators (spec §4.1).
    Timer::after(Duration::from_secs(BOOT_DELAY_S as u64)).await;
    let t0 = Instant::now();
    CB_CYCLES.store(0, Ordering::Relaxed);
    CB_COUNT.store(0, Ordering::Relaxed);
    IDLE_CYCLES.store(0, Ordering::Relaxed);
    LAST_TS_CYC.store(cpu::ccount(), Ordering::Relaxed);

    join(per_second_emitter(t0), schedule_driver(t0)).await;
    // Both joined futures are `-> !`, so this is unreachable.
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

    // Smaller heap than the base ESP-NOW peripheral (98440) — the CPU-experiment
    // statics push `.dram2_uninit` past the segment ceiling at full size. 64 KiB
    // is comfortably more than the CSI + ESP-NOW RX path needs in collector mode.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting ESP-NOW Peripheral CSI CPU-Utilization Experiment (spec v2)");

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
        esp_csi_rs::Node::Peripheral(esp_csi_rs::PeripheralOpMode::EspNow(
            EspNowConfig::default()
                .with_channel(TEST_CHANNEL)
                .with_phy_rate(WifiPhyRate::RateMcs0Lgi),
        )),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(10000),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    // PHY parity with the esp-csi C reference and the exper TX (MCS0-LGI + HT40)
    // is set on the EspNowConfig above.
    // Listen-only ESP-NOW peer: RX enabled, TX disabled. The responder then
    // never replies or adds a peer, so the DUT just receives the TX's broadcast
    // frames and produces CSI for them — no handshake (spec §1, §6.2).
    node.set_io_tasks(IOTaskConfig::new(false, true));

    // Register the inline CSI callback before starting the node so the first
    // packet already drains through it. `set_csi_callback` also flips delivery
    // to `Callback` mode and opens the publish gate.
    set_csi_callback(csi_cb);

    join(node.run(), dut_workload()).await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
