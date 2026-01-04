use core::{net::Ipv4Addr, str::FromStr};
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_net::raw::{IpProtocol, IpVersion, PacketMetadata, RawSocket};
use embassy_net::{Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use enumset::enum_set;
use esp_println::println;
use esp_radio::wifi::{ModeConfig, WifiController, WifiDevice, WifiEvent};
use smoltcp::phy::ChecksumCapabilities;

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};

use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, Ipv4Packet, Ipv4Repr};

use crate::WifiStationConfig;

static DHCP_CLIENT_INFO: Signal<CriticalSectionRawMutex, IpInfo> = Signal::new();

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[derive(Debug, Clone)]
struct IpInfo {
    pub local_address: Ipv4Cidr,
    pub gateway_address: Ipv4Address,
}

pub fn sta_init(
    interfaces: WifiDevice<'static>,
    config: &WifiStationConfig,
    controller: &mut WifiController,
    spawner: Spawner,
) -> Stack<'static> {
    // Station IP Configuration - DHCP
    let sta_ip_config = embassy_net::Config::dhcpv4(Default::default());
    let seed = 123456_u64;

    // Create STA Network Stack
    let (sta_stack, sta_runner) = embassy_net::new(
        interfaces,
        sta_ip_config,
        mk_static!(StackResources<6>, StackResources::<6>::new()),
        seed,
    );

    // Spawn the network runner task
    spawner.spawn(net_task(sta_runner)).ok();
    println!("Network Task Running");

    // Configure WiFi Client/Station Connection
    let station_config = ModeConfig::Client(config.client_config.clone());
    // Set the Configuration
    match controller.set_config(&station_config) {
        Ok(_) => println!("WiFi Configuration Set: {:?}", config),
        Err(_) => {
            println!("WiFi Configuration Error");
            println!("Error Config: {:?}", config);
        }
    }

    sta_stack
}

pub async fn sta_connect(
    controller: &'static mut WifiController<'static>,
    freq: Option<u16>,
    sta_stack: Stack<'static>,
    spawner: Spawner,
) {
    // Connect WiFi
    match controller.connect_async().await {
        Ok(_) => println!("WiFi Connected"),
        Err(e) => {
            panic!("Failed to connect WiFi: {:?}", e);
        }
    }

    // Run DHCP Client to acquire IP
    run_dhcp_client(sta_stack).await;
    // Run NTP Sync to synchronize time
    // if self.sync_time {
    //     run_ntp_sync(sta_stack).await;
    // }

    spawner.spawn(sta_connection(controller)).ok();
    spawner.spawn(sta_network_ops(sta_stack, freq)).ok();
}

#[embassy_executor::task]
pub async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    // println!("Network Task Running");
    runner.run().await
}

async fn run_dhcp_client(sta_stack: Stack<'static>) {
    println!("Running DHCP Client");

    // Acquire and store IP information for gateway and client after configuration is up

    // Check if link is up
    sta_stack.wait_link_up().await;
    println!("Link is up!");

    // Create instance to store acquired IP information
    let mut ip_info = IpInfo {
        local_address: Ipv4Cidr::new(Ipv4Addr::UNSPECIFIED, 24),
        gateway_address: Ipv4Address::UNSPECIFIED,
    };

    println!("Acquiring config...");
    sta_stack.wait_config_up().await;
    println!("Config Acquired");

    // Print out acquired IP configuration
    loop {
        if let Some(config) = sta_stack.config_v4() {
            ip_info.local_address = config.address;
            ip_info.gateway_address = config.gateway.unwrap();

            #[cfg(feature = "defmt")]
            {
                info!("Local IP: {:?}", ip_info.local_address);
                info!("Gateway IP: {:?}", ip_info.gateway_address);
            }

            #[cfg(not(feature = "defmt"))]
            {
                println!("Local IP: {:?}", ip_info.local_address);
                println!("Gateway IP: {:?}", ip_info.gateway_address);
            }

            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }
    // // Store Gateway Address in Global Context
    // GATEWAY_ADDRESS.lock(|lock| {
    //     lock.replace(ip_info.gateway_address);
    // });
    // Signal that DHCP is complete
    DHCP_CLIENT_INFO.signal(ip_info);
}

#[embassy_executor::task]
pub async fn sta_connection(controller: &'static mut WifiController<'static>) {
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
        let mut wait_event_fut = controller.wait_for_events(sta_events, true).await;

        // MAYBE JUST DO A STOP COLLECTION & DEINIT WHERE WE RETURN FROM ALL TASKS

        // // If either future completes, handle accordingly
        // match select(wait_event_fut, stop_coll_fut).await {
        //     // Wait event future cases
        //     Either::First(mut event) => {
        if wait_event_fut.contains(WifiEvent::StaDisconnected) {
            println!("STA Disconnected");
        }
        if wait_event_fut.contains(WifiEvent::StaStop) {
            println!("STA Stopped");
        }
        wait_event_fut.clear();
        //     }
        //     // Stop collection future case
        //     Either::Second(sig) => {
        //         // Stop Signal
        //         if !sig {
        //             println!("Halting CSI Collection...");
        //             // Send the controller back before exiting loop
        //             // CONTROLLER_CH.send(controller).await;
        //             // CONTROLLER_HALTED_SIGNAL.signal(true);
        //             break;
        //         }
        //     }
        // }
    }
}

// This task manages network operations for the station
#[embassy_executor::task]
pub async fn sta_network_ops(sta_stack: Stack<'static>, freq: Option<u16>) {
    // Retrieve acquired IP information from DHCP
    let ip_info = DHCP_CLIENT_INFO.wait().await;

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

    // Create ICMP Packet
    let mut icmp_packet = Icmpv4Packet::new_unchecked(&mut icmp_buffer[..]);

    // Create an ICMPv4 Echo Request
    let icmp_repr = Icmpv4Repr::EchoRequest {
        ident: 0x22b,
        seq_no: 0,
        data: &[0xDE, 0xAD, 0xBE, 0xEF],
    };

    // Serialize the ICMP representation into the packet
    icmp_repr.emit(&mut icmp_packet, &ChecksumCapabilities::default());

    // Buffer for the full IPv4 packet
    let mut tx_ipv4_buffer = [0u8; 64];

    // Define the IPv4 representation
    let ipv4_repr = Ipv4Repr {
        src_addr: ip_info.local_address.address(),
        dst_addr: ip_info.gateway_address,
        payload_len: icmp_repr.buffer_len(),
        hop_limit: 64, // Time-to-live value
        next_header: IpProtocol::Icmp,
    };

    // Create the IPv4 packet
    let mut ipv4_packet = Ipv4Packet::new_unchecked(&mut tx_ipv4_buffer);

    // Serialize the IPv4 representation into the packet
    ipv4_repr.emit(&mut ipv4_packet, &ChecksumCapabilities::default());

    // Copy the ICMP packet into the IPv4 packet's payload
    ipv4_packet
        .payload_mut()
        .copy_from_slice(icmp_packet.into_inner());

    // IP Packet buffer that will be sent or recieved
    let ipv4_packet_buffer = ipv4_packet.into_inner();

    // loop {
    // Wait for start signal
    // while !start_collection_watch.changed().await {
    //     Timer::after(Duration::from_millis(100)).await;
    // }
    // println!("Starting Trigger Traffic");
    // Station Trigger supports sending ICMP Echo Requests as trigger packets at defined frequency

    let trigger_interval = match freq {
        Some(freq) => 1000_u64 / freq as u64,
        None => u32::MAX as u64,
    };
    // let trigger_interval = Duration::from_millis((1000 / trigger_config.trigger_freq_hz).into());
    // Start sending trigger packets
    loop {
        // Trigger Interval Future
        let _trigger_timer_fut = Timer::after(Duration::from_millis(trigger_interval)).await;
        // Stop Trigger Future
        // let stop_coll_fut = start_collection_watch.changed();

        // match select(trigger_timer_fut, stop_coll_fut).await {
        //     Either::First(_) => {
        // Send raw packet
        raw_socket.send(ipv4_packet_buffer).await;
        // }
        //     Either::Second(sig) => {
        //         if !sig {
        //             println!("Stopping Trigger Traffic");
        //             break;
        //         }
        //     }
        // };
    }
    // }
}
