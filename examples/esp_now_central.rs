#![no_std]
#![no_main]

use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Instant, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    config::CsiConfig, logging::logging::init_logger, CSINode, CollectionMode, EspNowConfig,
};
use esp_csi_rs::{
    get_dropped_packets_rx, get_pps_rx, get_pps_tx, get_total_rx_packets, get_total_tx_packets,
    log_ln, set_csi_callback, CSINodeClient, CSINodeHardware,
};
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

// Shared state written by the inline CSI callback and read by `stats_task`.
static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
static CSI_PKT_COUNT: AtomicU32 = AtomicU32::new(0);

// On-device CSI processing hook. Runs inline in the WiFi callback — keep
// it fast: no heap allocation, no locking, no blocking I/O.
fn on_csi(packet: &CSIDataPacket) {
    LATEST_RSSI.store(packet.rssi as i32, Ordering::Relaxed);
    CSI_PKT_COUNT.fetch_add(1, Ordering::Relaxed);
}

async fn stats_task() {
    let mut last_sample = Instant::now();
    let mut last_rx_total = get_total_rx_packets();
    let mut last_tx_total = get_total_tx_packets();

    loop {
        Timer::after_secs(1).await;

        let elapsed_us = last_sample.elapsed().as_micros() as u64;
        let rx_total = get_total_rx_packets();
        let tx_total = get_total_tx_packets();
        let rx_rate_hz = if elapsed_us == 0 {
            0
        } else {
            (rx_total.saturating_sub(last_rx_total) * 1_000_000 / elapsed_us) as u32
        };
        let tx_rate_hz = if elapsed_us == 0 {
            0
        } else {
            (tx_total.saturating_sub(last_tx_total) * 1_000_000 / elapsed_us) as u32
        };

        last_sample = Instant::now();
        last_rx_total = rx_total;
        last_tx_total = tx_total;

        log_ln!(
                "RX PPS(avg): {}, TX PPS(avg): {}, RX Hz(inst): {}, TX Hz(inst): {}, RX Total: {}, TX Total: {}, RX Dropped Packets: {}, CSI Packets: {}, Latest RSSI: {}",
                get_pps_rx(),
                get_pps_tx(),
                rx_rate_hz,
                tx_rate_hz,
                rx_total,
                tx_total,
                get_dropped_packets_rx(),
                CSI_PKT_COUNT.load(Ordering::Relaxed),
                LATEST_RSSI.load(Ordering::Relaxed),
            )
    }
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
    log_ln!("Starting EspNow Central Node");

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) =
        esp_radio::wifi::new(peripherals.WIFI, config_radio)
            .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::Node::Central(esp_csi_rs::CentralOpMode::EspNow(EspNowConfig::default())),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(1000),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    node.set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi);

    set_csi_callback(on_csi);
    let _ = &mut node_handle;
    join(node.run(), stats_task()).await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
