#![no_std]
#![no_main]

use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{config::CsiConfig, logging::logging::init_logger, CSINode, CollectionMode};
use esp_csi_rs::{
    log_ln, set_csi_callback, CSINodeClient, CSINodeHardware, WifiSnifferConfig,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::{
    wifi::{PowerSaveMode, WifiController},
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

// Shared state written by the inline CSI callback. A typical sniffer
// application processes packets in this hook (e.g. compute statistics,
// extract subcarriers, run a model) without spawning a consumer task.
static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
static CSI_PKT_COUNT: AtomicU32 = AtomicU32::new(0);

// On-device CSI processing hook. Runs inline in the WiFi callback — keep
// it fast: no heap allocation, no locking, no blocking I/O.
fn on_csi(packet: &CSIDataPacket) {
    LATEST_RSSI.store(packet.rssi as i32, Ordering::Relaxed);
    CSI_PKT_COUNT.fetch_add(1, Ordering::Relaxed);
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
    log_ln!("Starting Wi-Fi Sniffer Peripheral Node");

    let radio_init = mk_static!(
        Controller<'static>,
        esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller")
    );

    let config_radio = esp_radio::wifi::Config::default().with_power_save_mode(PowerSaveMode::None);
    let (wifi_controller, mut interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, config_radio)
            .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::Node::Peripheral(esp_csi_rs::PeripheralOpMode::WifiSniffer(
            WifiSnifferConfig::default(),
        )),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(1000),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::P802D11BGNLR);
    node.set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi);

    // Register the on-device CSI processing hook before starting the node.
    // Each sniffed CSI report invokes `on_csi` inline in the WiFi callback.
    set_csi_callback(on_csi);

    node.run_duration(1000, &mut node_handle).await;

    log_ln!(
        "Sniffer stopped. Total CSI packets: {}, Last observed RSSI: {}",
        CSI_PKT_COUNT.load(Ordering::Relaxed),
        LATEST_RSSI.load(Ordering::Relaxed),
    );

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v~1.0/examples
}
