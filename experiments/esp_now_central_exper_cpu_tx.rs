//! Companion TX firmware for the CPU-utilization experiment (full-library variant).
//!
//! Unlike the minimal TX (`esp_now_central_min_cpu_tx`, a raw `esp_now.send`
//! broadcaster), this TX drives the **real esp-csi-rs library** central path:
//! it brings up a `CSINode` in `NodeRole::Central(CentralOpMode::EspNow)` and lets
//! `run_esp_now_central` do the sending. A schedule-driver task steers that
//! loop from the shared `cpu_test_schedule` via the `cpu-test-tx` runtime hooks
//! (`set_test_tx_rate_hz` / `set_test_tx_payload_b` / `set_test_tx_paused`),
//! so the central emits real magic-prefixed `ControlPacket`s — padded up to the
//! cell payload — that the DUT (`esp_now_peripheral_exper_cpu`) fully ingests.
//! This makes the exper pair measure the esp-csi-rs full-stack cost (framing +
//! ingest + CSIDataPacket delivery), not just the radio/CSI floor that the min
//! pair measures.
//!
//! PHY parity with the esp-csi C reference: MCS0-LGI + HT40 (secondary below) on
//! `TEST_CHANNEL`. The on-air payload is `min(payload_b, ESP_NOW_MAX_DATA_LEN)`
//! (250 B cap); nominal `payload_b ∈ {32,128,512}` is still logged in
//! `SCHEDULE`/`PHASE_BEGIN`, while the actual on-air length is in
//! `TX_STATS,...,<wire_payload>` (spec §6.2). HT40 for ESP-NOW is best-effort in
//! esp-radio — confirm on-air (DUT CSI `bandwidth` field) that HT40 engaged.
//!
//! No `statistics` feature: the `cpu-test-tx` feature alone exposes the TX
//! send/fail counters used for `TX_STATS`, so the DUT carries none of the
//! per-frame statistics overhead the esp-csi C++ reference lacks. Both firmwares
//! run auto-pairing (no `with_peer_mac`), so the magic prefix is on the wire,
//! and both omit `statistics`, so `ControlPacket` is `{is_collector}` on both
//! sides (the DUT's `take_from_bytes` ignores the payload padding).
//!
//! Sync model: both firmwares wait `BOOT_DELAY_S` after `esp_rtos::start`. Power
//! both within that window. Clock drift at 240 MHz Xtensa is ≤±20 ppm.
//!
//! Build: `cargo build --release --example esp_now_central_exper_cpu_tx --features <chip>,cpu-test-tx,async-print`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_time::{Duration, Instant, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    CSINode, CSINodeClient, CSINodeHardware, CentralOpMode, EspNowConfig,
    IOTaskConfig, Node,
    central::esp_now::{get_tx_failed_packets, get_tx_queued_packets},
    config::CsiConfig,
    log_ln,
    logging::logging::init_logger,
    set_test_tx_paused, set_test_tx_payload_b, set_test_tx_rate_hz,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{ESP_NOW_MAX_DATA_LEN, WifiPhyRate};
use esp_radio::wifi::WifiController;
use portable_atomic::{AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

#[path = "cpu_test_schedule.rs"]
mod cpu_test_schedule;
use cpu_test_schedule::{BOOT_DELAY_S, PhaseKind, TEST_CHANNEL, phases_iter};

extern crate alloc;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// On-air payload size for the current phase (0 while silent), published by the
/// schedule driver and read by the per-second emitter for `TX_STATS`.
static CURRENT_WIRE_PAYLOAD: AtomicU32 = AtomicU32::new(0);

/// Per-second `TX_STATS` emitter. `sent`/`failed` are per-window deltas of the
/// library's central TX counters (the real send path; exposed by `cpu-test-tx`).
async fn per_second_emitter() -> ! {
    let t0 = Instant::now();
    let mut last_sent: u64 = 0;
    let mut last_failed: u64 = 0;
    loop {
        Timer::after(Duration::from_secs(1)).await;
        let t_ms = t0.elapsed().as_millis();
        let sent_total = get_tx_queued_packets();
        let failed_total = get_tx_failed_packets();
        let sent = sent_total.wrapping_sub(last_sent);
        let failed = failed_total.wrapping_sub(last_failed);
        last_sent = sent_total;
        last_failed = failed_total;
        let wire = CURRENT_WIRE_PAYLOAD.load(Ordering::Relaxed);
        log_ln!("TX_STATS,{},{},{},{}", t_ms, sent, failed, wire);
    }
}

/// Marches the shared schedule, steering the library central TX loop per phase.
async fn schedule_driver() -> ! {
    log_ln!("CPU_FREQ,{}", 240_000_000u32);
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

    // Start silent; the central loop boots paused (TEST_TX_PAUSED defaults true).
    set_test_tx_paused(true);

    Timer::after(Duration::from_secs(BOOT_DELAY_S as u64)).await;
    let t0 = Instant::now();

    for (idx, p) in phases_iter() {
        let t_ms = t0.elapsed().as_millis();
        log_ln!(
            "PHASE_BEGIN,{},{},{},{},{},{}",
            t_ms,
            idx,
            p.kind.as_str(),
            p.rate_hz,
            p.payload_b,
            p.rep
        );

        match p.kind {
            PhaseKind::BaselineWarmup | PhaseKind::BaselineCapture => {
                CURRENT_WIRE_PAYLOAD.store(0, Ordering::Relaxed);
                set_test_tx_paused(true);
            }
            PhaseKind::CellWarmup | PhaseKind::CellCapture => {
                let wire_payload = (p.payload_b as usize).min(ESP_NOW_MAX_DATA_LEN) as u32;
                CURRENT_WIRE_PAYLOAD.store(wire_payload, Ordering::Relaxed);
                set_test_tx_payload_b(wire_payload as u16);
                set_test_tx_rate_hz(p.rate_hz as u16);
                set_test_tx_paused(false);
            }
        }

        Timer::after(Duration::from_secs(p.duration_s as u64)).await;

        // Re-silence between phases so the boundary is clean.
        set_test_tx_paused(true);
        CURRENT_WIRE_PAYLOAD.store(0, Ordering::Relaxed);

        let t_ms = t0.elapsed().as_millis();
        log_ln!("PHASE_END,{},{}", t_ms, idx);
    }

    log_ln!("RUN_COMPLETE,{}", t0.elapsed().as_millis());
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(_spawner, LogMode::Text);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting ESP-NOW TX CPU-Utilization Traffic Generator (full-library)");

    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_static_tx_buf_num(25)
        .with_dynamic_tx_buf_num(128)
        .with_ampdu_tx_enable(false)
        .with_tx_queue_size(32);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut _node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        NodeRole::Central(CentralOpMode::EspNow(
            EspNowConfig::default()
                .with_channel(TEST_CHANNEL)
                .with_phy_rate(WifiPhyRate::RateMcs0Lgi),
        )),
        // Collector → ControlPacket.is_collector = true, so the DUT keeps its
        // configured (collector) mode without mode-switch churn.
        Some(CsiConfig::default()),
        Some(500),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    // PHY (MCS0-LGI + HT40) is set on the EspNowConfig above; applied per-peer
    // on the broadcast peer once the radio is up in STA mode.
    // TX-only generator: TX enabled, RX disabled.
    node.set_io_tasks(IOTaskConfig::new(true, false));

    join3(node.run(), schedule_driver(), per_second_emitter()).await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
