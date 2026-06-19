//! Packet drop rate test — **minimal** transmitter (raw ESP-NOW, no `CSINode`).
//!
//! The platform-floor counterpart to esp-csi's bare `espnow_tx_drop`, for a fair
//! cross-implementation CSI-drop comparison. It broadcasts an 8-byte frame
//! (4-byte big-endian sequence number + 4 zero padding) over raw
//! `esp_radio::esp_now::EspNow` with the **exact burst-catch-up pacing the
//! deployed `CSINode` TX loop uses** (the one that reaches the ~3 k radio
//! ceiling), with **no** `CSINode` state machine, `ControlPacket` framing,
//! magic, or timestamp — so the two TX builds put an equivalent load on the
//! link. Pair with `esp_now_peripheral_min_drop`.
//!
//! `seq` increments only when the driver accepts the frame (`send` returns Ok =
//! queued); an enqueue failure under saturation retries the **same** seq, so
//! local back-pressure never inflates the receiver's CSI-drop count. Once per
//! second the achieved queued rate is printed as `TX: <n>` (sanity channel).
//!
//! PHY is pinned to MCS0-LGI / HT20 on channel 1 via the per-peer rate config
//! (mirroring esp-csi's `esp_now_set_peer_rate_config`).
//!
//! Build: `cargo build --release --example esp_now_central_min_drop --features <chip>,async-print`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Instant, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{log_ln, logging::logging::init_logger, set_peer_espnow_phy};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{EspNow, WifiPhyRate, BROADCAST_ADDRESS};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, SecondaryChannel, WifiController};
use portable_atomic::{AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Wi-Fi channel — must match `esp_now_peripheral_min_drop`.
const CHANNEL: u8 = 1;
/// On-air payload size, synced with the other implementations: 4-byte seq + pad.
const PAYLOAD_B: usize = 8;

/// Offered-rate ramp: 500 → 5000 pps in 500-pps steps, 30 s dwell each
/// (~300 s), to trace the drop-vs-offered-rate knee in one run. Identical
/// schedule across all four drop transmitters.
const RAMP_STEP_HZ: u64 = 500;
const RAMP_MAX_HZ: u64 = 5000;
const RAMP_DWELL_S: u64 = 30;

/// Offered rate (Hz) for a given elapsed time: `500 * (elapsed/30 + 1)`, capped
/// at 5000 (held thereafter).
fn ramp_target_hz(elapsed_s: u64) -> u64 {
    (RAMP_STEP_HZ * (elapsed_s / RAMP_DWELL_S + 1)).min(RAMP_MAX_HZ)
}

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();
static ESP_NOW: static_cell::StaticCell<EspNow<'static>> = static_cell::StaticCell::new();

/// Frames queued OK this window, drained once per second by the reporter.
static TX_SENT: AtomicU32 = AtomicU32::new(0);
/// Current ramp setpoint (Hz), published by `tx_loop` for the reporter.
static CURRENT_TARGET_HZ: AtomicU32 = AtomicU32::new(RAMP_STEP_HZ as u32);

esp_bootloader_esp_idf::esp_app_desc!();

/// Once per second, print the achieved queued rate and the current ramp
/// setpoint: `TX: <n> <target_hz>`.
async fn tx_report_task() -> ! {
    loop {
        Timer::after(Duration::from_secs(1)).await;
        let n = TX_SENT.swap(0, Ordering::Relaxed);
        let hz = CURRENT_TARGET_HZ.load(Ordering::Relaxed);
        log_ln!("TX: {} {}", n, hz);
    }
}

/// Raw broadcast loop — a faithful copy of the deployed `CSINode` TX loop's
/// `tx_fast_no_wait` pacing (`central/esp_now.rs`), which is the proven path to
/// the ~3 k radio ceiling. The shape matters: trickling one send per
/// timer/yield round-trip caps the offer at ~1–2 k (each await costs hundreds of
/// µs), so instead we keep an absolute µs deadline that is *allowed to fall
/// behind* under load and fire a **catch-up burst** of up to 16 sends with no
/// await between them — filling the driver queue — then back off only when the
/// driver pushes back (`Err` ⇒ 50 µs) or sleep a short ≤25 µs slice after the
/// burst. (Do NOT resync the deadline to "now": the backlog is what triggers the
/// multi-send bursts.) `seq` advances only on a successful queue; an enqueue
/// failure retries the same seq, so back-pressure never inflates the drop count.
async fn tx_loop(esp_now: &'static mut EspNow<'static>) -> ! {
    const CATCH_UP_BURST: u8 = 16; // TX_CATCH_UP_BURST_NO_WAIT
    const FAST_BACKOFF_US: u64 = 50; // TX_FAST_BACKOFF_US

    let mut seq: u32 = 0;
    let mut buf = [0u8; PAYLOAD_B];
    let t0 = Instant::now();
    let mut tx_interval_us = 1_000_000 / ramp_target_hz(0);
    let mut next_tx_us = Instant::now().as_micros().saturating_add(tx_interval_us);
    log_ln!("sending numbered frames.");
    loop {
        // Step the offered rate per the ramp schedule (interval shrinks as the
        // target climbs); publish the setpoint for the reporter.
        let elapsed_s = (Instant::now() - t0).as_secs();
        let target = ramp_target_hz(elapsed_s);
        tx_interval_us = 1_000_000 / target;
        CURRENT_TARGET_HZ.store(target as u32, Ordering::Relaxed);

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
                    core::mem::forget(waiter); // don't block on send-complete
                    sent_ok = true;
                    seq = seq.wrapping_add(1);
                    TX_SENT.fetch_add(1, Ordering::Relaxed);
                }
                // Driver TX buffers full: brief backoff so the WiFi task drains.
                Err(_) => Timer::after_micros(FAST_BACKOFF_US).await,
            }
            // Keep periodic phase from the previous deadline; if we're behind,
            // the next iteration fires again immediately (the catch-up burst).
            next_tx_us = next_tx_us.saturating_add(tx_interval_us);
            now_us = Instant::now().as_micros();
            if !sent_ok {
                break;
            }
        }

        // Short slice between bursts (a fraction of the current interval), not a
        // per-frame timer.
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

    log_ln!("Starting MINIMAL drop-rate transmitter (raw ESP-NOW, no CSINode)");

    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_static_tx_buf_num(25)
        .with_dynamic_tx_buf_num(128)
        .with_ampdu_tx_enable(false)
        .with_tx_queue_size(32);
    let (wifi_controller, interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);
    let esp_now_ref = ESP_NOW.init(interfaces.esp_now);

    // MCS0-LGI + HT20 on CHANNEL: bring up an unassociated STA, set the channel
    // (no secondary = HT20), then force the broadcast peer's rate — the per-peer
    // rate config requires the radio in started STA mode (mirrors esp-csi).
    if controller
        .set_config(&Config::Station(StationConfig::default()))
        .is_err()
    {
        log_ln!("PHY: STA bring-up failed; rate may stay default/legacy");
    }
    let _ = controller.set_channel(CHANNEL, SecondaryChannel::None);
    set_peer_espnow_phy(&BROADCAST_ADDRESS, WifiPhyRate::RateMcs0Lgi, None);

    join(tx_loop(esp_now_ref), tx_report_task()).await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
