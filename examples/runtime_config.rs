//! **Runtime reconfiguration** — running one node through several configurations.
//!
//! Drives the same hardware through two runs without reconstructing the node:
//! a sniffer on `CHANNEL_A`, then `set_operational_mode` to a sniffer on
//! `CHANNEL_B`. Anything the model admits can be swapped in this way — a station
//! for an access point, a central for a peripheral — because the operational
//! mode carries its own network role and collection mode with it.
//!
//! This example used to demonstrate `set_csi_output_enabled(false)` as the way
//! to keep capturing without delivering. That method never worked: it wrote a
//! flag no CSI path read. The attribute it was reaching for is
//! `CollectionMode::Listener`, which is set on the mode's config — see
//! `esp_now.rs`, where a listening node is one line.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer, with_timeout};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    CSINode, CSINodeClient, NodeHardware, OperationalMode, WifiSnifferConfig, config::CsiConfig,
    log_ln, logging::logging::init_logger,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::{get_dropped_packets_rx, get_pps_rx, get_total_rx_packets};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

const CHANNEL_A: u8 = 7;
const CHANNEL_B: u8 = 1;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// First run: report throughput on `CHANNEL_A` for 10 s.
async fn run_phase_a(client: &mut CSINodeClient) {
    log_ln!("Phase 1: sniffing channel {}", CHANNEL_A);

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

/// Second run: the same node on a different channel. Counters restart, because `run()` stamps a
/// fresh capture window — that is how you tell a reconfiguration apart from a continuing run.
async fn run_phase_b(client: &mut CSINodeClient) {
    log_ln!("Phase 2: sniffing channel {}", CHANNEL_B);

    with_timeout(Duration::from_secs(5), async {
        loop {
            Timer::after_secs(1).await;
            #[cfg(feature = "statistics")]
            log_ln!("total RX: {}", get_total_rx_packets());
            #[cfg(not(feature = "statistics"))]
            log_ln!("collecting...");
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
    let mut node = CSINode::sniffer(
        WifiSnifferConfig::default().with_channel(CHANNEL_A),
        Some(CsiConfig::default()),
        hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);

    join(node.run(), run_phase_a(&mut node_handle)).await;

    // Same node, different configuration. The mode carries its own network role and collection
    // mode, so there is nothing else to keep in step.
    node.set_operational_mode(OperationalMode::Sniffer(
        WifiSnifferConfig::default().with_channel(CHANNEL_B),
    ));
    join(node.run(), run_phase_b(&mut node_handle)).await;

    loop {
        log_ln!("Done");
        Timer::after(Duration::from_secs(5)).await;
    }
}
