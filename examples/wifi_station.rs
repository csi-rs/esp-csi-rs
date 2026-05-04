#![no_std]
#![no_main]

use crate::alloc::string::ToString;
use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{with_timeout, Duration, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{config::CsiConfig, logging::logging::init_logger, CSINode, CollectionMode};
use esp_csi_rs::{
    get_dropped_packets_rx, get_one_way_latency, get_pps_rx, get_pps_tx, get_rx_rate_hz,
    get_tx_rate_hz, get_two_way_latency,
    log_ln, set_csi_callback, CSINodeClient, CSINodeHardware, WifiStationConfig,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::{
    wifi::{ClientConfig, WifiController},
    Controller,
};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

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

// Shared state written from the inline CSI callback and read by `node_task`.
static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
static CSI_CB_COUNT: AtomicU32 = AtomicU32::new(0);

// On-device CSI processing hook. Runs inline in the WiFi task — keep it
// fast: no heap allocation, no locking, no blocking I/O. For heavier work,
// copy what you need out of `packet` and post it to your own task.
fn on_csi(packet: &CSIDataPacket) {
    LATEST_RSSI.store(packet.rssi as i32, Ordering::Relaxed);
    CSI_CB_COUNT.fetch_add(1, Ordering::Relaxed);
}

async fn node_task(client: &mut CSINodeClient) {
    with_timeout(Duration::from_secs(1000), async {
        loop {
            Timer::after_secs(1).await;
            log_ln!(
                "RX PPS(avg): {}, TX PPS(avg): {}, RX Rate(Hz): {}, TX Rate(Hz): {}, RX Dropped Packets: {}, One Way Latency: {}, Two Way Latency: {}, Callback Invocations: {}, Latest RSSI: {}",
                get_pps_rx(),
                get_pps_tx(),
                get_rx_rate_hz(),
                get_tx_rate_hz(),
                get_dropped_packets_rx(),
                get_one_way_latency(),
                get_two_way_latency(),
                CSI_CB_COUNT.load(Ordering::Relaxed),
                LATEST_RSSI.load(Ordering::Relaxed),
            )
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
    init_logger(spawner, LogMode::ArrayList);

    // Keep reclaimed heap moderate so internal RAM remains available for
    // Wi-Fi/RTOS task stacks during STA scan/connect.
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

    let mut config_radio = esp_radio::wifi::Config::default().with_power_save_mode(esp_radio::wifi::PowerSaveMode::None);
    // Raise Wi-Fi buffer budget for sustained ESP-NOW + CSI traffic.
    config_radio = config_radio
        .with_power_save_mode(esp_radio::wifi::PowerSaveMode::None);
    let (wifi_controller, mut interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, config_radio)
            .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let client_config = ClientConfig::default()
        .with_ssid("SSID".to_string())
        .with_password("PASS".to_string())
        .with_auth_method(esp_radio::wifi::AuthMethod::Wpa2Personal);

    let station_config = WifiStationConfig {
        client_config, // Pass the config we created above
    };

    // Create a CSI Client Instance to handle CSI data and control messages
    let mut node_handle = CSINodeClient::new();
    // Create a CSINodeHardware instance which will be used by the CSINode to interact with the Wi-Fi hardware
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::Node::Central(esp_csi_rs::CentralOpMode::WifiStation(station_config)),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(1000),
        csi_hardware,
    );
    #[cfg(feature = "esp32c6")]
    node.set_protocol(esp_radio::wifi::Protocol::P802D11BGNAX);
    #[cfg(not(feature = "esp32c6"))]
    node.set_protocol(esp_radio::wifi::Protocol::P802D11BGN);

    // Register the on-device CSI processing hook before starting the node.
    // `set_csi_callback` also enables the CSI publish gate, so per-packet
    // logging continues to work alongside the callback.
    set_csi_callback(on_csi);

    join(node.run(), node_task(&mut node_handle)).await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v~1.0/examples
}
