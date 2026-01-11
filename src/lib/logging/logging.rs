use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel}
use heapless::String;
use core::fmt::Write;
use crate::csi::{CSIDataPacket}

static CSI_CHANNEL: Channel<CriticalSectionRawMutex, CSIDataPacket, 10> = Channel::new();

type DebugPayload = String<128>;
static DEBUG_CHANNEL: Channel<CriticalSectionRawMutex, DebugPayload, 20> = Channel::new();

#[macro_export]
macro_rules! log_ln {
    ($($arg:tt)*) => {{
        #[cfg(any(feature = "jtag-serial", feature = "uart", feature = "auto", feature = "println"))]
        {
            esp_println::println!($($arg)*);
        }

        #[cfg(feature = "defmt")]
        {
            defmt::info!($($arg)*);
        }

        #[cfg(not(any(
            feature = "println", 
            feature = "defmt", 
            feature = "jtag-serial", 
            feature = "uart", 
            feature = "auto"
        )))]
        {
        }
    }};
}

#[macro_export]
macro_rules! log_csi_ln {
}

#[embassy_executor::task]
