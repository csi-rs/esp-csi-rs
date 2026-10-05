//! **Access point** — self-contained softAP with DHCP; associated stations generate the uplink.
//!
//! | Attribute | Value |
//! |---|---|
//! | Operational mode | Wi-Fi access point |
//! | Network role | Central — beacons and DHCP make it a traffic source by construction |
//! | Reporting policy | `REPORTING` below — `Always`, `Never`, `Threshold` or `Decimate` |
//! | Session role | Responder |
//!
//! The network role is not settable, so there is no way to build a peripheral access point.
//!
//! No external router is needed: the AP hands out DHCP leases from a configurable pool and pings
//! associated clients, and the uplink ICMP replies become CSI here. Pair with `station` on the same
//! SSID. Expect tens to low hundreds of CSI/s depending on bidirectional contention and filter
//! settings — Wi-Fi airtime is the limit, not CPU.
//!
//! With `LEASE_POOL > 1` each client gets its own address and the AP round-robins its downlink
//! across the pool. `SYNC_BURST` changes that: every tick fires one unicast frame back-to-back to
//! all active leases, so every station receives its downlink within tens of microseconds of the
//! others — temporally synchronized multi-receiver CSI, rather than measurements smeared across the
//! tick interval. Group-addressed frames cannot do this job: a softAP buffers them until the next
//! DTIM beacon and drops them outright under a high-rate flood.
//!
//! Build / run:
//!   cargo esp32c6 --example access_point

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CsiPacket;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, ReportingPolicy, NodeHardware, WifiApConfig, config::CsiConfig, log_ln,
    set_csi_callback,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::{PowerSaveMode, WifiController};
use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

// ── Knobs ───────────────────────────────────────────────────────────────────────────────────────

const SSID: &str = "esp-csi-ap";

/// Primary channel. On the dual-band ESP32-C5, `>= 36` selects 5 GHz.
const CHANNEL: u8 = 6;

/// `Listener` keeps the flood on air without reporting — the AP half of a measurement whose data
/// comes from the stations.
const REPORTING: ReportingPolicy = ReportingPolicy::Always;

/// DHCP lease pool size. Each client gets a distinct address (MAC to IP binding).
const LEASE_POOL: u8 = 1;

/// Fire one unicast frame to *every* active lease per tick instead of one lease per tick.
const SYNC_BURST: bool = false;

/// ICMP ping rate (Hz) — each echo reply is uplink CSI.
const PING_RATE_HZ: u16 = 4000;

// ────────────────────────────────────────────────────────────────────────────────────────────────

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
static CSI_PKT_COUNT: AtomicU32 = AtomicU32::new(0);

fn on_csi(packet: &CsiPacket) {
    LATEST_RSSI.store(packet.rssi() as i32, Ordering::Relaxed);
    CSI_PKT_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[embassy_executor::task]
async fn stats_task() {
    loop {
        Timer::after_secs(1).await;
        log_ln!(
            "CSI Packets: {}, Latest RSSI: {}",
            CSI_PKT_COUNT.load(Ordering::Relaxed),
            LATEST_RSSI.load(Ordering::Relaxed),
        );
    }
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
    let _ = controller.set_power_saving(PowerSaveMode::None);

    let ap_radio_config = AccessPointConfig::default()
        .with_ssid(SSID.try_into().expect("SSID longer than 32 bytes"))
        .with_channel(CHANNEL);
    let ap_config = WifiApConfig::new(ap_radio_config, CHANNEL, None)
        .with_lease_pool(LEASE_POOL)
        .with_sync_burst(SYNC_BURST)
        .with_reporting(REPORTING);

    log_ln!(
        "Starting softAP (central) — SSID {}, channel {}, {} lease(s)",
        SSID,
        CHANNEL,
        LEASE_POOL
    );

    let csi_hardware = NodeHardware::new(controller);
    let mut node = CSINode::access_point(
        ap_config,
        Some(CsiConfig::default()),
        Some(PING_RATE_HZ),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);

    set_csi_callback(on_csi);
    spawner.spawn(stats_task().unwrap());
    node.run().await;

    loop {
        log_ln!("Access point stopped");
        Timer::after(Duration::from_secs(5)).await;
    }
}
