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
use packet_formats::ethernet::{
    ETHERNET_MIN_BODY_LEN_NO_TAG, EtherType, EthernetFrameBuilder,
};

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
    let (FakeCtx { core_ctx, bindings_ctx }, devices) = builder.build();
    let device_id = devices.into_iter().nth(index).unwrap();
    let mut ctx = CtxPair { core_ctx, bindings_ctx };
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
        udp.set_device(&socket, Some(&device_id.clone().into())).unwrap();
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
    port.transmit(EthernetFrame::try_from(frames[0].1.clone()).unwrap()).unwrap();
    let arp_request = port.take_transmitted().unwrap();
    assert_eq!(&arp_request.as_bytes()[12..14], &[0x08, 0x06]);

    port.inject(EthernetFrame::try_from(arp_response()).unwrap()).unwrap();
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
    port.transmit(EthernetFrame::try_from(frames[0].1.clone()).unwrap()).unwrap();
    let udp = port.take_transmitted().unwrap();
    assert_eq!(&udp.as_bytes()[0..6], PEER_MAC.bytes());
    assert_eq!(&udp.as_bytes()[6..12], LOCAL_MAC.bytes());
    assert_eq!(&udp.as_bytes()[12..14], &[0x08, 0x00]);
    assert!(udp.as_bytes().windows(b"netstack3".len()).any(|w| w == b"netstack3"));

    let _removed = ctx.core_api().udp::<Ipv4>().close(socket);
    drop(_removed);
    ctx.test_api().clear_routes_and_remove_device(device_id);
}
