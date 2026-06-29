//! Bandwidth / PHY diagnostic for the ESP-NOW CSI path (2.4 GHz HT20 legacy).
//!
//! Brings up an ESP-NOW peripheral in **Collector** mode (RX-only) on channel
//! **11** with automatic magic-prefix pairing — no hardcoded peer MACs. Pair
//! with `esp_now_central_bw_tx`, which broadcasts control frames on the same
//! channel at a fixed rate from boot.
//!
//! Serialized CSI (`LogMode::Serialized`) streams each capture to the host.
//! Decode with `python/csi_serial_to_parquet.py --port /dev/ttyACM* --stats-only`.
//!
//! Expected on success (HT20 MCS0, default CsiConfig):
//!   * **Subcarriers ~56** (`csi_data_len / 2`) — HT-LTF on 20 MHz
//!   * **rate = MCS0** — 802.11n HT20 (the central transmits `RateMcs0Lgi`)
//!   * **channel 11**
//!
//! NOTE: CSI is derived from OFDM training fields. The central MUST transmit an
//! OFDM rate (MCS0..7, or legacy-OFDM 6..54 Mbps for L-LTF only). An 802.11b
//! DSSS rate like `Rate11mL` carries no LTF, so the ESP-NOW frames are received
//! but the CSI engine never fires `capture_csi_info` — no CSI reaches serial.
//!
//! Build: `cargo build --release --example esp_now_peripheral_bw_check --features <chip>,async-print,jtag-serial`
//! (or `auto` instead of `jtag-serial` so the async logger drains serialized CSI).

#![no_std]
#![no_main]

use embassy_executor::Spawner;
#[cfg(feature = "statistics")]
use embassy_time::Instant;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::LogMode;
#[cfg(feature = "statistics")]
use esp_csi_rs::peripheral::esp_now::{
    get_rx_control_packets, get_rx_magic_drop_packets, get_rx_parse_fail_packets,
};
use esp_csi_rs::{
    CSINode, CSINodeClient, CSINodeHardware, CollectionMode, EspNowConfig, IOTaskConfig,
    config::CsiConfig, install_static_espnow_recv, log_ln, logging::logging::init_logger,
    set_csi_logging_enabled,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::{
    get_dropped_packets_rx, get_pps_rx, get_pps_tx, get_total_rx_packets, get_total_tx_packets,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

const TEST_CHANNEL: u8 = 11;

/// Periodic TX/RX rate reporter.
///
/// RX counts CSI reports captured by the WiFi callback; TX counts ESP-NOW
/// presence-beacon replies the peripheral sent back to the central. Both
/// `inst` (this 1 s window) and `avg` (since capture start) are shown.
#[embassy_executor::task]
async fn stats_task() {
    #[cfg(feature = "statistics")]
    let mut last_sample = Instant::now();
    #[cfg(feature = "statistics")]
    let mut last_rx_total = get_total_rx_packets();
    #[cfg(feature = "statistics")]
    let mut last_tx_total = get_total_tx_packets();

    loop {
        Timer::after_secs(1).await;
        #[cfg(feature = "statistics")]
        {
            let elapsed_us = last_sample.elapsed().as_micros() as u64;
            let rx_total = get_total_rx_packets();
            let tx_total = get_total_tx_packets();
            let rx_hz = if elapsed_us == 0 {
                0
            } else {
                (rx_total.saturating_sub(last_rx_total) * 1_000_000 / elapsed_us) as u32
            };
            let tx_hz = if elapsed_us == 0 {
                0
            } else {
                (tx_total.saturating_sub(last_tx_total) * 1_000_000 / elapsed_us) as u32
            };
            last_sample = Instant::now();
            last_rx_total = rx_total;
            last_tx_total = tx_total;
            log_ln!(
                "Peripheral stats: RX Hz(inst)={}, RX PPS(avg)={}, RX Total={}, TX Hz(inst)={}, TX PPS(avg)={}, TX Total={}, rx_dropped={}",
                rx_hz,
                get_pps_rx(),
                rx_total,
                tx_hz,
                get_pps_tx(),
                tx_total,
                get_dropped_packets_rx()
            );
            // ESP-NOW frame counters distinguish a dead link from a CSI-only
            // problem. `RX Total` above counts CSI reports; these count raw
            // ESP-NOW frames from the central:
            //   * control_pkts > 0, RX Total == 0 → frames arrive but no CSI
            //     (PHY/rate issue — the central's frames carry no usable LTF).
            //   * control_pkts == 0 && magic_drops == 0 && parse_fails == 0 →
            //     no ESP-NOW frames at all (channel/band/TX/range — link is dead).
            //   * magic_drops > 0 → frames arrive but fail the pairing magic.
            log_ln!(
                "Peripheral ESP-NOW RX: control_pkts={}, magic_drops={}, parse_fails={}",
                get_rx_control_packets(),
                get_rx_magic_drop_packets(),
                get_rx_parse_fail_packets()
            );
        }
        #[cfg(not(feature = "statistics"))]
        {
            log_ln!("Peripheral stats: statistics feature disabled");
        }
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 62000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!(
        "ESP-NOW peripheral CSI diagnostic — channel {} (2.4 GHz HT20 OFDM/MCS0), auto-pairing",
        TEST_CHANNEL
    );
    log_ln!(
        "Peripheral config: io_tasks=RX-only, CollectionMode=Collector, serialized CSI, phy={:?}",
        WifiPhyRate::RateMcs0Lgi
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default().with_ampdu_rx_enable(false);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    install_static_espnow_recv();

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut _node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    // `with_phy_rate` brings the radio up in started STA mode (required for CSI
    // on ESP-NOW RX). The peripheral is listen-only — rate matches the central.
    let mut node = CSINode::new(
        esp_csi_rs::Node::Peripheral(esp_csi_rs::PeripheralOpMode::EspNow(
            EspNowConfig::default()
                .with_channel(TEST_CHANNEL)
                .with_phy_rate(WifiPhyRate::RateMcs0Lgi),
        )),
        CollectionMode::Listener,
        Some(CsiConfig::default()),
        Some(10000),
        csi_hardware,
    );
    node.set_rate(WifiPhyRate::RateMcs0Lgi);
    node.set_protocol(esp_radio::wifi::Protocol::N);
    spawner.spawn(stats_task().unwrap());
    node.run().await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
