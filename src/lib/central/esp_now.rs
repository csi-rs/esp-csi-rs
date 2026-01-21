use embassy_futures::join::join;
use embassy_time::Timer;

use crate::log_ln;
use crate::STOP_SIGNAL;
use esp_radio::esp_now::{
    EspNow, EspNowManager, EspNowReceiver, EspNowSender, EspNowWifiInterface, PeerInfo,
    BROADCAST_ADDRESS,
};

use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};

use embassy_time::{Duration, Ticker};

use embassy_futures::select::select;
use embassy_futures::select::Either;

use crate::reconstruct_raw_csi;
use crate::EspNowConfig;
use crate::PROCESSED_CSI_DATA;

// macro to save a variable in static memory to stay forever in the program lifetime
macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

// setup function, configures the hardware and starts the background tasks
// manager -> adding peers (peripherals)
// sender -> transmitting
// receiver -> listening
// pub async fn run_esp_now_central(
//     esp_now: &mut EspNow<'static>, // Borrow the hardware
//     config: &EspNowConfig,
//     frequency_hz: Option<u16>,
// ) {
//     // 1. Configure
//     esp_now.set_channel(config.channel).unwrap();
//     log_ln!("esp-now version {}", esp_now.version().unwrap());
//     esp_now
//         .set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi)
//         .unwrap();

//     // 2. Split into components
//     //    These components borrow from 'esp_now' for the duration of this scope.
//     let (mut manager, sender, receiver) = esp_now.split();

//     // 3. Setup Initial Peers
//     if !manager.peer_exists(&BROADCAST_ADDRESS) {
//         manager
//             .add_peer(PeerInfo {
//                 peer_address: BROADCAST_ADDRESS,
//                 lmk: None,
//                 channel: None,
//                 encrypt: false,
//                 interface: EspNowWifiInterface::Sta,
//             })
//             .unwrap();
//     }

//     // 4. Run the components in parallel
//     //    We move 'manager' & 'receiver' into listener
//     //    We move 'sender' into broadcaster
//     join(
//         listener(manager, receiver),
//         broadcaster(sender, frequency_hz),
//     )
//     .await;

//     // When this finishes (e.g. Stop Signal), the split parts are dropped.
//     // The borrow on 'esp_now' ends, and it is ready to be used again!
// }

pub async fn run_esp_now_central(
    esp_now: &mut EspNow<'static>, // Borrow the hardware
    config: &EspNowConfig,
    frequency_hz: Option<u16>,
) {
    // Configure
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());
    esp_now
        .set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi)
        .unwrap();

    // Setup Initial Peers
    if !esp_now.peer_exists(&BROADCAST_ADDRESS) {
        esp_now
            .add_peer(PeerInfo {
                peer_address: BROADCAST_ADDRESS,
                lmk: None,
                channel: None,
                encrypt: false,
                interface: EspNowWifiInterface::Sta,
            })
            .unwrap();
    }

    let freq = match frequency_hz {
        Some(freq) => freq as u64,
        None => u16::MAX as u64,
    };

    let proc_csi_data = PROCESSED_CSI_DATA.publisher().unwrap();

    loop {
        // let current_timestamp = embassy_time::Instant::now();
        match select(
            STOP_SIGNAL.wait(),
            select(
                Timer::after(Duration::from_hz(freq)),
                esp_now.receive_async(),
            ),
        )
        .await
        {
            Either::First(_) => {
                // Stop signal received, exit the loop
                break;
            }
            Either::Second(inner_fut) => {
                match inner_fut {
                    Either::First(_) => {
                        // let elapsed = current_timestamp.elapsed().as_micros();
                        // log_ln!("Send Broadcast at {:?}", elapsed);
                        let status = esp_now.send_async(&BROADCAST_ADDRESS, b"H").await;
                        // log_ln!("Send broadcast status: {:?}", status);
                    }
                    Either::Second(r) => {
                        let csi_packet = reconstruct_raw_csi(r.data()).await;
                        if csi_packet.is_some() {
                            let mut packet = csi_packet.unwrap();
                            packet.mac = r.info.src_address;
                            let _ = proc_csi_data.publish_immediate(packet);
                        }

                        if r.info.dst_address == BROADCAST_ADDRESS {
                            if !esp_now.peer_exists(&r.info.src_address) {
                                esp_now
                                    .add_peer(PeerInfo {
                                        interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                                        peer_address: r.info.src_address,
                                        lmk: None,
                                        channel: None,
                                        encrypt: false,
                                    })
                                    .unwrap();
                                log_ln!("Added peer {:?}", r.info.src_address);
                            }
                        }
                    }
                }
            }
        }
    }

    // When this finishes (e.g. Stop Signal), the split parts are dropped.
    // The borrow on 'esp_now' ends, and it is ready to be used again!
}

// async fn broadcaster(mut sender: EspNowSender<'_>, frequency_hz: Option<u16>) {
//     // let interval_ms = match frequency_hz {
//     //     Some(freq) => 1000_u64 / freq as u64,
//     //     None => u32::MAX as u64,
//     // };

//     // let mut ticker = Ticker::every(Duration::from_millis(interval_ms));

//     let freq = match frequency_hz {
//         Some(freq) => freq as u64,
//         None => u16::MAX as u64,
//     };

//     loop {
//         // let current_timestamp = embassy_time::Instant::now();
//         match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_hz(freq))).await {
//             Either::First(_) => {
//                 // Stop signal received, exit the loop
//                 break;
//             }
//             Either::Second(_) => {
//                 // let elapsed = current_timestamp.elapsed().as_micros();
//                 // log_ln!("Send Broadcast at {:?}", elapsed);
//                 let status = sender.send_async(&BROADCAST_ADDRESS, b"H").await;
//                 // log_ln!("Send broadcast status: {:?}", status);
//             }
//         }
//     }
//     log_ln!("Node Stopped. Halting Broacasts.");
// }

// async fn listener(manager: EspNowManager<'_>, mut receiver: EspNowReceiver<'_>) {
//     let proc_csi_data = PROCESSED_CSI_DATA.publisher().unwrap();
//     loop {
//         match select(STOP_SIGNAL.wait(), receiver.receive_async()).await {
//             Either::First(_) => {
//                 // Stop signal received, exit the loop
//                 break;
//             }
//             Either::Second(r) => {
//                 // log_ln!("Received {:?}", r.data());

//                 let csi_packet = reconstruct_raw_csi(r.data()).await;
//                 if csi_packet.is_some() {
//                     let mut packet = csi_packet.unwrap();
//                     packet.mac = r.info.src_address;
//                     let _ = proc_csi_data.publish_immediate(packet);
//                 }

//                 if r.info.dst_address == BROADCAST_ADDRESS {
//                     if !manager.peer_exists(&r.info.src_address) {
//                         manager
//                             .add_peer(PeerInfo {
//                                 interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
//                                 peer_address: r.info.src_address,
//                                 lmk: None,
//                                 channel: None,
//                                 encrypt: false,
//                             })
//                             .unwrap();
//                         log_ln!("Added peer {:?}", r.info.src_address);
//                     }
//                 }
//             }
//         }
//     }
//     log_ln!("Node Stopped. Halting CSI Collection.");
// }
