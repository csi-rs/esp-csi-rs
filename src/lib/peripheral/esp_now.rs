use crate::log_ln;
use crate::reconstruct_raw_csi;
use crate::STOP_SIGNAL;

use esp_radio::esp_now::{EspNow, PeerInfo};

use heapless::Vec;

use embassy_futures::select::select;
use embassy_futures::select::Either;

use crate::EspNowConfig;

pub async fn run_esp_now_peripheral(esp_now: &mut EspNow<'static>, config: &EspNowConfig) {
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());
    esp_now
        .set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi)
        .unwrap();

    responder(esp_now).await;
}

async fn responder(esp_now: &mut EspNow<'static>) {
    // Create a message buffer for the data to be sent back

    // Message format w/ seq_no:
    // [0..1]   : 2 bytes seq_no (u16) - big endian
    // [2]      : 1 byte for CSI data format (mapping below)
    // [3..6]   : 4 bytes timestamp (u32) - big endian
    // [7..12]  : 6 bytes MAC Address of Station
    // [13..n]   : n-6 bytes CSI data (i8)

    // Width of message (625) = 2 bytes for seq_no + 1 byte for format + 4 bytes for timestamp + 6 bytes for MAC + 612 bytes for CSI data
    let mut message_u8: Vec<u8, 625> = Vec::new();
    loop {
        match select(STOP_SIGNAL.wait(), esp_now.receive_async()).await {
            Either::First(_) => {
                // Stop signal received, exit the loop
                break;
            }
            Either::Second(r) => {
                // Build message from raw CSI packet
                let csi_option = reconstruct_raw_csi(r.data()).await;
                if csi_option.is_some() {
                    let mut csi_packet = csi_option.unwrap();
                    csi_packet.mac = r.info.src_address;
                    message_u8.clear();

                    // sequence number
                    let _ = message_u8.extend_from_slice(&csi_packet.sequence_number.to_be_bytes());

                    // data format (may be Undefined in raw mode)
                    let _ = message_u8.push(csi_packet.data_format as u8);

                    // timestamp
                    let _ = message_u8.extend_from_slice(&csi_packet.timestamp.to_be_bytes());

                    // MAC
                    let _ = message_u8.extend_from_slice(&csi_packet.mac);

                    // CSI payload (raw)
                    for x in csi_packet.csi_data.iter() {
                        let _ = message_u8.push(*x as u8);
                    }

                    let _ = esp_now.send_async(&csi_packet.mac, &message_u8).await;
                }

                if !esp_now.peer_exists(&r.info.src_address) {
                    let _ = esp_now.add_peer(PeerInfo {
                        interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                        peer_address: r.info.src_address,
                        lmk: None,
                        channel: None,
                        encrypt: false,
                    });
                }
            }
        };
    }
    log_ln!("Node Stopped. Halting CSI Sending.");
}
