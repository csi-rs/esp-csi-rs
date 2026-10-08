//! **Sniffer** — promiscuous capture on a locked channel.
//!
//! | Attribute | Value |
//! |---|---|
//! | Operational mode | Wi-Fi sniffer |
//! | Network role | Peripheral — it never transmits, so it sources no traffic |
//! | Reporting policy | `REPORTING` below — `Always`, `Threshold` or `Decimate`; never `Never`, since a sniffer that does not report observes nothing |
//! | Session role | none — the run's controller is whatever calls `run()` |
//!
//! Neither of the first two is settable, so `CSINode::sniffer` takes no role arguments. See
//! `docs/network-model.md`.
//!
//! This is the only mode that needs no second device: the traffic it measures is whatever is
//! already on air. Point it at a channel with an `emitter` on it and it becomes the receiving half
//! of a controlled pairing instead — nothing about the node changes, only what is transmitting.
//!
//! Frames are attributed by transmitter MAC, so several emitters can share one sniffer and the
//! per-source rate below separates them. If a source's rate is lower than the emitter's configured
//! frame rate, the gap is what the collector missed. Each line also names the PPDU format and the
//! subcarrier layout the buffer was captured in (`CsiPayload::EspRaw`'s `LayoutId`).
//!
//! `REPORTING` decides how many measurements leave the node: every one, every *n*th, or only while
//! the channel is moving. On the ESP32-C5 / C6, `HE20` switches to HE-LTF capture for an HE20
//! emitter (`EmitterPhy::He20`): ~242 subcarriers instead of the 53 of the legacy field.
//!
//! Build / run:
//!   cargo esp32c6 --example sniffer
//!
//! Replace `esp32c6` with any supported chip — every chip can collect.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Timer};
use esp_csi_rs::config::CsiConfig;
use esp_csi_rs::csi::CsiPacket;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::wire::{CsiPayload, PpduFormat};
use esp_csi_rs::{
    CSINode, CSINodeClient, NodeHardware, ReportingPolicy, Threshold, WifiSnifferConfig, log_ln,
    set_csi_callback,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Channel to lock. Must match the emitter's primary channel when pairing with one.
const CHANNEL: u8 = 7;

/// `Always`, `Threshold(..)` or `Decimate(n)`. A threshold must be calibrated: run with
/// `Threshold::new(level, hold_ms).variation_only()` first and read the scores a still room gives.
const REPORTING: ReportingPolicy = ReportingPolicy::Always;
#[allow(dead_code)]
const EXAMPLE_THRESHOLD: Threshold = Threshold::new(6000, 500);

/// HE-LTF capture (ESP32-C5 / C6 only).
const HE20: bool = false;

/// How many distinct transmitters to track.
const MAX_SOURCES: usize = 4;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[derive(Clone, Copy)]
struct SourceTally {
    mac: [u8; 6],
    count: u32,
    rssi: i32,
    ppdu: PpduFormat,
    /// Subcarriers the buffer's layout maps; 0 when the layout is unknown.
    mapped: usize,
}

fn ppdu_name(p: PpduFormat) -> &'static str {
    match p {
        PpduFormat::Dsss => "DSSS",
        PpduFormat::NonHt => "non-HT",
        PpduFormat::Ht => "HT",
        PpduFormat::Vht | PpduFormat::VhtMu => "VHT",
        PpduFormat::HeSu => "HE SU",
        PpduFormat::HeMu | PpduFormat::HeErSu | PpduFormat::HeTb => "HE",
        _ => "?",
    }
}

/// Per-transmitter tallies, written from the CSI callback and drained by the reporting task.
static SOURCES: Mutex<
    CriticalSectionRawMutex,
    core::cell::RefCell<heapless::Vec<SourceTally, MAX_SOURCES>>,
> = Mutex::new(core::cell::RefCell::new(heapless::Vec::new()));

fn on_csi(packet: &CsiPacket) {
    // Two `i8` samples per subcarrier, so this is the width of the capture.
    SUBCARRIERS.store(
        packet.subcarriers() as u32,
        core::sync::atomic::Ordering::Relaxed,
    );
    let ppdu = packet.meta().ppdu;
    let mapped = match &packet.frame.payload {
        CsiPayload::EspRaw { layout, .. } => layout.indices().count(),
        _ => 0,
    };
    SOURCES.lock(|cell| {
        let mut list = cell.borrow_mut();
        if let Some(entry) = list.iter_mut().find(|e| e.mac == packet.mac()) {
            entry.count += 1;
            entry.rssi = packet.rssi() as i32;
            entry.ppdu = ppdu;
            entry.mapped = mapped;
            return;
        }
        let _ = list.push(SourceTally {
            mac: packet.mac(),
            count: 1,
            rssi: packet.rssi() as i32,
            ppdu,
            mapped,
        });
    });
}

async fn report_task() {
    let mut previous: heapless::Vec<([u8; 6], u32), MAX_SOURCES> = heapless::Vec::new();
    loop {
        Timer::after_secs(1).await;
        let snapshot = SOURCES.lock(|cell| cell.borrow().clone());
        if snapshot.is_empty() {
            log_ln!("No CSI yet — is anything transmitting on channel {}?", CHANNEL);
            continue;
        }
        for entry in snapshot.iter() {
            let last = previous
                .iter()
                .find(|(mac, _)| *mac == entry.mac)
                .map(|(_, c)| *c)
                .unwrap_or(0);
            log_ln!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  {} CSI/s  total {}  RSSI {}  subcarriers {}  {}  layout maps {}",
                entry.mac[0],
                entry.mac[1],
                entry.mac[2],
                entry.mac[3],
                entry.mac[4],
                entry.mac[5],
                entry.count.wrapping_sub(last),
                entry.count,
                entry.rssi,
                SUBCARRIERS.load(core::sync::atomic::Ordering::Relaxed),
                ppdu_name(entry.ppdu),
                entry.mapped,
            );
        }
        previous.clear();
        for entry in snapshot.iter() {
            let _ = previous.push((entry.mac, entry.count));
        }
    }
}

/// Last capture's subcarrier count. `>= 100` (commonly ~117) confirms HT40 actually engaged;
/// ~53 or ~56 means it fell back to legacy or HT20. See `docs/bandwidth.md`.
static SUBCARRIERS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// HE-LTF acquisition exists only on the 802.11ax parts.
fn csi_config() -> CsiConfig {
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    if HE20 {
        return CsiConfig::he20();
    }
    CsiConfig::default()
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let wifi_controller = esp_radio::wifi::WifiController::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    log_ln!("Starting sniffer (peripheral collector) on channel {}", CHANNEL);

    let mut node_handle = CSINodeClient::new();
    let hardware = NodeHardware::new(controller);
    let mut sniffer = WifiSnifferConfig::default().with_channel(CHANNEL);
    match REPORTING {
        ReportingPolicy::Threshold(t) => sniffer = sniffer.with_threshold(t),
        ReportingPolicy::Decimate(n) => sniffer = sniffer.with_decimation(n),
        _ => {}
    }
    let mut node = CSINode::sniffer(sniffer, Some(csi_config()), hardware);
    node.set_protocol(if HE20 {
        esp_radio::wifi::Protocol::AX
    } else {
        esp_radio::wifi::Protocol::N
    });

    set_csi_callback(on_csi);
    let _ = &mut node_handle;
    join(node.run(), report_task()).await;

    loop {
        log_ln!("Sniffer stopped");
        Timer::after(Duration::from_secs(5)).await;
    }
}
