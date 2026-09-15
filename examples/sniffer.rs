//! **Sniffer** — promiscuous capture on a locked channel.
//!
//! | Attribute | Value |
//! |---|---|
//! | Operational mode | Wi-Fi sniffer |
//! | Network role | Peripheral — it never transmits, so it sources no traffic |
//! | Collection mode | Collector — a sniffer that does not report observes nothing |
//! | Session role | Responder — the run is started by whatever calls `run()` |
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
//! frame rate, the gap is what the collector missed.
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
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{CSINode, CSINodeClient, NodeHardware, WifiSnifferConfig, log_ln, set_csi_callback};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Channel to lock. Must match the emitter's primary channel when pairing with one.
const CHANNEL: u8 = 7;

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
}

/// Per-transmitter tallies, written from the CSI callback and drained by the reporting task.
static SOURCES: Mutex<
    CriticalSectionRawMutex,
    core::cell::RefCell<heapless::Vec<SourceTally, MAX_SOURCES>>,
> = Mutex::new(core::cell::RefCell::new(heapless::Vec::new()));

fn on_csi(packet: &CSIDataPacket) {
    // Two `i8` samples per subcarrier, so this is the width of the capture.
    SUBCARRIERS.store(
        (packet.csi_data_len / 2) as u32,
        core::sync::atomic::Ordering::Relaxed,
    );
    SOURCES.lock(|cell| {
        let mut list = cell.borrow_mut();
        if let Some(entry) = list.iter_mut().find(|e| e.mac == packet.mac) {
            entry.count += 1;
            entry.rssi = packet.rssi;
            return;
        }
        let _ = list.push(SourceTally {
            mac: packet.mac,
            count: 1,
            rssi: packet.rssi,
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
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  {} CSI/s  total {}  RSSI {}  subcarriers {}",
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

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    log_ln!("Starting sniffer (peripheral collector) on channel {}", CHANNEL);

    let mut node_handle = CSINodeClient::new();
    let hardware = NodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::sniffer(
        WifiSnifferConfig::default().with_channel(CHANNEL),
        Some(CsiConfig::default()),
        hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);

    set_csi_callback(on_csi);
    let _ = &mut node_handle;
    join(node.run(), report_task()).await;

    loop {
        log_ln!("Sniffer stopped");
        Timer::after(Duration::from_secs(5)).await;
    }
}
