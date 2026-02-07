use core::sync::atomic::Ordering;
use core::time;

use crate::log_ln;
use crate::set_runtime_collection_mode;
use crate::ControlPacket;
use crate::PeripheralPacket;
use crate::CENTRAL_MAGIC_NUMBER;
use crate::IS_COLLECTOR;
use crate::STOP_SIGNAL;

use embassy_futures::select::select;
use embassy_futures::select::Either;
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
                            if central_mac == r.info.src_address && !initial_is_collector {
                                if packet.is_collector != !is_collector {
                                    set_runtime_collection_mode(!is_collector);
                                    is_collector = !is_collector;
                                }
                            }

                            let peripheral_packet =
                                PeripheralPacket::new(packet.central_send_uptime.into());
                            let message_u8: Vec<u8, 16> =
                                postcard::to_vec(&peripheral_packet).unwrap();
                            let _ = esp_now.send_async(&central_mac, &message_u8).await;
                        }
                    }
                    Err(_) => {}
                }
            }
        }
    }
    log_ln!("Node Stopped. Halting CSI Sending.");
}
