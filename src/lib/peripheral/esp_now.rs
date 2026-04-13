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

use embassy_futures::select::select3;
use embassy_futures::select::Either3;
use embassy_time::Duration;
use embassy_time::Instant;
use embassy_time::Timer;
use embassy_time::Ticker;
use esp_radio::esp_now::{Error as EspNowInnerError, EspNow, EspNowError, PeerInfo, ReceivedData};

use heapless::Vec;

use crate::EspNowConfig;

const TX_BACKOFF_US: u64 = 200;
const MAX_RX_DRAIN_PER_LOOP: usize = 8;

fn handle_control_packet(
    esp_now: &mut EspNow<'static>,
    channel: u8,
    r: ReceivedData,
    central_mac: &mut [u8; 6],
    is_connected: &mut bool,
    is_collector: &mut bool,
    pending_reply: &mut Option<(u64, u64)>,
) {
    let Ok(packet) = postcard::from_bytes::<ControlPacket>(r.data()) else {
        return;
    };

    let recv_time = Instant::now().as_micros();

    if packet.magic_number != CENTRAL_MAGIC_NUMBER {
        return;
    }

    if !*is_connected {
        let _ = esp_now.add_peer(PeerInfo {
            interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
            peer_address: r.info.src_address,
            lmk: None,
            channel: Some(channel),
            encrypt: false,
        });
        *central_mac = r.info.src_address;
        *is_connected = true;
    }

    if *central_mac != r.info.src_address {
        return;
    }

    #[cfg(feature = "statistics")]
    STATS.rx_count.fetch_add(1, Ordering::Relaxed);

    // Keep central/peripheral roles complementary.
    let desired_collector = !packet.is_collector;
    if desired_collector != *is_collector {
        set_runtime_collection_mode(desired_collector);
        *is_collector = desired_collector;
    }

    #[cfg(feature = "statistics")]
    if packet.latency_offset != -1 {
        let one_way_latency =
            recv_time as i64 - (packet.central_send_uptime as i64 + packet.latency_offset);
        STATS.one_way_latency.store(one_way_latency, Ordering::Relaxed);
    }

    // Keep the latest control timestamps and respond on the next TX tick.
    *pending_reply = Some((recv_time, packet.central_send_uptime));
}

/// Run ESP-NOW in Peripheral mode.
///
/// Configures the channel and starts the responder loop that listens for
/// `ControlPacket`s from a Central node and replies with `PeripheralPacket`s.
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

/// Responder loop that handles ESP-NOW control packets and sends replies.
async fn responder(esp_now: &mut EspNow<'static>, channel: u8, frequency_hz: u64) {
    let mut is_collector = IS_COLLECTOR.load(Ordering::Relaxed);
    let tx_interval = Duration::from_hz(frequency_hz.max(1));
    let mut tx_ticker = Ticker::every(tx_interval);
    let mut pending_reply: Option<(u64, u64)> = None;
    let mut central_mac: [u8; 6] = [0; 6];
    let mut is_connected = false;

    loop {
        // Drain a bounded number of queued RX frames so TX ticks are not starved.
        for _ in 0..MAX_RX_DRAIN_PER_LOOP {
            if let Some(r) = esp_now.receive() {
                handle_control_packet(
                    esp_now,
                    channel,
                    r,
                    &mut central_mac,
                    &mut is_connected,
                    &mut is_collector,
                    &mut pending_reply,
                );
            } else {
                break;
            }
        }

        match select3(STOP_SIGNAL.wait(), tx_ticker.next(), esp_now.receive_async()).await {
            Either3::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either3::Second(_) => {
                if is_connected {
                    if let Some((recv_time, central_send_uptime)) = pending_reply {
                        let peripheral_packet =
                            PeripheralPacket::new(recv_time, central_send_uptime.into());
                        let message_u8: Vec<u8, 32> = postcard::to_vec(&peripheral_packet).unwrap();
                        match esp_now.send_async(&central_mac, &message_u8).await {
                            Ok(()) => {
                                pending_reply = None;
                                #[cfg(feature = "statistics")]
                                STATS.tx_count.fetch_add(1, Ordering::Relaxed);
                            }
                            // Back off briefly when Wi-Fi TX buffers are full.
                            Err(EspNowError::Error(EspNowInnerError::OutOfMemory)
                            | EspNowError::SendFailed) => {
                                Timer::after_micros(TX_BACKOFF_US).await;
                            }
                            Err(_) => {
                                pending_reply = None;
                            }
                        }
                    }
                }
            }
            Either3::Third(r) => {
                handle_control_packet(
                    esp_now,
                    channel,
                    r,
                    &mut central_mac,
                    &mut is_connected,
                    &mut is_collector,
                    &mut pending_reply,
                );
            }
        }
    }
    log_ln!("Node Stopped. Halting CSI Sending.");
}
