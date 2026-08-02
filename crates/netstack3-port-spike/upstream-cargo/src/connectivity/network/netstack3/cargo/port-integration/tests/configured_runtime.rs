#![recursion_limit = "256"]

use std::net::Ipv4Addr as StdIpv4Addr;
use std::num::{NonZeroU16, NonZeroUsize};

use edge_dhcp::{MessageType as DhcpMessageType, Options, Packet};
use hickory_proto::op::{Message, OpCode};
use hickory_proto::rr::rdata::A;
use hickory_proto::rr::{Name, RData, Record, RecordType};
use net_declare::{net_ip_v4, net_mac, net_subnet_v4};
use net_types::ethernet::Mac;
use net_types::ip::Ipv4;
use net_types::{SpecifiedAddr, UnicastAddr, ZonedAddr};
use netstack3_base::{NetworkParsingContext, NetworkSerializationContext};
use netstack3_core::device::{EthernetDeviceId, EthernetLinkDevice, RecvEthernetFrameMeta};
use netstack3_core::testutil::{CtxPairExt as _, FakeBindingsCtx, FakeCtx, FakeCtxBuilder};
use netstack3_port_spike::control_plane::{DhcpOffer, Dhcpv4Client, DnsCodec};
use netstack3_port_spike::{EthernetDevice as _, EthernetFrame, FakeEthernetDevice};
use netstack3_tcp::testutil::{ProvidedBuffers, WriteBackClientBuffers};
use packet::{Buf, NestableSerializer as _, Serializer as _};
use packet_formats::ethernet::{ETHERNET_MIN_BODY_LEN_NO_TAG, EtherType, EthernetFrameBuilder};
use packet_formats::ip::{IpProto, Ipv4Proto};
use packet_formats::ipv4::Ipv4PacketBuilder;
use packet_formats::udp::UdpPacketBuilder;
use rand_core_06::RngCore;

const CLIENT_MAC: Mac = net_mac!("22:33:44:55:66:77");
const SERVER_MAC: Mac = net_mac!("88:88:88:88:88:88");
const CLIENT_IP: net_types::ip::Ipv4Addr = net_ip_v4!("192.0.2.10");
const SERVER_IP: net_types::ip::Ipv4Addr = net_ip_v4!("192.0.2.1");

#[derive(Clone, Copy)]
struct FixedRandom(u32);

impl RngCore for FixedRandom {
    fn next_u32(&mut self) -> u32 {
        self.0
    }

    fn next_u64(&mut self) -> u64 {
        u64::from(self.next_u32())
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(4) {
            let bytes = self.next_u32().to_ne_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core_06::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

fn dhcp_frame(
    src_mac: Mac,
    dst_mac: Mac,
    src: net_types::ip::Ipv4Addr,
    dst: net_types::ip::Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    body: &[u8],
) -> EthernetFrame {
    let bytes = Buf::new(body.to_vec(), ..)
        .wrap_in(UdpPacketBuilder::new(
            src,
            dst,
            NonZeroU16::new(src_port),
            NonZeroU16::new(dst_port).unwrap(),
        ))
        .wrap_in(Ipv4PacketBuilder::new(
            src,
            dst,
            64,
            Ipv4Proto::Proto(IpProto::Udp),
        ))
        .wrap_in(EthernetFrameBuilder::new(
            src_mac,
            dst_mac,
            EtherType::Ipv4,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        ))
        .serialize_vec_outer(&mut NetworkSerializationContext::default())
        .unwrap()
        .unwrap_b()
        .into_inner();
    EthernetFrame::try_from(bytes).unwrap()
}

fn udp_payload(frame: &EthernetFrame) -> &[u8] {
    let bytes = frame.as_bytes();
    assert_eq!(&bytes[12..14], &[0x08, 0x00]);
    let ip_header_len = usize::from(bytes[14] & 0x0f) * 4;
    &bytes[14 + ip_header_len + 8..]
}

fn encode_dhcp(packet: &Packet<'_>) -> Vec<u8> {
    let mut bytes = [0; 1232];
    packet.encode(&mut bytes).unwrap().to_vec()
}

fn acquire_lease(port: &mut FakeEthernetDevice) -> DhcpOffer {
    let mut client = Dhcpv4Client::new(FixedRandom(0x1234_5678), CLIENT_MAC.bytes());
    let (discover_tx, discover) = client.discover(0).unwrap();
    port.transmit(dhcp_frame(
        CLIENT_MAC,
        net_mac!("ff:ff:ff:ff:ff:ff"),
        net_ip_v4!("0.0.0.0"),
        net_ip_v4!("255.255.255.255"),
        68,
        67,
        discover.as_bytes(),
    ))
    .unwrap();
    let discover_frame = port.take_transmitted().unwrap();
    let discover = Packet::decode(udp_payload(&discover_frame)).unwrap();

    let gateways = [StdIpv4Addr::new(192, 0, 2, 1)];
    let dns = [StdIpv4Addr::new(192, 0, 2, 1)];
    let mut options = Options::buf();
    let offer_options = discover.options.reply(
        DhcpMessageType::Offer,
        StdIpv4Addr::new(192, 0, 2, 1),
        3600,
        &gateways,
        Some(StdIpv4Addr::new(255, 255, 255, 0)),
        &dns,
        None,
        &mut options,
    );
    let offer = discover.new_reply(Some(StdIpv4Addr::new(192, 0, 2, 10)), offer_options);
    let offer = encode_dhcp(&offer);
    let offer = client.accept_offer(discover_tx, &offer).unwrap().unwrap();

    let (request_tx, request) = client.request(1, offer).unwrap();
    port.transmit(dhcp_frame(
        CLIENT_MAC,
        net_mac!("ff:ff:ff:ff:ff:ff"),
        net_ip_v4!("0.0.0.0"),
        net_ip_v4!("255.255.255.255"),
        68,
        67,
        request.as_bytes(),
    ))
    .unwrap();
    let request_frame = port.take_transmitted().unwrap();
    let request = Packet::decode(udp_payload(&request_frame)).unwrap();
    let mut options = Options::buf();
    let ack_options = request.options.reply(
        DhcpMessageType::Ack,
        StdIpv4Addr::new(192, 0, 2, 1),
        3600,
        &gateways,
        Some(StdIpv4Addr::new(255, 255, 255, 0)),
        &dns,
        None,
        &mut options,
    );
    let ack = request.new_reply(Some(StdIpv4Addr::new(192, 0, 2, 10)), ack_options);
    client
        .accept_ack(request_tx, &encode_dhcp(&ack))
        .unwrap()
        .unwrap()
}

fn deliver(
    source: &mut FakeCtx,
    source_port: &mut FakeEthernetDevice,
    destination: &mut FakeCtx,
    destination_port: &mut FakeEthernetDevice,
    destination_device: &EthernetDeviceId<FakeBindingsCtx>,
) -> usize {
    let frames = source.bindings_ctx.take_ethernet_frames();
    let count = frames.len();
    for (_, bytes) in frames {
        source_port
            .transmit(EthernetFrame::try_from(bytes).unwrap())
            .unwrap();
        let frame = source_port.take_transmitted().unwrap();
        destination_port.inject(frame).unwrap();
        let frame = destination_port.receive().unwrap();
        destination
            .core_api()
            .device::<EthernetLinkDevice>()
            .receive_frame(
                RecvEthernetFrameMeta {
                    device_id: destination_device.clone(),
                    parsing_context: NetworkParsingContext::default(),
                },
                Buf::new(frame.into_vec(), ..),
            );
    }
    count
}

fn pump(
    client: &mut FakeCtx,
    client_port: &mut FakeEthernetDevice,
    client_device: &EthernetDeviceId<FakeBindingsCtx>,
    server: &mut FakeCtx,
    server_port: &mut FakeEthernetDevice,
    server_device: &EthernetDeviceId<FakeBindingsCtx>,
) {
    for _ in 0..32 {
        let count = deliver(client, client_port, server, server_port, server_device)
            + deliver(server, server_port, client, client_port, client_device);
        if count == 0 {
            return;
        }
    }
    panic!("network did not quiesce");
}

#[test]
fn dhcp_configuration_drives_dns_udp_and_tcp_over_bounded_ethernet() {
    let mut client_port = FakeEthernetDevice::new(16);
    let lease = acquire_lease(&mut client_port);
    assert_eq!(lease.address, StdIpv4Addr::new(192, 0, 2, 10));
    assert_eq!(lease.gateway, Some(StdIpv4Addr::new(192, 0, 2, 1)));
    assert_eq!(lease.dns_servers[0], Some(StdIpv4Addr::new(192, 0, 2, 1)));

    let mut client_builder = FakeCtxBuilder::default();
    let client_index = client_builder.add_device(UnicastAddr::new(CLIENT_MAC).unwrap());
    let (mut client, client_devices) = client_builder.build();
    let client_device = client_devices.into_iter().nth(client_index).unwrap();
    client
        .core_api()
        .device_ip::<Ipv4>()
        .add_ip_addr_subnet(
            &client_device.clone().into(),
            net_types::ip::AddrSubnet::new(CLIENT_IP, 24).unwrap(),
        )
        .unwrap();
    client
        .test_api()
        .add_route(
            netstack3_core::routes::AddableEntry::without_gateway(
                net_subnet_v4!("192.0.2.0/24"),
                client_device.clone().into(),
                netstack3_core::routes::AddableMetric::ExplicitMetric(
                    netstack3_core::routes::RawMetric(0),
                ),
            )
            .into(),
        )
        .unwrap();

    let mut server_builder = FakeCtxBuilder::default();
    let server_index = server_builder.add_device_with_ip(
        UnicastAddr::new(SERVER_MAC).unwrap(),
        SERVER_IP,
        net_subnet_v4!("192.0.2.0/24"),
    );
    let (mut server, server_devices) = server_builder.build();
    let server_device = server_devices.into_iter().nth(server_index).unwrap();
    let mut server_port = FakeEthernetDevice::new(16);

    let dns_port = NonZeroU16::new(53).unwrap();
    let server_dns = server.core_api().udp::<Ipv4>().create();
    server
        .core_api()
        .udp::<Ipv4>()
        .listen(
            &server_dns,
            SpecifiedAddr::new(SERVER_IP).map(|a| ZonedAddr::Unzoned(a).into()),
            Some(dns_port),
        )
        .unwrap();
    let client_dns = client.core_api().udp::<Ipv4>().create();
    client
        .core_api()
        .udp::<Ipv4>()
        .listen(
            &client_dns,
            SpecifiedAddr::new(CLIENT_IP).map(|a| ZonedAddr::Unzoned(a).into()),
            Some(NonZeroU16::new(53000).unwrap()),
        )
        .unwrap();
    let query = DnsCodec::query(7, "deployment.test.", RecordType::A).unwrap();
    client
        .core_api()
        .udp::<Ipv4>()
        .send_to(
            &client_dns,
            Some(ZonedAddr::Unzoned(SpecifiedAddr::new(SERVER_IP).unwrap()).into()),
            dns_port.into(),
            Buf::new(query.datagram().as_bytes().to_vec(), ..),
        )
        .unwrap();
    pump(
        &mut client,
        &mut client_port,
        &client_device,
        &mut server,
        &mut server_port,
        &server_device,
    );
    let request = server
        .bindings_ctx
        .take_udp_received(&server_dns)
        .pop()
        .unwrap();
    let request = Message::from_vec(&request).unwrap();
    let mut response = Message::response(7, OpCode::Query);
    response.add_query(request.queries[0].clone());
    response.answers.push(Record::from_rdata(
        Name::from_ascii("deployment.test.").unwrap(),
        60,
        RData::A(A(StdIpv4Addr::new(192, 0, 2, 1))),
    ));
    server
        .core_api()
        .udp::<Ipv4>()
        .send_to(
            &server_dns,
            Some(ZonedAddr::Unzoned(SpecifiedAddr::new(CLIENT_IP).unwrap()).into()),
            NonZeroU16::new(53000).unwrap().into(),
            Buf::new(response.to_vec().unwrap(), ..),
        )
        .unwrap();
    pump(
        &mut client,
        &mut client_port,
        &client_device,
        &mut server,
        &mut server_port,
        &server_device,
    );
    let response = client
        .bindings_ctx
        .take_udp_received(&client_dns)
        .pop()
        .unwrap();
    assert_eq!(
        DnsCodec::response(&query, &response).unwrap(),
        [std::net::IpAddr::V4(StdIpv4Addr::new(192, 0, 2, 1))]
    );

    let tcp_port = NonZeroU16::new(4040).unwrap();
    let listener = server
        .core_api()
        .tcp::<Ipv4>()
        .create(ProvidedBuffers::Buffers(WriteBackClientBuffers::default()));
    server
        .core_api()
        .tcp::<Ipv4>()
        .bind(&listener, None, Some(tcp_port))
        .unwrap();
    server
        .core_api()
        .tcp::<Ipv4>()
        .listen(&listener, NonZeroUsize::new(1).unwrap())
        .unwrap();
    let connection = client
        .core_api()
        .tcp::<Ipv4>()
        .create(ProvidedBuffers::Buffers(WriteBackClientBuffers::default()));
    client
        .core_api()
        .tcp::<Ipv4>()
        .connect(
            &connection,
            Some(ZonedAddr::Unzoned(SpecifiedAddr::new(SERVER_IP).unwrap())),
            tcp_port,
        )
        .unwrap();
    pump(
        &mut client,
        &mut client_port,
        &client_device,
        &mut server,
        &mut server_port,
        &server_device,
    );
    let (accepted, _, _) = server.core_api().tcp::<Ipv4>().accept(&listener).unwrap();
    client
        .core_api()
        .tcp::<Ipv4>()
        .with_send_buffer(&connection, |buffer| buffer.enqueue_data(b"configured-tcp"))
        .unwrap();
    client.core_api().tcp::<Ipv4>().do_send(&connection);
    pump(
        &mut client,
        &mut client_port,
        &client_device,
        &mut server,
        &mut server_port,
        &server_device,
    );
    let mut received = Vec::new();
    server
        .core_api()
        .tcp::<Ipv4>()
        .with_receive_buffer(&accepted, |buffer| {
            buffer.lock().read_with(|chunks| {
                let len = chunks.iter().map(|chunk| chunk.len()).sum();
                for chunk in chunks {
                    received.extend_from_slice(chunk);
                }
                len
            })
        })
        .unwrap();
    assert_eq!(received, b"configured-tcp");
}
