//! ESP-NOW peripheral (Listener) — **HT40 CSI**, MCS7-LGI, 802.11n, multi-chip.
//!
//! Companion to `esp_now_central_ht40`. Runs an ESP-NOW peripheral in
//! **Listener** mode with both RX and TX enabled: it listens for the central's
//! broadcast control frames, **learns the central's MAC automatically**
//! (magic-prefix auto-pairing — no hardcoded MACs), then replies by **unicast**
//! to that learned peer with the forced MCS7 / HT40 PHY applied. The central
//! collects the wide CSI from those unicast replies.
//!
//! Why unicast: an ESP-NOW frame's bandwidth/rate is a *per-peer* PHY property
//! (`esp_now_set_peer_rate_config`), and a per-peer HT40 rate only applies to a
//! *unicast* peer — never the broadcast peer (on C5, forcing the broadcast peer
//! also wedges the dual-band Wi-Fi ISR). In HT40 mode the responder unicasts its
//! replies to the learned central and applies the forced PHY to that peer (see
//! `unicast_replies` in `peripheral/esp_now.rs`).
//!
//! Band/channel are chosen at build time from the chip feature and MUST match
//! the central:
//!
//! - **ESP32-C5** (dual-band): channel **149** (5 GHz) + secondary `Above`
//!   → the 149 + 153 HT40 pair.
//! - **ESP32 / ESP32-S3 / ESP32-C3 / ESP32-C6** (2.4 GHz): channel **6** +
//!   secondary `Above` → the 6 + 10 HT40 pair.
//!
//! 2.4 GHz HT40 can be finicky on some chips. **Verify on the central** that
//! HT40 engaged: subcarrier count (`csi_data_len / 2`) ≥ 100 (commonly ~117–128)
//! confirms HT40; ~53/~56 means it fell back to legacy/HT20. If 2.4 GHz HT40
//! won't engage, try a different pair (ch 1 Above, ch 11 Below) or fall back to
//! HT20.
//!
//! Build / run (statistics feature required for the throughput counters — use
//! the `-defmt` aliases, which include it, or add `,statistics`):
//!   cargo esp32c5-defmt --example esp_now_peripheral_ht40   # ch 149 (5 GHz)
//!   cargo esp32c6-defmt --example esp_now_peripheral_ht40   # ch 6   (2.4 GHz)
//!   cargo esp32s3-defmt --example esp_now_peripheral_ht40   # ch 6   (2.4 GHz)
//!   cargo esp32-defmt   --example esp_now_peripheral_ht40   # ch 6   (2.4 GHz)

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
#[cfg(feature = "statistics")]
use embassy_time::Instant;
use embassy_time::{Duration, Timer, with_timeout};
use esp_csi_rs::csi::CSIDataPacket;
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{
    CSINode, CollectionMode, EspNowConfig, IOTaskConfig, config::CsiConfig,
    logging::logging::init_logger,
};
use esp_csi_rs::{CSINodeClient, CSINodeHardware, log_ln, set_csi_callback};
#[cfg(feature = "statistics")]
use esp_csi_rs::{
    get_dropped_packets_rx, get_pps_rx, get_pps_tx, get_total_rx_packets, get_total_tx_packets,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::{SecondaryChannel, WifiController};
use portable_atomic::{AtomicI32, AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

const PHY_RATE: WifiPhyRate = WifiPhyRate::RateMcs7Lgi;

// --- Per-chip channel / HT40 secondary selection (must match the central) ----
// C5 (dual-band) runs 5 GHz; every other supported chip is 2.4 GHz only.
#[cfg(feature = "esp32c5")]
const CHANNEL: u8 = 149;
#[cfg(feature = "esp32c5")]
const SECONDARY: SecondaryChannel = SecondaryChannel::Above; // 149 + 153
#[cfg(feature = "esp32c5")]
const BANDWIDTH: &str = "HT40 (5 GHz)";

#[cfg(all(
    not(feature = "esp32c5"),
    any(
        feature = "esp32",
        feature = "esp32s3",
        feature = "esp32c3",
        feature = "esp32c6"
    )
))]
const CHANNEL: u8 = 6;
#[cfg(all(
    not(feature = "esp32c5"),
    any(
        feature = "esp32",
        feature = "esp32s3",
        feature = "esp32c3",
        feature = "esp32c6"
    )
))]
const SECONDARY: SecondaryChannel = SecondaryChannel::Above; // 6 + 10
#[cfg(all(
    not(feature = "esp32c5"),
    any(
        feature = "esp32",
        feature = "esp32s3",
        feature = "esp32c3",
        feature = "esp32c6"
    )
))]
const BANDWIDTH: &str = "HT40 (2.4 GHz)";

// IDE fallback when rust-analyzer runs without a chip feature.
#[cfg(all(
    not(any(
        feature = "esp32",
        feature = "esp32s3",
        feature = "esp32c3",
        feature = "esp32c6",
        feature = "esp32c5"
    )),
    rust_analyzer
))]
const CHANNEL: u8 = 6;
#[cfg(all(
    not(any(
        feature = "esp32",
        feature = "esp32s3",
        feature = "esp32c3",
        feature = "esp32c6",
        feature = "esp32c5"
    )),
    rust_analyzer
))]
const SECONDARY: SecondaryChannel = SecondaryChannel::Above;
#[cfg(all(
    not(any(
        feature = "esp32",
        feature = "esp32s3",
        feature = "esp32c3",
        feature = "esp32c6",
        feature = "esp32c5"
    )),
    rust_analyzer
))]
const BANDWIDTH: &str = "HT40";

#[cfg(all(
    not(any(
        feature = "esp32",
        feature = "esp32s3",
        feature = "esp32c3",
        feature = "esp32c6",
        feature = "esp32c5"
    )),
    not(rust_analyzer)
))]
compile_error!(
    "Build with exactly one chip feature: esp32c5 (5 GHz HT40) or esp32 / esp32s3 / esp32c3 / esp32c6 (2.4 GHz HT40)."
);

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

// This creates a default app-descriptor required by the esp-idf bootloader.
esp_bootloader_esp_idf::esp_app_desc!();

// Shared state written by the inline CSI callback and read by `stats_task`.
// On the peripheral, CSI is incidental (it's collected on the central); these
// just confirm the link is live.
static LATEST_RSSI: AtomicI32 = AtomicI32::new(0);
static CSI_PKT_COUNT: AtomicU32 = AtomicU32::new(0);

// On-device CSI hook. Runs inline in the WiFi callback — keep it fast: no heap
// allocation, no locking, no blocking I/O.
fn on_csi(packet: &CSIDataPacket) {
    LATEST_RSSI.store(packet.rssi as i32, Ordering::Relaxed);
    CSI_PKT_COUNT.fetch_add(1, Ordering::Relaxed);
}

async fn stats_task(client: &mut CSINodeClient) {
    #[cfg(feature = "statistics")]
    let mut last_sample = Instant::now();
    #[cfg(feature = "statistics")]
    let mut last_rx_total = get_total_rx_packets();
    #[cfg(feature = "statistics")]
    let mut last_tx_total = get_total_tx_packets();

    with_timeout(Duration::from_secs(1000), async {
        loop {
            Timer::after_secs(1).await;

            #[cfg(feature = "statistics")]
            {
                let elapsed_us = last_sample.elapsed().as_micros() as u64;
                let rx_total = get_total_rx_packets();
                let tx_total = get_total_tx_packets();
                let rx_rate_hz = if elapsed_us == 0 {
                    0
                } else {
                    (rx_total.saturating_sub(last_rx_total) * 1_000_000 / elapsed_us) as u32
                };
                let tx_rate_hz = if elapsed_us == 0 {
                    0
                } else {
                    (tx_total.saturating_sub(last_tx_total) * 1_000_000 / elapsed_us) as u32
                };

                last_sample = Instant::now();
                last_rx_total = rx_total;
                last_tx_total = tx_total;

                log_ln!(
                    "RX PPS(avg): {}, TX PPS(avg): {}, RX Hz(inst): {}, TX Hz(inst): {}, RX Total: {}, TX Total: {}, RX Dropped Packets: {}, CSI Packets: {}, Latest RSSI: {}",
                    get_pps_rx(),
                    get_pps_tx(),
                    rx_rate_hz,
                    tx_rate_hz,
                    rx_total,
                    tx_total,
                    get_dropped_packets_rx(),
                    CSI_PKT_COUNT.load(Ordering::Relaxed),
                    LATEST_RSSI.load(Ordering::Relaxed),
                );
            }

            #[cfg(not(feature = "statistics"))]
            {
                log_ln!(
                    "CSI Packets: {}, Latest RSSI: {}",
                    CSI_PKT_COUNT.load(Ordering::Relaxed),
                    LATEST_RSSI.load(Ordering::Relaxed),
                );
            }
        }
    })
    .await
    .unwrap_err();
    client.send_stop().await;
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

    log_ln!("Embassy initialized!");
    log_ln!(
        "Starting EspNow Peripheral (Listener) — channel {}, {}, MCS7-LGI, 802.11n, auto-pairing unicast replies",
        CHANNEL,
        BANDWIDTH
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default().with_initial_config(
        esp_radio::wifi::Config::AccessPoint(esp_radio::wifi::ap::AccessPointConfig::default()),
    );
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    esp_csi_rs::install_static_espnow_recv();

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    // Auto-pairing (no `with_peer_mac`): the peripheral learns the central's MAC
    // from the first broadcast control frame, then unicasts forced-PHY HT40
    // replies back to it. `with_ht40` implies `force_phy`, so the unicast replies
    // go out at MCS7 / HT40 — which is what makes the central capture wide CSI.
    let espnow_cfg = EspNowConfig::default()
        .with_channel(CHANNEL)
        .with_phy_rate(PHY_RATE)
        .with_ht40(SECONDARY);

    let mut node = CSINode::new(
        esp_csi_rs::Node::Peripheral(esp_csi_rs::PeripheralOpMode::EspNow(espnow_cfg)),
        CollectionMode::Listener,
        Some(CsiConfig::default()),
        Some(1000),
        csi_hardware,
    );
    // 802.11n (HT) — required for the MCS7 / HT40 PHY forced below.
    node.set_protocol(esp_radio::wifi::Protocol::N);
    node.set_rate(PHY_RATE);
    // RX learns the central + receives; TX sends the unicast replies. Both
    // required for the listen → learn → unicast flow.
    node.set_io_tasks(IOTaskConfig::new(true, true));

    set_csi_callback(on_csi);
    join(node.run(), stats_task(&mut node_handle)).await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
