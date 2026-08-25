//! Sniffer collector — the RX half of an emitter/collector pair.
//!
//! Locks a channel in promiscuous mode and measures the CSI of every frame it
//! overhears, including the raw sounding frames from an `ht20_emitter` or
//! `ht40_emitter` on the same channel. No association or handshake is involved:
//! the emitter transmits blindly and this node measures what arrives.
//!
//! Each frame carries its transmitter's MAC, so one collector can serve several
//! emitters and attribute measurements by source. This example reports the CSI
//! rate per source MAC once a second, which is the quickest way to confirm on
//! hardware that an emitter is actually being heard.

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
use esp_csi_rs::{
    CSINode, CSINodeClient, CollectorMode, NodeHardware, WifiSnifferConfig, log_ln,
    set_csi_callback,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Must match the emitter's primary channel.
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

/// Per-transmitter tallies, written from the CSI callback and drained by the
/// reporting task.
static SOURCES: Mutex<CriticalSectionRawMutex, core::cell::RefCell<heapless::Vec<SourceTally, MAX_SOURCES>>> =
    Mutex::new(core::cell::RefCell::new(heapless::Vec::new()));

fn on_csi(packet: &CSIDataPacket) {
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
            log_ln!("No CSI yet — is an emitter running on channel {}?", CHANNEL);
            continue;
        }
        for entry in snapshot.iter() {
            let last = previous
                .iter()
                .find(|(mac, _)| *mac == entry.mac)
                .map(|(_, c)| *c)
                .unwrap_or(0);
            log_ln!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  {} CSI/s  total {}  RSSI {}",
                entry.mac[0],
                entry.mac[1],
                entry.mac[2],
                entry.mac[3],
                entry.mac[4],
                entry.mac[5],
                entry.count.wrapping_sub(last),
                entry.count,
                entry.rssi,
            );
        }
        previous.clear();
        for entry in snapshot.iter() {
            let _ = previous.push((entry.mac, entry.count));
        }
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

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    log_ln!("Starting sniffer collector on channel {}", CHANNEL);

    let mut node_handle = CSINodeClient::new();
    let hardware = NodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new_collector(
        CollectorMode::Sniffer(WifiSnifferConfig::default().with_channel(CHANNEL)),
        Some(CsiConfig::default()),
        None,
        hardware,
    );

    node.set_protocol(esp_radio::wifi::Protocol::N);

    set_csi_callback(on_csi);
    let _ = &mut node_handle;
    join(node.run(), report_task()).await;

    loop {
        log_ln!("Collector stopped");
        Timer::after(Duration::from_secs(5)).await;
    }
}
