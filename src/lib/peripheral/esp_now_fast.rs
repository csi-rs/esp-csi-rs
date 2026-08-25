//! Fast one-to-one ESP-NOW source (asymmetric simplex).
//!
//! The source listens for a
//! [`EspNowFastCollector`](crate::CentralOpMode::EspNowFastCollector) beacon,
//! learns the collector's MAC, registers it as a **unicast peer with a forced
//! PHY** (HT20 MCS7-LGI by default, HT40 when a secondary channel is set), sends
//! one magic-tagged hello so the collector switches to RX-only, then unicasts a
//! **continuous max-rate flood**. Unlike the balanced peripheral responder there
//! is no per-control-packet reply gating and no adaptive pacing — the source
//! transmits as fast as `send_async` completes, which (with the collector silent)
//! drives the maximum CSI packets/sec on the collector.

use embassy_futures::select::{Either, select};
use embassy_time::{Instant, Timer};

use esp_radio::esp_now::{
    Error as EspNowInnerError, EspNow, EspNowError, EspNowWifiInterface, PeerInfo, WifiPhyRate,
};

use crate::espnow_phy::with_espnow_recv_suspended;
use crate::{
    CENTRAL_MAGIC_NUMBER, ControlPacket, EspNowConfig, IOTaskConfig, PERIPHERAL_MAGIC_NUMBER,
    PeripheralPacket, STOP_SIGNAL, apply_peer_espnow_phy, log_ln, parse_with_magic,
    serialize_with_magic,
};
#[cfg(feature = "statistics")]
use crate::STATS;
#[cfg(feature = "statistics")]
use portable_atomic::Ordering;

/// Backoff when Wi-Fi TX buffers are full (`OutOfMemory` / `SendFailed`).
const TX_BACKOFF_US: u64 = 200;
/// How often (in sends) to re-verify the unicast peer still exists. Power of two
/// so the check is a cheap mask.
const PEER_HEALTHCHECK_PERIOD: u16 = 256;
/// Flood-frame scratch buffer (4-byte magic + small postcard body).
const FLOOD_BUF_LEN: usize = 16;

fn add_collector_peer(esp_now: &EspNow<'static>, mac: &[u8; 6], channel: u8) -> bool {
    if esp_now.peer_exists(mac) {
        return true;
    }
    esp_now
        .add_peer(PeerInfo {
            interface: EspNowWifiInterface::Station,
            peer_address: *mac,
            lmk: None,
            channel: Some(channel),
            encrypt: false,
        })
        .is_ok()
}

/// Run the fast ESP-NOW source: discover collector → forced-PHY unicast flood.
pub async fn run_esp_now_fast_source(
    esp_now: &mut EspNow<'static>,
    config: &EspNowConfig,
    freq_hz: Option<u16>,
    _io_tasks: IOTaskConfig,
) {
    let peer_mac = config.peer_mac();
    let send_magic = peer_mac.is_none();

    // In HT40 mode the channel (+secondary) was already set on the controller
    // before this task ran; skip set_channel so the secondary isn't reset.
    if config.secondary_channel().is_none() {
        with_espnow_recv_suspended(|| {
            esp_now.set_channel(config.channel).unwrap();
        });
    }
    log_ln!("esp-now version {}", esp_now.version().unwrap());

    // Forced unicast PHY: default to HT20 MCS7-LGI for max throughput unless the
    // user forced a specific rate. Applied to the learned collector peer below.
    let rate = if config.force_phy() {
        *config.phy_rate()
    } else {
        WifiPhyRate::RateMcs7Lgi
    };

    // ---- Phase 1: discover the collector -----------------------------------
    let collector_mac: [u8; 6] = match peer_mac {
        Some(mac) => mac,
        None => loop {
            match select(STOP_SIGNAL.wait(), crate::esp_now_pool::receive_async()).await {
                Either::First(_) => {
                    log_ln!("STOP signal received, shutting down fast source...");
                    STOP_SIGNAL.signal(());
                    return;
                }
                Either::Second(r) => {
                    if parse_with_magic::<ControlPacket>(r.data(), CENTRAL_MAGIC_NUMBER, true)
                        .is_some()
                    {
                        break r.info.src_address;
                    }
                }
            }
        },
    };

    // Register the collector as a unicast peer and force the PHY (recv-suspended,
    // C5-safe — per-peer rate config on a unicast peer works on all chips).
    add_collector_peer(esp_now, &collector_mac, config.channel);
    apply_peer_espnow_phy(&collector_mac, rate, config.secondary_channel());
    log_ln!(
        "ESP-NOW fast source: locked collector {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, unicast flood rate {:?}",
        collector_mac[0],
        collector_mac[1],
        collector_mac[2],
        collector_mac[3],
        collector_mac[4],
        collector_mac[5],
        rate,
    );

    // One magic-tagged hello so the collector deterministically stops beaconing
    // and switches to RX-only (auto-pairing only; manual mode pre-latches).
    if send_magic {
        let mut hello_buf = [0u8; 8];
        if let Ok(hello) = serialize_with_magic(
            &PeripheralPacket::new(),
            PERIPHERAL_MAGIC_NUMBER,
            true,
            &mut hello_buf,
        ) {
            let _ = esp_now.send_async(&collector_mac, hello).await;
        }
    }

    // ---- Phase 2: continuous flood -----------------------------------------
    // Optional rate cap (None = flat-out; the send_async completion is the
    // natural limiter as the radio frees TX buffers).
    let cap_interval_us = freq_hz.map(|f| 1_000_000u64 / (f.max(1) as u64));
    let mut next_tx_us = Instant::now().as_micros();
    #[cfg(feature = "statistics")]
    let mut seq: u32 = 0;
    let mut tx_buf = [0u8; FLOOD_BUF_LEN];
    let mut healthcheck: u16 = 0;

    loop {
        if STOP_SIGNAL.signaled() {
            log_ln!("STOP signal received, shutting down fast source...");
            STOP_SIGNAL.signal(());
            break;
        }

        // Periodic peer healthcheck — the driver peer table can churn under
        // pressure; keep the unicast peer present so TX doesn't stall on NotFound.
        healthcheck = healthcheck.wrapping_add(1);
        if (healthcheck & (PEER_HEALTHCHECK_PERIOD - 1)) == 0 && !esp_now.peer_exists(&collector_mac)
        {
            add_collector_peer(esp_now, &collector_mac, config.channel);
            apply_peer_espnow_phy(&collector_mac, rate, config.secondary_channel());
        }

        let pkt = ControlPacket::new(
            false,
            #[cfg(feature = "statistics")]
            seq,
        );
        let msg = match serialize_with_magic(&pkt, CENTRAL_MAGIC_NUMBER, send_magic, &mut tx_buf) {
            Ok(m) => m,
            Err(_) => {
                log_ln!("Failed to serialize ESP-NOW flood packet");
                break;
            }
        };

        // Exactly one send in flight, awaited to completion — never drop or
        // queue a second send first (the driver has one global completion flag).
        match esp_now.send_async(&collector_mac, msg).await {
            Ok(()) => {
                #[cfg(feature = "statistics")]
                {
                    STATS.tx_count.fetch_add(1, Ordering::Relaxed);
                    seq = seq.wrapping_add(1);
                }
            }
            Err(EspNowError::Error(EspNowInnerError::OutOfMemory) | EspNowError::SendFailed) => {
                match select(STOP_SIGNAL.wait(), Timer::after_micros(TX_BACKOFF_US)).await {
                    Either::First(_) => {
                        STOP_SIGNAL.signal(());
                        break;
                    }
                    Either::Second(_) => {}
                }
            }
            Err(e) => {
                log_ln!("Failed to send ESP-NOW flood packet: {:?}", e);
            }
        }

        // Optional rate cap.
        if let Some(interval) = cap_interval_us {
            next_tx_us = next_tx_us.saturating_add(interval);
            let now = Instant::now().as_micros();
            if next_tx_us > now {
                match select(STOP_SIGNAL.wait(), Timer::after_micros(next_tx_us - now)).await {
                    Either::First(_) => {
                        STOP_SIGNAL.signal(());
                        break;
                    }
                    Either::Second(_) => {}
                }
            }
        }
    }

    log_ln!("Node Stopped. Halting flood.");
}
