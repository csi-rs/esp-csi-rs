//! Heap-usage test — Wi-Fi active STA role.
//!
//! Active-STA counterpart to the ESP32-CSI-Tool `active_sta_heap` for the
//! heap-usage benchmark. Associates to an AP, collects CSI from the link with a
//! **silent** path (CSI publish gate closed — no per-frame `CSIDataPacket`
//! build, no logging, matching the C references' count-only CSI callback), then
//! samples the heap once per second per the shared heap-test standard
//! (`docs/heap_usage_test_spec.md`).
//!
//! Records: `HEAP_BASELINE,<bytes>` then
//! `HEAP,<t_ms>,<free>,<used_delta>,<min_ever>,<largest_free>`.
//! The comparable cross-stack metric is `used_delta` (working set / leak);
//! absolute `free` is layout-dependent (Rust static BSS vs C DRAM pool) and is
//! not cross-compared. `largest_free` is 0 (esp-alloc 0.10 has no API).
//!
//! Build: `cargo build --release --example wifi_station_heap --features <chip>,async-print`.

#![no_std]
#![no_main]

use crate::alloc::string::ToString;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Instant, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{CSINode, CollectionMode, config::CsiConfig, logging::logging::init_logger};
use esp_csi_rs::{
    CSINodeClient, CSINodeHardware, WifiStationConfig, log_ln, set_csi_logging_enabled,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use esp_radio::wifi::sta::StationConfig;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// Shared heap reporter (identical across all esp-csi-rs `*_heap` examples).
/// STA may need a few seconds to associate before the heap settles, so the
/// baseline is captured after a 5 s settle (spec: settle until steady idle).
async fn heap_reporter() -> ! {
    Timer::after_secs(5).await;

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
    // Silent CSI for the heap test (see module docs).
    set_csi_logging_enabled(false);

    // Moderate reclaimed heap so internal RAM stays available for the STA
    // network stack (smoltcp + Wi-Fi task stacks) during scan/connect.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting Wi-Fi Station Node (Heap test)");

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    // Set your AP credentials here (the STA associates to capture CSI).
    let client_config = StationConfig::default()
        .with_ssid("Connected Motion ")
        .with_password("automotion@123".to_string())
        .with_auth_method(esp_radio::wifi::AuthenticationMethod::Wpa2Personal);
    let station_config = WifiStationConfig { client_config };

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::Node::Central(esp_csi_rs::CentralOpMode::WifiStation(station_config)),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(1000),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    // No set_csi_callback → CSI stays silent (publish gate closed above). Run
    // indefinitely (no send_stop) so the reporter samples ≥300 s.
    let _ = &mut node_handle;

    join(node.run(), heap_reporter()).await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
