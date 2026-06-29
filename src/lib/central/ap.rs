//! Self-contained softAP CSI collector.
//!
//! Starts a Wi-Fi access point and brings up an embassy-net stack on the AP
//! interface with a **static** IPv4 address. A built-in **single-lease DHCP
//! server** (hand-rolled over an embassy-net UDP socket using smoltcp's DHCP
//! wire codec — no extra dependency) hands the one associating station an
//! address whose gateway is the AP itself. The existing
//! [`WifiStation`](crate::CentralOpMode::WifiStation) mode can therefore
//! associate **unmodified**: it DHCP-discovers, gets a lease + gateway, and
//! pings the gateway (= this AP). The AP also pings the leased station so ICMP
//! echo **replies** (uplink data frames) drive CSI at the configured rate even
//! when the client is a phone or other device that does not flood on its own.

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

use crate::central::sta::{StackResourcesSlot, run_icmp_flood, run_net_task};
use crate::espnow_phy::with_espnow_recv_suspended;
use crate::{IOTaskConfig, STOP_SIGNAL, WifiApConfig, log_ln, set_csi};

/// Reusable storage for the AP stack's `StackResources` (separate instance from
/// the STA stack so a stop/restart cycle doesn't trip `StaticCell::uninit`).
static AP_STACK_RESOURCES: StackResourcesSlot = StackResourcesSlot::new();

const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_LEASE_SECS: u32 = 3600;

/// Initialize the AP interface: build a static-IP embassy-net stack and apply
/// the access-point configuration to the controller (which restarts the radio).
pub fn ap_init<'a>(
    interface: &'a mut Interface<'static>,
    config: &WifiApConfig,
    controller: &mut WifiController<'static>,
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
    with_espnow_recv_suspended(|| match controller.set_config(&ap_cfg) {
        Ok(_) => log_ln!("AP Configuration Set"),
        Err(_) => log_ln!("AP Configuration Error"),
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

/// ICMP flood to the fixed lease address — each echo reply / 802.11 ACK from
/// the station is an uplink frame captured as CSI on the AP.
async fn ap_ping_lease(ap_stack: Stack<'_>, config: &WifiApConfig, frequency_hz: Option<u16>) {
    match select(STOP_SIGNAL.wait(), ap_stack.wait_link_up()).await {
        Either::First(_) => {
            STOP_SIGNAL.signal(());
            return;
        }
        Either::Second(_) => {}
    }

    // Brief settle so the client's DHCP stack is ready to reply.
    match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_millis(500))).await {
        Either::First(_) => {
            STOP_SIGNAL.signal(());
            return;
        }
        Either::Second(_) => {}
    }

    run_icmp_flood(
        ap_stack,
        Ipv4Address::from(config.ap_ipv4.octets()),
        Ipv4Address::from(config.lease_ipv4.octets()),
        frequency_hz.or(Some(1000)),
        "AP",
        false,
    )
    .await;
}

/// Minimal single-lease DHCP server over an embassy-net UDP socket.
///
/// Answers `Discover` with `Offer` and `Request` with `Ack`, always offering the
/// same fixed lease (`config.lease_ipv4`) with the AP as router/server. Replies
/// are broadcast to the client port since the client has no IP yet. Every await
/// is `STOP_SIGNAL`-guarded so a stop request never hangs the join.
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
    log_ln!("DHCP server listening on :67");

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

        let reply = DhcpRepr {
            message_type: reply_type,
            transaction_id: req.transaction_id,
            secs: 0,
            client_hardware_address: req.client_hardware_address,
            client_ip: core::net::Ipv4Addr::UNSPECIFIED,
            your_ip: config.lease_ipv4,
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
