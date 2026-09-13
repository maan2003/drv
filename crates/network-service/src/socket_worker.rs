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
const VERSION: u32 = 4;
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
pub(super) fn encode_address(a: &Address) -> Vec<u8> {
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
pub(super) struct Endpoint {
    pub(super) handle: RemoteSocketHandle,
    credits: usize,
    transmit: VecDeque<(Message, usize)>,
    closing: Option<Message>,
    eof: bool,
    pub(super) listening: bool,
    pub(super) pending_accept: Option<ProviderAcceptV2>,
    pub(super) accept_ready: bool,
    readiness_sequence: u64,
}
impl Endpoint {
    pub(super) fn new(handle: RemoteSocketHandle) -> Self {
        Self {
            handle,
            credits: 4,
            transmit: VecDeque::new(),
            closing: None,
            eof: false,
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
    pub(super) data: Option<Endpoint>,
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
                    self.data = Some(Endpoint::new(handle));
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
                e.transmit.push_back((m, 0));
                continue;
            }
            if m.op == CLOSE {
                e.closing = Some(m);
                break;
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
                    SHUTDOWN => {
                        let how = match scalar(&m.data)? {
                            1 => ProviderShutdownV2::Read,
                            2 => ProviderShutdownV2::Write,
                            3 => ProviderShutdownV2::ReadWrite,
                            _ => return Err(Error::InvalidState),
                        };
                        Provider::shutdown(&mut self.provider, e.handle, how)?;
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
                        let count = scalar(&m.data)? as usize;
                        if count == 0 || count + e.credits > 4 {
                            return Err(Error::InvalidState);
                        }
                        e.credits += count;
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
        if let Some((m, offset)) = e.transmit.front_mut() {
            let result = if m.data.len() < 24 {
                Err(Error::InvalidState)
            } else {
                address(&m.data[..24]).and_then(|peer| {
                    Provider::send_msg(
                        &mut self.provider,
                        e.handle,
                        0,
                        peer,
                        &m.data[24 + *offset..],
                    )
                })
            };
            match result {
                Err(Error::WouldBlock) => {}
                Ok(n) => {
                    progress |= n != 0;
                    *offset += n;
                    if *offset == m.data.len() - 24 {
                        m.reply(Ok(vec![]))?;
                        e.transmit.pop_front();
                        progress = true;
                    }
                }
                Err(error) => {
                    m.reply(Err(error))?;
                    e.transmit.pop_front();
                    progress = true;
                }
            }
        }
        if e.transmit.is_empty()
            && let Some(m) = e.closing.take()
        {
            if let Some(child) = e.pending_accept.take() {
                let _ = Provider::close(&mut self.provider, child.handle);
            }
            m.reply(Provider::close(&mut self.provider, e.handle).map(|_| vec![]))?;
            self.data = None;
            return Ok(ControlFlow::Break(()));
        }
        if !e.listening && !e.eof && e.credits != 0 {
            match Provider::recv_msg(&mut self.provider, e.handle, PAYLOAD as u32, 0) {
                Ok(packet) => {
                    if packet.eof {
                        Message {
                            fd: fd.clone(),
                            op: STATE,
                            socket: id,
                            request: 0,
                            status: 0,
                            data: 4u32.to_le_bytes().to_vec(),
                        }
                        .write()?;
                        e.eof = true;
                    } else {
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
                        e.credits -= 1;
                    }
                    progress = true;
                }
                Err(Error::WouldBlock | Error::InvalidState | Error::InProgress) => {}
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
                    e.eof = true;
                    progress = true;
                }
            }
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
