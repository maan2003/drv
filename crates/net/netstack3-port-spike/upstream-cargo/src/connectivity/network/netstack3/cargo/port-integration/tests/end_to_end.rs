#![recursion_limit = "256"]
use std::num::NonZeroU16;

use net_declare::{net_ip_v4, net_mac, net_subnet_v4};
use net_types::ethernet::Mac;
use net_types::ip::Ipv4;
use net_types::{SpecifiedAddr, UnicastAddr, ZonedAddr};
use netstack3_base::{NetworkParsingContext, NetworkSerializationContext};
use netstack3_core::CtxPair;
use netstack3_core::device::{EthernetLinkDevice, RecvEthernetFrameMeta};
use netstack3_core::routes::{AddableEntry, AddableMetric, RawMetric};
use netstack3_core::testutil::{CtxPairExt as _, FakeCtx, FakeCtxBuilder};
use netstack3_port_spike::{EthernetDevice as _, EthernetFrame, FakeEthernetDevice};
use packet::{Buf, InnerPacketBuilder as _, NestableSerializer as _, Serializer as _};
use packet_formats::arp::{ArpOp, ArpPacketBuilder};
use packet_formats::ethernet::{ETHERNET_MIN_BODY_LEN_NO_TAG, EtherType, EthernetFrameBuilder};

const LOCAL_MAC: Mac = net_mac!("22:33:44:55:66:77");
const PEER_MAC: Mac = net_mac!("88:88:88:88:88:88");
const LOCAL_IP: net_types::ip::Ipv4Addr = net_ip_v4!("192.0.2.1");
const PEER_IP: net_types::ip::Ipv4Addr = net_ip_v4!("192.0.2.2");

fn arp_response() -> Vec<u8> {
    ArpPacketBuilder::new(ArpOp::Response, PEER_MAC, PEER_IP, LOCAL_MAC, LOCAL_IP)
        .into_serializer()
        .wrap_in(EthernetFrameBuilder::new(
            PEER_MAC,
            LOCAL_MAC,
            EtherType::Arp,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        ))
        .serialize_vec_outer(&mut NetworkSerializationContext::default())
        .unwrap()
        .unwrap_b()
        .into_inner()
}

#[test]
fn bounded_device_drives_real_netstack3_arp_routing_and_udp() {
    let mut builder = FakeCtxBuilder::default();
    let index = builder.add_device_with_ip(
        UnicastAddr::new(LOCAL_MAC).unwrap(),
        LOCAL_IP,
        net_subnet_v4!("192.0.2.1/32"),
    );
    let (
        FakeCtx {
            core_ctx,
            bindings_ctx,
        },
        devices,
    ) = builder.build();
    let device_id = devices.into_iter().nth(index).unwrap();
    let mut ctx = CtxPair {
        core_ctx,
        bindings_ctx,
    };
    ctx.test_api()
        .add_route(
            AddableEntry::with_gateway(
                net_subnet_v4!("192.0.2.0/24"),
                device_id.clone().into(),
                SpecifiedAddr::new(PEER_IP).unwrap(),
                AddableMetric::ExplicitMetric(RawMetric(0)),
            )
            .into(),
        )
        .unwrap();

    let socket = {
        let mut udp = ctx.core_api().udp::<Ipv4>();
        let socket = udp.create();
        udp.listen(
            &socket,
            SpecifiedAddr::new(LOCAL_IP).map(|a| ZonedAddr::Unzoned(a).into()),
            Some(NonZeroU16::new(22222).unwrap()),
        )
        .unwrap();
        udp.set_device(&socket, Some(&device_id.clone().into()))
            .unwrap();
        udp.send_to(
            &socket,
            Some(ZonedAddr::Unzoned(SpecifiedAddr::new(PEER_IP).unwrap()).into()),
            NonZeroU16::new(33333).unwrap().into(),
            Buf::new(b"netstack3".to_vec(), ..),
        )
        .unwrap();
        socket
    };

    let mut port = FakeEthernetDevice::new(4);
    let frames = ctx.bindings_ctx.take_ethernet_frames();
    assert_eq!(frames.len(), 1, "send must start ARP resolution");
    port.transmit(EthernetFrame::try_from(frames[0].1.clone()).unwrap())
        .unwrap();
    let arp_request = port.take_transmitted().unwrap();
    assert_eq!(&arp_request.as_bytes()[12..14], &[0x08, 0x06]);

    port.inject(EthernetFrame::try_from(arp_response()).unwrap())
        .unwrap();
    let ingress = port.receive().unwrap();
    ctx.core_api().device::<EthernetLinkDevice>().receive_frame(
        RecvEthernetFrameMeta {
            device_id: device_id.clone(),
            parsing_context: NetworkParsingContext::default(),
        },
        Buf::new(ingress.into_vec(), ..),
    );

    let frames = ctx.bindings_ctx.take_ethernet_frames();
    assert_eq!(frames.len(), 1, "ARP completion must flush queued UDP");
    port.transmit(EthernetFrame::try_from(frames[0].1.clone()).unwrap())
        .unwrap();
    let udp = port.take_transmitted().unwrap();
    assert_eq!(&udp.as_bytes()[0..6], PEER_MAC.bytes());
    assert_eq!(&udp.as_bytes()[6..12], LOCAL_MAC.bytes());
    assert_eq!(&udp.as_bytes()[12..14], &[0x08, 0x00]);
    assert!(
        udp.as_bytes()
            .windows(b"netstack3".len())
            .any(|w| w == b"netstack3")
    );

    let _removed = ctx.core_api().udp::<Ipv4>().close(socket);
    drop(_removed);
    ctx.test_api().clear_routes_and_remove_device(device_id);
}

#[test]
fn bounded_device_drives_real_netstack3_ipv4_icmp_echo() {
    use packet_formats::icmp::{IcmpEchoRequest, IcmpPacketBuilder, IcmpZeroCode};
    use packet_formats::ip::Ipv4Proto;
    use packet_formats::ipv4::Ipv4PacketBuilder;

    let mut builder = FakeCtxBuilder::default();
    let index = builder.add_device_with_ip(
        UnicastAddr::new(LOCAL_MAC).unwrap(),
        LOCAL_IP,
        net_subnet_v4!("192.0.2.0/24"),
    );
    builder.add_arp_table_entry(
        index,
        SpecifiedAddr::new(PEER_IP).unwrap(),
        UnicastAddr::new(PEER_MAC).unwrap(),
    );
    let (mut ctx, devices) = builder.build();
    let device_id = devices.into_iter().nth(index).unwrap();

    let request = Buf::new(b"icmp-through-owned-frame".to_vec(), ..)
        .wrap_in(IcmpPacketBuilder::<Ipv4, _>::new(
            PEER_IP,
            LOCAL_IP,
            IcmpZeroCode,
            IcmpEchoRequest::new(7, 11),
        ))
        .wrap_in(Ipv4PacketBuilder::new(
            PEER_IP,
            LOCAL_IP,
            64,
            Ipv4Proto::Icmp,
        ))
        .wrap_in(EthernetFrameBuilder::new(
            PEER_MAC,
            LOCAL_MAC,
            EtherType::Ipv4,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        ))
        .serialize_vec_outer(&mut NetworkSerializationContext::default())
        .unwrap()
        .unwrap_b()
        .into_inner();

    let mut port = FakeEthernetDevice::new(2);
    port.inject(EthernetFrame::try_from(request).unwrap())
        .unwrap();
    let ingress = port.receive().unwrap();
    ctx.core_api().device::<EthernetLinkDevice>().receive_frame(
        RecvEthernetFrameMeta {
            device_id: device_id.clone(),
            parsing_context: NetworkParsingContext::default(),
        },
        Buf::new(ingress.into_vec(), ..),
    );

    let mut frames = ctx.bindings_ctx.take_ethernet_frames();
    assert_eq!(frames.len(), 1);
    port.transmit(EthernetFrame::try_from(frames.pop().unwrap().1).unwrap())
        .unwrap();
    let reply = port.take_transmitted().unwrap();
    let bytes = reply.as_bytes();
    assert_eq!(&bytes[0..6], PEER_MAC.bytes());
    assert_eq!(&bytes[6..12], LOCAL_MAC.bytes());
    assert_eq!(&bytes[12..14], &[0x08, 0x00]);
    assert_eq!(bytes[23], 1, "IPv4 protocol must be ICMP");
    assert_eq!(bytes[34], 0, "ICMP message must be echo reply");
    assert!(bytes.windows(24).any(|w| w == b"icmp-through-owned-frame"));

    ctx.test_api().clear_routes_and_remove_device(device_id);
}

#[test]
fn real_netstack3_tcp_completes_handshake_and_transfers_payload() {
    use std::num::NonZeroUsize;

    use netstack3_core::device::LoopbackDevice;
    use netstack3_core::types::WorkQueueReport;
    use netstack3_tcp::testutil::{ProvidedBuffers, WriteBackClientBuffers};

    fn drain_loopback(
        ctx: &mut netstack3_core::testutil::FakeCtx,
        lo: &netstack3_core::device::LoopbackDeviceId<netstack3_core::testutil::FakeBindingsCtx>,
    ) {
        loop {
            let wakes = core::mem::take(&mut ctx.bindings_ctx.state_mut().rx_available);
            if wakes.is_empty() {
                break;
            }
            assert_eq!(
                ctx.core_api()
                    .receive_queue::<LoopbackDevice>()
                    .handle_queued_frames(lo),
                WorkQueueReport::AllDone
            );
        }
    }

    let mut ctx = netstack3_core::testutil::FakeCtx::default();
    let lo = ctx.test_api().add_loopback();
    let port = NonZeroU16::new(4040).unwrap();
    let server = ctx
        .core_api()
        .tcp::<Ipv4>()
        .create(ProvidedBuffers::Buffers(WriteBackClientBuffers::default()));
    ctx.core_api()
        .tcp::<Ipv4>()
        .bind(&server, None, Some(port))
        .unwrap();
    ctx.core_api()
        .tcp::<Ipv4>()
        .listen(&server, NonZeroUsize::new(1).unwrap())
        .unwrap();

    let client = ctx
        .core_api()
        .tcp::<Ipv4>()
        .create(ProvidedBuffers::Buffers(WriteBackClientBuffers::default()));
    ctx.core_api()
        .tcp::<Ipv4>()
        .connect(
            &client,
            Some(ZonedAddr::Unzoned(
                SpecifiedAddr::new(net_declare::net_ip_v4!("127.0.0.1")).unwrap(),
            )),
            port,
        )
        .unwrap();
    drain_loopback(&mut ctx, &lo);

    let (accepted, _, _) = ctx.core_api().tcp::<Ipv4>().accept(&server).unwrap();
    ctx.core_api()
        .tcp::<Ipv4>()
        .with_send_buffer(&client, |buffer| buffer.enqueue_data(b"tcp-payload"))
        .unwrap();
    ctx.core_api().tcp::<Ipv4>().do_send(&client);
    drain_loopback(&mut ctx, &lo);

    let mut received = Vec::new();
    ctx.core_api()
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
    assert_eq!(received, b"tcp-payload");

    ctx.core_api().tcp::<Ipv4>().close(accepted);
    ctx.core_api().tcp::<Ipv4>().close(client);
    ctx.core_api().tcp::<Ipv4>().close(server);
    ctx.bindings_ctx.state_mut().rx_available.clear();
    ctx.test_api().clear_routes_and_remove_device(lo);
}

#[test]
fn bounded_device_drives_real_netstack3_ipv6_ndp_routing_and_udp() {
    use net_types::ip::Ipv6;
    use netstack3_ip::icmp::testutil::neighbor_advertisement_ip_packet;

    const LOCAL_V6: net_types::ip::Ipv6Addr = net_declare::net_ip_v6!("2001:db8::1");
    const PEER_V6: net_types::ip::Ipv6Addr = net_declare::net_ip_v6!("2001:db8::2");

    let mut builder = FakeCtxBuilder::default();
    let index = builder.add_device_with_ip(
        UnicastAddr::new(LOCAL_MAC).unwrap(),
        LOCAL_V6,
        net_declare::net_subnet_v6!("2001:db8::1/128"),
    );
    let (
        FakeCtx {
            core_ctx,
            bindings_ctx,
        },
        devices,
    ) = builder.build();
    let device_id = devices.into_iter().nth(index).unwrap();
    let mut ctx = CtxPair {
        core_ctx,
        bindings_ctx,
    };
    ctx.test_api()
        .add_route(
            AddableEntry::with_gateway(
                net_declare::net_subnet_v6!("2001:db8::/64"),
                device_id.clone().into(),
                SpecifiedAddr::new(PEER_V6).unwrap(),
                AddableMetric::ExplicitMetric(RawMetric(0)),
            )
            .into(),
        )
        .unwrap();

    let socket = {
        let mut udp = ctx.core_api().udp::<Ipv6>();
        let socket = udp.create();
        udp.listen(
            &socket,
            SpecifiedAddr::new(LOCAL_V6).map(|a| ZonedAddr::Unzoned(a).into()),
            Some(NonZeroU16::new(22222).unwrap()),
        )
        .unwrap();
        udp.set_device(&socket, Some(&device_id.clone().into()))
            .unwrap();
        udp.send_to(
            &socket,
            Some(ZonedAddr::Unzoned(SpecifiedAddr::new(PEER_V6).unwrap()).into()),
            NonZeroU16::new(33333).unwrap().into(),
            Buf::new(b"ipv6-udp".to_vec(), ..),
        )
        .unwrap();
        socket
    };

    let mut port = FakeEthernetDevice::new(4);
    let mut frames = ctx.bindings_ctx.take_ethernet_frames();
    assert_eq!(frames.len(), 1, "send must start NDP resolution");
    port.transmit(EthernetFrame::try_from(frames.pop().unwrap().1).unwrap())
        .unwrap();
    let solicitation = port.take_transmitted().unwrap();
    assert_eq!(&solicitation.as_bytes()[12..14], &[0x86, 0xdd]);
    assert_eq!(
        solicitation.as_bytes()[54],
        135,
        "must emit neighbor solicitation"
    );

    let confirmation =
        neighbor_advertisement_ip_packet(PEER_V6, LOCAL_V6, false, true, false, PEER_MAC)
            .wrap_in(EthernetFrameBuilder::new(
                PEER_MAC,
                LOCAL_MAC,
                EtherType::Ipv6,
                ETHERNET_MIN_BODY_LEN_NO_TAG,
            ))
            .serialize_vec_outer(&mut NetworkSerializationContext::default())
            .unwrap()
            .unwrap_b()
            .into_inner();
    port.inject(EthernetFrame::try_from(confirmation).unwrap())
        .unwrap();
    let ingress = port.receive().unwrap();
    ctx.core_api().device::<EthernetLinkDevice>().receive_frame(
        RecvEthernetFrameMeta {
            device_id: device_id.clone(),
            parsing_context: NetworkParsingContext::default(),
        },
        Buf::new(ingress.into_vec(), ..),
    );

    let mut frames = ctx.bindings_ctx.take_ethernet_frames();
    assert_eq!(frames.len(), 1, "NDP completion must flush queued UDP");
    port.transmit(EthernetFrame::try_from(frames.pop().unwrap().1).unwrap())
        .unwrap();
    let udp = port.take_transmitted().unwrap();
    assert_eq!(&udp.as_bytes()[0..6], PEER_MAC.bytes());
    assert_eq!(&udp.as_bytes()[6..12], LOCAL_MAC.bytes());
    assert_eq!(&udp.as_bytes()[12..14], &[0x86, 0xdd]);
    assert!(udp.as_bytes().windows(8).any(|w| w == b"ipv6-udp"));

    let removed = ctx.core_api().udp::<Ipv6>().close(socket);
    drop(removed);
    ctx.test_api().clear_routes_and_remove_device(device_id);
}
