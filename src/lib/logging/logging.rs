use embedded_io_async::Write;
use esp_hal::peripherals::Peripherals;
use heapless::String;
use portable_atomic::{AtomicU8, Ordering};
use postcard::experimental::max_size::MaxSize;

#[cfg(all(
    any(feature = "uart", feature = "jtag-serial", feature = "auto"),
    feature = "async-print"
))]
mod csi_interface {
    use crate::csi::CSIDataPacket;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
    use portable_atomic::AtomicU32;
    pub static CSI_CHANNEL: Channel<CriticalSectionRawMutex, CSIDataPacket, 2> = Channel::new();
    #[cfg(feature = "statistics")]
    pub static LOG_DROPPED_PACKETS: AtomicU32 = AtomicU32::new(0);
}

#[cfg(all(
    any(feature = "uart", feature = "jtag-serial", feature = "auto"),
    feature = "async-print"
))]
pub use csi_interface::{CSI_CHANNEL};
#[cfg(feature = "statistics")]
pub use csi_interface::LOG_DROPPED_PACKETS;

static LOG_MODE: AtomicU8 = AtomicU8::new(LogMode::Text as u8);

/// Return the number of CSI log packets dropped by the async logging channel.
///
/// Returns `0` when async logging or statistics are not enabled.
pub fn get_log_packet_drops() -> u32 {
    #[cfg(all(
        any(feature = "uart", feature = "jtag-serial", feature = "auto"),
        feature = "async-print",
        feature = "statistics"
    ))]
    {
        LOG_DROPPED_PACKETS.load(Ordering::Relaxed)
    }
    #[cfg(not(all(
        any(feature = "uart", feature = "jtag-serial", feature = "auto"),
        feature = "async-print",
        feature = "statistics"
    )))]
    {
        0
    }
}

#[cfg(all(feature = "println", feature = "async-print"))]
mod log_impl {
    use core::fmt::Write;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
    use heapless::String;

    pub static LOG_CHANNEL: Channel<CriticalSectionRawMutex, String<256>, 16> = Channel::new();

    struct EspLogger;

    impl log::Log for EspLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Info
        }

        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                let mut text: String<256> = String::new();
                // Format the log line
                if write!(&mut text, "{}\r\n", record.args()).is_ok() {
                    // Try to send. If the channel is full, the log is dropped.
                    // This is safe and non-blocking.
                    let _ = LOG_CHANNEL.try_send(text);
                }
            }
        }
        fn flush(&self) {}
    }

    pub fn init_logger(level: log::LevelFilter) {
        static LOGGER: EspLogger = EspLogger;
        log::set_logger(&LOGGER).unwrap();
        log::set_max_level(level);
    }
}

#[cfg(all(feature = "defmt", feature = "async-print"))]
mod defmt_impl {
    use super::*;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};

    pub static DEFMT_CHANNEL: Channel<CriticalSectionRawMutex, [u8; 256], 16> = Channel::new();

    #[defmt::global_logger]
    struct AsyncDefmtBackend;

    unsafe impl defmt::Logger for AsyncDefmtBackend {
        fn acquire() {}
        fn release() {}
        fn flush() {}

        unsafe fn write(bytes: &[u8]) {
            #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
            {
                let _ = DEFMT_CHANNEL.try_send(bytes.try_into().unwrap());
            }
        }
    }
}

/// Logging output format for CSI packets.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogMode {
    /// Human-readable text output.
    Text,
    /// Postcard-serialized output (COBS-framed).
    Serialized,
    /// Compact CSV-style array list.
    ArrayList,
}

impl From<u8> for LogMode {
    fn from(value: u8) -> Self {
        match value {
            0 => LogMode::Text,
            1 => LogMode::Serialized,
            2 => LogMode::ArrayList,
            _ => LogMode::Text, // Default fallback
        }
    }
}

#[cfg(all(
    any(feature = "uart", feature = "jtag-serial", feature = "auto"),
    feature = "async-print"
))]
mod logging_impl {
    use embedded_io_async::{ErrorType, Write};
    use esp_hal::peripherals::Peripherals;
    #[cfg(all(any(feature = "jtag-serial", feature = "auto"), not(feature = "esp32")))]
    use esp_hal::usb_serial_jtag::UsbSerialJtag;
    use esp_hal::{
        uart::{Config, Uart},
        Async,
    };

    use crate::logging::logging::LogMode;

    /// Low-level logging backend (UART or USB JTAG).
    pub enum Backend {
        #[cfg(any(feature = "uart", feature = "auto"))]
        Uart(Uart<'static, Async>),
        #[cfg(all(any(feature = "jtag-serial", feature = "auto"), not(feature = "esp32")))]
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

                #[cfg(all(any(feature = "jtag-serial", feature = "auto"), not(feature = "esp32")))]
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

                #[cfg(all(any(feature = "jtag-serial", feature = "auto"), not(feature = "esp32")))]
                Self::Jtag(driver) => driver
                    .flush()
                    .await
                    .map_err(|_| embedded_io_async::ErrorKind::Other),
            }
            #[cfg(feature = "no-print")]
            Err(embedded_io_async::ErrorKind::Other)
        }
    }

    /// Async log output wrapper with selected `LogMode`.
    pub struct LogOutput {
        inner: Backend,
        pub log_mode: LogMode,
    }

    impl LogOutput {
        #[cfg(any(feature = "uart", feature = "auto"))]
        pub fn new_uart(periphs: Peripherals, log_mode: LogMode) -> Self {
            let raw_driver = Uart::new(periphs.UART0, Config::default().with_baudrate(115_200))
                .unwrap()
                .into_async();
            Self {
                inner: Backend::Uart(raw_driver),
                log_mode,
            }
        }

        #[cfg(all(any(feature = "jtag-serial", feature = "auto"), not(feature = "esp32")))]
        pub fn new_jtag(periphs: Peripherals, log_mode: LogMode) -> Self {
            let raw_driver = UsbSerialJtag::new(periphs.USB_DEVICE).into_async();
            Self {
                inner: Backend::Jtag(raw_driver),
                log_mode,
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

/// Logging macro that routes to `println!`/`defmt` based on features.
///
/// Uses async logging when `async-print` is enabled; otherwise writes directly.
#[macro_export]
macro_rules! log_ln {
    ($($arg:tt)*) => {{
        #[cfg(
            all(any(feature = "uart", feature = "jtag-serial", feature = "auto"), feature = "async-print")
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
        #[cfg(all(not(feature = "async-print"), any(feature = "uart", feature = "jtag-serial", feature = "auto")))]
        {
            #[cfg(feature = "println")]
            {
                esp_println::println!($($arg)*);
            }

            #[cfg(feature = "defmt")]
            {
                defmt::println!($($arg)*);
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

/// Print raw bytes to the active logging backend (async-print only).
pub fn print_raw_bytes(bytes: &[u8]) {
    #[cfg(all(
        any(feature = "uart", feature = "jtag-serial", feature = "auto"),
        feature = "async-print"
    ))]
    {
        use core::fmt::Write;
        let mut printer = esp_println::Printer;
        for chunk in bytes.chunks(64) {
            for &b in chunk {
                let _ = printer.write_char(b as char);
            }
        }
    }
}

/// Log raw bytes (Only for blocking printer).
#[macro_export]
macro_rules! log_raw {
    ($data:expr) => {{
        #[cfg(all(
            any(feature = "uart", feature = "jtag-serial", feature = "auto"),
            feature = "async-print"
        ))]
        {
            #[cfg(feature = "println")]
            {
                print_raw_bytes($data.as_ref());
            }

            #[cfg(feature = "defmt")]
            {
                defmt::write!("{}", $data);
            }
        }
    }};
}

use crate::csi::CSIDataPacket;

/// Log a CSI packet according to the selected `LogMode`.
///
/// In async mode this enqueues the packet; otherwise it prints immediately.
pub fn log_csi(packet: CSIDataPacket) {
    #[cfg(feature = "async-print")]
    {
        #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
        {
            match CSI_CHANNEL.try_send(packet) {
                Ok(_) => {}
                Err(_) => {
                    #[cfg(feature = "statistics")]
                    LOG_DROPPED_PACKETS.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        #[cfg(not(any(feature = "uart", feature = "jtag-serial", feature = "auto")))]
        {}
    }
    #[cfg(not(feature = "async-print"))]
    {
        #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
        {
            use core::sync::atomic::Ordering;

            match LogMode::from(LOG_MODE.load(Ordering::Relaxed)) {
                LogMode::Text => {
                    write_text_packet(packet);
                }
                LogMode::Serialized => {
                    write_serialized_packet(packet);
                }
                LogMode::ArrayList => {
                    write_text_array_packet(packet);
                }
            }
        }
        #[cfg(not(any(feature = "uart", feature = "jtag-serial", feature = "auto")))]
        {}
    }
}

#[cfg(all(
    feature = "async-print",
    any(feature = "uart", feature = "jtag-serial", feature = "auto")
))]
use crate::logging::logging::logging_impl::LogOutput;

/// Initialize the logging backend and spawn the async logger task.
///
/// In async mode this selects UART/JTAG automatically (if enabled) and
/// spawns `logger_backend`. In non-async mode it stores the `LogMode`.
pub fn init_logger(spawner: embassy_executor::Spawner, log_mode: LogMode) {
    #[cfg(feature = "async-print")]
    {
        #[cfg(feature = "println")]
        {
            log_impl::init_logger(log::LevelFilter::Info);
        }
        #[cfg(feature = "auto")]
        {
            #[cfg(not(feature = "esp32"))]
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
                    let driver = LogOutput::new_jtag(periphs, log_mode);
                    spawner.spawn(logger_backend(driver)).unwrap();
                } else {
                    let driver = LogOutput::new_uart(periphs, log_mode);
                    spawner.spawn(logger_backend(driver)).unwrap();
                }
            }
            #[cfg(feature = "esp32")]
            {
                let periphs = unsafe { Peripherals::steal() };
                let driver = LogOutput::new_uart(periphs, log_mode);
                spawner.spawn(logger_backend(driver)).unwrap();
            }
        }
        #[cfg(all(feature = "jtag-serial", not(feature = "esp32")))]
        {
            let periphs = unsafe { Peripherals::steal() };
            let driver = LogOutput::new_jtag(periphs, log_mode);
            spawner.spawn(logger_backend(driver)).unwrap();
        }
        #[cfg(feature = "uart")]
        {
            let periphs = unsafe { Peripherals::steal() };
            let driver = LogOutput::new_uart(periphs, log_mode);
            spawner.spawn(logger_backend(driver)).unwrap();
        }

        #[cfg(not(any(feature = "uart", feature = "jtag-serial", feature = "auto")))]
        {}
    }
    #[cfg(not(feature = "async-print"))]
    {
        #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
        {
            use core::sync::atomic::Ordering;

            LOG_MODE.store(log_mode as u8, Ordering::Relaxed);
        }
    }
}

#[cfg(feature = "async-print")]
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
#[cfg(not(feature = "async-print"))]
fn write_serialized_packet(packet: CSIDataPacket) {
    const PACKET_MAX_SIZE: usize = CSIDataPacket::POSTCARD_MAX_SIZE;
    const PACKET_BUF_SIZE: usize = PACKET_MAX_SIZE + (PACKET_MAX_SIZE / 254) + 1;

    let mut buf = [0u8; PACKET_BUF_SIZE];
    match postcard::to_slice_cobs(&packet, &mut buf) {
        Ok(cobs_slice) => {
            log_raw!(cobs_slice);
        }
        Err(_) => {}
    }
}

#[cfg(feature = "async-print")]
async fn write_text_array_packet(packet: CSIDataPacket, driver: &mut LogOutput) -> Result<(), ()> {
    use core::fmt::Write as FmtWrite;

    let mut buf = String::<64>::new();
    macro_rules! write_field {
        ($arg:expr) => {
            buf.clear();
            if write!(&mut buf, "{},", $arg).is_ok() {
                driver.write(buf.as_bytes()).await.map_err(|_| ())?;
            }
        };
    }
    macro_rules! write_first_field {
        ($arg:expr) => {
            buf.clear();
            if write!(&mut buf, "[{},", $arg).is_ok() {
                driver.write(buf.as_bytes()).await.map_err(|_| ())?;
            }
        };
    }
    macro_rules! write_last_field {
        ($arg:expr) => {
            buf.clear();
            if write!(&mut buf, "{}]\r\n", $arg).is_ok() {
                driver.write(buf.as_bytes()).await.map_err(|_| ())?;
            }
        };
    }

    write_first_field!(packet.sequence_number);
    write_field!(packet.rssi);
    write_field!(packet.rate);
    write_field!(packet.noise_floor);
    write_field!(packet.channel);
    write_field!(packet.timestamp);
    write_field!(packet.sig_len);
    write_field!(packet.rx_state);
    #[cfg(not(feature = "esp32c6"))]
    {
        write_field!(packet.secondary_channel);
        write_field!(packet.sgi);
        write_field!(packet.antenna);
        write_field!(packet.ampdu_cnt);
        write_field!(packet.sig_mode);
        write_field!(packet.mcs);
        write_field!(packet.bandwidth);
        write_field!(packet.smoothing);
        write_field!(packet.not_sounding);
        write_field!(packet.aggregation);
        write_field!(packet.stbc);
        write_field!(packet.fec_coding);
    }
    #[cfg(feature = "esp32c6")]
    {
        write_field!(packet.dump_len);
        write_field!(packet.he_sigb_len);
        write_field!(packet.cur_single_mpdu);
        write_field!(packet.cur_bb_format);
        write_field!(packet.rx_channel_estimate_info_vld);
        write_field!(packet.rx_channel_estimate_len);
        write_field!(packet.second);
        write_field!(packet.channel);
        write_field!(packet.is_group);
        write_field!(packet.rxend_state);
        write_field!(packet.rxmatch3);
        write_field!(packet.rxmatch2);
        write_field!(packet.rxmatch1);
        write_field!(packet.rxmatch0);
    }
    write_field!(packet.sig_len);
    write_last_field!(packet.csi_data_len);

    Ok(())
}
#[cfg(not(feature = "async-print"))]
fn write_text_array_packet(packet: CSIDataPacket) {
    use core::fmt::Write as FmtWrite;

    let mut buf = String::<64>::new();
    macro_rules! write_field {
        ($arg:expr) => {
            buf.clear();
            if write!(&mut buf, "{},", $arg).is_ok() {
                log_raw!(buf.as_str());
            }
        };
    }
    macro_rules! write_first_field {
        ($arg:expr) => {
            buf.clear();
            if write!(&mut buf, "[{},", $arg).is_ok() {
                log_raw!(buf.as_str());
            }
        };
    }
    macro_rules! write_last_field {
        ($arg:expr) => {
            buf.clear();
            if write!(&mut buf, "{}]\r\n", $arg).is_ok() {
                log_raw!(buf.as_str());
            }
        };
    }

    write_first_field!(packet.sequence_number);
    write_field!(packet.rssi);
    write_field!(packet.rate);
    write_field!(packet.noise_floor);
    write_field!(packet.channel);
    write_field!(packet.timestamp);
    write_field!(packet.sig_len);
    write_field!(packet.rx_state);
    #[cfg(not(feature = "esp32c6"))]
    {
        write_field!(packet.secondary_channel);
        write_field!(packet.sgi);
        write_field!(packet.antenna);
        write_field!(packet.ampdu_cnt);
        write_field!(packet.sig_mode);
        write_field!(packet.mcs);
        write_field!(packet.bandwidth);
        write_field!(packet.smoothing);
        write_field!(packet.not_sounding);
        write_field!(packet.aggregation);
        write_field!(packet.stbc);
        write_field!(packet.fec_coding);
    }
    #[cfg(feature = "esp32c6")]
    {
        write_field!(packet.dump_len);
        write_field!(packet.he_sigb_len);
        write_field!(packet.cur_single_mpdu);
        write_field!(packet.cur_bb_format);
        write_field!(packet.rx_channel_estimate_info_vld);
        write_field!(packet.rx_channel_estimate_len);
        write_field!(packet.second);
        write_field!(packet.channel);
        write_field!(packet.is_group);
        write_field!(packet.rxend_state);
        write_field!(packet.rxmatch3);
        write_field!(packet.rxmatch2);
        write_field!(packet.rxmatch1);
        write_field!(packet.rxmatch0);
    }
    write_field!(packet.sig_len);
    write_last_field!(packet.csi_data_len);
}

#[cfg(feature = "async-print")]
async fn write_text_packet(packet: CSIDataPacket, driver: &mut LogOutput) -> Result<(), ()> {
    use core::fmt::Write as FmtWrite;

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
                dt.year,
                dt.month,
                dt.day,
                dt.hour,
                dt.minute,
                dt.second,
                dt.millisecond
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
            send_line!(
                "rx channel estimate info vld: {}\r\n",
                packet.rx_channel_estimate_info_vld
            );
            send_line!(
                "rx channel estimate len: {}\r\n",
                packet.rx_channel_estimate_len
            );
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

#[cfg(not(feature = "async-print"))]
fn write_text_packet(packet: CSIDataPacket) {
    use core::fmt::Write as FmtWrite;

    let mut buf = String::<128>::new();

    if let Some(dt) = &packet.date_time {
        log_ln!(
            "Recieved at {:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            dt.year,
            dt.month,
            dt.day,
            dt.hour,
            dt.minute,
            dt.second,
            dt.millisecond
        );
    }

    log_ln!(
        "mac: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        packet.mac[0],
        packet.mac[1],
        packet.mac[2],
        packet.mac[3],
        packet.mac[4],
        packet.mac[5]
    );

    log_ln!("sequence number: {}", packet.sequence_number);
    log_ln!("rssi: {}", packet.rssi);
    log_ln!("rate: {}", packet.rate);
    log_ln!("noise floor: {}", packet.noise_floor);
    log_ln!("channel: {}", packet.channel);
    log_ln!("timestamp: {}", packet.timestamp);
    log_ln!("sig len: {}", packet.sig_len);
    log_ln!("rx state: {}", packet.rx_state);
    #[cfg(not(feature = "esp32c6"))]
    {
        log_ln!("secondary channel: {}", packet.secondary_channel);
        log_ln!("sgi: {}", packet.sgi);
        log_ln!("ant: {}", packet.antenna);
        log_ln!("ampdu cnt: {}", packet.ampdu_cnt);
        log_ln!("sig_mode: {}", packet.sig_mode);
        log_ln!("mcs: {}", packet.mcs);
        log_ln!("cwb: {}", packet.bandwidth);
        log_ln!("smoothing: {}", packet.smoothing);
        log_ln!("not sounding: {}", packet.not_sounding);
        log_ln!("aggregation: {}", packet.aggregation);
        log_ln!("stbc: {}", packet.stbc);
        log_ln!("fec coding: {}", packet.fec_coding);
    }
    #[cfg(feature = "esp32c6")]
    {
        log_ln!("dump len: {}", packet.dump_len);
        log_ln!("he sigb len: {}", packet.he_sigb_len);
        log_ln!("cur single mpdu: {}", packet.cur_single_mpdu);
        log_ln!("cur bb format: {}", packet.cur_bb_format);
        log_ln!(
            "rx channel estimate info vld: {}",
            packet.rx_channel_estimate_info_vld
        );
        log_ln!(
            "rx channel estimate len: {}",
            packet.rx_channel_estimate_len
        );
        log_ln!("time seconds: {}", packet.second);
        log_ln!("channel: {}", packet.channel);
        log_ln!("is group: {}", packet.is_group);
        log_ln!("rxend state: {}", packet.rxend_state);
        log_ln!("rxmatch3: {}", packet.rxmatch3);
        log_ln!("rxmatch2: {}", packet.rxmatch2);
        log_ln!("rxmatch1: {}", packet.rxmatch1);
        log_ln!("rxmatch0: {}", packet.rxmatch0);
    }

    log_ln!("sig_len: {}", packet.sig_len);
    log_ln!("data length: {}", packet.csi_data_len);

    log_ln!("csi raw data: [{:X?}]", packet.csi_data);
}

#[cfg(all(
    any(feature = "uart", feature = "jtag-serial", feature = "auto"),
    feature = "async-print"
))]
#[embassy_executor::task]
/// Async logger backend task that drains CSI/log channels and writes output.
pub async fn logger_backend(mut driver: LogOutput) {
    use embassy_futures::select::{select, Either};
    use embedded_io_async::Write;

    loop {
        let csi_future = CSI_CHANNEL.receive();

        #[cfg(all(feature = "println", not(feature = "defmt")))]
        let log_future = log_impl::LOG_CHANNEL.receive();

        #[cfg(feature = "defmt")]
        let log_future = defmt_impl::DEFMT_CHANNEL.receive();

        #[cfg(not(any(feature = "println", feature = "defmt")))]
        let log_future = embassy_futures::pending::<usize>();

        match select(csi_future, log_future).await {
            Either::First(packet) => {
                let _ = match driver.log_mode {
                    LogMode::Serialized => write_serialized_packet(packet, &mut driver).await,
                    LogMode::ArrayList => write_text_array_packet(packet, &mut driver).await,
                    LogMode::Text => write_text_packet(packet, &mut driver).await,
                };

                if CSI_CHANNEL.is_empty() {
                    let _ = driver.flush().await;
                }
            }
            Either::Second(message) => {
                // 'message' is a heapless::String.
                // It is guaranteed to be a single linear chunk of memory.
                let _ = driver.write_all(message.as_bytes()).await;

                // Only flush if no more logs are pending
                if log_impl::LOG_CHANNEL.is_empty() {
                    let _ = driver.flush().await;
                }
            }
        }
    }
}

/// Reset the global dropped-log counter (statistics feature only).
pub fn reset_global_log_drops() {
    #[cfg(all(
        any(feature = "uart", feature = "jtag-serial", feature = "auto"),
        feature = "async-print"
    ))]
    {
        #[cfg(feature = "statistics")]
        LOG_DROPPED_PACKETS.store(0, Ordering::Relaxed);
    }
}
