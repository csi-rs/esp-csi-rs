use core::sync::atomic::Ordering;

use embassy_futures::select::{select, Either};
use embassy_time::Instant;
use embassy_time::Timer;
use heapless::Vec;

use crate::log_ln;
use crate::ControlPacket;
use crate::PeripheralPacket;
use crate::PERIPHERAL_MAGIC_NUMBER;
#[cfg(feature = "statistics")]
use crate::STATS;
use crate::STOP_SIGNAL;
use esp_radio::esp_now::{Error as EspNowInnerError, EspNow, EspNowError, PeerInfo, ReceivedData, BROADCAST_ADDRESS};

use crate::EspNowConfig;

const TX_BACKOFF_US: u64 = 200;
const RX_BURST_DRAIN_LIMIT: u8 = 1;
const TX_WAIT_SLICE_US: u64 = 200;
const ADAPT_UP_EVERY_SUCCESSES: u16 = 32;
const ADAPT_DOWN_PERCENT: u64 = 25;
const ADAPT_UP_PERCENT: u64 = 10;
const MIN_TX_HZ_FLOOR: u64 = 100;
const MAX_TX_HZ_CEILING: u64 = 2_000;

fn hz_to_interval_us(hz: u64) -> u64 {
    (1_000_000u64 / hz.max(1)).max(1)
}

fn handle_peripheral_packet(
    esp_now: &mut EspNow<'static>,
    r: ReceivedData,
    channel: u8,
    latency_offset: &mut i64,
) {
    #[cfg(feature = "statistics")]
    let r_time = Instant::now().as_micros();

    let Ok(packet) = postcard::from_bytes::<PeripheralPacket>(r.data()) else {
        return;
    };

    if packet.magic_number != PERIPHERAL_MAGIC_NUMBER {
        return;
    }

    if !esp_now.peer_exists(&r.info.src_address) {
        let _ = esp_now.add_peer(PeerInfo {
            interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
            peer_address: r.info.src_address,
            lmk: None,
            channel: Some(channel),
            encrypt: false,
        });
    }

    #[cfg(feature = "statistics")]
    {
        let rtt = r_time.saturating_sub(packet.central_send_uptime);
        // Sanity check: ignore delays > 1s
        if rtt > 0 && rtt < 1_000_000 {
            *latency_offset =
                packet.recv_uptime as i64 - (packet.central_send_uptime + rtt / 2) as i64;

            let total_elapsed = r_time.saturating_sub(packet.central_send_uptime);
            let b_processing_delay = packet.send_uptime.saturating_sub(packet.recv_uptime);
            let two_way_latency = (total_elapsed.saturating_sub(b_processing_delay)) as i64;
            let one_way_latency =
                (r_time as i64 - (packet.send_uptime as i64 - *latency_offset)) as i64;
            STATS
                .two_way_latency
                .store(two_way_latency, Ordering::Relaxed);
            STATS
                .one_way_latency
                .store(one_way_latency, Ordering::Relaxed);
        }
    }
}

/// Run ESP-NOW in Central mode, broadcasting control packets and handling replies.
///
/// This task periodically sends `ControlPacket` broadcasts at the specified
/// frequency, processes `PeripheralPacket` replies, and updates statistics
/// when the `statistics` feature is enabled.
pub async fn run_esp_now_central(
    esp_now: &mut EspNow<'static>, // Borrow the hardware
    _mac_addr: [u8; 6],
    config: &EspNowConfig,
    frequency_hz: Option<u16>,
    is_collector: bool,
) {
    let mut latency_offset: i64 = -1;
    // Configure
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());

    let freq = match frequency_hz {
        Some(freq) => u64::from(freq.max(1)),
        None => u16::MAX as u64,
    };

    // Adaptive control pacing: start from configured target and automatically
    // back off under TX pressure, then slowly climb back up on stable sends.
    let tx_hz_max = freq.clamp(1, MAX_TX_HZ_CEILING);
    let tx_hz_min = (tx_hz_max / 8).max(MIN_TX_HZ_FLOOR).min(tx_hz_max);
    let mut adaptive_tx_hz = tx_hz_max;
    let mut tx_interval_us = hz_to_interval_us(adaptive_tx_hz);
    let mut consecutive_tx_ok: u16 = 0;
    let mut next_tx_us = Instant::now().as_micros().saturating_add(tx_interval_us);

    loop {
        let now_us = Instant::now().as_micros();
        let tx_due = now_us >= next_tx_us;
        if tx_due {
            let control_packet = ControlPacket::new(is_collector, latency_offset);
            let message_u8: Vec<u8, 16> = postcard::to_vec(&control_packet).unwrap();

            let send_result = match esp_now.send(&BROADCAST_ADDRESS, &message_u8) {
                Ok(waiter) => waiter.wait(),
                Err(e) => Err(e),
            };

            match send_result {
                Ok(()) => {
                    #[cfg(feature = "statistics")]
                    STATS.tx_count.fetch_add(1, Ordering::Relaxed);

                    consecutive_tx_ok = consecutive_tx_ok.saturating_add(1);
                    if consecutive_tx_ok >= ADAPT_UP_EVERY_SUCCESSES {
                        consecutive_tx_ok = 0;
                        let step_up = (adaptive_tx_hz * ADAPT_UP_PERCENT / 100).max(1);
                        adaptive_tx_hz = (adaptive_tx_hz + step_up).min(tx_hz_max);
                        tx_interval_us = hz_to_interval_us(adaptive_tx_hz);
                    }
                }
                // Back off briefly when Wi-Fi TX buffers are full.
                Err(EspNowError::Error(EspNowInnerError::OutOfMemory) | EspNowError::SendFailed) => {
                    consecutive_tx_ok = 0;
                    let step_down = (adaptive_tx_hz * ADAPT_DOWN_PERCENT / 100).max(1);
                    adaptive_tx_hz = adaptive_tx_hz
                        .saturating_sub(step_down)
                        .max(tx_hz_min);
                    tx_interval_us = hz_to_interval_us(adaptive_tx_hz);
                    Timer::after_micros(TX_BACKOFF_US).await;
                }
                Err(e) => {
                    consecutive_tx_ok = 0;
                    log_ln!("Failed to send ESP-NOW packet: {:?}", e);
                }
            }

            // Keep periodic phase from the previous deadline to avoid adding
            // an extra full interval after each blocking send(). If we're
            // behind, send again as soon as possible on the next loop.
            next_tx_us = next_tx_us.saturating_add(tx_interval_us);
            let now_after_tx = Instant::now().as_micros();
            if next_tx_us <= now_after_tx {
                next_tx_us = now_after_tx.saturating_add(1);
            }
        }

        // Drain a bounded RX burst after the TX step so TX deadlines stay
        // prioritized at higher rates.
        let mut rx_packets: u8 = 0;
        while rx_packets < RX_BURST_DRAIN_LIMIT {
            let Some(r) = esp_now.receive() else {
                break;
            };

            handle_peripheral_packet(esp_now, r, config.channel, &mut latency_offset);
            rx_packets = rx_packets.saturating_add(1);
        }

        let now_us = Instant::now().as_micros();
        let until_tx_us = next_tx_us.saturating_sub(now_us);
        let wait_us = until_tx_us.min(TX_WAIT_SLICE_US).max(1);
        match select(STOP_SIGNAL.wait(), Timer::after_micros(wait_us)).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either::Second(_) => {}
        }
    }

    // When this finishes (e.g. Stop Signal), the split parts are dropped.
    // The borrow on 'esp_now' ends, and it is ready to be used again!
}
