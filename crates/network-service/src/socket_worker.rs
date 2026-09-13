// SPDX-License-Identifier: GPL-2.0-only
//! Per-socket request, data and close owner; never TCP/IP protocol state.
//! ABI is defined by kernel-provider/production/protocol.h.
//! Linux registration, sandboxing and service scheduling live in provider.rs.
#![forbid(unsafe_code)]
use netstack3_port_integration::socket_provider::NativeSocketProvider;
use netstack3_port_spike::provider_dispatch_v2::{
    ProviderAcceptV2, RemoteSocketProviderV2 as Provider,
};
use netstack3_port_spike::provider_transport_v2::{
    ProviderNameV2, ProviderReadinessV2 as Ready, ProviderShutdownV2,
    ProviderSocketAddressV2 as Address, ProviderSocketKindV2,
};
use netstack3_port_spike::{
    RemoteIpAddress, RemoteIpVersion, RemoteSocketError as Error, RemoteSocketHandle,
    SocketClientId,
};
use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::os::fd::OwnedFd;
use std::rc::Rc;
const VERSION: u32 = 5;
const PAYLOAD: usize = 16384;
const OPEN: u32 = 1;
const BIND: u32 = 2;
const LISTEN: u32 = 3;
const CONNECT: u32 = 4;
const ACCEPT: u32 = 5;
const SEND: u32 = 6;
const SHUTDOWN: u32 = 7;
const CLOSE: u32 = 8;
const RX: u32 = 9;
const STATE: u32 = 10;
const GETNAME: u32 = 11;
const CREDIT: u32 = 13;

struct Message {
    fd: Rc<OwnedFd>,
    op: u32,
    socket: u64,
    request: u64,
    status: u32,
    data: Vec<u8>,
}
impl Message {
    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(32 + self.data.len());
        b.extend(VERSION.to_le_bytes());
        b.extend(self.op.to_le_bytes());
        b.extend(self.socket.to_le_bytes());
        b.extend(self.request.to_le_bytes());
        b.extend((self.data.len() as u32).to_le_bytes());
        b.extend(self.status.to_le_bytes());
        b.extend(&self.data);
        b
    }
    fn read(fd: Rc<OwnedFd>, socket: u64) -> Result<Option<Self>, String> {
        let mut b = vec![0; 32 + PAYLOAD + 24];
        let n = match rustix::io::read(&*fd, &mut b) {
            Ok(n) => n,
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => return Ok(None),
            Err(rustix::io::Errno::NETDOWN) => {
                return Ok(Some(Self {
                    fd,
                    op: CLOSE,
                    socket,
                    request: 0,
                    status: libc::ENETDOWN as u32,
                    data: vec![],
                }));
            }
            Err(error) => return Err(error.to_string()),
        };
        if n < 32
            || u32::from_le_bytes(b[0..4].try_into().unwrap()) != VERSION
            || u32::from_le_bytes(b[24..28].try_into().unwrap()) as usize != n - 32
        {
            return Err("invalid kernel provider frame".into());
        }
        Ok(Some(Self {
            fd,
            op: u32::from_le_bytes(b[4..8].try_into().unwrap()),
            socket: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            request: u64::from_le_bytes(b[16..24].try_into().unwrap()),
            status: 0,
            data: b[32..n].to_vec(),
        }))
    }
    fn write(&self) -> Result<(), String> {
        let b = self.encode();
        loop {
            match rustix::io::write(&*self.fd, &b) {
                Ok(n) if n == b.len() => return Ok(()),
                Ok(_) => return Err("short provider write".into()),
                Err(rustix::io::Errno::INTR) => continue,
                Err(rustix::io::Errno::NETDOWN) => return Ok(()),
                Err(error) => return Err(format!("provider write: {error}")),
            }
        }
    }
    fn reply(&self, result: Result<Vec<u8>, Error>) -> Result<(), String> {
        if self.request == 0 {
            return Ok(());
        }
        let (data, status) = match result {
            Ok(b) => (b, 0),
            Err(e) => (vec![], errno(e)),
        };
        Self {
            fd: self.fd.clone(),
            op: self.op,
            socket: self.socket,
            request: self.request,
            status,
            data,
        }
        .write()
    }
}
fn errno(e: Error) -> u32 {
    (match e {
        Error::WouldBlock => libc::EAGAIN,
        Error::InProgress => libc::EINPROGRESS,
        Error::ConnectionRefused => libc::ECONNREFUSED,
        Error::TimedOut => libc::ETIMEDOUT,
        Error::NetworkUnreachable => libc::ENETUNREACH,
        Error::HostUnreachable => libc::EHOSTUNREACH,
        Error::AddressInUse => libc::EADDRINUSE,
        Error::AlreadyConnected => libc::EISCONN,
        Error::NotSupported => libc::EOPNOTSUPP,
        Error::PayloadTooLarge => libc::EMSGSIZE,
        Error::QuotaExceeded | Error::ResourceExhausted => libc::ENOBUFS,
        Error::PermissionDenied => libc::EACCES,
        Error::StaleHandle | Error::UnknownClient => libc::EBADF,
        _ => libc::EINVAL,
    }) as u32
}
fn address(b: &[u8]) -> Result<Option<Address>, Error> {
    if b.len() != 24 || b[20..24] != [0; 4] {
        return Err(Error::NotSupported);
    }
    let family = u16::from_le_bytes(b[..2].try_into().unwrap());
    let port = u16::from_le_bytes(b[2..4].try_into().unwrap());
    let ip = match family {
        0 => return Ok(None),
        4 => RemoteIpAddress::V4(b[4..8].try_into().unwrap()),
        6 => RemoteIpAddress::V6(b[4..20].try_into().unwrap()),
        _ => return Err(Error::AddressFamilyMismatch),
    };
    Ok(Some(Address { address: ip, port }))
}
fn encode_address(a: &Address) -> Vec<u8> {
    let mut b = vec![0; 24];
    b[2..4].copy_from_slice(&a.port.to_le_bytes());
    match a.address {
        RemoteIpAddress::V4(ip) => {
            b[0] = 4;
            b[4..8].copy_from_slice(&ip);
        }
        RemoteIpAddress::V6(ip) => {
            b[0] = 6;
            b[4..20].copy_from_slice(&ip);
        }
    }
    b
}
fn scalar(b: &[u8]) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(
        b.try_into().map_err(|_| Error::InvalidState)?,
    ))
}
// Adapted from pinned Fuchsia bindings/socket/stream/{buffer.rs,../stream.rs}:
// send_task, send_task_shutdown, receive_task, and TaskControl. The Linux
// message/credit transport replaces Zircon socket waits. Each poll is bounded;
// terminal handoff transfers admitted bytes to core, not to the remote peer.
trait SendTaskOps {
    fn send(&mut self, peer: Option<Address>, bytes: &[u8]) -> Result<usize, Error>;
    fn finish(&mut self, admitted: &[u8]) -> Result<(), Error>;
}
struct SocketOps<'a> {
    provider: &'a mut NativeSocketProvider,
    handle: RemoteSocketHandle,
}
impl SendTaskOps for SocketOps<'_> {
    fn send(&mut self, peer: Option<Address>, bytes: &[u8]) -> Result<usize, Error> {
        Provider::send_msg(self.provider, self.handle, 0, peer, bytes)
    }
    fn finish(&mut self, admitted: &[u8]) -> Result<(), Error> {
        self.provider.finish_tcp_send(self.handle, admitted)
    }
}
struct PendingSend {
    request: Message,
    peer: Option<Address>,
    offset: usize,
}
#[derive(Default)]
struct SendTask {
    pending: VecDeque<PendingSend>,
}
impl SendTask {
    fn admit(&mut self, request: Message) -> Result<(), String> {
        let peer = request
            .data
            .get(..24)
            .ok_or(Error::InvalidState)
            .and_then(address);
        match peer {
            Ok(peer) => self.pending.push_back(PendingSend {
                request,
                peer,
                offset: 24,
            }),
            Err(error) => request.reply(Err(error))?,
        }
        Ok(())
    }
    fn poll(&mut self, ops: &mut impl SendTaskOps) -> Result<bool, String> {
        let Some(write) = self.pending.front_mut() else {
            return Ok(false);
        };
        let remaining = &write.request.data[write.offset..];
        match ops.send(write.peer.clone(), remaining) {
            Err(Error::WouldBlock) => Ok(false),
            Ok(0) if !remaining.is_empty() => Ok(false),
            Ok(n) => {
                write.offset += n;
                if write.offset == write.request.data.len() {
                    write.request.reply(Ok(vec![]))?;
                    self.pending.pop_front();
                }
                Ok(true)
            }
            Err(error) => {
                write.request.reply(Err(error))?;
                self.pending.pop_front();
                Ok(true)
            }
        }
    }
    fn finish(&mut self, ops: &mut impl SendTaskOps) -> Result<(), String> {
        // Admission is bounded by the kernel endpoint's outstanding send bytes.
        // Unlike normal pumping, shutdown does not wait for buffer space/ACKs.
        let mut admitted = Vec::new();
        for write in &self.pending {
            admitted.extend_from_slice(&write.request.data[write.offset..]);
        }
        let result = ops.finish(&admitted);
        for write in self.pending.drain(..) {
            write.request.reply(result.map(|()| vec![]))?;
        }
        Ok(())
    }
}
struct ReceiveTask {
    credits: usize,
    ended: bool,
}
impl ReceiveTask {
    fn new() -> Self {
        Self {
            credits: 4,
            ended: false,
        }
    }
    fn credit(&mut self, count: usize) -> Result<(), Error> {
        if count == 0 || count > 4 - self.credits {
            return Err(Error::InvalidState);
        }
        self.credits += count;
        Ok(())
    }
    fn poll(
        &mut self,
        provider: &mut NativeSocketProvider,
        handle: RemoteSocketHandle,
        fd: &Rc<OwnedFd>,
        id: u64,
    ) -> Result<bool, String> {
        if self.ended || self.credits == 0 {
            return Ok(false);
        }
        let result = Provider::recv_msg(provider, handle, PAYLOAD as u32, 0);
        match result {
            Ok(packet) if !packet.eof => {
                let mut data = packet
                    .source
                    .as_ref()
                    .map(encode_address)
                    .unwrap_or_else(|| vec![0; 24]);
                data.extend(packet.data);
                Message {
                    fd: fd.clone(),
                    op: RX,
                    socket: id,
                    request: 0,
                    status: 0,
                    data,
                }
                .write()?;
                self.credits -= 1;
            }
            Ok(_) => {
                Message {
                    fd: fd.clone(),
                    op: STATE,
                    socket: id,
                    request: 0,
                    status: 0,
                    data: 4u32.to_le_bytes().to_vec(),
                }
                .write()?;
                self.ended = true;
            }
            Err(Error::WouldBlock | Error::InvalidState | Error::InProgress) => return Ok(false),
            Err(error) => {
                Message {
                    fd: fd.clone(),
                    op: STATE,
                    socket: id,
                    request: 0,
                    status: errno(error),
                    data: 4u32.to_le_bytes().to_vec(),
                }
                .write()?;
                self.ended = true;
            }
        }
        Ok(true)
    }
}
enum TaskControl {
    Running,
    Shutdown {
        request: Message,
        how: ProviderShutdownV2,
    },
    Close(Message),
}
struct Endpoint {
    handle: RemoteSocketHandle,
    tcp: bool,
    send: SendTask,
    receive: ReceiveTask,
    control: TaskControl,
    listening: bool,
    pending_accept: Option<ProviderAcceptV2>,
    accept_ready: bool,
    readiness_sequence: u64,
}
impl Endpoint {
    fn new(handle: RemoteSocketHandle) -> Self {
        Self::with_kind(handle, true)
    }
    fn with_kind(handle: RemoteSocketHandle, tcp: bool) -> Self {
        Self {
            handle,
            tcp,
            send: SendTask::default(),
            receive: ReceiveTask::new(),
            control: TaskControl::Running,
            listening: false,
            pending_accept: None,
            accept_ready: false,
            readiness_sequence: 0,
        }
    }
}

// Adapted from Fuchsia 1e1219e3fac944c9a906aea9646939746b6062b3:
// src/connectivity/network/netstack3/src/bindings/socket/worker.rs,
// SocketWorker::handle_stream and SocketWorkerHandler's request/close contract.
// Copyright 2023 The Fuchsia Authors. BSD-2-Clause license; see ../netstack3-port-spike/upstream-cargo/LICENSE.fuchsia.
// Linux differences: one kernel endpoint already represents all dup/fork users;
// bounded epoll batches replace FIDL streams, and admitted sends drain before
// the final close response. Core socket and transport have one lifetime owner.
pub(super) struct SocketWorker {
    pub(super) provider: NativeSocketProvider,
    pub(super) id: u64,
    pub(super) fd: Rc<OwnedFd>,
    data: Option<Endpoint>,
}

impl Drop for SocketWorker {
    fn drop(&mut self) {
        if let Some(mut data) = self.data.take() {
            if let Some(child) = data.pending_accept.take() {
                let _ = Provider::close(&mut self.provider, child.handle);
            }
            let _ = Provider::close(&mut self.provider, data.handle);
        }
    }
}
impl SocketWorker {
    pub(super) fn new(provider: NativeSocketProvider, id: u64, fd: Rc<OwnedFd>) -> Self {
        Self {
            provider,
            id,
            fd,
            data: None,
        }
    }

    pub(super) fn wants_accept(&self) -> bool {
        self.data
            .as_ref()
            .is_some_and(|e| e.listening && e.accept_ready)
    }

    pub(super) fn accept_info(&mut self) -> Result<Option<Vec<u8>>, String> {
        let e = self.data.as_mut().expect("listener has core socket");
        if e.pending_accept.is_none() {
            e.pending_accept = match Provider::accept(&mut self.provider, e.handle) {
                Ok(child) => Some(child),
                Err(Error::WouldBlock) => None,
                Err(error) => return Err(format!("accept from Netstack3: {error:?}")),
            };
        }
        Ok(e.pending_accept.as_ref().map(|child| {
            let mut info = encode_address(&child.local);
            info.extend(encode_address(&child.peer));
            info.extend([0; 8]);
            info
        }))
    }

    pub(super) fn pause_accept(&mut self) {
        self.data.as_mut().unwrap().accept_ready = false;
    }

    pub(super) fn take_accepted(&mut self, id: u64, fd: Rc<OwnedFd>) -> Self {
        let child = self.data.as_mut().unwrap().pending_accept.take().unwrap();
        Self {
            provider: self.provider.clone(),
            id,
            fd,
            data: Some(Endpoint::new(child.handle)),
        }
    }
    pub(super) fn handle_requests(
        &mut self,
        client: SocketClientId,
    ) -> Result<ControlFlow<(), bool>, String> {
        let mut progress = false;
        for _ in 0..32 {
            let Some(m) = Message::read(self.fd.clone(), self.id)? else {
                break;
            };
            progress = true;
            if m.op == CLOSE && (m.status != 0 || self.data.is_none()) {
                return Ok(ControlFlow::Break(()));
            }
            if m.op == OPEN {
                let result = (|| {
                    if self.data.is_some() || m.data.len() != 8 {
                        return Err(Error::InvalidState);
                    }
                    let kind = match scalar(&m.data[..4])? {
                        1 => ProviderSocketKindV2::Tcp,
                        2 => ProviderSocketKindV2::Udp,
                        _ => return Err(Error::WrongSocketKind),
                    };
                    let family = match scalar(&m.data[4..])? {
                        4 => RemoteIpVersion::V4,
                        6 => RemoteIpVersion::V6,
                        _ => return Err(Error::AddressFamilyMismatch),
                    };
                    let handle = Provider::open_socket(&mut self.provider, client, kind, family)?;
                    self.data = Some(Endpoint::with_kind(
                        handle,
                        kind == ProviderSocketKindV2::Tcp,
                    ));
                    Ok(vec![])
                })();
                m.reply(result)?;
                continue;
            }
            if m.op == ACCEPT {
                if let Some(e) = self.data.as_mut() {
                    e.accept_ready = true;
                }
                continue;
            }
            let Some(e) = self.data.as_mut() else {
                m.reply(Err(Error::StaleHandle))?;
                continue;
            };
            if m.op == SEND {
                e.send.admit(m)?;
                continue;
            }
            if m.op == CLOSE {
                e.control = TaskControl::Close(m);
                break;
            }
            if m.op == SHUTDOWN {
                let how = scalar(&m.data).and_then(|v| match v {
                    1 => Ok(ProviderShutdownV2::Read),
                    2 => Ok(ProviderShutdownV2::Write),
                    3 => Ok(ProviderShutdownV2::ReadWrite),
                    _ => Err(Error::InvalidState),
                });
                match how {
                    Ok(how) => e.control = TaskControl::Shutdown { request: m, how },
                    Err(error) => m.reply(Err(error))?,
                }
                continue;
            }
            let result = (|| -> Result<Vec<u8>, Error> {
                match m.op {
                    BIND => {
                        let a = address(&m.data)?.ok_or(Error::InvalidState)?;
                        let ip = match a.address {
                            RemoteIpAddress::V4([0, 0, 0, 0]) => None,
                            RemoteIpAddress::V6(ip) if ip == [0; 16] => None,
                            ip => Some(ip),
                        };
                        Provider::bind(&mut self.provider, e.handle, ip, a.port)
                            .map(|a| encode_address(&a))
                    }
                    LISTEN => {
                        let a = Provider::listen(&mut self.provider, e.handle, scalar(&m.data)?)?;
                        e.listening = true;
                        Ok(encode_address(&a))
                    }
                    CONNECT => {
                        Provider::connect(
                            &mut self.provider,
                            e.handle,
                            address(&m.data)?.ok_or(Error::InvalidState)?,
                        )?;
                        Ok(vec![])
                    }
                    GETNAME => Provider::get_name(
                        &mut self.provider,
                        e.handle,
                        if scalar(&m.data)? == 0 {
                            ProviderNameV2::Local
                        } else {
                            ProviderNameV2::Peer
                        },
                    )
                    .map(|a| encode_address(&a)),
                    CREDIT => {
                        e.receive.credit(scalar(&m.data)? as usize)?;
                        Ok(vec![])
                    }
                    _ => Err(Error::NotSupported),
                }
            })();
            m.reply(result)?;
        }
        Ok(ControlFlow::Continue(progress))
    }
    pub(super) fn poll_data(&mut self) -> Result<ControlFlow<(), bool>, String> {
        let Some(e) = self.data.as_mut() else {
            return Ok(ControlFlow::Continue(false));
        };
        let id = self.id;
        let fd = &self.fd;
        let mut progress = false;
        let finishing = matches!(e.control, TaskControl::Close(_))
            || matches!(
                e.control,
                TaskControl::Shutdown {
                    how: ProviderShutdownV2::Write | ProviderShutdownV2::ReadWrite,
                    ..
                }
            );
        {
            let mut ops = SocketOps {
                provider: &mut self.provider,
                handle: e.handle,
            };
            if finishing && e.tcp && !e.send.pending.is_empty() {
                e.send.finish(&mut ops)?;
                progress = true;
            } else {
                progress |= e.send.poll(&mut ops)?;
            }
        }
        if e.send.pending.is_empty()
            || matches!(
                e.control,
                TaskControl::Shutdown {
                    how: ProviderShutdownV2::Read,
                    ..
                }
            )
        {
            match std::mem::replace(&mut e.control, TaskControl::Running) {
                TaskControl::Running => {}
                TaskControl::Shutdown { request, how } => {
                    let result = Provider::shutdown(&mut self.provider, e.handle, how);
                    if result.is_ok()
                        && matches!(
                            how,
                            ProviderShutdownV2::Read | ProviderShutdownV2::ReadWrite
                        )
                    {
                        e.receive.ended = true;
                    }
                    request.reply(result.map(|()| vec![]))?;
                    progress = true;
                }
                TaskControl::Close(request) => {
                    if let Some(child) = e.pending_accept.take() {
                        let _ = Provider::close(&mut self.provider, child.handle);
                    }
                    // Stop both tasks before removing core state and responding.
                    e.receive.ended = true;
                    let result = Provider::close(&mut self.provider, e.handle);
                    self.data = None;
                    request.reply(result.map(|_| vec![]))?;
                    return Ok(ControlFlow::Break(()));
                }
            }
        }
        if !e.listening {
            progress |= e.receive.poll(&mut self.provider, e.handle, fd, id)?;
        }
        let snapshot = Provider::readiness(&mut self.provider, e.handle)
            .map_err(|error| format!("socket readiness: {error:?}"))?;
        if snapshot.sequence != e.readiness_sequence {
            e.readiness_sequence = snapshot.sequence;
            let state = u32::from(snapshot.readiness.0 & Ready::CONNECTED != 0);
            // EOF is emitted only after bytes have drained above.
            if state != 0 || snapshot.error.is_some() {
                Message {
                    fd: self.fd.clone(),
                    op: STATE,
                    socket: self.id,
                    request: 0,
                    status: snapshot.error.map(errno).unwrap_or(0),
                    data: state.to_le_bytes().to_vec(),
                }
                .write()?;
                progress = true;
            }
        }
        Ok(ControlFlow::Continue(progress))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram;

    #[derive(Default)]
    struct FakeSend {
        bytes: Vec<u8>,
        allowance: usize,
        finished: bool,
        fail_finish: bool,
    }
    impl SendTaskOps for FakeSend {
        fn send(&mut self, _: Option<Address>, bytes: &[u8]) -> Result<usize, Error> {
            assert!(!self.finished);
            if self.allowance == 0 {
                return Err(Error::WouldBlock);
            }
            let n = bytes.len().min(self.allowance);
            self.bytes.extend(&bytes[..n]);
            self.allowance -= n;
            Ok(n)
        }
        fn finish(&mut self, admitted: &[u8]) -> Result<(), Error> {
            self.finished = true;
            if self.fail_finish {
                return Err(Error::StaleHandle);
            }
            self.bytes.extend(admitted);
            Ok(())
        }
    }
    fn send(fd: &Rc<OwnedFd>, id: u64, bytes: &[u8]) -> Message {
        let mut data = vec![0; 24];
        data.extend(bytes);
        Message {
            fd: fd.clone(),
            op: SEND,
            socket: 1,
            request: id,
            status: 0,
            data,
        }
    }
    // Adapted from pinned Fuchsia stream/buffer.rs send_task_shutdown:
    // ordinary writes may be partial/full; the terminal handoff must not wait.
    #[test]
    fn send_task_terminal_handoff_completes_only_after_core_ownership() {
        for allowance in [0, 1, 3, 5] {
            let (tx, rx) = UnixDatagram::pair().unwrap();
            rx.set_nonblocking(true).unwrap();
            let fd = Rc::new(OwnedFd::from(tx));
            let mut task = SendTask::default();
            let mut ops = FakeSend {
                allowance,
                ..Default::default()
            };
            task.admit(send(&fd, 1, b"hello")).unwrap();
            task.admit(send(&fd, 2, b"world")).unwrap();
            task.poll(&mut ops).unwrap();
            let mut response = [0; 128];
            if allowance < 5 {
                assert_eq!(
                    rx.recv(&mut response).unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            } else {
                assert_eq!(rx.recv(&mut response).unwrap(), 32);
            }
            assert!(!task.poll(&mut ops).unwrap());
            task.finish(&mut ops).unwrap();
            assert!(ops.finished);
            assert_eq!(ops.bytes, b"helloworld");
            assert!(task.pending.is_empty());
            for id in (if allowance == 5 { 2 } else { 1 })..=2u64 {
                assert_eq!(rx.recv(&mut response).unwrap(), 32);
                assert_eq!(u64::from_le_bytes(response[16..24].try_into().unwrap()), id);
                assert_eq!(&response[28..32], &[0; 4]);
            }
        }
    }
    #[test]
    fn abi4_kernel_frames_are_rejected() {
        let (tx, rx) = UnixDatagram::pair().unwrap();
        let fd = Rc::new(OwnedFd::from(rx));
        let mut old = send(&fd, 1, b"old").encode();
        old[..4].copy_from_slice(&4u32.to_le_bytes());
        tx.send(&old).unwrap();
        assert!(Message::read(fd, 1).is_err());
    }

    #[test]
    fn failed_terminal_handoff_replies_error_to_every_admitted_send() {
        let (tx, rx) = UnixDatagram::pair().unwrap();
        let fd = Rc::new(OwnedFd::from(tx));
        let mut task = SendTask::default();
        task.admit(send(&fd, 1, b"one")).unwrap();
        task.admit(send(&fd, 2, b"two")).unwrap();
        let mut ops = FakeSend {
            fail_finish: true,
            ..Default::default()
        };
        task.finish(&mut ops).unwrap();
        assert!(ops.bytes.is_empty());
        assert!(task.pending.is_empty());
        for _ in 0..2 {
            let mut response = [0; 32];
            assert_eq!(rx.recv(&mut response).unwrap(), 32);
            assert_eq!(
                u32::from_le_bytes(response[28..32].try_into().unwrap()),
                libc::EBADF as u32
            );
        }
    }
    #[test]
    fn pending_and_transferred_children_have_exactly_one_lifetime_owner() {
        use netstack3_port_integration::Runtime;
        use std::{cell::RefCell, num::NonZeroU64};
        for publish in [false, true] {
            let runtime = Runtime::new(
                8,
                (0u8..=255).cycle().take(8192),
                NonZeroU64::new(1).unwrap(),
                [2, 0, 0, 0, 0, 1],
                1500,
            )
            .unwrap();
            let mut provider = NativeSocketProvider::new(Rc::new(RefCell::new(runtime)));
            let client = SocketClientId::from_raw(43);
            Provider::open_client(&mut provider, client, 2).unwrap();
            let listener = Provider::open_socket(
                &mut provider,
                client,
                ProviderSocketKindV2::Tcp,
                RemoteIpVersion::V4,
            )
            .unwrap();
            let child = Provider::open_socket(
                &mut provider,
                client,
                ProviderSocketKindV2::Tcp,
                RemoteIpVersion::V4,
            )
            .unwrap();
            let (tx, _rx) = UnixDatagram::pair().unwrap();
            let fd = Rc::new(OwnedFd::from(tx));
            let mut owner = SocketWorker::new(provider.clone(), 1, fd.clone());
            let mut endpoint = Endpoint::new(listener);
            let address = Address {
                address: RemoteIpAddress::V4([127, 0, 0, 1]),
                port: 1,
            };
            endpoint.pending_accept = Some(ProviderAcceptV2 {
                handle: child,
                local: address.clone(),
                peer: address,
            });
            owner.data = Some(endpoint);
            if publish {
                let child_owner = owner.take_accepted(2, fd);
                // Models failure after successful publication, e.g. epoll ADD.
                drop(child_owner);
                assert!(Provider::get_name(&mut provider, listener, ProviderNameV2::Local).is_ok());
                assert_eq!(
                    Provider::get_name(&mut provider, child, ProviderNameV2::Local),
                    Err(Error::StaleHandle)
                );
            }
            drop(owner);
            for handle in [listener, child] {
                assert_eq!(
                    Provider::get_name(&mut provider, handle, ProviderNameV2::Local),
                    Err(Error::StaleHandle)
                );
            }
            // Both handles returned their quota, including the unpublished child.
            for _ in 0..2 {
                Provider::open_socket(
                    &mut provider,
                    client,
                    ProviderSocketKindV2::Tcp,
                    RemoteIpVersion::V4,
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn receive_credits_cannot_be_created_or_returned_twice() {
        let mut receive = ReceiveTask::new();
        assert_eq!(receive.credit(1), Err(Error::InvalidState));
        receive.credits -= 2;
        assert_eq!(receive.credit(0), Err(Error::InvalidState));
        assert_eq!(receive.credit(3), Err(Error::InvalidState));
        receive.credit(2).unwrap();
        assert_eq!(receive.credit(1), Err(Error::InvalidState));
    }
}
