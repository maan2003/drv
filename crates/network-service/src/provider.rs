// SPDX-License-Identifier: GPL-2.0-only
//! Linux socket frontend binding. This module owns IPC, never TCP/IP state.
//! ABI is defined by kernel-provider/production/protocol.h.
use netstack3_port_integration::{Runtime, socket_provider::NativeSocketProvider};
use netstack3_port_spike::provider_dispatch_v2::{
    ProviderAcceptV2, RemoteSocketProviderV2 as Provider,
};
use netstack3_port_spike::provider_transport_v2::{
    ProviderNameV2, ProviderReadinessV2 as Ready, ProviderShutdownV2,
    ProviderSocketAddressV2 as Address, ProviderSocketKindV2,
};
use netstack3_port_spike::{
    NetworkServiceEndpoint, RemoteIpAddress, RemoteIpVersion, RemoteSocketError as Error,
    RemoteSocketHandle, SocketClientId,
};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::num::NonZeroU64;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::rc::Rc;
use std::time::Instant;
const VERSION: u32 = 4;
const CLAIM: libc::c_ulong = 0x8008B301;
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
        let n = unsafe { libc::read(fd.as_raw_fd(), b.as_mut_ptr().cast(), b.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            if e.raw_os_error() == Some(libc::ENETDOWN) {
                return Ok(Some(Self {
                    fd,
                    op: CLOSE,
                    socket,
                    request: 0,
                    status: libc::ENETDOWN as u32,
                    data: vec![],
                }));
            }
            return Err(e.to_string());
        }
        let n = n as usize;
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
            let n = unsafe { libc::write(self.fd.as_raw_fd(), b.as_ptr().cast(), b.len()) };
            if n == b.len() as isize {
                return Ok(());
            }
            if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if n < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ENETDOWN) {
                return Ok(());
            }
            return Err(format!("provider write: {}", io::Error::last_os_error()));
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
struct Endpoint {
    handle: RemoteSocketHandle,
    credits: usize,
    transmit: VecDeque<(Message, usize)>,
    closing: Option<Message>,
    eof: bool,
    listening: bool,
    pending_accept: Option<ProviderAcceptV2>,
    accept_ready: bool,
}
pub fn run_provider() -> Result<(), String> {
    // FD3 is the sole provider capability. No IP socket is opened by this process.
    if unsafe { libc::fcntl(3, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let runtime = Rc::new(RefCell::new(
        Runtime::new_with_capacities(
            512,
            1024,
            (0..65536).map(|_| rand::random::<u8>()),
            NonZeroU64::new(1).unwrap(),
            [2, 0, 0, 0, 0, 1],
            1500,
        )
        .map_err(|e| format!("{e:?}"))?,
    ));
    runtime.borrow_mut().enable_loopback();
    let mut provider = NativeSocketProvider::new(runtime.clone());
    let client = SocketClientId::from_raw(1);
    Provider::open_client(&mut provider, client, 512).map_err(|e| format!("{e:?}"))?;
    crate::child::provider_setup()?;
    eprintln!(
        "netstack3_provider_sandbox_ready=true uid=65534 gid=65534 empty_root=true own_netns=true no_new_privs=true seccomp_default=kill registration_fd=3 endpoint_scope=socket native_loopback=false"
    );
    let mut event = libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: 3,
    };
    if unsafe { libc::epoll_ctl(6, libc::EPOLL_CTL_ADD, 3, &mut event) } < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let mut endpoints: HashMap<u64, Endpoint> = HashMap::new();
    let mut fds: HashMap<u64, Rc<OwnedFd>> = HashMap::new();
    let start = Instant::now();
    let mut events = [libc::epoll_event { events: 0, u64: 0 }; 64];
    loop {
        let mut progress = false;
        loop {
            let mut id = 0u64;
            let fd = unsafe { libc::ioctl(3, CLAIM, &mut id) };
            if fd < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                return Err(format!("claim endpoint: {error}"));
            }
            let fd = Rc::new(unsafe { OwnedFd::from_raw_fd(fd) });
            let mut event = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: id,
            };
            if unsafe { libc::epoll_ctl(6, libc::EPOLL_CTL_ADD, fd.as_raw_fd(), &mut event) } < 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            fds.insert(id, fd);
            progress = true;
        }
        let ready: Vec<_> = fds.iter().map(|(&id, fd)| (id, fd.clone())).collect();
        for (id, fd) in ready {
            for _ in 0..32 {
                let Some(m) = Message::read(fd.clone(), id)? else {
                    break;
                };
                if m.op == CLOSE && (m.status != 0 || !endpoints.contains_key(&id)) {
                    if let Some(e) = endpoints.remove(&id) {
                        if let Some(child) = e.pending_accept {
                            let _ = Provider::close(&mut provider, child.handle);
                        }
                        let _ = Provider::close(&mut provider, e.handle);
                    }
                    fds.remove(&id);
                    progress = true;
                    break;
                }
                progress = true;
                if m.op == OPEN {
                    let result = (|| {
                        if endpoints.len() >= 256
                            || endpoints.contains_key(&m.socket)
                            || m.data.len() != 8
                        {
                            return Err(Error::QuotaExceeded);
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
                        let handle = Provider::open_socket(&mut provider, client, kind, family)?;
                        endpoints.insert(
                            m.socket,
                            Endpoint {
                                handle,
                                credits: 4,
                                transmit: VecDeque::new(),
                                closing: None,
                                eof: false,
                                listening: false,
                                pending_accept: None,
                                accept_ready: false,
                            },
                        );
                        Ok(vec![])
                    })();
                    m.reply(result)?;
                    continue;
                }
                if m.op == ACCEPT {
                    if let Some(e) = endpoints.get_mut(&id) {
                        e.accept_ready = true;
                    }
                    continue;
                }
                let Some(e) = endpoints.get_mut(&m.socket) else {
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
                            Provider::bind(&mut provider, e.handle, ip, a.port)
                                .map(|a| encode_address(&a))
                        }
                        LISTEN => {
                            let a = Provider::listen(&mut provider, e.handle, scalar(&m.data)?)?;
                            e.listening = true;
                            Ok(encode_address(&a))
                        }
                        CONNECT => {
                            Provider::connect(
                                &mut provider,
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
                            Provider::shutdown(&mut provider, e.handle, how)?;
                            Ok(vec![])
                        }
                        GETNAME => Provider::get_name(
                            &mut provider,
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
        }
        runtime.borrow_mut().poll_at(start.elapsed(), 64);
        let listeners: Vec<_> = endpoints
            .iter()
            .filter(|(_, e)| e.listening && e.accept_ready)
            .map(|(&id, _)| id)
            .collect();
        for id in listeners {
            let e = endpoints.get_mut(&id).unwrap();
            if e.pending_accept.is_none() {
                e.pending_accept = match Provider::accept(&mut provider, e.handle) {
                    Ok(child) => Some(child),
                    Err(Error::WouldBlock) => None,
                    Err(error) => return Err(format!("accept from Netstack3: {error:?}")),
                };
            }
            let Some(child) = e.pending_accept.as_ref() else {
                continue;
            };
            let mut info = encode_address(&child.local);
            info.extend(encode_address(&child.peer));
            info.extend([0u8; 8]);
            let newfd = unsafe {
                libc::ioctl(
                    fds[&id].as_raw_fd(),
                    0xC038B302u64 as libc::c_ulong,
                    info.as_mut_ptr(),
                )
            };
            if newfd < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    e.accept_ready = false;
                    continue;
                }
                if error.raw_os_error() == Some(libc::ENETDOWN) {
                    continue;
                }
                return Err(format!("publish accepted endpoint: {error}"));
            }
            let child = e.pending_accept.take().unwrap();
            let child_id = u64::from_le_bytes(info[48..56].try_into().unwrap());
            let fd = Rc::new(unsafe { OwnedFd::from_raw_fd(newfd) });
            let mut event = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: child_id,
            };
            if unsafe { libc::epoll_ctl(6, libc::EPOLL_CTL_ADD, fd.as_raw_fd(), &mut event) } < 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            fds.insert(child_id, fd);
            endpoints.insert(
                child_id,
                Endpoint {
                    handle: child.handle,
                    credits: 4,
                    transmit: VecDeque::new(),
                    closing: None,
                    eof: false,
                    listening: false,
                    pending_accept: None,
                    accept_ready: false,
                },
            );
            progress = true;
        }
        let mut remove = Vec::new();
        for (&id, e) in endpoints.iter_mut() {
            let Some(fd) = fds.get(&id) else { continue };
            if let Some((m, offset)) = e.transmit.front_mut() {
                let result = if m.data.len() < 24 {
                    Err(Error::InvalidState)
                } else {
                    address(&m.data[..24]).and_then(|peer| {
                        Provider::send_msg(
                            &mut provider,
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
                    let _ = Provider::close(&mut provider, child.handle);
                }
                m.reply(Provider::close(&mut provider, e.handle).map(|_| vec![]))?;
                remove.push(id);
                progress = true;
                continue;
            }
            if !e.listening && !e.eof && e.credits != 0 {
                match Provider::recv_msg(&mut provider, e.handle, PAYLOAD as u32, 0) {
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
        }
        for id in remove {
            endpoints.remove(&id);
            fds.remove(&id);
        }
        for (_, handle, snapshot) in provider.take_readiness_changes() {
            let Some((&id, _)) = endpoints.iter().find(|(_, e)| e.handle == handle) else {
                continue;
            };
            let Some(fd) = fds.get(&id) else { continue };
            let mut state = 0u32;
            if snapshot.readiness.0 & Ready::CONNECTED != 0 {
                state |= 1;
            }

            // EOF is emitted only after bytes have been drained above.
            if state != 0 || snapshot.error.is_some() {
                Message {
                    fd: fd.clone(),
                    op: STATE,
                    socket: id,
                    request: 0,
                    status: snapshot.error.map(errno).unwrap_or(0),
                    data: state.to_le_bytes().to_vec(),
                }
                .write()?;
                progress = true;
            }
        }
        if progress {
            continue;
        }
        let now = start.elapsed();
        let timeout = runtime
            .borrow()
            .next_timer_deadline()
            .map(|d| {
                d.saturating_sub(now)
                    .as_millis()
                    .min((i32::MAX - 1) as u128) as i32
                    + 1
            })
            .unwrap_or(-1);
        let n = unsafe {
            libc::syscall(
                libc::SYS_epoll_pwait,
                6u32,
                events.as_mut_ptr(),
                64u32,
                timeout,
                0usize,
                0usize,
            )
        };
        if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error().to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn polling_unconnected_tcp_does_not_shutdown_future_connection() {
        use netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2 as V2;
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
        V2::open_client(&mut provider, client, 2).unwrap();
        for family in [RemoteIpVersion::V4, RemoteIpVersion::V6] {
            let socket =
                V2::open_socket(&mut provider, client, ProviderSocketKindV2::Tcp, family).unwrap();
            for _ in 0..2 {
                let ready = V2::readiness(&mut provider, socket).unwrap();
                assert_eq!(
                    ready.readiness.0 & (Ready::READ_CLOSED | Ready::WRITE_CLOSED),
                    0,
                );
            }
        }
    }
}
