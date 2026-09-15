//! **Emitter** — unassociated transmit-only channel sounding.
//!
//! | Attribute | Value |
//! |---|---|
//! | Operational mode | Emitter (raw sounding) |
//! | Network role | Central — the sounding frames *are* the network's traffic |
//! | Collection mode | Listener — it captures nothing, so it has nothing to report |
//! | Session role | Responder |
//!
//! Neither role is settable, so `CSINode::emitter` takes no role arguments and no CSI config.
//!
//! This is the case the older two-role vocabulary could not express. "Emitter" was treated as a
//! role opposite "collector", which left no name for the far more useful description: a **central
//! listener**, a node that puts known energy into the channel and measures nothing. See
//! `docs/network-model.md`.
//!
//! Because the frames carry no meaning, an emitter needs no peer, no handshake and no protocol.
//! Pair it with any number of `sniffer` nodes on the same primary channel; adding collectors costs
//! the emitter nothing, and several emitters can share one collector because each frame carries its
//! transmitter's MAC.
//!
//! ## Transport is chosen by the chip, not by you
//!
//! On the ESP32-C5 and C6 the frames are raw-injected with `esp_wifi_80211_tx`. On every other
//! supported part they go out as ESP-NOW broadcasts instead, because **raw injection does not
//! radiate on the ESP32-S3**: the call reports success for every frame and nothing arrives. The
//! crate picks the transport that works for the chip you built for, so there is no knob here to set
//! wrong. See `docs/emitter-support.md`.
//!
//! ## Bandwidth
//!
//! [`HtBandwidth`] is plain 802.11n and works on every supported chip. HT40 needs room in the band:
//! `Ht40Above` on channel 7 occupies up to channel 11 and `Ht40Below` occupies down to channel 3, so
//! a primary too close to the edge silently falls back. Confirm it engaged at the *collector*, not
//! here: a subcarrier count >= 100 (commonly ~117) is HT40, ~53/~56 means fallback.
//!
//! Build / run:
//!   cargo esp32c6 --example emitter
//!   cargo esp32c5 --example emitter   # `CHANNEL >= 36` selects 5 GHz automatically

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::{CSINode, EmitterConfig, HtBandwidth, NodeHardware, log_ln};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

// ── Knobs ───────────────────────────────────────────────────────────────────────────────────────

/// Primary channel. Every node in the capture set must agree on it.
const CHANNEL: u8 = 7;

/// `Ht20`, `Ht40Above` or `Ht40Below`. HT40 roughly doubles the subcarriers.
const BANDWIDTH: HtBandwidth = HtBandwidth::Ht20;

/// Delay between injected frames. 20 ms is ~50 frames/s.
const PERIOD: Duration = Duration::from_millis(20);

/// Address a specific collector instead of broadcasting. Broadcast is the default; unicasting to
/// one collector tends to raise *that* collector's CSI callback rate.
const DST_MAC: Option<[u8; 6]> = None;

// ────────────────────────────────────────────────────────────────────────────────────────────────

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

    let config_radio = esp_radio::wifi::ControllerConfig::default();
    let (wifi_controller, mut interfaces) = esp_radio::wifi::new(peripherals.WIFI, config_radio)
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);

    let mut emitter = EmitterConfig::new(CHANNEL, BANDWIDTH).with_period(PERIOD);
    if let Some(mac) = DST_MAC {
        emitter = emitter.with_dst_mac(mac);
    }

    log_ln!(
        "Starting emitter (central listener) — channel {}, {} ms period",
        CHANNEL,
        PERIOD.as_millis()
    );

    let hardware = NodeHardware::new(&mut interfaces, controller);
    let mut node = CSINode::emitter(emitter, hardware);

    let started = Instant::now();
    node.run().await;

    loop {
        log_ln!("Emitter stopped after {} s", started.elapsed().as_secs());
        Timer::after(Duration::from_secs(5)).await;
    }
}
