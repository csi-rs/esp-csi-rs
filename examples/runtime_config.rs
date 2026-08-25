//! Reconfiguring a node between runs, and toggling CSI output at runtime.
//!
//! Runs a sniffer collector twice over the same hardware. The first run delivers
//! CSI normally and reports throughput; the second disables CSI *output* while
//! leaving capture running, which keeps the RX path and its timing identical but
//! stops anything being decoded, logged, or handed to a callback. That is the
//! distinction the old `CollectionMode::Listener` was reaching for, expressed as
//! what it actually controls.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer, with_timeout};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    CSINode, CSINodeClient, CollectorMode, NodeHardware, WifiSnifferConfig, config::CsiConfig,
    log_ln, logging::logging::init_logger,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::{get_dropped_packets_rx, get_pps_rx, get_total_rx_packets};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

const CHANNEL: u8 = 7;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// First run: CSI output on, report throughput for 10 s.
async fn run_with_output(client: &mut CSINodeClient) {
    log_ln!("Phase 1: CSI output enabled");

    with_timeout(Duration::from_secs(10), async {
        loop {
            Timer::after_secs(1).await;
            #[cfg(feature = "statistics")]
            {
                log_ln!(
                    "RX PPS: {}, total RX: {}, dropped: {}",
                    get_pps_rx(),
                    get_total_rx_packets(),
                    get_dropped_packets_rx()
                );
            }
            #[cfg(not(feature = "statistics"))]
            {
                log_ln!("collecting...");
            }
        }
    })
    .await
    .unwrap_err();
    client.send_stop().await;
}

/// Second run: capture continues, delivery is off. Statistics still climb —
/// that is the point, and it is how you tell this apart from simply stopping.
async fn run_without_output(client: &mut CSINodeClient) {
    log_ln!("Phase 2: CSI output disabled (capture still running)");

    with_timeout(Duration::from_secs(5), async {
        loop {
            Timer::after_secs(1).await;
            #[cfg(feature = "statistics")]
            log_ln!("captured but not delivered — total RX: {}", get_total_rx_packets());
            #[cfg(not(feature = "statistics"))]
            log_ln!("capturing, not delivering...");
        }
    })
    .await
    .unwrap_err();
    client.send_stop().await;
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let hardware = NodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new_collector(
        CollectorMode::Sniffer(WifiSnifferConfig::default().with_channel(CHANNEL)),
        Some(CsiConfig::default()),
        None,
        hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);

    join(node.run(), run_with_output(&mut node_handle)).await;

    node.set_csi_output_enabled(false);
    join(node.run(), run_without_output(&mut node_handle)).await;

    loop {
        log_ln!("Done");
        Timer::after(Duration::from_secs(5)).await;
    }
}
