#![no_std]
#![no_main]

//! Wi-Fi sniffer that cycles through every `LogMode` once per minute.
//!
//! On boot the node starts sniffing in `LogMode::Text`. Every minute it
//! advances to the next logging format, emitting a machine-readable marker
//! line just after the switch so the active mode is recorded in the capture.
//! Once all modes have been exercised it disables CSI logging, emits a
//! "test finished" marker, and idles forever (no further output).
//!
//! ## Parsing the output
//!
//! Marker lines are prefixed with a fixed sentinel so a Python script can
//! pick them out of the mixed CSI stream with a simple substring/regex match:
//!
//! ```text
//! CSI_TEST_EVENT mode_switch index=0 mode=Text elapsed_s=0
//! CSI_TEST_EVENT mode_switch index=1 mode=Serialized elapsed_s=60
//! CSI_TEST_EVENT mode_switch index=2 mode=ArrayList elapsed_s=120
//! CSI_TEST_EVENT mode_switch index=3 mode=EspCsiTool elapsed_s=180
//! CSI_TEST_EVENT test_finished modes_tested=4 elapsed_s=240
//! ```
//!
//! Suggested Python parsing:
//!
//! ```python
//! import re
//! SWITCH = re.compile(r"CSI_TEST_EVENT mode_switch index=(\d+) mode=(\w+) elapsed_s=(\d+)")
//! DONE   = re.compile(r"CSI_TEST_EVENT test_finished modes_tested=(\d+) elapsed_s=(\d+)")
//! for line in serial_lines:
//!     if (m := SWITCH.search(line)):
//!         idx, mode, elapsed = int(m[1]), m[2], int(m[3])
//!     elif (m := DONE.search(line)):
//!         break  # test complete
//! ```

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger, set_log_mode};
use esp_csi_rs::{CSINode, CollectionMode, config::CsiConfig};
use esp_csi_rs::{
    CSINodeClient, CSINodeHardware, WifiSnifferConfig, log_ln, set_csi_logging_enabled,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

/// Sentinel prefix on every test-event line, so a parser can locate them in
/// the mixed CSI/marker stream with a plain substring match.
const EVENT_TAG: &str = "CSI_TEST_EVENT";

/// How long to dwell in each logging mode before switching to the next.
const MODE_DWELL_SECS: u64 = 60;

/// Every `LogMode` variant, in the order the test walks through them.
/// Pairs the enum value with a stable string label used in the marker lines.
const MODES: [(LogMode, &str); 4] = [
    (LogMode::Text, "Text"),
    (LogMode::Serialized, "Serialized"),
    (LogMode::ArrayList, "ArrayList"),
    (LogMode::EspCsiTool, "EspCsiTool"),
];

/// Drives the `LogMode` cycle concurrently with the running sniffer node.
///
/// `node.run()` never returns on its own (the sniffer path loops in
/// `run_process_csi_packet` until a stop signal), so this driver must run
/// alongside it under `join` — otherwise the loop below is unreachable and
/// the logger stays stuck in `MODES[0]` (`Text`) forever.
///
/// Walks every mode dwelling `MODE_DWELL_SECS` in each, emits markers, then
/// disables CSI logging and stops the node so the `join` in `main` completes.
async fn mode_cycle_driver(node_handle: &CSINodeClient) {
    // The marker for index 0 is emitted immediately (elapsed_s=0) since the
    // logger already booted in MODES[0]; each subsequent iteration first
    // waits, then switches, then emits its marker.
    let mut elapsed_s: u64 = 0;
    for (index, (mode, label)) in MODES.iter().enumerate() {
        if index > 0 {
            Timer::after(Duration::from_secs(MODE_DWELL_SECS)).await;
            elapsed_s += MODE_DWELL_SECS;
            set_log_mode(*mode);
        }
        // Machine-readable marker; printed on its own line via the text log
        // channel so it never merges with a CSI record.
        log_ln!(
            "{} mode_switch index={} mode={} elapsed_s={}",
            EVENT_TAG,
            index,
            label,
            elapsed_s
        );
    }

    // All modes exercised: dwell in the final mode, then stop emitting CSI and
    // announce completion.
    Timer::after(Duration::from_secs(MODE_DWELL_SECS)).await;
    elapsed_s += MODE_DWELL_SECS;
    set_csi_logging_enabled(false);

    log_ln!(
        "{} test_finished modes_tested={} elapsed_s={}",
        EVENT_TAG,
        MODES.len(),
        elapsed_s
    );

    // Signal the node to stop so `node.run()` returns and the `join` in main
    // completes, letting us fall through to the idle loop.
    node_handle.send_stop().await;
}

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

    // Start in the first mode of the cycle so the initial capture matches the
    // first marker we emit below.
    init_logger(spawner, MODES[0].0);
    set_csi_logging_enabled(true);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting Wi-Fi Sniffer LogMode-cycle test");

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    // .with_static_rx_buf_num(25)
    // .with_dynamic_rx_buf_num(128)
    // .with_rx_queue_size(32);
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let node_handle = CSINodeClient::new();
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

    // Run the sniffer and the mode-cycle driver concurrently. `node.run()`
    // loops forever servicing CSI packets, so the driver must run alongside it
    // under `join`; the driver calls `send_stop()` when the cycle is done,
    // which lets `node.run()` return and the join complete.
    join(node.run(), mode_cycle_driver(&node_handle)).await;

    // Test complete — idle forever without producing any further output.
    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
}
