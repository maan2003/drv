// SPDX-License-Identifier: GPL-2.0-only
//! Per-socket control, data and close owner; never TCP/IP protocol state.
//! ABI is defined by kernel-provider/production/protocol.h.
//! Linux registration, sandboxing and service scheduling live in provider.rs.
#![forbid(unsafe_code)]
use netstack3_port_integration::{
    NativeIpAddress as Ip, NativeSocketAddress as Address, RuntimeError as Error, TcpShutdown,
    sockets::{Connection, IpVersion, Sockets, TcpListener, TcpSocket, UdpSocket},
};
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::time::{Duration, Instant};
const VERSION: u32 = 6;
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
const CONNECTION: u32 = 14;
const ACTIVATE: u32 = 13;

#[derive(Debug)]
pub(super) enum EndpointFault {
    Protocol(&'static str),
    Core(Error),
    Io(rustix::io::Errno),
    Allocation,
}
impl std::fmt::Display for EndpointFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(reason) => write!(f, "protocol: {reason}"),
            Self::Core(error) => write!(f, "core: {error:?}"),
            Self::Io(error) => write!(f, "endpoint I/O: {error}"),
            Self::Allocation => f.write_str("endpoint allocation failed"),
        }
    }
}
pub(super) enum Work {
    Idle,
    Progress,
    Closed,
}
enum AcceptState {
    WaitingForListenerSpace,
    Ready,
    RetryAt(Instant),
}
impl AcceptState {
    fn due(&self) -> bool {
        match self {
            Self::Ready => true,
            Self::RetryAt(at) => Instant::now() >= *at,
            _ => false,
        }
    }
    fn deadline(&self) -> Option<Instant> {
        match self {
            Self::RetryAt(at) => Some(*at),
            _ => None,
        }
    }
}

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
        let mut b = Vec::with_capacity(24 + self.data.len());
        b.extend(VERSION.to_le_bytes());
        b.extend(self.op.to_le_bytes());
        b.extend(self.request.to_le_bytes());
        b.extend((self.data.len() as u32).to_le_bytes());
        b.extend(self.status.to_le_bytes());
        b.extend(&self.data);
        b
    }
    fn read(fd: Rc<OwnedFd>, socket: u64, control: bool) -> Result<Option<Self>, EndpointFault> {
        let mut b = vec![0; 24 + PAYLOAD + 24];
        let result = if control {
            let mut bytes = [0; 128];
            crate::provider::read_control(&fd, &mut bytes).map(|n| {
                b[..n].copy_from_slice(&bytes[..n]);
                n
            })
        } else {
            rustix::io::read(&*fd, &mut b)
        };
        let n = match result {
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
            Err(error) => return Err(EndpointFault::Io(error)),
        };
        if n < 24
            || u32::from_le_bytes(b[0..4].try_into().unwrap()) != VERSION
            || u32::from_le_bytes(b[16..20].try_into().unwrap()) as usize != n - 24
        {
            return Err(EndpointFault::Protocol("invalid kernel provider frame"));
        }
        Ok(Some(Self {
            fd,
            op: u32::from_le_bytes(b[4..8].try_into().unwrap()),
            socket,
            request: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            status: 0,
            data: b[24..n].to_vec(),
        }))
    }
    fn write(&self) -> Result<(), EndpointFault> {
        if self.try_write()? {
            Ok(())
        } else {
            Err(EndpointFault::Protocol(
                "control output unexpectedly blocked",
            ))
        }
    }
    fn try_write(&self) -> Result<bool, EndpointFault> {
        let b = self.encode();
        loop {
            match rustix::io::write(&*self.fd, &b) {
                Ok(n) if n == b.len() => return Ok(true),
                Ok(_) => return Err(EndpointFault::Protocol("short provider write")),
                Err(rustix::io::Errno::INTR) => continue,
                Err(rustix::io::Errno::NETDOWN) => return Ok(true),
                Err(rustix::io::Errno::AGAIN) => return Ok(false),
                Err(error) => return Err(EndpointFault::Io(error)),
            }
        }
    }
    fn reply(&self, result: Result<Vec<u8>, Error>) -> Result<(), EndpointFault> {
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
    fn admit(&mut self, request: Message) -> Result<(), EndpointFault> {
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
            Err(error) => return Err(EndpointFault::Core(error)),
        }
        Ok(())
    }
    fn poll(&mut self, ops: &mut impl SendTaskOps) -> Result<bool, EndpointFault> {
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
                    self.pending.pop_front();
                }
                Ok(true)
            }
            Err(error) => {
                Message {
                    fd: write.request.fd.clone(),
                    op: STATE,
                    socket: write.request.socket,
                    request: 0,
                    status: errno(error),
                    data: 0u32.to_le_bytes().to_vec(),
                }
                .write()?;
                self.pending.pop_front();
                Ok(true)
            }
        }
    }
    fn finish(&mut self, ops: &mut impl SendTaskOps) -> Result<(), EndpointFault> {
        // Admission is bounded by the kernel endpoint's outstanding send bytes.
        // Unlike normal pumping, shutdown does not wait for buffer space/ACKs.
        let size: usize = self
            .pending
            .iter()
            .map(|p| p.request.data.len() - p.offset)
            .sum();
        let mut admitted = Vec::new();
        admitted
            .try_reserve_exact(size)
            .map_err(|_| EndpointFault::Allocation)?;
        for write in &self.pending {
            admitted.extend_from_slice(&write.request.data[write.offset..]);
        }
        ops.finish(&admitted).map_err(EndpointFault::Core)?;
        self.pending.clear();
        Ok(())
    }
}
/// Only transport-admitted data may be retained for RX publication.
struct RxRecord {
    source: Option<Address>,
    payload: Vec<u8>,
}
enum DroppedDatagram {
    Oversized,
}
impl RxRecord {
    fn tcp(payload: Vec<u8>) -> Self {
        assert!(payload.len() <= PAYLOAD);
        Self {
            source: None,
            payload,
        }
    }
    fn udp(packet: netstack3_port_integration::NativeUdpDatagram) -> Result<Self, DroppedDatagram> {
        if packet.body.len() > PAYLOAD {
            return Err(DroppedDatagram::Oversized);
        }
        Ok(Self {
            source: Some(packet.source),
            payload: packet.body,
        })
    }
    fn try_publish(&self, fd: &Rc<OwnedFd>, id: u64) -> Result<bool, EndpointFault> {
        let mut data = self
            .source
            .as_ref()
            .map(encode_address)
            .unwrap_or_else(|| vec![0; 24]);
        data.extend_from_slice(&self.payload);
        Message {
            fd: fd.clone(),
            op: RX,
            socket: id,
            request: 0,
            status: 0,
            data,
        }
        .try_write()
    }
}
enum Received {
    Record(RxRecord),
    Eof,
    Dropped(DroppedDatagram),
}
struct ReceiveTask {
    pending: Option<RxRecord>,
    ended: bool,
}
impl ReceiveTask {
    fn new() -> Self {
        Self {
            pending: None,
            ended: false,
        }
    }
    fn poll(
        &mut self,
        socket: &mut Socket,
        fd: &Rc<OwnedFd>,
        id: u64,
    ) -> Result<bool, EndpointFault> {
        if let Some(message) = self.pending.as_ref() {
            if !message.try_publish(fd, id)? {
                return Ok(false);
            }
            self.pending = None;
            return Ok(true);
        }
        if self.ended {
            return Ok(false);
        }
        let result = (|| -> Result<Received, Error> {
            match socket {
                Socket::Tcp(s) => {
                    let (readable, _, eof) = s.readiness()?;
                    if !readable {
                        return if eof {
                            Ok(Received::Eof)
                        } else {
                            Err(Error::WouldBlock)
                        };
                    }
                    let mut bytes = vec![0; PAYLOAD];
                    let n = s.read(&mut bytes)?;
                    bytes.truncate(n);
                    Ok(Received::Record(RxRecord::tcp(bytes)))
                }
                Socket::Udp(s) => match s.receive()? {
                    Some(packet) => Ok(match RxRecord::udp(packet) {
                        Ok(record) => Received::Record(record),
                        Err(reason) => Received::Dropped(reason),
                    }),
                    None => Err(Error::WouldBlock),
                },
                Socket::Listener(_) => Err(Error::InvalidState),
            }
        })();
        match result {
            Ok(Received::Record(record)) => {
                if !record.try_publish(fd, id)? {
                    self.pending = Some(record);
                }
            }
            Ok(Received::Eof) => {
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
            Ok(Received::Dropped(DroppedDatagram::Oversized)) => {
                // The binding supports atomic datagrams up to PAYLOAD. Drop an
                // oversize record atomically and report this endpoint's error;
                // never let one valid network packet terminate the provider.
                Message {
                    fd: fd.clone(),
                    op: STATE,
                    socket: id,
                    request: 0,
                    status: errno(Error::PayloadTooLarge),
                    data: 0u32.to_le_bytes().to_vec(),
                }
                .write()?;
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
    Shutdown {
        request: Message,
        how: TcpShutdown,
        seal: u64,
    },
    Close(Message),
}
struct Endpoint {
    socket: Option<Socket>, // taken only while consuming TcpSocket into TcpListener
    send: SendTask,
    receive: ReceiveTask,
    control: TaskControl,
    pending_accept: Option<TcpSocket>,
    accept: AcceptState,
    last_state: Option<(u32, Option<Error>)>,
    dequeued: u64,
    connection_attempt: Option<u64>,
}
impl Endpoint {
    fn new(socket: Socket) -> Self {
        Self {
            socket: Some(socket),
            send: SendTask::default(),
            receive: ReceiveTask::new(),
            control: TaskControl::Running,
            pending_accept: None,
            accept: AcceptState::WaitingForListenerSpace,
            last_state: None,
            dequeued: 0,
            connection_attempt: None,
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
            .is_some_and(|e| matches!(e.socket, Some(Socket::Listener(_))) && e.accept.due())
    }

    pub(super) fn accept_info(&mut self) -> Result<Option<Vec<u8>>, EndpointFault> {
        let e = self.data.as_mut().expect("listener has core socket");
        if e.pending_accept.is_none() {
            let Some(Socket::Listener(listener)) = e.socket.as_mut() else {
                return Err(EndpointFault::Protocol("accept on non-listener"));
            };
            e.pending_accept = match listener.accept() {
                Ok(child) => {
                    e.accept = AcceptState::Ready;
                    Some(child)
                }
                Err(Error::WouldBlock) => {
                    e.accept = AcceptState::Ready;
                    None
                }
                Err(Error::SocketLimit) => {
                    e.accept = AcceptState::RetryAt(Instant::now() + Duration::from_millis(100));
                    None
                }
                Err(error) => return Err(EndpointFault::Core(error)),
            };
        }
        e.pending_accept
            .as_ref()
            .map(|child| {
                let names = child.info().map_err(EndpointFault::Core)?;
                let mut info = encode_address(&names.local);
                info.extend(encode_address(
                    &names
                        .peer
                        .ok_or(EndpointFault::Protocol("accepted child has no peer"))?,
                ));
                info.extend([0; 8]);
                Ok(info)
            })
            .transpose()
    }

    pub(super) fn accept_deadline(&self) -> Option<Instant> {
        self.data.as_ref().and_then(|e| e.accept.deadline())
    }
    pub(super) fn retry_accept(&mut self) {
        self.data.as_mut().unwrap().accept =
            AcceptState::RetryAt(Instant::now() + Duration::from_millis(100));
    }

    pub(super) fn pause_accept(&mut self) {
        self.data.as_mut().unwrap().accept = AcceptState::WaitingForListenerSpace;
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
    pub(super) fn handle_requests(&mut self) -> Result<Work, EndpointFault> {
        let mut progress = false;
        for _ in 0..32 {
            let Some(m) = Message::read(self.fd.clone(), self.id, true)? else {
                break;
            };
            progress = true;
            if m.op == CLOSE && (m.status != 0 || self.data.is_none()) {
                return Ok(Work::Closed);
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
                    let local = socket.info()?.local;
                    self.data = Some(Endpoint::new(socket));
                    Ok(encode_address(&local))
                })();
                m.reply(result)?;
                continue;
            }
            if m.op == ACCEPT {
                if let Some(e) = self.data.as_mut() {
                    e.accept = AcceptState::Ready;
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
                if m.data.len() != 8 {
                    return Err(EndpointFault::Protocol("invalid close seal"));
                }
                e.control = TaskControl::Close(m);
                break;
            }
            if m.op == SHUTDOWN {
                if m.data.len() != 12 {
                    return Err(EndpointFault::Protocol("invalid shutdown control"));
                }
                let seal = u64::from_le_bytes(m.data[4..].try_into().unwrap());
                let how = scalar(&m.data[..4]).and_then(|v| match v {
                    1 => Ok(TcpShutdown::Receive),
                    2 => Ok(TcpShutdown::Send),
                    3 => Ok(TcpShutdown::SendAndReceive),
                    _ => Err(Error::InvalidState),
                });
                match how {
                    Ok(how) => {
                        e.control = TaskControl::Shutdown {
                            request: m,
                            how,
                            seal,
                        }
                    }
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
                            Socket::Tcp(s) => {
                                s.connect(a)?;
                                e.connection_attempt = Some(m.request);
                            }
                            Socket::Udp(s) => s.connect(a)?,
                            Socket::Listener(_) => return Err(Error::InvalidState),
                        }
                        let info = e.socket.as_ref().unwrap().info()?;
                        let mut data = encode_address(&info.local);
                        data.extend(encode_address(&a));
                        Ok(data)
                    }
                    ACTIVATE => {
                        let Some(Socket::Udp(socket)) = e.socket.as_mut() else {
                            return Err(Error::InvalidState);
                        };
                        let info = socket.info()?;
                        if info.local.port == 0 {
                            socket.bind(info.local)?;
                        }
                        Ok(encode_address(&socket.info()?.local))
                    }
                    _ => Err(Error::NotSupported),
                }
            })();
            m.reply(result)?;
        }
        Ok(if progress { Work::Progress } else { Work::Idle })
    }
    pub(super) fn poll_data(&mut self) -> Result<Work, EndpointFault> {
        let Some(e) = self.data.as_mut() else {
            return Ok(Work::Idle);
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
        let seal = match &e.control {
            TaskControl::Close(m) => Some(u64::from_le_bytes(m.data[..].try_into().unwrap())),
            TaskControl::Shutdown {
                seal,
                how: TcpShutdown::Send | TcpShutdown::SendAndReceive,
                ..
            } => Some(*seal),
            _ => None,
        };
        if seal.is_some_and(|seal| e.dequeued > seal) {
            return Err(EndpointFault::Protocol("TX seal behind dequeued position"));
        }
        let tcp = matches!(e.socket, Some(Socket::Tcp(_)));
        if e.send.pending.is_empty() || (finishing && tcp) {
            let limit = if finishing { 32 } else { 1 };
            for _ in 0..limit {
                if seal == Some(e.dequeued) {
                    break;
                }
                let Some(m) = Message::read(fd.clone(), id, false)? else {
                    break;
                };
                if m.op == CLOSE && m.status != 0 {
                    return Ok(Work::Closed);
                }
                if m.op != SEND || m.request != 0 || m.data.len() < 24 {
                    return Err(EndpointFault::Protocol("invalid data transfer"));
                }
                let step = if tcp { (m.data.len() - 24) as u64 } else { 1 };
                e.dequeued = e
                    .dequeued
                    .checked_add(step)
                    .ok_or(EndpointFault::Protocol("TX position overflow"))?;
                if seal.is_some_and(|seal| e.dequeued > seal) {
                    return Err(EndpointFault::Protocol("TX exceeds seal"));
                }
                let staged: usize = e
                    .send
                    .pending
                    .iter()
                    .map(|p| p.request.data.len() - p.offset)
                    .sum();
                if staged + m.data.len() - 24 > netstack3_port_integration::TCP_TERMINAL_ALLOWANCE {
                    return Err(EndpointFault::Protocol(
                        "TX staging exceeds reserved allowance",
                    ));
                }
                e.send.admit(m)?;
                progress = true;
            }
        }
        {
            let socket = e.socket.as_mut().unwrap();
            if finishing && tcp && seal == Some(e.dequeued) {
                e.send.finish(socket)?;
                progress = true;
            } else if !finishing || !tcp {
                progress |= e.send.poll(socket)?;
            }
        }
        if (e.send.pending.is_empty() && seal.is_none_or(|seal| e.dequeued == seal))
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
                TaskControl::Shutdown { request, how, .. } => {
                    let result = e.socket.as_mut().unwrap().shutdown(how);
                    if result.is_ok()
                        && matches!(how, TcpShutdown::Receive | TcpShutdown::SendAndReceive)
                    {
                        e.receive.ended = true;
                        e.receive.pending = None;
                    }
                    request.reply(result.map(|()| vec![]))?;
                    progress = true;
                }
                TaskControl::Close(request) => {
                    e.receive.ended = true;
                    self.data = None; // drops unpublished child and core owner
                    request.reply(Ok(vec![]))?;
                    return Ok(Work::Closed);
                }
            }
        }
        if !matches!(e.socket, Some(Socket::Listener(_))) {
            progress |= e.receive.poll(e.socket.as_mut().unwrap(), fd, id)?;
        }
        let state = match e.socket.as_mut().unwrap() {
            Socket::Tcp(s) => {
                let connection = s.connection().map_err(EndpointFault::Core)?;
                if let (Some(attempt), Connection::Finished(result)) =
                    (e.connection_attempt, connection)
                {
                    let (data, status) = match result {
                        Ok(info) => {
                            let mut data = encode_address(&info.local);
                            data.extend(encode_address(&info.peer.ok_or(
                                EndpointFault::Protocol("completed connection missing peer"),
                            )?));
                            (data, 0)
                        }
                        Err(error) => (vec![], errno(error)),
                    };
                    Message {
                        fd: fd.clone(),
                        op: CONNECTION,
                        socket: id,
                        request: attempt,
                        status,
                        data,
                    }
                    .write()?;
                    e.connection_attempt = None;
                    // Error was reported with its attempt; do not mirror it as
                    // an uncorrelated second failure.
                    s.take_error().map_err(EndpointFault::Core)?;
                    progress = true;
                }
                let error = s.take_error().map_err(EndpointFault::Core)?;
                (0u32, error)
            }
            Socket::Udp(_) => (0, None),
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
        Ok(if progress { Work::Progress } else { Work::Idle })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram;

    #[test]
    fn rx_admission_rejects_oversize_before_pending_publication() {
        let source = Address {
            address: Ip::V4([192, 0, 2, 1]),
            port: 80,
        };
        let packet = |n| netstack3_port_integration::NativeUdpDatagram {
            source,
            body: vec![7; n],
        };
        assert!(matches!(
            RxRecord::udp(packet(PAYLOAD + 1)),
            Err(DroppedDatagram::Oversized)
        ));
        assert_eq!(
            RxRecord::udp(packet(PAYLOAD)).ok().unwrap().payload.len(),
            PAYLOAD
        );
        assert!(RxRecord::udp(packet(0)).ok().unwrap().payload.is_empty());
    }

    #[test]
    fn fragmented_oversize_udp_is_local_drop_and_supported_record_follows() {
        use net_types::{ethernet::Mac, ip::Ipv4Addr};
        use netstack3_port_integration::{Runtime, service::DhcpService};
        use netstack3_port_spike::{EthernetFrame, StackEthernetEndpoint};
        use packet::{Buf, NestableSerializer as _, Serializer as _};
        use packet_formats::{
            ethernet::{ETHERNET_MIN_BODY_LEN_NO_TAG, EtherType, EthernetFrameBuilder},
            ip::{FragmentOffset, IpProto, Ipv4Proto},
            ipv4::Ipv4PacketBuilder,
            udp::UdpPacketBuilder,
        };
        use rand::SeedableRng as _;
        use std::num::{NonZeroU16, NonZeroU64};
        let mac = [2, 0, 0, 0, 0, 1];
        let mut rt = Runtime::new(
            16,
            (0u8..=255).cycle().take(65536),
            NonZeroU64::new(1).unwrap(),
            mac,
            1500,
        )
        .unwrap();
        rt.apply_ipv4([192, 0, 2, 2], 24, None).unwrap();
        let mut service = DhcpService::new(rt, rand::rngs::StdRng::seed_from_u64(1), mac);
        let sockets = service.sockets();
        let mut udp = sockets.udp(IpVersion::V4).unwrap();
        udp.bind(Address {
            address: Ip::V4([192, 0, 2, 2]),
            port: 9000,
        })
        .unwrap();
        let mut socket = Socket::Udp(udp);
        let (tx, rx) = UnixDatagram::pair().unwrap();
        rx.set_nonblocking(true).unwrap();
        let fd = Rc::new(OwnedFd::from(tx));
        let mut receive = ReceiveTask::new();
        let src = Ipv4Addr::new([192, 0, 2, 1]);
        let dst = Ipv4Addr::new([192, 0, 2, 2]);
        for (id, length) in [(1, PAYLOAD + 1), (2, 3)] {
            let udp = Buf::new(vec![7; length], ..)
                .wrap_in(UdpPacketBuilder::new(
                    src,
                    dst,
                    NonZeroU16::new(8000),
                    NonZeroU16::new(9000).unwrap(),
                ))
                .serialize_vec_outer(&mut netstack3_base::NetworkSerializationContext::default())
                .unwrap()
                .unwrap_b()
                .into_inner();
            let chunks = udp.chunks(1480);
            let count = chunks.len();
            for (index, chunk) in chunks.enumerate() {
                let mut ip = Ipv4PacketBuilder::new(src, dst, 64, Ipv4Proto::Proto(IpProto::Udp));
                ip.id(id);
                ip.mf_flag(index + 1 != count);
                ip.fragment_offset(FragmentOffset::new((index * 1480 / 8) as u16).unwrap());
                let bytes =
                    Buf::new(chunk.to_vec(), ..)
                        .wrap_in(ip)
                        .wrap_in(EthernetFrameBuilder::new(
                            Mac::new([2, 0, 0, 0, 0, 2]),
                            Mac::new(mac),
                            EtherType::Ipv4,
                            ETHERNET_MIN_BODY_LEN_NO_TAG,
                        ))
                        .serialize_vec_outer(
                            &mut netstack3_base::NetworkSerializationContext::default(),
                        )
                        .unwrap()
                        .unwrap_b()
                        .into_inner();
                service
                    .receive_frame(EthernetFrame::try_from(bytes).unwrap())
                    .unwrap();
            }
            assert!(receive.poll(&mut socket, &fd, 1).unwrap());
            let mut encoded = [0; 128];
            let n = rx.recv(&mut encoded).unwrap();
            if length > PAYLOAD {
                assert_eq!(n, 28);
                assert_eq!(u32::from_le_bytes(encoded[4..8].try_into().unwrap()), STATE);
                assert_eq!(
                    u32::from_le_bytes(encoded[20..24].try_into().unwrap()),
                    libc::EMSGSIZE as u32
                );
                assert!(receive.pending.is_none());
                assert!(!receive.ended);
            } else {
                assert_eq!(n, 24 + 24 + length);
                assert_eq!(u32::from_le_bytes(encoded[4..8].try_into().unwrap()), RX);
                assert_eq!(&encoded[48..n], &[7; 3]);
            }
        }
    }

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
    fn send(fd: &Rc<OwnedFd>, _id: u64, bytes: &[u8]) -> Message {
        let mut data = vec![0; 24];
        data.extend(bytes);
        Message {
            fd: fd.clone(),
            op: SEND,
            socket: 1,
            request: 0,
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
            assert_eq!(
                rx.recv(&mut response).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            assert!(!task.poll(&mut ops).unwrap());
            task.finish(&mut ops).unwrap();
            assert!(ops.finished);
            assert_eq!(ops.bytes, b"helloworld");
            assert!(task.pending.is_empty());
            assert_eq!(
                rx.recv(&mut response).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
    #[test]
    fn abi4_kernel_frames_are_rejected() {
        let (tx, rx) = UnixDatagram::pair().unwrap();
        let fd = Rc::new(OwnedFd::from(rx));
        let mut old = send(&fd, 1, b"old").encode();
        old[..4].copy_from_slice(&4u32.to_le_bytes());
        tx.send(&old).unwrap();
        assert!(Message::read(fd, 1, false).is_err());
    }

    #[test]
    fn failed_terminal_handoff_retains_staging_and_fails_the_owner() {
        let (tx, rx) = UnixDatagram::pair().unwrap();
        rx.set_nonblocking(true).unwrap();
        let fd = Rc::new(OwnedFd::from(tx));
        let mut task = SendTask::default();
        task.admit(send(&fd, 0, b"one")).unwrap();
        task.admit(send(&fd, 0, b"two")).unwrap();
        let mut ops = FakeSend {
            fail_finish: true,
            ..Default::default()
        };
        assert!(task.finish(&mut ops).is_err());
        assert!(ops.bytes.is_empty());
        assert_eq!(task.pending.len(), 2);
        assert_eq!(
            rx.recv(&mut [0; 32]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
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
}
