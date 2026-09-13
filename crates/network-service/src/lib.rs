// SPDX-License-Identifier: GPL-2.0-only

//! Sandboxed native Netstack3 service and application networking.

use netstack3_port_integration::{
    Runtime,
    dns_bridge::DnsLookupHandle,
    service::{DhcpService, DhcpStatus},
};
use netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2;
use netstack3_port_spike::provider_transport_v2::{ProviderReadinessV2, ProviderSocketAddressV2};
use netstack3_port_spike::{
    EthernetEventSource, EthernetRunner, NetworkServiceEndpoint, RemoteIpAddress, RemoteIpVersion,
    RemoteSocketAddress, RemoteSocketHandle, RemoteSocketProvider, SocketClientId,
};
use rand::{SeedableRng as _, rngs::StdRng};
#[cfg(test)]
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::time::{Duration, Instant};

mod child;
mod provider;
mod resolver;
pub use provider::run_provider;
mod ethernet_device;
mod lifecycle;
mod supervisor;

pub use child::{run, run_lab};
use ethernet_device::ServiceEthernetDevice;
pub use lifecycle::{WifiLifecycleReceiver, WifiLifecycleUpdate};
pub use supervisor::{NetworkServiceProcessExit, NetworkServiceSupervisor};

pub const SOFTMAC_ETHERNET_MTU: u16 = 1500;

#[cfg(test)]
mod integration_test;

#[derive(Clone)]
pub struct NetstackProofConfig {
    pub dns_name: String,
    pub server_port: NonZeroU16,
}

/// The existing Netstack3 DHCP/DNS/socket stack wired to the SoftMAC Ethernet
/// port. This is a bounded driver, not a DHCP, DNS, or TCP implementation.
struct BoundedNetstackProof {
    runner: EthernetRunner<DhcpService, ServiceEthernetDevice>,
    poller: NetworkPoller,
    config: NetstackProofConfig,
    now: Duration,
    anchor: Option<std::time::Instant>,
    resolved: Option<[u8; 4]>,
    socket: Option<netstack3_port_spike::RemoteSocketHandle>,
    proxy_client: SocketClientId,
    admission_capacity: usize,
    frame_events: u32,
}

// Admission and staging are separate budgets: idle clients consume descriptors
// and protocol metadata but cannot reserve the aggregate relay-buffer budget.
// Each admission is conservatively charged 896 KiB against a 64 MiB
// connection-state accounting pool; this is admission policy, not a claim of
// an exact process-RSS or kernel-memory bound:
// 512 KiB for Netstack's 256 KiB send/receive buffers, 256 KiB for Linux's
// doubled 64 KiB send/receive socket-buffer requests, and 128 KiB for client,
// scheduler, map, allocator, kernel, and transport metadata. Buffers allocate lazily,
// but charging their reachable maximum prevents idle admission from granting
// an unbounded future commitment. Actual relay staging has separate limits.
pub(crate) const HOST_SOCKET_BUFFER_REQUEST: i32 = 64 * 1024;
const SOCKS5_CLIENT_STATE_CHARGE: usize = 896 * 1024;
const SOCKS5_CONNECTION_STATE_BUDGET: usize = 64 * 1024 * 1024;
// Netstack's independent TX/event/readiness/UDP queues share a separate 4 MiB
// accounting budget. Each position is charged 72 KiB: one maximum 64 KiB UDP
// datagram plus Ethernet TX/device frames and event/readiness/map overhead in
// the other independently bounded queue families. This deliberately does not
// scale with admitted connections or with the scheduler work quantum.
const NETSTACK_QUEUE_MEMORY_BUDGET: usize = 4 * 1024 * 1024;
const NETSTACK_QUEUE_SLOT_CHARGE: usize = 72 * 1024;
// Production enters confinement with descriptors 0..=6 assigned to stdio,
// Ethernet, listener, bootstrap, and epoll. Charge all seven even if stdio is
// closed and retain one additional slot for operational/error-path headroom.
const SOCKS5_OPERATIONAL_FD_RESERVE: usize = 8;
const MAX_SOCKS5_PENDING_BYTES: usize = 256 * 1024;
const MAX_SOCKS5_TOTAL_PENDING_BYTES: usize = 16 * 1024 * 1024;
const MAX_SOCKS5_HANDSHAKE_BYTES: usize = 512;
const ADMISSION_RETRY_INTERVAL: Duration = Duration::from_millis(100);
const EVENT_BATCH: usize = 64;
const CLIENT_WORK_BUDGET: usize = 64;
const STACK_WORK_BUDGET: usize = 64;
const FRAME_TOKEN: u64 = 1;
const LISTENER_TOKEN: u64 = 2;
const FIRST_CLIENT_TOKEN: u64 = 3;
const BASE_EVENTS: u32 = (libc::EPOLLERR | libc::EPOLLHUP) as u32;
const CLIENT_BASE_EVENTS: u32 = BASE_EVENTS | libc::EPOLLRDHUP as u32;

#[derive(Clone, Copy)]
pub(crate) struct Socks5ResourceBudget {
    admission_capacity: usize,
}

impl Socks5ResourceBudget {
    pub(crate) fn from_process_limit() -> Result<Self, &'static str> {
        let mut limit = std::mem::MaybeUninit::<libc::rlimit>::zeroed();
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } != 0 {
            return Err("SOCKS5 descriptor limit query failed");
        }
        Self::from_soft_limit(unsafe { limit.assume_init() }.rlim_cur)
    }

    fn from_soft_limit(soft_limit: libc::rlim_t) -> Result<Self, &'static str> {
        let descriptor_capacity = usize::try_from(soft_limit)
            .unwrap_or(usize::MAX)
            .saturating_sub(SOCKS5_OPERATIONAL_FD_RESERVE);
        let state_capacity = SOCKS5_CONNECTION_STATE_BUDGET / SOCKS5_CLIENT_STATE_CHARGE;
        let admission_capacity = descriptor_capacity.min(state_capacity);
        if admission_capacity == 0 {
            return Err("SOCKS5 process resource allowance is too small");
        }
        Ok(Self { admission_capacity })
    }
}

pub(crate) fn bound_listener_socket_memory(listener: &TcpListener) -> Result<(), String> {
    for option in [libc::SO_RCVBUF, libc::SO_SNDBUF] {
        let value = HOST_SOCKET_BUFFER_REQUEST;
        if unsafe {
            libc::setsockopt(
                listener.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                (&value as *const i32).cast(),
                std::mem::size_of::<i32>() as libc::socklen_t,
            )
        } != 0
        {
            return Err(format!(
                "bound network-service listener socket memory: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut actual = 0i32;
        let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                listener.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                (&mut actual as *mut i32).cast(),
                &mut length,
            )
        } != 0
            || actual > HOST_SOCKET_BUFFER_REQUEST * 2
        {
            return Err("network-service listener socket memory exceeds accounting charge".into());
        }
    }
    Ok(())
}

pub(crate) struct NetworkPoller {
    fd: OwnedFd,
    #[cfg(test)]
    waits: Cell<(usize, usize)>,
}

impl NetworkPoller {
    pub(crate) fn new() -> Result<Self, &'static str> {
        let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if fd < 0 {
            return Err("network epoll creation failed");
        }
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            #[cfg(test)]
            waits: Cell::new((0, 0)),
        })
    }

    pub(crate) fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    fn update(
        &self,
        operation: i32,
        fd: RawFd,
        token: u64,
        events: u32,
    ) -> Result<(), &'static str> {
        let mut event = libc::epoll_event { events, u64: token };
        if unsafe { libc::epoll_ctl(self.fd.as_raw_fd(), operation, fd, &mut event) } < 0 {
            return Err("network epoll registration failed");
        }
        Ok(())
    }

    fn add(&self, fd: RawFd, token: u64, events: u32) -> Result<(), &'static str> {
        self.update(libc::EPOLL_CTL_ADD, fd, token, events)
    }

    fn modify(&self, fd: RawFd, token: u64, events: u32) -> Result<(), &'static str> {
        self.update(libc::EPOLL_CTL_MOD, fd, token, events)
    }

    fn delete(&self, fd: RawFd) -> Result<(), &'static str> {
        self.update(libc::EPOLL_CTL_DEL, fd, 0, 0)
    }

    fn wait(
        &self,
        events: &mut [libc::epoll_event],
        timeout_ms: i32,
    ) -> Result<usize, &'static str> {
        #[cfg(test)]
        {
            let (total, blocking) = self.waits.get();
            self.waits
                .set((total + 1, blocking + usize::from(timeout_ms != 0)));
        }
        let ready = unsafe {
            libc::syscall(
                libc::SYS_epoll_pwait,
                self.fd.as_raw_fd(),
                events.as_mut_ptr(),
                events.len() as i32,
                timeout_ms,
                std::ptr::null::<libc::sigset_t>(),
                0usize,
            )
        };
        if ready >= 0 {
            Ok(ready as usize)
        } else if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
            Ok(0)
        } else {
            Err("network epoll wait failed")
        }
    }

    #[cfg(test)]
    fn wait_counts(&self) -> (usize, usize) {
        self.waits.get()
    }
}

enum AcceptResult {
    Accepted(TcpStream, SocketAddr),
    Drained,
    ResourcePressure,
}

fn accept_nonblocking(listener_fd: RawFd) -> Result<AcceptResult, &'static str> {
    let mut address = std::mem::MaybeUninit::<libc::sockaddr_storage>::zeroed();
    let mut length = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    let fd = unsafe {
        libc::accept4(
            listener_fd,
            address.as_mut_ptr().cast(),
            &mut length,
            libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == ErrorKind::WouldBlock {
            return Ok(AcceptResult::Drained);
        }
        if matches!(
            error.raw_os_error(),
            Some(libc::EMFILE) | Some(libc::ENFILE)
        ) {
            return Ok(AcceptResult::ResourcePressure);
        }
        return Err("SOCKS5 accept failed");
    }
    let address = unsafe { address.assume_init() };
    let peer = match i32::from(address.ss_family) {
        libc::AF_INET => {
            let address =
                unsafe { *(&address as *const libc::sockaddr_storage).cast::<libc::sockaddr_in>() };
            SocketAddr::new(
                IpAddr::V4(Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes())),
                u16::from_be(address.sin_port),
            )
        }
        libc::AF_INET6 => {
            let address = unsafe {
                *(&address as *const libc::sockaddr_storage).cast::<libc::sockaddr_in6>()
            };
            SocketAddr::new(
                IpAddr::V6(std::net::Ipv6Addr::from(address.sin6_addr.s6_addr)),
                u16::from_be(address.sin6_port),
            )
        }
        _ => {
            unsafe { libc::close(fd) };
            return Err("SOCKS5 accepted unsupported peer family");
        }
    };
    Ok(AcceptResult::Accepted(
        unsafe { TcpStream::from_raw_fd(fd) },
        peer,
    ))
}

fn schedule_client(
    queue: &mut VecDeque<u64>,
    pending: &mut HashMap<u64, (u32, u8)>,
    token: u64,
    events: u32,
    passes: u8,
) {
    match pending.get_mut(&token) {
        Some((pending_events, pending_passes)) => {
            *pending_events |= events;
            *pending_passes = (*pending_passes).max(passes);
        }
        None => {
            pending.insert(token, (events, passes));
            queue.push_back(token);
        }
    }
}

struct Socks5Client {
    token: u64,
    stream: TcpStream,
    peer: SocketAddr,
    phase: Socks5Phase,
    host_out: VecDeque<u8>,
    host_to_remote: VecDeque<u8>,
    remote_to_host: VecDeque<u8>,
    socket: Option<RemoteSocketHandle>,
    idle_deadline: std::time::Instant,
    registered_events: u32,
    peer_half_closed: bool,
}

enum Socks5Phase {
    Greeting(Vec<u8>),
    Request(Vec<u8>),
    Dns {
        lookup: DnsLookupHandle,
        port: NonZeroU16,
    },
    Connecting {
        address: [u8; 4],
        port: NonZeroU16,
    },
    Reply,
    Relay,
    Closing,
}

impl Socks5Client {
    fn new(token: u64, stream: TcpStream, peer: SocketAddr) -> Self {
        Self {
            token,
            stream,
            peer,
            phase: Socks5Phase::Greeting(Vec::new()),
            host_out: VecDeque::new(),
            host_to_remote: VecDeque::new(),
            remote_to_host: VecDeque::new(),
            socket: None,
            idle_deadline: std::time::Instant::now() + Duration::from_secs(30),
            registered_events: CLIENT_BASE_EVENTS | libc::EPOLLIN as u32,
            peer_half_closed: false,
        }
    }

    fn pending_bytes(&self) -> usize {
        self.host_to_remote.len() + self.remote_to_host.len()
    }

    fn epoll_events(&self, aggregate_has_space: bool) -> u32 {
        let can_read = match self.phase {
            Socks5Phase::Greeting(_) | Socks5Phase::Request(_) => true,
            Socks5Phase::Relay => {
                aggregate_has_space && self.host_to_remote.len() < MAX_SOCKS5_PENDING_BYTES
            }
            _ => false,
        };
        (CLIENT_BASE_EVENTS
            & if self.peer_half_closed {
                !libc::EPOLLRDHUP as u32
            } else {
                u32::MAX
            })
            | if can_read { libc::EPOLLIN as u32 } else { 0 }
            | if self.host_out.is_empty() && self.remote_to_host.is_empty() {
                0
            } else {
                libc::EPOLLOUT as u32
            }
    }
}

impl BoundedNetstackProof {
    #[cfg(test)]
    fn poller_fd(&self) -> RawFd {
        self.poller.raw_fd()
    }

    #[cfg(test)]
    fn poller_wait_counts(&self) -> (usize, usize) {
        self.poller.wait_counts()
    }

    #[cfg(test)]
    fn new(
        device: ServiceEthernetDevice,
        config: NetstackProofConfig,
    ) -> Result<Self, &'static str> {
        Self::new_with_poller(
            device,
            config,
            NetworkPoller::new()?,
            Socks5ResourceBudget::from_process_limit()?,
        )
    }

    fn new_with_poller(
        device: ServiceEthernetDevice,
        config: NetstackProofConfig,
        poller: NetworkPoller,
        resources: Socks5ResourceBudget,
    ) -> Result<Self, &'static str> {
        let mac = device.mac_address();
        let frame_fd = device.raw_fd();
        let runtime_capacity = resources
            .admission_capacity
            .checked_mul(2)
            .and_then(|capacity| capacity.checked_add(1))
            .ok_or("Netstack runtime capacity overflow")?;
        let runtime = Runtime::new_with_capacities(
            runtime_capacity,
            NETSTACK_QUEUE_MEMORY_BUDGET / NETSTACK_QUEUE_SLOT_CHARGE,
            (0u8..=255).cycle().take(8192),
            NonZeroU64::new(1).unwrap(),
            mac,
            u32::from(SOFTMAC_ETHERNET_MTU),
        )
        .map_err(|_| "Netstack runtime initialization failed")?;
        let runner = EthernetRunner::new(
            DhcpService::new_with_dns_capacity(
                runtime,
                StdRng::seed_from_u64(7),
                mac,
                NonZeroUsize::new(resources.admission_capacity).unwrap(),
            ),
            device,
        );
        let proxy_client = RemoteSocketProvider::open_client(
            &mut runner.stack().socket_provider(),
            NonZeroUsize::new(resources.admission_capacity).unwrap(),
        )
        .map_err(|_| "SOCKS5 provider client initialization failed")?;
        let proof = Self {
            runner,
            poller,
            config,
            now: Duration::ZERO,
            anchor: None,
            resolved: None,
            socket: None,
            proxy_client,
            admission_capacity: resources.admission_capacity,
            frame_events: BASE_EVENTS | libc::EPOLLIN as u32,
        };
        proof
            .poller
            .add(frame_fd, FRAME_TOKEN, BASE_EVENTS | libc::EPOLLIN as u32)?;
        Ok(proof)
    }

    fn drive_once(&mut self, deadline: Option<Instant>) -> Result<(bool, bool), &'static str> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err("Netstack service deadline");
        }
        self.now = self.anchor.get_or_insert_with(Instant::now).elapsed();
        let mut events = 0;
        while events < STACK_WORK_BUDGET {
            let Some(event) = self.runner.device_mut().take_event() else {
                break;
            };
            if event == netstack3_port_spike::EthernetDeviceEvent::LinkStateChanged(false) {
                self.runner.discard_pending();
                self.resolved = None;
                self.socket = None;
                return Err("Ethernet frame seam closed");
            }
            self.runner.stack_mut().on_device_event(event);
            events += 1;
        }
        let service_work = self.runner.stack_mut().poll_at(self.now, STACK_WORK_BUDGET);
        let mut frames = 0;
        while frames < STACK_WORK_BUDGET {
            let report = self.runner.pump();
            frames += report.received.max(report.transmitted);
            if report == netstack3_port_spike::PumpReport::default()
                || report.ingress_blocked
                || report.egress_blocked
            {
                break;
            }
        }
        while events < STACK_WORK_BUDGET {
            let Some(event) = self.runner.device_mut().take_event() else {
                break;
            };
            if event == netstack3_port_spike::EthernetDeviceEvent::LinkStateChanged(false) {
                self.runner.discard_pending();
                self.resolved = None;
                self.socket = None;
                return Err("Ethernet frame seam closed");
            }
            self.runner.stack_mut().on_device_event(event);
            events += 1;
        }
        let frame_events = BASE_EVENTS
            | libc::EPOLLIN as u32
            | if self.runner.device().wants_write() {
                libc::EPOLLOUT as u32
            } else {
                0
            };
        if frame_events != self.frame_events {
            self.poller
                .modify(self.runner.device().raw_fd(), FRAME_TOKEN, frame_events)?;
            self.frame_events = frame_events;
        }
        Ok((
            events == STACK_WORK_BUDGET
                || service_work >= STACK_WORK_BUDGET
                || frames >= STACK_WORK_BUDGET,
            events != 0 || service_work != 0 || frames != 0,
        ))
    }

    fn next_wait_timeout(&self, deadlines: impl IntoIterator<Item = Instant>) -> i32 {
        let stack_deadline = self
            .runner
            .stack()
            .next_timer_deadline()
            .and_then(|deadline| self.anchor.and_then(|anchor| anchor.checked_add(deadline)));
        let Some(deadline) = deadlines.into_iter().chain(stack_deadline).min() else {
            return -1;
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return 0;
        }
        let millis = remaining.as_nanos().saturating_add(999_999) / 1_000_000;
        i32::try_from(millis).unwrap_or(i32::MAX)
    }

    fn drive(&mut self, deadline: Option<Instant>) -> Result<(), &'static str> {
        let (exhausted, worked) = self.drive_once(deadline)?;
        if exhausted || worked {
            return Ok(());
        }
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; EVENT_BATCH];
        let timeout = self.next_wait_timeout(deadline);
        let ready = self.poller.wait(&mut events, timeout)?;
        for event in &events[..ready] {
            if event.u64 == FRAME_TOKEN {
                self.runner.device_mut().notify_epoll(event.events);
            }
        }
        let _ = self.drive_once(deadline)?;
        Ok(())
    }

    pub fn prove_dhcp(&mut self, deadline: std::time::Instant) -> Result<(), &'static str> {
        while self.runner.stack().status() != DhcpStatus::Bound {
            self.drive(Some(deadline))?;
        }
        Ok(())
    }

    pub fn prove_dns(&mut self, deadline: std::time::Instant) -> Result<(), &'static str> {
        if self.runner.stack().status() != DhcpStatus::Bound {
            return Err("DNS requires DHCP");
        }
        let lookup = self
            .runner
            .stack_mut()
            .lookup_ip(self.config.dns_name.clone())
            .map_err(|_| "DNS start failed")?;
        loop {
            self.drive(Some(deadline))?;
            if let Some(result) = self.runner.stack_mut().take_lookup(lookup) {
                let addresses = result.map_err(|_| "DNS lookup failed")?;
                self.resolved = addresses.into_iter().find_map(|address| match address {
                    IpAddr::V4(v4) => Some(v4.octets()),
                    _ => None,
                });
                return self
                    .resolved
                    .map(|_| ())
                    .ok_or("DNS returned no IPv4 address");
            }
        }
    }

    pub fn prove_tcp(&mut self, deadline: std::time::Instant) -> Result<(), &'static str> {
        let address = self.resolved.ok_or("TCP requires DNS")?;
        let mut provider = self.runner.stack().socket_provider();
        let client =
            RemoteSocketProvider::open_client(&mut provider, NonZeroUsize::new(1).unwrap())
                .map_err(|_| "socket client failed")?;
        let socket = provider
            .tcp_socket(client, RemoteIpVersion::V4)
            .map_err(|_| "TCP socket failed")?;
        provider
            .tcp_connect(
                socket,
                RemoteSocketAddress {
                    address: RemoteIpAddress::V4(address),
                    port: self.config.server_port,
                },
            )
            .map_err(|_| "TCP connect failed")?;
        loop {
            self.drive(Some(deadline))?;
            let ready = RemoteSocketProvider::readiness(&mut provider, socket)
                .map_err(|_| "TCP readiness failed")?;
            if ready.writable {
                self.socket = Some(socket);
                return Ok(());
            }
        }
    }

    /// True only after the bounded product-readiness proof has completed.
    /// HTTP is deliberately not part of product readiness: it is an optional
    /// lab assertion over an already-proven TCP path.
    pub fn network_ready(&self) -> bool {
        self.runner.stack().status() == DhcpStatus::Bound
            && self.resolved.is_some()
            && self.socket.is_some()
    }

    pub fn serve_socks5_listener<F>(
        &mut self,
        listener: TcpListener,
        listen: SocketAddr,
        deadline: Option<std::time::Instant>,
        mut stop_requested: F,
    ) -> Result<(), &'static str>
    where
        F: FnMut() -> bool,
    {
        let mut clients = HashMap::new();
        let mut next_token = FIRST_CLIENT_TOKEN;
        let mut ready_clients = VecDeque::new();
        let mut pending_clients = HashMap::new();
        let listener_fd = listener.as_raw_fd();
        self.poller.add(
            listener_fd,
            LISTENER_TOKEN,
            BASE_EVENTS | libc::EPOLLIN as u32,
        )?;
        let mut listener_readable = false;
        let mut listener_events = BASE_EVENTS | libc::EPOLLIN as u32;
        let mut admission_retry = None;
        let mut stack_pending = true;
        println!("internet_proxy_ready=true listen={listen}");
        while !stop_requested() && deadline.is_none_or(|deadline| Instant::now() < deadline) {
            if admission_retry.is_some_and(|retry| Instant::now() >= retry) {
                admission_retry = None;
                // Retry explicitly rather than depending on a readiness edge
                // while EPOLLIN was disabled under descriptor pressure.
                listener_readable = true;
            }
            if stack_pending {
                match self.drive_once(deadline) {
                    Ok((exhausted, worked)) => {
                        // A productive pass may leave retained device egress or
                        // newly queued protocol work. Run one terminating
                        // no-work pass before sleeping.
                        stack_pending = exhausted || worked;
                        if worked {
                            for token in clients.keys().copied() {
                                schedule_client(
                                    &mut ready_clients,
                                    &mut pending_clients,
                                    token,
                                    0,
                                    1,
                                );
                            }
                        }
                    }
                    Err(error) => {
                        for client in clients.values_mut() {
                            self.close_socks5_client(client);
                        }
                        return Err(error);
                    }
                }
            }

            for _ in 0..32 {
                if !listener_readable || clients.len() >= self.admission_capacity {
                    break;
                }
                match accept_nonblocking(listener_fd) {
                    Ok(AcceptResult::Accepted(stream, peer)) => {
                        let token = next_token;
                        next_token = next_token
                            .checked_add(1)
                            .ok_or("SOCKS5 client token exhausted")?;
                        self.poller.add(
                            stream.as_raw_fd(),
                            token,
                            BASE_EVENTS | libc::EPOLLIN as u32,
                        )?;
                        println!("internet_proxy_client=true peer={peer}");
                        clients.insert(token, Socks5Client::new(token, stream, peer));
                        schedule_client(
                            &mut ready_clients,
                            &mut pending_clients,
                            token,
                            libc::EPOLLIN as u32,
                            4,
                        );
                    }
                    Ok(AcceptResult::Drained) => {
                        listener_readable = false;
                        break;
                    }
                    Ok(AcceptResult::ResourcePressure) => {
                        listener_readable = false;
                        admission_retry = Some(Instant::now() + ADMISSION_RETRY_INTERVAL);
                        break;
                    }
                    Err(error) => {
                        for client in clients.values_mut() {
                            self.close_socks5_client(client);
                        }
                        return Err(error);
                    }
                }
            }

            let mut released_client = false;
            let expired: Vec<_> = clients
                .iter()
                .filter_map(|(token, client)| {
                    (Instant::now() >= client.idle_deadline).then_some(*token)
                })
                .collect();
            for token in expired {
                if let Some(mut client) = clients.remove(&token) {
                    released_client = true;
                    pending_clients.remove(&token);
                    self.poller.delete(client.stream.as_raw_fd())?;
                    self.close_socks5_client(&mut client);
                    println!(
                        "internet_proxy_client_error=SOCKS5 client idle timeout peer={}",
                        client.peer
                    );
                }
            }

            let mut client_mutated_stack = false;
            for _ in 0..CLIENT_WORK_BUDGET {
                let Some(token) = ready_clients.pop_front() else {
                    break;
                };
                let Some((event_mask, passes)) = pending_clients.remove(&token) else {
                    continue;
                };
                if event_mask & (libc::EPOLLERR | libc::EPOLLHUP) as u32 != 0 {
                    if let Some(mut client) = clients.remove(&token) {
                        released_client = true;
                        self.poller.delete(client.stream.as_raw_fd())?;
                        self.close_socks5_client(&mut client);
                        client_mutated_stack = true;
                    }
                    continue;
                }
                let total_pending = clients.values().map(Socks5Client::pending_bytes).sum();
                let Some(client) = clients.get_mut(&token) else {
                    continue;
                };
                if event_mask & libc::EPOLLRDHUP as u32 != 0 {
                    client.peer_half_closed = true;
                }
                let before_phase = std::mem::discriminant(&client.phase);
                let result = self.poll_socks5_client(
                    client,
                    MAX_SOCKS5_TOTAL_PENDING_BYTES.saturating_sub(total_pending),
                );
                let phase_changed = before_phase != std::mem::discriminant(&client.phase);
                match result {
                    Ok(false) => {
                        // Provider readiness sampling and socket operations can
                        // queue local stack work without changing queue lengths.
                        client_mutated_stack = true;
                        if passes > 1 || phase_changed {
                            schedule_client(
                                &mut ready_clients,
                                &mut pending_clients,
                                token,
                                event_mask,
                                passes.saturating_sub(1).max(1),
                            );
                        }
                    }
                    Ok(true) => {
                        let mut client = clients.remove(&token).expect("client exists");
                        released_client = true;
                        pending_clients.remove(&token);
                        self.poller.delete(client.stream.as_raw_fd())?;
                        self.close_socks5_client(&mut client);
                        client_mutated_stack = true;
                        println!("internet_proxy_transfer_complete=true peer={}", client.peer);
                    }
                    Err(error) => {
                        let mut client = clients.remove(&token).expect("client exists");
                        released_client = true;
                        pending_clients.remove(&token);
                        self.poller.delete(client.stream.as_raw_fd())?;
                        self.close_socks5_client(&mut client);
                        client_mutated_stack = true;
                        println!("internet_proxy_client_error={error} peer={}", client.peer);
                    }
                }
            }
            stack_pending |= client_mutated_stack;
            if released_client {
                admission_retry = None;
                listener_readable = true;
            }

            let total_pending: usize = clients.values().map(Socks5Client::pending_bytes).sum();
            for client in clients.values_mut() {
                let events = client.epoll_events(total_pending < MAX_SOCKS5_TOTAL_PENDING_BYTES);
                if events != client.registered_events {
                    self.poller
                        .modify(client.stream.as_raw_fd(), client.token, events)?;
                    client.registered_events = events;
                }
            }
            let desired_listener_events = BASE_EVENTS
                | if clients.len() < self.admission_capacity && admission_retry.is_none() {
                    libc::EPOLLIN as u32
                } else {
                    0
                };
            if desired_listener_events != listener_events {
                self.poller
                    .modify(listener_fd, LISTENER_TOKEN, desired_listener_events)?;
                listener_events = desired_listener_events;
            }

            if stack_pending || !ready_clients.is_empty() {
                continue;
            }
            let timeout = self.next_wait_timeout(
                deadline
                    .into_iter()
                    .chain(clients.values().map(|client| client.idle_deadline))
                    .chain(admission_retry),
            );
            let mut events = [libc::epoll_event { events: 0, u64: 0 }; EVENT_BATCH];
            let ready = self.poller.wait(&mut events, timeout)?;
            if ready == 0 {
                stack_pending = true;
                continue;
            }
            for event in &events[..ready] {
                match event.u64 {
                    FRAME_TOKEN => {
                        self.runner.device_mut().notify_epoll(event.events);
                        stack_pending = true;
                    }
                    LISTENER_TOKEN => listener_readable = true,
                    token => {
                        schedule_client(
                            &mut ready_clients,
                            &mut pending_clients,
                            token,
                            event.events,
                            4,
                        );
                    }
                }
            }
        }
        self.poller.delete(listener_fd)?;
        for client in clients.values_mut() {
            self.poller.delete(client.stream.as_raw_fd())?;
            self.close_socks5_client(client);
        }
        println!("internet_proxy_stopped=true");
        Ok(())
    }

    fn poll_socks5_client(
        &mut self,
        client: &mut Socks5Client,
        aggregate_remaining: usize,
    ) -> Result<bool, &'static str> {
        if std::time::Instant::now() >= client.idle_deadline {
            return Err("SOCKS5 client idle timeout");
        }
        Self::write_host_once(client)?;
        let phase = std::mem::replace(&mut client.phase, Socks5Phase::Closing);
        client.phase = match phase {
            Socks5Phase::Greeting(mut bytes) => {
                if Self::read_host_once(client, &mut bytes, MAX_SOCKS5_HANDSHAKE_BYTES)? {
                    return Ok(true);
                }
                if bytes.len() < 2 {
                    Socks5Phase::Greeting(bytes)
                } else {
                    let needed = 2 + usize::from(bytes[1]);
                    if bytes[0] != 5 {
                        return Err("SOCKS5 invalid version");
                    }
                    if bytes.len() < needed {
                        Socks5Phase::Greeting(bytes)
                    } else if !bytes[2..needed].contains(&0) {
                        client.host_out.extend([5, 0xff]);
                        Socks5Phase::Closing
                    } else {
                        client.host_out.extend([5, 0]);
                        Socks5Phase::Request(bytes.split_off(needed))
                    }
                }
            }
            Socks5Phase::Request(mut bytes) => {
                if bytes.len() < 4
                    && Self::read_host_once(client, &mut bytes, MAX_SOCKS5_HANDSHAKE_BYTES)?
                {
                    return Ok(true);
                }
                if bytes.len() < 4 {
                    Socks5Phase::Request(bytes)
                } else {
                    if bytes[..3] != [5, 1, 0] {
                        return Err("SOCKS5 only CONNECT is supported");
                    }
                    let needed = match bytes[3] {
                        1 => 10,
                        3 if bytes.len() >= 5 => 7 + usize::from(bytes[4]),
                        3 => {
                            client.phase = Socks5Phase::Request(bytes);
                            return Ok(false);
                        }
                        _ => return Err("SOCKS5 address type is unsupported"),
                    };
                    if bytes.len() < needed
                        && Self::read_host_once(client, &mut bytes, MAX_SOCKS5_HANDSHAKE_BYTES)?
                    {
                        return Ok(true);
                    }
                    if bytes.len() < needed {
                        Socks5Phase::Request(bytes)
                    } else {
                        let port_offset = needed - 2;
                        let port = NonZeroU16::new(u16::from_be_bytes([
                            bytes[port_offset],
                            bytes[port_offset + 1],
                        ]))
                        .ok_or("SOCKS5 zero port")?;
                        let pipelined = bytes.split_off(needed);
                        client.host_to_remote.extend(pipelined);
                        match bytes[3] {
                            1 => Socks5Phase::Connecting {
                                address: [bytes[4], bytes[5], bytes[6], bytes[7]],
                                port,
                            },
                            3 => {
                                let name_end = 5 + usize::from(bytes[4]);
                                let mut name = String::from_utf8(bytes[5..name_end].to_vec())
                                    .map_err(|_| "SOCKS5 invalid domain")?;
                                if !name.ends_with('.') {
                                    name.push('.');
                                }
                                match self.runner.stack_mut().lookup_ip(name) {
                                    Ok(lookup) => Socks5Phase::Dns { lookup, port },
                                    Err(_) => Self::socks5_failure(
                                        client,
                                        netstack3_port_spike::RemoteSocketError::HostUnreachable,
                                    ),
                                }
                            }
                            _ => unreachable!(),
                        }
                    }
                }
            }
            Socks5Phase::Dns { lookup, port } => {
                match self.runner.stack_mut().take_lookup(lookup) {
                    None => Socks5Phase::Dns { lookup, port },
                    Some(Err(_)) => Self::socks5_failure(
                        client,
                        netstack3_port_spike::RemoteSocketError::HostUnreachable,
                    ),
                    Some(Ok(addresses)) => {
                        client.idle_deadline = std::time::Instant::now() + Duration::from_secs(30);
                        match addresses.into_iter().find_map(|address| match address {
                            IpAddr::V4(address) => Some(address.octets()),
                            IpAddr::V6(_) => None,
                        }) {
                            Some(address) => Socks5Phase::Connecting { address, port },
                            None => Self::socks5_failure(
                                client,
                                netstack3_port_spike::RemoteSocketError::HostUnreachable,
                            ),
                        }
                    }
                }
            }
            Socks5Phase::Connecting { address, port } => 'connecting: {
                let mut provider = self.runner.stack().socket_provider();
                if client.socket.is_none() {
                    let socket = match provider.tcp_socket(self.proxy_client, RemoteIpVersion::V4) {
                        Ok(socket) => socket,
                        Err(error) => break 'connecting Self::socks5_failure(client, error),
                    };
                    if let Err(error) = RemoteSocketProviderV2::connect(
                        &mut provider,
                        socket,
                        ProviderSocketAddressV2 {
                            address: RemoteIpAddress::V4(address),
                            port: port.get(),
                        },
                    ) && error != netstack3_port_spike::RemoteSocketError::InProgress
                    {
                        let _ = RemoteSocketProviderV2::close(&mut provider, socket);
                        break 'connecting Self::socks5_failure(client, error);
                    }
                    client.socket = Some(socket);
                    client.idle_deadline = std::time::Instant::now() + Duration::from_secs(30);
                }
                let socket = client.socket.expect("socket was created");
                let ready = match RemoteSocketProviderV2::readiness(&mut provider, socket) {
                    Ok(ready) => ready,
                    Err(error) => break 'connecting Self::socks5_failure(client, error),
                };
                if ready.readiness.0
                    & (ProviderReadinessV2::CONNECT_FAILED | ProviderReadinessV2::ERROR)
                    != 0
                {
                    let error = RemoteSocketProviderV2::take_socket_error(&mut provider, socket)
                        .ok()
                        .flatten()
                        .unwrap_or(netstack3_port_spike::RemoteSocketError::InvalidState);
                    break 'connecting Self::socks5_failure(client, error);
                }
                if ready.readiness.0 & ProviderReadinessV2::CONNECTED != 0 {
                    client.host_out.extend([5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
                    println!(
                        "internet_proxy_connect=true remote={}:{} peer={}",
                        Ipv4Addr::from(address),
                        port,
                        client.peer
                    );
                    break 'connecting Socks5Phase::Reply;
                }
                Socks5Phase::Connecting { address, port }
            }
            Socks5Phase::Reply => {
                if client.host_out.is_empty() {
                    Socks5Phase::Relay
                } else {
                    Socks5Phase::Reply
                }
            }
            Socks5Phase::Relay => {
                let socket = client.socket.ok_or("SOCKS5 relay socket missing")?;
                let mut buffer = [0; 16 * 1024];
                let read_capacity = MAX_SOCKS5_PENDING_BYTES
                    .saturating_sub(client.host_to_remote.len())
                    .min(aggregate_remaining)
                    .min(buffer.len());
                if read_capacity != 0 {
                    let read = unsafe {
                        libc::read(
                            client.stream.as_raw_fd(),
                            buffer.as_mut_ptr().cast(),
                            read_capacity,
                        )
                    };
                    match read {
                        0 => return Ok(true),
                        read if read > 0 => client.host_to_remote.extend(&buffer[..read as usize]),
                        _ if std::io::Error::last_os_error().kind() == ErrorKind::WouldBlock => {}
                        _ => return Err("SOCKS5 host read failed"),
                    }
                }
                let mut provider = self.runner.stack().socket_provider();
                let ready = RemoteSocketProviderV2::readiness(&mut provider, socket)
                    .map_err(|_| "SOCKS5 relay readiness failed")?;
                if ready.readiness.0
                    & (ProviderReadinessV2::CONNECT_FAILED | ProviderReadinessV2::ERROR)
                    != 0
                {
                    return Err("SOCKS5 relay socket failed");
                }
                if ready.readiness.0 & ProviderReadinessV2::WRITABLE != 0
                    && !client.host_to_remote.is_empty()
                {
                    let written = RemoteSocketProviderV2::send_msg(
                        &mut provider,
                        socket,
                        0,
                        None,
                        client.host_to_remote.make_contiguous(),
                    )
                    .or_else(|error| {
                        (error == netstack3_port_spike::RemoteSocketError::WouldBlock)
                            .then_some(0)
                            .ok_or(error)
                    })
                    .map_err(|_| "SOCKS5 remote write failed")?;
                    client.host_to_remote.drain(..written);
                    client.host_to_remote.shrink_to_fit();
                    if written != 0 {
                        client.idle_deadline = std::time::Instant::now() + Duration::from_secs(30);
                    }
                }
                if ready.readiness.0 & ProviderReadinessV2::READABLE != 0
                    && client.remote_to_host.len() < MAX_SOCKS5_PENDING_BYTES
                    && aggregate_remaining != 0
                {
                    let receive_capacity = MAX_SOCKS5_PENDING_BYTES
                        .saturating_sub(client.remote_to_host.len())
                        .min(aggregate_remaining)
                        .min(buffer.len());
                    match RemoteSocketProviderV2::recv_msg(
                        &mut provider,
                        socket,
                        receive_capacity as u32,
                        0,
                    ) {
                        Ok(message) if message.eof => Socks5Phase::Closing,
                        Ok(message) => {
                            if !message.data.is_empty() {
                                client.idle_deadline =
                                    std::time::Instant::now() + Duration::from_secs(30);
                            }
                            client.remote_to_host.extend(&message.data);
                            Socks5Phase::Relay
                        }
                        Err(netstack3_port_spike::RemoteSocketError::WouldBlock) => {
                            Socks5Phase::Relay
                        }
                        Err(_) => return Err("SOCKS5 remote read failed"),
                    }
                } else {
                    Socks5Phase::Relay
                }
            }
            Socks5Phase::Closing => {
                if client.host_out.is_empty() && client.remote_to_host.is_empty() {
                    return Ok(true);
                }
                Socks5Phase::Closing
            }
        };
        Self::write_host_once(client)?;
        Ok(false)
    }

    fn read_host_once(
        client: &mut Socks5Client,
        bytes: &mut Vec<u8>,
        limit: usize,
    ) -> Result<bool, &'static str> {
        let mut buffer = [0; 16 * 1024];
        let capacity = limit.saturating_sub(bytes.len()).min(buffer.len());
        if capacity == 0 {
            return Err("SOCKS5 handshake exceeds buffer budget");
        }
        let read = unsafe {
            libc::read(
                client.stream.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                capacity,
            )
        };
        match read {
            0 => Ok(true),
            read if read > 0 => {
                bytes.extend_from_slice(&buffer[..read as usize]);
                client.idle_deadline = std::time::Instant::now() + Duration::from_secs(30);
                Ok(false)
            }
            _ if std::io::Error::last_os_error().kind() == ErrorKind::WouldBlock => Ok(false),
            _ => Err("SOCKS5 host read failed"),
        }
    }

    fn write_host_once(client: &mut Socks5Client) -> Result<(), &'static str> {
        let pending = if !client.host_out.is_empty() {
            &mut client.host_out
        } else {
            &mut client.remote_to_host
        };
        if pending.is_empty() {
            return Ok(());
        }
        let bytes = pending.make_contiguous();
        let written = unsafe {
            libc::write(
                client.stream.as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
            )
        };
        match written {
            0 => Err("SOCKS5 host closed during write"),
            written if written > 0 => {
                pending.drain(..written as usize);
                pending.shrink_to_fit();
                client.idle_deadline = std::time::Instant::now() + Duration::from_secs(30);
                Ok(())
            }
            _ if std::io::Error::last_os_error().kind() == ErrorKind::WouldBlock => Ok(()),
            _ => Err("SOCKS5 host write failed"),
        }
    }

    fn close_socks5_client(&mut self, client: &mut Socks5Client) {
        if let Socks5Phase::Dns { lookup, .. } = client.phase {
            self.runner.stack_mut().cancel_lookup(lookup);
        }
        let mut provider = self.runner.stack().socket_provider();
        if let Some(socket) = client.socket.take() {
            let _ = RemoteSocketProviderV2::close(&mut provider, socket);
        }
    }

    fn socks5_failure(
        client: &mut Socks5Client,
        error: netstack3_port_spike::RemoteSocketError,
    ) -> Socks5Phase {
        let reply = match error {
            netstack3_port_spike::RemoteSocketError::PermissionDenied => 2,
            netstack3_port_spike::RemoteSocketError::NetworkUnreachable => 3,
            netstack3_port_spike::RemoteSocketError::HostUnreachable => 4,
            netstack3_port_spike::RemoteSocketError::ConnectionRefused => 5,
            netstack3_port_spike::RemoteSocketError::TimedOut => 6,
            netstack3_port_spike::RemoteSocketError::NotSupported
            | netstack3_port_spike::RemoteSocketError::AddressFamilyMismatch => 8,
            _ => 1,
        };
        client.host_out.extend([5, reply, 0, 1, 0, 0, 0, 0, 0, 0]);
        Socks5Phase::Closing
    }
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;

    #[test]
    fn admission_is_derived_from_descriptor_and_connection_state_budgets() {
        assert_eq!(
            Socks5ResourceBudget::from_soft_limit(80)
                .unwrap()
                .admission_capacity,
            72
        );
        assert_eq!(
            Socks5ResourceBudget::from_soft_limit(libc::rlim_t::MAX)
                .unwrap()
                .admission_capacity,
            SOCKS5_CONNECTION_STATE_BUDGET / SOCKS5_CLIENT_STATE_CHARGE
        );
        assert!(Socks5ResourceBudget::from_soft_limit(8).is_err());
        assert!(
            size_of::<Socks5Client>() + MAX_SOCKS5_HANDSHAKE_BYTES
                < SOCKS5_CLIENT_STATE_CHARGE - 128 * 1024 - 256 * 1024
        );
    }

    #[test]
    fn runnable_queue_coalesces_and_preserves_round_robin_order() {
        let mut queue = VecDeque::new();
        let mut pending = HashMap::new();
        for token in FIRST_CLIENT_TOKEN..FIRST_CLIENT_TOKEN + 64 {
            schedule_client(&mut queue, &mut pending, token, 0, 1);
            schedule_client(&mut queue, &mut pending, token, libc::EPOLLIN as u32, 4);
        }
        assert_eq!(queue.len(), 64);
        assert_eq!(pending.len(), 64);
        assert_eq!(queue.pop_front(), Some(FIRST_CLIENT_TOKEN));
        assert_eq!(queue.pop_back(), Some(FIRST_CLIENT_TOKEN + 63));
        assert_eq!(pending[&FIRST_CLIENT_TOKEN], (libc::EPOLLIN as u32, 4));
    }

    #[test]
    fn epoll_interests_apply_relay_backpressure() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, address) = listener.accept().unwrap();
        let mut client = Socks5Client::new(FIRST_CLIENT_TOKEN, stream, address);
        client.phase = Socks5Phase::Relay;
        assert_ne!(client.epoll_events(true) & libc::EPOLLIN as u32, 0);
        assert_eq!(client.epoll_events(false) & libc::EPOLLIN as u32, 0);
        client.host_out.push_back(1);
        assert_ne!(client.epoll_events(false) & libc::EPOLLOUT as u32, 0);
        drop(peer);
    }
}
