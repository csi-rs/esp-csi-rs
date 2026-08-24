//! ESP-NOW emitter — channel sounding without raw injection.
//!
//! Same job as `ht20_emitter`: put steady, known RF energy into a fixed channel
//! so a collector can measure the channel's response. The difference is *how*
//! the frames leave the radio. `ht20_emitter` calls `esp_wifi_80211_tx` to
//! inject a hand-built frame; this example sends ESP-NOW broadcasts, which go
//! out through the driver's ordinary TX path as vendor-specific action frames.
//!
//! That matters because **raw injection does not radiate on the ESP32-S3** (see
//! the "Emitter chip support" section of the README): `esp_wifi_80211_tx` reports
//! success and nothing arrives. ESP-NOW is the emitter path that works on every
//! supported chip, S3 included, and it still needs no association, no AP, and no
//! peer running on the other side — broadcast frames are never ACKed, so the
//! offered rate stays flat whether anyone is listening or not.
//!
//! This node captures no CSI. Pair it with a collector locked to the same
//! channel — either the in-tree `collector_sniffer`, or `esp-csi-litegui-rs`
//! built with its default `mode-snf` (promiscuous sniffer), whose `CSI_CHANNEL`
//! is 1 and therefore matches [`CHANNEL`] below out of the box.
//!
//! ESP-NOW frames carry this node's station MAC as the transmitter address, so a
//! collector attributes CSI to this emitter exactly as it would for an injected
//! sounding frame, and several emitters can share one collector.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::log_ln;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{BROADCAST_ADDRESS, WifiPhyRate};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, PowerSaveMode, Protocols, SecondaryChannel, WifiController};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

/// Channel to sound. Every node in the capture set must agree on this — a
/// mismatch is silent and looks like "no packets are arriving". 1 is the
/// `esp-csi-litegui-rs` `CSI_CHANNEL` default.
const CHANNEL: u8 = 1;

/// Delay between frames. Each send also waits for the driver's TX-done
/// callback, so the achieved rate is a little below `1000 / PERIOD_MS`.
const PERIOD_MS: u64 = 1;

/// PHY rate for ESP-NOW frames.
///
/// MCS0 long-GI is an 802.11n HT20 PPDU, so the collector measures an HT-LTF
/// channel estimate (64 subcarriers — what `esp-csi-litegui-rs` decodes). If the
/// driver rejects the rate config the frames fall back to the ESP-NOW default
/// legacy rate; CSI still arrives, just from the L-LTF of a non-HT PPDU.
const TX_RATE: WifiPhyRate = WifiPhyRate::RateMcs0Lgi;

/// Payload of the broadcast. Contents are irrelevant to CSI — the channel
/// estimate comes from the PPDU preamble — so this carries only a tag and a
/// counter, which makes the stream easy to identify on the air.
const TAG: [u8; 4] = *b"ECSI";

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(_spawner, LogMode::Text);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    log_ln!("Starting ESP-NOW emitter on channel {}", CHANNEL);

    // Bring the station interface up *without* associating. `set_config` starts
    // the interface (esp-radio 0.18 dropped the separate `start_async`), which
    // is all ESP-NOW needs — it is connection-less.
    if controller
        .set_config(&Config::Station(StationConfig::default()))
        .is_err()
    {
        log_ln!("espnow emitter: set_config(Station) failed");
    }

    // B|G|N on 2.4 GHz (the `Protocols` default) so an HT PPDU is allowed, and
    // no modem sleep so the TX cadence stays even.
    let _ = controller.set_protocols(Protocols::default());
    let _ = controller.set_power_saving(PowerSaveMode::None);

    // Lock the channel *after* `set_config`, which embeds its own defaults.
    // Nothing moves it again because this node never associates or scans.
    if controller.set_channel(CHANNEL, SecondaryChannel::None).is_err() {
        log_ln!("espnow emitter: set_channel failed");
    }

    let esp_now = &mut interfaces.esp_now;

    // Rate config has to come after the interface is started. Log a rejection
    // instead of failing: the emitter is still useful at the default rate.
    if esp_now.set_rate(TX_RATE).is_err() {
        log_ln!("espnow emitter: set_rate rejected — frames go out at the default rate");
    }

    // `EspNow::new_internal` already registered a broadcast peer on the station
    // interface, so no `add_peer` call is needed here.
    let src = interfaces.station.mac_address();
    log_ln!(
        "ESP-NOW emitter running: ch {}, {} ms period, src {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        CHANNEL,
        PERIOD_MS,
        src[0],
        src[1],
        src[2],
        src[3],
        src[4],
        src[5]
    );

    let mut payload = [0u8; 16];
    payload[..4].copy_from_slice(&TAG);

    let mut sent: u32 = 0;
    let mut failed: u32 = 0;
    let mut seq: u32 = 0;
    let mut last_report = Instant::now();
    let mut last_sent: u32 = 0;

    loop {
        payload[4..8].copy_from_slice(&seq.to_le_bytes());
        seq = seq.wrapping_add(1);

        // Broadcasts are never ACKed, so this resolves as soon as the frame has
        // left the MAC — a failure here means the driver refused it, not that
        // no one was listening.
        match esp_now.send_async(&BROADCAST_ADDRESS, &payload).await {
            Ok(()) => sent = sent.wrapping_add(1),
            Err(_) => {
                failed = failed.wrapping_add(1);
                if failed == 1 {
                    log_ln!("espnow emitter: first send failed");
                }
            }
        }

        if last_report.elapsed() >= Duration::from_secs(1) {
            log_ln!(
                "TX {} frames/s (total {}, failed {})",
                sent.wrapping_sub(last_sent),
                sent,
                failed
            );
            last_sent = sent;
            last_report = Instant::now();
        }

        Timer::after(Duration::from_millis(PERIOD_MS)).await;
    }
}
