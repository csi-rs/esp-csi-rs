#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Instant, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    CSINode, CollectionMode, EspNowConfig, config::CsiConfig, logging::logging::init_logger,
};
use esp_csi_rs::{CSINodeClient, CSINodeHardware, log_ln, set_csi_logging_enabled};
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
    // Settle delay so post-init async allocations have landed before the
    // baseline marker is captured.
    Timer::after_secs(2).await;

    let baseline_free = esp_alloc::HEAP.free();
    let mut min_ever = baseline_free;
    log_ln!("HEAP_BASELINE,{}", baseline_free);

    loop {
        Timer::after_secs(1).await;
        let t_ms = Instant::now().as_millis();
        let free = esp_alloc::HEAP.free();
        if free < min_ever {
            min_ever = free;
        }
        let used_delta: i64 = baseline_free as i64 - free as i64;
        // esp-alloc 0.10 exposes no largest-contiguous-free API; emit 0 per spec.
        let largest_free: usize = 0;
        log_ln!(
            "HEAP,{},{},{},{},{}",
            t_ms,
            free,
            used_delta,
            min_ever,
            largest_free
        );
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting EspNow Central Node (Exper Heap)");

    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_static_tx_buf_num(25)
        .with_dynamic_tx_buf_num(128)
        .with_ampdu_tx_enable(false)
        .with_tx_queue_size(32);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::Node::Central(esp_csi_rs::CentralOpMode::EspNow(
            EspNowConfig::default().with_channel(11),
        )),
        CollectionMode::Listener,
        Some(CsiConfig::default()),
        Some(10000),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    node.set_rx_enabled(false);
    node.set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi);

    join(node.run(), node_task(&mut node_handle)).await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
