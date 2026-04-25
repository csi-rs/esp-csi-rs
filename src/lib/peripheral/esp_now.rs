use core::sync::atomic::Ordering;

use crate::log_ln;
use crate::set_runtime_collection_mode;
use crate::ControlPacket;
use crate::PeripheralPacket;
use crate::CENTRAL_MAGIC_NUMBER;
use crate::IS_COLLECTOR;
#[cfg(feature = "statistics")]
use crate::STATS;
use crate::STOP_SIGNAL;

use embassy_futures::select::{select, Either};
use embassy_time::Instant;
use embassy_time::Timer;
use esp_radio::esp_now::{Error as EspNowInnerError, EspNow, EspNowError, PeerInfo, ReceivedData};

use portable_atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64};

use crate::{EspNowConfig, IOTaskConfig};

const TX_BACKOFF_US: u64 = 200;
const TX_FAST_BACKOFF_US: u64 = 50;
const TX_WAIT_SLICE_US: u64 = 100;
const ADAPT_UP_EVERY_SUCCESSES: u16 = 32;
const ADAPT_DOWN_PERCENT: u64 = 25;
const ADAPT_UP_PERCENT: u64 = 10;
const MIN_REPLY_HZ_FLOOR: u64 = 100;
const MAX_REPLY_HZ_CEILING: u64 = 8_000;
const PERIPHERAL_PACKET_BUF_LEN: usize = 32;
const TX_CATCH_UP_BURST: u8 = 3;
const TX_CATCH_UP_BURST_NO_WAIT: u8 = 16;
const RX_BURST_MAX_WITH_TX: u16 = 16;
const RX_RESERVED_TX_GUARD_US: u64 = 15;
const PEER_HEALTHCHECK_PERIOD: u16 = 256;

#[cfg(feature = "statistics")]
static RX_PARSE_FAIL_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "statistics")]
static RX_MAGIC_DROP_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "statistics")]
static RX_SOURCE_FILTER_DROP_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "statistics")]
static RX_PEER_ADD_FAIL_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "statistics")]
static RX_SEQUENCE_MISS_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "statistics")]
static RX_TX_GUARD_BREAK_COUNT: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "statistics")]
fn reset_rx_diagnostics() {
    RX_PARSE_FAIL_COUNT.store(0, Ordering::Relaxed);
    RX_MAGIC_DROP_COUNT.store(0, Ordering::Relaxed);
    RX_SOURCE_FILTER_DROP_COUNT.store(0, Ordering::Relaxed);
    RX_PEER_ADD_FAIL_COUNT.store(0, Ordering::Relaxed);
    RX_SEQUENCE_MISS_COUNT.store(0, Ordering::Relaxed);
    RX_TX_GUARD_BREAK_COUNT.store(0, Ordering::Relaxed);
}

#[cfg(feature = "statistics")]
pub fn get_rx_parse_fail_packets() -> u64 {
    RX_PARSE_FAIL_COUNT.load(Ordering::Relaxed)
}

#[cfg(feature = "statistics")]
pub fn get_rx_magic_drop_packets() -> u64 {
    RX_MAGIC_DROP_COUNT.load(Ordering::Relaxed)
}

#[cfg(feature = "statistics")]
pub fn get_rx_source_filter_drop_packets() -> u64 {
    RX_SOURCE_FILTER_DROP_COUNT.load(Ordering::Relaxed)
}

#[cfg(feature = "statistics")]
pub fn get_rx_peer_add_fail_packets() -> u64 {
    RX_PEER_ADD_FAIL_COUNT.load(Ordering::Relaxed)
}

#[cfg(feature = "statistics")]
pub fn get_rx_sequence_miss_packets() -> u64 {
    RX_SEQUENCE_MISS_COUNT.load(Ordering::Relaxed)
}

#[cfg(feature = "statistics")]
pub fn get_rx_tx_guard_breaks() -> u64 {
    RX_TX_GUARD_BREAK_COUNT.load(Ordering::Relaxed)
}

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
    peer_healthcheck_counter: AtomicU16,
    last_control_sequence: AtomicU32,
    sequence_initialized: AtomicBool,
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
    tx_enabled: bool,
) {
    let is_connected = shared.is_connected.load(Ordering::Acquire);
    if is_connected {
        let expected = u64_to_mac(shared.central_mac.load(Ordering::Relaxed));
        if expected != r.info.src_address {
            #[cfg(feature = "statistics")]
            RX_SOURCE_FILTER_DROP_COUNT.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }

    let Ok(packet) = postcard::from_bytes::<ControlPacket>(r.data()) else {
        #[cfg(feature = "statistics")]
        RX_PARSE_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if packet.magic_number != CENTRAL_MAGIC_NUMBER {
        #[cfg(feature = "statistics")]
        RX_MAGIC_DROP_COUNT.fetch_add(1, Ordering::Relaxed);
        return;
    }

    #[cfg(feature = "statistics")]
    {
        if shared.sequence_initialized.load(Ordering::Acquire) {
            let last_seq = shared.last_control_sequence.load(Ordering::Relaxed);
            let diff = packet.sequence_number.wrapping_sub(last_seq);
            if diff > 1 {
                RX_SEQUENCE_MISS_COUNT.fetch_add((diff - 1) as u64, Ordering::Relaxed);
            }
        } else {
            shared.sequence_initialized.store(true, Ordering::Release);
        }
        shared
            .last_control_sequence
            .store(packet.sequence_number, Ordering::Relaxed);
    }

    if tx_enabled {
        // Peer table management is only needed so we can send unicast replies.
        let recv_time = Instant::now().as_micros();

        if !is_connected {
            // Lock onto the first valid central and add it as a unicast peer.
            let add_res = esp_now.add_peer(PeerInfo {
                interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                peer_address: r.info.src_address,
                lmk: None,
                channel: Some(channel),
                encrypt: false,
            });
            if add_res.is_err() {
                #[cfg(feature = "statistics")]
                RX_PEER_ADD_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
            }
            shared
                .central_mac
                .store(mac_to_u64(&r.info.src_address), Ordering::Relaxed);
            shared.peer_healthcheck_counter.store(0, Ordering::Relaxed);
            shared.is_connected.store(true, Ordering::Release);
        } else {
            // Driver-side peer table can churn under pressure; keep unicast peer
            // present so TX doesn't get stuck on recurring NotFound. Checking on
            // every frame is expensive, so sample periodically.
            let expected = u64_to_mac(shared.central_mac.load(Ordering::Relaxed));
            let check_counter = shared
                .peer_healthcheck_counter
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            if (check_counter & (PEER_HEALTHCHECK_PERIOD - 1)) == 0
                && !esp_now.peer_exists(&expected)
            {
                if esp_now
                    .add_peer(PeerInfo {
                        interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                        peer_address: expected,
                        lmk: None,
                        channel: Some(channel),
                        encrypt: false,
                    })
                    .is_err()
                {
                    #[cfg(feature = "statistics")]
                    RX_PEER_ADD_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                }
            }
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
        shared.pending_recv_time.store(recv_time, Ordering::Relaxed);
        shared
            .pending_csu
            .store(packet.central_send_uptime, Ordering::Relaxed);
        shared.pending_flag.store(true, Ordering::Release);
    }

    #[cfg(feature = "statistics")]
    STATS.rx_count.fetch_add(1, Ordering::Relaxed);

    // Keep central/peripheral roles complementary.
    let desired_collector = !packet.is_collector;
    let prev = shared.is_collector.load(Ordering::Relaxed);
    if desired_collector != prev {
        set_runtime_collection_mode(desired_collector);
        shared
            .is_collector
            .store(desired_collector, Ordering::Relaxed);
    }
}

/// Run ESP-NOW in Peripheral mode.
///
/// Configures the channel and starts the responder loop that listens for
/// `ControlPacket`s from a Central node and reply with `PeripheralPacket`s.
pub async fn run_esp_now_peripheral(
    esp_now: &mut EspNow<'static>,
    config: &EspNowConfig,
    freq_hz: Option<u16>,
    io_tasks: IOTaskConfig,
) {
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());

    let freq = match freq_hz {
        Some(freq) => freq as u64,
        None => u16::MAX as u64,
    };

    #[cfg(feature = "statistics")]
    reset_rx_diagnostics();

    responder(esp_now, config.channel, freq, io_tasks).await;
}

/// Run a single sequential responder loop: receive/process first, then send.
///
/// RX and TX intentionally do not run concurrently in this mode.
async fn responder(
    esp_now: &mut EspNow<'static>,
    channel: u8,
    frequency_hz: u64,
    io_tasks: IOTaskConfig,
) {
    let shared = Shared {
        is_connected: AtomicBool::new(false),
        is_collector: AtomicBool::new(IS_COLLECTOR.load(Ordering::Relaxed)),
        central_mac: AtomicU64::new(0),
        peer_healthcheck_counter: AtomicU16::new(0),
        last_control_sequence: AtomicU32::new(0),
        sequence_initialized: AtomicBool::new(false),
        pending_recv_time: AtomicU64::new(0),
        pending_csu: AtomicU64::new(0),
        pending_flag: AtomicBool::new(false),
    };

    // Adaptive reply pacing: start from configured target and automatically
    // back off under TX pressure, then slowly climb back up on stable sends.
    let reply_hz_max = frequency_hz.clamp(1, MAX_REPLY_HZ_CEILING);
    let reply_hz_min = (reply_hz_max / 8).max(MIN_REPLY_HZ_FLOOR).min(reply_hz_max);
    let mut adaptive_reply_hz = reply_hz_max;
    let mut tx_interval_us = if io_tasks.rx_enabled {
        hz_to_interval_us(adaptive_reply_hz)
    } else {
        hz_to_interval_us(frequency_hz)
    };
    let adaptive_pacing_enabled = io_tasks.rx_enabled && io_tasks.tx_enabled;
    let tx_fast_no_wait = io_tasks.tx_enabled && !io_tasks.rx_enabled;
    let mut consecutive_tx_ok: u16 = 0;
    let mut next_tx_us = Instant::now().as_micros().saturating_add(tx_interval_us);
    let mut tx_buf = [0u8; PERIPHERAL_PACKET_BUF_LEN];

    loop {
        let mut now_us = Instant::now().as_micros();
        if io_tasks.tx_enabled {
            let mut burst_budget = if tx_fast_no_wait {
                TX_CATCH_UP_BURST_NO_WAIT
            } else {
                TX_CATCH_UP_BURST
            };
            while now_us >= next_tx_us
                && burst_budget > 0
                && shared.is_connected.load(Ordering::Acquire)
                && shared.pending_flag.swap(false, Ordering::Acquire)
            {
                burst_budget = burst_budget.saturating_sub(1);

                let recv_time = shared.pending_recv_time.load(Ordering::Relaxed);
                let csu = shared.pending_csu.load(Ordering::Relaxed);
                let central_mac = u64_to_mac(shared.central_mac.load(Ordering::Relaxed));

                let peripheral_packet = PeripheralPacket::new(recv_time, csu);
                let message = match postcard::to_slice(&peripheral_packet, &mut tx_buf) {
                    Ok(slice) => slice,
                    Err(_) => {
                        log_ln!("Failed to serialize ESP-NOW peripheral packet");
                        break;
                    }
                };

                let mut send_succeeded = false;
                if tx_fast_no_wait {
                    match esp_now.send(&central_mac, message) {
                        Ok(waiter) => {
                            // Max-throughput mode: queue packets as fast as the
                            // driver accepts them and avoid per-packet callback waits.
                            core::mem::forget(waiter);
                            send_succeeded = true;
                            #[cfg(feature = "statistics")]
                            STATS.tx_count.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(
                            EspNowError::Error(EspNowInnerError::OutOfMemory)
                            | EspNowError::SendFailed,
                        ) => {
                            Timer::after_micros(TX_FAST_BACKOFF_US).await;
                        }
                        Err(e) => {
                            log_ln!("Failed to queue ESP-NOW packet: {:?}", e);
                        }
                    }
                } else {
                    let send_result = match esp_now.send(&central_mac, message) {
                        Ok(waiter) => waiter.wait(),
                        Err(e) => Err(e),
                    };

                    match send_result {
                        Ok(()) => {
                            send_succeeded = true;
                            #[cfg(feature = "statistics")]
                            STATS.tx_count.fetch_add(1, Ordering::Relaxed);

                            if adaptive_pacing_enabled {
                                consecutive_tx_ok = consecutive_tx_ok.saturating_add(1);
                                if consecutive_tx_ok >= ADAPT_UP_EVERY_SUCCESSES {
                                    consecutive_tx_ok = 0;
                                    let step_up = (adaptive_reply_hz * ADAPT_UP_PERCENT / 100).max(1);
                                    adaptive_reply_hz = (adaptive_reply_hz + step_up).min(reply_hz_max);
                                    tx_interval_us = hz_to_interval_us(adaptive_reply_hz);
                                }
                            }
                        }
                        Err(
                            EspNowError::Error(EspNowInnerError::OutOfMemory)
                            | EspNowError::SendFailed,
                        ) => {
                            consecutive_tx_ok = 0;
                            if adaptive_pacing_enabled {
                                let step_down = (adaptive_reply_hz * ADAPT_DOWN_PERCENT / 100).max(1);
                                adaptive_reply_hz =
                                    adaptive_reply_hz.saturating_sub(step_down).max(reply_hz_min);
                                tx_interval_us = hz_to_interval_us(adaptive_reply_hz);
                                Timer::after_micros(TX_BACKOFF_US).await;
                            }
                        }
                        Err(e) => {
                            consecutive_tx_ok = 0;
                            log_ln!("Failed to send ESP-NOW packet: {:?}", e);
                        }
                    }
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
            let tx_reply_pending = io_tasks.tx_enabled
                && shared.is_connected.load(Ordering::Acquire)
                && shared.pending_flag.load(Ordering::Acquire);

            let rx_deadline_us = if tx_reply_pending {
                next_tx_us.saturating_sub(RX_RESERVED_TX_GUARD_US)
            } else {
                u64::MAX
            };
            let rx_burst_drain_limit = if tx_reply_pending {
                RX_BURST_MAX_WITH_TX
            } else {
                u16::MAX
            };

            let mut rx_packets: u16 = 0;
            while rx_packets < rx_burst_drain_limit {
                if tx_reply_pending && Instant::now().as_micros() >= rx_deadline_us {
                    #[cfg(feature = "statistics")]
                    RX_TX_GUARD_BREAK_COUNT.fetch_add(1, Ordering::Relaxed);
                    break;
                }

                let Some(r) = esp_now.receive() else {
                    break;
                };

                ingest_control_packet(esp_now, channel, r, &shared, io_tasks.tx_enabled);
                rx_packets = rx_packets.saturating_add(1);
            }

            // If there is more RX work and TX is not immediately due, loop
            // again without sleeping to reduce queueing latency.
            if rx_packets > 0 {
                if !tx_reply_pending || Instant::now().as_micros() < next_tx_us {
                    continue;
                }
            }
        }

        if !io_tasks.tx_enabled && io_tasks.rx_enabled {
            // RX-only: block until the driver ISR wakes us with the next frame.
            // This eliminates the polling sleep and its variable wake-up latency.
            match select(STOP_SIGNAL.wait(), esp_now.receive_async()).await {
                Either::First(_) => {
                    log_ln!("STOP signal received, shutting down responder...");
                    STOP_SIGNAL.signal(());
                    break;
                }
                Either::Second(r) => {
                    ingest_control_packet(esp_now, channel, r, &shared, false);
                    // Drain any frames that stacked up while we were processing.
                    while let Some(r) = esp_now.receive() {
                        ingest_control_packet(esp_now, channel, r, &shared, false);
                    }
                }
            }
        } else {
            let tx_reply_pending = io_tasks.tx_enabled
                && shared.is_connected.load(Ordering::Acquire)
                && shared.pending_flag.load(Ordering::Acquire);

            let wait_us = if tx_reply_pending {
                let until_tx_us = next_tx_us.saturating_sub(Instant::now().as_micros());
                let slice_div = if io_tasks.rx_enabled { 8 } else { 4 };
                let slice_us = (tx_interval_us / slice_div).clamp(1, TX_WAIT_SLICE_US);
                until_tx_us.min(slice_us).max(1)
            } else if io_tasks.rx_enabled {
                1
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
    }

    log_ln!("Node Stopped. Halting CSI Sending.");
}
