use core::sync::atomic::Ordering;

use crate::log_ln;
use crate::set_runtime_collection_mode;
use crate::ControlPacket;
use crate::IS_COLLECTOR;
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

    let freq = match frequency_hz {
        Some(freq) => freq as u64,
        None => u16::MAX as u64,
    };

    responder(esp_now, freq).await;
}

async fn responder(esp_now: &mut EspNow<'static>, freq: u64) {
    let mut is_connected = false;
    let mut is_collector = IS_COLLECTOR.load(Ordering::Relaxed);
    let initial_is_collector = is_collector;
    let mut central_mac: [u8; 6] = [0; 6];
    loop {
        match select(
            STOP_SIGNAL.wait(),
            select(
                esp_now.receive_async(),
                Timer::after(Duration::from_hz(freq)),
            ),
        )
        .await
        {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either::Second(inner) => match inner {
                Either::First(r) => {
                    let res = ControlPacket::ref_from_bytes(r.data());
                    match res {
                        Ok(packet) => {
                            if packet.magic_number == 0xA8912BF0 {
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
                                    let is_collector_bool = packet.is_collector != 0;
                                    if is_collector_bool != !is_collector {
                                        set_runtime_collection_mode(!is_collector_bool);
                                        is_collector = !is_collector_bool;
                                    }
                                }
                            }
                        }
                        Err(_) => {}
                    }
                }
                Either::Second(_) => {
                    if is_connected && !is_collector {
                        let _: Result<(), esp_radio::esp_now::EspNowError> = esp_now.send_async(&central_mac, b"H").await;
                    }
                }
            },
        }
    }
    log_ln!("Node Stopped. Halting CSI Sending.");
}
