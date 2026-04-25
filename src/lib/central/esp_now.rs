use core::sync::atomic::Ordering;

use embassy_futures::select::{select, Either};
use embassy_time::Instant;
use embassy_time::Timer;
use heapless::LinearMap;

use crate::log_ln;
use crate::ControlPacket;
use crate::PeripheralPacket;
use crate::PERIPHERAL_MAGIC_NUMBER;
#[cfg(feature = "statistics")]
use crate::STATS;
use crate::STOP_SIGNAL;
use esp_radio::esp_now::{Error as EspNowInnerError, EspNow, EspNowError, PeerInfo, ReceivedData, BROADCAST_ADDRESS};
#[cfg(feature = "statistics")]
use portable_atomic::AtomicU64;

use crate::{EspNowConfig, IOTaskConfig};

const TX_BACKOFF_US: u64 = 200;
const TX_FAST_BACKOFF_US: u64 = 50;
const TX_WAIT_SLICE_US: u64 = 100;
const ADAPT_UP_EVERY_SUCCESSES: u16 = 32;
const ADAPT_DOWN_PERCENT: u64 = 25;
const ADAPT_UP_PERCENT: u64 = 10;
const MIN_TX_HZ_FLOOR: u64 = 100;
const MAX_TX_HZ_CEILING: u64 = 8_000;
const CONTROL_PACKET_BUF_LEN: usize = 24;
const TX_CATCH_UP_BURST: u8 = 3;
const TX_CATCH_UP_BURST_NO_WAIT: u8 = 16;
const RX_BURST_MAX_WITH_TX: u16 = 16;
const RX_RESERVED_TX_GUARD_US: u64 = 15;
const RX_TRACKED_PEERS_CAPACITY: usize = 16;

#[cfg(feature = "statistics")]
static TX_QUEUED_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "statistics")]
static TX_CONFIRMED_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "statistics")]
static TX_FAILED_COUNT: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "statistics")]
fn reset_tx_diagnostics() {
    TX_QUEUED_COUNT.store(0, Ordering::Relaxed);
    TX_CONFIRMED_COUNT.store(0, Ordering::Relaxed);
    TX_FAILED_COUNT.store(0, Ordering::Relaxed);
}

#[cfg(feature = "statistics")]
pub fn get_tx_queued_packets() -> u64 {
    TX_QUEUED_COUNT.load(Ordering::Relaxed)
}

#[cfg(feature = "statistics")]
pub fn get_tx_confirmed_packets() -> u64 {
    TX_CONFIRMED_COUNT.load(Ordering::Relaxed)
}

#[cfg(feature = "statistics")]
pub fn get_tx_failed_packets() -> u64 {
    TX_FAILED_COUNT.load(Ordering::Relaxed)
}

fn hz_to_interval_us(hz: u64) -> u64 {
    (1_000_000u64 / hz.max(1)).max(1)
}

fn handle_peripheral_packet(
    esp_now: &mut EspNow<'static>,
    r: ReceivedData,
    channel: u8,
    latency_offset: &mut i64,
    known_peers: &mut LinearMap<[u8; 6], (), RX_TRACKED_PEERS_CAPACITY>,
) {
    #[cfg(feature = "statistics")]
    let r_time = Instant::now().as_micros();

    let Ok(packet) = postcard::from_bytes::<PeripheralPacket>(r.data()) else {
        return;
    };

    if packet.magic_number != PERIPHERAL_MAGIC_NUMBER {
        return;
    }

    if known_peers.get(&r.info.src_address).is_none() {
        if !esp_now.peer_exists(&r.info.src_address) {
            let _ = esp_now.add_peer(PeerInfo {
                interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                peer_address: r.info.src_address,
                lmk: None,
                channel: Some(channel),
                encrypt: false,
            });
        }

        if known_peers.insert(r.info.src_address, ()).is_err() {
            known_peers.clear();
            let _ = known_peers.insert(r.info.src_address, ());
        }
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
    io_tasks: IOTaskConfig,
) {
    #[cfg(feature = "statistics")]
    reset_tx_diagnostics();

    let mut latency_offset: i64 = -1;
    let mut control_sequence: u32 = 0;
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
    // let mut tx_interval_us = hz_to_interval_us(adaptive_tx_hz);
    let mut tx_interval_us = if io_tasks.rx_enabled {
        hz_to_interval_us(adaptive_tx_hz)
    } else {
        hz_to_interval_us(freq)
    };
    let adaptive_pacing_enabled = io_tasks.rx_enabled && io_tasks.tx_enabled;
    let tx_fast_no_wait = io_tasks.tx_enabled && !io_tasks.rx_enabled;
    let mut consecutive_tx_ok: u16 = 0;
    let mut next_tx_us = Instant::now().as_micros().saturating_add(tx_interval_us);
    let mut tx_buf = [0u8; CONTROL_PACKET_BUF_LEN];
    let mut known_peers: LinearMap<[u8; 6], (), RX_TRACKED_PEERS_CAPACITY> = LinearMap::new();

    loop {
        let mut now_us = Instant::now().as_micros();
        if io_tasks.tx_enabled {
            let mut burst_budget = if tx_fast_no_wait {
                TX_CATCH_UP_BURST_NO_WAIT
            } else {
                TX_CATCH_UP_BURST
            };
            while now_us >= next_tx_us && burst_budget > 0 {
                burst_budget = burst_budget.saturating_sub(1);

                let control_packet = ControlPacket::new(is_collector, latency_offset, control_sequence);
                let message = match postcard::to_slice(&control_packet, &mut tx_buf) {
                    Ok(slice) => slice,
                    Err(_) => {
                        log_ln!("Failed to serialize ESP-NOW control packet");
                        break;
                    }
                };

                let mut send_succeeded = false;
                let mut packet_queued = false;
                if tx_fast_no_wait {
                    match esp_now.send(&BROADCAST_ADDRESS, message) {
                        Ok(waiter) => {
                            // Max-throughput mode: queue packets as fast as the
                            // driver accepts them and avoid per-packet callback waits.
                            core::mem::forget(waiter);
                            send_succeeded = true;
                            packet_queued = true;
                            #[cfg(feature = "statistics")]
                            {
                                STATS.tx_count.fetch_add(1, Ordering::Relaxed);
                                TX_QUEUED_COUNT.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(
                            EspNowError::Error(EspNowInnerError::OutOfMemory)
                            | EspNowError::SendFailed,
                        ) => {
                            #[cfg(feature = "statistics")]
                            TX_FAILED_COUNT.fetch_add(1, Ordering::Relaxed);
                            Timer::after_micros(TX_FAST_BACKOFF_US).await;
                        }
                        Err(e) => {
                            #[cfg(feature = "statistics")]
                            TX_FAILED_COUNT.fetch_add(1, Ordering::Relaxed);
                            log_ln!("Failed to queue ESP-NOW packet: {:?}", e);
                        }
                    }
                } else {
                    let send_result = match esp_now.send(&BROADCAST_ADDRESS, message) {
                        Ok(waiter) => {
                            packet_queued = true;
                            #[cfg(feature = "statistics")]
                            TX_QUEUED_COUNT.fetch_add(1, Ordering::Relaxed);
                            waiter.wait()
                        }
                        Err(e) => {
                            #[cfg(feature = "statistics")]
                            TX_FAILED_COUNT.fetch_add(1, Ordering::Relaxed);
                            Err(e)
                        }
                    };

                    match send_result {
                        Ok(()) => {
                            send_succeeded = true;
                            #[cfg(feature = "statistics")]
                            {
                                STATS.tx_count.fetch_add(1, Ordering::Relaxed);
                                TX_CONFIRMED_COUNT.fetch_add(1, Ordering::Relaxed);
                            }

                            if adaptive_pacing_enabled {
                                consecutive_tx_ok = consecutive_tx_ok.saturating_add(1);
                                if consecutive_tx_ok >= ADAPT_UP_EVERY_SUCCESSES {
                                    consecutive_tx_ok = 0;
                                    let step_up = (adaptive_tx_hz * ADAPT_UP_PERCENT / 100).max(1);
                                    adaptive_tx_hz = (adaptive_tx_hz + step_up).min(tx_hz_max);
                                    tx_interval_us = hz_to_interval_us(adaptive_tx_hz);
                                }
                            }
                        }
                        // Back off briefly when Wi-Fi TX buffers are full.
                        Err(
                            EspNowError::Error(EspNowInnerError::OutOfMemory)
                            | EspNowError::SendFailed,
                        ) => {
                            #[cfg(feature = "statistics")]
                            TX_FAILED_COUNT.fetch_add(1, Ordering::Relaxed);
                            consecutive_tx_ok = 0;
                            if adaptive_pacing_enabled {
                                let step_down = (adaptive_tx_hz * ADAPT_DOWN_PERCENT / 100).max(1);
                                adaptive_tx_hz = adaptive_tx_hz.saturating_sub(step_down).max(tx_hz_min);
                                tx_interval_us = hz_to_interval_us(adaptive_tx_hz);
                                Timer::after_micros(TX_BACKOFF_US).await;
                            }
                        }
                        Err(e) => {
                            #[cfg(feature = "statistics")]
                            TX_FAILED_COUNT.fetch_add(1, Ordering::Relaxed);
                            consecutive_tx_ok = 0;
                            log_ln!("Failed to send ESP-NOW packet: {:?}", e);
                        }
                    }
                }

                if packet_queued {
                    control_sequence = control_sequence.wrapping_add(1);
                }

                // Keep periodic phase from the previous deadline to avoid adding
                // an extra full interval after each blocking send(). If we're
                // behind, send again as soon as possible in this same loop.
                next_tx_us = next_tx_us.saturating_add(tx_interval_us);
                now_us = Instant::now().as_micros();

                if !send_succeeded {
                    break;
                }
            }
        }

        // Drain a bounded RX burst after the TX step so TX deadlines stay
        // prioritized at higher rates.
        if io_tasks.rx_enabled {
            let rx_deadline_us = if io_tasks.tx_enabled {
                next_tx_us.saturating_sub(RX_RESERVED_TX_GUARD_US)
            } else {
                u64::MAX
            };
            let rx_burst_drain_limit = if io_tasks.tx_enabled {
                RX_BURST_MAX_WITH_TX
            } else {
                u16::MAX
            };

            let mut rx_packets: u16 = 0;
            while rx_packets < rx_burst_drain_limit {
                if io_tasks.tx_enabled && Instant::now().as_micros() >= rx_deadline_us {
                    break;
                }

                let Some(r) = esp_now.receive() else {
                    break;
                };

                handle_peripheral_packet(
                    esp_now,
                    r,
                    config.channel,
                    &mut latency_offset,
                    &mut known_peers,
                );
                rx_packets = rx_packets.saturating_add(1);
            }

            // If there is more RX work and TX is not immediately due, loop
            // again without sleeping to reduce queueing latency.
            if rx_packets > 0 {
                if !io_tasks.tx_enabled || Instant::now().as_micros() < next_tx_us {
                    continue;
                }
            }
        }

        let wait_us = if io_tasks.tx_enabled {
            let until_tx_us = next_tx_us.saturating_sub(Instant::now().as_micros());
            let slice_div = if io_tasks.rx_enabled { 8 } else { 4 };
            let slice_us = (tx_interval_us / slice_div).clamp(1, TX_WAIT_SLICE_US);
            until_tx_us.min(slice_us).max(1)
        } else if io_tasks.rx_enabled {
            20
        } else {
            1
        };
        match select(STOP_SIGNAL.wait(), Timer::after_micros(wait_us)).await {
            Either::First(_) => {
                log_ln!("STOP signal received, shutting down responder...");
                STOP_SIGNAL.signal(());
                break;
            }
            Either::Second(_) => {}
        }
    }

    // When this finishes (e.g. Stop Signal), the split parts are dropped.
    // The borrow on 'esp_now' ends, and it is ready to be used again!
}
