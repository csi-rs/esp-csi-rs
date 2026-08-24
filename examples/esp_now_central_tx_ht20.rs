//! ESP-NOW central — **TX only**, channel 6, **HT20**, MCS7-LGI, 802.11n.
//!
//! Companion to `esp_now_peripheral_rx_ht20`. This node only *transmits*: it
//! broadcasts ESP-NOW control frames on **channel 6** at a forced **HT20**
//! (20 MHz) 802.11n PHY (`RateMcs7Lgi`). It does **not** enable RX or CSI
//! capture — the paired peripheral is the receiver and emits the CSI. Pairing
//! is automatic (magic-prefix broadcast; no hardcoded MACs).
//!
//! How HT20 is forced: `with_phy_rate` (without `with_ht40`) forces the per-peer
//! TX PHY to MCS7 Long-GI at 20 MHz. On 2.4 GHz single-band chips the forced PHY
//! is applied to the broadcast peer, so every control frame goes out as HT20
//! 802.11n. Without this, ESP-NOW broadcasts fall back to a legacy (11b/g) rate.
//!
//! **Chip note:** on the dual-band **ESP32-C5**, forcing the PHY on the
//! *broadcast* peer is unsafe (it wedges the Wi-Fi ISR), so this TX-only/RX-only
//! broadcast pairing does not force HT20 there — use the unicast HT40 examples on
//! C5 instead. This example targets the 2.4 GHz chips (esp32 / c3 / c6 / s3).
//!
//! Build / run (pair with `esp_now_peripheral_rx_ht20` on the same channel):
//!   cargo esp32c6 --example esp_now_central_tx_ht20   # ch 6 (2.4 GHz)
//!   cargo esp32s3 --example esp_now_central_tx_ht20   # ch 6 (2.4 GHz)
//!   cargo esp32c3 --example esp_now_central_tx_ht20   # ch 6 (2.4 GHz)
//!   cargo esp32   --example esp_now_central_tx_ht20   # ch 6 (2.4 GHz)

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeHardware, EspNowConfig, IOTaskConfig, install_static_espnow_recv,
    log_ln,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

const CHANNEL: u8 = 6;
const PHY_RATE: WifiPhyRate = WifiPhyRate::RateMcs7Lgi;
// Broadcast control-frame rate (Hz).
const TX_RATE_HZ: u16 = 100;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

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
        "Starting EspNow Central — TX only, channel {}, HT20, MCS7-LGI, 802.11n, {} TX/s",
        CHANNEL,
        TX_RATE_HZ
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    // Replace esp-radio's heap recv queue before any peer traffic can arrive.
    install_static_espnow_recv();

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    // Channel 6, forced HT20 (no `with_ht40`) at MCS7 Long-GI. `with_phy_rate`
    // implies `force_phy`, so the broadcast frames go out as HT20 802.11n.
    let espnow_cfg = EspNowConfig::default()
        .with_channel(CHANNEL)
        .with_phy_rate(PHY_RATE);

    let mut node = CSINode::new(
        esp_csi_rs::NodeRole::Central(esp_csi_rs::CentralOpMode::EspNow(espnow_cfg)),
        // Listener: this node doesn't collect CSI; the peripheral does.
        None,
        Some(TX_RATE_HZ),
        csi_hardware,
    );
    // `CollectionMode::Listener` became this: keep the radio capturing, and its
    // timing intact, but deliver nothing off-device.
    node.set_csi_output_enabled(false);
    // 802.11n (HT) — required for the forced MCS7 / HT20 PHY.
    node.set_protocol(esp_radio::wifi::Protocol::N);
    node.set_rate(PHY_RATE);
    // TX only: broadcast control frames, no RX / no CSI capture.
    node.set_io_tasks(IOTaskConfig::new(true, false));

    node.run().await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
