use heapless::Vec;
use postcard::experimental::max_size::MaxSize;
use serde::{Deserialize, Serialize};

use crate::log_ln;
use crate::time::DateTime;

/// A mapping of the different possible recieved CSI data formats supported by the Espressif WiFi driver.
/// `RxCSIFmt`` encodes the different formats (each column in the table) in one byte to save space when transmitting back CSI data.
/// The driver can be found here:
/// <https://docs.espressif.com/projects/esp-idf/en/latest/esp32s3/api-guides/wifi.html#wi-fi-channel-state-information>
#[derive(Debug, Clone, Serialize, Deserialize, MaxSize)]
#[repr(u8)]
pub enum RxCSIFmt {
    /// Sec Chnl = None, Sig Mode = non-Ht, Chnl BW = 20MHz, non-STBC
    Bw20,
    /// Sec Chnl = None, Sig Mode = Ht, Chnl BW = 20MHz, non-STBC         
    HtBw20,
    /// Sec Chnl = None, Sig Mode = Ht, Chnl BW = 20MHz, STBC
    HtBw20Stbc,
    /// Sec Chnl = Below, Sig Mode = non-Ht, Chnl BW = 20MHz, non-STBC
    SecbBw20,
    /// Sec Chnl = Below, Sig Mode = Ht, Chnl BW = 20MHz, non-STBC
    SecbHtBw20,
    /// Sec Chnl = Below, Sig Mode = Ht, Chnl BW = 20MHz, STBC
    SecbHtBw20Stbc,
    /// Sec Chnl = Below, Sig Mode = Ht, Chnl BW = 40MHz, non-STBC
    SecbHtBw40,
    /// Sec Chnl = Below, Sig Mode = Ht, Chnl BW = 40MHz, STBC
    SecbHtBw40Stbc,
    /// Sec Chnl = Above, Sig Mode = non-Ht, Chnl BW = 20MHz, non-STBC
    SecaBw20,
    /// Sec Chnl = Above, Sig Mode = Ht, Chnl BW = 20MHz, non-STBC
    SecaHtBw20,
    /// Sec Chnl = Above, Sig Mode = Ht, Chnl BW = 20MHz, STBC
    SecaHtBw20Stbc,
    /// Sec Chnl = Above, Sig Mode = Ht, Chnl BW = 40MHz, non-STBC
    SecaHtBw40,
    /// Sec Chnl = Above, Sig Mode = Ht, Chnl BW = 40MHz, STBC
    SecaHtBw40Stbc,
    /// Not a defined format
    Undefined,
}

// CSI Received Packet Radio Metadata Header Value Interpretations for non-ESP32-C6 devices

// rssi: Received Signal Strength Indicator(RSSI) of packet. unit: dBm.
// rate: PHY rate encoding of the packet. Only valid for non HT(11bg) packet.
// sig_mode: Protocol of the received packet, 0: non HT(11bg) packet; 1: HT(11n) packet; 3: VHT(11ac) packet.
// mcs: Modulation Coding Scheme. If is HT(11n) packet, shows the modulation, range from 0 to 76(MSC0 ~ MCS76).
// cwb: Channel Bandwidth of the packet. 0: 20MHz; 1: 40MHz.
// smoothing: Set to 1 indicates that channel estimate smoothing is recommended. Set to 0 indicates that only per-carrier independent (unsmoothed) channel estimate is recommended.
// not_sounding: Set to 0 indicates that PPDU is a sounding PPDU. Set to 1 indicates that the PPDU is not a sounding PPDU. Sounding PPDU is used for channel estimation by the request receiver.
// aggregation: Aggregation. 0: MPDU packet; 1: AMPDU packet
// stbc: Space Time Block Code(STBC). 0: non STBC packet; 1: STBC packet.
// fec_coding: Forward Error Correction (FEC). Flag is set for 11n packets which are LDPC.
// sgi: Short Guide Interval (SGI). 0: Long GI; 1: Short GI.
// noise_floor: noise floor of Radio Frequency Module(RF). unit: dBm.
// ampdu_cnt: The number of subframes aggregated in AMPDU.
// channel: Primary channel on which this packet is received.
// secondary_channel: Secondary channel on which this packet is received. 0: none; 1: above; 2: below.
// timestamp: Timestamp. The local time when this packet is received. It is precise only if modem sleep or light sleep is not enabled. The timer is started when controller.start() is returned. unit: microsecond.
// noise_floor: Noise floor of Radio Frequency Module(RF). unit: dBm.
// ant: Antenna number from which this packet is received. 0: WiFi antenna 0; 1: WiFi antenna 1.
// noise_floor: Noise floor of Radio Frequency Module(RF). unit: dBm.
// sig_len: Length of packet including Frame Check Sequence(FCS).
// rx_state: State of the packet. 0: no error; others: error numbers which are not public.

// CSI Received Packet Radio Metadata Header Value Interpretations for ESP32-C6 devices

// rssi: Received Signal Strength Indicator (RSSI) of the packet, in dBm.
// rate: PHY rate encoding of the packet. Only valid for non-HT (802.11b/g) packets.
// sig_len: Length of the received packet including the Frame Check Sequence (FCS).
// rx_state: Reception state of the packet: 0 for no error, others indicate error codes.
// dump_len: Length of the dump buffer.
// he_sigb_len: Length of HE-SIG-B field (802.11ax).
// cur_single_mpdu: Indicates if this is a single MPDU.
// cur_bb_format: Current baseband format.
// rx_channel_estimate_info_vld: Channel estimation validity.
// rx_channel_estimate_len: Length of the channel estimation.
// second: Timing information in seconds.
// channel: Primary channel on which the packet is received.
// noise_floor: Noise floor of the Radio Frequency module, in dBm.
// is_group: Indicates if this is a group-addressed frame.
// rxend_state: End state of the packet reception.
// rxmatch3: Indicate whether the reception frame is from interface 3.
// rxmatch2: Indicate whether the reception frame is from interface 2.
// rxmatch1: Indicate whether the reception frame is from interface 1.
// rxmatch0: Indicate whether the reception frame is from interface 0.

/// CSI Received Packet w/ Radio Metadata
#[cfg(not(feature = "esp32c6"))]
#[derive(Debug, Clone, Serialize, Deserialize, MaxSize)]
pub struct CSIDataPacket {
    /// MAC address of the sender.
    pub mac: [u8; 6],
    /// Received Signal Strength Indicator.
    pub rssi: i32,
    /// Local Timestamp of Recieved Packet (microseconds)  .                 
    pub timestamp: u32,
    /// PHY rate encoding of the packet. Only valid for non HT(11bg) packet.              
    pub rate: u32,
    /// Short Guide Interval (SGI). 0: Long GI; 1: Short GI.
    pub sgi: u32,
    /// Secondary Channel on which the Packet was Received.
    /// 0: none; 1: above; 2: below.
    pub secondary_channel: u32,
    /// Primary channel on which the Packet was Received.
    pub channel: u32,
    /// Channel Bandwidth of the packet.
    /// 0: 20MHz; 1: 40MHz.
    pub bandwidth: u32,
    /// Antenna number from which this packet is received.
    /// 0: WiFi antenna 0; 1: WiFi antenna 1.
    pub antenna: u32,
    /// Protocol of the received packet.
    /// 0: non HT(11bg) packet; 1: HT(11n) packet; 3: VHT(11ac) packet.
    pub sig_mode: u32,
    /// Modulation Coding Scheme.
    /// If Packet is HT(11n) packet, shows the modulation, range from 0 to 76(MSC0 ~ MCS76).
    pub mcs: u32,
    /// Set to 1 indicates that channel estimate smoothing is recommended.
    /// Set to 0 indicates that only per-carrier independent (unsmoothed) channel estimate is recommended.
    pub smoothing: u32,
    /// Sounding PPDU is used for channel estimation by the request receiver.
    /// Set to 0 indicates that PPDU is a sounding PPDU.
    /// Set to 1 indicates that the PPDU is not a sounding PPDU.
    pub not_sounding: u32,
    /// Aggregation.
    /// 0: MPDU packet; 1: AMPDU packet
    pub aggregation: u32,
    /// Space-Time Block Coding.
    /// 0: non STBC packet; 1: STBC packet.
    pub stbc: u32,
    /// Forward Error Correction (FEC).
    /// Flag is set for 11n packets which are LDPC.
    pub fec_coding: u32,
    /// The number of subframes aggregated in AMPDU.
    pub ampdu_cnt: u32,
    /// Noise floor of Radio Frequency Module(RF).
    /// unit: dBm.
    pub noise_floor: i32,
    /// RX state.
    /// 0: no error; others: error numbers which are not public.
    pub rx_state: u32,
    /// Length of packet including Frame Check Sequence(FCS).
    pub sig_len: u32,
    /// Optional NTP-based Timestamp Indicating the Time CSI Captured.
    pub date_time: Option<DateTime>,
    /// Sequence Number Associated with the Packet that triggered a CSI capture.
    pub sequence_number: u16,
    /// NEW FIELD: Number of packets dropped since the last successful packet
    pub packet_drop_count: u32,
    /// Data format of the recieved CSI.
    /// RxCSIFmt is a Compact Representation of the Different Recieved CSI Data Format Options as defined in the ESP WiFi Driver.
    pub data_format: RxCSIFmt,
    /// Length of CSI data.
    pub csi_data_len: u16,
    /// Raw CSI data, largest case size is 612 bytes.
    pub csi_data: Vec<i8, 612>,
}

#[cfg(not(feature = "esp32c6"))]
impl CSIDataPacket {
    /// Prints Recieved CSI Data Packet with it's Metadata
    pub fn print_csi_w_metadata(&self) {
        use crate::logging::logging::log_csi;

        log_csi(self.clone());
    }

    // Retrieves `RxCSIFmt` for a `CSIDataPacket`
    // The RxCSIFmt enum is a mapping of the different possible recieved CSI data formats supported by the Espressif WiFi driver.
    // RxCSIFmt encodes the different formats (each column in the table) in one byte to save space
    // More details on the different data formats can be found in the ESP CSI WiFi driver here:
    // https://docs.espressif.com/projects/esp-idf/en/latest/esp32s3/api-guides/wifi.html#wi-fi-channel-state-information
    //
    // The encoding is as follows:
    // Bw20 => 0.              Secondary Channel = None, Signal Mode = non-HT, Channel BW = 20MHz, non-STBC
    // HtBw20 => 1             Secondary Channel = None, Signal Mode = HT, Channel BW = 20MHz, non-STBC
    // HtBw20Stbc => 2         Secondary Channel = None, Signal Mode = HT, Channel BW = 20MHz, STBC
    // SecbBw20 => 3           Secondary Channel = Below, Signal Mode = non-HT, Channel BW = 20MHz, non-STBC
    // SecbHtBw20 => 4         Secondary Channel = Below, Signal Mode = HT, Channel BW = 20MHz, non-STBC
    // SecbHtBw20Stbc => 5     Secondary Channel = Below, Signal Mode = HT, Channel BW = 20MHz, STBC
    // SecbHtBw40 => 6         Secondary Channel = Below, Signal Mode = HT, Channel BW = 40MHz, non-STBC
    // SecbHtBw40Stbc  => 7    Secondary Channel = Below, Signal Mode = HT, Channel BW = 40MHz, STBC
    // SecaBw20 => 8           Secondary Channel = Above, Signal Mode = non-HT, Channel BW = 20MHz, non-STBC
    // SecaHtBw20 => 9         Secondary Channel = Above, Signal Mode = HT, Channel BW = 20MHz, non-STBC
    // SecaHtBw20Stbc => 10    Secondary Channel = Above, Signal Mode = HT, Channel BW = 20MHz, STBC
    // SecaHtBw40 => 11        Secondary Channel = Above, Signal Mode = HT, Channel BW = 40MHz, non-STBC
    // SecaHtBw40Stbc => 12    Secondary Channel = Above, Signal Mode = HT, Channel BW = 40MHz, STBC
    // Undefined => 13
    pub fn csi_fmt_from_params(&mut self) {
        match self.secondary_channel {
            // None
            0 => {
                match self.sig_mode {
                    // non-HTc
                    0 => self.data_format = RxCSIFmt::Bw20,
                    // HT
                    1 => {
                        match self.stbc {
                            // non-STBC
                            0 => self.data_format = RxCSIFmt::HtBw20,
                            // STBC
                            1 => self.data_format = RxCSIFmt::HtBw20Stbc,
                            _ => self.data_format = RxCSIFmt::Undefined,
                        }
                    }
                    _ => self.data_format = RxCSIFmt::Undefined,
                }
            }
            // Above
            1 => {
                match self.sig_mode {
                    // non-HT
                    0 => self.data_format = RxCSIFmt::SecaBw20,
                    // HT
                    1 => {
                        match self.bandwidth {
                            // 20MHz
                            0 => {
                                match self.stbc {
                                    // non-STBC
                                    0 => self.data_format = RxCSIFmt::SecaHtBw20,
                                    // STBC
                                    1 => self.data_format = RxCSIFmt::SecaHtBw20Stbc,
                                    _ => self.data_format = RxCSIFmt::Undefined,
                                }
                            }
                            // 40MHz
                            1 => {
                                match self.stbc {
                                    // non-STBC
                                    0 => self.data_format = RxCSIFmt::SecaHtBw40,
                                    // STBC
                                    1 => self.data_format = RxCSIFmt::SecaHtBw40Stbc,
                                    _ => self.data_format = RxCSIFmt::Undefined,
                                }
                            }
                            _ => self.data_format = RxCSIFmt::Undefined,
                        }
                    }
                    _ => self.data_format = RxCSIFmt::Undefined,
                }
            }
            // Below
            2 => {
                match self.sig_mode {
                    // non-HT
                    0 => self.data_format = RxCSIFmt::SecbBw20,
                    // HT
                    1 => {
                        match self.bandwidth {
                            // 20MHz
                            0 => {
                                match self.stbc {
                                    // non-STBC
                                    0 => self.data_format = RxCSIFmt::SecbHtBw20,
                                    // STBC
                                    1 => self.data_format = RxCSIFmt::SecbHtBw20Stbc,
                                    _ => self.data_format = RxCSIFmt::Undefined,
                                }
                            }
                            // 40MHz
                            1 => {
                                match self.stbc {
                                    // non-STBC
                                    0 => self.data_format = RxCSIFmt::SecbHtBw40,
                                    // STBC
                                    1 => self.data_format = RxCSIFmt::SecbHtBw40Stbc,
                                    _ => self.data_format = RxCSIFmt::Undefined,
                                }
                            }
                            _ => self.data_format = RxCSIFmt::Undefined,
                        }
                    }
                    _ => self.data_format = RxCSIFmt::Undefined,
                }
            }
            _ => self.data_format = RxCSIFmt::Undefined,
        }
    }

    pub fn mac(&self) -> &[u8; 6] {
        &self.mac
    }
    pub fn rssi(&self) -> i32 {
        self.rssi
    }
    pub fn timestamp(&self) -> u32 {
        self.timestamp
    }
    pub fn rate(&self) -> u32 {
        self.rate
    }
    pub fn sgi(&self) -> u32 {
        self.sgi
    }
    pub fn secondary_channel(&self) -> u32 {
        self.secondary_channel
    }
    pub fn channel(&self) -> u32 {
        self.channel
    }
    pub fn bandwidth(&self) -> u32 {
        self.bandwidth
    }
    pub fn antenna(&self) -> u32 {
        self.antenna
    }
    pub fn sig_mode(&self) -> u32 {
        self.sig_mode
    }
    pub fn mcs(&self) -> u32 {
        self.mcs
    }
    pub fn smoothing(&self) -> u32 {
        self.smoothing
    }
    pub fn not_sounding(&self) -> u32 {
        self.not_sounding
    }
    pub fn aggregation(&self) -> u32 {
        self.aggregation
    }
    pub fn stbc(&self) -> u32 {
        self.stbc
    }
    pub fn fec_coding(&self) -> u32 {
        self.fec_coding
    }
    pub fn ampdu_cnt(&self) -> u32 {
        self.ampdu_cnt
    }
    pub fn noise_floor(&self) -> i32 {
        self.noise_floor
    }
    pub fn rx_state(&self) -> u32 {
        self.rx_state
    }
    pub fn sig_len(&self) -> u32 {
        self.sig_len
    }
    pub fn date_time(&self) -> Option<&DateTime> {
        self.date_time.as_ref()
    }
    pub fn sequence_number(&self) -> u16 {
        self.sequence_number
    }
    pub fn data_format(&self) -> RxCSIFmt {
        self.data_format.clone()
    }
    pub fn csi_data_len(&self) -> u16 {
        self.csi_data_len
    }
    pub fn csi_data(&self) -> &[i8] {
        self.csi_data.as_slice()
    }
}

#[cfg(feature = "esp32c6")]
#[derive(Debug, Clone, Serialize, Deserialize, MaxSize)]
pub struct CSIDataPacket {
    /// MAC address of the sender.
    pub mac: [u8; 6],
    /// Received Signal Strength Indicator.
    pub rssi: i32,
    /// Local Timestamp of Recieved Packet (microseconds).
    pub timestamp: u32,
    /// PHY rate encoding of the packet.
    pub rate: u32,
    /// Noise floor of Radio Frequency Module(RF).
    /// unit: dBm.
    pub noise_floor: i32,
    /// Length of packet including Frame Check Sequence(FCS).
    pub sig_len: u32,
    /// Reception state of the packet.
    /// 0 for no error, others indicate error codes.
    pub rx_state: u32,
    /// Length of dump buffer.
    pub dump_len: u32,
    /// Length of HE-SIG-B field (802.11ax).
    pub he_sigb_len: u32,
    /// Indicates if this is a single MPDU.
    pub cur_single_mpdu: u32,
    /// Current baseband format.
    pub cur_bb_format: u32,
    /// Channel estimation validity.
    pub rx_channel_estimate_info_vld: u32,
    /// Length of the channel estimation.
    pub rx_channel_estimate_len: u32,
    /// Timing information in seconds.
    pub second: u32,
    /// Primary channel on which the packet is received.
    pub channel: u32,
    /// Indicates if this is a group-addressed frame.
    pub is_group: u32,
    /// End state of the packet reception.
    pub rxend_state: u32,
    /// Indicate whether the reception frame is from interface 3.
    pub rxmatch3: u32,
    /// Indicate whether the reception frame is from interface 2.
    pub rxmatch2: u32,
    /// Indicate whether the reception frame is from interface 1.
    pub rxmatch1: u32,
    /// Indicate whether the reception frame is from interface 0.
    pub rxmatch0: u32,
    /// Optional NTP-based Timestamp Indicating the Time CSI Captured.
    pub date_time: Option<DateTime>,
    /// Sequence number associated with packet.
    pub sequence_number: u16,
    /// Length of CSI data.
    pub csi_data_len: u16,
    /// Data format of the recieved CSI.
    /// RxCSIFmt is a Compact Representation of the Different Recieved CSI Data Format Options as defined in the ESP WiFi Driver.
    pub data_format: RxCSIFmt,
    /// Raw CSI data, largest case size is 612 bytes.
    pub csi_data: Vec<i8, 612>,
}

#[cfg(feature = "esp32c6")]
impl CSIDataPacket {
    pub fn print_csi_w_metadata(&self) {
        // Calculate Elapsed time here and add offset to date_time then call to calculate new time

        use crate::logging::logging::log_csi;
        if let Some(date_time) = &self.date_time {
            log_ln!(
                "Recieved at {:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
                date_time.year,
                date_time.month,
                date_time.day,
                date_time.hour,
                date_time.minute,
                date_time.second,
                date_time.millisecond
            );
        }
        log_csi(packet);
    }
    pub fn csi_fmt_from_params(&mut self) {
        self.data_format = RxCSIFmt::Undefined;
    }

    pub fn mac(&self) -> &[u8; 6] {
        &self.mac
    }

    pub fn rssi(&self) -> i32 {
        self.rssi
    }
    pub fn timestamp(&self) -> u32 {
        self.timestamp
    }
    pub fn rate(&self) -> u32 {
        self.rate
    }
    pub fn noise_floor(&self) -> i32 {
        self.noise_floor
    }
    pub fn sig_len(&self) -> u32 {
        self.sig_len
    }
    pub fn rx_state(&self) -> u32 {
        self.rx_state
    }
    pub fn dump_len(&self) -> u32 {
        self.dump_len
    }
    pub fn he_sigb_len(&self) -> u32 {
        self.he_sigb_len
    }
    pub fn cur_single_mpdu(&self) -> u32 {
        self.cur_single_mpdu
    }
    pub fn cur_bb_format(&self) -> u32 {
        self.cur_bb_format
    }
    pub fn rx_channel_estimate_info_vld(&self) -> u32 {
        self.rx_channel_estimate_info_vld
    }
    pub fn rx_channel_estimate_len(&self) -> u32 {
        self.rx_channel_estimate_len
    }
    pub fn second(&self) -> u32 {
        self.second
    }
    pub fn channel(&self) -> u32 {
        self.channel
    }
    pub fn is_group(&self) -> u32 {
        self.is_group
    }
    pub fn rxend_state(&self) -> u32 {
        self.rxend_state
    }
    pub fn rxmatch3(&self) -> u32 {
        self.rxmatch3
    }
    pub fn rxmatch2(&self) -> u32 {
        self.rxmatch2
    }
    pub fn rxmatch1(&self) -> u32 {
        self.rxmatch1
    }
    pub fn rxmatch0(&self) -> u32 {
        self.rxmatch0
    }
    pub fn csi_data(&self) -> &[i8] {
        self.csi_data.as_slice()
    }
    pub fn csi_data_len(&self) -> u16 {
        self.csi_data_len
    }
}
