// SPDX-License-Identifier: GPL-2.0-only

use super::*;
use dhcp_protocol::{DhcpOption, Message as DhcpMessage, MessageType, OpCode};
use net_types::ethernet::Mac;
use net_types::ip::{Ipv4, Ipv4Addr as NetIpv4Addr, PrefixLength};
use netstack3_base::NetworkSerializationContext;
use netstack3_port_integration::{
    NativeIpAddress, Runtime,
    service::{DhcpService, DhcpStatus},
};
use netstack3_port_spike::{
    EthernetDeviceEvent, EthernetFrame, EthernetRunner, NetworkServiceEndpoint, RemoteIpAddress,
    RemoteIpVersion, RemoteSocketAddress, RemoteSocketError, RemoteSocketProvider,
};
use packet::{Buf, NestableSerializer as _, Serializer as _};
use packet_formats::{
    ethernet::{ETHERNET_MIN_BODY_LEN_NO_TAG, EtherType, EthernetFrameBuilder},
    ip::{IpProto, Ipv4Proto},
    ipv4::Ipv4PacketBuilder,
    udp::UdpPacketBuilder,
};
use rand::rngs::StdRng;
use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr, TcpListener, TcpStream},
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
    time::Duration,
};

#[test]
fn offline_service_returns_socks_network_unreachable_and_keeps_serving() {
    let (device_capability, _driver) = ethernet_port(CLIENT_MAC, 32).unwrap();
    let device = unsafe {
        ServiceEthernetDevice::from_frame_fd(device_capability.into_frame_fd(), CLIENT_MAC)
    };
    let mut service = BoundedNetstackProof::new(
        device,
        NetstackProofConfig {
            dns_name: "unused.invalid.".into(),
            server_port: NonZeroU16::new(80).unwrap(),
        },
    )
    .unwrap();
    assert!(!service.network_ready());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let listen = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(listen).unwrap();
    client
        .write_all(&[
            5, 1, 0, // greeting: SOCKS5, one method, no authentication
            5, 1, 0, 1, 192, 0, 2, 1, 0, 80, // CONNECT to TEST-NET-1 while offline
        ])
        .unwrap();
    let mut domain_client = TcpStream::connect(listen).unwrap();
    domain_client
        .write_all(&[
            5, 1, 0, // greeting
            5, 1, 0, 3, 15, b'o', b'f', b'f', b'l', b'i', b'n', b'e', b'.', b'i', b'n', b'v', b'a',
            b'l', b'i', b'd', 0, 80, // domain CONNECT while DNS is unavailable
        ])
        .unwrap();

    service
        .serve_socks5_listener(
            listener,
            listen,
            Some(std::time::Instant::now() + Duration::from_millis(200)),
            || false,
        )
        .unwrap();
    let mut response = [0; 12];
    client.read_exact(&mut response).unwrap();
    assert_eq!(
        response,
        [
            5, 0, // greeting accepted
            5, 3, 0, 1, 0, 0, 0, 0, 0, 0, // network unreachable
        ]
    );
    let mut domain_response = [0; 12];
    domain_client.read_exact(&mut domain_response).unwrap();
    assert_eq!(
        domain_response,
        [
            5, 0, // greeting accepted
            5, 4, 0, 1, 0, 0, 0, 0, 0, 0, // host unreachable
        ]
    );
    assert!(!service.network_ready());
}

#[test]
fn revoked_frame_generation_exits_and_replacement_starts() {
    let make_service = || {
        let (capability, driver) = ethernet_port(CLIENT_MAC, 32).unwrap();
        let device =
            unsafe { ServiceEthernetDevice::from_frame_fd(capability.into_frame_fd(), CLIENT_MAC) };
        let service = BoundedNetstackProof::new(
            device,
            NetstackProofConfig {
                dns_name: "unused.invalid.".into(),
                server_port: NonZeroU16::new(80).unwrap(),
            },
        )
        .unwrap();
        (service, driver)
    };

    let (mut revoked, old_driver) = make_service();
    drop(old_driver);
    let old_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    old_listener.set_nonblocking(true).unwrap();
    let old_listen = old_listener.local_addr().unwrap();
    assert_eq!(
        revoked.serve_socks5_listener(
            old_listener,
            old_listen,
            Some(std::time::Instant::now() + Duration::from_millis(20)),
            || false,
        ),
        Err("Ethernet frame seam closed")
    );

    let (mut replacement, _new_driver) = make_service();
    let new_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    new_listener.set_nonblocking(true).unwrap();
    let new_listen = new_listener.local_addr().unwrap();
    replacement
        .serve_socks5_listener(
            new_listener,
            new_listen,
            Some(std::time::Instant::now() + Duration::from_millis(5)),
            || false,
        )
        .unwrap();
    assert!(!replacement.network_ready());
}
use wlan_softmac_host::ethernet::{
    AssociatedSoftmacTx, DriverEthernetPort, EthernetIngressError, ethernet_port,
};

const CLIENT_MAC: [u8; 6] = [2, 0, 0, 0, 0, 1];
const AP_MAC: [u8; 6] = [2, 0, 0, 0, 0, 2];
const CLIENT_IP: [u8; 4] = [192, 0, 2, 10];
const SERVER_IP: [u8; 4] = [192, 0, 2, 1];
const RENEWED_DNS_IP: [u8; 4] = [192, 0, 2, 53];

struct AssociatedAp {
    server: Runtime,
    dns: netstack3_port_integration::UdpSocketHandle,
    advertised_dns: [u8; 4],
    renewal_requests: usize,
    pending: VecDeque<EthernetFrame>,
}

impl AssociatedAp {
    fn new() -> Self {
        let mut server = Runtime::new(
            32,
            (0u8..=255).rev().cycle().take(8192),
            NonZeroU64::new(2).unwrap(),
            AP_MAC,
            1500,
        )
        .unwrap();
        server.apply_ipv4(SERVER_IP, 24, None).unwrap();
        let dns = server.udp_socket().unwrap();
        server
            .udp_bind(dns, Some(SERVER_IP), NonZeroU16::new(53).unwrap())
            .unwrap();
        Self {
            server,
            dns,
            advertised_dns: SERVER_IP,
            renewal_requests: 0,
            pending: VecDeque::new(),
        }
    }

    fn dhcp_request(frame: &[u8]) -> Option<DhcpMessage> {
        if frame.get(12..14)? != [0x08, 0x00] {
            return None;
        }
        let ip = 14;
        let ihl = usize::from(*frame.get(ip)? & 0x0f) * 4;
        let udp = ip.checked_add(ihl)?;
        if frame.get(udp..udp + 4)? != [0, 68, 0, 67] {
            return None;
        }
        DhcpMessage::from_buffer(frame.get(udp + 8..)?).ok()
    }

    fn queue_dhcp_reply(&mut self, request: DhcpMessage) {
        if request.ciaddr.octets() == CLIENT_IP {
            self.renewal_requests += 1;
        }
        let destination = if request.ciaddr.is_unspecified() {
            [255, 255, 255, 255]
        } else {
            request.ciaddr.octets()
        };
        let kind = match request.get_dhcp_type().unwrap() {
            MessageType::DHCPDISCOVER => MessageType::DHCPOFFER,
            MessageType::DHCPREQUEST => MessageType::DHCPACK,
            other => panic!("unexpected DHCP request {other:?}"),
        };
        let reply = DhcpMessage {
            op: OpCode::BOOTREPLY,
            xid: request.xid,
            secs: 0,
            bdcast_flag: false,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            yiaddr: Ipv4Addr::from(CLIENT_IP),
            siaddr: Ipv4Addr::from(SERVER_IP),
            giaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: Mac::new(CLIENT_MAC),
            sname: Default::default(),
            file: Default::default(),
            options: vec![
                DhcpOption::DhcpMessageType(kind),
                DhcpOption::ServerIdentifier(Ipv4Addr::from(SERVER_IP)),
                DhcpOption::IpAddressLeaseTime(120),
                DhcpOption::SubnetMask(PrefixLength::<Ipv4>::new(24).unwrap()),
                DhcpOption::Router([Ipv4Addr::from(SERVER_IP)].into()),
                DhcpOption::DomainNameServer([Ipv4Addr::from(self.advertised_dns)].into()),
            ],
        };
        let src = NetIpv4Addr::new(SERVER_IP);
        let dst = NetIpv4Addr::new(destination);
        let bytes = Buf::new(reply.serialize(), ..)
            .wrap_in(UdpPacketBuilder::new(
                src,
                dst,
                Some(NonZeroU16::new(67).unwrap()),
                NonZeroU16::new(68).unwrap(),
            ))
            .wrap_in(Ipv4PacketBuilder::new(
                src,
                dst,
                64,
                Ipv4Proto::Proto(IpProto::Udp),
            ))
            .wrap_in(EthernetFrameBuilder::new(
                Mac::new(AP_MAC),
                Mac::new(CLIENT_MAC),
                EtherType::Ipv4,
                ETHERNET_MIN_BODY_LEN_NO_TAG,
            ))
            .serialize_vec_outer(&mut NetworkSerializationContext::default())
            .unwrap()
            .unwrap_b()
            .into_inner();
        self.pending
            .push_back(EthernetFrame::try_from(bytes).unwrap());
    }

    fn service_dns(&mut self) {
        let Some(datagram) = self.server.udp_receive_msg(self.dns).unwrap() else {
            return;
        };
        let NativeIpAddress::V4(source) = datagram.source.address else {
            panic!("IPv6 query on IPv4 DNS socket")
        };
        let request = &datagram.body;
        assert!(request.len() >= 17 && request[4..6] == [0, 1]);
        let mut question_end = 12;
        loop {
            let label = usize::from(request[question_end]);
            question_end += 1;
            if label == 0 {
                break;
            }
            question_end += label;
        }
        question_end += 4;
        let mut response = Vec::with_capacity(question_end + 16);
        response.extend_from_slice(&request[..2]);
        response.extend_from_slice(&[0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
        response.extend_from_slice(&request[12..question_end]);
        response.extend_from_slice(&[
            0xc0,
            0x0c,
            0,
            1,
            0,
            1,
            0,
            0,
            0,
            60,
            0,
            4,
            SERVER_IP[0],
            SERVER_IP[1],
            SERVER_IP[2],
            SERVER_IP[3],
        ]);
        self.server
            .udp_send_to(
                self.dns,
                source,
                NonZeroU16::new(datagram.source.port).unwrap(),
                &response,
            )
            .unwrap();
    }

    fn collect_server_frames(&mut self) {
        self.service_dns();
        while let Some(frame) = self.server.take_tx() {
            self.pending.push_back(frame);
        }
    }
}

impl AssociatedSoftmacTx for AssociatedAp {
    type Error = ();

    fn transmit_ethernet(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        if let Some(request) = Self::dhcp_request(frame) {
            self.queue_dhcp_reply(request);
        } else {
            self.server
                .receive_frame(EthernetFrame::copy_from_slice(frame).unwrap());
        }
        Ok(())
    }
}

fn drive(
    runner: &mut EthernetRunner<DhcpService, ServiceEthernetDevice>,
    sink: &mut DriverEthernetPort,
    ap: &mut AssociatedAp,
    now: Duration,
) {
    for _ in 0..8 {
        while let Some(event) = runner.device_mut().take_event() {
            if event == EthernetDeviceEvent::LinkStateChanged(false) {
                runner.discard_pending();
            }
            runner.stack_mut().on_device_event(event);
        }
        runner.stack_mut().poll_at(now, 64);
        while runner.pump().transmitted != 0 {}
        loop {
            match sink.take_transmit() {
                Ok(Some(frame)) => ap.transmit_ethernet(frame.as_bytes()).unwrap(),
                Ok(None) | Err(EthernetIngressError::LinkDown) => break,
                Err(error) => panic!("unexpected associated TX failure: {error:?}"),
            }
        }
        ap.collect_server_frames();
        while let Some(frame) = ap.pending.pop_front() {
            match sink.deliver(frame.as_bytes()) {
                Ok(()) | Err(EthernetIngressError::LinkDown) => {}
                Err(error) => panic!("unexpected associated RX failure: {error:?}"),
            }
        }
        while runner.pump().received != 0 {}
    }
}

#[test]
fn socks_connect_relays_application_bytes_over_ethernet() {
    let (device_capability, mut sink) = ethernet_port(CLIENT_MAC, 32).unwrap();
    let device = unsafe {
        ServiceEthernetDevice::from_frame_fd(device_capability.into_frame_fd(), CLIENT_MAC)
    };
    let mut service = BoundedNetstackProof::new(
        device,
        NetstackProofConfig {
            dns_name: "unused.invalid.".into(),
            server_port: NonZeroU16::new(8080).unwrap(),
        },
    )
    .unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    let completed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ap_completed = completed.clone();
    let ap_thread = std::thread::spawn(move || {
        let mut ap = AssociatedAp::new();
        let listener = ap.server.tcp_socket().unwrap();
        ap.server
            .tcp_bind(listener, Some(SERVER_IP), NonZeroU16::new(8080).unwrap())
            .unwrap();
        ap.server
            .tcp_listen(listener, NonZeroUsize::new(1).unwrap())
            .unwrap();
        sink.set_link(true);
        ready_tx.send(()).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut accepted = None;
        let mut request = Vec::new();
        let mut response_sent = false;
        loop {
            while let Ok(Some(frame)) = sink.take_transmit() {
                ap.transmit_ethernet(frame.as_bytes()).unwrap();
            }
            ap.collect_server_frames();
            while let Some(frame) = ap.pending.pop_front() {
                let _ = sink.deliver(frame.as_bytes());
            }
            if accepted.is_none() && ap.server.tcp_pending_connections(listener).unwrap() != 0 {
                accepted = Some(ap.server.tcp_accept(listener).unwrap());
            }
            if let Some(socket) = accepted {
                let mut bytes = [0; 64];
                let read = ap.server.tcp_read(socket, &mut bytes).unwrap();
                request.extend_from_slice(&bytes[..read]);
                if request.len() >= 7 && !response_sent {
                    request_tx.send(request.clone()).unwrap();
                    assert_eq!(
                        ap.server
                            .tcp_write(socket, b"HTTP/1.0 200 OK\r\n\r\n")
                            .unwrap(),
                        19
                    );
                    response_sent = true;
                }
            }
            if ap_completed.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "SOCKS test AP timed out"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    ready_rx.recv().unwrap();
    service
        .prove_dhcp(std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let listen = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(listen).unwrap();
    client
        .write_all(&[
            5,
            1,
            0, // greeting
            5,
            1,
            0,
            1,
            SERVER_IP[0],
            SERVER_IP[1],
            SERVER_IP[2],
            SERVER_IP[3],
            0x1f,
            0x90,
            b'G',
            b'E',
            b'T',
            b' ',
            b'/',
            b'\r',
            b'\n', // pipelined application request
        ])
        .unwrap();
    client.set_nonblocking(true).unwrap();
    let mut response = Vec::new();
    service
        .serve_socks5_listener(
            listener,
            listen,
            Some(std::time::Instant::now() + Duration::from_secs(2)),
            || {
                let mut bytes = [0; 64];
                loop {
                    match client.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(read) => response.extend_from_slice(&bytes[..read]),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) => panic!("SOCKS client read failed: {error}"),
                    }
                }
                if response.len() >= 31 {
                    completed.store(true, std::sync::atomic::Ordering::Release);
                    true
                } else {
                    false
                }
            },
        )
        .unwrap();

    assert_eq!(response.len(), 31);
    assert_eq!(&response[..2], &[5, 0]);
    assert_eq!(&response[2..12], &[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&response[12..], b"HTTP/1.0 200 OK\r\n\r\n");
    assert_eq!(request_rx.recv().unwrap(), b"GET /\r\n");
    ap_thread.join().unwrap();
}

#[test]
fn associated_link_renews_dns_without_destroying_tcp_then_revokes_on_loss() {
    let (device_capability, mut sink) = ethernet_port(CLIENT_MAC, 32).unwrap();
    let device = unsafe {
        ServiceEthernetDevice::from_frame_fd(device_capability.into_frame_fd(), CLIENT_MAC)
    };
    let runtime = Runtime::new(
        32,
        (0u8..=255).cycle().take(8192),
        NonZeroU64::new(1).unwrap(),
        CLIENT_MAC,
        u32::from(SOFTMAC_ETHERNET_MTU),
    )
    .unwrap();
    let service = DhcpService::new(runtime, StdRng::seed_from_u64(7), CLIENT_MAC);
    let mut runner = EthernetRunner::new(service, device);
    let mut ap = AssociatedAp::new();

    // The host admits frames only after the controlled-port owner raises link.
    sink.set_link(true);
    for second in 0..16 {
        drive(&mut runner, &mut sink, &mut ap, Duration::from_secs(second));
        if runner.stack().status() == DhcpStatus::Bound {
            break;
        }
    }
    assert_eq!(runner.stack().status(), DhcpStatus::Bound);
    assert_eq!(runner.stack().runtime().ipv4_address(), Some(CLIENT_IP));
    assert_eq!(
        runner.stack().runtime().dns_servers(),
        [Some(Ipv4Addr::from(SERVER_IP)), None]
    );

    let lookup = runner.stack_mut().lookup_ip("internet.test.").unwrap();
    for second in 16..48 {
        drive(&mut runner, &mut sink, &mut ap, Duration::from_secs(second));
        if let Some(result) = runner.stack_mut().take_lookup(lookup) {
            assert_eq!(result.unwrap(), [IpAddr::V4(Ipv4Addr::from(SERVER_IP))]);
            break;
        }
        assert!(second != 47, "DNS lookup did not complete");
    }

    let listener = ap.server.tcp_socket().unwrap();
    ap.server
        .tcp_bind(listener, Some(SERVER_IP), NonZeroU16::new(8080).unwrap())
        .unwrap();
    ap.server
        .tcp_listen(listener, NonZeroUsize::new(1).unwrap())
        .unwrap();
    let mut provider = runner.stack().socket_provider();
    let client =
        RemoteSocketProvider::open_client(&mut provider, NonZeroUsize::new(2).unwrap()).unwrap();
    let socket = provider.tcp_socket(client, RemoteIpVersion::V4).unwrap();
    provider
        .tcp_connect(
            socket,
            RemoteSocketAddress {
                address: RemoteIpAddress::V4(SERVER_IP),
                port: NonZeroU16::new(8080).unwrap(),
            },
        )
        .unwrap();
    for second in 48..80 {
        drive(&mut runner, &mut sink, &mut ap, Duration::from_secs(second));
        if ap.server.tcp_pending_connections(listener).unwrap() != 0 {
            break;
        }
    }
    let accepted = ap.server.tcp_accept(listener).unwrap();
    assert_eq!(
        provider
            .tcp_write(socket, b"GET / HTTP/1.0\r\n\r\n")
            .unwrap(),
        18
    );
    // The upstream DHCP client renews at half the lease lifetime. Change the
    // advertised DNS server before advancing past that deadline.
    assert_eq!(ap.renewal_requests, 0);
    ap.advertised_dns = RENEWED_DNS_IP;
    for second in 80..96 {
        drive(&mut runner, &mut sink, &mut ap, Duration::from_secs(second));
    }
    let mut request = [0; 64];
    let read = ap.server.tcp_read(accepted, &mut request).unwrap();
    assert_eq!(&request[..read], b"GET / HTTP/1.0\r\n\r\n");

    assert_eq!(
        runner.stack().runtime().dns_servers(),
        [Some(Ipv4Addr::from(RENEWED_DNS_IP)), None]
    );
    assert_eq!(ap.renewal_requests, 1);
    // Prove the established application socket remains usable through the
    // renewal rather than merely retaining its provider handle.
    assert_eq!(
        ap.server
            .tcp_write(accepted, b"HTTP/1.0 200 OK\r\n\r\n")
            .unwrap(),
        19
    );
    for second in 96..112 {
        drive(&mut runner, &mut sink, &mut ap, Duration::from_secs(second));
    }
    let mut response = [0; 64];
    let read = provider.tcp_read(socket, &mut response).unwrap();
    assert_eq!(&response[..read], b"HTTP/1.0 200 OK\r\n\r\n");

    sink.set_link(false);
    for second in 112..128 {
        drive(&mut runner, &mut sink, &mut ap, Duration::from_secs(second));
        if runner.stack().runtime().ipv4_address().is_none() {
            break;
        }
    }
    assert_eq!(runner.stack().runtime().ipv4_address(), None);
    assert_eq!(runner.stack().runtime().dns_servers(), [None, None]);
    let blocked = provider.tcp_socket(client, RemoteIpVersion::V4).unwrap();
    assert_eq!(
        provider.tcp_connect(
            blocked,
            RemoteSocketAddress {
                address: RemoteIpAddress::V4(SERVER_IP),
                port: NonZeroU16::new(8080).unwrap(),
            },
        ),
        Err(RemoteSocketError::NetworkUnreachable)
    );
}
