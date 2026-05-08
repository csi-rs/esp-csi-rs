//! Logging backends and CSI emission paths.
//!
//! Routes both human-readable text logs and binary CSI packets to the
//! configured transport. Two write paths exist:
//!
//! - **Sync path** (default): the Wi-Fi callback formats and writes
//!   directly to the transport. Lower DRAM cost, higher in-callback
//!   latency.
//! - **Async path** (`async-print` feature): the callback enqueues into
//!   a bounded channel and a dedicated Embassy task drains it.
//!
//! Transport selection (`println!`, JTAG-serial, UART, no-op, or
//! `defmt`) is driven by the crate's feature flags. When `defmt` is
//! enabled, `esp-println`'s `defmt-espflash` backend is the global
//! logger — defmt frames stream over the same UART/USB-Serial-JTAG
//! channel as `println!` and are decoded by `espflash --log-format defmt`.

#[cfg(feature = "async-print")]
use embedded_io_async::Write;
use esp_hal::peripherals::Peripherals;
use heapless::String;
use portable_atomic::{AtomicBool, AtomicU8, Ordering};
use postcard::experimental::max_size::MaxSize;

#[cfg(all(feature = "defmt", not(feature = "async-print")))]
use esp_println as _;

#[allow(dead_code)]
const CSI_LOG_CHANNEL_CAPACITY: usize = 32;
#[allow(dead_code)]
const TEXT_LOG_CHANNEL_CAPACITY: usize = 64;
#[allow(dead_code)]
const DEFMT_LOG_CHANNEL_CAPACITY: usize = 64;
const fn parse_u32(s: &str) -> u32 {
    let bytes = s.as_bytes();
    let mut result: u32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        result = result * 10 + (bytes[i] - b'0') as u32;
        i += 1;
    }
    result
}
const UART_LOG_BAUDRATE: u32 = parse_u32(env!("UART_LOG_BAUDRATE"));

#[cfg(all(
    any(feature = "uart", feature = "jtag-serial", feature = "auto"),
    feature = "async-print"
))]
mod csi_interface {
    use crate::csi::CSIDataPacket;
    use crate::logging::logging::CSI_LOG_CHANNEL_CAPACITY;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
    #[cfg(feature = "statistics")]
    use portable_atomic::AtomicU32;
    /// Bounded channel between the WiFi callback (producer) and the
    /// async drainer task (consumer) on the async-print path.
    pub static CSI_CHANNEL: Channel<
        CriticalSectionRawMutex,
        CSIDataPacket,
        CSI_LOG_CHANNEL_CAPACITY,
    > = Channel::new();
    /// Counter incremented when the WiFi callback fails to enqueue a CSI
    /// packet because [`CSI_CHANNEL`] is full.
    #[cfg(feature = "statistics")]
    pub static LOG_DROPPED_PACKETS: AtomicU32 = AtomicU32::new(0);
}

#[cfg(all(
    any(feature = "uart", feature = "jtag-serial", feature = "auto"),
    feature = "async-print"
))]
pub use csi_interface::CSI_CHANNEL;
#[cfg(all(feature = "statistics", feature = "async-print"))]
pub use csi_interface::LOG_DROPPED_PACKETS;

static LOG_MODE: AtomicU8 = AtomicU8::new(LogMode::Text as u8);
static ROLE: AtomicU8 = AtomicU8::new(Role::Sta as u8);
static ESP_CSI_TOOL_HEADER_PRINTED: AtomicBool = AtomicBool::new(false);
/// When non-zero, the EspCsiTool formatter emits at most this many `i8`
/// samples in column 26 and reports `len` accordingly. Mirrors
/// ESP32-CSI-Tool's `CONFIG_SHOULD_COLLECT_ONLY_LLTF=128` behavior — at
/// 460800 baud, sniffer mode picks up a mix of 11b / 11n / HT-LTF frames
/// whose CSI lengths vary, blowing line size past the UART ceiling.
/// Capping to 128 keeps every line at ~475B and lets sniffer hit the
/// same PPS as a peripheral seeing only one PHY type.
///
/// Default 0 = no cap (emit full `csi_data`).
static CSI_TOOL_EMIT_CAP: portable_atomic::AtomicU16 = portable_atomic::AtomicU16::new(0);

/// Cap the number of `i8` samples emitted in column 26 of an EspCsiTool line.
///
/// Pass `0` to disable the cap (emit all captured samples — the default).
/// Pass `128` to match ESP32-CSI-Tool's `CONFIG_SHOULD_COLLECT_ONLY_LLTF`
/// behavior, which produces uniform ~475B lines in sniffer mode and keeps
/// PPS at the UART ceiling regardless of the captured PHY type.
pub fn set_csi_tool_emit_cap(cap: u16) {
    CSI_TOOL_EMIT_CAP.store(cap, Ordering::Relaxed);
}

/// Role string emitted in column 2 of the ESP32-CSI-Tool CSV format.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Wi-Fi station role (`STA`).
    Sta = 0,
    /// Soft-AP role (`AP`).
    Ap = 1,
    /// Promiscuous sniffer role (`PASSIVE`).
    Passive = 2,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Sta => "STA",
            Role::Ap => "AP",
            Role::Passive => "PASSIVE",
        }
    }
}

impl From<u8> for Role {
    fn from(value: u8) -> Self {
        match value {
            1 => Role::Ap,
            2 => Role::Passive,
            _ => Role::Sta,
        }
    }
}

/// Set the role string used by `LogMode::EspCsiTool`.
///
/// Call once before starting the node when emitting the ESP32-CSI-Tool CSV
/// format. Defaults to `Role::Sta`.
pub fn set_role(role: Role) {
    ROLE.store(role as u8, Ordering::Relaxed);
}

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
    use crate::logging::logging::TEXT_LOG_CHANNEL_CAPACITY;
    use core::fmt::Write;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
    use heapless::String;

    pub static LOG_CHANNEL: Channel<
        CriticalSectionRawMutex,
        String<256>,
        TEXT_LOG_CHANNEL_CAPACITY,
    > = Channel::new();

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
        // `log::set_logger` requires the `std` feature; use the unsafe
        // `set_logger_racy` which is available in no-std environments.
        unsafe { log::set_logger_racy(&LOGGER) }.unwrap();
        unsafe { log::set_max_level_racy(level) };
    }
}

#[cfg(all(feature = "defmt", feature = "async-print"))]
mod defmt_impl {
    use super::*;
    use crate::logging::logging::DEFMT_LOG_CHANNEL_CAPACITY;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};

    pub static DEFMT_CHANNEL: Channel<
        CriticalSectionRawMutex,
        [u8; 256],
        DEFMT_LOG_CHANNEL_CAPACITY,
    > = Channel::new();

    #[defmt::global_logger]
    struct AsyncDefmtBackend;

    unsafe impl defmt::Logger for AsyncDefmtBackend {
        fn acquire() {}
        unsafe fn release() {}
        unsafe fn flush() {}

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
    /// ESP32-CSI-Tool compatible CSV (`CSI_DATA,...` lines, 26 columns, header
    /// printed once at startup). See the crate-level documentation for the
    /// exact field layout.
    EspCsiTool,
}

impl From<u8> for LogMode {
    fn from(value: u8) -> Self {
        match value {
            0 => LogMode::Text,
            1 => LogMode::Serialized,
            2 => LogMode::ArrayList,
            3 => LogMode::EspCsiTool,
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

    use crate::logging::logging::UART_LOG_BAUDRATE;

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
    }

    impl LogOutput {
        #[cfg(any(feature = "uart", feature = "auto"))]
        pub fn new_uart(periphs: Peripherals) -> Self {
            let raw_driver =
                Uart::new(periphs.UART0, Config::default().with_baudrate(UART_LOG_BAUDRATE))
                .unwrap()
                .into_async();
            Self {
                inner: Backend::Uart(raw_driver),
            }
        }

        #[cfg(all(any(feature = "jtag-serial", feature = "auto"), not(feature = "esp32")))]
        pub fn new_jtag(periphs: Peripherals) -> Self {
            let raw_driver = UsbSerialJtag::new(periphs.USB_DEVICE).into_async();
            Self {
                inner: Backend::Jtag(raw_driver),
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
#[allow(unused_variables)]
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

/// Log raw bytes without any added newline (blocking path only; no-op in async-print mode).
///
/// `defmt` is a structured/framed logger and cannot stream raw binary, so it
/// is intentionally excluded here.
#[macro_export]
macro_rules! log_raw {
    ($data:expr) => {{
        #[cfg(all(
            any(feature = "uart", feature = "jtag-serial", feature = "auto"),
            not(feature = "async-print"),
            feature = "println"
        ))]
        {
            use core::fmt::Write as _FmtWrite;
            let mut _printer = esp_println::Printer;
            for &_b in AsRef::<[u8]>::as_ref(&$data) {
                let _ = _printer.write_char(_b as char);
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
                LogMode::EspCsiTool => {
                    write_csi_tool_packet(packet);
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
    LOG_MODE.store(log_mode as u8, Ordering::Relaxed);
    ESP_CSI_TOOL_HEADER_PRINTED.store(false, Ordering::Relaxed);
    // In sync mode the WiFi callback is the CSI consumer (it formats and
    // writes inline). Enable the publish gate up front so it starts running.
    // Users can call `set_csi_logging_enabled(false)` after `init_logger`
    // to keep the logger backend up but suppress CSI output.
    #[cfg(not(feature = "async-print"))]
    crate::set_csi_logging_enabled(true);
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
                #[cfg(feature = "esp32c5")]
                const USB_DEVICE_INT_RAW: *const u32 = 0x600D_F008 as *const u32;
                #[cfg(feature = "esp32c6")]
                const USB_DEVICE_INT_RAW: *const u32 = 0x6000f008 as *const u32;
                #[cfg(feature = "esp32s3")]
                const USB_DEVICE_INT_RAW: *const u32 = 0x60038000 as *const u32;

                const SOF_INT_MASK: u32 = 0b10;
                let res = unsafe { (USB_DEVICE_INT_RAW.read_volatile() & SOF_INT_MASK) != 0 };
                if res == true {
                    let driver = LogOutput::new_jtag(periphs);
                    spawner.spawn(logger_backend(driver)).unwrap();
                } else {
                    let driver = LogOutput::new_uart(periphs);
                    spawner.spawn(logger_backend(driver)).unwrap();
                }
            }
            #[cfg(feature = "esp32")]
            {
                let periphs = unsafe { Peripherals::steal() };
                let driver = LogOutput::new_uart(periphs);
                spawner.spawn(logger_backend(driver)).unwrap();
            }
        }
        #[cfg(all(feature = "jtag-serial", not(feature = "esp32")))]
        {
            let periphs = unsafe { Peripherals::steal() };
            let driver = LogOutput::new_jtag(periphs);
            spawner.spawn(logger_backend(driver)).unwrap();
        }
        #[cfg(feature = "uart")]
        {
            let periphs = unsafe { Peripherals::steal() };
            let driver = LogOutput::new_uart(periphs);
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
        // Reconfigure UART0 at the compiled-in baud rate so that esp_println
        // (which writes directly to the UART hardware via ROM) uses the correct
        // speed. The driver is forgotten rather than dropped so the hardware
        // clock-divider register is not reset before esp_println first writes.
        #[cfg(any(feature = "uart", feature = "auto"))]
        {
            let periphs = unsafe { Peripherals::steal() };
            if let Ok(uart) = esp_hal::uart::Uart::new(
                periphs.UART0,
                esp_hal::uart::Config::default().with_baudrate(UART_LOG_BAUDRATE),
            ) {
                core::mem::forget(uart);
            }
        }
        // `spawner` is unused in the sync path now that the UART writer runs
        // inline in the WiFi callback. Keep the parameter for API stability —
        // async-print still uses it.
        let _ = spawner;
    }
}

/// Set the logging output mode at runtime.
///
/// This updates the global mode used by `log_csi` formatting paths.
pub fn set_log_mode(log_mode: LogMode) {
    LOG_MODE.store(log_mode as u8, Ordering::Relaxed);
    ESP_CSI_TOOL_HEADER_PRINTED.store(false, Ordering::Relaxed);
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
    if let Ok(cobs_slice) = postcard::to_slice_cobs(&packet, &mut buf) {
        let _ = cobs_slice;
        log_raw!(cobs_slice);
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
    #[allow(unused_macros)]
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
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
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
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    {
        write_field!(packet.dump_len);
        #[cfg(feature = "esp32c6")]
        write_field!(packet.he_sigb_len);
        #[cfg(feature = "esp32c6")]
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
        #[cfg(feature = "esp32c6")]
        write_field!(packet.rxmatch0);
    }
    write_field!(packet.sig_len);
    write_field!(packet.csi_data_len);

    driver.write(b"[").await.map_err(|_| ())?;
    let data_len = packet.csi_data.len();
    for (i, val) in packet.csi_data.iter().enumerate() {
        buf.clear();
        if i + 1 < data_len {
            let _ = write!(&mut buf, "{},", val);
        } else {
            let _ = write!(&mut buf, "{}", val);
        }
        driver.write(buf.as_bytes()).await.map_err(|_| ())?;
    }
    driver.write(b"]]
").await.map_err(|_| ())?;

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
    #[allow(unused_macros)]
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
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
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
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    {
        write_field!(packet.dump_len);
        #[cfg(feature = "esp32c6")]
        write_field!(packet.he_sigb_len);
        #[cfg(feature = "esp32c6")]
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
        #[cfg(feature = "esp32c6")]
        write_field!(packet.rxmatch0);
    }
    write_field!(packet.sig_len);
    write_field!(packet.csi_data_len);

    log_raw!("[");
    let data_len = packet.csi_data.len();
    for (i, val) in packet.csi_data.iter().enumerate() {
        buf.clear();
        if i + 1 < data_len {
            let _ = write!(&mut buf, "{},", val);
        } else {
            let _ = write!(&mut buf, "{}", val);
        }
        log_raw!(buf.as_str());
    }
    log_raw!("]]
");
}

/// Header line emitted once at the top of an ESP32-CSI-Tool capture.
#[allow(dead_code)]
const ESP_CSI_TOOL_HEADER: &str = "type,role,mac,rssi,rate,sig_mode,mcs,bandwidth,smoothing,not_sounding,aggregation,stbc,fec_coding,sgi,noise_floor,ampdu_cnt,channel,secondary_channel,local_timestamp,ant,sig_len,rx_state,real_time_set,real_timestamp,len,CSI_DATA\n";

/// Write `val` followed by a single space into `buf[*offset..]`, advancing
/// `*offset`. Replaces `write!(&mut elem, "{} ", val) + copy` per i8 in the
/// CSI body — `core::fmt` for an `i8` is several hundred cycles per value
/// due to dynamic-dispatched `Formatter`/`Arguments` machinery, and the body
/// runs that 128–384 times per CSI line. This direct lookup-style encoder
/// reduces it to ~10 instructions.
///
/// Caller must ensure at least 5 bytes free (worst case `-128 ` = 5 bytes).
#[inline(always)]
fn write_i8_space(buf: &mut [u8], offset: &mut usize, val: i8) {
    let mut o = *offset;
    let mut n: i16 = val as i16;
    if n < 0 {
        buf[o] = b'-';
        o += 1;
        n = -n;
    }
    let n = n as u16;
    if n >= 100 {
        buf[o] = b'0' + (n / 100) as u8;
        buf[o + 1] = b'0' + ((n / 10) % 10) as u8;
        buf[o + 2] = b'0' + (n % 10) as u8;
        o += 3;
    } else if n >= 10 {
        buf[o] = b'0' + (n / 10) as u8;
        buf[o + 1] = b'0' + (n % 10) as u8;
        o += 2;
    } else {
        buf[o] = b'0' + n as u8;
        o += 1;
    }
    buf[o] = b' ';
    *offset = o + 1;
}

/// Append a u32 in decimal, MSD first, to `buf[*pos..]`. Up to 10 digits.
#[inline(always)]
fn write_u32(buf: &mut [u8], pos: &mut usize, mut n: u32) {
    if n == 0 {
        buf[*pos] = b'0';
        *pos += 1;
        return;
    }
    let mut tmp = [0u8; 10];
    let mut i = 0;
    while n > 0 {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    let mut p = *pos;
    while i > 0 {
        i -= 1;
        buf[p] = tmp[i];
        p += 1;
    }
    *pos = p;
}

/// Append an i32 in decimal (with sign).
#[inline(always)]
fn write_i32(buf: &mut [u8], pos: &mut usize, n: i32) {
    if n < 0 {
        buf[*pos] = b'-';
        *pos += 1;
        // Cast through i64 to handle i32::MIN safely.
        write_u32(buf, pos, (-(n as i64)) as u32);
    } else {
        write_u32(buf, pos, n as u32);
    }
}

/// Append a u32 zero-padded to width 6 (used for `real_timestamp` fractional).
#[inline(always)]
fn write_u32_pad6(buf: &mut [u8], pos: &mut usize, mut n: u32) {
    let mut tmp = [b'0'; 6];
    let mut i = 0;
    while n > 0 && i < 6 {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    let mut p = *pos;
    for j in (0..6).rev() {
        buf[p] = tmp[j];
        p += 1;
    }
    *pos = p;
}

/// Append a u8 as 2-char uppercase hex.
#[inline(always)]
fn write_hex_u8(buf: &mut [u8], pos: &mut usize, n: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let p = *pos;
    buf[p] = HEX[(n >> 4) as usize];
    buf[p + 1] = HEX[(n & 0x0f) as usize];
    *pos = p + 2;
}

/// Append a literal byte slice (no length prefix).
#[inline(always)]
fn write_slice(buf: &mut [u8], pos: &mut usize, s: &[u8]) {
    let p = *pos;
    buf[p..p + s.len()].copy_from_slice(s);
    *pos = p + s.len();
}

/// Format an EspCsiTool CSV line into `buf` (one packet) using only byte-level
/// integer writers — no `core::fmt`. Returns the number of bytes written.
///
/// Worst-case line: 200 prefix + 612 i8 × 5 + 2 trailer = ~3060 B.
/// Caller must pass a buffer of at least that size.
fn format_csi_tool_into(packet: &CSIDataPacket, buf: &mut [u8]) -> usize {
    let role = Role::from(ROLE.load(Ordering::Relaxed)).as_str();
    let real_time_set: u8 = if packet.date_time.is_some() { 1 } else { 0 };
    let real_secs = packet.timestamp / 1_000_000;
    let real_usecs = packet.timestamp % 1_000_000;

    let mut p = 0usize;

    write_slice(buf, &mut p, b"CSI_DATA,");
    write_slice(buf, &mut p, role.as_bytes());
    buf[p] = b',';
    p += 1;
    write_hex_u8(buf, &mut p, packet.mac[0]);
    buf[p] = b':';
    p += 1;
    write_hex_u8(buf, &mut p, packet.mac[1]);
    buf[p] = b':';
    p += 1;
    write_hex_u8(buf, &mut p, packet.mac[2]);
    buf[p] = b':';
    p += 1;
    write_hex_u8(buf, &mut p, packet.mac[3]);
    buf[p] = b':';
    p += 1;
    write_hex_u8(buf, &mut p, packet.mac[4]);
    buf[p] = b':';
    p += 1;
    write_hex_u8(buf, &mut p, packet.mac[5]);
    buf[p] = b',';
    p += 1;
    write_i32(buf, &mut p, packet.rssi);
    buf[p] = b',';
    p += 1;
    write_u32(buf, &mut p, packet.rate);
    buf[p] = b',';
    p += 1;

    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    {
        write_u32(buf, &mut p, packet.sig_mode);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.mcs);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.bandwidth);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.smoothing);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.not_sounding);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.aggregation);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.stbc);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.fec_coding);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.sgi);
        buf[p] = b',';
        p += 1;
        write_i32(buf, &mut p, packet.noise_floor);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.ampdu_cnt);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.channel);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.secondary_channel);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.timestamp);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.antenna);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.sig_len);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.rx_state);
        buf[p] = b',';
        p += 1;
    }
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    {
        write_slice(buf, &mut p, b"0,0,0,0,0,0,0,0,0,");
        write_i32(buf, &mut p, packet.noise_floor);
        buf[p] = b',';
        p += 1;
        write_slice(buf, &mut p, b"0,");
        write_u32(buf, &mut p, packet.channel);
        buf[p] = b',';
        p += 1;
        write_slice(buf, &mut p, b"0,");
        write_u32(buf, &mut p, packet.timestamp);
        buf[p] = b',';
        p += 1;
        write_slice(buf, &mut p, b"0,");
        write_u32(buf, &mut p, packet.sig_len);
        buf[p] = b',';
        p += 1;
        write_u32(buf, &mut p, packet.rx_state);
        buf[p] = b',';
        p += 1;
    }
    write_u32(buf, &mut p, real_time_set as u32);
    buf[p] = b',';
    p += 1;
    write_u32(buf, &mut p, real_secs);
    buf[p] = b'.';
    p += 1;
    write_u32_pad6(buf, &mut p, real_usecs);
    buf[p] = b',';
    p += 1;
    // If a cap is set, report the capped count in column 25 so the line is
    // self-consistent — column 26's array length always matches column 25.
    let cap = CSI_TOOL_EMIT_CAP.load(Ordering::Relaxed) as usize;
    let actual_len = packet.csi_data.len();
    let emit_len = if cap == 0 { actual_len } else { actual_len.min(cap) };

    write_u32(buf, &mut p, emit_len as u32);
    buf[p] = b',';
    p += 1;
    buf[p] = b'[';
    p += 1;

    // Body: i8 + space, with worst-case 5-byte reservation per value.
    let body_cap = buf.len().saturating_sub(2); // reserve `]\n`
    for &val in packet.csi_data.iter().take(emit_len) {
        if p + 5 > body_cap {
            break;
        }
        write_i8_space(buf, &mut p, val);
    }
    buf[p] = b']';
    buf[p + 1] = b'\n';
    p + 2
}

#[cfg(feature = "async-print")]
async fn write_csi_tool_packet(packet: CSIDataPacket, driver: &mut LogOutput) -> Result<(), ()> {
    if !ESP_CSI_TOOL_HEADER_PRINTED.swap(true, Ordering::Relaxed) {
        driver
            .write(ESP_CSI_TOOL_HEADER.as_bytes())
            .await
            .map_err(|_| ())?;
    }

    // Single-task formatter scratch: only `logger_backend` calls this fn,
    // and embassy executors are single-threaded so no concurrent access.
    static mut SCRATCH: [u8; 3328] = [0u8; 3328];
    let scratch = unsafe { &mut *core::ptr::addr_of_mut!(SCRATCH) };
    let n = format_csi_tool_into(&packet, scratch);
    driver.write(&scratch[..n]).await.map_err(|_| ())?;
    Ok(())
}

/// Write `bytes` directly to the UART0 hardware FIFO register, bypassing
/// `esp_println::Printer`. esp-println's ESP32 UART path calls the ROM
/// function `uart_tx_one_char` once per byte under a critical-section
/// lock — that's ~2-3 µs of per-byte dispatch overhead on top of the
/// actual UART transmission, which adds up to ~1.5 ms per ~490-byte CSI
/// line. Direct register access drops that to a handful of CPU cycles.
///
/// We just spin-wait for the TX FIFO to have space, then push the byte.
/// No critical section: there is exactly one writer (the sync log path
/// runs from `node_task`); no other code in this crate writes to UART0.
#[cfg(all(
    not(feature = "async-print"),
    feature = "esp32",
    any(feature = "uart", feature = "auto")
))]
#[inline]
fn uart0_write_bytes_fast(bytes: &[u8]) {
    // ESP32 UART0 base = 0x3FF40000. The TX FIFO is at offset 0x00. The
    // STATUS register is at offset 0x1C, with TXFIFO_CNT in bits [23:16]
    // (8 bits, max 128 = FIFO size).
    //
    // Bulk push: read STATUS once, then write `free` bytes back-to-back
    // before re-checking. Cuts bus traffic vs. the byte-at-a-time variant
    // and stops the FIFO from ever momentarily underrunning between
    // consecutive lines (the drainer's back-to-back pattern keeps it
    // pressurized).
    const UART0_FIFO: *mut u32 = 0x3FF40000 as *mut u32;
    const UART0_STATUS: *const u32 = 0x3FF4001C as *const u32;
    const FIFO_SIZE: usize = 128;
    let mut i = 0usize;
    while i < bytes.len() {
        let used = unsafe { (UART0_STATUS.read_volatile() >> 16) & 0xFF } as usize;
        if used >= FIFO_SIZE {
            // FIFO full — spin until at least one byte drains. We don't
            // need to be precise; the next iteration re-reads STATUS.
            continue;
        }
        let free = FIFO_SIZE - used;
        let chunk = core::cmp::min(free, bytes.len() - i);
        let end = i + chunk;
        while i < end {
            unsafe { UART0_FIFO.write_volatile(bytes[i] as u32) };
            i += 1;
        }
    }
}

/// Fallback for non-ESP32 sync builds: use `log_raw!` semantics via
/// `esp_println::Printer::write_bytes` in one batched call.
#[cfg(all(
    not(feature = "async-print"),
    feature = "println",
    any(feature = "uart", feature = "jtag-serial", feature = "auto"),
    not(feature = "esp32")
))]
#[allow(dead_code)]
fn uart0_write_bytes_fast(bytes: &[u8]) {
    esp_println::Printer::write_bytes(bytes);
}

/// Cumulative microseconds spent formatting CSI packets on the sync write
/// path. Read by examples to compute the format vs UART-write split.
#[cfg(not(feature = "async-print"))]
pub static SYNC_FORMAT_US: portable_atomic::AtomicU64 = portable_atomic::AtomicU64::new(0);
/// Cumulative microseconds spent writing formatted CSI bytes to the
/// transport on the sync write path.
#[cfg(not(feature = "async-print"))]
pub static SYNC_WRITE_US: portable_atomic::AtomicU64 = portable_atomic::AtomicU64::new(0);
/// Number of CSI packets emitted on the sync write path; the divisor used
/// with [`SYNC_FORMAT_US`] / [`SYNC_WRITE_US`] for per-packet averages.
#[cfg(not(feature = "async-print"))]
pub static SYNC_PKT_COUNT: portable_atomic::AtomicU64 = portable_atomic::AtomicU64::new(0);

#[cfg(not(feature = "async-print"))]
fn write_csi_tool_packet(packet: CSIDataPacket) {
    // Format + spin UART0 directly in the WiFi callback context. This is the
    // hot path that achieves baud-bound PPS — moving the spin out to a
    // separate embassy task introduces wake/schedule latency between lines
    // that caps throughput well below the UART ceiling.
    //
    // The associated risk (heap exhaustion via ESP-NOW VecDeque growth while
    // the callback blocks UART) is mitigated by the sniffer-mode ESP-NOW
    // VecDeque drainer in `lib::run` — that task `esp_now.receive()`s and
    // drops, bounding the queue.
    static mut SCRATCH: [u8; 3328] = [0u8; 3328];
    let scratch = unsafe { &mut *core::ptr::addr_of_mut!(SCRATCH) };

    if !ESP_CSI_TOOL_HEADER_PRINTED.swap(true, Ordering::Relaxed) {
        #[cfg(all(feature = "esp32", any(feature = "uart", feature = "auto")))]
        uart0_write_bytes_fast(ESP_CSI_TOOL_HEADER.as_bytes());
        #[cfg(not(all(feature = "esp32", any(feature = "uart", feature = "auto"))))]
        log_raw!(ESP_CSI_TOOL_HEADER);
    }

    let t0 = embassy_time::Instant::now();
    let _n = format_csi_tool_into(&packet, scratch);
    let t1 = embassy_time::Instant::now();

    #[cfg(all(feature = "esp32", any(feature = "uart", feature = "auto")))]
    uart0_write_bytes_fast(&scratch[.._n]);
    #[cfg(not(all(feature = "esp32", any(feature = "uart", feature = "auto"))))]
    log_raw!(&scratch[.._n]);

    let t2 = embassy_time::Instant::now();
    SYNC_FORMAT_US.fetch_add(t1.duration_since(t0).as_micros() as u64, Ordering::Relaxed);
    SYNC_WRITE_US.fetch_add(t2.duration_since(t1).as_micros() as u64, Ordering::Relaxed);
    SYNC_PKT_COUNT.fetch_add(1, Ordering::Relaxed);
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
        #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
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
        #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
        {
            send_line!("dump len: {}\r\n", packet.dump_len);
            #[cfg(feature = "esp32c6")]
            send_line!("he sigb len: {}\r\n", packet.he_sigb_len);
            #[cfg(feature = "esp32c6")]
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
            #[cfg(feature = "esp32c6")]
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
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
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
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    {
        log_ln!("dump len: {}", packet.dump_len);
        #[cfg(feature = "esp32c6")]
        log_ln!("he sigb len: {}", packet.he_sigb_len);
        #[cfg(feature = "esp32c6")]
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
        #[cfg(feature = "esp32c6")]
        log_ln!("rxmatch0: {}", packet.rxmatch0);
    }

    log_ln!("sig_len: {}", packet.sig_len);
    log_ln!("data length: {}", packet.csi_data_len);

    #[cfg(not(feature = "defmt"))]
    log_ln!("csi raw data: [{:X?}]", packet.csi_data);
    #[cfg(feature = "defmt")]
    log_ln!("csi raw data: [{=[?]}]", packet.csi_data.as_slice());
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
                let _ = match LOG_MODE.load(Ordering::Relaxed).into() {
                    LogMode::Serialized => write_serialized_packet(packet, &mut driver).await,
                    LogMode::ArrayList => write_text_array_packet(packet, &mut driver).await,
                    LogMode::Text => write_text_packet(packet, &mut driver).await,
                    LogMode::EspCsiTool => write_csi_tool_packet(packet, &mut driver).await,
                };

                // Drain pending text messages after each CSI write to prevent
                // starvation: at high CSI rates CSI_CHANNEL is always ready so
                // `select` never reaches Either::Second on its own.
                #[cfg(all(feature = "println", not(feature = "defmt")))]
                while let Ok(message) = log_impl::LOG_CHANNEL.try_receive() {
                    let _ = driver.write_all(message.as_bytes()).await;
                }
                #[cfg(feature = "defmt")]
                while let Ok(message) = defmt_impl::DEFMT_CHANNEL.try_receive() {
                    let _ = driver.write_all(&message).await;
                }

                // No per-packet flush: at sustained throughput each packet's
                // bytes are still draining from the TX FIFO when we start the
                // next packet, and the next `driver.write` will naturally
                // block on FIFO capacity. An explicit flush() here only adds
                // dead time on every iteration where the channel briefly
                // empties between packets.
            }
            Either::Second(message) => {
                // `message` is heapless::String<256> in println mode,
                // or [u8; 256] in defmt mode.
                #[cfg(all(feature = "println", not(feature = "defmt")))]
                let _ = driver.write_all(message.as_bytes()).await;
                #[cfg(feature = "defmt")]
                let _ = driver.write_all(&message).await;

                // Only flush if no more messages are pending.
                #[cfg(all(feature = "println", not(feature = "defmt")))]
                if log_impl::LOG_CHANNEL.is_empty() {
                    let _ = driver.flush().await;
                }
                #[cfg(feature = "defmt")]
                if defmt_impl::DEFMT_CHANNEL.is_empty() {
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
