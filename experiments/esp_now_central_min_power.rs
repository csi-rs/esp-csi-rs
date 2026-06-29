//! Power-test ESP-NOW central — **minimal** path (raw `EspNow`, no `CSINode`).
//!
//! Platform-floor counterpart to `esp_now_central_power`: identical radio setup
//! (STA bring-up, ch 11, MCS0-LGI/HT20, 8-byte broadcast at the same fixed rate)
//! but **without the state machine** — no `CSINode`, no `ControlPacket`
//! serialization, no RX/TX scheduler. Comparing the two isolates the power cost
//! of the esp-csi-rs state machine itself:
//!   * `(min active − min idle)`        = raw ESP-NOW transmit power.
//!   * `(deployed idle − min idle)`     = state-machine radio-up/poll overhead.
//!   * `(deployed active − min active)` = state-machine transmit overhead.
//!
//! One binary, two captures via `TX_ENABLED`:
//!   * `true`  → active: fixed-rate raw broadcast loop.
//!   * `false` → idle baseline: radio up (STA + channel + peer PHY), **no TX**,
//!     just sleep (no poll loop → the true minimal radio-up floor).
//!
//! Flash with `TX_ENABLED = true` → `UM34C_espnow_esp32min_active.csv`; flip to
//! `false` → `UM34C_espnow_esp32min_idle.csv`. Build: `--features=esp32,async-print`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Instant, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{log_ln, logging::logging::init_logger, set_peer_espnow_phy};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNow, WifiPhyRate};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, SecondaryChannel, WifiController};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Wi-Fi channel — match `esp_now_central_power` and the spec (ch 11).
const CHANNEL: u8 = 11;
/// On-air payload size, synced with the deployed/other implementations.
const PAYLOAD_B: usize = 8;
/// Fixed, sub-saturation offered rate (Hz) — must match `esp_now_central_power`.
const PACKET_RATE_HZ: u64 = 1000;
/// `true` = active (broadcasting); `false` = idle baseline (radio up, no TX).
const TX_ENABLED: bool = false;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();
static ESP_NOW: static_cell::StaticCell<EspNow<'static>> = static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// Slow liveness heartbeat only — no per-packet serial (keeps it silent so the
/// power meter sees the radio, not the UART).
async fn heartbeat_task() -> ! {
    loop {
        Timer::after(Duration::from_secs(30)).await;
        log_ln!("alive (tx_enabled={})", TX_ENABLED);
    }
}

/// Raw broadcast loop at a **fixed** rate — same burst-catch-up pacing as the
/// deployed TX loop (an absolute µs deadline allowed to fall behind, firing a
/// short catch-up burst), but the offered rate is constant (no ramp). At a
/// sub-saturation rate the budget rarely exceeds one, so it trickles ~1 frame
/// per interval; the burst shape is kept only so the rate is faithful if the
/// driver briefly stalls.
async fn tx_loop(esp_now: &'static mut EspNow<'static>) -> ! {
    const CATCH_UP_BURST: u8 = 16;
    const FAST_BACKOFF_US: u64 = 50;
    let tx_interval_us = 1_000_000 / PACKET_RATE_HZ;

    let mut seq: u32 = 0;
    let mut buf = [0u8; PAYLOAD_B];
    let mut next_tx_us = Instant::now().as_micros().saturating_add(tx_interval_us);
    log_ln!("sending numbered frames at {} Hz.", PACKET_RATE_HZ);
    loop {
        let mut now_us = Instant::now().as_micros();
        let mut budget = CATCH_UP_BURST;
        while now_us >= next_tx_us && budget > 0 {
            budget -= 1;
            buf[0] = (seq >> 24) as u8;
            buf[1] = (seq >> 16) as u8;
            buf[2] = (seq >> 8) as u8;
            buf[3] = seq as u8;

            let mut sent_ok = false;
            match esp_now.send(&BROADCAST_ADDRESS, &buf) {
                Ok(waiter) => {
                    core::mem::forget(waiter);
                    sent_ok = true;
                    seq = seq.wrapping_add(1);
                }
                Err(_) => Timer::after_micros(FAST_BACKOFF_US).await,
            }
            next_tx_us = next_tx_us.saturating_add(tx_interval_us);
            now_us = Instant::now().as_micros();
            if !sent_ok {
                break;
            }
        }

        let until_tx = next_tx_us.saturating_sub(Instant::now().as_micros());
        let wait_us = until_tx.min(tx_interval_us / 4).max(1);
        Timer::after_micros(wait_us).await;
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!(
        "Power test — MINIMAL ESP-NOW central (raw, no CSINode), tx_enabled={}, rate={} Hz, ch {}",
        TX_ENABLED,
        PACKET_RATE_HZ,
        CHANNEL
    );

    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_static_tx_buf_num(25)
        .with_dynamic_tx_buf_num(128)
        .with_ampdu_tx_enable(false)
        .with_tx_queue_size(32);
    let (wifi_controller, interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);
    let esp_now_ref = ESP_NOW.init(interfaces.esp_now);

    // MCS0-LGI + HT20 on CHANNEL: bring up an unassociated STA, set the channel,
    // force the broadcast peer's rate (mirrors esp-csi / the deployed path).
    if controller
        .set_config(&Config::Station(StationConfig::default()))
        .is_err()
    {
        log_ln!("PHY: STA bring-up failed; rate may stay default/legacy");
    }
    let _ = controller.set_channel(CHANNEL, SecondaryChannel::None);
    set_peer_espnow_phy(&BROADCAST_ADDRESS, WifiPhyRate::RateMcs0Lgi, None);

    if TX_ENABLED {
        join(tx_loop(esp_now_ref), heartbeat_task()).await;
    } else {
        // Idle baseline: radio up, no transmit, no poll loop — the minimal
        // radio-up floor (lower than the deployed RX-listen idle by design).
        let _ = esp_now_ref;
        heartbeat_task().await;
    }
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
