use core::sync::atomic::Ordering;

#[cfg(feature = "statistics")]
use crate::STATS;
use crate::log_ln;
use crate::set_runtime_collection_mode;
use crate::ControlPacket;
use crate::PeripheralPacket;
use crate::CENTRAL_MAGIC_NUMBER;
use crate::IS_COLLECTOR;
use crate::STOP_SIGNAL;

use embassy_futures::select::{select, Either};
use embassy_time::Instant;
use embassy_time::Timer;
use esp_radio::esp_now::{Error as EspNowInnerError, EspNow, EspNowError, PeerInfo, ReceivedData};

use heapless::Vec;
use portable_atomic::{AtomicBool, AtomicU64};

use crate::EspNowConfig;

const TX_BACKOFF_US: u64 = 200;
const RX_BURST_DRAIN_LIMIT: u8 = 1;
const TX_WAIT_SLICE_US: u64 = 200;
const ADAPT_UP_EVERY_SUCCESSES: u16 = 32;
const ADAPT_DOWN_PERCENT: u64 = 25;
const ADAPT_UP_PERCENT: u64 = 10;
const MIN_REPLY_HZ_FLOOR: u64 = 100;
const MAX_REPLY_HZ_CEILING: u64 = 2_000;

fn hz_to_interval_us(hz: u64) -> u64 {
    (1_000_000u64 / hz.max(1)).max(1)
}

/// Pack a 6-byte MAC into the low 48 bits of a `u64` so it can live in a
/// single atomic without a mutex on the TX/RX hot path.
fn mac_to_u64(mac: &[u8; 6]) -> u64 {
    (mac[0] as u64)
        | ((mac[1] as u64) << 8)
        | ((mac[2] as u64) << 16)
        | ((mac[3] as u64) << 24)
        | ((mac[4] as u64) << 32)
        | ((mac[5] as u64) << 40)
}

fn u64_to_mac(v: u64) -> [u8; 6] {
    [
        (v & 0xFF) as u8,
        ((v >> 8) & 0xFF) as u8,
        ((v >> 16) & 0xFF) as u8,
        ((v >> 24) & 0xFF) as u8,
        ((v >> 32) & 0xFF) as u8,
        ((v >> 40) & 0xFF) as u8,
    ]
}

/// Shared responder state.
struct Shared {
    is_connected: AtomicBool,
    is_collector: AtomicBool,
    central_mac: AtomicU64,
    pending_recv_time: AtomicU64,
    pending_csu: AtomicU64,
    pending_flag: AtomicBool,
}

/// Parse and ingest one received control packet into responder state.
///
/// This takes `&EspNow` because it only needs peer-management helpers.
fn ingest_control_packet(
    esp_now: &EspNow<'static>,
    channel: u8,
    r: ReceivedData,
    shared: &Shared,
) {
    let Ok(packet) = postcard::from_bytes::<ControlPacket>(r.data()) else {
        return;
    };
    if packet.magic_number != CENTRAL_MAGIC_NUMBER {
        return;
    }

    let recv_time = Instant::now().as_micros();

    if !shared.is_connected.load(Ordering::Acquire) {
        // Lock onto the first valid central and add it as a unicast peer.
        let add_res = esp_now.add_peer(PeerInfo {
            interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
            peer_address: r.info.src_address,
            lmk: None,
            channel: Some(channel),
            encrypt: false,
        });
        if add_res.is_err() {
            return;
        }
        shared
            .central_mac
            .store(mac_to_u64(&r.info.src_address), Ordering::Relaxed);
        shared.is_connected.store(true, Ordering::Release);
    } else {
        // Ignore frames from any sender other than the locked-on central.
        let expected = u64_to_mac(shared.central_mac.load(Ordering::Relaxed));
        if expected != r.info.src_address {
            log_ln!(
                "Received control packet from unexpected peer {:02X?}, expected {:02X?}",
                r.info.src_address,
                expected
            );
            return;
        }

        // Driver-side peer table can churn under pressure; keep unicast peer
        // present so TX doesn't get stuck on recurring NotFound.
        if !esp_now.peer_exists(&expected) {
            let _ = esp_now.add_peer(PeerInfo {
                interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                peer_address: expected,
                lmk: None,
                channel: Some(channel),
                encrypt: false,
            });
        }
    }

    #[cfg(feature = "statistics")]
    STATS.rx_count.fetch_add(1, Ordering::Relaxed);

    // Keep central/peripheral roles complementary.
    let desired_collector = !packet.is_collector;
    let prev = shared.is_collector.load(Ordering::Relaxed);
    if desired_collector != prev {
        set_runtime_collection_mode(desired_collector);
        shared.is_collector.store(desired_collector, Ordering::Relaxed);
    }

    #[cfg(feature = "statistics")]
    if packet.latency_offset != -1 {
        let one_way_latency =
            recv_time as i64 - (packet.central_send_uptime as i64 + packet.latency_offset);
        STATS
            .one_way_latency
            .store(one_way_latency, Ordering::Relaxed);
    }

    // Publish timestamps then raise pending flag for the TX step.
    shared
        .pending_recv_time
        .store(recv_time, Ordering::Relaxed);
    shared
        .pending_csu
        .store(packet.central_send_uptime, Ordering::Relaxed);
    shared.pending_flag.store(true, Ordering::Release);
}

/// Run ESP-NOW in Peripheral mode.
///
/// Configures the channel and starts the responder loop that listens for
/// `ControlPacket`s from a Central node and reply with `PeripheralPacket`s.
pub async fn run_esp_now_peripheral(
    esp_now: &mut EspNow<'static>,
    config: &EspNowConfig,
    freq_hz: Option<u16>,
) {
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());

    let freq = match freq_hz {
        Some(freq) => freq as u64,
        None => u16::MAX as u64,
    };

    responder(esp_now, config.channel, freq).await;
}

/// Run a single sequential responder loop: receive/process first, then send.
///
/// RX and TX intentionally do not run concurrently in this mode.
async fn responder(esp_now: &mut EspNow<'static>, channel: u8, frequency_hz: u64) {
    let shared = Shared {
        is_connected: AtomicBool::new(false),
        is_collector: AtomicBool::new(IS_COLLECTOR.load(Ordering::Relaxed)),
        central_mac: AtomicU64::new(0),
        pending_recv_time: AtomicU64::new(0),
        pending_csu: AtomicU64::new(0),
        pending_flag: AtomicBool::new(false),
    };

    // Adaptive reply pacing: start from configured target and automatically
    // back off under TX pressure, then slowly climb back up on stable sends.
    let reply_hz_max = frequency_hz.clamp(1, MAX_REPLY_HZ_CEILING);
    let reply_hz_min = (reply_hz_max / 8).max(MIN_REPLY_HZ_FLOOR).min(reply_hz_max);
    let mut adaptive_reply_hz = reply_hz_max;
    let mut tx_interval_us = hz_to_interval_us(adaptive_reply_hz);
    let mut consecutive_tx_ok: u16 = 0;
    let mut next_tx_us = Instant::now().as_micros().saturating_add(tx_interval_us);

    loop {
        let now_us = Instant::now().as_micros();
        let tx_due = now_us >= next_tx_us;
        if tx_due
            && shared.is_connected.load(Ordering::Acquire)
            && shared.pending_flag.swap(false, Ordering::Acquire)
        {
            let recv_time = shared.pending_recv_time.load(Ordering::Relaxed);
            let csu = shared.pending_csu.load(Ordering::Relaxed);
            let central_mac = u64_to_mac(shared.central_mac.load(Ordering::Relaxed));

            let peripheral_packet = PeripheralPacket::new(recv_time, csu);
            let message_u8: Vec<u8, 32> = postcard::to_vec(&peripheral_packet).unwrap();

            let send_result = match esp_now.send(&central_mac, &message_u8) {
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
                        let step_up = (adaptive_reply_hz * ADAPT_UP_PERCENT / 100).max(1);
                        adaptive_reply_hz = (adaptive_reply_hz + step_up).min(reply_hz_max);
                        tx_interval_us = hz_to_interval_us(adaptive_reply_hz);
                    }
                }
                Err(EspNowError::Error(EspNowInnerError::OutOfMemory) | EspNowError::SendFailed) => {
                    consecutive_tx_ok = 0;
                    let step_down = (adaptive_reply_hz * ADAPT_DOWN_PERCENT / 100).max(1);
                    adaptive_reply_hz = adaptive_reply_hz
                        .saturating_sub(step_down)
                        .max(reply_hz_min);
                    tx_interval_us = hz_to_interval_us(adaptive_reply_hz);
                    Timer::after_micros(TX_BACKOFF_US).await;
                    log_ln!("TX buffer full, backing off for {} us", TX_BACKOFF_US);
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

            ingest_control_packet(esp_now, channel, r, &shared);
            rx_packets = rx_packets.saturating_add(1);
        }

        let now_us = Instant::now().as_micros();
        let until_tx_us = next_tx_us.saturating_sub(now_us);
        let wait_us = until_tx_us.min(TX_WAIT_SLICE_US).max(1);
        match select(STOP_SIGNAL.wait(), Timer::after_micros(wait_us)).await {
            Either::First(_) => {
                log_ln!("STOP signal received, shutting down responder...");
                STOP_SIGNAL.signal(());
                break;
            }
            Either::Second(_) => {}
        }
    }

    log_ln!("Node Stopped. Halting CSI Sending.");
}
