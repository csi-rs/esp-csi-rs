#![no_std]
#![no_main]

//! Wi-Fi sniffer that emits CSI in the ESP32-CSI-Tool CSV format.
//!
//! A copy of `sniffer_wifi_exper.rs` that swaps the human-readable `Text`
//! log mode for `LogMode::EspCsiTool`, producing output byte-compatible with
//! Steven M. Hernández's ESP32-CSI-Tool `passive` project:
//!
//! - A 26-column header is printed once at startup
//!   (`type,role,mac,rssi,...,len,CSI_DATA`).
//! - Every received frame becomes one `CSI_DATA,...` CSV line ending in a
//!   bracketed `[v0 v1 ... ]` array of raw `i8` CSI samples.
//!
//! Two settings tailor the output to the `passive` sub-project:
//!
//! - [`set_role`]`(Role::Passive)` writes `PASSIVE` in column 2 (`role`),
//!   matching the role the C++ `passive` build passes to `csi_init()`.
//! - [`set_csi_tool_emit_cap`]`(128)` mirrors `CONFIG_SHOULD_COLLECT_ONLY_LLTF`:
//!   the LLTF-only CSI scope below produces `len = 128` (64 subcarriers × 2),
//!   so column 25 reports 128 and column 26 holds exactly 128 samples,
//!   keeping every line uniform — see the ESP32-CSI-Tool packet-format notes
//!   in §1.3.
//!
//! The capture is parseable by `python_utils/parse_csi.py` from the upstream
//! ESP32-CSI-Tool repository.

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{
    init_logger, set_csi_tool_emit_cap, set_role, LogMode, Role,
};
use esp_csi_rs::{config::CsiConfig, CSINode, CollectionMode};
use esp_csi_rs::{
    log_ln, set_csi_logging_enabled, CSINodeClient, CSINodeHardware, WifiSnifferConfig,
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

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.1.0

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Emit the ESP32-CSI-Tool CSV format. The role is `PASSIVE` (this is a
    // promiscuous sniffer) and the per-line CSI array is capped to 128 `i8`
    // samples to match the LLTF-only scope configured below.
    init_logger(spawner, LogMode::EspCsiTool);
    set_role(Role::Passive);
    set_csi_tool_emit_cap(128);
    set_csi_logging_enabled(true);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting Wi-Fi Sniffer Peripheral Node (EspCsiTool format)");

    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_static_rx_buf_num(25)
        .with_dynamic_rx_buf_num(128)
        .with_rx_queue_size(32);
    let (wifi_controller, mut interfaces) =
        esp_radio::wifi::new(peripherals.WIFI, config_radio)
            .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    // Match C++ passive sniffer's CSI scope (SHOULD_COLLECT_ONLY_LLTF=y):
    // only legacy LTF, channel filter on, no HT/STBC LTF, no ACK dump.
    #[allow(unused_mut)]
    let mut csi_config = CsiConfig::default();
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    {
        csi_config.htltf_en = false;
        csi_config.stbc_htltf2_en = false;
        csi_config.ltf_merge_en = false;
        csi_config.channel_filter_en = true;
    }

    let mut node = CSINode::new(
        esp_csi_rs::Node::Peripheral(esp_csi_rs::PeripheralOpMode::WifiSniffer(
            WifiSnifferConfig::default().with_channel(1),
        )),
        CollectionMode::Collector,
        Some(csi_config),
        Some(10000),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    node.run().await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v~1.0/examples
}
