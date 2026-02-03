#![no_std]

use portable_atomic::AtomicI64;

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use embassy_futures::join::join;
use embassy_futures::select::{select3, Either3};
use embassy_sync::pubsub::{PubSubBehavior, Subscriber};

use embassy_time::Instant;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::{ClientConfig, CsiConfig, Interfaces, Protocol, WifiController};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

use embassy_sync::pubsub::PubSubChannel;
use embassy_sync::signal::Signal;

use heapless::Vec;
extern crate alloc;
use alloc::collections::BTreeMap;

pub mod central;
pub mod config;
pub mod csi;
pub mod logging;
pub mod peripheral;
pub mod time;

use crate::central::esp_now::run_esp_now_central;
use crate::central::sta::{run_sta_connect, sta_init};
use crate::config::CsiConfig as CsiConfiguration;
use crate::csi::{CSIDataPacket, RxCSIFmt};
use crate::peripheral::esp_now::run_esp_now_peripheral;

const PROC_CSI_CH_CAPACITY: usize = 20;
const PROC_CSI_CH_SUBS: usize = 2;

// PubSub Channels
static CSI_PACKET: PubSubChannel<
    CriticalSectionRawMutex,
    CSIDataPacket,
    PROC_CSI_CH_CAPACITY,
    PROC_CSI_CH_SUBS,
    2,
> = PubSubChannel::new();

static IS_COLLECTOR: AtomicBool = AtomicBool::new(false);
static COLLECTION_MODE_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static CENTRAL_MAGIC_NUMBER: u32 = 0xA8912BF0;
static PERIPHERAL_MAGIC_NUMBER: u32 = !CENTRAL_MAGIC_NUMBER;
static AVG_LATENCY: AtomicI64 = AtomicI64::new(0);

// Signals
static STOP_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

fn set_runtime_collection_mode(is_collector: bool) {
    IS_COLLECTOR.store(is_collector, Ordering::Relaxed);
    COLLECTION_MODE_CHANGED.signal(());
}

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
    2,
>;

pub struct CSIClient {
    csi_subscriber: CSIRxSubscriber,
}

impl CSIClient {
    pub fn new() -> Self {
        Self {
            csi_subscriber: CSI_PACKET.subscriber().unwrap(),
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

#[derive(IntoBytes, FromBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct ControlPacket {
    magic_number: u32,        // Magic number to identify packet type
    is_collector: u8,         // 1 = Collector, 0 = Listener
    _padding: [u8; 3],        // Align t1 to 8-byte boundary
    central_send_uptime: u64, // When Central sent this Control Packet
}

impl ControlPacket {
    pub fn new(is_collector: bool) -> Self {
        Self {
            magic_number: CENTRAL_MAGIC_NUMBER,
            is_collector: is_collector as u8,
            _padding: [0u8; 3],
            central_send_uptime: Instant::now().as_micros(),
        }
    }
}

#[derive(IntoBytes, FromBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct PeripheralPacket {
    magic_number: u32,        // Magic number to identify packet type
    _padding: [u8; 4],        // Align timestamps to 8-byte boundary
    central_send_uptime: u64, // When Central sent the Control Packet
}

impl PeripheralPacket {
    pub fn new(central_send_uptime: u64) -> Self {
        Self {
            magic_number: PERIPHERAL_MAGIC_NUMBER,
            _padding: [0u8; 4],
            central_send_uptime,
        }
    }
}

pub struct CSINode<'a> {
    kind: Node,
    collection_mode: CollectionMode,
    /// CSI Configuration
    csi_config: Option<CsiConfiguration>,
    /// Traffic Generation Frequency
    traffic_freq_hz: Option<u16>,
    hardware: CSINodeHardware<'a>,
    protocol: Option<Protocol>,
    rate: Option<WifiPhyRate>,
}

impl<'a> CSINode<'a> {
    pub fn new(
        kind: Node,
        collection_mode: CollectionMode,
        csi_config: Option<CsiConfiguration>,
        traffic_freq_hz: Option<u16>,
        hardware: CSINodeHardware<'a>,
    ) -> Self {
        Self {
            kind,
            collection_mode,
            csi_config,
            traffic_freq_hz,
            hardware,
            protocol: Some(Protocol::P802D11BGNLR),
            rate: Some(WifiPhyRate::RateMcs0Lgi),
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
            protocol: Some(Protocol::P802D11BGNLR),
            rate: Some(WifiPhyRate::RateMcs0Lgi),
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

    pub fn set_protocol(&mut self, protocol: Protocol) {
        self.protocol = Some(protocol);
    }

    pub fn set_rate(&mut self, rate: WifiPhyRate) {
        self.rate = Some(rate);
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

        // Apply Protocol if specified
        if let Some(protocol) = self.protocol.take() {
            let old_protocol = reconstruct_protocol(&protocol);
            controller.set_protocol(protocol.into()).unwrap();
            self.protocol = Some(old_protocol);
        }

        // Start the controller
        controller.start_async().await.unwrap();
        log_ln!("Wi-Fi Controller Started");

        let is_collector = self.collection_mode == CollectionMode::Collector;
        IS_COLLECTOR.store(is_collector, Ordering::Relaxed);

        // Set Peripheral/Central to Collect CSI
        set_csi(controller, config);
        let sniffer: &esp_radio::wifi::Sniffer<'_> = &interfaces.sniffer;

        // Initialize Nodes based on type
        match &self.kind {
            Node::Peripheral(op_mode) => match op_mode {
                PeripheralOpMode::EspNow(esp_now_config) => {
                    // Initialize as Peripheral node with EspNowConfig
                    if let Some(rate) = self.rate.take() {
                        let old_rate = reconstruct_wifi_rate(&rate);
                        let _ = interfaces.esp_now.set_rate(rate);
                        self.rate = Some(old_rate);
                    }

                    let main_task = run_esp_now_peripheral(&mut interfaces.esp_now, esp_now_config);
                    join(main_task, run_process_csi_packet()).await;
                }
                PeripheralOpMode::WifiSniffer(sniffer_config) => {
                    let sniffer = &interfaces.sniffer;
                    sniffer.set_promiscuous_mode(true).unwrap();
                    run_process_csi_packet().await;
                    sniffer.set_promiscuous_mode(false).unwrap();
                }
            },
            Node::Central(op_mode) => match op_mode {
                CentralOpMode::EspNow(esp_now_config) => {
                    // Initialize as Central node with EspNowConfig
                    if let Some(rate) = self.rate.take() {
                        let old_rate = reconstruct_wifi_rate(&rate);
                        let _ = interfaces.esp_now.set_rate(rate);
                        self.rate = Some(old_rate);
                    }

                    let main_task = run_esp_now_central(
                        &mut interfaces.esp_now,
                        interfaces.sta.mac_address(),
                        esp_now_config,
                        self.traffic_freq_hz,
                        is_collector,
                    );
                    join(main_task, run_process_csi_packet()).await;
                }
                CentralOpMode::WifiStation(sta_config) => {
                    // Initialize as Wifi Station Collector with WifiStationConfig
                    // 1. Connect to Wi-Fi network, etc.
                    // 2. Run DHCP, NTP sync if enabled in config, etc.
                    // 3. Spawn STA Connection Handling Task
                    // 4. Spawn STA Network Operation Task
                    let (sta_stack, sta_runner) = sta_interface.unwrap();

                    let main_task =
                        run_sta_connect(controller, self.traffic_freq_hz, sta_stack, sta_runner);
                    join(main_task, run_process_csi_packet()).await;
                }
            },
        }

        STOP_SIGNAL.reset();
        let _ = controller.stop_async().await;
        GLOBAL_PACKET_COUNT.store(0, Ordering::Relaxed);
        GLOBAL_DROP_COUNT.store(0, Ordering::Relaxed);
        reset_global_log_drops();
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

use portable_atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
    if IS_COLLECTOR.load(Ordering::Relaxed) == false {
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
    GLOBAL_PACKET_COUNT.fetch_add(1, Ordering::Relaxed);
}

pub async fn run_process_csi_packet() {
    // Initialize CSI process start time
    GLOBAL_CAPTURE_START_TIME.store(Instant::now().as_ticks(), Ordering::Relaxed);
    // Subscribe to CSI packet capture updates
    let mut csi_packet_sub = CSI_PACKET.subscriber().unwrap();
    // Map to track sequence numbers per MAC address
    let mut peer_tracker: BTreeMap<[u8; 6], u16> = BTreeMap::new();
    let mut is_collector = IS_COLLECTOR.load(Ordering::Relaxed);

    loop {
        match select3(
            STOP_SIGNAL.wait(),
            COLLECTION_MODE_CHANGED.wait(),
            csi_packet_sub.next_message_pure(),
        )
        .await
        {
            Either3::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either3::Second(_) => {
                COLLECTION_MODE_CHANGED.reset();
                is_collector = IS_COLLECTOR.load(Ordering::Relaxed);
                GLOBAL_PACKET_COUNT.store(0, Ordering::Relaxed);
                GLOBAL_DROP_COUNT.store(0, Ordering::Relaxed);
                reset_global_log_drops();
                GLOBAL_CAPTURE_START_TIME.store(Instant::now().as_ticks(), Ordering::Relaxed);
            }
            Either3::Third(csi_packet) => {
                if is_collector {
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
                    // --- DROP DETECTION LOGIC END ---
                }
            }
        }
    }
}

use crate::logging::logging::{get_log_packet_drops, reset_global_log_drops};

pub fn get_dropped_packets() -> u32 {
    GLOBAL_DROP_COUNT.load(Ordering::Relaxed) + get_log_packet_drops()
}

pub fn get_avg_latency() -> i64 {
    AVG_LATENCY.load(Ordering::Relaxed)
}

fn reconstruct_wifi_rate(rate: &WifiPhyRate) -> WifiPhyRate {
    match rate {
        WifiPhyRate::Rate1mL => WifiPhyRate::Rate1mL,
        WifiPhyRate::Rate2m => WifiPhyRate::Rate2m,
        WifiPhyRate::Rate5mL => WifiPhyRate::Rate5mL,
        WifiPhyRate::Rate11mL => WifiPhyRate::Rate11mL,
        WifiPhyRate::Rate2mS => WifiPhyRate::Rate2mS,
        WifiPhyRate::Rate5mS => WifiPhyRate::Rate5mS,
        WifiPhyRate::Rate11mS => WifiPhyRate::Rate11mS,
        WifiPhyRate::Rate48m => WifiPhyRate::Rate48m,
        WifiPhyRate::Rate24m => WifiPhyRate::Rate24m,
        WifiPhyRate::Rate12m => WifiPhyRate::Rate12m,
        WifiPhyRate::Rate6m => WifiPhyRate::Rate6m,
        WifiPhyRate::Rate54m => WifiPhyRate::Rate54m,
        WifiPhyRate::Rate36m => WifiPhyRate::Rate36m,
        WifiPhyRate::Rate18m => WifiPhyRate::Rate18m,
        WifiPhyRate::Rate9m => WifiPhyRate::Rate9m,
        WifiPhyRate::RateMcs0Lgi => WifiPhyRate::RateMcs0Lgi,
        WifiPhyRate::RateMcs1Lgi => WifiPhyRate::RateMcs1Lgi,
        WifiPhyRate::RateMcs2Lgi => WifiPhyRate::RateMcs2Lgi,
        WifiPhyRate::RateMcs3Lgi => WifiPhyRate::RateMcs3Lgi,
        WifiPhyRate::RateMcs4Lgi => WifiPhyRate::RateMcs4Lgi,
        WifiPhyRate::RateMcs5Lgi => WifiPhyRate::RateMcs5Lgi,
        WifiPhyRate::RateMcs6Lgi => WifiPhyRate::RateMcs6Lgi,
        WifiPhyRate::RateMcs7Lgi => WifiPhyRate::RateMcs7Lgi,
        WifiPhyRate::RateMcs0Sgi => WifiPhyRate::RateMcs0Sgi,
        WifiPhyRate::RateMcs1Sgi => WifiPhyRate::RateMcs1Sgi,
        WifiPhyRate::RateMcs2Sgi => WifiPhyRate::RateMcs2Sgi,
        WifiPhyRate::RateMcs3Sgi => WifiPhyRate::RateMcs3Sgi,
        WifiPhyRate::RateMcs4Sgi => WifiPhyRate::RateMcs4Sgi,
        WifiPhyRate::RateMcs5Sgi => WifiPhyRate::RateMcs5Sgi,
        WifiPhyRate::RateMcs6Sgi => WifiPhyRate::RateMcs6Sgi,
        WifiPhyRate::RateMcs7Sgi => WifiPhyRate::RateMcs7Sgi,
        WifiPhyRate::RateLora250k => WifiPhyRate::RateLora250k,
        WifiPhyRate::RateLora500k => WifiPhyRate::RateLora500k,
        WifiPhyRate::RateMax => WifiPhyRate::RateMax,
    }
}

fn reconstruct_protocol(protocol: &Protocol) -> Protocol {
    match protocol {
        Protocol::P802D11B => Protocol::P802D11B,
        Protocol::P802D11BG => Protocol::P802D11BG,
        Protocol::P802D11BGN => Protocol::P802D11BGN,
        Protocol::P802D11BGNLR => Protocol::P802D11BGNLR,
        Protocol::P802D11LR => Protocol::P802D11LR,
        Protocol::P802D11BGNAX => Protocol::P802D11BGNAX,
        _ => Protocol::P802D11BGNLR,
    }
}