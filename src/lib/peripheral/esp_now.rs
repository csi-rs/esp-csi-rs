use core::sync::atomic::Ordering;
use core::time;

#[cfg(feature = "statistics")]
use crate::STATS;
use crate::log_ln;
use crate::set_runtime_collection_mode;
use crate::ControlPacket;
use crate::PeripheralPacket;
use crate::CENTRAL_MAGIC_NUMBER;
use crate::IS_COLLECTOR;
use crate::STOP_SIGNAL;

use embassy_futures::select::select;
use embassy_futures::select::Either;
use embassy_time::Instant;
use embassy_time::with_timeout;
use embassy_time::Duration;
use embassy_time::Timer;
use embassy_time::WithTimeout;
use esp_radio::esp_now::BROADCAST_ADDRESS;
use esp_radio::esp_now::{EspNow, PeerInfo};

use heapless::Vec;
use zerocopy::FromBytes;
use zerocopy::IntoBytes;

use crate::EspNowConfig;

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

    responder(esp_now, freq).await;
}

async fn responder(esp_now: &mut EspNow<'static>, frequency_hz: u64) {
    let mut is_collector = IS_COLLECTOR.load(Ordering::Relaxed);
    let initial_is_collector = is_collector;
    let mut central_mac: [u8; 6] = [0; 6];
    let mut is_connected = false;

    loop {
        match select(STOP_SIGNAL.wait(), esp_now.receive_async()).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either::Second(r) => {
                let res = postcard::from_bytes::<ControlPacket>(r.data());
                match res {
                    Ok(packet) => {
                        let recv_time = Instant::now().as_micros();
                        if packet.magic_number == CENTRAL_MAGIC_NUMBER {
                            if !is_connected {
                                let _ = esp_now.add_peer(PeerInfo {
                                    interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                                    peer_address: r.info.src_address,
                                    lmk: None,
                                    channel: Some(11),
                                    encrypt: false,
                                });
                                central_mac = r.info.src_address;
                                is_connected = true;
                            }
                            if central_mac == r.info.src_address {
                                #[cfg(feature = "statistics")]
                                STATS.rx_count.fetch_add(1, Ordering::Relaxed);
                                if packet.is_collector != !is_collector {
                                    set_runtime_collection_mode(!is_collector);
                                    is_collector = !is_collector;
                                }

                                #[cfg(feature = "statistics")]
                                if packet.latency_offset != -1 {
                                    let one_way_latency = (recv_time as i64
                                        - (packet.central_send_uptime as i64 + packet.latency_offset))
                                        as i64;
                                    STATS.one_way_latency.store(one_way_latency, Ordering::Relaxed);
                                }

                                let peripheral_packet = PeripheralPacket::new(
                                    recv_time,
                                    packet.central_send_uptime.into(),
                                );
                                let message_u8: Vec<u8, 32> =
                                    postcard::to_vec(&peripheral_packet).unwrap();
                                let res = esp_now.send_async(&central_mac, &message_u8).await;
                                #[cfg(feature = "statistics")]
                                if res.is_ok() {
                                    STATS.tx_count.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    Err(_) => {}
                }
            }
        }
    }
    log_ln!("Node Stopped. Halting CSI Sending.");
}
