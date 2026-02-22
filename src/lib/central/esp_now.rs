use core::sync::atomic::Ordering;

use embassy_futures::select::select3;
use embassy_futures::select::Either3;
use embassy_time::with_timeout;
use embassy_time::Instant;
use embassy_time::Timer;
use heapless::LinearMap;
use heapless::Vec;
use zerocopy::FromBytes;
use zerocopy::IntoBytes;

use crate::log_ln;
use crate::ControlPacket;
use crate::PeripheralPacket;
use crate::PERIPHERAL_MAGIC_NUMBER;
#[cfg(feature = "statistics")]
use crate::STATS;
use crate::STOP_SIGNAL;
use esp_radio::esp_now::{EspNow, BROADCAST_ADDRESS};

use embassy_time::Duration;

use crate::EspNowConfig;

/// Run ESP-NOW in Central mode, broadcasting control packets and handling replies.
///
/// This task periodically sends `ControlPacket` broadcasts at the specified
/// frequency, processes `PeripheralPacket` replies, and updates statistics
/// when the `statistics` feature is enabled.
pub async fn run_esp_now_central(
    esp_now: &mut EspNow<'static>, // Borrow the hardware
    mac_addr: [u8; 6],
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
        Some(freq) => freq as u64,
        None => u16::MAX as u64,
    };

    loop {
        // let current_timestamp = embassy_time::Instant::now();
        match select3(
            STOP_SIGNAL.wait(),
            Timer::after(Duration::from_hz(freq)),
            esp_now.receive_async(),
        )
        .await
        {
            Either3::First(_) => {
                // Stop signal received, exit the loop
                STOP_SIGNAL.signal(());
                break;
            }
            Either3::Second(_) => {
                let control_packet = ControlPacket::new(is_collector, latency_offset);
                let message_u8: Vec<u8, 16> = postcard::to_vec(&control_packet).unwrap();
                let res = esp_now.send_async(&BROADCAST_ADDRESS, &message_u8).await;
                #[cfg(feature = "statistics")]
                if res.is_ok() {
                    STATS.tx_count.fetch_add(1, Ordering::Relaxed);
                }
            }
            Either3::Third(r) => {
                #[cfg(feature = "statistics")]
                let r_time = Instant::now().as_micros();
                let res = postcard::from_bytes::<PeripheralPacket>(r.data());
                match res {
                    Ok(packet) => {
                        if packet.magic_number == PERIPHERAL_MAGIC_NUMBER {
                            if !esp_now.peer_exists(&r.info.src_address) {
                                let _ = esp_now.add_peer(esp_radio::esp_now::PeerInfo {
                                    interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                                    peer_address: r.info.src_address,
                                    lmk: None,
                                    channel: Some(11),
                                    encrypt: false,
                                });
                            }
                            #[cfg(feature = "statistics")]
                            {
                                let rtt = r_time - packet.central_send_uptime;
                                // Sanity check: ignore delays > 1s
                                if rtt > 0 && rtt < 1_000_000 {
                                    // if latency_offset == -1 {
                                    //     latency_offset = packet.recv_uptime as i64
                                    //         - (packet.central_send_uptime + rtt / 2) as i64;
                                    // }
                                    latency_offset = packet.recv_uptime as i64
                                        - (packet.central_send_uptime + rtt / 2) as i64;
                                    let _ = peripheral_offsets
                                        .insert(r.info.src_address, latency_offset);

                                    let total_elapsed = r_time - packet.central_send_uptime;
                                    let b_processing_delay =
                                        packet.send_uptime - packet.recv_uptime;
                                    let two_way_latency =
                                        (total_elapsed - b_processing_delay) as i64;
                                    let one_way_latency = (r_time as i64
                                        - (packet.send_uptime as i64 - latency_offset))
                                        as i64;
                                    STATS
                                        .two_way_latency
                                        .store(two_way_latency, Ordering::Relaxed);
                                    STATS
                                        .one_way_latency
                                        .store(one_way_latency, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    Err(_) => {}
                }
            }
        }
    }

    // When this finishes (e.g. Stop Signal), the split parts are dropped.
    // The borrow on 'esp_now' ends, and it is ready to be used again!
}
