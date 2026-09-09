// SPDX-License-Identifier: GPL-2.0-only

use fidl_fuchsia_wlan_ieee80211 as ieee;
use fidl_fuchsia_wlan_sme as sme;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use wifi_control_service::{
    EndpointError, PreparedServer, SimulatedWifiRuntime, UnixSeqpacketEndpoint,
};
use wlan_control_wire::{Message, Packet, encode};

const GENERATION: [u8; 16] = [8; 16];

fn pair() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1; 2];
    assert_eq!(
        unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        },
        0
    );
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

#[test]
fn setup_gate_cannot_receive_before_post_lockdown_transition() {
    let (server_fd, peer_fd) = pair();
    let peer = UnixSeqpacketEndpoint::from_inherited_fd(peer_fd).unwrap();
    let (supervisor_server, _supervisor_peer) = pair();
    let prepared = PreparedServer::new(
        server_fd,
        supervisor_server,
        GENERATION,
        SimulatedWifiRuntime::new([2, 0, 0, 0, 0, 1]),
    )
    .unwrap();
    assert!(peer.try_receive_packet().unwrap().is_none());
    let mut server = prepared.post_lockdown_open_complete().unwrap();
    futures::executor::block_on(server.drive_once()).unwrap();
    assert!(matches!(
        peer.try_receive_packet().unwrap().unwrap().packet.message,
        Message::Ready
    ));
}

#[test]
fn endpoint_rejects_non_seqpacket_and_truncated_packet() {
    let (stream, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(matches!(
        UnixSeqpacketEndpoint::from_inherited_fd(stream.into()),
        Err(EndpointError::NotSeqpacket)
    ));

    let (receiver, sender) = pair();
    let endpoint = UnixSeqpacketEndpoint::from_inherited_fd(receiver).unwrap();
    let oversized = vec![0u8; wlan_control_wire::MAX_PACKET + 1];
    assert_eq!(
        unsafe {
            libc::send(
                sender.as_raw_fd(),
                oversized.as_ptr().cast(),
                oversized.len(),
                libc::MSG_NOSIGNAL,
            )
        },
        oversized.len() as isize
    );
    assert!(matches!(
        endpoint.try_receive_packet(),
        Err(EndpointError::TruncatedPacket)
    ));
}

#[test]
fn endpoint_rejects_truncated_rights() {
    let (receiver, sender) = pair();
    let endpoint = UnixSeqpacketEndpoint::from_inherited_fd(receiver).unwrap();
    let ready = encode(&Packet {
        generation: GENERATION,
        request_id: 2,
        message: Message::Ready,
    })
    .unwrap();
    send_many_fds(sender.as_raw_fd(), &ready, 32);
    assert!(matches!(
        endpoint.try_receive_packet(),
        Err(EndpointError::TruncatedAncillary)
    ));
}

#[test]
fn policy_endpoint_rejects_rights_without_installing_them() {
    let (receiver, sender) = pair();
    let endpoint = UnixSeqpacketEndpoint::from_inherited_fd(receiver).unwrap();
    let ready = encode(&Packet {
        generation: GENERATION,
        request_id: 2,
        message: Message::Ready,
    })
    .unwrap();
    let (capability, peer) = pair();
    send_fds(sender.as_raw_fd(), &ready, &[capability.as_raw_fd()]);
    drop(capability);

    assert!(matches!(
        endpoint.try_receive_packet(),
        Err(EndpointError::TruncatedAncillary)
    ));
    let mut byte = 0u8;
    assert_eq!(
        unsafe {
            libc::recv(
                peer.as_raw_fd(),
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_DONTWAIT,
            )
        },
        0,
        "rejected SCM_RIGHTS descriptor remained installed"
    );
}

#[test]
fn prepared_server_rejects_aliased_policy_and_supervisor_endpoints() {
    let (policy, _peer) = pair();
    let alias = unsafe { libc::dup(policy.as_raw_fd()) };
    assert!(alias >= 0);
    let result = PreparedServer::new(
        policy,
        unsafe { OwnedFd::from_raw_fd(alias) },
        GENERATION,
        SimulatedWifiRuntime::new([2, 0, 0, 0, 0, 1]),
    );
    assert!(matches!(result, Err(EndpointError::AliasedEndpoints)));
}

fn send_many_fds(socket: i32, bytes: &[u8], count: usize) {
    send_fds(socket, bytes, &vec![socket; count]);
}

fn send_fds(socket: i32, bytes: &[u8], fds: &[i32]) {
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let control_bytes = unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) } as usize;
    let words = control_bytes.div_ceil(size_of::<usize>());
    let mut control = vec![0usize; words];
    let mut header: libc::msghdr = unsafe { zeroed() };
    header.msg_iov = &mut iov;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = control_bytes;
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&header);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as usize;
        let data = libc::CMSG_DATA(cmsg).cast::<i32>();
        for (index, fd) in fds.iter().enumerate() {
            *data.add(index) = *fd;
        }
        assert_eq!(
            libc::sendmsg(socket, &header, libc::MSG_NOSIGNAL),
            bytes.len() as isize
        );
    }
}

#[test]
fn outbound_backpressure_preserves_queued_packets_then_ends_generation() {
    let (server_fd, peer_fd) = pair();
    let (supervisor_server, _supervisor_peer) = pair();
    let peer = UnixSeqpacketEndpoint::from_inherited_fd(peer_fd).unwrap();
    let result = sme::ConnectResult {
        code: ieee::StatusCode::Success,
        is_credential_rejected: false,
        is_reconnect: false,
    };
    let mut runtime = SimulatedWifiRuntime::new([2, 0, 0, 0, 0, 1]);
    for _ in 0..64 {
        runtime.inject_connection_event(sme::ConnectTransactionEvent::OnConnectResult { result });
    }
    let mut server = PreparedServer::new(server_fd, supervisor_server, GENERATION, runtime)
        .unwrap()
        .post_lockdown_open_complete()
        .unwrap();
    futures::executor::block_on(server.drive_once()).unwrap();
    let mut events = 0;
    loop {
        let received = peer.try_receive_packet().unwrap().unwrap();
        match received.packet.message {
            Message::Ready => {}
            Message::Event(_) => events += 1,
            Message::GenerationEnd(wlan_control_wire::GenerationEndReason::Backpressure) => break,
            other => panic!("unexpected queued packet: {other:?}"),
        }
    }
    assert_eq!(events, 63);
}
