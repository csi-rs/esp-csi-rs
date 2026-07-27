//! Wi-Fi station mode central driver.
//!
//! Connects to a configured access point, brings up an embassy-net stack
//! with DHCP, and drives the network/ICMP plumbing required to keep CSI
//! flowing while the device is associated. The Wi-Fi driver delivers CSI
//! samples for received frames out-of-band via the global CSI channel.

use core::cell::UnsafeCell;
use core::future::poll_fn;
use core::mem::MaybeUninit;
use core::net::Ipv4Addr;
use core::task::Poll;
use embassy_futures::join::{join3, join4};
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_net::raw::{IpProtocol, IpVersion, PacketMetadata, RawSocket};
use embassy_net::{Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_radio::wifi::csi::CsiConfig;
use esp_radio::wifi::{Config, Interface, WifiController};
use portable_atomic::{AtomicBool, Ordering};
use smoltcp::phy::ChecksumCapabilities;

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};

use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, Ipv4Packet, Ipv4Repr};

use crate::log_ln;
use crate::profile::RadioProfile;
use crate::{IOTaskConfig, STOP_SIGNAL, WifiStationConfig, set_csi};

static DHCP_CLIENT_INFO: Signal<CriticalSectionRawMutex, IpInfo> = Signal::new();

/// Raw-socket TX queue depth — a single slot caps offered traffic at ~30 Hz
/// because each `send().await` waits for the previous datagram to leave smoltcp.
const ICMP_FLOOD_TX_SLOTS: usize = 16;
const ICMP_FLOOD_RX_SLOTS: usize = 4;
/// Max datagrams queued per scheduler wake (matches `esp_now_central_min_drop`).
const ICMP_FLOOD_CATCH_UP_BURST: u8 = 16;
const ICMP_FLOOD_QUEUE_BACKOFF_US: u64 = 50;

/// One-shot-then-reusable storage for the STA stack's `StackResources`.
///
/// `StaticCell` panics on the second `uninit()`, which broke
/// stop-then-restart cycles for `CSINode::run`. The previous `&mut` borrow
/// is always gone by the time we land here again (`node.run` joins every
/// STA task before returning, dropping the `Stack`/`Runner`), so we can
/// safely hand the same buffer back out.
pub(crate) struct StackResourcesSlot {
    cell: UnsafeCell<MaybeUninit<StackResources<6>>>,
    inited: AtomicBool,
}

// SAFETY: Access is serialised — `sta_init` is only called from
// `CSINode::run`/`run_duration`, which run on a single executor with
// exclusive `&mut self`, and any prior `&mut` to the inner storage has
// been dropped before we get here.
unsafe impl Sync for StackResourcesSlot {}

impl StackResourcesSlot {
    pub(crate) const fn new() -> Self {
        Self {
            cell: UnsafeCell::new(MaybeUninit::uninit()),
            inited: AtomicBool::new(false),
        }
    }

    // Single-call init guarded by `inited`; access is serialised (see the
    // `Sync` impl and the SAFETY note below), so the `&mut`-from-`&` is sound
    // by construction — clippy's `mut_from_ref` is a false positive here.
    #[allow(clippy::mut_from_ref)]
    pub(crate) fn get_or_init(&'static self) -> &'static mut StackResources<6> {
        // SAFETY: see the `Sync` impl. First call writes the value; later
        // calls reuse the same buffer. `StackResources` has no destructor
        // state that depends on initialising "fresh" — embassy-net reuses
        // it just like any user-owned buffer.
        unsafe {
            if !self.inited.swap(true, Ordering::AcqRel) {
                (*self.cell.get()).write(StackResources::<6>::new());
            }
            (*self.cell.get()).assume_init_mut()
        }
    }
}

static STACK_RESOURCES: StackResourcesSlot = StackResourcesSlot::new();

/// DHCP-acquired IP configuration for the STA interface.
#[derive(Debug, Clone)]
struct IpInfo {
    pub local_address: Ipv4Cidr,
    pub gateway_address: Ipv4Address,
}

/// Initialize the station interface and return the network stack and runner.
pub fn sta_init<'a>(
    interfaces: &'a mut Interface<'static>,
    config: &WifiStationConfig,
    controller: &mut WifiController<'static>,
    profile: &dyn RadioProfile,
    bringup: bool,
) -> (Stack<'a>, Runner<'a, &'a mut Interface<'static>>) {
    let sta_ip_config = embassy_net::Config::dhcpv4(Default::default());
    let seed = 123456_u64;

    // Create STA Network Stack and Runner. `StackResources` is held in a
    // reusable static so stop-then-restart cycles don't trip the old
    // `StaticCell::uninit()` panic.
    let (sta_stack, sta_runner) = embassy_net::new(
        interfaces,
        sta_ip_config,
        STACK_RESOURCES.get_or_init(),
        seed,
    );

    // Configure WiFi Client/Station Connection
    let station_config = Config::Station(config.client_config.clone());
    // Set the Configuration. The profile hook fires immediately before
    // `set_config` so a PHY back-end can force TX parameters at the exact point
    // the radio expects them.
    if bringup {
        profile.before_sta_config();
    }
    match controller.set_config(&station_config) {
        Ok(_) => log_ln!("WiFi Configuration Set: {:?}", config),
        Err(_) => {
            log_ln!("WiFi Configuration Error");
            log_ln!("Error Config: {:?}", config);
        }
    }

    (sta_stack, sta_runner)
}

/// Connect to Wi-Fi and run all STA tasks (connection, DHCP, network ops).
pub async fn run_sta_connect(
    controller: &mut WifiController<'_>,
    freq: Option<u16>,
    sta_stack: Stack<'_>,
    sta_runner: Runner<'_, &mut Interface<'_>>,
    csi_config: CsiConfig,
    io_tasks: IOTaskConfig,
) {
    // Settle, watchdog, and recovery policy: after a hard reset the radio can
    // (a) hang inside connect_async, or (b) succeed on retry but deliver no
    // CSI because the controller-level state got wedged. The first is caught
    // by the timeout; the second is caught by re-applying set_csi after a
    // full stop/start cycle.
    const CONNECT_TIMEOUT_SECS: u64 = 10;
    // Any connect failure wedges controller-level state badly enough that the
    // retried association comes up with no CSI traffic, so always cycle the
    // radio and re-apply CSI instead of relying on a bare retry.
    const FAILURES_BEFORE_RADIO_CYCLE: u8 = 1;
    let mut consecutive_failures: u8 = 0;

    // Let the controller settle after start_async before the first connect.
    // Without this, first-boot connect_async often races scan/state setup and
    // returns Err(Disconnected), which wedges CSI on the retried association.
    match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_secs(2))).await {
        Either::First(_) => {
            STOP_SIGNAL.signal(());
            return;
        }
        Either::Second(_) => {}
    }

    // Connect WiFi (retry on transient failures)
    loop {
        let connect_fut = with_timeout(
            Duration::from_secs(CONNECT_TIMEOUT_SECS),
            controller.connect_async(),
        );
        let failure_kind: &str = match select(STOP_SIGNAL.wait(), connect_fut).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                return;
            }
            Either::Second(Ok(Ok(_))) => {
                log_ln!("WiFi Connected");
                if io_tasks.rx_enabled {
                    set_csi(controller, csi_config.clone());
                    log_ln!("CSI re-armed after Wi-Fi association");
                }
                break;
            }
            Either::Second(Ok(Err(e))) => {
                log_ln!("Connect failed: {:?}", e);
                "error"
            }
            Either::Second(Err(_)) => {
                log_ln!("connect_async timed out after {}s", CONNECT_TIMEOUT_SECS);
                "timeout"
            }
        };

        consecutive_failures = consecutive_failures.saturating_add(1);
        // disconnect_async no-ops when not associated; cheap defensive cleanup.
        let _ = controller.disconnect_async().await;

        if consecutive_failures >= FAILURES_BEFORE_RADIO_CYCLE {
            log_ln!(
                "Cycling Wi-Fi controller after {} failures (last: {}) to clear stale state (kind: {})",
                consecutive_failures,
                failure_kind,
                failure_kind
            );
            // esp-radio 0.18 removed start_async/stop_async; reapply CSI filter
            // directly to clear any controller state that wedged after the
            // failed connect attempt.
            set_csi(controller, csi_config.clone());
            Timer::after(Duration::from_millis(300)).await;
            consecutive_failures = 0;
        }

        match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_secs(1))).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                return;
            }
            Either::Second(_) => {}
        }
    }

    if io_tasks.tx_enabled {
        join4(
            sta_connection(controller, csi_config.clone(), io_tasks),
            sta_network_ops(sta_stack, freq),
            run_net_task(sta_runner),
            run_dhcp_client(sta_stack),
        )
        .await;
    } else {
        join3(
            sta_connection(controller, csi_config.clone(), io_tasks),
            run_net_task(sta_runner),
            run_dhcp_client(sta_stack),
        )
        .await;
    }
}

/// Run the embassy-net runner until a stop signal is received.
pub(crate) async fn run_net_task(mut sta_runner: Runner<'_, &mut Interface<'_>>) {
    loop {
        match select(STOP_SIGNAL.wait(), sta_runner.run()).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either::Second(_) => {}
        }
    }
}

/// Run a DHCP client and publish the acquired IP configuration.
///
/// Every await in this task is guarded by `STOP_SIGNAL` — without that, a
/// stop request issued mid-DHCP (or while idly waiting for the link to
/// flap) would leave this future pending forever and prevent the join in
/// `run_sta_connect` from completing, which in turn would hang `node.run()`.
async fn run_dhcp_client(sta_stack: Stack<'_>) {
    log_ln!("Running DHCP Client");

    loop {
        // Check if link is up
        match select(STOP_SIGNAL.wait(), sta_stack.wait_link_up()).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                return;
            }
            Either::Second(_) => {}
        }
        log_ln!("Link is up!");

        // Create instance to store acquired IP information
        let mut ip_info = IpInfo {
            local_address: Ipv4Cidr::new(Ipv4Addr::UNSPECIFIED, 24),
            gateway_address: Ipv4Address::UNSPECIFIED,
        };

        log_ln!("Acquiring config...");
        match select(STOP_SIGNAL.wait(), sta_stack.wait_config_up()).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                return;
            }
            Either::Second(_) => {}
        }
        log_ln!("Config Acquired");

        // Print out acquired IP configuration
        loop {
            if let Some(config) = sta_stack.config_v4() {
                ip_info.local_address = config.address;
                ip_info.gateway_address = config.gateway.unwrap_or(Ipv4Address::UNSPECIFIED);

                let octets = ip_info.local_address.address().octets();
                log_ln!(
                    "Local IP: {}.{}.{}.{}/{}",
                    octets[0],
                    octets[1],
                    octets[2],
                    octets[3],
                    ip_info.local_address.prefix_len()
                );
                let g = ip_info.gateway_address.octets();
                log_ln!("Gateway IP: {}.{}.{}.{}", g[0], g[1], g[2], g[3]);

                break;
            }
            match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_millis(500))).await {
                Either::First(_) => {
                    STOP_SIGNAL.signal(());
                    return;
                }
                Either::Second(_) => {}
            }
        }

        // Publish DHCP info. On reconnect this updates consumers.
        DHCP_CLIENT_INFO.signal(ip_info);

        // Wait until link drops before looping for next lease/config.
        while sta_stack.is_link_up() {
            match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_millis(250))).await {
                Either::First(_) => {
                    STOP_SIGNAL.signal(());
                    return;
                }
                Either::Second(_) => {}
            }
        }
        log_ln!("Link down, waiting to reacquire DHCP config...");
    }
}

/// Monitor STA events (connect/disconnect/stop) until a stop signal.
pub async fn sta_connection(
    controller: &mut WifiController<'_>,
    csi_config: CsiConfig,
    io_tasks: IOTaskConfig,
) {
    // Monitoring/stop loop
    loop {
        match select(STOP_SIGNAL.wait(), controller.wait_for_disconnect_async()).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either::Second(_) => {
                log_ln!("STA Disconnected");

                // Try to reconnect until successful or stop requested.
                loop {
                    match select(STOP_SIGNAL.wait(), controller.connect_async()).await {
                        Either::First(_) => {
                            STOP_SIGNAL.signal(());
                            return;
                        }
                        Either::Second(Ok(_)) => {
                            log_ln!("STA Reconnected");
                            if io_tasks.rx_enabled {
                                set_csi(controller, csi_config.clone());
                                log_ln!("CSI re-armed after Wi-Fi re-association");
                            }
                            break;
                        }
                        Either::Second(Err(e)) => {
                            log_ln!("STA reconnect failed: {:?}", e);
                            match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_secs(1)))
                                .await
                            {
                                Either::First(_) => {
                                    STOP_SIGNAL.signal(());
                                    return;
                                }
                                Either::Second(_) => {}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// When set, the ICMP flood sends unsolicited **echo replies** instead of echo
/// requests. A peer's smoltcp stack silently ignores an unsolicited reply
/// (`Icmpv4Repr::EchoReply => None` in its ICMP dispatch), so the flood
/// becomes strictly one-directional: the receiver still hardware-ACKs every
/// data frame (keeping the sender's rate control fed) and still captures CSI
/// per frame, but never transmits an IP-level response. Halves on-air frame
/// count vs request/reply and removes the CSMA contention that makes the
/// offered rate oscillate. The cost: the flooding node gets no CSI back.
static ICMP_FLOOD_UNSOLICITED: AtomicBool = AtomicBool::new(false);

/// Select unsolicited-echo-reply flood mode (see [`ICMP_FLOOD_UNSOLICITED`]).
/// Applied by `CSINode::run` from node configuration on every run so the
/// process-wide flag never leaks across differently-configured runs.
pub(crate) fn set_icmp_flood_unsolicited(enabled: bool) {
    ICMP_FLOOD_UNSOLICITED.store(enabled, Ordering::Relaxed);
}

/// Build a raw IPv4/ICMP echo datagram into `out` and return the length.
/// `unsolicited_reply` selects echo reply (one-directional flood) over echo
/// request (bidirectional request/reply).
fn build_icmp_echo_ipv4(
    out: &mut [u8],
    src: Ipv4Address,
    dst: Ipv4Address,
    seq_no: u16,
    unsolicited_reply: bool,
) -> Option<usize> {
    if out.len() < 64 {
        return None;
    }
    let mut icmp_buffer = [0u8; 12];
    let mut icmp_packet = Icmpv4Packet::new_unchecked(&mut icmp_buffer[..]);
    let icmp_repr = if unsolicited_reply {
        Icmpv4Repr::EchoReply {
            ident: 0x22b,
            seq_no,
            data: &[0xDE, 0xAD, 0xBE, 0xEF],
        }
    } else {
        Icmpv4Repr::EchoRequest {
            ident: 0x22b,
            seq_no,
            data: &[0xDE, 0xAD, 0xBE, 0xEF],
        }
    };
    icmp_repr.emit(&mut icmp_packet, &ChecksumCapabilities::default());

    let ipv4_repr = Ipv4Repr {
        src_addr: src,
        dst_addr: dst,
        payload_len: icmp_repr.buffer_len(),
        hop_limit: 64,
        next_header: IpProtocol::Icmp,
    };
    let len = ipv4_repr.buffer_len() + icmp_repr.buffer_len();
    let mut ipv4_packet = Ipv4Packet::new_unchecked(out);
    ipv4_repr.emit(&mut ipv4_packet, &ChecksumCapabilities::default());
    ipv4_packet
        .payload_mut()
        .copy_from_slice(icmp_packet.into_inner());
    Some(len)
}

/// How often the flood loop logs its enqueue/backpressure rate report.
const ICMP_FLOOD_REPORT_INTERVAL_US: u64 = 1_000_000;

/// High-rate ICMP echo flood over a raw socket.
///
/// Uses a deep TX queue and catch-up bursts so offered traffic is not capped at
/// ~30 Hz by awaiting one in-flight datagram per timer tick. On the CSI
/// collector, uplink 802.11 ACKs (and ICMP echo replies) become CSI reports.
///
/// Catch-up debt is clamped to one burst's worth of ticks: a stall (radio
/// busy, buffer full, heap pressure) is absorbed, not repaid at max rate —
/// unbounded repayment previously drove a saturate/starve oscillation that
/// showed up as bursty CSI with ~500 ms gaps on the collector.
///
/// With [`ICMP_FLOOD_UNSOLICITED`] set the flood sends echo *replies* instead
/// of requests — the peer never answers at the IP level, making the traffic
/// strictly one-directional (see that static's doc for the trade-off).
///
/// Neither `embassy-net`'s raw socket nor `esp-radio`'s WiFi event set expose a
/// per-packet TX result: `RawSocket::poll_send` only ever returns `Ready` (send
/// buffer accepted the datagram) or `Pending` (send buffer full) — there is no
/// error variant for a frame that reached the radio and then failed over the
/// air (no ACK, retry limit, CCA busy, etc). `esp-radio`'s only TX-completion
/// event, `WifiEvent::ActionTransmissionStatus`, covers 802.11 Action frames,
/// not the data frames this flood sends. So instead of a per-failure cause,
/// this logs a periodic enqueue/backpressure rate: once the send buffer is
/// kept full (`blocked` dominates `enqueued`), the `enqueued`-per-second figure
/// is the true bottleneck rate, since the radio can only be pulling frames out
/// of the buffer that slowly.
pub(crate) async fn run_icmp_flood(
    stack: Stack<'_>,
    mut src: Ipv4Address,
    mut dst: Ipv4Address,
    frequency_hz: Option<u16>,
    label: &'static str,
    watch_dhcp: bool,
) {
    let mut rx_meta = [PacketMetadata::EMPTY; ICMP_FLOOD_RX_SLOTS];
    let mut rx_buffer = [0u8; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; ICMP_FLOOD_TX_SLOTS];
    let mut tx_buffer = [0u8; 128 * ICMP_FLOOD_TX_SLOTS];

    let raw_socket = RawSocket::new::<Interface<'_>>(
        stack,
        IpVersion::Ipv4,
        IpProtocol::Icmp,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );

    let target_hz = frequency_hz.map(u64::from).unwrap_or(100).max(1);
    let tx_interval_us = 1_000_000 / target_hz;
    let mut next_tx_us = Instant::now().as_micros().saturating_add(tx_interval_us);
    let mut seq_counter: u16 = 0;
    let mut tx_ipv4_buffer = [0u8; 64];

    let mut enqueued: u32 = 0;
    let mut blocked: u32 = 0;
    let mut last_report_us = Instant::now().as_micros();

    let dst_oct = dst.octets();
    log_ln!(
        "{}: ICMP flood {} Hz → {}.{}.{}.{} (deep TX queue, burst={})",
        label,
        target_hz,
        dst_oct[0],
        dst_oct[1],
        dst_oct[2],
        dst_oct[3],
        ICMP_FLOOD_CATCH_UP_BURST,
    );

    loop {
        let mut now_us = Instant::now().as_micros();
        // Bound catch-up debt: after any stall (radio busy, TX buffer full,
        // heap pressure) don't repay unbounded virtual ticks at max rate —
        // that turns a one-off stall into a saturate/starve oscillation. Cap
        // the backlog at one burst's worth so the flood re-converges to the
        // target rate within a single wake instead of thousands of ticks.
        let max_debt_us = tx_interval_us.saturating_mul(u64::from(ICMP_FLOOD_CATCH_UP_BURST));
        let debt_floor = now_us.saturating_sub(max_debt_us);
        if next_tx_us < debt_floor {
            next_tx_us = debt_floor;
        }
        let unsolicited = ICMP_FLOOD_UNSOLICITED.load(Ordering::Relaxed);
        let mut burst_budget = ICMP_FLOOD_CATCH_UP_BURST;
        while now_us >= next_tx_us && burst_budget > 0 {
            burst_budget = burst_budget.saturating_sub(1);
            seq_counter = seq_counter.wrapping_add(1);
            let Some(len) =
                build_icmp_echo_ipv4(&mut tx_ipv4_buffer, src, dst, seq_counter, unsolicited)
            else {
                break;
            };
            let buf = &tx_ipv4_buffer[..len];
            let queued = poll_fn(|cx| match raw_socket.poll_send(buf, cx) {
                Poll::Ready(()) => Poll::Ready(true),
                Poll::Pending => Poll::Ready(false),
            })
            .await;
            if queued {
                enqueued = enqueued.saturating_add(1);
                next_tx_us = next_tx_us.saturating_add(tx_interval_us);
                now_us = Instant::now().as_micros();
            } else {
                blocked = blocked.saturating_add(1);
                break;
            }
        }

        let since_report_us = now_us.saturating_sub(last_report_us);
        if since_report_us >= ICMP_FLOOD_REPORT_INTERVAL_US {
            let actual_hz = (u64::from(enqueued) * 1_000_000 / since_report_us.max(1)) as u32;
            log_ln!(
                "{}: TX buffer {} enqueued/s (target {} Hz), {} blocked-on-full-buffer/s — \
                 no per-packet TX error is available (see run_icmp_flood doc); a low \
                 enqueued/s here with high blocked/s means the radio is draining the \
                 buffer that slowly, not that sends are being rejected locally",
                label,
                actual_hz,
                target_hz,
                blocked,
            );
            enqueued = 0;
            blocked = 0;
            last_report_us = now_us;
        }

        let until_tx = next_tx_us.saturating_sub(Instant::now().as_micros());
        let wait_us = if burst_budget < ICMP_FLOOD_CATCH_UP_BURST {
            until_tx.min(tx_interval_us / 4).max(1)
        } else {
            ICMP_FLOOD_QUEUE_BACKOFF_US
        };

        if watch_dhcp {
            match select3(
                STOP_SIGNAL.wait(),
                Timer::after_micros(wait_us),
                DHCP_CLIENT_INFO.wait(),
            )
            .await
            {
                Either3::First(_) => {
                    STOP_SIGNAL.signal(());
                    return;
                }
                Either3::Second(_) => {}
                Either3::Third(new_ip) => {
                    src = new_ip.local_address.address();
                    dst = new_ip.gateway_address;
                    let dst_oct = dst.octets();
                    log_ln!(
                        "{}: updated ICMP target gateway {}.{}.{}.{}",
                        label,
                        dst_oct[0],
                        dst_oct[1],
                        dst_oct[2],
                        dst_oct[3],
                    );
                }
            }
        } else {
            match select(STOP_SIGNAL.wait(), Timer::after_micros(wait_us)).await {
                Either::First(_) => {
                    STOP_SIGNAL.signal(());
                    return;
                }
                Either::Second(_) => {}
            }
        }
    }
}

/// ICMP flood round-robin across multiple destination addresses.
///
/// When `refresh_targets` is set, the destination list is reloaded every
/// second so newly DHCP'd stations start receiving traffic without restarting
/// the flood task.
pub(crate) async fn run_icmp_flood_multi(
    stack: Stack<'_>,
    src: Ipv4Address,
    mut dsts: heapless::Vec<Ipv4Address, 8>,
    frequency_hz: Option<u16>,
    label: &'static str,
    refresh_targets: Option<fn() -> heapless::Vec<Ipv4Address, 8>>,
) {
    if dsts.is_empty() {
        return;
    }
    // Keep the refreshable loop even for a single initial lease — the first
    // station to DHCP often starts the flood alone; additional stations must
    // be picked up via `refresh_targets` without restarting this task.
    if dsts.len() == 1 && refresh_targets.is_none() {
        run_icmp_flood(stack, src, dsts[0], frequency_hz, label, false).await;
        return;
    }

    let mut rx_meta = [PacketMetadata::EMPTY; ICMP_FLOOD_RX_SLOTS];
    let mut rx_buffer = [0u8; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; ICMP_FLOOD_TX_SLOTS];
    let mut tx_buffer = [0u8; 128 * ICMP_FLOOD_TX_SLOTS];

    let raw_socket = RawSocket::new::<Interface<'_>>(
        stack,
        IpVersion::Ipv4,
        IpProtocol::Icmp,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );

    let target_hz = frequency_hz.map(u64::from).unwrap_or(100).max(1);
    let tx_interval_us = 1_000_000 / target_hz;
    let mut next_tx_us = Instant::now().as_micros().saturating_add(tx_interval_us);
    let mut seq_counter: u16 = 0;
    let mut dst_idx = 0usize;
    let mut tx_ipv4_buffer = [0u8; 64];
    let mut enqueued: u32 = 0;
    let mut blocked: u32 = 0;
    let mut last_report_us = Instant::now().as_micros();

    log_ln!(
        "{}: ICMP flood {} Hz → {} lease(s) (round-robin)",
        label,
        target_hz,
        dsts.len(),
    );

    loop {
        let dst = dsts[dst_idx % dsts.len()];
        let mut now_us = Instant::now().as_micros();
        let max_debt_us = tx_interval_us.saturating_mul(u64::from(ICMP_FLOOD_CATCH_UP_BURST));
        let debt_floor = now_us.saturating_sub(max_debt_us);
        if next_tx_us < debt_floor {
            next_tx_us = debt_floor;
        }
        let unsolicited = ICMP_FLOOD_UNSOLICITED.load(Ordering::Relaxed);
        let mut burst_budget = ICMP_FLOOD_CATCH_UP_BURST;
        while now_us >= next_tx_us && burst_budget > 0 {
            burst_budget = burst_budget.saturating_sub(1);
            seq_counter = seq_counter.wrapping_add(1);
            let Some(len) =
                build_icmp_echo_ipv4(&mut tx_ipv4_buffer, src, dst, seq_counter, unsolicited)
            else {
                break;
            };
            let buf = &tx_ipv4_buffer[..len];
            let queued = poll_fn(|cx| match raw_socket.poll_send(buf, cx) {
                Poll::Ready(()) => Poll::Ready(true),
                Poll::Pending => Poll::Ready(false),
            })
            .await;
            if queued {
                enqueued = enqueued.saturating_add(1);
                dst_idx = dst_idx.wrapping_add(1);
                next_tx_us = next_tx_us.saturating_add(tx_interval_us);
                now_us = Instant::now().as_micros();
            } else {
                blocked = blocked.saturating_add(1);
                break;
            }
        }

        let since_report_us = now_us.saturating_sub(last_report_us);
        if since_report_us >= ICMP_FLOOD_REPORT_INTERVAL_US {
            if let Some(refresh) = refresh_targets {
                let updated = refresh();
                let changed = updated.len() != dsts.len()
                    || updated
                        .iter()
                        .zip(dsts.iter())
                        .any(|(a, b)| a != b);
                if !updated.is_empty() && changed {
                    dsts = updated;
                    dst_idx = 0;
                    log_ln!(
                        "{}: ping targets refreshed ({} active lease(s))",
                        label,
                        dsts.len(),
                    );
                }
            }
            let actual_hz = (u64::from(enqueued) * 1_000_000 / since_report_us.max(1)) as u32;
            log_ln!(
                "{}: TX {} enqueued/s (target {} Hz, {} dst), {} blocked/s",
                label,
                actual_hz,
                target_hz,
                dsts.len(),
                blocked,
            );
            enqueued = 0;
            blocked = 0;
            last_report_us = now_us;
        }

        let until_tx = next_tx_us.saturating_sub(Instant::now().as_micros());
        let wait_us = if burst_budget < ICMP_FLOOD_CATCH_UP_BURST {
            until_tx.min(tx_interval_us / 4).max(1)
        } else {
            ICMP_FLOOD_QUEUE_BACKOFF_US
        };

        match select(STOP_SIGNAL.wait(), Timer::after_micros(wait_us)).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                return;
            }
            Either::Second(_) => {}
        }
    }
}

/// Synchronized-burst ICMP flood: every tick fires one frame back-to-back to
/// **all** active destinations, so every associated station receives its
/// downlink PPDU within tens of microseconds of the others. Contrast with
/// [`run_icmp_flood_multi`], which advances one destination per tick and spreads
/// receivers across the whole interval.
///
/// This is the workable substitute for a single group-addressed broadcast frame.
/// An ESP32 softAP buffers broadcast/multicast frames for post-DTIM delivery and
/// drops them under a high-rate flood (and only ever sends them at the legacy
/// basic rate), so a broadcast frame mostly never leaves the radio. Only unicast
/// honours the configured TX rate and transmits immediately. Sending N unicast
/// frames per tick keeps near-simultaneous arrival across receivers.
///
/// Per-receiver rate is `frequency_hz`; total offered rate is
/// `frequency_hz * dsts.len()`, so lower the rate if airtime saturates. Honours
/// [`ICMP_FLOOD_UNSOLICITED`] like the other flood loops — with it set the frames
/// are unsolicited echo replies (strictly downlink; stations never answer), which
/// is what the receiving stations turn into CSI.
pub(crate) async fn run_icmp_flood_burst(
    stack: Stack<'_>,
    src: Ipv4Address,
    mut dsts: heapless::Vec<Ipv4Address, 8>,
    frequency_hz: Option<u16>,
    label: &'static str,
    refresh_targets: Option<fn() -> heapless::Vec<Ipv4Address, 8>>,
) {
    if dsts.is_empty() {
        return;
    }

    let mut rx_meta = [PacketMetadata::EMPTY; ICMP_FLOOD_RX_SLOTS];
    let mut rx_buffer = [0u8; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; ICMP_FLOOD_TX_SLOTS];
    let mut tx_buffer = [0u8; 128 * ICMP_FLOOD_TX_SLOTS];

    let raw_socket = RawSocket::new::<Interface<'_>>(
        stack,
        IpVersion::Ipv4,
        IpProtocol::Icmp,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );

    let target_hz = frequency_hz.map(u64::from).unwrap_or(100).max(1);
    let tx_interval_us = 1_000_000 / target_hz;
    let mut next_tx_us = Instant::now().as_micros().saturating_add(tx_interval_us);
    let mut seq_counter: u16 = 0;
    let mut tx_ipv4_buffer = [0u8; 64];
    let mut enqueued: u32 = 0;
    let mut blocked: u32 = 0;
    let mut bursts: u32 = 0;
    let mut last_report_us = Instant::now().as_micros();
    // Rotate the burst's start index whenever the TX buffer fills mid-burst so a
    // chronically-full buffer doesn't always starve the same tail receivers.
    let mut burst_start = 0usize;

    log_ln!(
        "{}: ICMP burst flood {} Hz/receiver → {} lease(s) (all per tick)",
        label,
        target_hz,
        dsts.len(),
    );

    loop {
        let mut now_us = Instant::now().as_micros();
        // Bound catch-up debt exactly as run_icmp_flood does: after a stall don't
        // repay accumulated ticks at max rate (that oscillates saturate/starve).
        let max_debt_us = tx_interval_us.saturating_mul(u64::from(ICMP_FLOOD_CATCH_UP_BURST));
        let debt_floor = now_us.saturating_sub(max_debt_us);
        if next_tx_us < debt_floor {
            next_tx_us = debt_floor;
        }
        let unsolicited = ICMP_FLOOD_UNSOLICITED.load(Ordering::Relaxed);

        let mut blocked_burst = false;
        if now_us >= next_tx_us {
            let n = dsts.len();
            let mut sent_any = false;
            for i in 0..n {
                let dst = dsts[(burst_start + i) % n];
                seq_counter = seq_counter.wrapping_add(1);
                let Some(len) =
                    build_icmp_echo_ipv4(&mut tx_ipv4_buffer, src, dst, seq_counter, unsolicited)
                else {
                    break;
                };
                let buf = &tx_ipv4_buffer[..len];
                let queued = poll_fn(|cx| match raw_socket.poll_send(buf, cx) {
                    Poll::Ready(()) => Poll::Ready(true),
                    Poll::Pending => Poll::Ready(false),
                })
                .await;
                if queued {
                    enqueued = enqueued.saturating_add(1);
                    sent_any = true;
                } else {
                    // TX buffer full mid-burst: resume the next burst at the first
                    // receiver that missed out so coverage stays fair, then back off.
                    blocked = blocked.saturating_add(1);
                    blocked_burst = true;
                    burst_start = (burst_start + i) % n;
                    break;
                }
            }
            if sent_any {
                bursts = bursts.saturating_add(1);
                if !blocked_burst {
                    burst_start = 0;
                }
            }
            next_tx_us = next_tx_us.saturating_add(tx_interval_us);
            now_us = Instant::now().as_micros();
        }

        let since_report_us = now_us.saturating_sub(last_report_us);
        if since_report_us >= ICMP_FLOOD_REPORT_INTERVAL_US {
            if let Some(refresh) = refresh_targets {
                let updated = refresh();
                let changed = updated.len() != dsts.len()
                    || updated.iter().zip(dsts.iter()).any(|(a, b)| a != b);
                if !updated.is_empty() && changed {
                    dsts = updated;
                    burst_start = 0;
                    log_ln!(
                        "{}: burst targets refreshed ({} active lease(s))",
                        label,
                        dsts.len(),
                    );
                }
            }
            let bursts_hz = (u64::from(bursts) * 1_000_000 / since_report_us.max(1)) as u32;
            let frames_hz = (u64::from(enqueued) * 1_000_000 / since_report_us.max(1)) as u32;
            log_ln!(
                "{}: {} bursts/s ({} frames/s, target {} Hz × {} dst), {} blocked/s",
                label,
                bursts_hz,
                frames_hz,
                target_hz,
                dsts.len(),
                blocked,
            );
            enqueued = 0;
            blocked = 0;
            bursts = 0;
            last_report_us = now_us;
        }

        let until_tx = next_tx_us.saturating_sub(Instant::now().as_micros());
        let wait_us = if blocked_burst {
            ICMP_FLOOD_QUEUE_BACKOFF_US
        } else {
            until_tx.min(tx_interval_us).max(1)
        };
        match select(STOP_SIGNAL.wait(), Timer::after_micros(wait_us)).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                return;
            }
            Either::Second(_) => {}
        }
    }
}

/// Manage station network operations and emit periodic ICMP traffic.
pub async fn sta_network_ops(sta_stack: Stack<'_>, frequency_hz: Option<u16>) {
    // Retrieve acquired IP information from DHCP. Guard against `STOP_SIGNAL`
    // — if stop fires before DHCP completes (e.g. AP never associated), this
    // unguarded wait would hang the join in `run_sta_connect`.
    let ip_info = match select(STOP_SIGNAL.wait(), DHCP_CLIENT_INFO.wait()).await {
        Either::First(_) => {
            STOP_SIGNAL.signal(());
            return;
        }
        Either::Second(info) => info,
    };

    run_icmp_flood(
        sta_stack,
        ip_info.local_address.address(),
        ip_info.gateway_address,
        frequency_hz,
        "STA",
        true,
    )
    .await;
}
