use embassy_time::Timer;

use crate::log_ln;
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
use crate::{RX_STOP_SIGNAL, TX_STOP_SIGNAL};


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
pub fn esp_now_central_init(
    esp_now: EspNow<'static>,
    config: &EspNowConfig,
    spawner: embassy_executor::Spawner,
    frequency_hz: Option<u16>,
) {
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());
    esp_now
        .set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi)
        .unwrap();

    let (manager, sender, receiver) = esp_now.split();
    let manager = mk_static!(EspNowManager<'static>, manager);
    let sender = mk_static!(
        Mutex::<NoopRawMutex, EspNowSender<'static>>,
        Mutex::<NoopRawMutex, _>::new(sender)
    );

    if !manager.peer_exists(&BROADCAST_ADDRESS) {
        manager
            .add_peer(PeerInfo {
                peer_address: BROADCAST_ADDRESS,
                lmk: None,
                channel: None,
                encrypt: false,
                interface: EspNowWifiInterface::Sta,
            })
            .unwrap();
    }

    spawner.spawn(listener(manager, receiver)).ok();
    spawner.spawn(broadcaster(sender, frequency_hz)).ok();

    // spawner.spawn(collector(esp_now)).ok();
}

#[embassy_executor::task]
async fn broadcaster(
    sender: &'static Mutex<NoopRawMutex, EspNowSender<'static>>,
    frequency_hz: Option<u16>,
) {
    // let interval_ms = match frequency_hz {
    //     Some(freq) => 1000_u64 / freq as u64,
    //     None => u32::MAX as u64,
    // };

    // let mut ticker = Ticker::every(Duration::from_millis(interval_ms));

    let freq = match frequency_hz {
        Some(freq) => freq as u64,
        None => u16::MAX as u64,
    };

    loop {
        let current_timestamp = embassy_time::Instant::now();
        match select(TX_STOP_SIGNAL.wait(), Timer::after(Duration::from_hz(freq))).await {
            Either::First(_) => {
                // Stop signal received, exit the loop
                break;
            }
            Either::Second(_) => {
                let elapsed = current_timestamp.elapsed().as_micros();
                log_ln!("Send Broadcast at {:?}", elapsed);
                let mut sender = sender.lock().await;
                let status = sender.send_async(&BROADCAST_ADDRESS, b"H").await;
                log_ln!("Send broadcast status: {:?}", status);
            }
        }
    }
    TX_STOP_SIGNAL.reset();
    log_ln!("Node Stopped. Halting Broacasts.");
}

#[embassy_executor::task]
async fn listener(manager: &'static EspNowManager<'static>, mut receiver: EspNowReceiver<'static>) {
    let proc_csi_data = PROCESSED_CSI_DATA.publisher().unwrap();
    loop {
        match select(RX_STOP_SIGNAL.wait(), receiver.receive_async()).await {
            Either::First(_) => {
                // Stop signal received, exit the loop
                break;
            }
            Either::Second(r) => {
                // log_ln!("Received {:?}", r.data());

                let csi_packet = reconstruct_raw_csi(r.data()).await;
                if csi_packet.is_some() {
                    let mut packet = csi_packet.unwrap();
                    packet.mac = r.info.src_address;
                    let _ = proc_csi_data.publish_immediate(packet);
                }

                if r.info.dst_address == BROADCAST_ADDRESS {
                    if !manager.peer_exists(&r.info.src_address) {
                        manager
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
    RX_STOP_SIGNAL.reset();
    log_ln!("Node Stopped. Halting CSI Collection.");
}
