#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    config::CsiConfig, logging::logging::init_logger, CSINode, CollectionMode,
};
use esp_csi_rs::{
    CSIClient, CSINodeHardware, WifiStationConfig, get_avg_pps, get_dropped_packets, get_total_packets, log_ln
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::{
    wifi::{ClientConfig, WifiController},
    Controller,
};
use {esp_backtrace as _, esp_println as _};
use crate::alloc::string::ToString;

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

async fn node_task(client: &mut CSIClient) {
    let mut last_log_time = Instant::now();

    with_timeout(Duration::from_secs(1000), async {
            loop {
                // Timer::after_millis(10).await;
                // if last_log_time.elapsed() >= Duration::from_secs(1) {
                //     log_ln!(
                //         "Total Packets: {}, Average PPS: {}, Dropped Packets: {}",
                //         get_total_packets(),
                //         get_avg_pps(),
                //         get_dropped_packets()
                //     );
                //     last_log_time = Instant::now();
                // }
                let _ = with_timeout(Duration::from_millis(10), client.print_csi_w_metadata()).await;
            }
        })
    .await
    .unwrap_err();
    client.send_stop().await;
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.1.0

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::ArrayList);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    #[cfg(any(feature = "esp32c6", feature = "esp32c3"))]
    {
        let sw_interrupt =
            esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
        esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);
    }
    #[cfg(not(any(feature = "esp32c6", feature = "esp32c3")))]
    esp_rtos::start(timg0.timer0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting Wifi Station Node");

    let radio_init = mk_static!(
        Controller<'static>,
        esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller")
    );

    let (wifi_controller, mut interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let client_config = ClientConfig::default()
        .with_ssid("Connected Motion ".to_string())
        .with_password("automotion@123".to_string())
        .with_auth_method(esp_radio::wifi::AuthMethod::Wpa2Personal);

    let station_config = WifiStationConfig {
        ntp_sync: true, // Set to true if you want NTP time sync
        client_config,  // Pass the config we created above
    };
    let mut node_handle = CSIClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::Node::Central(esp_csi_rs::CentralOpMode::WifiStation(station_config)),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(1000),
        csi_hardware
    );
    node.set_protocol(esp_radio::wifi::Protocol::P802D11BGNLR);
    node.set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi);

    join(
        node.run(),
        node_task(&mut node_handle),
    )
    .await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v~1.0/examples
}
