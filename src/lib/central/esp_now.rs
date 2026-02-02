use embassy_time::Timer;
use heapless::Vec;
use zerocopy::IntoBytes;

use crate::ControlPacket;
use crate::log_ln;
use crate::STOP_SIGNAL;
use esp_radio::esp_now::{
    EspNow,
    BROADCAST_ADDRESS,
};

use embassy_time::{Duration};

use embassy_futures::select::select;
use embassy_futures::select::Either;

use crate::EspNowConfig;

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
    is_collector: bool,
) {
    // Configure
    esp_now.set_channel(config.channel).unwrap();
    log_ln!("esp-now version {}", esp_now.version().unwrap());
    esp_now
        .set_rate(esp_radio::esp_now::WifiPhyRate::RateMcs0Lgi)
        .unwrap();

    let freq = match frequency_hz {
        Some(freq) => freq as u64,
        None => u16::MAX as u64,
    };

    let mut message_u8: Vec<u8, 9> = Vec::new();
    
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
                        message_u8.clear();
                        let control_packet = ControlPacket::new(is_collector);
                        let _ = message_u8.extend_from_slice(control_packet.as_bytes());
                        let _ = esp_now.send_async(&BROADCAST_ADDRESS, &message_u8).await;
                    }
                    Either::Second(r) => {
                    }
                }
            }
        }
    }

    // When this finishes (e.g. Stop Signal), the split parts are dropped.
    // The borrow on 'esp_now' ends, and it is ready to be used again!
}