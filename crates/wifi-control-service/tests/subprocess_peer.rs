// SPDX-License-Identifier: GPL-2.0-only

use fidl_fuchsia_wlan_common::ScanType;
use fidl_fuchsia_wlan_ieee80211 as ieee;
use fidl_fuchsia_wlan_internal as internal;
use fidl_fuchsia_wlan_sme as sme;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::thread::sleep;
use std::time::{Duration, Instant};
use wifi_control_service::UnixSeqpacketEndpoint;
use wifi_supervisor_wire::{LifecycleKind, LifecycleMessage};
use wlan_control_wire::{CommandReply, ConnectReply, GenerationEndReason, Message, Packet};

const GENERATION: [u8; 16] = [37; 16];

fn bss() -> ieee::BssDescription {
    let channel = |number| ieee::ChannelNumber {
        band: ieee::WlanBand::FiveGhz,
        number,
    };
    ieee::BssDescription {
        bssid: [1, 2, 3, 4, 5, 6],
        bss_type: ieee::BssType::Infrastructure,
        beacon_period: 100,
        capability_info: 0x1234,
        ies: vec![0, 2, b'a', b'p'],
        primary: channel(36),
        bandwidth: ieee::ChannelBandwidth::Cbw80,
        vht_secondary_80_channel: channel(0),
        rssi_dbm: -42,
        snr_db: 31,
    }
}

fn connect(ssid: &[u8]) -> sme::ConnectRequest {
    sme::ConnectRequest {
        ssid: ssid.to_vec(),
        bss_description: bss(),
        multiple_bss_candidates: true,
        authentication: internal::Authentication {
            protocol: internal::Protocol::Open,
            credentials: None,
        },
        deprecated_scan_type: ScanType::Active,
    }
}

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

fn spawn_peer(policy_fd: &OwnedFd, supervisor_fd: &OwnedFd) -> Child {
    const CHILD_POLICY_FD: i32 = 3;
    const CHILD_SUPERVISOR_FD: i32 = 4;
    let policy_raw = policy_fd.as_raw_fd();
    let supervisor_raw = supervisor_fd.as_raw_fd();
    let mut command = Command::new(env!("CARGO_BIN_EXE_wifi-control-simulated-peer"));
    command
        .arg(CHILD_POLICY_FD.to_string())
        .arg(CHILD_SUPERVISOR_FD.to_string())
        .arg(GENERATION[0].to_string());
    unsafe {
        command.pre_exec(move || {
            for (source, target) in [
                (policy_raw, CHILD_POLICY_FD),
                (supervisor_raw, CHILD_SUPERVISOR_FD),
            ] {
                if source != target && libc::dup2(source, target) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let flags = libc::fcntl(target, libc::F_GETFD);
                if flags < 0 || libc::fcntl(target, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command.spawn().expect("spawn simulated Wi-Fi peer")
}

fn send(endpoint: &UnixSeqpacketEndpoint, id: u64, message: Message) {
    let packet = Packet {
        generation: GENERATION,
        request_id: id,
        message,
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while !endpoint.try_send_packet(&packet, None).unwrap() {
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(1));
    }
}

fn receive(endpoint: &UnixSeqpacketEndpoint) -> wifi_control_service::ReceivedPacket {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(packet) = endpoint.try_receive_packet().unwrap() {
            return packet;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for peer packet"
        );
        sleep(Duration::from_millis(1));
    }
}

fn receive_supervisor(fd: &OwnedFd) -> ([u8; 40], Vec<OwnedFd>) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut bytes = [0u8; 40];
        let mut control = [0usize; 4];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = std::mem::size_of_val(&control);
        let received = unsafe { libc::recvmsg(fd.as_raw_fd(), &mut header, libc::MSG_DONTWAIT) };
        if received < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                assert!(Instant::now() < deadline);
                sleep(Duration::from_millis(1));
                continue;
            }
            panic!("supervisor recvmsg: {error}");
        }
        assert_eq!(received, 40);
        assert_eq!(header.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC), 0);
        let mut fds = Vec::new();
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&header);
            while !cmsg.is_null() {
                assert_eq!((*cmsg).cmsg_level, libc::SOL_SOCKET);
                assert_eq!((*cmsg).cmsg_type, libc::SCM_RIGHTS);
                let count =
                    ((*cmsg).cmsg_len - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<i32>();
                let data = libc::CMSG_DATA(cmsg).cast::<i32>();
                for index in 0..count {
                    fds.push(OwnedFd::from_raw_fd(*data.add(index)));
                }
                cmsg = libc::CMSG_NXTHDR(&header, cmsg);
            }
        }
        return (bytes, fds);
    }
}

#[test]
fn real_subprocess_simulated_peer_exercises_control_contract() {
    let (client_fd, child_fd) = pair();
    let (supervisor_fd, child_supervisor_fd) = pair();
    let mut child = spawn_peer(&child_fd, &child_supervisor_fd);
    drop(child_fd);
    drop(child_supervisor_fd);
    let endpoint = UnixSeqpacketEndpoint::from_inherited_fd(client_fd).unwrap();
    let supervisor = supervisor_fd;

    let ready = receive(&endpoint);
    assert!(matches!(ready.packet.message, Message::Ready));
    assert!(ready.fds.is_empty());

    // Requests overlap; their replies correlate to incoming ids rather than
    // the server's independently increasing outgoing packet sequence.
    send(
        &endpoint,
        1,
        Message::Scan(sme::ScanRequest::Passive(sme::PassiveScanRequest {
            channels: vec![],
        })),
    );
    send(&endpoint, 2, Message::Connect(connect(b"fail")));
    let mut saw_scan = false;
    let mut saw_exact_failure = false;
    for _ in 0..2 {
        match receive(&endpoint).packet.message {
            Message::ScanReply(reply) => {
                assert_eq!(reply.in_reply_to, 1);
                assert_eq!(reply.result, Ok(vec![]));
                saw_scan = true;
            }
            Message::ConnectReply(reply) => {
                assert_eq!(reply.in_reply_to, 2);
                let ConnectReply::Completed(result) = reply.result;
                assert_eq!(result.code, ieee::StatusCode::RefusedReasonUnspecified);
                assert!(result.is_credential_rejected);
                saw_exact_failure = true;
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }
    assert!(saw_scan && saw_exact_failure);

    // The exact SME failure was retry-safe: the same generation accepts a
    // fresh attempt, emits its event, and transfers Ethernet exactly once.
    send(&endpoint, 3, Message::Connect(connect(b"retry")));
    assert!(matches!(
        receive(&endpoint).packet.message,
        Message::ConnectReply(ref reply)
            if reply.in_reply_to == 3
                && matches!(reply.result, ConnectReply::Completed(sme::ConnectResult {
                    code: ieee::StatusCode::Success,
                    ..
                }))
    ));
    assert!(matches!(
        receive(&endpoint).packet.message,
        Message::Event(sme::ConnectTransactionEvent::OnConnectResult {
            result: sme::ConnectResult {
                code: ieee::StatusCode::Success,
                ..
            }
        })
    ));
    let (ethernet_record, ethernet_fds) = receive_supervisor(&supervisor);
    assert_eq!(
        LifecycleMessage::decode(&ethernet_record).unwrap(),
        LifecycleMessage {
            kind: LifecycleKind::Install,
            wifi_generation: GENERATION,
            ethernet_generation: 1,
            mac_address: [2, 4, 6, 8, 10, 12],
        }
    );
    assert_eq!(ethernet_fds.len(), 1);
    assert!(
        endpoint.try_receive_packet().unwrap().is_none(),
        "policy socket received an extra packet"
    );

    send(
        &endpoint,
        4,
        Message::Roam(sme::RoamRequest {
            bss_description: bss(),
        }),
    );
    assert!(
        matches!(receive(&endpoint).packet.message, Message::RoamReply(ref reply) if reply.in_reply_to == 4 && reply.result == CommandReply::Success)
    );

    send(&endpoint, 5, Message::Connect(connect(b"hold")));
    send(
        &endpoint,
        6,
        Message::Scan(sme::ScanRequest::Passive(sme::PassiveScanRequest {
            channels: vec![],
        })),
    );
    assert!(
        matches!(receive(&endpoint).packet.message, Message::ScanReply(ref reply) if reply.in_reply_to == 6 && reply.result == Ok(vec![]))
    );
    send(
        &endpoint,
        7,
        Message::Disconnect(sme::UserDisconnectReason::NetworkUnsaved),
    );
    let first = receive(&endpoint).packet.message;
    let second = receive(&endpoint).packet.message;
    assert!(
        matches!(first, Message::ConnectReply(ref reply) if reply.in_reply_to == 5 && matches!(reply.result, ConnectReply::Completed(sme::ConnectResult { code: ieee::StatusCode::Canceled, .. })))
    );
    assert!(
        matches!(second, Message::DisconnectReply(ref reply) if reply.in_reply_to == 7 && reply.result == CommandReply::Success)
    );

    // A server-only message from the client is a terminal protocol violation.
    send(&endpoint, 8, Message::Ready);
    let ended = receive(&endpoint);
    assert!(ended.fds.is_empty());
    assert!(matches!(
        ended.packet.message,
        Message::GenerationEnd(GenerationEndReason::ProtocolViolation)
    ));
    drop(endpoint);
    drop(supervisor);
    assert!(child.wait().unwrap().success());
}

#[test]
fn stale_failed_attempt_event_terminates_without_a_failure_reply() {
    let (client_fd, child_fd) = pair();
    let (supervisor_fd, child_supervisor_fd) = pair();
    let mut child = spawn_peer(&child_fd, &child_supervisor_fd);
    drop(child_fd);
    drop(child_supervisor_fd);
    let endpoint = UnixSeqpacketEndpoint::from_inherited_fd(client_fd).unwrap();
    assert!(matches!(receive(&endpoint).packet.message, Message::Ready));
    send(&endpoint, 1, Message::Connect(connect(b"stale")));
    assert!(matches!(
        receive(&endpoint).packet.message,
        Message::GenerationEnd(GenerationEndReason::DriverFault)
    ));
    assert!(
        endpoint.try_receive_packet().is_err() || endpoint.try_receive_packet().unwrap().is_none()
    );
    drop(endpoint);
    drop(supervisor_fd);
    assert!(child.wait().unwrap().success());
}

#[test]
fn runtime_faults_are_sole_terminal_generation_messages() {
    for (ssid, reason) in [
        (b"timeout".as_slice(), GenerationEndReason::Timeout),
        (b"driver".as_slice(), GenerationEndReason::DriverFault),
        (
            b"containment".as_slice(),
            GenerationEndReason::ContainmentFault,
        ),
    ] {
        let (client_fd, child_fd) = pair();
        let (supervisor_fd, child_supervisor_fd) = pair();
        let mut child = spawn_peer(&child_fd, &child_supervisor_fd);
        drop(child_fd);
        drop(child_supervisor_fd);
        let endpoint = UnixSeqpacketEndpoint::from_inherited_fd(client_fd).unwrap();
        assert!(matches!(receive(&endpoint).packet.message, Message::Ready));
        send(&endpoint, 1, Message::Connect(connect(ssid)));
        assert!(
            matches!(receive(&endpoint).packet.message, Message::GenerationEnd(actual) if actual == reason)
        );
        drop(endpoint);
        drop(supervisor_fd);
        assert!(child.wait().unwrap().success());
    }
}
