//! Hardware check for the 0.12 wire contract: one firmware, configured at build time.
//!
//! Emits `LogMode::Serialized` (the `wire` format) so a host can decode every frame, plus a
//! framed statistics line every few seconds. Configure with environment variables at build time:
//!
//! | Variable | Values (default first) |
//! |---|---|
//! | `HW_MODE` | `sniffer`, `emitter`, `espnow-central`, `espnow-peripheral` |
//! | `HW_CH` | primary channel, default `6` |
//! | `HW_PHY` | `ht20`, `he20` — emitter / ESP-NOW forced PHY |
//! | `HW_REPORT` | `always`, `never`, `decim4`, `thr`, `thrvar` |
//! | `HW_CSI` | `default`, `he20`, `ht` (HT-LTF only), `nolltf` (default without forced L-LTF, C5) |
//! | `HW_PROTO` | unset, or `ax` — `set_protocol(AX)` (sniffer HE bring-up) |
//! | `HW_HZ` | traffic rate in Hz, default `100` |
//! | `HW_SETUP` | unset, or `1` — apply a `MeasurementSetup` (id 5) to an ESP-NOW central |
//! | `HW_LOG` | `serialized`, `text`, `array`, `tool` |
//! | `HW_EPOCH` | unset, or UNIX seconds — `set_session(0x5e55_1013, epoch)` before the run |
//!
//! ```sh
//! HW_MODE=emitter HW_PHY=he20 cargo build --release --example hw_check \
//!   --features esp32c5,statistics --target riscv32imac-unknown-none-elf
//! ```

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};
use esp_csi_rs::config::CsiConfig;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::wire::{Bandwidth, MeasurementSetup, StimulusParams};
use esp_csi_rs::{
    CSINode, CSINodeClient, EmitterConfig, EmitterPhy, EspNowConfig, NetworkRole, NodeHardware,
    ReportingPolicy, Threshold, WifiSnifferConfig, log_ln,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::WifiPhyRate;
use esp_radio::wifi::WifiController;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

static WIFI_CONTROLLER: static_cell::StaticCell<WifiController<'static>> =
    static_cell::StaticCell::new();

const fn eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn num(s: Option<&str>, default: u32) -> u32 {
    let Some(s) = s else { return default };
    let b = s.as_bytes();
    let mut v = 0u32;
    let mut i = 0;
    while i < b.len() {
        v = v * 10 + (b[i] - b'0') as u32;
        i += 1;
    }
    v
}

const MODE: &str = match option_env!("HW_MODE") {
    Some(m) => m,
    None => "sniffer",
};
const CH: u8 = num(option_env!("HW_CH"), 6) as u8;
const HZ: u16 = num(option_env!("HW_HZ"), 100) as u16;
const HE: bool = matches!(option_env!("HW_PHY"), Some(p) if eq(p, "he20"));
const CSI_HE: bool = matches!(option_env!("HW_CSI"), Some(p) if eq(p, "he20"));
#[allow(dead_code)]
const CSI_HT: bool = matches!(option_env!("HW_CSI"), Some(p) if eq(p, "ht"));
#[allow(dead_code)]
const CSI_NOLLTF: bool = matches!(option_env!("HW_CSI"), Some(p) if eq(p, "nolltf"));
const PROTO_AX: bool = matches!(option_env!("HW_PROTO"), Some(p) if eq(p, "ax"));
const SETUP: bool = option_env!("HW_SETUP").is_some();
const EPOCH: Option<&str> = option_env!("HW_EPOCH");

const fn log_mode() -> LogMode {
    match option_env!("HW_LOG") {
        Some(l) if eq(l, "text") => LogMode::Text,
        Some(l) if eq(l, "array") => LogMode::ArrayList,
        Some(l) if eq(l, "tool") => LogMode::EspCsiTool,
        _ => LogMode::Serialized,
    }
}

const fn reporting() -> ReportingPolicy {
    match option_env!("HW_REPORT") {
        Some(r) if eq(r, "never") => ReportingPolicy::Never,
        Some(r) if eq(r, "decim4") => ReportingPolicy::Decimate(4),
        Some(r) if eq(r, "thr") => ReportingPolicy::Threshold(Threshold::new(6000, 200)),
        Some(r) if eq(r, "thrvar") => {
            ReportingPolicy::Threshold(Threshold::new(2000, 200).variation_only())
        }
        _ => ReportingPolicy::Always,
    }
}

fn csi_config() -> CsiConfig {
    // HE-LTF acquisition and the HE PHY exist only on the 802.11ax parts.
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    {
        if CSI_HE {
            return CsiConfig::he20();
        }
        if CSI_HT {
            let mut c = CsiConfig::default();
            c.acquire_csi_legacy = 0;
            c.acquire_csi_ht40 = 0;
            c.acquire_csi_su = 0;
            c.acquire_csi_mu = 0;
            c.acquire_csi_dcm = 0;
            c.acquire_csi_beamformed = 0;
            c.dump_ack_en = 0;
            #[cfg(feature = "esp32c5")]
            {
                c.acquire_csi_force_lltf = false;
                c.acquire_csi_vht = false;
            }
            return c;
        }
        #[cfg(feature = "esp32c5")]
        if CSI_NOLLTF {
            let mut c = CsiConfig::default();
            c.acquire_csi_force_lltf = false;
            return c;
        }
    }
    CsiConfig::default()
}

fn emitter_phy() -> EmitterPhy {
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    if HE {
        return EmitterPhy::He20;
    }
    EmitterPhy::Ht20
}

fn maybe_he20(cfg: EspNowConfig) -> EspNowConfig {
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    if HE {
        return cfg.with_he20();
    }
    cfg
}

async fn stats_task() {
    let mut tx = [esp_csi_rs::TxStats::default(); 4];
    loop {
        Timer::after(Duration::from_secs(5)).await;
        let d = esp_csi_rs::get_drop_breakdown();
        log_ln!(
            "STATS rx={} tx={} lost={} filtered={} policy={} oversize={} qfull={} seqgap={} logdrop={} reporting={:?}",
            esp_csi_rs::get_total_rx_packets(),
            esp_csi_rs::get_total_tx_packets(),
            esp_csi_rs::get_dropped_packets_rx(),
            d.filtered,
            d.policy_suppressed,
            d.oversize,
            d.queue_full,
            d.seq_gap,
            d.log_dropped,
            esp_csi_rs::runtime_reporting(),
        );
        let n = esp_csi_rs::snapshot_tx_stats(&mut tx);
        for t in &tx[..n] {
            log_ln!(
                "TX {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} frames={} gaps={} retries={}",
                t.mac[0], t.mac[1], t.mac[2], t.mac[3], t.mac[4], t.mac[5],
                t.frames, t.seq_gaps, t.retries
            );
        }
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, log_mode());

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 61440);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let wifi_controller = WifiController::new(peripherals.WIFI, Default::default())
        .expect("Failed to initialize Wi-Fi controller");
    let controller = WIFI_CONTROLLER.init(wifi_controller);
    let hardware = NodeHardware::new(controller);

    log_ln!(
        "HWCHECK mode={} ch={} he={} csi_he={} ax={} hz={} reporting={:?} setup={}",
        MODE, CH, HE, CSI_HE, PROTO_AX, HZ, reporting(), SETUP
    );

    let mut node = if eq(MODE, "emitter") {
        let phy = emitter_phy();
        CSINode::emitter(
            EmitterConfig::new(CH, phy).with_period(Duration::from_micros(1_000_000 / HZ as u64)),
            hardware,
        )
    } else if eq(MODE, "espnow-central") || eq(MODE, "espnow-peripheral") {
        let role = if eq(MODE, "espnow-central") {
            NetworkRole::Central
        } else {
            NetworkRole::Peripheral
        };
        let cfg = maybe_he20(
            EspNowConfig::default()
                .with_channel(CH)
                .with_phy_rate(WifiPhyRate::RateMcs0Lgi)
                .with_network_role(role)
                .with_reporting(reporting()),
        );
        let mut n = CSINode::esp_now(cfg, Some(csi_config()), Some(HZ), hardware);
        if SETUP {
            let setup = MeasurementSetup::new(5)
                .with_stimulus(StimulusParams::new(1_000_000 / HZ as u32, Bandwidth::Mhz20));
            match n.apply_measurement_setup(&setup) {
                Ok(()) => log_ln!("HWCHECK setup applied"),
                Err(_) => log_ln!("HWCHECK setup refused"),
            }
        }
        n
    } else {
        let mut cfg = WifiSnifferConfig::default().with_channel(CH);
        match reporting() {
            ReportingPolicy::Decimate(n) => cfg = cfg.with_decimation(n),
            ReportingPolicy::Threshold(t) => cfg = cfg.with_threshold(t),
            _ => {}
        }
        CSINode::sniffer(cfg, Some(csi_config()), hardware)
    };
    if PROTO_AX {
        node.set_protocol(esp_radio::wifi::Protocol::AX);
    }

    if EPOCH.is_some() {
        let secs = num(EPOCH, 0) as u64;
        esp_csi_rs::set_session(0x5e55_1013, Some(secs * 1_000_000));
    }

    let mut client = CSINodeClient::new();
    let _ = &mut client;
    join(node.run(), stats_task()).await;

    loop {
        log_ln!("HWCHECK stopped");
        Timer::after(Duration::from_secs(5)).await;
    }
}
