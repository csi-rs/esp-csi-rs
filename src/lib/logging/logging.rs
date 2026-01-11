use crate::csi::CSIDataPacket;
use core::sync::atomic::AtomicUsize;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use heapless::String;

#[cfg(any(feature = "uart", feature = "jtag-serial"))]
pub static CSI_CHANNEL: Channel<CriticalSectionRawMutex, CSIDataPacket, 10> = Channel::new();
#[cfg(any(feature = "uart", feature = "jtag-serial"))]
pub static DROPPED_PACKETS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "println")]
mod log_impl {
    use super::*;
    #[cfg(any(feature = "uart", feature = "jtag-serial"))]
    use core::fmt::Write;

    #[cfg(any(feature = "uart", feature = "jtag-serial"))]
    pub static LOG_CHANNEL: Channel<CriticalSectionRawMutex, String<128>, 10> = Channel::new();

    struct EspLogger;
    impl log::Log for EspLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Info
        }
        fn log(&self, record: &log::Record) {
            #[cfg(any(feature = "uart", feature = "jtag-serial"))]
            if self.enabled(record.metadata()) {
                let mut text: DebugPayload = String::new();
                if write!(&mut text, "[{}] {}\r\n", record.level(), record.args()).is_ok() {
                    let _ = LOG_CHANNEL.try_send(text);
                }
            }
        }
        fn flush(&self) {}
    }

    static LOG_INSTANCE: EspLogger = EspLogger;

    pub fn init_logger(level: log::LevelFilter) {
        unsafe {
            log::set_logger_racy(&LOG_INSTANCE).unwrap();
            log::set_max_level(level);
        }
    }
}

#[cfg(feature = "defmt")]
mod defmt_impl {
    use super::*;
    #[cfg(any(feature = "uart", feature = "jtag-serial"))]
    use embassy_sync::pipe::Pipe;

    #[cfg(any(feature = "uart", feature = "jtag-serial"))]
    pub static DEFMT_PIPE: Pipe<CriticalSectionRawMutex, { 128 * 10 }> = Pipe::new();

    #[defmt::global_logger]
    struct AsyncDefmtBackend;

    unsafe impl defmt::Logger for AsyncDefmtBackend {
        fn acquire() {}
        fn release() {}
        fn flush() {}

        unsafe fn write(bytes: &[u8]) {
            #[cfg(any(feature = "uart", feature = "jtag-serial"))]
            {
                let _ = DEFMT_PIPE.write(bytes);
            }
        }
    }
}

#[macro_export]
macro_rules! log_ln {
    ($($arg:tt)*) => {{
        #[cfg(any(feature = "uart", feature = "jtag-serial"))]
        {
            #[cfg(feature = "println")]
            {
                log::log!($($arg)*);
            }

            #[cfg(feature = "defmt")]
            {
                defmt::info!($($arg)*);
            }

            #[cfg(not(any(
                feature = "println",
                feature = "defmt"
            )))]
            {
            }
        }
    }};
}

#[macro_export]
macro_rules! log_csi {
    ($arg:expr) => {
        #[cfg(any(feature = "uart", feature = "jtag-serial"))]
        {
            match $crate::logging::CSI_CHANNEL.try_send($arg) {
                Ok(_) => {}
                Err(_) => $crate::logging::DROPPED_PACKETS
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed),
            }
        }
    };
}
