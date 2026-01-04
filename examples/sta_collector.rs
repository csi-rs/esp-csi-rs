//! Example of Station Mode Trigger for CSI Collection
//!
//! This configuration collects CSI data locally by generating traffic to a connected WiFi router or ESP Access Point.
//!
//! At least two devices are needed in this configuration.
//!
//! Connection Options:
//! - Option 1: Connect to an existing commercial router
//! - Option 2: Connect to another ESP operating as an AP Monitor or AP/STA Monitor
//!
//! The SSID and Password defined is for the Access Point or Router the ESP Station will be connecting to.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{with_timeout, Duration, Timer};
use esp_bootloader_esp_idf::esp_app_desc;
use esp_csi_rs::{
    collector::{CSIStation, StaOperationMode, StaTriggerConfig},
    config::CSIConfig,
};
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_println as _;
use esp_println::println;
use esp_wifi::{init, wifi::ClientConfiguration, EspWifiController};

esp_app_desc!();

extern crate alloc;

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    // Configure System Clock
    let config = esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::max());
    // Take Peripherals
    let peripherals = esp_hal::init(config);

    // Allocate some heap space
    esp_alloc::heap_allocator!(size: 72 * 1024);

    println!("Embassy Init");
    // Initialize Embassy
    let timg1 = TimerGroup::new(peripherals.TIMG1);
    esp_hal_embassy::init(timg1.timer0);

    // Instantiate peripherals necessary to set up  WiFi
    let timer1 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
    let wifi = peripherals.WIFI;
    let timer = timer1.timer0;
    let rng = Rng::new(peripherals.RNG);

    println!("Controller Init");
    // Initialize WiFi Controller
    let init = &*mk_static!(EspWifiController<'static>, init(timer, rng,).unwrap());

    // Instantiate WiFi controller and interfaces
    let (controller, interfaces) = esp_wifi::wifi::new(&init, wifi).unwrap();

    println!("WiFi Controller Initialized");

    // Create a CSI collector station configuration to establish/trigger traffic
    let mut csi_coll_sta = CSIStation::new(
        CSIConfig::default(),
        ClientConfiguration {
            ssid: "esp".into(),
            password: "12345678".into(),
            auth_method: esp_wifi::wifi::AuthMethod::WPA2Personal,
            channel: Some(1),
            ..Default::default()
        },
        // Configure the traffic frequency to 1 Hz (1 packets per second)
        StaOperationMode::Trigger(StaTriggerConfig { trigger_freq_hz: 1 }),
        // Set to true only if there is an internet connection at AP (commercial router or AP+STA with internet)
        false,
        controller,
    )
    .await;

    // Initalize CSI collector
    csi_coll_sta.init(interfaces, &spawner).await.unwrap();

    // Start Collection
    csi_coll_sta.start_collection().await;

    // Collect for 10 Seconds
    with_timeout(Duration::from_secs(5), async {
        loop {
            csi_coll_sta.print_csi_w_metadata().await;
        }
    })
    .await
    .unwrap_err();

    // Stop Collection
    csi_coll_sta.stop_collection().await;

    println!("Starting Again in 3 seconds");
    Timer::after(Duration::from_secs(3)).await;

    // Start Collection
    csi_coll_sta.start_collection().await;

    // Collect for 2 Seconds
    with_timeout(Duration::from_secs(2), async {
        loop {
            csi_coll_sta.print_csi_w_metadata().await;
        }
    })
    .await
    .unwrap_err();

    // Stop Collection
    csi_coll_sta.stop_collection().await;

    loop {
        Timer::after(Duration::from_secs(1)).await
    }
}
