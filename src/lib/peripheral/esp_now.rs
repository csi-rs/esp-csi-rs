use crate::log_ln;
use crate::CSI_PACKET;

use esp_radio::esp_now::{EspNow, PeerInfo};

use heapless::Vec;

use embassy_futures::select::select;
use embassy_futures::select::Either;

use crate::EspNowConfig;
use crate::TX_STOP_SIGNAL;

pub fn esp_now_peripheral_init(
    esp_now: EspNow<'static>,
    config: &EspNowConfig,
    spawner: embassy_executor::Spawner,
) {
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());
    esp_now
        .set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi)
        .unwrap();

    spawner.spawn(responder(esp_now)).ok();
}

#[embassy_executor::task]
async fn responder(mut esp_now: EspNow<'static>) {
    let mut csi_data = CSI_PACKET.subscriber().unwrap();

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
        match select(TX_STOP_SIGNAL.wait(), csi_data.next_message_pure()).await {
            Either::First(_) => {
                // Stop signal received, exit the loop
                break;
            }
            Either::Second(raw_csi_data) => {
                // Build message from raw CSI packet
                message_u8.clear();

                // sequence number
                let _ = message_u8.extend_from_slice(&raw_csi_data.sequence_number.to_be_bytes());

                // data format (may be Undefined in raw mode)
                let _ = message_u8.push(raw_csi_data.data_format as u8);

                // timestamp
                let _ = message_u8.extend_from_slice(&raw_csi_data.timestamp.to_be_bytes());

                // MAC
                let _ = message_u8.extend_from_slice(&raw_csi_data.mac);

                // CSI payload (raw)
                for x in raw_csi_data.csi_data.iter() {
                    let _ = message_u8.push(*x as u8);
                }

                if !esp_now.peer_exists(&raw_csi_data.mac) {
                    let _ = esp_now.add_peer(PeerInfo {
                        interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                        peer_address: raw_csi_data.mac,
                        lmk: None,
                        channel: None,
                        encrypt: false,
                    });
                }

                let _ = esp_now.send_async(&raw_csi_data.mac, &message_u8).await;
            }
        };
    }
    TX_STOP_SIGNAL.reset();
    log_ln!("Node Stopped. Halting CSI Sending.");
}
