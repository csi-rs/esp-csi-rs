#![no_std]
#![no_main]

#[cfg(not(feature = "statistics"))]
compile_error!("This experiment requires the `statistics` feature.");

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    CSINode, EspNowConfig, config::CsiConfig, logging::logging::init_logger,
};
use esp_csi_rs::{
    CSINodeClient, CSINodeHardware, log_ln, set_csi_logging_enabled,
};
#[cfg(feature = "statistics")]
use esp_csi_rs::get_total_tx_packets;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]


async fn node_task(_client: &mut CSINodeClient) {
    // Diagnostic counters: TX rate, TX-fail delta (WiFi-driver OOM / SendFailed
    // per second), and heap-free. A monotonically declining heap-free that
    // tracks a declining TX rate indicates esp-radio internal heap
    // fragmentation from sustained dynamic-tx-buf churn.
    let mut last_tx_total = get_total_tx_packets();
    loop {
        Timer::after_secs(1).await;
        let tx_total = get_total_tx_packets();
        let tx_delta = tx_total.saturating_sub(last_tx_total);
        last_tx_total = tx_total;
        log_ln!("TX: {}", tx_delta);
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);
    // TX-only Listener experiment: `set_rx_enabled(false)` only skips the
    // RX stats task — the WiFi CSI callback is still registered and fires on
    // every received reply. With the gate left open by `init_logger`, each
    // reply CPU-spins UART writing a verbose CSI line, which blocks the
    // WiFi task and slows TX. Close the gate so the callback returns at the
    // first atomic load.
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 60000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting EspNow Central Node (Exper)");

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    // .with_static_tx_buf_num(25)
    // .with_dynamic_tx_buf_num(128)
    // .with_ampdu_tx_enable(false).
    // with_tx_queue_size(32);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::NodeRole::Central(esp_csi_rs::CentralOpMode::EspNow(
            EspNowConfig::default().with_channel(11),
        )),
        Some(CsiConfig::default()),
        Some(10000),
        csi_hardware,
    );
    // `CollectionMode::Listener` became this: keep the radio capturing, and its
    // timing intact, but deliver nothing off-device.
    node.set_csi_output_enabled(false);
    node.set_protocol(esp_radio::wifi::Protocol::N);
    node.set_rx_enabled(false);
    node.set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi);

    join(node.run(), node_task(&mut node_handle)).await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
