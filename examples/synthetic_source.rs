//! **Synthetic source** — feed measurements that did not come from the ESP radio through the same
//! pipeline the radio's own CSI takes.
//!
//! | Attribute | Value |
//! |---|---|
//! | Source | `SyntheticBf` — a deterministic IEEE 802.11bf-shaped generator (`synthetic` feature) |
//! | Reporting policy | `REPORTING` below |
//! | Session role | none — this program is the run's controller |
//!
//! A `MeasurementSource` hands its frames to `emit`, which applies the publish gate, the reporting
//! policy, this node's envelope (node id, session id, the next `stream_seq`), the statistics and
//! the delivery path, exactly as for a captured frame. The generator emits what an 802.11bf sensing
//! initiator collects: one `CsiPayload::Grouped` report per responder per sounding, stamped with
//! the measurement setup and the sounding instance. The callback below regroups them by instance,
//! which is the shape a host consumer of a real 802.11bf source would also see.
//!
//! No radio and no second board: this runs on any supported chip. An out-of-tree driver, or a
//! future 802.11bf mode, implements `MeasurementSource` the same way.
//!
//! Build / run:
//!   cargo esp32c6 --example synthetic_source --features synthetic

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::Timer;
use esp_csi_rs::csi::CsiPacket;
use esp_csi_rs::logging::logging::{LogMode, init_logger};
use esp_csi_rs::wire::synthetic::SyntheticBf;
use esp_csi_rs::wire::{CsiPayload, Stimulus};
use esp_csi_rs::{
    ReportingPolicy, emit, log_ln, set_csi_callback, set_csi_logging_enabled, set_reporting,
    set_session,
};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use portable_atomic::{AtomicU32, Ordering};
use {esp_backtrace as _, esp_println as _};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

// ── Knobs ───────────────────────────────────────────────────────────────────────────────────────

/// Responders taking part in the measurement setup.
const RESPONDERS: [[u8; 6]; 3] = [
    [0x02, 0, 0, 0, 0, 0xa1],
    [0x02, 0, 0, 0, 0, 0xa2],
    [0x02, 0, 0, 0, 0, 0xa3],
];

/// Measurement setup id, subcarrier grouping, and the interval between soundings.
const SETUP_ID: u8 = 1;
const NG: u8 = 4;
const PERIOD_MS: u64 = 100;

/// `Decimate(n)` keeps every *n*th report.
const REPORTING: ReportingPolicy = ReportingPolicy::Always;

// ────────────────────────────────────────────────────────────────────────────────────────────────

static REPORTS: AtomicU32 = AtomicU32::new(0);
static LAST_INSTANCE: AtomicU32 = AtomicU32::new(0);
static RESPONDERS_IN_INSTANCE: AtomicU32 = AtomicU32::new(0);
static LAST_N_SC: AtomicU32 = AtomicU32::new(0);
static LAST_STREAM_SEQ: AtomicU32 = AtomicU32::new(0);

fn on_csi(packet: &CsiPacket) {
    REPORTS.fetch_add(1, Ordering::Relaxed);
    LAST_STREAM_SEQ.store(packet.envelope.stream_seq, Ordering::Relaxed);
    if let Stimulus::Controlled { instance_id, .. } = packet.frame.stimulus {
        if LAST_INSTANCE.swap(instance_id as u32, Ordering::Relaxed) != instance_id as u32 {
            RESPONDERS_IN_INSTANCE.store(0, Ordering::Relaxed);
        }
        RESPONDERS_IN_INSTANCE.fetch_add(1, Ordering::Relaxed);
    }
    if let CsiPayload::Grouped { n_sc, .. } = &packet.frame.payload {
        LAST_N_SC.store(*n_sc as u32, Ordering::Relaxed);
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    init_logger(spawner, LogMode::Text);
    set_csi_logging_enabled(false);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    set_session(0x5e55_0001, None);
    set_reporting(REPORTING);
    set_csi_callback(on_csi);

    let mut source = SyntheticBf::new(SETUP_ID, RESPONDERS, NG, (PERIOD_MS * 1000) as u32, 42);
    log_ln!(
        "Synthetic 802.11bf source: setup {}, {} responders, Ng {}, {} subcarriers per report",
        SETUP_ID,
        RESPONDERS.len(),
        NG,
        source.n_sc()
    );

    let mut tick = 0u32;
    loop {
        for _ in 0..RESPONDERS.len() {
            let frame = source.next_frame();
            emit(&source, frame);
        }
        Timer::after_millis(PERIOD_MS).await;

        tick += 1;
        if tick % 10 == 0 {
            log_ln!(
                "reports {}, stream_seq {}, instance {} ({} of {} responders), {} subcarriers",
                REPORTS.load(Ordering::Relaxed),
                LAST_STREAM_SEQ.load(Ordering::Relaxed),
                LAST_INSTANCE.load(Ordering::Relaxed),
                RESPONDERS_IN_INSTANCE.load(Ordering::Relaxed),
                RESPONDERS.len(),
                LAST_N_SC.load(Ordering::Relaxed),
            );
        }
    }
}
