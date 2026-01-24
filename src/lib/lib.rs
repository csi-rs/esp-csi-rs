#![no_std]

use core::future::ready;

use embassy_executor::Spawner;

use embassy_futures::join::join;
use embassy_futures::select::{select, Either};
use embassy_sync::pubsub::{PubSubBehavior, Subscriber};

use embassy_time::Instant;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::{ClientConfig, CsiConfig, Interfaces, WifiController};

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, watch::Watch};

use embassy_sync::pubsub::PubSubChannel;
use embassy_sync::signal::Signal;
// use embassy_sync::watch::Receiver;

use heapless::Vec;
extern crate alloc;
use alloc::collections::BTreeMap;

pub mod central;
pub mod config;
pub mod csi;
pub mod logging;
pub mod peripheral;
pub mod time;

use crate::central::esp_now::{run_esp_now_central};
use crate::central::sta::{run_sta_connect, sta_init};
use crate::config::CsiConfig as CsiConfiguration;
use crate::csi::{CSIDataPacket, RxCSIFmt};
use crate::peripheral::esp_now::{run_esp_now_peripheral};

// Channels
static CONTROLLER_CH: Channel<CriticalSectionRawMutex, WifiController<'static>, 1> = Channel::new();
static INTERFACES_CH: Channel<CriticalSectionRawMutex, Interfaces<'static>, 1> = Channel::new();
// static CSI_RAW_CH: Channel<CriticalSectionRawMutex, Vec<u8, 625>, 2> = Channel::new();

// Watches
// static PROCESSED_CSI_DATA: Watch<CriticalSectionRawMutex, CSIDataPacket, 3> = Watch::new();

// PubSub Channels
static CSI_PACKET: PubSubChannel<
    CriticalSectionRawMutex,
    CSIDataPacket,
    PROC_CSI_CH_CAPACITY,
    2,
    1,
> = PubSubChannel::new();
static PROCESSED_CSI_DATA: PubSubChannel<
    CriticalSectionRawMutex,
    CSIDataPacket,
    PROC_CSI_CH_CAPACITY,
    PROC_CSI_CH_SUBS,
    2,
> = PubSubChannel::new();

// Signals
static STOP_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

const PROC_CSI_CH_CAPACITY: usize = 20;
const PROC_CSI_CH_SUBS: usize = 2;

// macro_rules! mk_static {
//     ($t:ty,$val:expr) => {{
//         static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
//         #[deny(unused_attributes)]
//         let x = STATIC_CELL.uninit().write(($val));
//         x
//     }};
// }

pub struct EspNowConfig {
    phy_rate: WifiPhyRate,
    channel: u8,
}

impl Default for EspNowConfig {
    fn default() -> Self {
        Self {
            phy_rate: WifiPhyRate::RateMcs0Lgi,
            channel: 11,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WifiSnifferConfig {
    mac_filter: Option<[u8; 6]>,
}

impl Default for WifiSnifferConfig {
    fn default() -> Self {
        Self { mac_filter: None }
    }
}

#[derive(Debug, Clone)]
pub struct WifiStationConfig {
    pub ntp_sync: bool,
    pub client_config: ClientConfig,
}

// Enum for Central modes, each wrapping its specific config.

pub enum CentralOpMode {
    EspNow(EspNowConfig),
    WifiStation(WifiStationConfig),
}

// Enum for Peripheral modes, each wrapping its specific config.
pub enum PeripheralOpMode {
    EspNow(EspNowConfig),
    WifiSniffer(WifiSnifferConfig),
}

pub enum Node {
    Peripheral(PeripheralOpMode), // Mode is implicit (only EspNow), directly holds config.
    Central(CentralOpMode),       // Uses the sub-enum for mode selection.
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum CollectionMode {
    Collector, // Enables CSI collection + Collect CSI Data
    Listener,  // Enables CSI collection + Does not collect CSI Data
}

pub struct CSINodeHardware<'a> {
    interfaces: &'a mut Interfaces<'static>,
    controller: &'a mut WifiController<'static>,
}

impl<'a> CSINodeHardware<'a> {
    pub fn new(
        interfaces: &'a mut Interfaces<'static>,
        controller: &'a mut WifiController<'static>,
    ) -> Self {
        Self {
            interfaces,
            controller,
        }
    }
}

type CSIRxSubscriber = Subscriber<
    'static,
    CriticalSectionRawMutex,
    CSIDataPacket,
    PROC_CSI_CH_CAPACITY,
    PROC_CSI_CH_SUBS,
    2
>;

pub struct CSIClient {
    csi_subscriber: CSIRxSubscriber,
}

impl CSIClient {
    pub fn new() -> Self {
        Self {
            csi_subscriber: PROCESSED_CSI_DATA.subscriber().unwrap(),
        }
    }

    pub async fn get_csi_data(&mut self) -> CSIDataPacket {
        self.csi_subscriber.next_message_pure().await
    }
    
    pub async fn print_csi_w_metadata(&mut self) {
        let packet = self.get_csi_data().await;
        packet.print_csi_w_metadata();
    }

    pub async fn send_stop(&self) {
        STOP_SIGNAL.signal(());
    }
}

pub struct CSINode<'a> {
    kind: Node,
    collection_mode: CollectionMode,
    /// CSI Configuration
    csi_config: Option<CsiConfiguration>,
    /// Traffic Generation Frequency
    traffic_freq_hz: Option<u16>,
    /// Receiver for Processed CSI Data Packets
    hardware: CSINodeHardware<'a>,
}

impl<'a> CSINode<'a> {
    pub fn new(
        kind: Node,
        collection_mode: CollectionMode,
        csi_config: Option<CsiConfiguration>,
        traffic_freq_hz: Option<u16>,
        hardware: CSINodeHardware<'a>,
    ) -> Self {
        let csi_data_rx = PROCESSED_CSI_DATA.subscriber().unwrap();
        Self {
            kind,
            collection_mode,
            csi_config,
            traffic_freq_hz,
            hardware,
        }
    }

    pub fn new_central_node(
        op_mode: CentralOpMode,
        collection_mode: CollectionMode,
        csi_config: Option<CsiConfiguration>,
        traffic_freq_hz: Option<u16>,
        hardware: CSINodeHardware<'a>,
    ) -> Self {
        Self {
            kind: Node::Central(op_mode),
            collection_mode,
            csi_config,
            traffic_freq_hz,
            hardware,
        }
    }

    pub fn get_node_type(&self) -> &Node {
        &self.kind
    }

    pub fn get_collection_mode(&self) -> CollectionMode {
        self.collection_mode
    }

    pub fn get_central_op_mode(&self) -> Option<&CentralOpMode> {
        match &self.kind {
            Node::Central(mode) => Some(mode),
            Node::Peripheral(_) => None,
        }
    }

    pub fn get_peripheral_op_mode(&self) -> Option<&PeripheralOpMode> {
        match &self.kind {
            Node::Peripheral(mode) => Some(mode),
            Node::Central(_) => None,
        }
    }

    pub fn set_csi_config(&mut self, config: CsiConfiguration) {
        self.csi_config = Some(config);
    }

    pub fn set_station_config(&mut self, config: WifiStationConfig) {
        if let Node::Central(CentralOpMode::WifiStation(_)) = &mut self.kind {
            self.kind = Node::Central(CentralOpMode::WifiStation(config));
        }
    }

    pub fn set_traffic_frequency(&mut self, freq_hz: u16) {
        self.traffic_freq_hz = Some(freq_hz);
    }

    pub fn set_collection_mode(&mut self, mode: CollectionMode) {
        self.collection_mode = mode;
    }

    pub fn set_op_mode(&mut self, mode: Node) {
        self.kind = mode;
    }

    pub async fn run(&mut self) {
        let interfaces = &mut self.hardware.interfaces;
        let controller = &mut self.hardware.controller;

        // Tasks Necessary for Central Station & Sniffer
        let sta_interface = if let Node::Central(CentralOpMode::WifiStation(config)) = &self.kind {
            Some(sta_init(&mut interfaces.sta, config, controller))
        } else {
            None
        };

        // Set Wi-Fi mode to Station for all node types
        controller.set_mode(esp_radio::wifi::WifiMode::Sta).unwrap();

        // Build CSI Configuration
        let config = match self.csi_config {
            Some(ref config) => {
                log_ln!("CSI Configuration Set: {:?}", config);
                build_csi_config(config)
            }
            None => {
                let default_config = CsiConfiguration::default();
                log_ln!(
                    "No CSI Configuration Provided. Going with defaults: {:?}",
                    default_config
                );
                build_csi_config(&default_config)
            }
        };

        // Start the controller
        controller.start_async().await.unwrap();
        log_ln!("Wi-Fi Controller Started");

        let is_collector = self.collection_mode == CollectionMode::Collector;

        // Initialize Nodes
        match &self.kind {
            Node::Peripheral(op_mode) => match op_mode {
                PeripheralOpMode::EspNow(esp_now_config) => {
                    // Set Peripheral to Collect CSI
                    set_csi(controller, config);
                    // Initialize as Peripheral node with EspNowConfig
                    let main_task = run_esp_now_peripheral(&mut interfaces.esp_now, esp_now_config);
                    if (is_collector) {
                        join(main_task, process_csi_packet()).await;
                    } else {
                        main_task.await;
                    }
                    STOP_SIGNAL.reset();
                }
                PeripheralOpMode::WifiSniffer(sniffer_config) => {
                    let sniffer = &interfaces.sniffer;
                    sniffer.set_promiscuous_mode(true).unwrap();
                    // Set Sniffer to Collect CSI
                    set_csi(controller, config);
                    // Initialize as Wifi Sniffer Collector with WifiSnifferConfig

                    if (is_collector) {
                        process_csi_packet().await;
                    } else {
                        STOP_SIGNAL.wait().await;
                    }
                    STOP_SIGNAL.reset();
                    sniffer.set_promiscuous_mode(false).unwrap();
                }
            },
            Node::Central(op_mode) => match op_mode {
                CentralOpMode::EspNow(esp_now_config) => {
                    set_csi(controller, config);
                    let main_task = run_esp_now_central(
                        &mut interfaces.esp_now,
                        esp_now_config,
                        self.traffic_freq_hz,
                    );
                    if is_collector {
                        join(main_task, process_csi_packet()).await;
                    } else {
                        main_task.await;
                    }
                    STOP_SIGNAL.reset();
                }
                CentralOpMode::WifiStation(sta_config) => {
                    // Set Station to collect CSI
                    set_csi(controller, config);

                    // Initialize as Wifi Station Collector with WifiStationConfig
                    // 1. Connect to Wi-Fi network, etc.
                    // 2. Run DHCP, NTP sync if enabled in config, etc.
                    // 3. Spawn STA Connection Handling Task
                    // 4. Spawn STA Network Operation Task
                    let (sta_stack, sta_runner) = sta_interface.unwrap();

                    let main_task =
                        run_sta_connect(controller, self.traffic_freq_hz, sta_stack, sta_runner);
                    if is_collector {
                        join(main_task, process_csi_packet()).await;
                    } else {
                        main_task.await;
                    }
                    STOP_SIGNAL.reset();
                }
            },
        }
    }
}

#[cfg(feature = "esp32c6")]
fn build_csi_config(csi_config: &CsiConfiguration) -> CsiConfig {
    CsiConfig {
        enable: csi_config.enable,
        acquire_csi_legacy: csi_config.acquire_csi_legacy,
        acquire_csi_ht20: csi_config.acquire_csi_ht20,
        acquire_csi_ht40: csi_config.acquire_csi_ht40,
        acquire_csi_su: csi_config.acquire_csi_su,
        acquire_csi_mu: csi_config.acquire_csi_mu,
        acquire_csi_dcm: csi_config.acquire_csi_dcm,
        acquire_csi_beamformed: csi_config.acquire_csi_beamformed,
        acquire_csi_he_stbc: csi_config.acquire_csi_he_stbc,
        val_scale_cfg: csi_config.val_scale_cfg,
        dump_ack_en: csi_config.dump_ack_en,
        reserved: csi_config.reserved,
    }
}

#[cfg(not(feature = "esp32c6"))]
fn build_csi_config(csi_config: &CsiConfiguration) -> CsiConfig {
    CsiConfig {
        lltf_en: csi_config.lltf_en,
        htltf_en: csi_config.htltf_en,
        stbc_htltf2_en: csi_config.stbc_htltf2_en,
        ltf_merge_en: csi_config.ltf_merge_en,
        channel_filter_en: csi_config.channel_filter_en,
        manu_scale: csi_config.manu_scale,
        shift: csi_config.shift,
        dump_ack_en: csi_config.dump_ack_en,
    }
}

use portable_atomic::{AtomicU32, AtomicU64, Ordering};
// Global counter for all drops across all MAC addresses
static GLOBAL_DROP_COUNT: AtomicU32 = AtomicU32::new(0);
static GLOBAL_PACKET_COUNT: AtomicU64 = AtomicU64::new(0);
static GLOBAL_CAPTURE_START_TIME: AtomicU64 = AtomicU64::new(0);

pub fn get_total_packets() -> u64 {
    GLOBAL_PACKET_COUNT.load(Ordering::Relaxed)
}

pub fn get_avg_pps() -> u64 {
    let start_time = Instant::from_ticks(GLOBAL_CAPTURE_START_TIME.load(Ordering::Relaxed));
    let elapsed_secs = start_time.elapsed().as_secs() as u64;
    let total_packets = GLOBAL_PACKET_COUNT.load(Ordering::Relaxed);
    if elapsed_secs == 0 {
        return total_packets;
    }
    total_packets / elapsed_secs
}

/// Sets CSI Configuration.
/// - If `spawn_processor` is true (Collector Mode), it starts the processing task.
/// - If `spawn_processor` is false (Listener Mode), it enables hardware but ignores data processing.
fn set_csi(controller: &mut WifiController, config: CsiConfig) {
    // Set CSI Configuration with callback
    controller
        .set_csi(config, |info: esp_radio::wifi::wifi_csi_info_t| {
            capture_csi_info(info);
        })
        .unwrap();
}

// Function to capture CSI info from callback and publish to channel
fn capture_csi_info(info: esp_radio::wifi::wifi_csi_info_t) {
    GLOBAL_PACKET_COUNT.fetch_add(1, Ordering::Relaxed);
    if CSI_PACKET.is_full() {
        GLOBAL_DROP_COUNT.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let rssi = if info.rx_ctrl.rssi() > 127 {
        info.rx_ctrl.rssi() - 256
    } else {
        info.rx_ctrl.rssi()
    };

    let mut csi_data = Vec::<i8, 612>::new();
    // let csi_buf = info.buf;
    let csi_buf_len = info.len;
    let csi_slice =
        unsafe { core::slice::from_raw_parts(info.buf as *const i8, csi_buf_len as usize) };
    match csi_data.extend_from_slice(csi_slice) {
        Ok(_) => {}
        Err(_) => {
            GLOBAL_DROP_COUNT.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    // for data in 0..csi_buf_len {
    //     unsafe {
    //         let value = *csi_buf.add(data as usize);
    //         csi_data.push(value).expect("Exceeded maximum capacity");
    //     }
    // }

    #[cfg(not(feature = "esp32c6"))]
    let csi_packet = CSIDataPacket {
        sequence_number: info.rx_seq,
        data_format: RxCSIFmt::Undefined,
        date_time: None,
        mac: [
            info.mac[0],
            info.mac[1],
            info.mac[2],
            info.mac[3],
            info.mac[4],
            info.mac[5],
        ],
        rssi,
        bandwidth: info.rx_ctrl.cwb(),
        antenna: info.rx_ctrl.ant(),
        rate: info.rx_ctrl.rate(),
        sig_mode: info.rx_ctrl.sig_mode(),
        mcs: info.rx_ctrl.mcs(),
        smoothing: info.rx_ctrl.smoothing(),
        not_sounding: info.rx_ctrl.not_sounding(),
        aggregation: info.rx_ctrl.aggregation(),
        stbc: info.rx_ctrl.stbc(),
        fec_coding: info.rx_ctrl.fec_coding(),
        sgi: info.rx_ctrl.sgi(),
        noise_floor: info.rx_ctrl.noise_floor(),
        ampdu_cnt: info.rx_ctrl.ampdu_cnt(),
        channel: info.rx_ctrl.channel(),
        secondary_channel: info.rx_ctrl.secondary_channel(),
        timestamp: info.rx_ctrl.timestamp(),
        rx_state: info.rx_ctrl.rx_state(),
        sig_len: info.rx_ctrl.sig_len(),
        packet_drop_count: 0, // Initialize to 0. The listener task will calculate the real value later.
        csi_data_len: csi_buf_len,
        csi_data: csi_data,
    };

    #[cfg(feature = "esp32c6")]
    let csi_packet = CSIDataPacket {
        mac: [
            info.mac[0],
            info.mac[1],
            info.mac[2],
            info.mac[3],
            info.mac[4],
            info.mac[5],
        ],
        rssi,
        timestamp: info.rx_ctrl.timestamp(),
        rate: info.rx_ctrl.rate(),
        noise_floor: info.rx_ctrl.noise_floor(),
        sig_len: info.rx_ctrl.sig_len(),
        rx_state: info.rx_ctrl.rx_state(),
        dump_len: info.rx_ctrl.dump_len(),
        he_sigb_len: info.rx_ctrl.he_sigb_len(),
        cur_single_mpdu: info.rx_ctrl.cur_single_mpdu(),
        cur_bb_format: info.rx_ctrl.cur_bb_format(),
        rx_channel_estimate_info_vld: info.rx_ctrl.rx_channel_estimate_info_vld(),
        rx_channel_estimate_len: info.rx_ctrl.rx_channel_estimate_len(),
        second: info.rx_ctrl.second(),
        channel: info.rx_ctrl.channel(),
        is_group: info.rx_ctrl.is_group(),
        rxend_state: info.rx_ctrl.rxend_state(),
        rxmatch3: info.rx_ctrl.rxmatch3(),
        rxmatch2: info.rx_ctrl.rxmatch2(),
        rxmatch1: info.rx_ctrl.rxmatch1(),
        rxmatch0: info.rx_ctrl.rxmatch0(),
        date_time: None,
        sequence_number: info.rx_seq,
        data_format: RxCSIFmt::Undefined,
        csi_data_len: info.len as u16,
        csi_data: csi_data,
    };

    CSI_PACKET.publish_immediate(csi_packet);
}

pub async fn process_csi_packet() {
    // Initialize CSI process start time
    GLOBAL_CAPTURE_START_TIME.store(Instant::now().as_ticks(), Ordering::Relaxed);
    // Subscribe to CSI packet capture updates
    let mut csi_packet_sub = CSI_PACKET.subscriber().unwrap();
    let proc_csi_packet_sender = PROCESSED_CSI_DATA.publisher().unwrap();
    // Map to track sequence numbers per MAC address
    let mut peer_tracker: BTreeMap<[u8; 6], u16> = BTreeMap::new();
    // Loop that will process CSI data as soon as it arrives
    loop {
        match select(STOP_SIGNAL.wait(), csi_packet_sub.next_message_pure()).await {
            Either::First(_) => {
                // Stop signal received, exit the loop
                break;
            }
            Either::Second(mut csi_packet) => {
                // --- DROP DETECTION LOGIC START ---
                let current_seq = csi_packet.sequence_number;

                // Check if we have seen this MAC before
                if let Some(&last_seq) = peer_tracker.get(&csi_packet.mac) {
                    // Station Mode / Hardware Sequence Number Fix:
                    // WiFi hardware sequence numbers (802.11) are 12-bit (0-4095).
                    // We use '& 0x0FFF' to handle the wraparound from 4095 -> 0 correctly.
                    let diff = (current_seq.wrapping_sub(last_seq)) & 0x0FFF;

                    if diff > 1 {
                        let lost = (diff - 1) as u32;

                        // Sanity check for huge gaps (e.g. router reset)
                        if lost < 500 {
                            GLOBAL_DROP_COUNT.fetch_add(lost, Ordering::Relaxed);
                        }
                    }
                }

                // Update tracker with new sequence
                peer_tracker.insert(csi_packet.mac, current_seq);

                // Assign the calculated global drop count to the packet
                csi_packet.packet_drop_count = GLOBAL_DROP_COUNT.load(Ordering::Relaxed);
                // --- DROP DETECTION LOGIC END ---

                // Update the CSI data format
                #[cfg(not(feature = "esp32c6"))]
                {
                    csi_packet.csi_fmt_from_params();
                }

                // Process Date/Time if Date Time is valid/supported
                // if DATE_TIME_VALID.load(core::sync::atomic::Ordering::Relaxed) {
                //     let dt_cap = DATE_TIME.get().await;
                //     let elapsed_time = Instant::now()
                //         .checked_duration_since(dt_cap.captured_at)
                //         .unwrap_or(Duration::from_secs(0));
                //     // Add seconds and adjust for overflow from milliseconds
                //     let total_time_secs = dt_cap.captured_secs + elapsed_time.as_secs();

                //     // Add milliseconds and adjust if they exceed 1000
                //     let total_millis = dt_cap.captured_millis + elapsed_time.as_millis();
                //     let extra_secs = total_millis / 1000; // 1000ms = 1 second
                //     let final_millis = total_millis % 1000; // Remainder in milliseconds

                //     // Add extra seconds from milliseconds overflow to total seconds
                //     let total_time_secs = total_time_secs + extra_secs;

                //     // Now call the date-time conversion function
                //     let (year, month, day, hour, minute, second, millisecond) =
                //         unix_to_date_time(total_time_secs, final_millis);

                //     let dt = DateTime {
                //         year,
                //         month,
                //         day,
                //         hour,
                //         minute,
                //         second,
                //         millisecond,
                //     };

                //     csi_packet.date_time = Some(dt);
                // }
                // Update the Watch with the processed CSI
                proc_csi_packet_sender.publish_immediate(csi_packet);
            }
        }
    }
}

use crate::logging::logging::get_log_packet_drops;

pub fn get_dropped_packets() -> u32 {
    GLOBAL_DROP_COUNT.load(Ordering::Relaxed) + get_log_packet_drops()
}

/// Reconstructs a `CSIDataPacket` from a raw message buffer received in collector mode.
///
/// The expected format of `raw_csi_data` is:
/// - Bytes 0-1: u16 sequence_number (big-endian)
/// - Byte 2: u8 data_format (as repr of RxCSIFmt)
/// - Bytes 3-6: u32 timestamp (big-endian)
/// - Bytes 7..end: CSI data (u8 cast from original i8, up to 612 bytes)
///
/// Fields not transmitted (e.g., MAC, RSSI, rate, etc.) are set to default values:
/// - u32/i32 fields: 0
/// - mac: [0; 6]
/// - date_time: None
/// - sig_len: 0 (cannot be reliably reconstructed without additional data)
/// - rx_state: 0 (assumes no error)
///
/// Returns an error if the buffer length is invalid (<7 bytes or CSI data >612 bytes).
async fn reconstruct_raw_csi(raw_csi_data: &[u8]) -> Option<CSIDataPacket> {
    // Retrive the new CSI raw data from UDP channel
    // let raw_csi_data = CSI_RAW_CH.receive().await;

    if raw_csi_data.len() < 7 {
        return None;
    }

    let csi_data_start = 13;
    let csi_len = (raw_csi_data.len() - csi_data_start) as u16;
    if csi_len > 612 {
        return None;
    }

    // Extract sequence_number (u16 Big Endian)
    let sequence_number = u16::from_be_bytes([raw_csi_data[0], raw_csi_data[1]]);

    // Extract data_format (u8 -> RxCSIFmt)
    #[cfg(not(feature = "esp32c6"))]
    let fmt_u8 = raw_csi_data[2];
    #[cfg(not(feature = "esp32c6"))]
    let (data_format, bandwidth, sig_mode, stbc, secondary_channel) = match fmt_u8 {
        0 => (RxCSIFmt::Bw20, 0, 0, 0, 0),
        1 => (RxCSIFmt::HtBw20, 0, 1, 0, 0),
        2 => (RxCSIFmt::HtBw20Stbc, 0, 1, 1, 0),
        3 => (RxCSIFmt::SecbBw20, 0, 0, 0, 2),
        4 => (RxCSIFmt::SecbHtBw20, 0, 1, 0, 2),
        5 => (RxCSIFmt::SecbHtBw20Stbc, 0, 1, 1, 2),
        6 => (RxCSIFmt::SecbHtBw40, 1, 1, 0, 2),
        7 => (RxCSIFmt::SecbHtBw40Stbc, 1, 1, 1, 2),
        8 => (RxCSIFmt::SecaBw20, 0, 0, 0, 1),
        9 => (RxCSIFmt::SecaHtBw20, 0, 1, 0, 1),
        10 => (RxCSIFmt::SecaHtBw20Stbc, 0, 1, 1, 1),
        11 => (RxCSIFmt::SecaHtBw40, 1, 1, 0, 1),
        12 => (RxCSIFmt::SecaHtBw40Stbc, 1, 1, 1, 1),
        _ => (RxCSIFmt::Undefined, 0, 0, 0, 0),
    };

    // Extract timestamp (u32 BE)
    let timestamp = u32::from_be_bytes([
        raw_csi_data[3],
        raw_csi_data[4],
        raw_csi_data[5],
        raw_csi_data[6],
    ]);

    let mac_address = [
        raw_csi_data[7],
        raw_csi_data[8],
        raw_csi_data[9],
        raw_csi_data[10],
        raw_csi_data[11],
        raw_csi_data[12],
    ];

    // Reconstruct CSI data (u8 -> i8, preserving sign via bit reinterpretation)
    let mut csi_data = Vec::new();
    for &b in &raw_csi_data[csi_data_start..] {
        csi_data
            .push(b as i8)
            .map_err(|_| "Failed to push to Vec (capacity exceeded)")
            .unwrap();
    }

    // Build CSIDataPacket with defaults for missing fields
    #[cfg(not(feature = "esp32c6"))]
    let data_packet = CSIDataPacket {
        mac: mac_address,
        rssi: 0,
        timestamp,
        rate: 0,
        sgi: 0,
        secondary_channel: secondary_channel,
        channel: 0,
        bandwidth: bandwidth,
        antenna: 0,
        sig_mode: sig_mode,
        mcs: 0,
        smoothing: 0,
        not_sounding: 0,
        aggregation: 0,
        stbc: stbc,
        fec_coding: 0,
        ampdu_cnt: 0,
        noise_floor: 0,
        rx_state: 0,
        sig_len: 0,
        date_time: None,
        sequence_number,
        data_format,
        packet_drop_count: 0,
        csi_data_len: csi_len,
        csi_data,
    };

    #[cfg(feature = "esp32c6")]
    let data_packet = CSIDataPacket {
        mac: mac_address,
        rssi: 0,
        timestamp,
        rate: 0,
        noise_floor: 0,
        sig_len: 0,
        rx_state: 0,
        dump_len: 0,
        he_sigb_len: 0,
        cur_single_mpdu: 0,
        cur_bb_format: 0,
        rx_channel_estimate_info_vld: 0,
        rx_channel_estimate_len: 0,
        second: 0,
        channel: 0,
        is_group: 0,
        rxend_state: 0,
        rxmatch3: 0,
        rxmatch2: 0,
        rxmatch1: 0,
        rxmatch0: 0,
        date_time: None,
        sequence_number,
        data_format: RxCSIFmt::Undefined,
        csi_data_len: csi_len,
        csi_data,
    };

    Some(data_packet)
}
