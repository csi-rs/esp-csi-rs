//! Power-test ESP-NOW central — **deployed** path (full `CSINode` state machine).
//!
//! DUT for the `espnow_active` power scenario (esp-csi-rs side). Broadcasts at a
//! fixed, sub-saturation rate with **silent CSI** (no per-packet serial), so the
//! UM34C reads the stack's transmit power, not the UART. Pair it with the matched
//! esp-csi `espnow_central_heap` at the same `PACKET_RATE_HZ` / payload.
//!
//! One binary covers both captures via `TX_ENABLED`:
//!   * `true`  → **active**: broadcaster (`tx_enabled, !rx_enabled`) at PACKET_RATE_HZ.
//!   * `false` → **idle baseline**: radio up, RX-listening, **TX off** (`!tx_enabled,
//!     rx_enabled`) — the faithful "radio up, not transmitting" state. (TX *and* RX
//!     both off would busy-spin the run loop at 1 µs, inflating idle power, so the
//!     idle keeps RX enabled — its natural 20 µs listen poll.)
//!
//! Flash with `TX_ENABLED = true`, capture → `UM34C_espnow_esp32_active.csv`;
//! flip to `false`, re-flash, capture → `UM34C_espnow_esp32_idle.csv`.
//! Build: `--features=esp32,async-print`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::config::CsiConfig;
use esp_csi_rs::logging::logging::{init_logger, LogMode};
use esp_csi_rs::{
    log_ln, set_csi_logging_enabled, CSINode, CSINodeClient, CSINodeHardware, CollectionMode,
    EspNowConfig, IOTaskConfig,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Wi-Fi channel — match the esp-csi central and the power-test spec (ch 11).
const CHANNEL: u8 = 11;
/// Fixed, sub-saturation offered rate (Hz). Both stacks in the pair must use the
/// same rate so the on-air load is identical and the power delta reflects the
/// stack, not the traffic.
const PACKET_RATE_HZ: u16 = 1000;
/// `true` = active (broadcasting); `false` = idle baseline (radio up, TX off).
const TX_ENABLED: bool = true;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// Slow liveness heartbeat only — no per-packet serial (keeps CSI silent so the
/// power meter sees the radio, not the UART).
async fn heartbeat_task() -> ! {
    loop {
        Timer::after(Duration::from_secs(30)).await;
        log_ln!("alive (tx_enabled={})", TX_ENABLED);
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);
    // Silent CSI: close the inline per-packet log gate init_logger opens. The
    // active broadcaster is TX-only (no CSI RX), but the idle mode enables RX, so
    // close it unconditionally to keep the meter on the radio, not UART.
    set_csi_logging_enabled(false);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!(
        "Power test — DEPLOYED ESP-NOW central (CSINode), tx_enabled={}, rate={} Hz, ch {}",
        TX_ENABLED,
        PACKET_RATE_HZ,
        CHANNEL
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut node_handle = CSINodeClient::new();
    let csi_hardware = CSINodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::new(
        esp_csi_rs::Node::Central(esp_csi_rs::CentralOpMode::EspNow(
            EspNowConfig::default().with_channel(CHANNEL),
        )),
        CollectionMode::Collector,
        Some(CsiConfig::default()),
        Some(PACKET_RATE_HZ),
        csi_hardware,
    );
    node.set_protocol(esp_radio::wifi::Protocol::N);
    node.set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi);

    // Active = pure broadcaster (tx, !rx → fast-no-wait TX path). Idle = radio up,
    // RX-listening, TX off (the faithful "not transmitting" baseline).
    node.set_io_tasks(IOTaskConfig::new(TX_ENABLED, !TX_ENABLED));

    // CSI capture stays on but nothing prints it (silent) — measure the radio.
    let _ = &mut node_handle;
    join(node.run(), heartbeat_task()).await;
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
