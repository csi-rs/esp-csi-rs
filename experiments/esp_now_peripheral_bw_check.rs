//! Bandwidth / PHY diagnostic for the ESP-NOW CSI path.
//!
//! Answers one question: when the node is configured for MCS0-LGI + HT40, does
//! the radio actually receive HT40 frames, or does HT40 silently fail (because
//! `set_bandwidths` is a no-op unless the controller is in started STA/AP mode,
//! and the ESP-NOW path never sets a Wi-Fi mode)?
//!
//! It brings up the ESP-NOW peripheral with the SAME config the CPU-test exper
//! DUT uses (`with_ht40(Below)` + `set_rate(RateMcs0Lgi)`), registers a full
//! CSI callback, and logs the PHY fields of the first `N` received CSI reports:
//!
//!   BW_CHECK,<i>,sig_mode=<0=nonHT|1=HT|3=VHT>,bw=<0=20MHz|1=40MHz>,
//!            sec=<secondary_channel>,mcs=<mcs>,rate=<rate>,csi_len=<bytes>
//!
//! Read-off:
//!   * HT40 utilized  → bw=1, csi_len ≈ 256–384.
//!   * HT20 only      → bw=0, csi_len ≈ 128 (ESP32).
//!   * Legacy (no HT) → sig_mode=0 (rate setting also not engaging).
//!
//! Needs a TX broadcasting on the same channel — pair with
//! `esp_now_central_exper_cpu_tx` (it requests MCS0-LGI/HT40), or any ESP-NOW
//! broadcaster on channel 11. The RX-reported `bw`/`sig_mode` reflect the PPDU
//! actually transmitted, so this tests the TX's effective PHY end-to-end.
//!
//! Build: `cargo build --release --example esp_now_peripheral_bw_check --features <chip>,async-print`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    config::CsiConfig, log_ln, logging::logging::init_logger, set_csi_callback,
    set_csi_logging_enabled, CSINode, CSINodeClient, CSINodeHardware, CollectionMode, EspNowConfig,
    IOTaskConfig,
};
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
/// Log this many CSI reports, then go quiet (avoid flooding the console).
const N_REPORTS: u32 = 30;

static SEEN: AtomicU32 = AtomicU32::new(0);

/// Full CSI callback: log the PHY fields for the first `N_REPORTS` frames.
fn csi_cb(p: &CSIDataPacket) {
    let i = SEEN.fetch_add(1, Ordering::Relaxed);
    if i >= N_REPORTS {
        return;
    }
    log_ln!(
        "BW_CHECK,{},sig_mode={},bw={},sec={},mcs={},rate={},csi_len={}",
        i,
        p.sig_mode,
        p.bandwidth,
        p.secondary_channel,
        p.mcs,
        p.rate,
        p.csi_data_len
    );
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

    log_ln!("ESP-NOW bandwidth/PHY diagnostic — HT20-MCS0 expected: sig_mode=1,bw=0,csi_len~128");

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
                .with_channel(TEST_CHANNEL),
        )),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(10000),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    node.set_rate(WifiPhyRate::RateMcs0Lgi);
    node.set_io_tasks(IOTaskConfig::new(false, true));

    set_csi_callback(csi_cb);

    node.run().await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
