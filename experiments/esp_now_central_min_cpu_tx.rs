//! Companion **minimal** TX for the matched esp-now-idf comparison — pairs with
//! `esp_now_peripheral_min_cpu` (the minimal DUT).
//!
//! This is the same raw-broadcast traffic generator as the standard
//! `esp_now_central_exper_cpu_tx`: it marches the shared `cpu_test_schedule`
//! and, during the cell phases, broadcasts a fixed `0xA5` byte pattern at the
//! cell's `rate_hz`/`payload_b` over ESP-NOW. There is **no** `ControlPacket`
//! framing, sequence number, magic, or timestamp on the wire — exactly the
//! stimulus the ESP-IDF `espnow_tx_cpu_test` emits, so the two RX builds see an
//! equivalent on-air load.
//!
//! It is kept as a separate, explicitly-named example so the minimal DUT/TX
//! pair is self-contained; functionally it is identical to
//! `esp_now_central_exper_cpu_tx` (the framing always lived on the RX side, in
//! the DUT's `ingest_control_packet`, not here).
//!
//! On-air payload cap: ESP-NOW frames hard-cap at `ESP_NOW_MAX_DATA_LEN = 250`
//! bytes, so a nominal `payload_b` of 512 goes out as 250 on air. The nominal
//! value is preserved in the SCHEDULE/PHASE_BEGIN records; the actual on-air
//! length is logged in `TX_STATS,...,<wire_payload>` (spec §6.2).
//!
//! Sync model and build flags match the standard TX — see
//! `esp_now_central_exper_cpu_tx` for details.
//!
//! Build: `cargo build --release --example esp_now_central_min_cpu_tx --features <chip>,async-print`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Instant, Timer};
use esp_csi_rs::logging::logging::LogMode;
use esp_csi_rs::{log_ln, logging::logging::init_logger, set_peer_espnow_phy};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{EspNow, WifiPhyRate, BROADCAST_ADDRESS, ESP_NOW_MAX_DATA_LEN};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, SecondaryChannel, WifiController};
use portable_atomic::{AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

#[path = "cpu_test_schedule.rs"]
mod cpu_test_schedule;
use cpu_test_schedule::{phases_iter, PhaseKind, BOOT_DELAY_S, TEST_CHANNEL};

extern crate alloc;

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();
static ESP_NOW: static_cell::StaticCell<EspNow<'static>> = static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

/// Per-phase send stats published from the TX loop, read once per second
/// by the per-second emitter for `TX_STATS` log lines.
static TX_SENT: AtomicU32 = AtomicU32::new(0);
static TX_FAILED: AtomicU32 = AtomicU32::new(0);
static CURRENT_WIRE_PAYLOAD: AtomicU32 = AtomicU32::new(0);

/// Static payload buffer — fixed byte pattern so the same bytes go on the wire
/// across all runs (deterministic frame, no framing/headers).
const MAX_PAYLOAD: usize = ESP_NOW_MAX_DATA_LEN;
static mut PAYLOAD_BUF: [u8; MAX_PAYLOAD] = [0xA5; MAX_PAYLOAD];

async fn per_second_emitter() -> ! {
    let t0 = Instant::now();
    loop {
        Timer::after(Duration::from_secs(1)).await;
        let t_ms = t0.elapsed().as_millis();
        let sent = TX_SENT.swap(0, Ordering::Relaxed);
        let failed = TX_FAILED.swap(0, Ordering::Relaxed);
        let wire = CURRENT_WIRE_PAYLOAD.load(Ordering::Relaxed);
        log_ln!("TX_STATS,{},{},{},{}", t_ms, sent, failed, wire);
    }
}

async fn tx_schedule_driver(esp_now: &'static mut EspNow<'static>) -> ! {
    // Channel (+HT40 secondary) and the per-peer MCS0/HT40 rate config are set
    // on the controller in `main` before this task runs; calling
    // `esp_now.set_channel` here would reset the secondary to HT20, so don't.

    log_ln!("CPU_FREQ,{}", 240_000_000u32);
    for (idx, p) in phases_iter() {
        log_ln!(
            "SCHEDULE,{},{},{},{},{},{}",
            idx,
            p.kind.as_str(),
            p.rate_hz,
            p.payload_b,
            p.rep,
            p.duration_s
        );
    }

    Timer::after(Duration::from_secs(BOOT_DELAY_S as u64)).await;
    let t0 = Instant::now();

    for (idx, p) in phases_iter() {
        let t_ms = t0.elapsed().as_millis();
        log_ln!(
            "PHASE_BEGIN,{},{},{},{},{},{}",
            t_ms,
            idx,
            p.kind.as_str(),
            p.rate_hz,
            p.payload_b,
            p.rep
        );

        match p.kind {
            PhaseKind::BaselineWarmup | PhaseKind::BaselineCapture => {
                CURRENT_WIRE_PAYLOAD.store(0, Ordering::Relaxed);
                Timer::after(Duration::from_secs(p.duration_s as u64)).await;
            }
            PhaseKind::CellWarmup | PhaseKind::CellCapture => {
                let wire_payload = (p.payload_b as usize).min(MAX_PAYLOAD);
                CURRENT_WIRE_PAYLOAD.store(wire_payload as u32, Ordering::Relaxed);

                let period_us: u64 = 1_000_000 / p.rate_hz as u64;
                let phase_deadline = Instant::now() + Duration::from_secs(p.duration_s as u64);
                let mut next_send = Instant::now();
                let payload: &[u8] = unsafe {
                    // SAFETY: PAYLOAD_BUF is only read here on a single task; the
                    // slice is handed to `send` which copies into the ESP-NOW
                    // driver before returning.
                    &PAYLOAD_BUF[..wire_payload]
                };

                while Instant::now() < phase_deadline {
                    let now = Instant::now();
                    if now < next_send {
                        Timer::at(next_send).await;
                    }
                    next_send += Duration::from_micros(period_us);

                    match esp_now.send(&BROADCAST_ADDRESS, payload) {
                        Ok(waiter) => {
                            // Drop the waiter via mem::forget so we don't block
                            // on the send-completion callback.
                            core::mem::forget(waiter);
                            TX_SENT.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            TX_FAILED.fetch_add(1, Ordering::Relaxed);
                            Timer::after(Duration::from_micros(period_us)).await;
                        }
                    }
                }
                CURRENT_WIRE_PAYLOAD.store(0, Ordering::Relaxed);
            }
        }

        let t_ms = t0.elapsed().as_millis();
        log_ln!("PHASE_END,{},{}", t_ms, idx);
    }

    log_ln!("RUN_COMPLETE,{}", t0.elapsed().as_millis());
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    log_ln!("Embassy initialized!");
    log_ln!("Starting MINIMAL ESP-NOW TX CPU-Utilization Traffic Generator (matched to esp-now-idf)");

    let config_radio = esp_radio::wifi::ControllerConfig::default()
        .with_static_tx_buf_num(25)
        .with_dynamic_tx_buf_num(128)
        .with_ampdu_tx_enable(false)
        .with_tx_queue_size(32);
    let (wifi_controller, interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");

    let controller = WIFI_CONTROLLER.init(wifi_controller);
    let esp_now_ref = ESP_NOW.init(interfaces.esp_now);

    // Match the esp-csi reference raw TX PHY: MCS0-LGI + HT20 on TEST_CHANNEL.
    // The per-peer rate config requires the radio in started STA mode, so bring
    // it up (unassociated STA), set the channel (no secondary = HT20), then
    // force the broadcast peer's rate — mirroring esp-csi's
    // `esp_now_set_peer_rate_config`.
    if controller
        .set_config(&Config::Station(StationConfig::default()))
        .is_err()
    {
        log_ln!("PHY: STA bring-up failed; rate may stay default/legacy");
    }
    let _ = controller.set_channel(TEST_CHANNEL, SecondaryChannel::None);
    set_peer_espnow_phy(&BROADCAST_ADDRESS, WifiPhyRate::RateMcs0Lgi, None);

    join(tx_schedule_driver(esp_now_ref), per_second_emitter()).await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
