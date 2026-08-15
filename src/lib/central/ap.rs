//! Self-contained softAP CSI collector.
//!
//! Starts a Wi-Fi access point and brings up an embassy-net stack on the AP
//! interface with a **static** IPv4 address. A built-in **multi-lease DHCP
//! server** (hand-rolled over an embassy-net UDP socket using smoltcp's DHCP
//! wire codec — no extra dependency) hands associating stations distinct
//! addresses whose gateway is the AP itself.

use embassy_futures::join::{join, join3};
use embassy_futures::select::{Either, select};
use embassy_net::{Ipv4Address, Runner, Stack, StaticConfigV4};
use embassy_time::{Duration, Timer};
use esp_radio::wifi::csi::CsiConfig;
use esp_radio::wifi::{
    AccessPointStationEventInfo, Config, Interface, WifiController,
};
use embassy_net::udp::{PacketMetadata as UdpPacketMetadata, UdpSocket};
use smoltcp::wire::{DhcpMessageType, DhcpPacket, DhcpRepr};

use crate::central::sta::{
    StackResourcesSlot, run_icmp_flood, run_icmp_flood_burst, run_icmp_flood_multi, run_net_task,
};
use crate::espnow_phy::with_espnow_recv_suspended;
use crate::profile::RadioProfile;
use crate::{IOTaskConfig, STOP_SIGNAL, WifiApConfig, log_ln, set_csi};
use core::net::Ipv4Addr;
use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};

/// Reusable storage for the AP stack's `StackResources` (separate instance from
/// the STA stack so a stop/restart cycle doesn't trip `StaticCell::uninit`).
static AP_STACK_RESOURCES: StackResourcesSlot = StackResourcesSlot::new();

const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_LEASE_SECS: u32 = 3600;
const MAX_DHCP_CLIENTS: usize = 8;

#[derive(Clone, Copy)]
struct DhcpBinding {
    mac: [u8; 6],
    ip: Ipv4Addr,
}

struct ActiveLeaseRegistry {
    ips: [Ipv4Addr; MAX_DHCP_CLIENTS],
    count: u8,
}

impl ActiveLeaseRegistry {
    const fn new() -> Self {
        Self {
            ips: [Ipv4Addr::UNSPECIFIED; MAX_DHCP_CLIENTS],
            count: 0,
        }
    }

    fn register(&mut self, ip: Ipv4Addr) {
        for slot in self.ips.iter().take(self.count as usize) {
            if *slot == ip {
                return;
            }
        }
        if (self.count as usize) < MAX_DHCP_CLIENTS {
            self.ips[self.count as usize] = ip;
            self.count += 1;
        }
    }

    fn snapshot(&self) -> heapless::Vec<Ipv4Addr, MAX_DHCP_CLIENTS> {
        let mut v = heapless::Vec::new();
        for ip in self.ips.iter().take(self.count as usize) {
            let _ = v.push(*ip);
        }
        v
    }
}

static ACTIVE_LEASES: Mutex<CriticalSectionRawMutex, ActiveLeaseRegistry> =
    Mutex::new(ActiveLeaseRegistry::new());

fn snapshot_active_ping_targets() -> heapless::Vec<Ipv4Address, MAX_DHCP_CLIENTS> {
    let mut addrs = heapless::Vec::new();
    for ip in ACTIVE_LEASES.lock(|r| r.snapshot()) {
        let _ = addrs.push(Ipv4Address::from(ip.octets()));
    }
    addrs
}

fn active_leases_pending() -> bool {
    !ACTIVE_LEASES.lock(|r| r.snapshot()).is_empty()
}

fn register_active_lease(ip: Ipv4Addr) {
    // SAFETY: single executor, no re-entrant lock on ACTIVE_LEASES.
    unsafe {
        ACTIVE_LEASES.lock_mut(|r| r.register(ip));
    }
    let oct = ip.octets();
    log_ln!(
        "DHCP: active lease registered {}.{}.{}.{}",
        oct[0],
        oct[1],
        oct[2],
        oct[3],
    );
}

fn dhcp_assign_ip(
    clients: &mut [Option<DhcpBinding>; MAX_DHCP_CLIENTS],
    mac: [u8; 6],
    config: &WifiApConfig,
) -> Option<Ipv4Addr> {
    for slot in clients.iter() {
        if let Some(c) = slot {
            if c.mac == mac {
                return Some(c.ip);
            }
        }
    }
    for i in 0..config.lease_count.min(MAX_DHCP_CLIENTS as u8) {
        let candidate = config.lease_ip_at(i);
        let taken = clients.iter().any(|s| s.as_ref().is_some_and(|c| c.ip == candidate));
        if !taken {
            for slot in clients.iter_mut() {
                if slot.is_none() {
                    *slot = Some(DhcpBinding { mac, ip: candidate });
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Initialize the AP interface: build a static-IP embassy-net stack and apply
/// the access-point configuration to the controller (which restarts the radio).
pub fn ap_init<'a>(
    interface: &'a mut Interface<'static>,
    config: &WifiApConfig,
    controller: &mut WifiController<'static>,
    profile: &dyn RadioProfile,
    bringup: bool,
) -> (Stack<'a>, Runner<'a, &'a mut Interface<'static>>) {
    let ip_config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: embassy_net::Ipv4Cidr::new(config.ap_ipv4, 24),
        gateway: Some(config.ap_ipv4),
        dns_servers: Default::default(),
    });
    let seed = 654_321_u64;

    let (ap_stack, ap_runner) =
        embassy_net::new(interface, ip_config, AP_STACK_RESOURCES.get_or_init(), seed);

    let ap_cfg = Config::AccessPoint(config.ap_config.clone());
    with_espnow_recv_suspended(|| {
        // Profile hook fires inside the suspend closure, immediately before
        // `set_config`, preserving the exact ordering a forced-TX PHY needs.
        if bringup {
            profile.before_ap_config();
        }
        match controller.set_config(&ap_cfg) {
            Ok(_) => log_ln!("AP Configuration Set"),
            Err(_) => log_ln!("AP Configuration Error"),
        }
    });

    (ap_stack, ap_runner)
}

/// Start the AP, register CSI, and run the net stack + (optional) DHCP server
/// + (optional) ICMP trigger traffic until a stop signal. CSI from associated
/// stations' uplink frames is captured by `capture_csi_info` independently of
/// this task.
pub async fn run_ap(
    controller: &mut WifiController<'_>,
    ap_stack: Stack<'_>,
    ap_runner: Runner<'_, &mut Interface<'_>>,
    config: &WifiApConfig,
    csi_config: CsiConfig,
    io_tasks: IOTaskConfig,
    frequency_hz: Option<u16>,
) {
    // Let the AP-start radio restart settle before re-arming CSI.
    match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_millis(500))).await {
        Either::First(_) => {
            STOP_SIGNAL.signal(());
            return;
        }
        Either::Second(_) => {}
    }

    // CSI must be registered AFTER the AP-start radio restart (which clears the
    // CSI filter) — this is why the shared run_inner set_csi block skips the AP.
    if io_tasks.rx_enabled {
        with_espnow_recv_suspended(|| {
            set_csi(controller, csi_config.clone());
        });
    }
    log_ln!(
        "AP started on channel {} — collecting CSI from associated stations",
        config.channel
    );

    if config.serve_dhcp {
        if io_tasks.tx_enabled {
            join3(
                run_net_task(ap_runner),
                run_dhcp_server(ap_stack, config),
                join(
                    ap_station_monitor(controller, csi_config, io_tasks),
                    ap_ping_lease(ap_stack, config, frequency_hz),
                ),
            )
            .await;
        } else {
            join3(
                run_net_task(ap_runner),
                run_dhcp_server(ap_stack, config),
                ap_station_monitor(controller, csi_config, io_tasks),
            )
            .await;
        }
    } else if io_tasks.tx_enabled {
        join3(
            run_net_task(ap_runner),
            ap_station_monitor(controller, csi_config, io_tasks),
            ap_ping_lease(ap_stack, config, frequency_hz),
        )
        .await;
    } else {
        join(
            run_net_task(ap_runner),
            ap_station_monitor(controller, csi_config, io_tasks),
        )
        .await;
    }
}

/// Monitor station connect/disconnect and re-arm CSI after association.
async fn ap_station_monitor(
    controller: &mut WifiController<'_>,
    csi_config: CsiConfig,
    io_tasks: IOTaskConfig,
) {
    loop {
        match select(
            STOP_SIGNAL.wait(),
            controller.wait_for_access_point_connected_event_async(),
        )
        .await
        {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                return;
            }
            Either::Second(Ok(AccessPointStationEventInfo::Connected(info))) => {
                log_ln!(
                    "AP: station {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} connected",
                    info.mac[0],
                    info.mac[1],
                    info.mac[2],
                    info.mac[3],
                    info.mac[4],
                    info.mac[5],
                );
                if io_tasks.rx_enabled {
                    with_espnow_recv_suspended(|| {
                        set_csi(controller, csi_config.clone());
                    });
                }
            }
            Either::Second(Ok(AccessPointStationEventInfo::Disconnected(info))) => {
                log_ln!(
                    "AP: station {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} disconnected",
                    info.mac[0],
                    info.mac[1],
                    info.mac[2],
                    info.mac[3],
                    info.mac[4],
                    info.mac[5],
                );
            }
            Either::Second(Err(_)) => {}
        }
    }
}

/// ICMP flood to leased station(s) — each echo reply is uplink CSI on the AP.
async fn ap_ping_lease(ap_stack: Stack<'_>, config: &WifiApConfig, frequency_hz: Option<u16>) {
    match select(STOP_SIGNAL.wait(), ap_stack.wait_link_up()).await {
        Either::First(_) => {
            STOP_SIGNAL.signal(());
            return;
        }
        Either::Second(_) => {}
    }

    match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_millis(500))).await {
        Either::First(_) => {
            STOP_SIGNAL.signal(());
            return;
        }
        Either::Second(_) => {}
    }

    let src = Ipv4Address::from(config.ap_ipv4.octets());
    let hz = frequency_hz.or(Some(1000));

    // Single lease, round-robin mode: nothing to synchronize across, so use the
    // simple single-destination flood. Sync-burst always takes the lease-gather
    // path below (even for one lease) so it picks up more receivers as they join.
    if !config.sync_burst && config.lease_count <= 1 {
        run_icmp_flood(
            ap_stack,
            src,
            Ipv4Address::from(config.lease_ipv4.octets()),
            hz,
            "AP",
            false,
        )
        .await;
        return;
    }

    // Wait until at least one station completes DHCP so smoltcp can ARP-resolve
    // the target MAC before the flood starts (avoids blind pool pings to empty
    // addresses eating airtime).
    const LEASE_WAIT_ATTEMPTS: u32 = 120;
    for _ in 0..LEASE_WAIT_ATTEMPTS {
        if active_leases_pending() {
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

    let mut addrs = snapshot_active_ping_targets();
    if addrs.is_empty() {
        log_ln!("AP: no DHCP leases yet — falling back to configured pool");
        for ip in config.lease_pool() {
            let _ = addrs.push(Ipv4Address::from(ip.octets()));
        }
    }
    if addrs.is_empty() {
        return;
    }

    // Sync-burst: fire one unicast frame back-to-back to every lease per tick so
    // all associated stations receive their downlink within microseconds of
    // each other (broadcast can't do this — softAP multicast is DTIM-buffered).
    // Round-robin otherwise: one lease per tick.
    if config.sync_burst {
        run_icmp_flood_burst(
            ap_stack,
            src,
            addrs,
            hz,
            "AP-BURST",
            Some(snapshot_active_ping_targets),
        )
        .await;
    } else {
        run_icmp_flood_multi(
            ap_stack,
            src,
            addrs,
            hz,
            "AP",
            Some(snapshot_active_ping_targets),
        )
        .await;
    }
}

/// Minimal multi-lease DHCP server over an embassy-net UDP socket.
async fn run_dhcp_server(stack: Stack<'_>, config: &WifiApConfig) {
    match select(STOP_SIGNAL.wait(), stack.wait_link_up()).await {
        Either::First(_) => {
            STOP_SIGNAL.signal(());
            return;
        }
        Either::Second(_) => {}
    }

    let mut rx_meta = [UdpPacketMetadata::EMPTY; 4];
    let mut rx_buffer = [0u8; 1024];
    let mut tx_meta = [UdpPacketMetadata::EMPTY; 4];
    let mut tx_buffer = [0u8; 1024];
    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    if socket.bind(DHCP_SERVER_PORT).is_err() {
        log_ln!("DHCP server: bind to :67 failed");
        return;
    }
    log_ln!(
        "DHCP server listening on :67 (pool {} host(s) from {}.{}.{}.{})",
        config.lease_count,
        config.lease_ipv4.octets()[0],
        config.lease_ipv4.octets()[1],
        config.lease_ipv4.octets()[2],
        config.lease_ipv4.octets()[3],
    );

    let mut clients: [Option<DhcpBinding>; MAX_DHCP_CLIENTS] = [None; MAX_DHCP_CLIENTS];

    let mut in_buf = [0u8; 600];
    let mut out_buf = [0u8; 600];

    loop {
        let n = match select(STOP_SIGNAL.wait(), socket.recv_from(&mut in_buf)).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                return;
            }
            Either::Second(Ok((n, _meta))) => n,
            Either::Second(Err(_)) => continue,
        };

        // Parse the request (borrows `in_buf` for this iteration only).
        let packet = match DhcpPacket::new_checked(&in_buf[..n]) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let req = match DhcpRepr::parse(&packet) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let reply_type = match req.message_type {
            DhcpMessageType::Discover => DhcpMessageType::Offer,
            DhcpMessageType::Request => DhcpMessageType::Ack,
            _ => continue,
        };

        let mac = req.client_hardware_address.0;

        let Some(your_ip) = dhcp_assign_ip(&mut clients, mac, config) else {
            log_ln!("DHCP: pool full, ignoring client");
            continue;
        };
        if reply_type == DhcpMessageType::Offer || reply_type == DhcpMessageType::Ack {
            register_active_lease(your_ip);
        }

        let reply = DhcpRepr {
            message_type: reply_type,
            transaction_id: req.transaction_id,
            secs: 0,
            client_hardware_address: req.client_hardware_address,
            client_ip: core::net::Ipv4Addr::UNSPECIFIED,
            your_ip,
            server_ip: config.ap_ipv4,
            router: Some(config.ap_ipv4),
            subnet_mask: Some(core::net::Ipv4Addr::new(255, 255, 255, 0)),
            relay_agent_ip: core::net::Ipv4Addr::UNSPECIFIED,
            broadcast: true,
            requested_ip: None,
            client_identifier: None,
            server_identifier: Some(config.ap_ipv4),
            parameter_request_list: None,
            dns_servers: None,
            max_size: None,
            lease_duration: Some(DHCP_LEASE_SECS),
            renew_duration: None,
            rebind_duration: None,
            additional_options: &[],
        };

        let len = reply.buffer_len();
        if len > out_buf.len() {
            continue;
        }
        // Zero the option area so any stale bytes can't trail the emitted packet.
        for b in out_buf[..len].iter_mut() {
            *b = 0;
        }
        let mut reply_packet = DhcpPacket::new_unchecked(&mut out_buf[..len]);
        if reply.emit(&mut reply_packet).is_err() {
            continue;
        }

        // Broadcast the reply to the client port (client has no IP yet).
        let dst = (core::net::Ipv4Addr::BROADCAST, DHCP_CLIENT_PORT);
        let _ = socket.send_to(&out_buf[..len], dst).await;
    }
}
