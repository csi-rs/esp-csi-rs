use core::{net::Ipv4Addr};
use embassy_futures::join::{join4};
use embassy_futures::select::{select, select3, Either, Either3};
use embassy_net::raw::{IpProtocol, IpVersion, PacketMetadata, RawSocket};
use embassy_net::{Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use enumset::enum_set;
use esp_radio::wifi::{ModeConfig, WifiController, WifiDevice, WifiEvent};
use smoltcp::phy::ChecksumCapabilities;

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};

use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, Ipv4Packet, Ipv4Repr};

use crate::log_ln;
use crate::{WifiStationConfig, STOP_SIGNAL};

static DHCP_CLIENT_INFO: Signal<CriticalSectionRawMutex, IpInfo> = Signal::new();

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

/// DHCP-acquired IP configuration for the STA interface.
#[derive(Debug, Clone)]
struct IpInfo {
    pub local_address: Ipv4Cidr,
    pub gateway_address: Ipv4Address,
}

/// Initialize the station interface and return the network stack and runner.
pub fn sta_init<'a>(
    interfaces: &'a mut WifiDevice<'static>,
    config: &WifiStationConfig,
    controller: &mut WifiController<'static>,
) -> (Stack<'a>, Runner<'a, &'a mut WifiDevice<'static>>) {
    let sta_ip_config = embassy_net::Config::dhcpv4(Default::default());
    let seed = 123456_u64;

    // Create STA Network Stack and Runner
    let (sta_stack, sta_runner) = embassy_net::new(
        interfaces,
        sta_ip_config,
        mk_static!(StackResources<6>, StackResources::<6>::new()),
        seed,
    );

    // Configure WiFi Client/Station Connection
    let station_config = ModeConfig::Client(config.client_config.clone());
    // Set the Configuration
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
    sta_runner: Runner<'_, &mut WifiDevice<'_>>
) {
    // Connect WiFi (retry on transient failures)
    loop {
        match select(STOP_SIGNAL.wait(), controller.connect_async()).await {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                return;
            }
            Either::Second(Ok(_)) => {
                log_ln!("WiFi Connected");
                break;
            }
            Either::Second(Err(e)) => {
                log_ln!("Failed to connect WiFi: {:?}. Retrying...", e);
                match select(STOP_SIGNAL.wait(), Timer::after(Duration::from_secs(1))).await {
                    Either::First(_) => {
                        STOP_SIGNAL.signal(());
                        return;
                    }
                    Either::Second(_) => {}
                }
            }
        }
    }

    join4(
        sta_connection(controller),
        sta_network_ops(sta_stack, freq),
        run_net_task(sta_runner),
        run_dhcp_client(sta_stack)
    )
    .await;
}

/// Run the embassy-net runner until a stop signal is received.
async fn run_net_task(mut sta_runner: Runner<'_, &mut WifiDevice<'_>>) {
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
async fn run_dhcp_client(sta_stack: Stack<'_>) {
    log_ln!("Running DHCP Client");

    loop {
        // Check if link is up
        sta_stack.wait_link_up().await;
        log_ln!("Link is up!");

        // Create instance to store acquired IP information
        let mut ip_info = IpInfo {
            local_address: Ipv4Cidr::new(Ipv4Addr::UNSPECIFIED, 24),
            gateway_address: Ipv4Address::UNSPECIFIED,
        };

        log_ln!("Acquiring config...");
        sta_stack.wait_config_up().await;
        log_ln!("Config Acquired");

        // Print out acquired IP configuration
        loop {
            if let Some(config) = sta_stack.config_v4() {
                ip_info.local_address = config.address;
                ip_info.gateway_address = config.gateway.unwrap_or(Ipv4Address::UNSPECIFIED);

                log_ln!("Local IP: {:?}", ip_info.local_address);
                log_ln!("Gateway IP: {:?}", ip_info.gateway_address);

                break;
            }
            Timer::after(Duration::from_millis(500)).await;
        }

        // Publish DHCP info. On reconnect this updates consumers.
        DHCP_CLIENT_INFO.signal(ip_info);

        // Wait until link drops before looping for next lease/config.
        while sta_stack.is_link_up() {
            Timer::after(Duration::from_millis(250)).await;
        }
        log_ln!("Link down, waiting to reacquire DHCP config...");
    }
}

/// Monitor STA events (connect/disconnect/stop) until a stop signal.
pub async fn sta_connection(controller: &mut WifiController<'_>) {
    // let mut start_collection_watch = match START_COLLECTION.receiver() {
    //     Some(r) => r,
    //     None => panic!("Maximum number of recievers reached"),
    // };

    // Define Events to Listen for
    let sta_events =
        enum_set!(WifiEvent::StaDisconnected | WifiEvent::StaStop | WifiEvent::StaConnected);

    // Monitoring/stop loop
    loop {
        // // Stop Collection Future
        // let stop_coll_fut = start_collection_watch.changed();
        // // Events Future
        // let mut wait_event_fut = controller.wait_for_events(sta_events, true);
        match select(
            STOP_SIGNAL.wait(),
            controller.wait_for_events(sta_events, true),
        )
        .await
        {
            Either::First(_) => {
                STOP_SIGNAL.signal(());
                break;
            }
            Either::Second(mut wait_event_fut) => {
                if wait_event_fut.contains(WifiEvent::StaDisconnected) {
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
                                break;
                            }
                            Either::Second(Err(e)) => {
                                log_ln!("STA reconnect failed: {:?}", e);
                                match select(
                                    STOP_SIGNAL.wait(),
                                    Timer::after(Duration::from_secs(1)),
                                )
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
                if wait_event_fut.contains(WifiEvent::StaStop) {
                    log_ln!("STA Stopped");
                }
                wait_event_fut.clear();
            }
        }
    }
}

/// Manage station network operations and emit periodic ICMP traffic.
pub async fn sta_network_ops(sta_stack: Stack<'_>, frequency_hz: Option<u16>) {
    // Retrieve acquired IP information from DHCP
    let mut ip_info = DHCP_CLIENT_INFO.wait().await;

    // let mut start_collection_watch = match START_COLLECTION.receiver() {
    //     Some(r) => r,
    //     None => panic!("Maximum number of recievers reached"),
    // };

    // ------------------ ICMP Socket Setup ------------------
    let mut rx_buffer = [0; 64];
    let mut tx_buffer = [0; 64];
    let mut rx_meta: [PacketMetadata; 1] = [PacketMetadata::EMPTY; 1];
    let mut tx_meta: [PacketMetadata; 1] = [PacketMetadata::EMPTY; 1];

    let raw_socket = RawSocket::new::<WifiDevice<'_>>(
        sta_stack,
        IpVersion::Ipv4,
        IpProtocol::Icmp,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );

    // Buffer to hold ICMP Packet
    let mut icmp_buffer = [0u8; 12];
    // Buffer for the full IPv4 packet
    let mut tx_ipv4_buffer = [0u8; 64];

    // Determine trigger frequency
    let freq = match frequency_hz {
        Some(freq) => freq as u64,
        None => 100,
    };

    // Initialize sequence counter
    let mut seq_counter: u16 = 0;

    log_ln!("Starting Trigger Traffic");

    // Start sending trigger packets
    loop {
        match select3(
            STOP_SIGNAL.wait(),
            Timer::after(Duration::from_hz(freq)),
            DHCP_CLIENT_INFO.wait(),
        )
        .await
        {
            Either3::First(_) => {
                // Stop signal received, exit the loop
                STOP_SIGNAL.signal(());
                break;
            }
            Either3::Second(_) => {
                // Increment sequence number for this packet
                seq_counter = seq_counter.wrapping_add(1);

                // --- PACKET CONSTRUCTION START ---
                // We reconstruct the packet inside the loop to update the 'seq_no'

                // Create ICMP Packet wrapper around the existing buffer
                let mut icmp_packet = Icmpv4Packet::new_unchecked(&mut icmp_buffer[..]);

                // Create an ICMPv4 Echo Request with dynamic Sequence Number
                let icmp_repr = Icmpv4Repr::EchoRequest {
                    ident: 0x22b,
                    seq_no: seq_counter, // <--- Updated per loop iteration
                    data: &[0xDE, 0xAD, 0xBE, 0xEF],
                };

                // Serialize the ICMP representation into the packet
                icmp_repr.emit(&mut icmp_packet, &ChecksumCapabilities::default());

                // Define the IPv4 representation
                let ipv4_repr = Ipv4Repr {
                    src_addr: ip_info.local_address.address(),
                    dst_addr: ip_info.gateway_address,
                    payload_len: icmp_repr.buffer_len(),
                    hop_limit: 64, // Time-to-live value
                    next_header: IpProtocol::Icmp,
                };

                // Create the IPv4 packet wrapper around the existing buffer
                let mut ipv4_packet = Ipv4Packet::new_unchecked(&mut tx_ipv4_buffer);

                // Serialize the IPv4 representation into the packet
                ipv4_repr.emit(&mut ipv4_packet, &ChecksumCapabilities::default());

                // Copy the ICMP packet into the IPv4 packet's payload
                ipv4_packet
                    .payload_mut()
                    .copy_from_slice(icmp_packet.into_inner());

                // IP Packet buffer that will be sent
                let ipv4_packet_buffer = ipv4_packet.into_inner();
                // --- PACKET CONSTRUCTION END ---

                // Send raw packet
                raw_socket.send(ipv4_packet_buffer).await;
            }
            Either3::Third(new_ip_info) => {
                ip_info = new_ip_info;
                log_ln!("Updated station IP context for trigger traffic");
            }
        }
    }
}
