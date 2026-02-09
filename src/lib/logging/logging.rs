use embedded_io_async::Write;
use esp_hal::peripherals::Peripherals;
use heapless::String;
use portable_atomic::AtomicU8;
use postcard::experimental::max_size::MaxSize;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogMode {
    Text,
    Serialized,
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

static LOG_MODE: AtomicU8 = AtomicU8::new(LogMode::Text as u8);

#[macro_export]
macro_rules! log_ln {
    ($($arg:tt)*) => {{
        #[cfg(
            any(feature = "uart", feature = "jtag-serial", feature = "auto")
        )]
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

#[macro_export]
macro_rules! log_str {
    ($($arg:tt)*) => {{
        #[cfg(
            any(feature = "uart", feature = "jtag-serial", feature = "auto")
        )]
        {
            #[cfg(feature = "println")]
            {
                esp_println::print!($($arg)*);
            }

            #[cfg(feature = "defmt")]
            {
                defmt::print!($($arg)*);
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

pub fn print_raw_bytes(bytes: &[u8]) {
    use core::fmt::Write;
    let mut printer = esp_println::Printer;
    for chunk in bytes.chunks(64) {
         for &b in chunk {
            let _ = printer.write_char(b as char);
         }
    }
}

#[macro_export]
macro_rules! log_raw {
    ($data:expr) => {{
        #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
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

pub fn log_csi(packet: CSIDataPacket) {
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

pub fn set_logging_mode(log_mode: LogMode) {
    #[cfg(any(feature = "uart", feature = "jtag-serial", feature = "auto"))]
    {
        use core::sync::atomic::Ordering;

        LOG_MODE.store(log_mode as u8, Ordering::Relaxed);
    }
}

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
