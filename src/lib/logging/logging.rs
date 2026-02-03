use embedded_io_async::Write;
use esp_hal::peripherals::Peripherals;

#[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
mod csi_interface {
    use crate::csi::CSIDataPacket;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
    use portable_atomic::AtomicU32;
    pub static CSI_CHANNEL: Channel<CriticalSectionRawMutex, CSIDataPacket, 2> = Channel::new();
    pub static LOG_DROPPED_PACKETS: AtomicU32 = AtomicU32::new(0);
}

#[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
pub use csi_interface::{CSI_CHANNEL, LOG_DROPPED_PACKETS};
use portable_atomic::Ordering;
use postcard::experimental::max_size::MaxSize;

pub fn get_log_packet_drops() -> u32 {
    #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
    {
        LOG_DROPPED_PACKETS.load(Ordering::Relaxed)
    }
    #[cfg(not(any(feature = "uart", feature = "jtag-serial", feature = "auto")))]
    {
        0
    }
}

#[cfg(feature = "println")]
mod log_impl {
    use heapless::String;

    use core::fmt::Write;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, pipe::Pipe};

    #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
    pub static LOG_PIPE: Pipe<CriticalSectionRawMutex, 2048> = Pipe::new();

    struct EspLogger;

    impl log::Log for EspLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Info
        }

        fn log(&self, record: &log::Record) {
            #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
            if self.enabled(record.metadata()) {
                let mut text: String<512> = String::new();

                if write!(&mut text, "{}\r\n", record.args()).is_ok() {
                    if text.len() <= LOG_PIPE.free_capacity() {
                        let _ = LOG_PIPE.try_write(text.as_bytes());
                    }
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
                if bytes.len() <= DEFMT_PIPE.free_capacity() {
                    let _ = DEFMT_PIPE.try_write(bytes);
                }
            }
        }
    }
}

mod logging_impl {
    use embedded_io_async::{ErrorType, Write};
    #[cfg(any(feature = "jtag-serial", feature = "auto"))]
    use esp_hal::peripherals::Peripherals;
    use esp_hal::{
        uart::{Config, Uart},
        usb_serial_jtag::UsbSerialJtag,
        Async,
    };

    pub enum Backend {
        #[cfg(any(feature = "uart", feature = "auto"))]
        Uart(Uart<'static, Async>),
        #[cfg(any(feature = "jtag-serial", feature = "auto"))]
        Jtag(UsbSerialJtag<'static, Async>),
    }

    impl ErrorType for Backend {
        type Error = embedded_io_async::ErrorKind;
    }

    impl embedded_io_async::Write for Backend {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
            match self {
                #[cfg(any(feature = "uart", feature = "auto"))]
                Self::Uart(driver) => driver
                    .write_all(buf)
                    .await
                    .map(|_| buf.len())
                    .map_err(|_| embedded_io_async::ErrorKind::Other),

                #[cfg(any(feature = "jtag-serial", feature = "auto"))]
                Self::Jtag(driver) => driver
                    .write_all(buf)
                    .await
                    .map(|_| buf.len())
                    .map_err(|_| embedded_io_async::ErrorKind::Other),
            }
            #[cfg(feature = "no-print")]
            Err(embedded_io_async::ErrorKind::Other)
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
            match self {
                #[cfg(any(feature = "uart", feature = "auto"))]
                Self::Uart(driver) => driver
                    .flush_async()
                    .await
                    .map_err(|_| embedded_io_async::ErrorKind::Other),

                #[cfg(any(feature = "jtag-serial", feature = "auto"))]
                Self::Jtag(driver) => driver
                    .flush()
                    .await
                    .map_err(|_| embedded_io_async::ErrorKind::Other),
            }
            #[cfg(feature = "no-print")]
            Err(embedded_io_async::ErrorKind::Other)
        }
    }

    pub struct LogOutput {
        inner: Backend,
        pub serialization_enabled: bool,
    }

    impl LogOutput {
        #[cfg(any(feature = "uart", feature = "auto"))]
        pub fn new_uart(periphs: Peripherals, serialization_enabled: bool) -> Self {
            let raw_driver = Uart::new(periphs.UART0, Config::default())
                .unwrap()
                .into_async();
            Self {
                inner: Backend::Uart(raw_driver),
                serialization_enabled,
            }
        }

        #[cfg(any(feature = "jtag-serial", feature = "auto"))]
        pub fn new_jtag(periphs: Peripherals, serialization_enabled: bool) -> Self {
            let raw_driver = UsbSerialJtag::new(periphs.USB_DEVICE).into_async();
            Self {
                inner: Backend::Jtag(raw_driver),
                serialization_enabled,
            }
        }
    }

    impl ErrorType for LogOutput {
        type Error = embedded_io_async::ErrorKind;
    }

    impl Write for LogOutput {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            self.inner.write(buf).await
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            self.inner.flush().await
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

use crate::{csi::CSIDataPacket};

pub fn log_csi(packet: CSIDataPacket) {
    #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
    {
        match CSI_CHANNEL.try_send(packet) {
            Ok(_) => {}
            Err(_) => {
                LOG_DROPPED_PACKETS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    #[cfg(not(any(feature = "uart", feature = "jtag-serial", feature = "auto")))]
    {}
}

use crate::{logging::logging::logging_impl::LogOutput};

pub fn init_logger(spawner: embassy_executor::Spawner, serialization_enabled: bool) {
    #[cfg(feature = "println")]
    {
        log_impl::init_logger(log::LevelFilter::Info);
    }
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
        let res = unsafe { (USB_DEVICE_INT_RAW.read_volatile() & SOF_INT_MASK) != 0 };
        if res == true {
            let driver = LogOutput::new_jtag(periphs, serialization_enabled);
            spawner.spawn(logger_backend(driver)).unwrap();
        } else {
            let driver = LogOutput::new_uart(periphs, serialization_enabled);
            spawner.spawn(logger_backend(driver)).unwrap();
        }
    }
    #[cfg(feature = "jtag-serial")]
    {
        let periphs = unsafe { Peripherals::steal() };
        let driver = LogOutput::new_jtag(periphs, serialization_enabled);
        spawner.spawn(logger_backend(driver)).unwrap();
    }
    #[cfg(feature = "uart")]
    {
        let periphs = unsafe { Peripherals::steal() };
        let driver = LogOutput::new_uart(periphs, serialization_enabled);
        spawner.spawn(logger_backend(driver)).unwrap();
    }

    #[cfg(not(any(feature = "uart", feature = "jtag-serial", feature = "auto")))]
    {}
}

async fn write_serialized_packet(packet: CSIDataPacket, driver: &mut LogOutput) -> Result<(), ()> {
    const PACKET_MAX_SIZE: usize = CSIDataPacket::POSTCARD_MAX_SIZE;
    const PACKET_BUF_SIZE: usize = PACKET_MAX_SIZE + (PACKET_MAX_SIZE / 254) + 1;

    let mut buf = [0u8; PACKET_BUF_SIZE];
    match postcard::to_slice_cobs(&packet, &mut buf) {
        Ok(cobs_slice) => match driver.write(cobs_slice).await {
            Ok(_) => Ok(()),
            Err(_) => Err(()),
        },
        Err(_) => Err(()),
    }
}

async fn write_text_packet(packet: CSIDataPacket, driver: &mut LogOutput) -> Result<(), ()> {
    use core::fmt::Write as FmtWrite;
    use heapless::String;

    let mut buf = String::<128>::new();

    macro_rules! send_line {
        ($($arg:tt)*) => {
            buf.clear();
            if write!(&mut buf, $($arg)*).is_ok() {
                driver.write(buf.as_bytes()).await.map_err(|_| ())?;
            }
        };
    }

    let res = async {
        if let Some(dt) = &packet.date_time {
            send_line!(
                "Recieved at {:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}\r\n",
                dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second, dt.millisecond
            );
        }

        send_line!(
            "mac: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}\r\n",
            packet.mac[0],
            packet.mac[1],
            packet.mac[2],
            packet.mac[3],
            packet.mac[4],
            packet.mac[5]
        );

        send_line!("sequence number: {}\r\n", packet.sequence_number);
        send_line!("rssi: {}\r\n", packet.rssi);
        send_line!("rate: {}\r\n", packet.rate);
        send_line!("noise floor: {}\r\n", packet.noise_floor);
        send_line!("channel: {}\r\n", packet.channel);
        send_line!("timestamp: {}\r\n", packet.timestamp);
        send_line!("sig len: {}\r\n", packet.sig_len);
        send_line!("rx state: {}\r\n", packet.rx_state);
        #[cfg(not(feature = "esp32c6"))]
        {
            send_line!("secondary channel: {}\r\n", packet.secondary_channel);
            send_line!("sgi: {}\r\n", packet.sgi);
            send_line!("ant: {}\r\n", packet.antenna);
            send_line!("ampdu cnt: {}\r\n", packet.ampdu_cnt);
            send_line!("sig_mode: {}\r\n", packet.sig_mode);
            send_line!("mcs: {}\r\n", packet.mcs);
            send_line!("cwb: {}\r\n", packet.bandwidth);
            send_line!("smoothing: {}\r\n", packet.smoothing);
            send_line!("not sounding: {}\r\n", packet.not_sounding);
            send_line!("aggregation: {}\r\n", packet.aggregation);
            send_line!("stbc: {}\r\n", packet.stbc);
            send_line!("fec coding: {}\r\n", packet.fec_coding);
        }
        #[cfg(feature = "esp32c6")]
        {
            send_line!("dump len: {}\r\n", packet.dump_len);
            send_line!("he sigb len: {}\r\n", packet.he_sigb_len);
            send_line!("cur single mpdu: {}\r\n", packet.cur_single_mpdu);
            send_line!("cur bb format: {}\r\n", packet.cur_bb_format);
            send_line!("rx channel estimate info vld: {}\r\n", packet.rx_channel_estimate_info_vld);
            send_line!("rx channel estimate len: {}\r\n", packet.rx_channel_estimate_len);
            send_line!("time seconds: {}\r\n", packet.second);
            send_line!("channel: {}\r\n", packet.channel);
            send_line!("is group: {}\r\n", packet.is_group);
            send_line!("rxend state: {}\r\n", packet.rxend_state);
            send_line!("rxmatch3: {}\r\n", packet.rxmatch3);
            send_line!("rxmatch2: {}\r\n", packet.rxmatch2);
            send_line!("rxmatch1: {}\r\n", packet.rxmatch1);
            send_line!("rxmatch0: {}\r\n", packet.rxmatch0);
        }

        send_line!("sig_len: {}\r\n", packet.sig_len);
        send_line!("data length: {}\r\n", packet.csi_data_len);
        Ok::<(), ()>(())
    }
    .await;

    if res.is_err() {
        return Err(());
    }

    if driver.write(b"csi raw data: [").await.is_err() {
        return Err(());
    }

    let mut chunk_buf = [0u8; 128];
    let mut offset = 0;

    for (i, val) in packet.csi_data.iter().enumerate() {
        let mut wrapper = String::<16>::new();

        if i == packet.csi_data.len() - 1 {
            write!(wrapper, "{}", val).ok();
        } else {
            write!(wrapper, "{}, ", val).ok();
        }

        let bytes = wrapper.as_bytes();

        if offset + bytes.len() > chunk_buf.len() {
            if driver.write(&chunk_buf[..offset]).await.is_err() {
                return Err(());
            }
            offset = 0;
        }

        chunk_buf[offset..offset + bytes.len()].copy_from_slice(bytes);
        offset += bytes.len();
    }

    if offset > 0 {
        if driver.write(&chunk_buf[..offset]).await.is_err() {
            return Err(());
        }
    }

    if driver.write(b"]\r\n").await.is_err() {
        return Err(());
    }

    Ok(())
}

#[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
#[embassy_executor::task]
pub async fn logger_backend(mut driver: LogOutput) {
    use embassy_futures::select::{select, Either};
    use embedded_io_async::Write;

    let mut raw = [0u8; 512];
    loop {
        let csi_future = CSI_CHANNEL.receive();

        #[cfg(all(feature = "println", not(feature = "defmt")))]
        let log_future = log_impl::LOG_PIPE.read(&mut raw);
        #[cfg(feature = "defmt")]
        let log_future = defmt_impl::DEFMT_PIPE.read(&mut raw);
        #[cfg(not(any(feature = "println", feature = "defmt")))]
        let log_future = embassy_futures::pending::<usize>();

        let mut did_write = false;

        match select(csi_future, log_future).await {
            Either::First(packet) => {
                let res: Result<(), ()>;
                if driver.serialization_enabled {
                    res = write_serialized_packet(packet, &mut driver).await;
                } else {
                    res = write_text_packet(packet, &mut driver).await;
                }
                match res {
                    Ok(_) => {}
                    Err(_) => {
                        LOG_DROPPED_PACKETS.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Either::Second(n) => {
                if n > 0 {
                    use embedded_io_async::Write;
                    match driver.write(&raw[..n]).await {
                        Ok(_) => {
                            did_write = true;
                        }
                        Err(_) => {}
                    }
                }
            }
        }
        let logs_empty = {
            #[cfg(all(feature = "println", not(feature = "defmt")))]
            {
                log_impl::LOG_PIPE.is_empty()
            }
            #[cfg(feature = "defmt")]
            {
                defmt_impl::DEFMT_PIPE.is_empty()
            }
            #[cfg(not(any(feature = "println", feature = "defmt")))]
            {
                true
            }
        };

        if did_write && CSI_CHANNEL.is_empty() && logs_empty {
            let _ = driver.flush().await;
        }
    }
}

pub fn reset_global_log_drops() {
    #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
    {
        LOG_DROPPED_PACKETS.store(0, Ordering::Relaxed);
    }
}