//! The **emitter** role: a transmit-only node that sounds the channel.
//!
//! An emitter exists to put known RF energy into the channel so that collectors
//! can measure the channel's response to it. It never associates and never
//! captures CSI: it forces its interface to a fixed TX PHY (see [`phy`]) and
//! loop-injects a raw, rate-agnostic frame (see [`frame`]) at a configured
//! period.
//!
//! Because the frame carries no meaning, an emitter needs no peer, no handshake,
//! and no protocol — which is what makes it compose into any topology. One
//! emitter with many collectors, or several emitters distinguished by their
//! source MAC, are both just deployment choices.

pub mod frame;
pub mod phy;

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};

use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, Interfaces, SecondaryChannel, WifiController};

use crate::radio::apply_band_for_channel;
use crate::{STOP_SIGNAL, log_ln};

use frame::{BROADCAST, PROBE_FRAME_LEN, build_probe_frame, inject_probe_once};

/// Largest frame the CPU-utilization harness can pad up to. Sized to the 802.11
/// MTU rather than the minimum sounding frame so the experiment can sweep
/// on-air frame size without a reallocation.
#[cfg(feature = "cpu-test-tx")]
const CPU_TEST_MAX_FRAME_LEN: usize = 1500;

/// The padded buffer must still hold a minimum well-formed sounding frame.
#[cfg(feature = "cpu-test-tx")]
const _: () = assert!(CPU_TEST_MAX_FRAME_LEN >= PROBE_FRAME_LEN);

/// Bandwidth and secondary-channel selection for an emitter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HtBandwidth {
    /// HT20 — 20 MHz, no secondary channel.
    Ht20,
    /// HT40 — 40 MHz with the secondary channel above the primary.
    Ht40Above,
    /// HT40 — 40 MHz with the secondary channel below the primary.
    Ht40Below,
}

impl HtBandwidth {
    /// Whether this is a 40 MHz configuration.
    pub fn is_forty(self) -> bool {
        !matches!(self, HtBandwidth::Ht20)
    }

    /// The secondary-channel offset this bandwidth implies.
    pub fn secondary(self) -> SecondaryChannel {
        match self {
            HtBandwidth::Ht20 => SecondaryChannel::None,
            HtBandwidth::Ht40Above => SecondaryChannel::Above,
            HtBandwidth::Ht40Below => SecondaryChannel::Below,
        }
    }
}

/// TX parameters for an emitter.
#[derive(Clone, Copy)]
pub struct EmitterConfig {
    /// Primary channel. Every node in a capture set must share it.
    pub channel: u8,
    /// HT20, or HT40 with the secondary channel above/below the primary.
    pub bandwidth: HtBandwidth,
    /// Destination address of injected frames (broadcast by default). Addressing
    /// a specific collector tends to raise that collector's CSI callback rate.
    pub dst_mac: [u8; 6],
    /// Delay between injected frames (20 ms ≈ 50 frames/s by default).
    pub period: Duration,
    /// Inject on the STA interface (`true`) or the AP interface (`false`).
    pub use_sta_if: bool,
}

impl EmitterConfig {
    /// New config on `channel` at `bandwidth`, broadcast destination, 20 ms
    /// period, STA interface.
    pub fn new(channel: u8, bandwidth: HtBandwidth) -> Self {
        Self {
            channel,
            bandwidth,
            dst_mac: BROADCAST,
            period: Duration::from_millis(20),
            use_sta_if: true,
        }
    }

    /// Set the destination address of injected frames.
    pub fn with_dst_mac(mut self, dst_mac: [u8; 6]) -> Self {
        self.dst_mac = dst_mac;
        self
    }

    /// Set the delay between injected frames.
    pub fn with_period(mut self, period: Duration) -> Self {
        self.period = period;
        self
    }

    /// Inject on the AP interface instead of the STA interface.
    pub fn with_ap_interface(mut self) -> Self {
        self.use_sta_if = false;
        self
    }
}

impl Default for EmitterConfig {
    fn default() -> Self {
        Self::new(1, HtBandwidth::Ht20)
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for EmitterConfig {
    fn format(&self, fmt: defmt::Formatter<'_>) {
        defmt::write!(
            fmt,
            "EmitterConfig {{ channel: {}, forty: {}, period_ms: {} }}",
            self.channel,
            self.bandwidth.is_forty(),
            self.period.as_millis()
        );
    }
}

/// Force the TX PHY, start the interface **without associating**, and lock the
/// channel.
///
/// The order matters: the forced TX rate has to be applied before `set_config`
/// starts the interface, and the bandwidth/protocol set has to be re-applied
/// after, because `set_config` embeds its own defaults.
fn bringup(controller: &mut WifiController<'_>, cfg: &EmitterConfig) {
    let forty = cfg.bandwidth.is_forty();

    // `set_config` only calls `esp_wifi_start()` when the *mode* changes, so the
    // forced rate has to be applied before it — hence the `_before_start` names.
    // The driver's status code is reported rather than discarded: a rate that was
    // never applied produces an emitter that looks healthy while transmitting in
    // the wrong format, or not at all.
    let rc = if cfg.use_sta_if {
        let rc = if forty {
            phy::force_ht40_tx_sta_before_start()
        } else {
            phy::force_ht20_tx_sta_before_start()
        };
        if controller
            .set_config(&Config::Station(StationConfig::default()))
            .is_err()
        {
            log_ln!("emitter: set_config(Station) failed");
        }
        rc
    } else {
        let rc = if forty {
            phy::force_ht40_tx_ap_before_start()
        } else {
            phy::force_ht20_tx_ap_before_start()
        };
        if controller
            .set_config(&Config::AccessPoint(AccessPointConfig::default()))
            .is_err()
        {
            log_ln!("emitter: set_config(AccessPoint) failed");
        }
        rc
    };
    if rc != 0 {
        log_ln!(
            "Emitter: forced TX PHY rejected (rc={}); frames will not use the requested format",
            rc
        );
    }

    // Re-apply after `set_config`, which embeds its own defaults, then re-force the
    // rate: `set_config` may stop/start the interface, and a restart drops a rate
    // that was configured before it.
    apply_band_for_channel(controller, cfg.channel);
    phy::apply_ht_bandwidth(controller, forty);
    phy::apply_ht_protocols(controller);
    if controller
        .set_channel(cfg.channel, cfg.bandwidth.secondary())
        .is_err()
    {
        log_ln!("emitter: set_channel failed");
    }
    let rc_post = if cfg.use_sta_if {
        if forty {
            phy::force_ht40_tx_sta_before_start()
        } else {
            phy::force_ht20_tx_sta_before_start()
        }
    } else if forty {
        phy::force_ht40_tx_ap_before_start()
    } else {
        phy::force_ht20_tx_ap_before_start()
    };
    log_ln!("Emitter: forced TX PHY rc pre-start={} post-start={}", rc, rc_post);
}

/// Run the emitter: bring up the radio, then loop-inject until stopped.
pub async fn run_emitter(
    controller: &mut WifiController<'static>,
    interfaces: &mut Interfaces<'static>,
    cfg: &EmitterConfig,
) {
    bringup(controller, cfg);

    // Source address is the interface the frames actually leave from, so a
    // collector can attribute each frame to this emitter.
    let src = if cfg.use_sta_if {
        interfaces.station.mac_address()
    } else {
        interfaces.access_point.mac_address()
    };

    // The CPU-utilization experiment pads the frame, so size the buffer for the
    // largest frame that harness can ask for rather than the minimum.
    #[cfg(feature = "cpu-test-tx")]
    let mut frame = [0u8; CPU_TEST_MAX_FRAME_LEN];
    #[cfg(not(feature = "cpu-test-tx"))]
    let mut frame = [0u8; PROBE_FRAME_LEN];

    let len = build_probe_frame(&src, &cfg.dst_mac, &mut frame);

    log_ln!(
        "Emitter running: ch {}, {} MHz, {} ms period, src {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        cfg.channel,
        if cfg.bandwidth.is_forty() { 40 } else { 20 },
        cfg.period.as_millis(),
        src[0],
        src[1],
        src[2],
        src[3],
        src[4],
        src[5]
    );

    // A raw injection that the driver rejects is silent otherwise: the loop keeps
    // running and the node looks healthy while nothing reaches the air. Report the
    // first rejection (and only the first, to keep the hot path quiet) so a
    // "collector sees nothing" investigation starts at the right end.
    let mut reported_failure = false;

    loop {
        // Under the CPU-utilization harness the rate, frame size, and silence are
        // steered per phase from the experiment's schedule; otherwise the
        // configured period and minimum frame are used unchanged.
        #[cfg(feature = "cpu-test-tx")]
        let (period, len, paused) = {
            use portable_atomic::Ordering;
            let rate = crate::TEST_TX_RATE_HZ.load(Ordering::Relaxed).max(1);
            let want = crate::TEST_TX_PAYLOAD_B.load(Ordering::Relaxed) as usize;
            (
                Duration::from_micros(1_000_000 / rate as u64),
                len.max(want.min(CPU_TEST_MAX_FRAME_LEN)),
                crate::TEST_TX_PAUSED.load(Ordering::Relaxed),
            )
        };
        #[cfg(not(feature = "cpu-test-tx"))]
        let (period, len, paused) = (cfg.period, len, false);

        if !paused {
            match inject_probe_once(&mut interfaces.sniffer, cfg.use_sta_if, &frame[..len]) {
                Ok(()) => {
                    // Count accepted frames so `get_pps_tx` / `get_total_tx_packets`
                    // report an emitter's offered rate. Without this an emitter looks
                    // idle in `show-stats`, which is exactly the wrong signal when
                    // diagnosing "the collector sees nothing".
                    #[cfg(feature = "statistics")]
                    crate::stats::record_tx();
                }
                Err(e) => {
                    if !reported_failure {
                        reported_failure = true;
                        log_ln!("Emitter: raw injection rejected by the driver: {:?}", e);
                    }
                }
            }
        }
        match select(STOP_SIGNAL.wait(), Timer::after(period)).await {
            Either::First(_) => {
                log_ln!("STOP signal received, shutting down emitter...");
                STOP_SIGNAL.signal(());
                return;
            }
            Either::Second(_) => {}
        }
    }
}
