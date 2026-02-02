use crate::log_ln;
use crate::ControlPacket;
use crate::STOP_SIGNAL;

use embassy_time::Duration;
use embassy_time::Timer;
use esp_radio::esp_now::{EspNow, PeerInfo};

use embassy_futures::select::select;
use embassy_futures::select::Either;
use zerocopy::FromBytes;

use crate::EspNowConfig;

pub async fn run_esp_now_peripheral(
    esp_now: &mut EspNow<'static>,
    config: &EspNowConfig,
    frequency_hz: Option<u16>,
) {
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());
    esp_now
        .set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi)
        .unwrap();

    let freq = match frequency_hz {
        Some(freq) => freq as u64,
        None => u16::MAX as u64,
    };

    responder(esp_now, freq).await;
}

async fn responder(esp_now: &mut EspNow<'static>, freq: u64) {
    let mut is_connected = false;
    let mut central_mac: [u8; 6] = [0; 6];
    loop {
        if is_connected {
            match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_hz(freq))).await {
                Either::First(_) => {
                    // Stop signal received, exit the loop
                    break;
                }
                Either::Second(_) => {
                    let _ = esp_now.send_async(&central_mac, b"H").await;
                }
            }
        } else {
            match select(STOP_SIGNAL.wait(), esp_now.receive_async()).await {
                Either::First(_) => {
                    // Stop signal received, exit the loop
                    break;
                }
                Either::Second(r) => {
                    let res = ControlPacket::ref_from_bytes(r.data());
                    match res {
                        Ok(packet) => {
                            if packet.magic_number == 0xA8912BF0 {
                                let _ = esp_now.add_peer(PeerInfo {
                                    interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                                    peer_address: r.info.src_address,
                                    lmk: None,
                                    channel: None,
                                    encrypt: false,
                                });
                                is_connected = true;
                                central_mac = r.info.src_address;
                            }
                        }
                        Err(e) => {}
                    }
                }
            };
        }
    }
    log_ln!("Node Stopped. Halting CSI Sending.");
}
