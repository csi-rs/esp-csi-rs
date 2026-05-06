//! Dedicated test for `set_csi_callback`.
//!
//! Demonstrates on-device CSI processing via the inline callback hook.
//! Runs the node in WiFi-sniffer mode (no peer required — captures CSI for
//! every 802.11n frame the radio decodes on the configured channel) and
//! processes each packet inside a `fn(&CSIDataPacket)` invoked directly
//! from the WiFi-task callback. A periodic stats task aggregates results
//! and logs them to UART once per second.
//!
//! What the callback does (per packet):
//!   1. Extracts RSSI and stores it in an atomic.
//!   2. Counts callback invocations.
//!   3. Computes a tiny aggregate over the CSI tones (sum of absolute
//!      values across the I/Q pairs in `csi_data`) — purely to show that
//!      heavier inline math is allowed, as long as it's bounded and
//!      non-blocking.
//!
//! Constraints (read once, then forget):
//!   - The callback runs on the WiFi task hot path. **Keep it fast.**
//!   - No heap allocation, no `Mutex` locks, no UART writes from the
//!     callback itself. Use atomics to publish state to other tasks.
//!   - For heavier work (logging, ML inference), copy what you need out
//!     and post to a separate task via a queue or static.

#![no_std]
#![no_main]

use core::sync::atomic::Ordering;

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    config::CsiConfig, logging::logging::init_logger, CSINode, CollectionMode, WifiSnifferConfig,
};
use esp_csi_rs::{
    log_ln, set_csi_callback, set_csi_logging_enabled, CSINodeClient, CSINodeHardware,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::{
    wifi::{PowerSaveMode, WifiController},
    Controller,
};
use portable_atomic::{AtomicI32, AtomicU32, AtomicU64};
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

/// Atomics published from the inline CSI callback and read by `stats_task`.
/// Atomic-only access keeps the callback wait-free relative to the stats
/// task — no critical sections, no contention with the WiFi-task ISR path.
static CSI_PKT_COUNT: AtomicU32 = AtomicU32::new(0);
static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
/// Sum of `csi_data` magnitudes for the most recent packet — a tiny
/// example aggregate. Stored as `u64` so even very long CSI payloads
/// don't wrap.
static LATEST_TONE_ENERGY: AtomicU64 = AtomicU64::new(0);
static LATEST_TONE_COUNT: AtomicU32 = AtomicU32::new(0);

/// On-device CSI processing hook.
///
/// Runs inline in the WiFi task callback. Must be fast and non-blocking
/// — no heap allocation, no locking, no UART writes. Reads/writes only
/// to atomics or stack memory.
fn on_csi(packet: &CSIDataPacket) {
    LATEST_RSSI.store(packet.rssi as i32, Ordering::Relaxed);
    CSI_PKT_COUNT.fetch_add(1, Ordering::Relaxed);

    // Demonstrate bounded inline math: sum |I| + |Q| across all CSI tones.
    // CSI data is interleaved I/Q pairs of `i8`; absolute-value sum is a
    // crude amplitude proxy good enough to show "callback can do real
    // work" without dragging in `f32` or heap.
    let mut energy: u64 = 0;
    let tones = packet.csi_data.len();
    for sample in packet.csi_data.iter() {
        energy = energy.wrapping_add((*sample as i32).unsigned_abs() as u64);
    }
    LATEST_TONE_ENERGY.store(energy, Ordering::Relaxed);
    LATEST_TONE_COUNT.store(tones as u32, Ordering::Relaxed);
}

/// Reads the atomics published by `on_csi` and logs them once per second.
async fn stats_task() {
    let mut last_count = CSI_PKT_COUNT.load(Ordering::Relaxed);
    loop {
        Timer::after_secs(1).await;
        let count = CSI_PKT_COUNT.load(Ordering::Relaxed);
        let delta = count.wrapping_sub(last_count);
        last_count = count;
        log_ln!(
            "CSI/sec: {}, total: {}, last RSSI: {} dBm, last tones: {}, last energy: {}",
            delta,
            count,
            LATEST_RSSI.load(Ordering::Relaxed),
            LATEST_TONE_COUNT.load(Ordering::Relaxed),
            LATEST_TONE_ENERGY.load(Ordering::Relaxed),
        );
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);

    // Suppress the per-packet UART CSI dump that `init_logger` enables —
    // we only want our `log_ln!` output plus what the inline callback
    // does. `set_csi_callback` will re-enable the gate so the callback
    // still fires.
    set_csi_logging_enabled(false);

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
    log_ln!("Starting CSI callback test (sniffer mode)");

    let radio_init = mk_static!(
        Controller<'static>,
        esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller")
    );

    let config_radio =
        esp_radio::wifi::Config::default().with_power_save_mode(PowerSaveMode::None);
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

    // Register the inline CSI processing hook. `set_csi_callback` also
    // implicitly calls `set_csi_logging_enabled(true)` so the callback
    // fires without a separate enable step.
    set_csi_callback(on_csi);

    let _ = &mut node_handle;
    join(node.run(), stats_task()).await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
