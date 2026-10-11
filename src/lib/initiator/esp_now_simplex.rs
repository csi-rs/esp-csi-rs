//! The **source** end of the ESP-NOW simplex exchange: an initiator and sensing transmitter that
//! reports nothing.
//!
//! Built by [`SimplexConfig::source`](crate::SimplexConfig::source) /
//! [`CsiNode::esp_now_simplex_source`](crate::CsiNode::esp_now_simplex_source).
//!
//! The source listens for the [peer](crate::SimplexConfig::peer)'s discovery beacon, learns the
//! peer's MAC, registers it as a **unicast peer with a forced PHY** (HT20 MCS7-LGI by default,
//! HT40 when a secondary channel is set), sends one magic-tagged hello so the peer switches to
//! RX-only, then unicasts a **continuous max-rate flood**. Unlike the symmetric exchange's
//! responder there is no per-control-packet reply gating and no adaptive pacing — the source
//! transmits as fast as `send_async` completes (or at the periodicity, when one is set),
//! which with the peer silent drives the maximum CSI packets/sec at the peer.

use embassy_futures::select::{Either, select};
use embassy_time::{Instant, Timer};

use esp_radio::esp_now::{
    Error as EspNowInnerError, EspNow, EspNowError, EspNowWifiInterface, PeerInfo, WifiPhyRate,
};

use crate::espnow_phy::{PeerPhy, with_espnow_recv_suspended};
use crate::{
    INITIATOR_MAGIC_NUMBER, ControlPacket, EspNowConfig, IOTaskConfig, RESPONDER_MAGIC_NUMBER,
    ResponderPacket, STOP_SIGNAL, apply_peer_espnow_phy, log_ln, parse_with_magic,
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

fn add_simplex_peer(esp_now: &EspNow, mac: &[u8; 6], channel: u8) -> bool {
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

/// Run the simplex source end: discover the peer → forced-PHY unicast flood.
pub async fn run_esp_now_simplex_source(
    esp_now: &mut EspNow,
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
    // user forced a specific rate. Applied to the learned peer below.
    let rate = if config.force_phy() {
        *config.phy_rate()
    } else {
        WifiPhyRate::RateMcs7Lgi
    };

    // ---- Phase 1: discover the peer ------------------------------------------
    let peer_addr: [u8; 6] = match peer_mac {
        Some(mac) => mac,
        None => loop {
            match select(STOP_SIGNAL.wait(), crate::esp_now_pool::receive_async()).await {
                Either::First(_) => {
                    log_ln!("STOP signal received, shutting down simplex source...");
                    STOP_SIGNAL.signal(());
                    return;
                }
                Either::Second(r) => {
                    if parse_with_magic::<ControlPacket>(r.data(), INITIATOR_MAGIC_NUMBER, true)
                        .is_some()
                    {
                        break r.info.src_address;
                    }
                }
            }
        },
    };

    // Register the peer as a unicast peer and force the PHY (recv-suspended,
    // C5-safe — per-peer rate config on a unicast peer works on all chips).
    add_simplex_peer(esp_now, &peer_addr, config.channel);
    apply_peer_espnow_phy(esp_now, &peer_addr, PeerPhy { rate, ..config.peer_phy() });
    log_ln!(
        "ESP-NOW simplex source: locked peer {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, unicast flood rate {:?}",
        peer_addr[0],
        peer_addr[1],
        peer_addr[2],
        peer_addr[3],
        peer_addr[4],
        peer_addr[5],
        rate,
    );

    // One magic-tagged hello so the peer deterministically stops beaconing
    // and switches to RX-only (auto-pairing only; manual mode pre-latches).
    if send_magic {
        let mut hello_buf = [0u8; 8];
        if let Ok(hello) = serialize_with_magic(
            &ResponderPacket::new(),
            RESPONDER_MAGIC_NUMBER,
            true,
            &mut hello_buf,
        ) {
            let _ = esp_now.send_async(&peer_addr, hello).await;
        }
    }

    // ---- Phase 2: continuous flood -----------------------------------------
    // Optional rate cap (None = flat-out; the send_async completion is the
    // natural limiter as the radio frees TX buffers).
    let cap_interval_us = freq_hz.map(|f| 1_000_000u64 / (f.max(1) as u64));
    let mut next_tx_us = Instant::now().as_micros();
    let mut seq: u32 = 0;
    let mut tx_buf = [0u8; FLOOD_BUF_LEN];
    let mut healthcheck: u16 = 0;

    loop {
        if STOP_SIGNAL.signaled() {
            log_ln!("STOP signal received, shutting down simplex source...");
            STOP_SIGNAL.signal(());
            break;
        }

        // Periodic peer healthcheck — the driver peer table can churn under
        // pressure; keep the unicast peer present so TX doesn't stall on NotFound.
        healthcheck = healthcheck.wrapping_add(1);
        if (healthcheck & (PEER_HEALTHCHECK_PERIOD - 1)) == 0 && !esp_now.peer_exists(&peer_addr)
        {
            add_simplex_peer(esp_now, &peer_addr, config.channel);
            apply_peer_espnow_phy(esp_now, &peer_addr, PeerPhy { rate, ..config.peer_phy() });
        }

        let pkt = ControlPacket::new(false, seq);
        let msg = match serialize_with_magic(&pkt, INITIATOR_MAGIC_NUMBER, send_magic, &mut tx_buf) {
            Ok(m) => m,
            Err(_) => {
                log_ln!("Failed to serialize ESP-NOW flood packet");
                break;
            }
        };

        // Exactly one send in flight, awaited to completion — never drop or
        // queue a second send first (the driver has one global completion flag).
        match esp_now.send_async(&peer_addr, msg).await {
            Ok(()) => {
                seq = seq.wrapping_add(1);
                #[cfg(feature = "statistics")]
                STATS.tx_count.fetch_add(1, Ordering::Relaxed);
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
