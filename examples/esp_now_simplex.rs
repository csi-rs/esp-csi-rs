//! **ESP-NOW simplex** — the asymmetric exchange, and the highest CSI rate the crate offers.
//!
//! | Attribute | `END = Source` | `END = Peer` |
//! |---|---|---|
//! | Operational mode | ESP-NOW simplex | ESP-NOW simplex |
//! | Network role | **Central** — it owns all transmit airtime | **Peripheral** — it sources nothing |
//! | Collection mode | **Listener** — it captures nothing | **Collector** |
//! | Session role | Responder | Responder |
//!
//! Neither attribute is settable: the asymmetry fixes both. The source floods, the peer measures.
//! Leaving the channel to a single transmitter is exactly why this sustains a markedly higher
//! sample rate than the symmetric `esp_now` pairing.
//!
//! How the link forms: the **peer** broadcasts a sparse (~1 Hz) discovery beacon until it hears a
//! source; the source learns its MAC, registers it as a unicast peer with a forced PHY, sends one
//! hello so the peer stops beaconing, and then unicasts continuously. After that the peer is purely
//! receive-only.
//!
//! ## The roles changed sides in 0.11
//!
//! This pairing used to be spelled the other way around — the flooding end was a
//! `PeripheralOpMode::EspNowFastSource` and the receive-only end a
//! `CentralOpMode::EspNowFastCollector` — so the only node transmitting was the one called
//! "peripheral". The network role follows the traffic in every other mode, and now it does here
//! too. Nothing about what either node does on air changed; only the names.
//!
//! Build / run (both ends on the same channel, `END` flipped between them):
//!   cargo esp32c6 --example esp_now_simplex

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeClient, EspNowConfig, NodeHardware, config::CsiConfig,
    install_static_espnow_recv, log_ln, set_csi_callback,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::WifiController;
use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

// ── Knobs ───────────────────────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Eq)]
enum End {
    /// Floods. Central listener.
    Source,
    /// Measures. Peripheral collector.
    Peer,
}

/// Flash the same binary to both boards, changing only this.
const END: End = End::Source;

/// Both ends must agree.
const CHANNEL: u8 = 6;

/// Forced TX PHY for the flood. Source only — the peer has no rate to force.
const PHY_RATE: WifiPhyRate = WifiPhyRate::RateMcs7Lgi;

// ────────────────────────────────────────────────────────────────────────────────────────────────

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
static CSI_PKT_COUNT: AtomicU32 = AtomicU32::new(0);

fn on_csi(packet: &CSIDataPacket) {
    LATEST_RSSI.store(packet.rssi as i32, Ordering::Relaxed);
    CSI_PKT_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[embassy_executor::task]
async fn stats_task() {
    let mut previous = 0u32;
    loop {
        Timer::after_secs(1).await;
        let total = CSI_PKT_COUNT.load(Ordering::Relaxed);
        log_ln!(
            "CSI {}/s, total {}, RSSI {}",
            total.wrapping_sub(previous),
            total,
            LATEST_RSSI.load(Ordering::Relaxed),
        );
        previous = total;
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

    install_static_espnow_recv();
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = NodeHardware::new(&mut interfaces, controller);

    let mut node = match END {
        End::Source => {
            log_ln!("Starting simplex source (central listener) on channel {}", CHANNEL);
            CSINode::esp_now_simplex_source(
                EspNowConfig::default()
                    .with_channel(CHANNEL)
                    .with_phy_rate(PHY_RATE),
                None,
                csi_hardware,
            )
        }
        End::Peer => {
            log_ln!("Starting simplex peer (peripheral collector) on channel {}", CHANNEL);
            CSINode::esp_now_simplex_peer(CHANNEL, Some(CsiConfig::default()), csi_hardware)
        }
    };
    node.set_protocol(esp_radio::wifi::Protocol::N);

    if END == End::Peer {
        set_csi_callback(on_csi);
        spawner.spawn(stats_task().unwrap());
    }
    let _ = &mut node_handle;
    node.run().await;

    loop {
        log_ln!("Simplex node stopped");
        Timer::after(Duration::from_secs(5)).await;
    }
}
