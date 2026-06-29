//! Fast one-to-one ESP-NOW collector (asymmetric simplex).
//!
//! The collector broadcasts a **sparse discovery beacon** (~1 Hz) until it hears
//! a [`EspNowFastSource`](crate::PeripheralOpMode::EspNowFastSource), then stops
//! beaconing entirely and goes **RX-only**, letting the source own all the
//! airtime with its continuous unicast flood. CSI is captured from that flood by
//! the radio's `capture_csi_info` callback independently of this task — the
//! collector's only jobs are (1) discovery and (2) draining the receive pool so
//! it never wedges. Leaving the channel to a single transmitter is what
//! maximizes CSI packets/sec versus the balanced bidirectional ESP-NOW mode.

use embassy_futures::select::{Either, Either3, select, select3};
use embassy_time::{Duration, Instant, Timer};

use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNow};

use crate::espnow_phy::with_espnow_recv_suspended;
use crate::{
    CENTRAL_MAGIC_NUMBER, ControlPacket, EspNowConfig, IOTaskConfig, PERIPHERAL_MAGIC_NUMBER,
    PeripheralPacket, STOP_SIGNAL, log_ln, parse_with_magic, serialize_with_magic,
};
#[cfg(feature = "statistics")]
use crate::STATS;
#[cfg(feature = "statistics")]
use portable_atomic::Ordering;

/// Discovery beacon period.
const BEACON_PERIOD: Duration = Duration::from_secs(1);
/// Max frames drained from the pool per RX wake-up (absorbs a full 802.11n
/// AMPDU without holding the executor). Mirrors the peripheral RX-only cap.
const RX_BURST_MAX_RX_ONLY: u16 = 64;
/// Control-packet scratch buffer (4-byte magic + small postcard body).
const BEACON_BUF_LEN: usize = 16;

/// Run the fast ESP-NOW collector: sparse beacon → detect source → RX-only.
pub async fn run_esp_now_fast_collector(
    esp_now: &mut EspNow<'static>,
    config: &EspNowConfig,
    io_tasks: IOTaskConfig,
) {
    let peer_mac = config.peer_mac();
    let send_magic = peer_mac.is_none();

    // In HT40 mode the channel (+secondary) was already set on the controller
    // before this task ran; calling esp_now.set_channel here would reset the
    // secondary to HT20, so skip it.
    if config.secondary_channel().is_none() {
        with_espnow_recv_suspended(|| {
            esp_now.set_channel(config.channel).unwrap();
        });
    }
    // The collector only receives — it never forces a TX PHY (forced broadcast
    // PHY wedges C5, and sparse beacons are rate-insensitive).
    log_ln!("esp-now version {}", esp_now.version().unwrap());

    #[cfg(feature = "statistics")]
    let mut beacon_seq: u32 = 0;
    let mut tx_buf = [0u8; BEACON_BUF_LEN];

    // ---- Phase 1: sparse discovery beacon until a source is detected --------
    // Manual pairing pre-latches the configured peer; auto pairing detects the
    // source's magic-tagged hello.
    let mut detected_src: Option<[u8; 6]> = peer_mac;

    if detected_src.is_none() {
        let mut beacon_at = Instant::now();
        loop {
            if io_tasks.tx_enabled && Instant::now() >= beacon_at {
                let pkt = ControlPacket::new(
                    true,
                    #[cfg(feature = "statistics")]
                    beacon_seq,
                );
                if let Ok(msg) =
                    serialize_with_magic(&pkt, CENTRAL_MAGIC_NUMBER, send_magic, &mut tx_buf)
                {
                    // One send in flight, awaited to completion (driver has a
                    // single global completion flag).
                    let _ = esp_now.send_async(&BROADCAST_ADDRESS, msg).await;
                    #[cfg(feature = "statistics")]
                    {
                        STATS.tx_count.fetch_add(1, Ordering::Relaxed);
                        beacon_seq = beacon_seq.wrapping_add(1);
                    }
                }
                beacon_at = Instant::now() + BEACON_PERIOD;
            }

            let until_beacon = beacon_at.saturating_duration_since(Instant::now());
            match select3(STOP_SIGNAL.wait(), crate::esp_now_pool::receive_async(), Timer::after(until_beacon)).await {
                Either3::First(_) => {
                    log_ln!("STOP signal received, shutting down fast collector...");
                    STOP_SIGNAL.signal(());
                    return;
                }
                Either3::Second(r) => {
                    let is_source = match peer_mac {
                        Some(expected) => r.info.src_address == expected,
                        None => parse_with_magic::<PeripheralPacket>(
                            r.data(),
                            PERIPHERAL_MAGIC_NUMBER,
                            true,
                        )
                        .is_some(),
                    };
                    if is_source {
                        detected_src = Some(r.info.src_address);
                        break;
                    }
                }
                Either3::Third(_) => { /* beacon deadline — loop to re-beacon */ }
            }
        }
    }

    if let Some(src) = detected_src {
        log_ln!(
            "ESP-NOW fast collector: source {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} detected, beacon stopped, RX-only flood capture",
            src[0],
            src[1],
            src[2],
            src[3],
            src[4],
            src[5],
        );
    }

    // ---- Phase 2: RX-only collect ------------------------------------------
    // CSI is captured by `capture_csi_info` for every flood frame the radio
    // receives, independent of this drain. We drain purely so the 16-slot pool
    // never stays full and the source-flood frames keep flowing.
    loop {
        match select(STOP_SIGNAL.wait(), crate::esp_now_pool::receive_async()).await {
            Either::First(_) => {
                log_ln!("STOP signal received, shutting down fast collector...");
                STOP_SIGNAL.signal(());
                break;
            }
            Either::Second(_first) => {
                let mut drained: u16 = 0;
                while drained < RX_BURST_MAX_RX_ONLY {
                    if crate::esp_now_pool::receive().is_none() {
                        break;
                    }
                    drained = drained.saturating_add(1);
                }
            }
        }
    }
}
