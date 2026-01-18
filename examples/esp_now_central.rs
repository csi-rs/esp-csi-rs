#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{with_timeout, Duration, Timer};
use esp_csi_rs::log_ln;
use esp_csi_rs::{
    config::CsiConfig, logging::logging::init_logger, CSINode, CollectionMode, EspNowConfig,
    PeripheralOpMode,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;
use esp_radio::{
    wifi::{ClientConfig, Interfaces, WifiController},
    Controller,
};
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

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.1.0

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    // esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);
    esp_rtos::start(timg0.timer0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting ESP-NOW Central Node");

    let radio_init = mk_static!(
        Controller<'static>,
        esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller")
    );

    let (wifi_controller, interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");

    let mut node = CSINode::new(
        esp_csi_rs::Node::Central(esp_csi_rs::CentralOpMode::EspNow((EspNowConfig::default()))),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(1000),
    )
    .await;

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    use embassy_time::{Duration, Instant}; // Or use std::time::Instant if running on PC

    // ... (your init code) ...
    node.init(interfaces, spawner, controller).await;

    let mut last_log_time = Instant::now();
    let mut start_time = Instant::now();
    let mut total_packets: u64 = 0;

    with_timeout(Duration::from_secs(1000), async {
        loop {
            node.get_csi_data().await;
            total_packets += 1;
            if last_log_time.elapsed() >= Duration::from_secs(1) {
                let elapsed_secs = start_time.elapsed().as_secs();
                let avg_packets: u64 = if elapsed_secs == 0 {
                    total_packets
                } else {
                    total_packets / elapsed_secs
                };
                log_ln!(
                    "Total Packets: {}, Average PPS: {}",
                    total_packets,
                    avg_packets
                );
                last_log_time = Instant::now();
            }
        }
    })
    .await
    .unwrap_err();

    node.stop();

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v~1.0/examples
}
