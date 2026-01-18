#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_time::{with_timeout, Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;
use esp_radio::{
    wifi::{ClientConfig, Interfaces, Protocol, WifiController},
    Controller,
};
use esp_csi_rs::{CSINode, EspNowConfig, WifiSnifferConfig, WifiStationConfig, config::CsiConfig, logging::logging::init_logger};
use {esp_backtrace as _, esp_println as _};

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

extern crate alloc;

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

use esp_csi_rs::log_ln;

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

    let radio_init = mk_static!(
        Controller<'static>,
        esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller")
    );
    // let radio_init = esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller");
    let (wifi_controller, interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");

    // let (mut wifi_controller, interfaces) = mk_static!(
    //     (WifiController, Interfaces),
    //     esp_radio::wifi::new(radio_init, peripherals.WIFI, Default::default())
    //         .expect("Failed to initialize Wi-Fi controller")
    // );

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    // let node = CSINode::new(
    //     testup::Node::Collector(testup::CollectorMode::EspNow),
    //     Some(CsiConfig::default()),
    //     Some(1),
    // )
    // .await;

    // // Create a Sniffer Node
    // let mut sniffer_node = CSINode::new(
    //     testup::Node::Collector(testup::CollectorMode::WifiSniffer(
    //         WifiSnifferConfig::default(),
    //     )),
    //     Some(CsiConfig::default()),
    //     Some(1),
    // )
    // .await;

    // sniffer_node.init(interfaces, spawner, controller).await;

    // // Create a Station Node
    // let mut sta_node = CSINode::new(
    //     testup::Node::Collector(testup::CollectorMode::WifiStation(WifiStationConfig {
    //         ntp_sync: false,
    //         client_config: ClientConfig::default()
    //             .with_ssid("Connected Motion ".into())
    //             .with_password("automotion@123".into())
    //             .with_auth_method(esp_radio::wifi::AuthMethod::Wpa2Personal),
    //     })),
    //     Some(CsiConfig::default()),
    //     Some(1),
    // )
    // .await;

    // Create a ESP NOW Node
    let mut sniffer_node = CSINode::new(
        esp_csi_rs::Node::Peripheral(esp_csi_rs::PeripheralOpMode::WifiSniffer(WifiSnifferConfig::default())),
        esp_csi_rs::CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(100),
    )
    .await;

    sniffer_node.init(interfaces, spawner, controller).await;

    // Collect for 5 Seconds
    with_timeout(Duration::from_secs(5000), async {
        loop {
            sniffer_node.print_csi_w_metadata().await;
        }
    })
    .await
    .unwrap_err();
    Timer::after(Duration::from_secs(5000)).await;

    sniffer_node.stop();

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v~1.0/examples
}
