use core::sync::atomic::Ordering;

use embassy_futures::select::select3;
use embassy_futures::select::Either3;
use embassy_time::Duration;
use embassy_time::Instant;
use embassy_time::Timer;
use heapless::LinearMap;
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

fn handle_peripheral_packet(
    esp_now: &mut EspNow<'static>,
    r: ReceivedData,
    channel: u8,
    peripheral_offsets: &mut LinearMap<[u8; 6], i64, 8>,
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
            let _ = peripheral_offsets.insert(r.info.src_address, *latency_offset);

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
    let mut peripheral_offsets: LinearMap<[u8; 6], i64, 8> = LinearMap::new();
    // Configure
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());

    let freq = match frequency_hz {
        Some(freq) => u64::from(freq.max(1)),
        None => u16::MAX as u64,
    };

    let tx_interval = Duration::from_hz(freq);

    loop {
        // Drain queued packets first so RX does not get starved by high TX rates.
        while let Some(r) = esp_now.receive() {
            handle_peripheral_packet(
                esp_now,
                r,
                config.channel,
                &mut peripheral_offsets,
                &mut latency_offset,
            );
        }

        match select3(
            STOP_SIGNAL.wait(),
            esp_now.receive_async(),
            Timer::after(tx_interval),
        )
        .await
        {
            Either3::First(_) => {
                // Stop signal received, exit the loop
                STOP_SIGNAL.signal(());
                break;
            }
            Either3::Second(r) => {
                handle_peripheral_packet(
                    esp_now,
                    r,
                    config.channel,
                    &mut peripheral_offsets,
                    &mut latency_offset,
                );
            }
            Either3::Third(_) => {
                let control_packet = ControlPacket::new(is_collector, latency_offset);
                let message_u8: Vec<u8, 16> = postcard::to_vec(&control_packet).unwrap();
                match esp_now.send_async(&BROADCAST_ADDRESS, &message_u8).await {
                    Ok(()) => {
                        #[cfg(feature = "statistics")]
                        STATS.tx_count.fetch_add(1, Ordering::Relaxed);
                    }
                    // Back off briefly when Wi-Fi TX buffers are full.
                    Err(EspNowError::Error(EspNowInnerError::OutOfMemory)
                    | EspNowError::SendFailed) => {
                        Timer::after_micros(TX_BACKOFF_US).await;
                    }
                    Err(_) => {}
                }
            }
        }
    }

    // When this finishes (e.g. Stop Signal), the split parts are dropped.
    // The borrow on 'esp_now' ends, and it is ready to be used again!
}
