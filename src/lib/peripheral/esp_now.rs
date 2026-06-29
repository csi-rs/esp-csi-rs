//! Peripheral-side ESP-NOW driver task.
//!
//! Receives [`crate::ControlPacket`] frames from the central, mirrors the
//! collector mode advertised by the central, and replies with a
//! [`crate::PeripheralPacket`] presence beacon. Operates against the lock-free
//! [`crate::esp_now_pool`] receive queue to avoid heap churn in the
//! ESP-NOW interrupt path.

use core::sync::atomic::Ordering;

use crate::CENTRAL_MAGIC_NUMBER;
use crate::ControlPacket;
use crate::IS_COLLECTOR;
use crate::PERIPHERAL_BEACON_SENTINEL;
use crate::PERIPHERAL_MAGIC_NUMBER;
use crate::PeripheralPacket;
#[cfg(feature = "statistics")]
use crate::STATS;
use crate::STOP_SIGNAL;
use crate::log_ln;
use crate::parse_with_magic;
use crate::serialize_with_magic;
use crate::set_runtime_collection_mode;

use crate::espnow_phy::{apply_peer_espnow_phy, with_espnow_recv_suspended};
use embassy_futures::select::{Either, select};
use embassy_futures::yield_now;
use embassy_time::Instant;
use embassy_time::Timer;
use esp_radio::esp_now::{
    BROADCAST_ADDRESS, Error as EspNowInnerError, EspNow, EspNowError, PeerInfo,
};

use crate::esp_now_pool::PoolFrame;

#[cfg(feature = "statistics")]
use portable_atomic::AtomicU32;
use portable_atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64};

use crate::{EspNowConfig, IOTaskConfig};

const TX_BACKOFF_US: u64 = 200;
const TX_WAIT_SLICE_US: u64 = 100;
const ADAPT_UP_EVERY_SUCCESSES: u16 = 32;
const ADAPT_DOWN_PERCENT: u64 = 25;
const ADAPT_UP_PERCENT: u64 = 10;
const MIN_REPLY_HZ_FLOOR: u64 = 100;
const MAX_REPLY_HZ_CEILING: u64 = 8_000;
const PERIPHERAL_PACKET_BUF_LEN: usize = 8;
const TX_CATCH_UP_BURST: u8 = 1;
const RX_BURST_MAX_WITH_TX: u16 = 1;
const RX_RESERVED_TX_GUARD_US: u64 = 15;
const PEER_HEALTHCHECK_PERIOD: u16 = 256;
// Cap any single drain burst so an arbitrarily large RX queue cannot keep the
// loop in the synchronous drain without an executor turn. Sized to absorb a
// full 802.11n AMPDU in one pass (max 64 subframes) so we don't bounce in/out
// of yield_now mid-aggregate and let the driver overrun.
const RX_BURST_MAX_RX_ONLY: u16 = 64;
// Number of consecutive packets that must agree on a mode before applying it.
// Filters out single-packet noise/corruption flips.
const MODE_SWITCH_HYSTERESIS: u8 = 3;

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
static RX_CONTROL_PACKET_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "statistics")]
static RX_TX_GUARD_BREAK_COUNT: AtomicU64 = AtomicU64::new(0);

/// Raw-listen mode (CPU-benchmark use). When enabled, the responder still
/// drains received ESP-NOW frames from the pool but `ingest_control_packet`
/// returns immediately — skipping the postcard deserialize, magic/source
/// checks, sequence/timestamp bookkeeping and mode hysteresis. This is the
/// fair match to the ESP-IDF reference's empty receive callback. See
/// [`set_raw_listen`].
static RAW_LISTEN: AtomicBool = AtomicBool::new(false);

/// Enable/disable raw-listen mode — see [`RAW_LISTEN`]. Set this before the
/// node starts so no control packets are ingested during the run.
///
/// Also enables pool raw-drop ([`crate::esp_now_pool::set_raw_drop`]) so the
/// receive callback discards frames inline without waking the responder task —
/// removing the per-frame context switch so the RX path matches the ESP-IDF
/// reference's empty inline `recv_cb`. The `ingest_control_packet` early-return
/// below is then a harmless backstop (the responder never receives a frame).
pub fn set_raw_listen(enabled: bool) {
    RAW_LISTEN.store(enabled, Ordering::Relaxed);
    crate::esp_now_pool::set_raw_drop(enabled);
}

#[cfg(feature = "statistics")]
fn reset_rx_diagnostics() {
    RX_PARSE_FAIL_COUNT.store(0, Ordering::Relaxed);
    RX_MAGIC_DROP_COUNT.store(0, Ordering::Relaxed);
    RX_SOURCE_FILTER_DROP_COUNT.store(0, Ordering::Relaxed);
    RX_PEER_ADD_FAIL_COUNT.store(0, Ordering::Relaxed);
    RX_SEQUENCE_MISS_COUNT.store(0, Ordering::Relaxed);
    RX_CONTROL_PACKET_COUNT.store(0, Ordering::Relaxed);
    RX_TX_GUARD_BREAK_COUNT.store(0, Ordering::Relaxed);
}

/// Returns the number of received ESP-NOW frames that failed
/// `ControlPacket` deserialization (postcard parse error).
#[cfg(feature = "statistics")]
pub fn get_rx_parse_fail_packets() -> u64 {
    RX_PARSE_FAIL_COUNT.load(Ordering::Relaxed)
}

/// Returns the number of received frames dropped because the magic
/// number did not match [`crate::CENTRAL_MAGIC_NUMBER`].
#[cfg(feature = "statistics")]
pub fn get_rx_magic_drop_packets() -> u64 {
    RX_MAGIC_DROP_COUNT.load(Ordering::Relaxed)
}

/// Returns the number of received frames dropped because the source MAC
/// did not match the currently locked-in central peer.
#[cfg(feature = "statistics")]
pub fn get_rx_source_filter_drop_packets() -> u64 {
    RX_SOURCE_FILTER_DROP_COUNT.load(Ordering::Relaxed)
}

/// Returns the number of times adding the central as an ESP-NOW peer
/// failed (e.g., peer table full).
#[cfg(feature = "statistics")]
pub fn get_rx_peer_add_fail_packets() -> u64 {
    RX_PEER_ADD_FAIL_COUNT.load(Ordering::Relaxed)
}

/// Returns the number of detected gaps in the central's
/// `sequence_number` (drops or reordering).
#[cfg(feature = "statistics")]
pub fn get_rx_sequence_miss_packets() -> u64 {
    RX_SEQUENCE_MISS_COUNT.load(Ordering::Relaxed)
}

/// Returns the number of valid control packets received from the central
/// (passed source/parse/magic checks). Together with
/// [`get_rx_sequence_miss_packets`] this gives the sequence-gap drop rate:
/// `missed / (received + missed)`.
#[cfg(feature = "statistics")]
pub fn get_rx_control_packets() -> u64 {
    RX_CONTROL_PACKET_COUNT.load(Ordering::Relaxed)
}

/// Returns the number of times an RX completed inside the TX guard
/// window — i.e. the peripheral's reply landed before the central had
/// finished its previous transmit slot.
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

/// Whether the peripheral replies by unicast to the auto-learned central MAC
/// (vs. broadcast). The central MAC is always discovered from the first control
/// frame in auto-pairing mode — unicast here never requires a configured MAC.
///
/// - **HT40** always unicasts: a per-peer HT40 rate config only applies to a
///   unicast peer, never the broadcast peer.
/// - **C5 HT20 with forced PHY** also unicasts. `esp_now_set_peer_rate_config`
///   on the broadcast peer wedges the C5 dual-band Wi-Fi ISR, so forced MCS/HT20
///   can only be applied to a learned unicast peer. This is what lets a C5
///   central collect CSI automatically (OFDM replies) without hardcoded MACs.
/// - **Non-C5 HT20** stays broadcast: the broadcast peer accepts forced PHY
///   there, and broadcast is more robust for an unassociated started-STA central.
fn unicast_replies(config: &EspNowConfig) -> bool {
    if config.secondary_channel().is_some() {
        return true;
    }
    #[cfg(feature = "esp32c5")]
    {
        config.force_phy()
    }
    #[cfg(not(feature = "esp32c5"))]
    {
        false
    }
}

fn apply_central_peer_phy(config: &EspNowConfig, central_mac: &[u8; 6]) {
    if !config.force_phy() {
        return;
    }
    if unicast_replies(config) {
        apply_peer_espnow_phy(central_mac, *config.phy_rate(), config.secondary_channel());
    }
}

fn reply_destination(shared: &Shared, config: &EspNowConfig) -> [u8; 6] {
    if unicast_replies(config) {
        u64_to_mac(shared.central_mac.load(Ordering::Relaxed))
    } else {
        BROADCAST_ADDRESS
    }
}

fn register_central_peer(
    esp_now: &EspNow<'static>,
    channel: u8,
    config: &EspNowConfig,
    central_mac: [u8; 6],
) -> bool {
    let add_res = if esp_now.peer_exists(&central_mac) {
        Ok(())
    } else {
        esp_now.add_peer(PeerInfo {
            interface: esp_radio::esp_now::EspNowWifiInterface::Station,
            peer_address: central_mac,
            lmk: None,
            channel: Some(channel),
            encrypt: false,
        })
    };

    match add_res {
        Ok(()) => {
            apply_central_peer_phy(config, &central_mac);
            true
        }
        Err(_) => {
            #[cfg(feature = "statistics")]
            RX_PEER_ADD_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

/// Shared responder state.
struct Shared {
    is_connected: AtomicBool,
    is_collector: AtomicBool,
    central_mac: AtomicU64,
    peer_healthcheck_counter: AtomicU16,
    #[cfg(feature = "statistics")]
    last_control_sequence: AtomicU32,
    #[cfg(feature = "statistics")]
    sequence_initialized: AtomicBool,
    /// Raised when a valid control packet has been ingested and a presence
    /// beacon reply is owed to the central.
    pending_flag: AtomicBool,
    /// Last `!packet.is_collector` value seen; used to detect direction changes.
    last_central_is_listener: AtomicBool,
    /// Consecutive-packet streak counter for mode-switch hysteresis.
    mode_streak: AtomicU8,
}

/// Parse and ingest one received control packet into responder state.
///
/// This takes `&EspNow` because it only needs peer-management helpers.
fn ingest_control_packet(
    esp_now: &EspNow<'static>,
    channel: u8,
    config: &EspNowConfig,
    r: PoolFrame,
    shared: &Shared,
    tx_enabled: bool,
    peer_mac: Option<[u8; 6]>,
) {
    // Raw-listen (CPU-benchmark): the frame `r` has already been drained from
    // the pool by the caller; returning here discards it without any parse /
    // validation / bookkeeping — the fair match to the IDF empty recv callback.
    if RAW_LISTEN.load(Ordering::Relaxed) {
        return;
    }

    let is_connected = shared.is_connected.load(Ordering::Acquire);
    if is_connected {
        let expected = u64_to_mac(shared.central_mac.load(Ordering::Relaxed));
        if expected != r.info.src_address {
            #[cfg(feature = "statistics")]
            RX_SOURCE_FILTER_DROP_COUNT.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }

    // Magic is only on the wire in auto-pairing mode; in manual mode the
    // source-MAC filter above is the discriminator.
    let expect_magic = peer_mac.is_none();
    let Some(packet) =
        parse_with_magic::<ControlPacket>(r.data(), CENTRAL_MAGIC_NUMBER, expect_magic)
    else {
        #[cfg(feature = "statistics")]
        if expect_magic {
            // A mismatched magic prefix and a postcard error are indistinguishable
            // here; count it as a magic drop in auto mode, parse failure otherwise.
            RX_MAGIC_DROP_COUNT.fetch_add(1, Ordering::Relaxed);
        } else {
            RX_PARSE_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        return;
    };

    #[cfg(feature = "statistics")]
    {
        RX_CONTROL_PACKET_COUNT.fetch_add(1, Ordering::Relaxed);
        if shared.sequence_initialized.load(Ordering::Acquire) {
            let last_seq = shared.last_control_sequence.load(Ordering::Relaxed);
            // 64-bit signed gap so a central reboot (sequence restarting at
            // 0) or a reordered frame shows up as `gap <= 0` and resyncs
            // silently instead of registering ~2^32 misses. The trade-off is
            // that a genuine u32 sequence wrap also resyncs, but that takes
            // 2^32 packets while reboots are routine.
            let gap = packet.sequence_number as i64 - last_seq as i64;
            if gap > 1 {
                RX_SEQUENCE_MISS_COUNT.fetch_add((gap - 1) as u64, Ordering::Relaxed);
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
        if !is_connected {
            // Lock onto the first valid central and add it as a unicast peer.
            if register_central_peer(esp_now, channel, config, r.info.src_address) {
                shared
                    .central_mac
                    .store(mac_to_u64(&r.info.src_address), Ordering::Relaxed);
                shared.peer_healthcheck_counter.store(0, Ordering::Relaxed);
                shared.is_connected.store(true, Ordering::Release);
                if unicast_replies(config) {
                    log_ln!(
                        "ESP-NOW peripheral: locked central {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, unicast forced-PHY replies enabled (rate {:?})",
                        r.info.src_address[0],
                        r.info.src_address[1],
                        r.info.src_address[2],
                        r.info.src_address[3],
                        r.info.src_address[4],
                        r.info.src_address[5],
                        config.phy_rate()
                    );
                } else {
                    log_ln!(
                        "ESP-NOW peripheral: locked central {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, broadcast HT20 replies",
                        r.info.src_address[0],
                        r.info.src_address[1],
                        r.info.src_address[2],
                        r.info.src_address[3],
                        r.info.src_address[4],
                        r.info.src_address[5]
                    );
                }
            }
        } else {
            // Driver-side peer table can churn under pressure; keep unicast peer
            // present so TX doesn't get stuck on recurring NotFound. Checking on
            // every frame is expensive, so sample periodically.
            let expected = u64_to_mac(shared.central_mac.load(Ordering::Relaxed));
            let check_counter = shared
                .peer_healthcheck_counter
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            if (check_counter & (PEER_HEALTHCHECK_PERIOD - 1)) == 0 {
                let _ = register_central_peer(esp_now, channel, config, expected);
            }
        }

        // Raise the pending flag so the TX step emits a presence beacon reply.
        shared.pending_flag.store(true, Ordering::Release);
    }

    // Peripheral mode policy: only follow the central when it becomes a listener,
    // ensuring someone is always collecting. When central is a collector, the
    // peripheral keeps its configured mode — both nodes can collect simultaneously.
    //
    // Hysteresis: require MODE_SWITCH_HYSTERESIS consecutive packets with the same
    // value before acting, so a single noise-corrupted packet cannot flip the mode.
    let central_is_listener = !packet.is_collector;
    let prev_seen = shared.last_central_is_listener.load(Ordering::Relaxed);

    if central_is_listener != prev_seen {
        shared
            .last_central_is_listener
            .store(central_is_listener, Ordering::Relaxed);
        shared.mode_streak.store(1, Ordering::Relaxed);
    } else {
        let streak = shared.mode_streak.load(Ordering::Relaxed).saturating_add(1);
        shared.mode_streak.store(streak, Ordering::Relaxed);

        if streak == MODE_SWITCH_HYSTERESIS && central_is_listener {
            // Central has consistently been a listener → switch peripheral to collector.
            if !shared.is_collector.load(Ordering::Relaxed) {
                set_runtime_collection_mode(true);
                shared.is_collector.store(true, Ordering::Relaxed);
            }
        }
        // When central is consistently a collector, do NOT force peripheral to
        // listener — peripheral and central are allowed to both collect.
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
    // In HT40 mode the channel (+secondary) was already set on the controller
    // before this task ran; calling esp_now.set_channel here would reset the
    // secondary to HT20, so skip it.
    if config.secondary_channel().is_none() {
        with_espnow_recv_suspended(|| {
            esp_now.set_channel(config.channel).unwrap();
        });
    }
    // Forced PHY on the broadcast peer (HT20 only): skipped on C5 — see central/esp_now.rs.
    // HT40 applies forced PHY to the learned central unicast peer inside `responder`.
    #[cfg(not(feature = "esp32c5"))]
    if config.force_phy() && config.secondary_channel().is_none() {
        crate::set_peer_espnow_phy(
            &BROADCAST_ADDRESS,
            *config.phy_rate(),
            config.secondary_channel(),
        );
    }
    log_ln!("esp-now version {}", esp_now.version().unwrap());

    // The static-pool `rcv_cb` is installed in `lib::run` before `set_csi`,
    // so by the time we reach this function ESP-NOW receives are already
    // landing in BSS slots rather than the heap-backed `VecDeque`.

    let freq = match freq_hz {
        Some(freq) => freq as u64,
        None => u16::MAX as u64,
    };

    #[cfg(feature = "statistics")]
    reset_rx_diagnostics();

    responder(esp_now, config, freq, io_tasks).await;
}

/// Run a single sequential responder loop: receive/process first, then send.
///
/// RX and TX intentionally do not run concurrently in this mode.
async fn responder(
    esp_now: &mut EspNow<'static>,
    config: &EspNowConfig,
    frequency_hz: u64,
    io_tasks: IOTaskConfig,
) {
    let channel = config.channel;
    let peer_mac = config.peer_mac();
    let send_magic = peer_mac.is_none();
    let shared = Shared {
        // Manual pairing skips the "lock onto first valid central" handshake:
        // pre-seed the configured peer so source-MAC filtering applies from the
        // very first frame.
        is_connected: AtomicBool::new(peer_mac.is_some()),
        is_collector: AtomicBool::new(IS_COLLECTOR.load(Ordering::Relaxed)),
        central_mac: AtomicU64::new(peer_mac.map(|m| mac_to_u64(&m)).unwrap_or(0)),
        peer_healthcheck_counter: AtomicU16::new(0),
        #[cfg(feature = "statistics")]
        last_control_sequence: AtomicU32::new(0),
        #[cfg(feature = "statistics")]
        sequence_initialized: AtomicBool::new(false),
        pending_flag: AtomicBool::new(false),
        last_central_is_listener: AtomicBool::new(false),
        mode_streak: AtomicU8::new(0),
    };

    // In manual mode, register the central as a unicast peer up front so the
    // first reply can be sent without waiting to discover it.
    if let Some(mac) = peer_mac
        && io_tasks.tx_enabled
        && !register_central_peer(esp_now, channel, config, mac)
    {
        // register_central_peer already counted the failure.
    }

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
    let mut consecutive_tx_ok: u16 = 0;
    let mut next_tx_us = Instant::now().as_micros().saturating_add(tx_interval_us);
    let mut tx_buf = [0u8; PERIPHERAL_PACKET_BUF_LEN];

    loop {
        // Service queued control packets before attempting a due reply. In
        // bidirectional mode this prevents TX from overtaking already-received
        // central control frames; the burst cap and deadline guard keep replies
        // from being starved by a busy RX queue.
        if io_tasks.rx_enabled && io_tasks.tx_enabled {
            let mut rx_packets: u16 = 0;
            while rx_packets < RX_BURST_MAX_WITH_TX {
                let tx_reply_pending = shared.is_connected.load(Ordering::Acquire)
                    && shared.pending_flag.load(Ordering::Acquire);
                let rx_deadline_us = if tx_reply_pending {
                    next_tx_us.saturating_sub(RX_RESERVED_TX_GUARD_US)
                } else {
                    u64::MAX
                };

                if rx_packets > 0 && Instant::now().as_micros() >= rx_deadline_us {
                    #[cfg(feature = "statistics")]
                    RX_TX_GUARD_BREAK_COUNT.fetch_add(1, Ordering::Relaxed);
                    break;
                }

                let Some(r) = crate::esp_now_pool::receive() else {
                    break;
                };

                ingest_control_packet(esp_now, channel, config, r, &shared, true, peer_mac);
                rx_packets = rx_packets.saturating_add(1);
            }

            if rx_packets > 0 {
                yield_now().await;
            }
        }
        let mut now_us = Instant::now().as_micros();

        if io_tasks.tx_enabled {
            let mut burst_budget = TX_CATCH_UP_BURST;
            while now_us >= next_tx_us
                && burst_budget > 0
                && shared.is_connected.load(Ordering::Acquire)
                && shared.pending_flag.swap(false, Ordering::Acquire)
            {
                burst_budget = burst_budget.saturating_sub(1);

                // HT20 (e.g. C6 ch 11): reply via broadcast so an unassociated
                // started-STA central surfaces CSI reliably. HT40 (e.g. C5 ch
                // 149+153): unicast to the learned central MAC — broadcast
                // forced PHY wedges the C5 dual-band path; per-peer HT40 applies
                // only to that unicast peer (see `apply_central_peer_phy`).
                let dest_mac = reply_destination(&shared, config);

                // Auto mode: send the 4-byte magic prefix (empty beacon body).
                // Manual mode: a single sentinel byte so the frame isn't empty.
                let message: &[u8] = if send_magic {
                    match serialize_with_magic(
                        &PeripheralPacket::new(),
                        PERIPHERAL_MAGIC_NUMBER,
                        true,
                        &mut tx_buf,
                    ) {
                        Ok(slice) => slice,
                        Err(_) => {
                            log_ln!("Failed to serialize ESP-NOW peripheral packet");
                            break;
                        }
                    }
                } else {
                    tx_buf[0] = PERIPHERAL_BEACON_SENTINEL;
                    &tx_buf[..1]
                };

                let mut send_succeeded = false;
                // esp-radio has one global ESP-NOW send-completion flag/waker.
                // Keep exactly one send in flight and await it to completion;
                // dropping this future or queueing another send first can corrupt
                // the driver's completion state and freeze later sends.
                match esp_now.send_async(&dest_mac, message).await {
                    Ok(()) => {
                            send_succeeded = true;
                            #[cfg(feature = "statistics")]
                            STATS.tx_count.fetch_add(1, Ordering::Relaxed);

                            if adaptive_pacing_enabled {
                                consecutive_tx_ok = consecutive_tx_ok.saturating_add(1);
                                if consecutive_tx_ok >= ADAPT_UP_EVERY_SUCCESSES {
                                    consecutive_tx_ok = 0;
                                    let step_up =
                                        (adaptive_reply_hz * ADAPT_UP_PERCENT / 100).max(1);
                                    adaptive_reply_hz =
                                        (adaptive_reply_hz + step_up).min(reply_hz_max);
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
                                let step_down =
                                    (adaptive_reply_hz * ADAPT_DOWN_PERCENT / 100).max(1);
                                adaptive_reply_hz = adaptive_reply_hz
                                    .saturating_sub(step_down)
                                    .max(reply_hz_min);
                                tx_interval_us = hz_to_interval_us(adaptive_reply_hz);
                                Timer::after_micros(TX_BACKOFF_US).await;
                            }
                    }
                    Err(e) => {
                            consecutive_tx_ok = 0;
                            log_ln!("Failed to send ESP-NOW packet: {:?}", e);
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

        if !io_tasks.tx_enabled && io_tasks.rx_enabled {
            // RX-only: block until the driver ISR wakes us with the next frame.
            // This eliminates the polling sleep and its variable wake-up latency.
            match select(STOP_SIGNAL.wait(), crate::esp_now_pool::receive_async()).await {
                Either::First(_) => {
                    log_ln!("STOP signal received, shutting down responder...");
                    STOP_SIGNAL.signal(());
                    break;
                }
                Either::Second(r) => {
                    ingest_control_packet(esp_now, channel, config, r, &shared, false, peer_mac);
                    // Drain any frames that stacked up while we were processing.
                    // Bound the synchronous drain so a sustained inflow can't
                    // hold the executor here past the next loop iteration.
                    let mut drained: u16 = 0;
                    while drained < RX_BURST_MAX_RX_ONLY {
                        let Some(r) = crate::esp_now_pool::receive() else {
                            break;
                        };
                        ingest_control_packet(
                            esp_now, channel, config, r, &shared, false, peer_mac,
                        );
                        drained = drained.saturating_add(1);
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
