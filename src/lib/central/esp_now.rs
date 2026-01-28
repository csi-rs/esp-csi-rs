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

pub async fn run_esp_now_central(
    esp_now: &mut EspNow<'static>, // Borrow the hardware
    config: &EspNowConfig,
    frequency_hz: Option<u16>,
) {
    // Configure
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());
    // esp_now
    //     .set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi)
    //     .unwrap();

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
                        let _ = esp_now.send_async(&BROADCAST_ADDRESS, b"H").await;
                        // log_ln!("Send broadcast status: {:?}", status);
                    }
                    Either::Second(r) => {
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