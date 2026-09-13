//! Bounded NSS resolver endpoint, served by the existing single-owner DNS runtime.
#![forbid(unsafe_code)]
use drv_dns_wire as wire;
use netstack3_port_integration::{dns_bridge::DnsLookupHandle, service::DhcpService};
use rustix::{
    event::epoll,
    net::{SocketFlags, accept_with},
};
use std::{
    io,
    os::{
        fd::AsFd,
        unix::net::{UnixListener, UnixStream},
    },
    time::{Duration, Instant},
};

pub(crate) const TOKEN: u64 = u64::MAX - 1;
const LIMIT: usize = 64;
const LIFETIME: Duration = Duration::from_secs(4);
struct Client {
    stream: UnixStream,
    input: [u8; wire::REQUEST_LEN],
    received: usize,
    lookup: Option<DnsLookupHandle>,
    output: Option<[u8; wire::RESPONSE_LEN]>,
    sent: usize,
    deadline: Instant,
}
pub(crate) struct ResolverServer {
    listener: UnixListener,
    clients: Vec<Client>,
}
impl ResolverServer {
    pub(crate) fn new(listener: UnixListener, epoll_fd: impl AsFd) -> io::Result<Self> {
        epoll::add(
            epoll_fd,
            &listener,
            epoll::EventData::new_u64(TOKEN),
            epoll::EventFlags::IN,
        )?;
        Ok(Self {
            listener,
            clients: Vec::new(),
        })
    }
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.clients.iter().map(|c| c.deadline).min()
    }
    pub(crate) fn poll(
        &mut self,
        service: &mut DhcpService,
        epoll_fd: impl AsFd,
    ) -> io::Result<bool> {
        let mut progress = false;
        for _ in 0..16 {
            match accept_with(&self.listener, SocketFlags::CLOEXEC | SocketFlags::NONBLOCK) {
                Ok(fd) => {
                    progress = true;
                    if self.clients.len() == LIMIT {
                        continue;
                    }
                    let stream = UnixStream::from(fd);
                    epoll::add(
                        &epoll_fd,
                        &stream,
                        epoll::EventData::new_u64(TOKEN),
                        epoll::EventFlags::IN,
                    )?;
                    self.clients.push(Client {
                        stream,
                        input: [0; wire::REQUEST_LEN],
                        received: 0,
                        lookup: None,
                        output: None,
                        sent: 0,
                        deadline: Instant::now() + LIFETIME,
                    });
                }
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        self.clients.retain_mut(|client| {
            let result = client.poll(service, &epoll_fd);
            match result {
                Ok(active) => progress |= active,
                Err(_) => {
                    if let Some(handle) = client.lookup.take() {
                        service.cancel_lookup(handle);
                    }
                    progress = true;
                    return false;
                }
            }
            true
        });
        Ok(progress)
    }
}
impl Client {
    fn poll(&mut self, service: &mut DhcpService, epoll_fd: impl AsFd) -> io::Result<bool> {
        if Instant::now() >= self.deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut progress = false;
        if self.received < self.input.len() {
            match rustix::io::read(&self.stream, &mut self.input[self.received..])
                .map_err(io::Error::from)
            {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    self.received += n;
                    progress = true;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
            if self.received == self.input.len() {
                let name = wire::name(&self.input).ok_or(io::ErrorKind::InvalidData)?;
                match service.lookup_ip(name) {
                    Ok(handle) => self.lookup = Some(handle),
                    Err(error) => {
                        self.output = Some(wire::response(
                            if error.kind() == io::ErrorKind::WouldBlock {
                                wire::TEMPORARY
                            } else {
                                wire::UNAVAILABLE
                            },
                            &[],
                        ))
                    }
                }
                // No request pipelining. Keep disconnect notifications, not readable
                // junk that could turn a malicious client's connection into a busy loop.
                epoll::modify(
                    &epoll_fd,
                    &self.stream,
                    epoll::EventData::new_u64(TOKEN),
                    epoll::EventFlags::RDHUP,
                )?;
            }
        }
        if self.lookup.is_some() {
            match rustix::io::read(&self.stream, &mut [0; 1]).map_err(io::Error::from) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        if let Some(handle) = self.lookup {
            if let Some(result) = service.take_lookup(handle) {
                self.lookup = None;
                self.output = Some(match result {
                    Ok(addresses) => wire::response(wire::OK, &addresses),
                    Err(error) if error.is_no_records_found() => {
                        wire::response(wire::NOT_FOUND, &[])
                    }
                    Err(_) => wire::response(wire::TEMPORARY, &[]),
                });
            }
        }
        if let Some(output) = &self.output {
            match rustix::io::write(&self.stream, &output[self.sent..]).map_err(io::Error::from) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    self.sent += n;
                    progress = true;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    epoll::modify(
                        &epoll_fd,
                        &self.stream,
                        epoll::EventData::new_u64(TOKEN),
                        epoll::EventFlags::OUT,
                    )?;
                }
                Err(e) => return Err(e),
            }
            if self.sent == output.len() {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
        Ok(progress)
    }
}
