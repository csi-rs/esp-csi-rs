//! ESP-NOW peripheral — **RX only**, channel 6, **HT20**, **serialized CSI out**.
//!
//! Companion to `esp_now_central_tx_ht20`. This node only *receives*: it listens
//! on **channel 6** for the central's HT20 802.11n broadcast control frames,
//! captures CSI from them, and emits each [`CSIDataPacket`] as **postcard-
//! serialized, COBS-framed binary** (`LogMode::Serialized`). It never transmits —
//! no replies, no presence beacons.
//!
//! The output is **not** human-readable: each CSI packet is one COBS frame
//! (zero-byte delimited). Decode it on the host with a `serde`/`postcard` reader,
//! e.g. read the serial stream, split on `0x00` boundaries, and `postcard`-
//! deserialize each frame back into a `CSIDataPacket`. A few human-readable
//! startup log lines precede the binary stream; a COBS reader resynchronizes on
//! the first frame boundary.
//!
//! Pairing is automatic (magic-prefix; no hardcoded MACs). No on-device CSI
//! callback and no periodic stats text are used here, so the serial stream stays
//! a clean sequence of serialized CSI frames once capture begins.
//!
//! Build / run (pair with `esp_now_central_tx_ht20` on the same channel):
//!   cargo esp32c6 --example esp_now_peripheral_rx_ht20   # ch 6 (2.4 GHz)
//!   cargo esp32s3 --example esp_now_peripheral_rx_ht20   # ch 6 (2.4 GHz)
//!   cargo esp32c3 --example esp_now_peripheral_rx_ht20   # ch 6 (2.4 GHz)
//!   cargo esp32   --example esp_now_peripheral_rx_ht20   # ch 6 (2.4 GHz)

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_csi_rs::config::CsiConfig;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{
    CSINode, CSINodeHardware, EspNowConfig, IOTaskConfig, install_static_espnow_recv,
    log_ln,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

const CHANNEL: u8 = 6;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    // Serialized: each captured CSI packet is emitted as a COBS-framed,
    // postcard-serialized binary frame instead of human-readable text.
    init_logger(spawner, LogMode::Serialized);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!(
        "Starting EspNow Peripheral — RX only, channel {}, HT20, serialized CSI output",
        CHANNEL
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    // Replace esp-radio's heap recv queue before any peer traffic can arrive.
    install_static_espnow_recv();

    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);

    // RX only: just listen on channel 6. No `with_phy_rate` — the peripheral
    // doesn't transmit, so there's no TX PHY to force; CSI bandwidth is whatever
    // the central sends (HT20 here).
    let espnow_cfg = EspNowConfig::default().with_channel(CHANNEL);

    let mut node = CSINode::new(
        esp_csi_rs::NodeRole::Peripheral(esp_csi_rs::PeripheralOpMode::EspNow(espnow_cfg)),
        // Collector: this node provides the CSI output (serialized).
        Some(CsiConfig::default()),
        None,
        csi_hardware,
    );
    // 802.11n (HT) so HT20 frames from the central are captured as HT.
    node.set_protocol(esp_radio::wifi::Protocol::N);
    // RX only: receive + capture CSI, never transmit. No CSI callback is set, so
    // each packet flows through the serialized inline-logging path.
    node.set_io_tasks(IOTaskConfig::new(false, true));

    node.run().await;

    loop {
        log_ln!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }
}
