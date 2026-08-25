//! Continuous legacy 11 Mbps broadcaster — companion TX for
//! `esp_now_peripheral_bw_check`.
//!
//! Unlike the CPU-test TXs (schedule-gated, silent for the first ~40 s), this
//! drives the library central in plain TX-only mode at a fixed rate from boot,
//! so the paired `bw_check` peripheral starts logging serialized CSI within a
//! second. Uses **channel 11** (2.4 GHz HT20), **802.11n MCS0 long-GI**
//! (`RateMcs0Lgi`, an OFDM rate that carries the HT-LTF the receiver needs for
//! CSI), and **automatic magic-prefix pairing** — no hardcoded MACs.
//!
//! Build: `cargo build --release --example esp_now_central_bw_tx --features <chip>,async-print`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
#[cfg(feature = "statistics")]
use embassy_time::Instant;
use embassy_time::{Duration, Timer};
#[cfg(feature = "statistics")]
use esp_csi_rs::central::esp_now::{
    get_tx_confirmed_packets, get_tx_failed_packets, get_tx_queued_packets,
};
use esp_csi_rs::config::CsiConfig;
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    CSINode, CSINodeClient, CSINodeHardware, CentralOpMode, EspNowConfig,
    NodeRole, install_static_espnow_recv, log_ln, logging::logging::init_logger,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::{get_pps_tx, get_total_tx_packets};
use esp_csi_rs::{set_csi_callback, set_csi_logging_enabled};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::WifiController;
use portable_atomic::{AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

const TEST_CHANNEL: u8 = 11;
// HT20 CSI requires an OFDM PHY: the receiver derives CSI from the HT-LTF
// (MCS rates) or L-LTF (legacy-OFDM 6..54 Mbps). 802.11b DSSS rates such as
// Rate11mL carry NO training fields, so they produce no CSI report at all.
// MCS0 long-GI → HT20 HT-LTF (~56 subcarriers on the peripheral).
const REQUESTED_PHY_RATE: WifiPhyRate = WifiPhyRate::RateMcs0Lgi;

/// Cumulative subcarrier-count histogram. Index = subcarriers in a CSI report
/// (`csi_data_len / 2`, since each subcarrier is one i8 I + one i8 Q); the last
/// bin is an `>=` overflow bucket. Written from the CSI callback on the WiFi
/// task, read by `subcarrier_histogram_task`.
const SC_HIST_BINS: usize = 320;
static SC_HIST: [AtomicU32; SC_HIST_BINS] = [const { AtomicU32::new(0) }; SC_HIST_BINS];
static SC_TOTAL: AtomicU32 = AtomicU32::new(0);

/// Inline CSI callback: bin each report by its subcarrier count instead of
/// logging the packet. Runs on the WiFi-task hot path, so it does only two
/// relaxed atomic adds — no formatting, no allocation.
fn on_csi(packet: &CSIDataPacket) {
    let subcarriers = (packet.csi_data_len / 2) as usize;
    let idx = subcarriers.min(SC_HIST_BINS - 1);
    SC_HIST[idx].fetch_add(1, Ordering::Relaxed);
    SC_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Periodically dump the cumulative subcarrier-count histogram (non-empty bins).
#[embassy_executor::task]
async fn subcarrier_histogram_task() {
    loop {
        Timer::after_secs(2).await;
        let total = SC_TOTAL.load(Ordering::Relaxed);
        if total == 0 {
            log_ln!("Subcarrier histogram: no CSI captured yet");
            continue;
        }
        log_ln!("Subcarrier histogram (cumulative, {} reports):", total);
        for sc in 0..SC_HIST_BINS {
            let count = SC_HIST[sc].load(Ordering::Relaxed);
            if count == 0 {
                continue;
            }
            let pct = (count as u64 * 100 / total as u64) as u32;
            if sc == SC_HIST_BINS - 1 {
                log_ln!("  >={} subcarriers: {} ({}%)", sc, count, pct);
            } else {
                log_ln!("  {} subcarriers: {} ({}%)", sc, count, pct);
            }
        }
    }
}

#[embassy_executor::task]
async fn tx_stats_task() {
    #[cfg(feature = "statistics")]
    let mut last_sample = Instant::now();
    #[cfg(feature = "statistics")]
    let mut last_total = get_total_tx_packets();
    #[cfg(feature = "statistics")]
    let mut last_queued = get_tx_queued_packets();
    #[cfg(feature = "statistics")]
    let mut last_confirmed = get_tx_confirmed_packets();
    #[cfg(feature = "statistics")]
    let mut last_failed = get_tx_failed_packets();

    loop {
        Timer::after_secs(1).await;
        #[cfg(feature = "statistics")]
        {
            let elapsed_us = last_sample.elapsed().as_micros() as u64;
            let tx_total = get_total_tx_packets();
            let tx_hz = if elapsed_us == 0 {
                0
            } else {
                (tx_total.saturating_sub(last_total) * 1_000_000 / elapsed_us) as u32
            };
            let queued_total = get_tx_queued_packets();
            let confirmed_total = get_tx_confirmed_packets();
            let failed_total = get_tx_failed_packets();
            let queued_hz = queued_total.saturating_sub(last_queued);
            let confirmed_hz = confirmed_total.saturating_sub(last_confirmed);
            let failed_hz = failed_total.saturating_sub(last_failed);
            last_sample = Instant::now();
            last_total = tx_total;
            last_queued = queued_total;
            last_confirmed = confirmed_total;
            last_failed = failed_total;
            log_ln!(
                "Central TX stats: TX PPS(avg)={}, TX Hz(inst)={}, TX Total={}, queued_hz={}, queued_total={}, confirmed_hz={}, confirmed_total={}, failed_hz={}, failed_total={}",
                get_pps_tx(),
                tx_hz,
                tx_total,
                queued_hz,
                queued_total,
                confirmed_hz,
                confirmed_total,
                failed_hz,
                failed_total
            );
        }
        #[cfg(not(feature = "statistics"))]
        {
            log_ln!("Central TX stats: statistics feature disabled");
        }
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);
    // Histogram mode: do not log each CSI packet inline — the registered
    // `on_csi` callback bins reports by subcarrier count instead.
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!(
        "Continuous legacy broadcaster @100 Hz on channel {} (2.4 GHz HT20, {:?})",
        TEST_CHANNEL,
        REQUESTED_PHY_RATE
    );
    log_ln!(
        "Central config: role=ESP-NOW central listener, io_tasks=TX-only, auto-pairing (broadcast control)"
    );
    log_ln!(
        "C5 note: broadcast PHY forcing is skipped on dual-band; driver default legacy may apply"
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default().with_ampdu_tx_enable(false);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    install_static_espnow_recv();

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let csi_cfg = CsiConfig {
        // acquire_csi_legacy: 1,
        // acquire_csi_force_lltf: true,
        // acquire_csi_ht20: 1,
        // acquire_csi_ht40: 0,
        // dump_ack_en: 0,
        ..CsiConfig::default()
    };

    let mut _node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        NodeRole::Central(CentralOpMode::EspNow(
            // HT20: do NOT call with_ht40 — `with_ht40(SecondaryChannel::None)`
            // still flags the node HT40 (`secondary_channel().is_some()`), which
            // on C5 forces the 2.4 GHz interface to 40 MHz and breaks RX.
            EspNowConfig::default()
                .with_channel(TEST_CHANNEL)
                .with_phy_rate(REQUESTED_PHY_RATE),
        )),
        Some(csi_cfg), // central is TX-only; CSI is collected on the paired peripheral
        Some(1000),
        csi_hardware,
    );
    node.set_rate(REQUESTED_PHY_RATE);
    node.set_protocol(esp_radio::wifi::Protocol::N);
    // Bin captured CSI by subcarrier count instead of printing each packet.
    set_csi_callback(on_csi);
    spawner.spawn(tx_stats_task().unwrap());
    spawner.spawn(subcarrier_histogram_task().unwrap());
    node.run().await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
