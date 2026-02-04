use core::sync::atomic::Ordering;

use embassy_futures::select::select3;
use embassy_futures::select::Either3;
use embassy_time::Instant;
use embassy_time::Timer;
use heapless::Vec;
use zerocopy::FromBytes;
use zerocopy::IntoBytes;

use crate::log_ln;
use crate::ControlPacket;
use crate::PeripheralPacket;
use crate::AVG_LATENCY;
use crate::PERIPHERAL_MAGIC_NUMBER;
use crate::STOP_SIGNAL;
use esp_radio::esp_now::{EspNow, BROADCAST_ADDRESS};

use embassy_time::Duration;

use crate::EspNowConfig;

pub async fn run_esp_now_central(
    esp_now: &mut EspNow<'static>, // Borrow the hardware
    mac_addr: [u8; 6],
    config: &EspNowConfig,
    frequency_hz: Option<u16>,
    is_collector: bool,
) {
    // Configure
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());

    let freq = match frequency_hz {
        Some(freq) => freq as u64,
        None => u16::MAX as u64,
    };

    let _ = esp_now.add_peer(esp_radio::esp_now::PeerInfo {
        interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
        peer_address: BROADCAST_ADDRESS,
        lmk: None,
        channel: Some(11),
        encrypt: false,
    });

    let mut message_u8: Vec<u8, 16> = Vec::new();

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
                message_u8.clear();
                let control_packet = ControlPacket::new(is_collector);
                let _ = message_u8.extend_from_slice(control_packet.as_bytes());
                let _ = esp_now.send_async(&BROADCAST_ADDRESS, &message_u8).await;
            }
            Either3::Third(r) => {
                let r_time = Instant::now().as_micros();
                let res = PeripheralPacket::ref_from_bytes(r.data());
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
                            let rtt = r_time - packet.central_send_uptime;
                            // Sanity check: ignore delays > 1s
                            if rtt > 0 && rtt < 1_000_000 {
                                let latency = rtt.get() as i64 / 2;
                                // Update average latency using a simple moving average
                                let current_avg = AVG_LATENCY.load(Ordering::Relaxed);
                                let new_avg = if current_avg == 0 {
                                    latency
                                } else {
                                    // Math trick: (avg * 7 + latency) / 8 without floating point
                                    ((current_avg << 3) - current_avg + latency) >> 3
                                };
                                AVG_LATENCY.store(new_avg, Ordering::Relaxed);
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
