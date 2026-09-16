// SPDX-License-Identifier: GPL-2.0-only

//! Private supervisor/provider Ethernet capability transfer.
//!
//! The channel is an inherited connected `SOCK_SEQPACKET`; it is not exposed
//! to policy or applications. Requests and acknowledgements are generation
//! tagged so an old or replayed capability cannot become current.

use rustix::fd::{BorrowedFd, OwnedFd};
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
    SendAncillaryBuffer, SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};
use std::io::{IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::RawFd;

pub(crate) const CONTROL_FD: RawFd = 8;
pub(crate) const FRAME_RESERVATION_FD: RawFd = 9;
const MAGIC: [u8; 4] = *b"DLNK";
const VERSION: u8 = 1;
const MESSAGE_LEN: usize = 16;

#[derive(Debug)]
pub(crate) enum Request {
    Attach { generation: u64, frame: OwnedFd },
    Detach { generation: u64 },
    Watch { generation: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AckStatus {
    Applied,
    Rejected,
}

fn encode(kind: u8, status: u8, generation: u64) -> [u8; MESSAGE_LEN] {
    let mut bytes = [0; MESSAGE_LEN];
    bytes[..4].copy_from_slice(&MAGIC);
    bytes[4] = VERSION;
    bytes[5] = kind;
    bytes[6] = status;
    bytes[8..].copy_from_slice(&generation.to_le_bytes());
    bytes
}

fn decode(bytes: &[u8]) -> Result<(u8, u8, u64), String> {
    if bytes.len() != MESSAGE_LEN
        || bytes[..4] != MAGIC
        || bytes[4] != VERSION
        || bytes[7] != 0
    {
        return Err("malformed link-control record".into());
    }
    Ok((
        bytes[5],
        bytes[6],
        u64::from_le_bytes(bytes[8..].try_into().unwrap()),
    ))
}

pub(crate) fn send_attach(
    channel: BorrowedFd<'_>,
    generation: u64,
    frame: BorrowedFd<'_>,
) -> Result<(), String> {
    let bytes = encode(1, 0, generation);
    let iov = [IoSlice::new(&bytes)];
    let rights = [frame];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if !ancillary.push(SendAncillaryMessage::ScmRights(&rights)) {
        return Err("construct link-control descriptor transfer".into());
    }
    match sendmsg(channel, &iov, &mut ancillary, SendFlags::DONTWAIT | SendFlags::NOSIGNAL) {
        Ok(MESSAGE_LEN) => Ok(()),
        Ok(_) => Err("partial link-control request".into()),
        Err(error) => Err(format!("send link-control attach: {error}")),
    }
}

pub(crate) fn send_detach(channel: BorrowedFd<'_>, generation: u64) -> Result<(), String> {
    let bytes = encode(2, 0, generation);
    let iov = [IoSlice::new(&bytes)];
    let mut ancillary = SendAncillaryBuffer::default();
    match sendmsg(channel, &iov, &mut ancillary, SendFlags::DONTWAIT | SendFlags::NOSIGNAL) {
        Ok(MESSAGE_LEN) => Ok(()),
        Ok(_) => Err("partial link-control request".into()),
        Err(error) => Err(format!("send link-control detach: {error}")),
    }
}

pub(crate) fn receive_request(channel: BorrowedFd<'_>) -> Result<Option<Request>, String> {
    let mut bytes = [0; MESSAGE_LEN];
    let mut iov = [IoSliceMut::new(&mut bytes)];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let message = match recvmsg(
        channel,
        &mut iov,
        &mut ancillary,
        RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC,
    ) {
        Ok(message) => message,
        Err(rustix::io::Errno::AGAIN) => return Ok(None),
        Err(error) => return Err(format!("receive link-control request: {error}")),
    };
    if message.bytes == 0 {
        return Err("link-control channel closed".into());
    }
    if message.flags.intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC) {
        return Err("truncated link-control record".into());
    }
    let mut descriptors = Vec::new();
    for message in ancillary.drain() {
        match message {
            RecvAncillaryMessage::ScmRights(rights) => descriptors.extend(rights),
            _ => return Err("unknown link-control ancillary data".into()),
        }
    }
    let (kind, status, generation) = decode(&bytes[..message.bytes])?;
    if status != 0 || generation == 0 {
        return Err("invalid link-control request fields".into());
    }
    match kind {
        1 if descriptors.len() == 1 => Ok(Some(Request::Attach {
            generation,
            frame: descriptors.pop().unwrap(),
        })),
        2 if descriptors.is_empty() => Ok(Some(Request::Detach { generation })),
        4 if descriptors.is_empty() => Ok(Some(Request::Watch { generation })),
        1 => Err("link-control attach requires exactly one descriptor".into()),
        2 => Err("link-control detach must not carry descriptors".into()),
        _ => Err("unknown link-control request".into()),
    }
}

#[cfg(test)]
pub(crate) fn send_ack(channel: BorrowedFd<'_>, generation: u64, status: AckStatus) -> Result<(), String> {
    if try_send_ack(channel, generation, status)? { Ok(()) }
    else { Err("link-control acknowledgement backpressure".into()) }
}

pub(crate) fn try_send_ack(
    channel: BorrowedFd<'_>,
    generation: u64,
    status: AckStatus,
) -> Result<bool, String> {
    let bytes = encode(
        3,
        match status {
            AckStatus::Applied => 1,
            AckStatus::Rejected => 2,
        },
        generation,
    );
    let iov = [IoSlice::new(&bytes)];
    let mut ancillary = SendAncillaryBuffer::default();
    match sendmsg(channel, &iov, &mut ancillary, SendFlags::DONTWAIT | SendFlags::NOSIGNAL) {
        Ok(MESSAGE_LEN) => Ok(true),
        Ok(_) => Err("partial link-control acknowledgement".into()),
        Err(rustix::io::Errno::AGAIN) => Ok(false),
        Err(error) => Err(format!("send link-control acknowledgement: {error}")),
    }
}

pub(crate) fn receive_ack(channel: BorrowedFd<'_>, generation: u64) -> Result<AckStatus, String> {
    let mut bytes = [0; MESSAGE_LEN];
    let mut iov = [IoSliceMut::new(&mut bytes)];
    let mut ancillary = RecvAncillaryBuffer::default();
    let message = recvmsg(channel, &mut iov, &mut ancillary, RecvFlags::DONTWAIT)
        .map_err(|error| format!("receive link-control acknowledgement: {error}"))?;
    if message.flags.intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC) {
        return Err("truncated link-control acknowledgement".into());
    }
    let (kind, status, received_generation) = decode(&bytes[..message.bytes])?;
    if kind != 3 || received_generation != generation || ancillary.drain().next().is_some() {
        return Err("mismatched link-control acknowledgement".into());
    }
    match status {
        1 => Ok(AckStatus::Applied),
        2 => Ok(AckStatus::Rejected),
        _ => Err("invalid link-control acknowledgement status".into()),
    }
}

pub(crate) const MAX_REPORT: usize = 65536;
pub(crate) enum Reply {
    Ack { generation: u64, status: AckStatus },
    Interface(netstack3_port_integration::interfaces::InterfaceSnapshot),
}

/// Opt-in only: older capability installers continue receiving just their ACKs.
pub(crate) fn watch(channel: BorrowedFd<'_>, generation: u64) -> Result<(), String> {
    let bytes = encode(4, 0, generation);
    let mut ancillary = SendAncillaryBuffer::default();
    match sendmsg(channel, &[IoSlice::new(&bytes)], &mut ancillary,
        SendFlags::DONTWAIT | SendFlags::NOSIGNAL) {
        Ok(MESSAGE_LEN) => Ok(()),
        result => Err(format!("subscribe interface watcher: {result:?}")),
    }
}

/// False means backpressure. The producer retains only its latest state and
/// waits for writable readiness rather than spinning or blocking core work.
pub(crate) fn send_interface(channel: BorrowedFd<'_>, bytes: &[u8]) -> Result<bool, String> {
    if bytes.len() > MAX_REPORT { return Err("interface report exceeds wire budget".into()); }
    let mut ancillary = SendAncillaryBuffer::default();
    match sendmsg(channel, &[IoSlice::new(bytes)], &mut ancillary,
        SendFlags::DONTWAIT | SendFlags::NOSIGNAL) {
        Ok(n) if n == bytes.len() => Ok(true),
        Err(rustix::io::Errno::AGAIN) => Ok(false),
        result => Err(format!("send interface report: {result:?}")),
    }
}

pub(crate) fn receive_reply(channel: BorrowedFd<'_>) -> Result<Option<Reply>, String> {
    let mut bytes = vec![0; MAX_REPORT];
    let mut ancillary = RecvAncillaryBuffer::default();
    let message = match recvmsg(channel, &mut [IoSliceMut::new(&mut bytes)],
        &mut ancillary, RecvFlags::DONTWAIT) {
        Ok(message) => message,
        Err(rustix::io::Errno::AGAIN) => return Ok(None),
        Err(error) => return Err(format!("receive interface report: {error}")),
    };
    if message.bytes == 0 { return Err("provider administration channel closed".into()); }
    if message.flags.intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC) {
        return Err("truncated interface report or unexpected descriptors".into());
    }
    bytes.truncate(message.bytes);
    if bytes.starts_with(&MAGIC) {
        let (kind, status, generation) = decode(&bytes)?;
        let status = match (kind, status) {
            (3, 1) => AckStatus::Applied,
            (3, 2) => AckStatus::Rejected,
            _ => return Err("invalid interface acknowledgement".into()),
        };
        Ok(Some(Reply::Ack { generation, status }))
    } else {
        serde_json::from_slice(&bytes).map(Reply::Interface).map(Some)
            .map_err(|error| format!("invalid interface observation: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fd::AsFd as _;
    use std::os::fd::{AsRawFd, FromRawFd};

    fn pair() -> (OwnedFd, OwnedFd) {
        let mut fds = [-1; 2];
        assert_eq!(unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        }, 0);
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    #[test]
    fn descriptor_transfer_and_generation_round_trip() {
        let (sender, receiver) = pair();
        let (frame, _peer) = pair();
        send_attach(sender.as_fd(), 7, frame.as_fd()).unwrap();
        let request = receive_request(receiver.as_fd()).unwrap().unwrap();
        assert!(matches!(request, Request::Attach { generation: 7, .. }));
        send_ack(receiver.as_fd(), 7, AckStatus::Applied).unwrap();
        assert_eq!(receive_ack(sender.as_fd(), 7), Ok(AckStatus::Applied));
    }

    #[test]
    fn malformed_and_descriptor_shape_are_rejected() {
        let (sender, receiver) = pair();
        assert_eq!(unsafe {
            libc::send(sender.as_raw_fd(), b"bad".as_ptr().cast(), 3, libc::MSG_DONTWAIT)
        }, 3);
        assert!(matches!(
            receive_request(receiver.as_fd()),
            Err(error) if error == "malformed link-control record"
        ));
        send_detach(sender.as_fd(), 4).unwrap();
        assert!(matches!(
            receive_request(receiver.as_fd()),
            Ok(Some(Request::Detach { generation: 4 }))
        ));
    }
    #[test]
    fn interface_reports_fit_budget_and_backpressure_preserves_ack() {
        use netstack3_port_integration::interfaces::*;
        let address = std::net::IpAddr::V6([0xffff; 8].into());
        let snapshot = InterfaceSnapshot {
            id: u64::MAX, ipv4_enabled: true, ipv6_enabled: true,
            addresses: vec![Address { address, prefix: 128, state: AddressState::Unavailable,
                valid_until: Some(u64::MAX),
                preferred_until: PreferredUntil::Preferred(Some(u64::MAX)) }; 64],
            neighbors: vec![Neighbor { address, state: NeighborState::Unreachable,
                mac: Some([255; 6]), observed_at: u64::MAX }; 64],
            multicast: vec![[255; 6]; 64],
            router_advertisement: Some(RouterAdvertisement {
                source: [0xffff; 8].into(), observed_at: u64::MAX, options: vec![255; 8192],
            }),
            incomplete: true,
        };
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        assert!(bytes.len() + 512 < MAX_REPORT, "also reserve the public status envelope");
        let (sender, receiver) = pair();
        watch(receiver.as_fd(), 1).unwrap();
        assert!(matches!(receive_request(sender.as_fd()).unwrap(), Some(Request::Watch { generation: 1 })));
        let mut queued = 0;
        while send_interface(sender.as_fd(), &bytes).unwrap() { queued += 1; }
        assert!(queued > 0);
        // Fill the remaining small-message credit, too.
        while try_send_ack(sender.as_fd(), 7, AckStatus::Applied).unwrap() {}
        assert!(!try_send_ack(sender.as_fd(), 8, AckStatus::Applied).unwrap());
        for _ in 0..queued {
            let Some(Reply::Interface(report)) = receive_reply(receiver.as_fd()).unwrap() else {
                panic!("expected an interface report");
            };
            assert_eq!(report, snapshot);
        }
        while receive_reply(receiver.as_fd()).unwrap().is_some() {}
        assert!(try_send_ack(sender.as_fd(), 8, AckStatus::Applied).unwrap());
        assert!(matches!(receive_reply(receiver.as_fd()).unwrap(),
            Some(Reply::Ack { generation: 8, status: AckStatus::Applied })));
    }

}
