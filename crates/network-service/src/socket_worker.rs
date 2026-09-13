// SPDX-License-Identifier: GPL-2.0-only
//! Per-socket request, data and close owner; never TCP/IP protocol state.
//! ABI is defined by kernel-provider/production/protocol.h.
//! Linux registration, sandboxing and service scheduling live in provider.rs.
#![forbid(unsafe_code)]
use netstack3_port_integration::{
    NativeIpAddress as Ip, NativeSocketAddress as Address, RuntimeError as Error, TcpShutdown,
    sockets::{Connection, IpVersion, Sockets, TcpListener, TcpSocket, UdpSocket},
};
use std::collections::VecDeque;
use std::num::NonZeroUsize;
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
        Error::ConnectionPending => libc::EINPROGRESS,
        Error::ConnectionRefused => libc::ECONNREFUSED,
        Error::TimedOut => libc::ETIMEDOUT,
        Error::NetworkUnreachable => libc::ENETUNREACH,
        Error::HostUnreachable => libc::EHOSTUNREACH,
        Error::AddressInUse => libc::EADDRINUSE,
        Error::AlreadyConnected => libc::EISCONN,
        Error::NotSupported => libc::EOPNOTSUPP,
        Error::PayloadTooLarge => libc::EMSGSIZE,
        Error::SocketLimit => libc::ENOBUFS,
        Error::PermissionDenied => libc::EACCES,
        Error::UnknownSocket => libc::EBADF,
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
        4 => Ip::V4(b[4..8].try_into().unwrap()),
        6 => Ip::V6(b[4..20].try_into().unwrap()),
        _ => return Err(Error::InvalidAddress),
    };
    Ok(Some(Address { address: ip, port }))
}
fn encode_address(a: &Address) -> Vec<u8> {
    let mut b = vec![0; 24];
    b[2..4].copy_from_slice(&a.port.to_le_bytes());
    match a.address {
        Ip::V4(ip) => {
            b[0] = 4;
            b[4..8].copy_from_slice(&ip);
        }
        Ip::V6(ip) => {
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
enum Socket {
    Tcp(TcpSocket),
    Listener(TcpListener),
    Udp(UdpSocket),
}
impl Socket {
    fn info(&self) -> Result<netstack3_port_integration::NativeSocketInfo, Error> {
        match self {
            Self::Tcp(s) => s.info(),
            Self::Listener(s) => s.info(),
            Self::Udp(s) => s.info(),
        }
    }
    fn shutdown(&mut self, how: TcpShutdown) -> Result<(), Error> {
        match self {
            Self::Tcp(s) => s.shutdown(how),
            Self::Udp(s) => s.shutdown(how),
            Self::Listener(_) => Err(Error::InvalidState),
        }
    }
}
impl SendTaskOps for Socket {
    fn send(&mut self, peer: Option<Address>, bytes: &[u8]) -> Result<usize, Error> {
        match self {
            Self::Tcp(s) => s.write(bytes),
            Self::Udp(s) => s.send(peer, bytes).map(|()| bytes.len()),
            Self::Listener(_) => Err(Error::InvalidState),
        }
    }
    fn finish(&mut self, admitted: &[u8]) -> Result<(), Error> {
        match self {
            Self::Tcp(s) => s.finish_write(admitted),
            _ => Err(Error::InvalidState),
        }
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
    fn poll(&mut self, socket: &mut Socket, fd: &Rc<OwnedFd>, id: u64) -> Result<bool, String> {
        if self.ended || self.credits == 0 {
            return Ok(false);
        }
        let result = (|| -> Result<Option<(Option<Address>, Vec<u8>)>, Error> {
            match socket {
                Socket::Tcp(s) => {
                    let (readable, _, eof) = s.readiness()?;
                    if !readable {
                        return if eof {
                            Ok(None)
                        } else {
                            Err(Error::WouldBlock)
                        };
                    }
                    let mut bytes = vec![0; PAYLOAD];
                    let n = s.read(&mut bytes)?;
                    bytes.truncate(n);
                    Ok(Some((None, bytes)))
                }
                Socket::Udp(s) => s
                    .receive()?
                    .map(|p| Some((Some(p.source), p.body)))
                    .ok_or(Error::WouldBlock),
                Socket::Listener(_) => Err(Error::InvalidState),
            }
        })();
        match result {
            Ok(Some((source, bytes))) => {
                let mut data = source
                    .as_ref()
                    .map(encode_address)
                    .unwrap_or_else(|| vec![0; 24]);
                data.extend(bytes);
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
            Err(Error::WouldBlock | Error::InvalidState | Error::ConnectionPending) => {
                return Ok(false);
            }
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
    Shutdown { request: Message, how: TcpShutdown },
    Close(Message),
}
struct Endpoint {
    socket: Option<Socket>, // taken only while consuming TcpSocket into TcpListener
    send: SendTask,
    receive: ReceiveTask,
    control: TaskControl,
    pending_accept: Option<TcpSocket>,
    accept_ready: bool,
    last_state: Option<(u32, Option<Error>)>,
}
impl Endpoint {
    fn new(socket: Socket) -> Self {
        Self {
            socket: Some(socket),
            send: SendTask::default(),
            receive: ReceiveTask::new(),
            control: TaskControl::Running,
            pending_accept: None,
            accept_ready: false,
            last_state: None,
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
    sockets: Sockets,
    pub(super) id: u64,
    pub(super) fd: Rc<OwnedFd>,
    data: Option<Endpoint>,
}

impl SocketWorker {
    pub(super) fn new(sockets: Sockets, id: u64, fd: Rc<OwnedFd>) -> Self {
        Self {
            sockets,
            id,
            fd,
            data: None,
        }
    }

    pub(super) fn wants_accept(&self) -> bool {
        self.data
            .as_ref()
            .is_some_and(|e| matches!(e.socket, Some(Socket::Listener(_))) && e.accept_ready)
    }

    pub(super) fn accept_info(&mut self) -> Result<Option<Vec<u8>>, String> {
        let e = self.data.as_mut().expect("listener has core socket");
        if e.pending_accept.is_none() {
            let Some(Socket::Listener(listener)) = e.socket.as_mut() else {
                return Err("accept on non-listener".into());
            };
            e.pending_accept = match listener.accept() {
                Ok(child) => Some(child),
                Err(Error::WouldBlock) => None,
                Err(error) => return Err(format!("accept from Netstack3: {error:?}")),
            };
        }
        e.pending_accept
            .as_ref()
            .map(|child| {
                let names = child.info().map_err(|e| format!("accepted names: {e:?}"))?;
                let mut info = encode_address(&names.local);
                info.extend(encode_address(
                    &names.peer.ok_or("accepted child has no peer")?,
                ));
                info.extend([0; 8]);
                Ok(info)
            })
            .transpose()
    }

    pub(super) fn pause_accept(&mut self) {
        self.data.as_mut().unwrap().accept_ready = false;
    }

    pub(super) fn take_accepted(&mut self, id: u64, fd: Rc<OwnedFd>) -> Self {
        let child = self.data.as_mut().unwrap().pending_accept.take().unwrap();
        Self {
            sockets: self.sockets.clone(),
            id,
            fd,
            data: Some(Endpoint::new(Socket::Tcp(child))),
        }
    }
    pub(super) fn handle_requests(&mut self) -> Result<ControlFlow<(), bool>, String> {
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
                    let version = match scalar(&m.data[4..])? {
                        4 => IpVersion::V4,
                        6 => IpVersion::V6,
                        _ => return Err(Error::InvalidAddress),
                    };
                    let socket = match scalar(&m.data[..4])? {
                        1 => Socket::Tcp(self.sockets.tcp(version)?),
                        2 => Socket::Udp(self.sockets.udp(version)?),
                        _ => return Err(Error::InvalidState),
                    };
                    self.data = Some(Endpoint::new(socket));
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
                m.reply(Err(Error::UnknownSocket))?;
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
                    1 => Ok(TcpShutdown::Receive),
                    2 => Ok(TcpShutdown::Send),
                    3 => Ok(TcpShutdown::SendAndReceive),
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
                        match e.socket.as_mut().unwrap() {
                            Socket::Tcp(s) => s.bind(a)?,
                            Socket::Udp(s) => s.bind(a)?,
                            Socket::Listener(_) => return Err(Error::InvalidState),
                        }
                        Ok(encode_address(&e.socket.as_ref().unwrap().info()?.local))
                    }
                    LISTEN => {
                        let backlog =
                            NonZeroUsize::new((scalar(&m.data)? as usize).max(1)).unwrap();
                        match e.socket.take().unwrap() {
                            Socket::Tcp(s) => match s.listen(backlog) {
                                Ok(listener) => e.socket = Some(Socket::Listener(listener)),
                                Err((s, error)) => {
                                    e.socket = Some(Socket::Tcp(s));
                                    return Err(error);
                                }
                            },
                            other => {
                                e.socket = Some(other);
                                return Err(Error::InvalidState);
                            }
                        }
                        Ok(encode_address(&e.socket.as_ref().unwrap().info()?.local))
                    }
                    CONNECT => {
                        let a = address(&m.data)?.ok_or(Error::InvalidState)?;
                        match e.socket.as_mut().unwrap() {
                            Socket::Tcp(s) => s.connect(a)?,
                            Socket::Udp(s) => s.connect(a)?,
                            Socket::Listener(_) => return Err(Error::InvalidState),
                        }
                        Ok(vec![])
                    }
                    GETNAME => {
                        let socket = e.socket.as_ref().unwrap();
                        let info = socket.info()?;
                        let name = if scalar(&m.data)? == 0 {
                            info.local
                        } else {
                            match socket {
                                Socket::Udp(s) => s.peer(),
                                _ => info.peer,
                            }
                            .ok_or(Error::InvalidState)?
                        };
                        Ok(encode_address(&name))
                    }
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
                    how: TcpShutdown::Send | TcpShutdown::SendAndReceive,
                    ..
                }
            );
        {
            let socket = e.socket.as_mut().unwrap();
            if finishing && matches!(socket, Socket::Tcp(_)) && !e.send.pending.is_empty() {
                e.send.finish(socket)?;
                progress = true;
            } else {
                progress |= e.send.poll(socket)?;
            }
        }
        if e.send.pending.is_empty()
            || matches!(
                e.control,
                TaskControl::Shutdown {
                    how: TcpShutdown::Receive,
                    ..
                }
            )
        {
            match std::mem::replace(&mut e.control, TaskControl::Running) {
                TaskControl::Running => {}
                TaskControl::Shutdown { request, how } => {
                    let result = e.socket.as_mut().unwrap().shutdown(how);
                    if result.is_ok()
                        && matches!(how, TcpShutdown::Receive | TcpShutdown::SendAndReceive)
                    {
                        e.receive.ended = true;
                    }
                    request.reply(result.map(|()| vec![]))?;
                    progress = true;
                }
                TaskControl::Close(request) => {
                    e.receive.ended = true;
                    self.data = None; // drops unpublished child and core owner
                    request.reply(Ok(vec![]))?;
                    return Ok(ControlFlow::Break(()));
                }
            }
        }
        if !matches!(e.socket, Some(Socket::Listener(_))) {
            progress |= e.receive.poll(e.socket.as_mut().unwrap(), fd, id)?;
        }
        let state = match e.socket.as_mut().unwrap() {
            Socket::Tcp(s) => {
                let connection = s.connection().map_err(|e| format!("connection: {e:?}"))?;
                let error = s.take_error().map_err(|e| format!("socket error: {e:?}"))?;
                (
                    u32::from(matches!(connection, Connection::Finished(Ok(_)))),
                    error,
                )
            }
            Socket::Udp(s) => (u32::from(s.peer().is_some()), None),
            Socket::Listener(_) => (0, None),
        };
        if e.last_state != Some(state) {
            e.last_state = Some(state);
            if state.0 != 0 || state.1.is_some() {
                Message {
                    fd: fd.clone(),
                    op: STATE,
                    socket: id,
                    request: 0,
                    status: state.1.map(errno).unwrap_or(0),
                    data: state.0.to_le_bytes().to_vec(),
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
                return Err(Error::UnknownSocket);
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
        use netstack3_port_integration::{Runtime, service::DhcpService};
        use rand::SeedableRng;
        use std::num::NonZeroU64;
        for publish in [false, true] {
            let runtime = Runtime::new_with_capacities(
                2,
                8,
                (0u8..=255).cycle().take(8192),
                NonZeroU64::new(1).unwrap(),
                [2, 0, 0, 0, 0, 1],
                1500,
            )
            .unwrap();
            let service = DhcpService::new(
                runtime,
                rand::rngs::StdRng::seed_from_u64(1),
                [2, 0, 0, 0, 0, 1],
            );
            let sockets = service.sockets();
            let listener = sockets.tcp(IpVersion::V4).unwrap();
            let child = sockets.tcp(IpVersion::V4).unwrap();
            assert!(matches!(
                sockets.tcp(IpVersion::V4),
                Err(Error::SocketLimit)
            ));
            let (tx, _rx) = UnixDatagram::pair().unwrap();
            let fd = Rc::new(OwnedFd::from(tx));
            let mut owner = SocketWorker::new(sockets.clone(), 1, fd.clone());
            let mut endpoint = Endpoint::new(Socket::Tcp(listener));
            endpoint.pending_accept = Some(child);
            owner.data = Some(endpoint);
            if publish {
                let child_owner = owner.take_accepted(2, fd);
                drop(child_owner); // failure after publication (e.g. epoll ADD)
                assert!(
                    owner
                        .data
                        .as_ref()
                        .unwrap()
                        .socket
                        .as_ref()
                        .unwrap()
                        .info()
                        .is_ok()
                );
                let replacement = sockets.tcp(IpVersion::V4).unwrap();
                assert!(matches!(
                    sockets.tcp(IpVersion::V4),
                    Err(Error::SocketLimit)
                ));
                drop(replacement);
            }
            drop(owner);
            let first = sockets.tcp(IpVersion::V4).unwrap();
            let second = sockets.tcp(IpVersion::V4).unwrap();
            assert!(matches!(
                sockets.tcp(IpVersion::V4),
                Err(Error::SocketLimit)
            ));
            drop((first, second));
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
