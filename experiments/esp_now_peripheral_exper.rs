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
use esp_csi_rs::get_total_rx_packets;
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

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

async fn node_task(_client: &mut CSINodeClient) {
    // `get_total_rx_packets` increments unconditionally inside
    // `capture_csi_info` before the publish gate, so it counts raw radio
    // events even with `set_csi_logging_enabled(false)`.
    let mut last_rx_total = get_total_rx_packets();
    loop {
        Timer::after_secs(1).await;
        let rx_total = get_total_rx_packets();
        let rx_delta = rx_total.saturating_sub(last_rx_total);
        last_rx_total = rx_total;
        log_ln!("RX: {}", rx_delta);
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.1.0

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting EspNow Peripheral Node (Exper)");

    // Raise Wi-Fi buffer budget for sustained ESP-NOW + CSI traffic.
    let config_radio = esp_radio::wifi::ControllerConfig::default();
    // .with_static_rx_buf_num(25)
    // .with_dynamic_rx_buf_num(128)
    // .with_ampdu_rx_enable(false)
    // .with_rx_queue_size(32);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::NodeRole::Peripheral(esp_csi_rs::PeripheralOpMode::EspNow(
            EspNowConfig::default().with_channel(1),
        )),
        Some(CsiConfig::default()),
        Some(10000),
        csi_hardware,
    );
    // `CollectionMode::Listener` became this: keep the radio capturing, and its
    // timing intact, but deliver nothing off-device.
    node.set_csi_output_enabled(false);
    node.set_protocol(esp_radio::wifi::Protocol::N);
    node.set_tx_enabled(false);

    join(node.run(), node_task(&mut node_handle)).await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v~1.0/examples
}
