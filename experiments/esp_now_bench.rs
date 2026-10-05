//! **ESP-NOW measurement harness** — heap, CPU, power, packet drop and bandwidth.
//!
//! One harness for both ends of the symmetric ESP-NOW exchange and every quantity the specs in
//! `specs/` measure. Set `NETWORK_ROLE` and `MEASURE` below and build; the node is a **listener**
//! throughout, because a measurement of acquisition cost should not be paying delivery cost.
//!
//! Replaces the eleven single-purpose harnesses that differed only in these two constants
//! (`esp_now_{central,peripheral}_{exper,exper_heap,exper_cpu*,power,drop,bw_*}`).
//!
//! ## Reading the numbers
//!
//! The binary-footprint test is the one measurement that is **not** taken from this file's output.
//! It is `full − min`, where `min` is `esp_now_bench_min` — the same platform and radio bring-up
//! with no `CSINode` in it at all. Both halves must be built with the **same feature set** for the
//! difference to mean anything, because features like `statistics` and `cpu-test-tx` link code of
//! their own. Record the feature set alongside the number.
//!
//! Build:
//!   cargo esp32c6-build --example esp_now_bench --features=statistics
//!   cargo esp32c6-build --example esp_now_bench --features=statistics,cpu-test-tx   # MEASURE = Cpu

#![no_std]
#![no_main]

#[cfg(not(feature = "statistics"))]
compile_error!("This harness requires the `statistics` feature.");

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeClient, ReportingPolicy, EspNowConfig, NetworkRole, NodeHardware,
    config::CsiConfig, install_static_espnow_recv, log_ln, set_csi_logging_enabled,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::{
    get_dropped_packets_rx, get_pps_rx, get_total_rx_packets, get_total_tx_packets,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

// The CPU-utilization run drives a fixed phase schedule (rate x payload x repetition) that the DUT
// and this traffic generator both walk, so the two stay in lockstep with no control channel — the
// ordering has to be bit-for-bit identical on both sides. Only linked for that measurement.
#[cfg(feature = "cpu-test-tx")]
#[path = "cpu_test_schedule.rs"]
mod cpu_test_schedule;

// ── Knobs ───────────────────────────────────────────────────────────────────────────────────────

/// Which end of the exchange this board drives.
const NETWORK_ROLE: NetworkRole = NetworkRole::Central;

/// What this run reports each second.
#[derive(PartialEq, Eq)]
#[allow(dead_code, reason = "selected by the `MEASURE` const; the unused arms are the other measurements")]
enum Measure {
    /// TX rate and TX-fail delta. A declining TX rate that tracks declining heap-free is
    /// esp-radio internal heap fragmentation from sustained dynamic-tx-buf churn.
    Throughput,
    /// Heap-free, sampled once a second.
    Heap,
    /// RX totals and the sequence-gap drop count.
    Drop,
    /// Quiet: no per-second line at all, so the serial port is not part of what is being
    /// measured. Used for the power run, where the instrument is at the supply.
    Quiet,
    /// CPU utilization: march the shared phase schedule, steering the library's real central TX
    /// loop through each (rate, payload, repetition) cell and emitting `TX_STATS` once a second.
    /// The DUT walks the identical schedule, which is what keeps the two in lockstep without a
    /// control channel. Requires `--features=cpu-test-tx`.
    Cpu,
}
const MEASURE: Measure = Measure::Throughput;

/// Both ends must agree. Channel 11 is what the heap and drop specs used.
///
/// For the CPU run it is taken from the shared schedule rather than restated, because the DUT
/// reads the same constant — a channel that disagreed would silently measure two boards that
/// never hear each other.
#[cfg(feature = "cpu-test-tx")]
const CHANNEL: u8 = cpu_test_schedule::TEST_CHANNEL;
#[cfg(not(feature = "cpu-test-tx"))]
const CHANNEL: u8 = 11;

/// Forced TX PHY. MCS0-LGI is the slowest HT rate and the one the drop spec used, because a slow
/// rate makes contention visible sooner.
const PHY_RATE: WifiPhyRate = WifiPhyRate::RateMcs0Lgi;

/// Control-packet rate (Hz) on the central. 10 kHz offers more than the radio can send, which is
/// the point for a saturation run.
const TRAFFIC_HZ: u16 = 10_000;

// ────────────────────────────────────────────────────────────────────────────────────────────────

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// On-air payload for the current phase (0 while silent), read by the per-second line.
#[cfg(feature = "cpu-test-tx")]
static CURRENT_WIRE_PAYLOAD: portable_atomic::AtomicU32 = portable_atomic::AtomicU32::new(0);

/// Marches the shared phase schedule, steering the library's real central TX loop through each
/// (rate, payload, repetition) cell.
///
/// The phase ordering must stay bit-for-bit identical to the DUT's — that identity is what keeps
/// the two boards in lockstep with no control channel between them — which is why the schedule
/// lives in the shared module and not here.
#[cfg(feature = "cpu-test-tx")]
async fn schedule_driver() -> ! {
    use cpu_test_schedule::{BOOT_DELAY_S, PhaseKind, phases_iter};
    use embassy_time::Instant;
    use esp_csi_rs::central::esp_now::{get_tx_failed_packets, get_tx_queued_packets};
    use esp_csi_rs::{set_test_tx_paused, set_test_tx_payload_b, set_test_tx_rate_hz};
    use portable_atomic::Ordering;

    /// Matches `esp_radio::esp_now::ESP_NOW_MAX_DATA_LEN`; a larger payload cannot go on air.
    const ESP_NOW_MAX_DATA_LEN: u32 = 250;

    log_ln!("CPU_FREQ,{}", 240_000_000u32);
    for (idx, p) in phases_iter() {
        log_ln!(
            "SCHEDULE,{},{},{},{},{},{}",
            idx, p.kind.as_str(), p.rate_hz, p.payload_b, p.rep, p.duration_s
        );
    }

    // Start silent; the central TX loop boots paused.
    set_test_tx_paused(true);
    Timer::after(Duration::from_secs(BOOT_DELAY_S as u64)).await;

    let t0 = Instant::now();
    let mut last_sent = get_tx_queued_packets();
    let mut last_failed = get_tx_failed_packets();

    for (idx, p) in phases_iter() {
        log_ln!(
            "PHASE_BEGIN,{},{},{},{},{},{}",
            t0.elapsed().as_millis(), idx, p.kind.as_str(), p.rate_hz, p.payload_b, p.rep
        );

        match p.kind {
            PhaseKind::BaselineWarmup | PhaseKind::BaselineCapture => {
                CURRENT_WIRE_PAYLOAD.store(0, Ordering::Relaxed);
                set_test_tx_paused(true);
            }
            PhaseKind::CellWarmup | PhaseKind::CellCapture => {
                let wire = p.payload_b.min(ESP_NOW_MAX_DATA_LEN);
                CURRENT_WIRE_PAYLOAD.store(wire, Ordering::Relaxed);
                set_test_tx_payload_b(wire as u16);
                set_test_tx_rate_hz(p.rate_hz as u16);
                set_test_tx_paused(false);
            }
        }

        // `sent`/`failed` are per-second deltas of the library's real send path, emitted while the
        // phase runs so a rate that the radio cannot sustain is visible in the trace rather than
        // only in the DUT's CPU number.
        for _ in 0..p.duration_s {
            Timer::after(Duration::from_secs(1)).await;
            let sent_total = get_tx_queued_packets();
            let failed_total = get_tx_failed_packets();
            log_ln!(
                "TX_STATS,{},{},{},{}",
                t0.elapsed().as_millis(),
                sent_total.wrapping_sub(last_sent),
                failed_total.wrapping_sub(last_failed),
                CURRENT_WIRE_PAYLOAD.load(Ordering::Relaxed),
            );
            last_sent = sent_total;
            last_failed = failed_total;
        }

        // Re-silence between phases so the boundary is clean.
        set_test_tx_paused(true);
        CURRENT_WIRE_PAYLOAD.store(0, Ordering::Relaxed);
        log_ln!("PHASE_END,{},{}", t0.elapsed().as_millis(), idx);
    }

    log_ln!("RUN_COMPLETE,{}", t0.elapsed().as_millis());
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

async fn report_task(_client: &mut CSINodeClient) {
    let mut last_tx_total = get_total_tx_packets();
    let mut last_rx_total = get_total_rx_packets();
    loop {
        Timer::after_secs(1).await;
        match MEASURE {
            Measure::Throughput => {
                let tx_total = get_total_tx_packets();
                log_ln!("TX: {}", tx_total.saturating_sub(last_tx_total));
                last_tx_total = tx_total;
            }
            Measure::Heap => {
                log_ln!("heap free: {}", esp_alloc::HEAP.free());
            }
            Measure::Drop => {
                let rx_total = get_total_rx_packets();
                log_ln!(
                    "RX: {}, RX PPS: {}, dropped: {}",
                    rx_total.saturating_sub(last_rx_total),
                    get_pps_rx(),
                    get_dropped_packets_rx(),
                );
                last_rx_total = rx_total;
            }
            // The CPU run emits its own `TX_STATS` line from the schedule driver, on the
            // schedule's clock rather than this one.
            Measure::Quiet | Measure::Cpu => {}
        }
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);

    // The CSI callback is registered and fires on every received frame even with the RX stats task
    // off. With the logging gate left open by `init_logger`, each frame CPU-spins the console
    // writing a verbose CSI line, which blocks the Wi-Fi task and depresses the very rate being
    // measured. Close the gate so the callback returns at its first atomic load.
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 60000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let wifi_controller = esp_radio::wifi::WifiController::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    install_static_espnow_recv();
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    log_ln!("ESP-NOW bench — channel {}", CHANNEL);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = NodeHardware::new(controller);
    let mut node = CSINode::esp_now(
        EspNowConfig::default()
            .with_channel(CHANNEL)
            .with_phy_rate(PHY_RATE)
            .with_network_role(NETWORK_ROLE)
            // Acquisition cost without delivery cost. This is what the retired
            // `set_csi_output_enabled(false)` was reaching for.
            .with_reporting(ReportingPolicy::Never),
        Some(CsiConfig::default()),
        Some(TRAFFIC_HZ),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    // A TX-side measurement has no use for the RX stats task.
    node.set_rx_enabled(NETWORK_ROLE == NetworkRole::Peripheral || MEASURE == Measure::Drop);

    #[cfg(feature = "cpu-test-tx")]
    if MEASURE == Measure::Cpu {
        join(node.run(), schedule_driver()).await;
    } else {
        join(node.run(), report_task(&mut node_handle)).await;
    }
    #[cfg(not(feature = "cpu-test-tx"))]
    join(node.run(), report_task(&mut node_handle)).await;

    loop {
        log_ln!("bench stopped");
        Timer::after(Duration::from_secs(5)).await;
    }
}
