use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use esp_hal::{peripherals::Peripherals, uart::Uart, usb_serial_jtag::UsbSerialJtag};

#[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
mod csi_interface {
    use crate::csi::CSIDataPacket;
    use core::sync::atomic::AtomicUsize;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
    pub static CSI_CHANNEL: Channel<CriticalSectionRawMutex, CSIDataPacket, 10> = Channel::new();
    pub static DROPPED_PACKETS: AtomicUsize = AtomicUsize::new(0);
}

#[cfg(feature = "println")]
mod log_impl {
    use super::*;
    use heapless::String;

    use core::fmt::Write;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, pipe::Pipe};

    #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
    pub static LOG_PIPE: Pipe<CriticalSectionRawMutex, { 2048 }> = Pipe::new();

    struct EspLogger;

    impl log::Log for EspLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Info
        }

        fn log(&self, record: &log::Record) {
            #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
            if self.enabled(record.metadata()) {
                let mut text: String<128> = String::new();

                if write!(&mut text, "[{}] {}\r\n", record.level(), record.args()).is_ok() {
                    let _ = LOG_PIPE.try_write(text.as_bytes());
                }
            }
        }

        fn flush(&self) {}
    }

    static LOG_INSTANCE: EspLogger = EspLogger;

    pub fn init_logger(level: log::LevelFilter) {
        unsafe {
            log::set_logger_racy(&LOG_INSTANCE).unwrap();
            log::set_max_level_racy(level);
        }
    }
}

#[cfg(feature = "defmt")]
mod defmt_impl {
    use super::*;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, pipe::Pipe};

    pub static DEFMT_PIPE: Pipe<CriticalSectionRawMutex, { 2048 }> = Pipe::new();

    #[defmt::global_logger]
    struct AsyncDefmtBackend;

    unsafe impl defmt::Logger for AsyncDefmtBackend {
        fn acquire() {}
        fn release() {}
        fn flush() {}

        unsafe fn write(bytes: &[u8]) {
            #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
            {
                let _ = DEFMT_PIPE.try_write(bytes);
            }
        }
    }
}

#[cfg(any(feature = "jtag-serial", feature = "auto"))]
mod timeout_impl {
    use embassy_time::{with_timeout, Duration};
    use embedded_io_async::{ErrorKind, ErrorType, Write};

    pub struct TimeoutWriter<W> {
        inner: W,
        timeout: Duration,
    }

    impl<W> TimeoutWriter<W> {
        pub fn new(inner: W) -> Self {
            Self {
                inner,
                timeout: Duration::from_millis(10),
            }
        }
    }

    impl<W: ErrorType> ErrorType for TimeoutWriter<W> {
        type Error = ErrorKind;
    }

    impl<W: Write> Write for TimeoutWriter<W> {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            match with_timeout(self.timeout, self.inner.write(buf)).await {
                Ok(Ok(len)) => Ok(len),
                Ok(Err(_)) => Err(ErrorKind::Other),
                Err(_) => Err(ErrorKind::TimedOut),
            }
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            match with_timeout(self.timeout, self.inner.flush()).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) => Err(ErrorKind::Other),
                Err(_) => Err(ErrorKind::TimedOut),
            }
        }
    }
}

#[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
mod logging_impl {
    use crate::logging::logging::timeout_impl::TimeoutWriter;
    use embassy_time::Duration;
    use embedded_io_async::{ErrorKind, ErrorType, Write};
    use esp_hal::{
        peripherals::Peripherals, peripherals::UART0, uart::Uart, usb_serial_jtag::UsbSerialJtag,
        Async,
    };

    pub enum LogOutput {
        #[cfg(any(feature = "uart", feature = "auto"))]
        Uart(Uart<'static, Async>),
        #[cfg(any(feature = "jtag-serial", feature = "auto"))]
        Jtag(TimeoutWriter<UsbSerialJtag<'static, Async>>),
    }

    impl ErrorType for LogOutput {
        type Error = embedded_io_async::ErrorKind;
    }

    impl embedded_io_async::Write for LogOutput {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            match self {
                #[cfg(any(feature = "uart", feature = "auto"))]
                Self::Uart(driver) => driver
                    .write_async(buf)
                    .await
                    .map_err(|_| embedded_io_async::ErrorKind::Other),

                #[cfg(any(feature = "jtag-serial", feature = "auto"))]
                Self::Jtag(driver) => driver.write(buf).await,
            }
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            match self {
                #[cfg(any(feature = "uart", feature = "auto"))]
                Self::Uart(driver) => driver
                    .flush_async()
                    .await
                    .map_err(|_| embedded_io_async::ErrorKind::Other),

                #[cfg(any(feature = "jtag-serial", feature = "auto"))]
                Self::Jtag(driver) => driver.flush().await,
            }
        }
    }
}

#[macro_export]
macro_rules! log_ln {
    ($($arg:tt)*) => {{
        #[cfg(
            any(feature = "uart", feature = "jtag-serial", feature = "auto")
        )]
        {
            #[cfg(feature = "println")]
            {
                log::info!($($arg)*);
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
        #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
        {
            match $crate::logging::CSI_CHANNEL.try_send($arg) {
                Ok(_) => {}
                Err(_) => $crate::logging::DROPPED_PACKETS
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed),
            }
        }
    };
}

use crate::logging::logging::logging_impl::LogOutput;
use crate::logging::logging::timeout_impl::TimeoutWriter;
use esp_hal::uart::Config;

pub fn init_logger(spawner: embassy_executor::Spawner) {
    #[cfg(feature = "auto")]
    {
        let periphs = unsafe { Peripherals::steal() };
        #[cfg(feature = "esp32c3")]
        const USB_DEVICE_INT_RAW: *const u32 = 0x60043008 as *const u32;
        #[cfg(feature = "esp32c6")]
        const USB_DEVICE_INT_RAW: *const u32 = 0x6000f008 as *const u32;
        #[cfg(feature = "esp32h2")]
        const USB_DEVICE_INT_RAW: *const u32 = 0x6000f008 as *const u32;
        #[cfg(feature = "esp32s3")]
        const USB_DEVICE_INT_RAW: *const u32 = 0x60038000 as *const u32;

        const SOF_INT_MASK: u32 = 0b10;
        let mut res = unsafe { (USB_DEVICE_INT_RAW.read_volatile() & SOF_INT_MASK) != 0 };
        if res == true {
            let raw_driver = UsbSerialJtag::new(periphs.USB_DEVICE).into_async();
            let safe_driver = TimeoutWriter::new(raw_driver);
            let mut output_int = LogOutput::Jtag(safe_driver);
            spawner.spawn(logger_backend(output_int)).unwrap();
        } else {
            let mut uart_reg = Uart::new(periphs.UART0, Config::default())
                .unwrap()
                .into_async();
            let mut driver = LogOutput::Uart(uart_reg);
            spawner.spawn(logger_backend(driver)).unwrap();
        }
    }
    #[cfg(feature = "jtag-serial")]
    {
        let periphs = unsafe { Peripherals::steal() };
        let raw_driver = UsbSerialJtag::new(periphs.USB_DEVICE).into_async();
        let safe_driver = TimeoutWriter::new(raw_driver);
        let mut output_int = LogOutput::Jtag(safe_driver);
        spawner.spawn(logger_backend(output_int)).unwrap();
    }
    #[cfg(feature = "uart")]
    {
        let periphs = unsafe { Peripherals::steal() };
        let mut uart_reg = Uart::new(periphs.UART0, Config::default())
            .unwrap()
            .into_async();
        let mut driver = LogOutput::Uart(uart_reg);
        spawner.spawn(logger_backend(driver)).unwrap();
    }

    #[cfg(not(any(feature = "uart", feature = "jtag-serial", feature = "auto")))]
    {}
}

#[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
#[embassy_executor::task]
pub async fn logger_backend(mut driver: LogOutput) {
    use embassy_futures::select::{select, Either};
    let mut raw = [0u8; 1024];
    loop {
        let csi_future = csi_interface::CSI_CHANNEL.receive();
        #[cfg(feature = "println")]
        let log_future = log_impl::LOG_PIPE.read(&mut raw);
        #[cfg(feature = "defmt")]
        let log_future = defmt_impl::DEFMT_PIPE.read(&mut raw);
        match select(csi_future, log_future).await {
            Either::First(packet) => {
                use embedded_io_async::Write;

                use crate::csi::CSIDataPacket;

                let header = [
                    0xFA, 0xFB,
                    0x01,
                    (size_of<CSIDataPacket>() & 0xFF) as u8,
                    (size_of<CSIDataPacket>() >> 8) as u8,
                ];
                let res = driver.write(header);
                match res {
                    Ok(_) => {}
                    Error(_) => {
                        crate::logging::DROPPED_PACKETS
                        .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    }
                }
                let _ = driver.write(packet);
            }
            Either::Second(n) => {
                if n > 0 {
                    use embedded_io_async::Write;
                    let _ = driver.write(&raw[..n]).await;
                }
            }
        }
        if csi_interface::CSI_CHANNEL.is_empty() && log_impl::LOG_PIPE.is_empty() {
            let _ = driver.flush().await;
        }
    }
}