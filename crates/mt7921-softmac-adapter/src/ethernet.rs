// SPDX-License-Identifier: GPL-2.0-only

//! Bounded Ethernet-II handoff between the pinned client MLME and Netstack3.
//!
//! The client MLME, not this module, owns 802.11 encapsulation/decapsulation,
//! controlled-port policy, and selection of protected data frames.

use netstack3_port_spike::{
    EthernetDevice, EthernetDeviceEvent, EthernetEventSource, EthernetFrame, FrameSizeError,
};
use std::collections::VecDeque;
use std::fmt;
use std::io::{ErrorKind, Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use netstack3_port_integration::{
    Runtime,
    dns_bridge::DnsLookupHandle,
    service::{DhcpService, DhcpStatus},
};
use netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2;
use netstack3_port_spike::provider_transport_v2::{ProviderReadinessV2, ProviderSocketAddressV2};
use netstack3_port_spike::{
    EthernetRunner, NetworkServiceEndpoint, RemoteIpAddress, RemoteIpVersion, RemoteSocketAddress,
    RemoteSocketHandle, RemoteSocketProvider, SocketClientId,
};
use rand::{SeedableRng as _, rngs::StdRng};

pub const MT7921_ETHERNET_MTU: u16 = 1500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EthernetPortProperties {
    pub mac_address: [u8; 6],
    pub mtu: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetPortConfigError {
    ZeroQueueCapacity,
    InvalidMacAddress,
    SocketPair,
}

impl fmt::Display for EthernetPortConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid MT7921 Ethernet port configuration: {self:?}")
    }
}

impl std::error::Error for EthernetPortConfigError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetIngressError {
    Closed,
    LinkDown,
    Backpressure,
    InvalidFrame(FrameSizeError),
}

/// The entire contract between the associated driver and Netstack3.
///
/// Implementations carry Ethernet II frames in both directions. Link policy,
/// association state, controlled-port state, and every other control verb stay
/// on the driver side of this interface.
pub trait EthernetFrameSeam {
    type Error;

    /// Attempt to send exactly one whole frame without blocking.
    fn try_send_frame(&mut self, frame: EthernetFrame) -> Result<(), (Self::Error, EthernetFrame)>;
    /// Attempt to receive exactly one whole frame without blocking.
    fn try_receive_frame(&mut self) -> Result<Option<EthernetFrame>, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SeqpacketFrameError {
    Closed,
    Backpressure,
    InvalidFrame(usize),
}

struct PortLifecycleState {
    properties: Option<EthernetPortProperties>,
    link_up: bool,
    events: VecDeque<EthernetDeviceEvent>,
}

struct SeqpacketFrameEndpoint {
    fd: Option<OwnedFd>,
    receive_notified: bool,
    transmit_blocked: bool,
}

impl SeqpacketFrameEndpoint {
    fn discard_frames(&mut self) {
        let mut bytes = [0u8; 1515];
        while unsafe { recv(self.raw_fd(), bytes.as_mut_ptr(), bytes.len(), MSG_DONTWAIT) } > 0 {}
        bytes.fill(0);
        self.receive_notified = false;
        self.transmit_blocked = false;
    }

    fn close(&mut self) {
        self.fd.take();
    }

    fn take_event(&mut self) -> Option<EthernetDeviceEvent> {
        let mut descriptor = PollFd {
            fd: self.raw_fd(),
            events: POLLIN | if self.transmit_blocked { POLLOUT } else { 0 },
            revents: 0,
        };
        if unsafe { poll(&mut descriptor, 1, 0) } <= 0 {
            return None;
        }
        if descriptor.revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
            return Some(EthernetDeviceEvent::LinkStateChanged(false));
        }
        if descriptor.revents & POLLIN != 0 && !self.receive_notified {
            self.receive_notified = true;
            return Some(EthernetDeviceEvent::ReceiveReady);
        }
        if descriptor.revents & POLLOUT != 0 && self.transmit_blocked {
            self.transmit_blocked = false;
            return Some(EthernetDeviceEvent::TransmitReady);
        }
        None
    }

    fn raw_fd(&self) -> RawFd {
        self.fd.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    fn take_fd(&mut self) -> OwnedFd {
        self.fd.take().expect("frame endpoint is open")
    }
}

impl Drop for SeqpacketFrameEndpoint {
    fn drop(&mut self) {
        self.close();
    }
}

impl EthernetFrameSeam for SeqpacketFrameEndpoint {
    type Error = SeqpacketFrameError;

    /// Attempt to send exactly one whole frame without blocking.
    fn try_send_frame(&mut self, frame: EthernetFrame) -> Result<(), (Self::Error, EthernetFrame)> {
        let sent = unsafe {
            send(
                self.raw_fd(),
                frame.as_bytes().as_ptr(),
                frame.as_bytes().len(),
                MSG_DONTWAIT | MSG_NOSIGNAL,
            )
        };
        if sent == frame.as_bytes().len() as isize {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        let kind = if error.kind() == ErrorKind::WouldBlock {
            self.transmit_blocked = true;
            SeqpacketFrameError::Backpressure
        } else {
            SeqpacketFrameError::Closed
        };
        Err((kind, frame))
    }

    fn try_receive_frame(&mut self) -> Result<Option<EthernetFrame>, Self::Error> {
        let mut bytes = [0u8; 1514];
        let received = unsafe {
            recv(
                self.raw_fd(),
                bytes.as_mut_ptr(),
                bytes.len(),
                MSG_DONTWAIT | MSG_TRUNC,
            )
        };
        if received == 0 {
            return Err(SeqpacketFrameError::Closed);
        }
        if received < 0 {
            return match std::io::Error::last_os_error().kind() {
                ErrorKind::WouldBlock => {
                    self.receive_notified = false;
                    Ok(None)
                }
                _ => Err(SeqpacketFrameError::Closed),
            };
        }
        let received = received as usize;
        if received > 1514 {
            return Err(SeqpacketFrameError::InvalidFrame(received));
        }
        EthernetFrame::copy_from_slice(&bytes[..received])
            .map(Some)
            .map_err(|_| SeqpacketFrameError::InvalidFrame(received))
    }
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

const AF_UNIX: i32 = 1;
const SOCK_SEQPACKET: i32 = 5;
const SOCK_NONBLOCK: i32 = 0x800;
const SOCK_CLOEXEC: i32 = 0x80000;
const MSG_DONTWAIT: i32 = 0x40;
const MSG_TRUNC: i32 = 0x20;
const MSG_NOSIGNAL: i32 = 0x4000;
const POLLIN: i16 = 0x001;
const POLLOUT: i16 = 0x004;
const POLLERR: i16 = 0x008;
const POLLHUP: i16 = 0x010;
const POLLNVAL: i16 = 0x020;

unsafe extern "C" {
    fn socketpair(domain: i32, socket_type: i32, protocol: i32, sockets: *mut i32) -> i32;
    fn send(fd: i32, bytes: *const u8, len: usize, flags: i32) -> isize;
    fn recv(fd: i32, bytes: *mut u8, len: usize, flags: i32) -> isize;
    fn poll(fds: *mut PollFd, count: usize, timeout_ms: i32) -> i32;
    fn fcntl(fd: i32, command: i32, ...) -> i32;
}

fn set_nonblocking(fd: RawFd) -> Result<(), ()> {
    const F_GETFL: i32 = 3;
    const F_SETFL: i32 = 4;
    const O_NONBLOCK: i32 = 0x800;
    let flags = unsafe { fcntl(fd, F_GETFL) };
    if flags < 0 || unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } < 0 {
        Err(())
    } else {
        Ok(())
    }
}

/// Netstack3-facing half of the port.
pub struct Mt7921EthernetDevice {
    seam: SeqpacketFrameEndpoint,
    lifecycle: Arc<Mutex<PortLifecycleState>>,
}

/// Driver-side endpoint retained by `Mt7921ClientDevice`. The controlled-port
/// gate is deliberately outside [`EthernetFrameSeam`].
pub(crate) struct DriverEthernetPort {
    seam: SeqpacketFrameEndpoint,
    lifecycle: Arc<Mutex<PortLifecycleState>>,
}

pub trait AssociatedSoftmacTx {
    type Error;

    /// Accept one Ethernet II frame without FCS. Success transfers ownership
    /// to the associated SoftMAC path.
    fn transmit_ethernet(&mut self, frame: &[u8]) -> Result<(), Self::Error>;
}

/// Production RX companion to [`AssociatedSoftmacTx`]. One call admits at
/// most one already-validated associated data frame into the MLME Ethernet
/// sink; it must not wait beyond `deadline`.
pub trait AssociatedDataPump: AssociatedSoftmacTx {
    fn pump_transmit(&mut self) -> Result<bool, EthernetTxPumpError<Self::Error>>;
    fn pump_receive(&mut self, deadline: std::time::Instant) -> Result<bool, Self::Error>;
}

#[derive(Clone)]
pub struct NetstackProofConfig {
    pub dns_name: String,
    pub server_port: NonZeroU16,
}

/// The existing Netstack3 DHCP/DNS/socket stack wired to the MT7921 Ethernet
/// port. This is a bounded driver, not a DHCP, DNS, or TCP implementation.
pub struct BoundedNetstackProof {
    runner: EthernetRunner<DhcpService, Mt7921EthernetDevice>,
    config: NetstackProofConfig,
    now: Duration,
    anchor: Option<std::time::Instant>,
    resolved: Option<[u8; 4]>,
    socket: Option<netstack3_port_spike::RemoteSocketHandle>,
}

const MAX_SOCKS5_CLIENTS: usize = 24;
const MAX_SOCKS5_PENDING_BYTES: usize = 256 * 1024;

struct Socks5Client {
    stream: TcpStream,
    peer: SocketAddr,
    phase: Socks5Phase,
    host_out: VecDeque<u8>,
    host_to_remote: VecDeque<u8>,
    remote_to_host: VecDeque<u8>,
    socket_client: Option<SocketClientId>,
    socket: Option<RemoteSocketHandle>,
    idle_deadline: std::time::Instant,
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
    fn new(stream: TcpStream, peer: SocketAddr) -> Self {
        Self {
            stream,
            peer,
            phase: Socks5Phase::Greeting(Vec::new()),
            host_out: VecDeque::new(),
            host_to_remote: VecDeque::new(),
            remote_to_host: VecDeque::new(),
            socket_client: None,
            socket: None,
            idle_deadline: std::time::Instant::now() + Duration::from_secs(30),
        }
    }
}

impl BoundedNetstackProof {
    pub fn new(
        device: Mt7921EthernetDevice,
        config: NetstackProofConfig,
    ) -> Result<Self, &'static str> {
        let mac = device
            .properties()
            .ok_or("Ethernet port is closed")?
            .mac_address;
        let runtime = Runtime::new(
            32,
            (0u8..=255).cycle().take(8192),
            NonZeroU64::new(1).unwrap(),
            mac,
            u32::from(MT7921_ETHERNET_MTU),
        )
        .map_err(|_| "Netstack runtime initialization failed")?;
        Ok(Self {
            runner: EthernetRunner::new(
                DhcpService::new(runtime, StdRng::seed_from_u64(7), mac),
                device,
            ),
            config,
            now: Duration::ZERO,
            anchor: None,
            resolved: None,
            socket: None,
        })
    }

    fn drive(&mut self, deadline: std::time::Instant) -> Result<(), &'static str> {
        if std::time::Instant::now() >= deadline {
            return Err("Netstack proof deadline");
        }
        self.now = self
            .anchor
            .get_or_insert_with(std::time::Instant::now)
            .elapsed();
        for _ in 0..8 {
            while let Some(event) = self.runner.device_mut().take_event() {
                if event == netstack3_port_spike::EthernetDeviceEvent::LinkStateChanged(false) {
                    self.runner.discard_pending();
                    self.resolved = None;
                    self.socket = None;
                    return Err("Ethernet frame seam closed");
                }
                self.runner.stack_mut().on_device_event(event);
            }
            self.runner.stack_mut().poll_at(self.now, 64);
            while self.runner.pump().transmitted != 0 {}
            while self.runner.pump().received != 0 {}
        }
        std::thread::sleep(Duration::from_millis(1));
        Ok(())
    }

    pub fn prove_dhcp(&mut self, deadline: std::time::Instant) -> Result<(), &'static str> {
        while self.runner.stack().status() != DhcpStatus::Bound {
            self.drive(deadline)?;
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
            self.drive(deadline)?;
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
            self.drive(deadline)?;
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

    /// Serve SOCKS5 CONNECT requests through the same Netstack3 instance used
    /// by the bounded bring-up proof. The host listener is only a byte-stream
    /// handoff: all remote DNS and TCP traffic goes through Netstack3 and the
    /// associated SoftMAC data pump.
    pub fn serve_socks5<F>(
        &mut self,
        listen: SocketAddr,
        deadline: std::time::Instant,
        stop_requested: F,
    ) -> Result<(), &'static str>
    where
        F: FnMut() -> bool,
    {
        if !self.network_ready() {
            return Err("SOCKS5 requires DHCP, DNS, and TCP proof");
        }
        let listener = TcpListener::bind(listen).map_err(|_| "SOCKS5 bind failed")?;
        set_nonblocking(listener.as_raw_fd()).map_err(|_| "SOCKS5 nonblocking setup failed")?;
        self.serve_socks5_listener(listener, listen, deadline, stop_requested)
    }

    pub fn serve_socks5_listener<F>(
        &mut self,
        listener: TcpListener,
        listen: SocketAddr,
        deadline: std::time::Instant,
        mut stop_requested: F,
    ) -> Result<(), &'static str>
    where
        F: FnMut() -> bool,
    {
        if !self.network_ready() {
            return Err("SOCKS5 requires DHCP, DNS, and TCP proof");
        }
        let mut clients = Vec::new();
        println!("internet_proxy_ready=true listen={listen}");
        while !stop_requested() && std::time::Instant::now() < deadline {
            // Exactly one shared Netstack3 drive precedes a bounded amount of
            // work for every client. No client-specific wait can delay another.
            if let Err(error) = self.drive(deadline) {
                for client in &mut clients {
                    self.close_socks5_client(client);
                }
                return Err(error);
            }
            for _ in 0..32 {
                match listener.accept() {
                    Ok((stream, peer)) if clients.len() < MAX_SOCKS5_CLIENTS => {
                        if set_nonblocking(stream.as_raw_fd()).is_err() {
                            println!(
                                "internet_proxy_client_error=SOCKS5 client nonblocking setup failed peer={peer}"
                            );
                            continue;
                        }
                        println!("internet_proxy_client=true peer={peer}");
                        clients.push(Socks5Client::new(stream, peer));
                    }
                    Ok((_stream, peer)) => {
                        println!("internet_proxy_client_error=SOCKS5 client limit peer={peer}");
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => {
                        for client in &mut clients {
                            self.close_socks5_client(client);
                        }
                        return Err("SOCKS5 accept failed");
                    }
                }
            }

            let mut index = 0;
            while index < clients.len() {
                match self.poll_socks5_client(&mut clients[index]) {
                    Ok(false) => index += 1,
                    Ok(true) => {
                        let mut client = clients.swap_remove(index);
                        self.close_socks5_client(&mut client);
                        println!("internet_proxy_transfer_complete=true peer={}", client.peer);
                    }
                    Err(error) => {
                        let mut client = clients.swap_remove(index);
                        self.close_socks5_client(&mut client);
                        println!("internet_proxy_client_error={error} peer={}", client.peer);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        for client in &mut clients {
            self.close_socks5_client(client);
        }
        println!("internet_proxy_stopped=true");
        Ok(())
    }

    fn poll_socks5_client(&mut self, client: &mut Socks5Client) -> Result<bool, &'static str> {
        if std::time::Instant::now() >= client.idle_deadline {
            return Err("SOCKS5 client idle timeout");
        }
        Self::write_host_once(client)?;
        let phase = std::mem::replace(&mut client.phase, Socks5Phase::Closing);
        client.phase = match phase {
            Socks5Phase::Greeting(mut bytes) => {
                if Self::read_host_once(client, &mut bytes)? {
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
                if bytes.len() < 4 && Self::read_host_once(client, &mut bytes)? {
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
                    if bytes.len() < needed {
                        if Self::read_host_once(client, &mut bytes)? {
                            return Ok(true);
                        }
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
                                let lookup = self
                                    .runner
                                    .stack_mut()
                                    .lookup_ip(name)
                                    .map_err(|_| "SOCKS5 DNS start failed")?;
                                Socks5Phase::Dns { lookup, port }
                            }
                            _ => unreachable!(),
                        }
                    }
                }
            }
            Socks5Phase::Dns { lookup, port } => {
                match self.runner.stack_mut().take_lookup(lookup) {
                    None => Socks5Phase::Dns { lookup, port },
                    Some(result) => {
                        client.idle_deadline = std::time::Instant::now() + Duration::from_secs(30);
                        let address = result
                            .map_err(|_| "SOCKS5 DNS lookup failed")?
                            .into_iter()
                            .find_map(|address| match address {
                                IpAddr::V4(address) => Some(address.octets()),
                                IpAddr::V6(_) => None,
                            })
                            .ok_or("SOCKS5 DNS returned no IPv4 address")?;
                        Socks5Phase::Connecting { address, port }
                    }
                }
            }
            Socks5Phase::Connecting { address, port } => {
                if client.socket.is_none() {
                    let mut provider = self.runner.stack().socket_provider();
                    let socket_client = RemoteSocketProvider::open_client(
                        &mut provider,
                        NonZeroUsize::new(1).unwrap(),
                    )
                    .map_err(|_| "SOCKS5 socket client failed")?;
                    let socket = provider
                        .tcp_socket(socket_client, RemoteIpVersion::V4)
                        .map_err(|_| "SOCKS5 TCP socket failed")?;
                    match RemoteSocketProviderV2::connect(
                        &mut provider,
                        socket,
                        ProviderSocketAddressV2 {
                            address: RemoteIpAddress::V4(address),
                            port: port.get(),
                        },
                    ) {
                        Ok(()) | Err(netstack3_port_spike::RemoteSocketError::InProgress) => {}
                        Err(_) => {
                            let _ = RemoteSocketProviderV2::close(&mut provider, socket);
                            let _ =
                                RemoteSocketProvider::close_client(&mut provider, socket_client);
                            return Err("SOCKS5 TCP connect failed");
                        }
                    }
                    client.socket_client = Some(socket_client);
                    client.socket = Some(socket);
                    client.idle_deadline = std::time::Instant::now() + Duration::from_secs(30);
                }
                let mut provider = self.runner.stack().socket_provider();
                let ready = RemoteSocketProviderV2::readiness(
                    &mut provider,
                    client.socket.expect("socket was created"),
                )
                .map_err(|_| "SOCKS5 TCP readiness failed")?;
                if ready.readiness.0
                    & (ProviderReadinessV2::CONNECT_FAILED | ProviderReadinessV2::ERROR)
                    != 0
                {
                    return Err("SOCKS5 TCP connect failed");
                }
                if ready.readiness.0 & ProviderReadinessV2::CONNECTED != 0 {
                    client.host_out.extend([5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
                    println!(
                        "internet_proxy_connect=true remote={}:{} peer={}",
                        Ipv4Addr::from(address),
                        port,
                        client.peer
                    );
                    Socks5Phase::Reply
                } else {
                    Socks5Phase::Connecting { address, port }
                }
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
                if client.host_to_remote.len() < MAX_SOCKS5_PENDING_BYTES {
                    match client.stream.read(&mut buffer) {
                        Ok(0) => return Ok(true),
                        Ok(read) => client.host_to_remote.extend(&buffer[..read]),
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                        Err(_) => return Err("SOCKS5 host read failed"),
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
                    if written != 0 {
                        client.idle_deadline = std::time::Instant::now() + Duration::from_secs(30);
                    }
                }
                if ready.readiness.0 & ProviderReadinessV2::READABLE != 0
                    && client.remote_to_host.len() < MAX_SOCKS5_PENDING_BYTES
                {
                    match RemoteSocketProviderV2::recv_msg(
                        &mut provider,
                        socket,
                        buffer.len() as u32,
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
    ) -> Result<bool, &'static str> {
        let mut buffer = [0; 16 * 1024];
        match client.stream.read(&mut buffer) {
            Ok(0) => Ok(true),
            Ok(read) => {
                bytes.extend_from_slice(&buffer[..read]);
                client.idle_deadline = std::time::Instant::now() + Duration::from_secs(30);
                Ok(false)
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(false),
            Err(_) => Err("SOCKS5 host read failed"),
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
        match client.stream.write(pending.make_contiguous()) {
            Ok(0) => Err("SOCKS5 host closed during write"),
            Ok(written) => {
                pending.drain(..written);
                client.idle_deadline = std::time::Instant::now() + Duration::from_secs(30);
                Ok(())
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(()),
            Err(_) => Err("SOCKS5 host write failed"),
        }
    }

    fn close_socks5_client(&mut self, client: &mut Socks5Client) {
        let mut provider = self.runner.stack().socket_provider();
        if let Some(socket) = client.socket.take() {
            let _ = RemoteSocketProviderV2::close(&mut provider, socket);
        }
        if let Some(socket_client) = client.socket_client.take() {
            let _ = RemoteSocketProvider::close_client(&mut provider, socket_client);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetTxPumpError<E> {
    Closed,
    LinkDown,
    Target(E),
}

pub(crate) fn ethernet_port(
    mac_address: [u8; 6],
    queue_capacity: usize,
) -> Result<(Mt7921EthernetDevice, DriverEthernetPort), EthernetPortConfigError> {
    if queue_capacity == 0 {
        return Err(EthernetPortConfigError::ZeroQueueCapacity);
    }
    if mac_address == [0; 6] || mac_address[0] & 1 != 0 {
        return Err(EthernetPortConfigError::InvalidMacAddress);
    }
    let mut sockets = [-1; 2];
    if unsafe {
        socketpair(
            AF_UNIX,
            SOCK_SEQPACKET | SOCK_NONBLOCK | SOCK_CLOEXEC,
            0,
            sockets.as_mut_ptr(),
        )
    } != 0
    {
        return Err(EthernetPortConfigError::SocketPair);
    }
    let driver_fd = unsafe { OwnedFd::from_raw_fd(sockets[0]) };
    let netstack_fd = unsafe { OwnedFd::from_raw_fd(sockets[1]) };
    let lifecycle = Arc::new(Mutex::new(PortLifecycleState {
        properties: Some(EthernetPortProperties {
            mac_address,
            mtu: MT7921_ETHERNET_MTU,
        }),
        link_up: false,
        events: VecDeque::with_capacity(queue_capacity.saturating_mul(2).saturating_add(1)),
    }));
    Ok((
        Mt7921EthernetDevice {
            seam: SeqpacketFrameEndpoint {
                fd: Some(netstack_fd),
                receive_notified: false,
                transmit_blocked: false,
            },
            lifecycle: lifecycle.clone(),
        },
        DriverEthernetPort {
            seam: SeqpacketFrameEndpoint {
                fd: Some(driver_fd),
                receive_notified: false,
                transmit_blocked: false,
            },
            lifecycle,
        },
    ))
}

impl Mt7921EthernetDevice {
    pub fn properties(&self) -> Option<EthernetPortProperties> {
        self.lifecycle.lock().unwrap().properties
    }

    pub fn into_frame_fd(mut self) -> OwnedFd {
        self.seam.take_fd()
    }

    /// Reconstruct the netstack side from the sole frame capability passed by
    /// the trusted launcher.
    pub unsafe fn from_frame_fd(fd: OwnedFd, mac_address: [u8; 6]) -> Self {
        Self {
            seam: SeqpacketFrameEndpoint {
                fd: Some(fd),
                receive_notified: false,
                transmit_blocked: false,
            },
            lifecycle: Arc::new(Mutex::new(PortLifecycleState {
                properties: Some(EthernetPortProperties {
                    mac_address,
                    mtu: MT7921_ETHERNET_MTU,
                }),
                link_up: true,
                events: VecDeque::from([EthernetDeviceEvent::LinkStateChanged(true)]),
            })),
        }
    }
}

impl EthernetDevice for Mt7921EthernetDevice {
    fn receive(&mut self) -> Option<EthernetFrame> {
        let state = self.lifecycle.lock().unwrap();
        if state.properties.is_none() || !state.link_up {
            return None;
        }
        self.seam.try_receive_frame().ok().flatten()
    }

    fn transmit(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
        let state = self.lifecycle.lock().unwrap();
        if state.properties.is_none() || !state.link_up {
            return Err(frame);
        }
        self.seam.try_send_frame(frame).map_err(|(_, frame)| frame)
    }
}

impl EthernetEventSource for Mt7921EthernetDevice {
    fn take_event(&mut self) -> Option<EthernetDeviceEvent> {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        let event = lifecycle.events.pop_front();
        if event.is_some() || !lifecycle.link_up || lifecycle.properties.is_none() {
            return event;
        }
        drop(lifecycle);
        self.seam.take_event()
    }
}

impl<D: wlan_mlme::device::DeviceOps> AssociatedSoftmacTx for wlan_mlme::client::ClientMlme<D> {
    type Error = anyhow::Error;

    fn transmit_ethernet(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        wlan_mlme::MlmeImpl::handle_eth_frame_tx(self, frame, fuchsia_trace::Id::new())
    }
}

/// The sole production associated-data owner. Both directions pass through
/// the same pinned `ClientMlme`: TX enters its associated Ethernet handler and
/// RX enters its raw MAC handler through the MT7921 runner. Consequently this
/// type cannot be constructed around `OpenClientMlme` or an independently
/// maintained association state.
pub struct PinnedAssociatedDataPump<'a, E, T> {
    mlme: &'a mut wlan_mlme::client::ClientMlme<
        crate::client_device::Mt7921ClientDevice<E, crate::Mt7921SoftmacAdapter<T>>,
    >,
    runner: &'a crate::client_device::Mt7921ScanRunner<E, T>,
}

#[derive(Debug)]
pub enum PinnedDataPumpError {
    Tx(anyhow::Error),
    Rx(zx::Status),
}

impl std::fmt::Display for PinnedDataPumpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tx(error) => write!(formatter, "pinned client Ethernet TX failed: {error}"),
            Self::Rx(status) => write!(formatter, "pinned client MAC RX failed: {status}"),
        }
    }
}

impl std::error::Error for PinnedDataPumpError {}

impl<'a, E, T> PinnedAssociatedDataPump<'a, E, T>
where
    E: crate::client_device::Mt7921ClientEffects,
    T: crate::Mt7921PassiveTransport,
{
    pub fn new(
        mlme: &'a mut wlan_mlme::client::ClientMlme<
            crate::client_device::Mt7921ClientDevice<E, crate::Mt7921SoftmacAdapter<T>>,
        >,
        runner: &'a crate::client_device::Mt7921ScanRunner<E, T>,
    ) -> Self {
        Self { mlme, runner }
    }
}

impl<E, T> AssociatedSoftmacTx for PinnedAssociatedDataPump<'_, E, T>
where
    E: crate::client_device::Mt7921ClientEffects,
    T: crate::Mt7921PassiveTransport,
{
    type Error = PinnedDataPumpError;

    fn transmit_ethernet(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        wlan_mlme::MlmeImpl::handle_eth_frame_tx(self.mlme, frame, fuchsia_trace::Id::new())
            .map_err(PinnedDataPumpError::Tx)
    }
}

impl<E, T> AssociatedDataPump for PinnedAssociatedDataPump<'_, E, T>
where
    E: crate::client_device::Mt7921ClientEffects,
    T: crate::Mt7921PassiveTransport,
{
    fn pump_transmit(&mut self) -> Result<bool, EthernetTxPumpError<Self::Error>> {
        // Pop the owned frame under the backend lock, then release it before
        // entering ClientMlme: DeviceOps TX re-enters that same backend.
        let frame = self
            .runner
            .take_ethernet_transmit()
            .map_err(|status| match status {
                zx::Status::CANCELED => EthernetTxPumpError::Closed,
                zx::Status::BAD_STATE => EthernetTxPumpError::LinkDown,
                status => EthernetTxPumpError::Target(PinnedDataPumpError::Rx(status)),
            })?;
        let Some(frame) = frame else { return Ok(false) };
        self.transmit_ethernet(frame.as_bytes())
            .map_err(EthernetTxPumpError::Target)?;
        Ok(true)
    }

    fn pump_receive(&mut self, deadline: std::time::Instant) -> Result<bool, Self::Error> {
        if std::time::Instant::now() >= deadline {
            return Err(PinnedDataPumpError::Rx(zx::Status::TIMED_OUT));
        }
        futures::executor::block_on(self.runner.pump_client_rx(self.mlme))
            .map_err(PinnedDataPumpError::Rx)
    }
}

impl DriverEthernetPort {
    pub(crate) fn deliver(&mut self, bytes: &[u8]) -> Result<(), EthernetIngressError> {
        let frame =
            EthernetFrame::copy_from_slice(bytes).map_err(EthernetIngressError::InvalidFrame)?;
        let state = self.lifecycle.lock().unwrap();
        if state.properties.is_none() {
            return Err(EthernetIngressError::Closed);
        }
        if !state.link_up {
            return Err(EthernetIngressError::LinkDown);
        }
        self.seam
            .try_send_frame(frame)
            .map_err(|(error, _)| match error {
                SeqpacketFrameError::Closed => EthernetIngressError::Closed,
                SeqpacketFrameError::Backpressure => EthernetIngressError::Backpressure,
                SeqpacketFrameError::InvalidFrame(len) => {
                    EthernetIngressError::InvalidFrame(if len < 14 {
                        FrameSizeError::TooShort { len }
                    } else {
                        FrameSizeError::TooLong { len }
                    })
                }
            })
    }

    pub(crate) fn take_transmit(&mut self) -> Result<Option<EthernetFrame>, EthernetIngressError> {
        let state = self.lifecycle.lock().unwrap();
        if state.properties.is_none() {
            return Err(EthernetIngressError::Closed);
        }
        if !state.link_up {
            return Err(EthernetIngressError::LinkDown);
        }
        self.seam.try_receive_frame().map_err(|error| match error {
            SeqpacketFrameError::Closed => EthernetIngressError::Closed,
            SeqpacketFrameError::Backpressure => unreachable!(),
            SeqpacketFrameError::InvalidFrame(len) => {
                EthernetIngressError::InvalidFrame(if len < 14 {
                    FrameSizeError::TooShort { len }
                } else {
                    FrameSizeError::TooLong { len }
                })
            }
        })
    }

    pub(crate) fn set_link(&mut self, up: bool) {
        let mut state = self.lifecycle.lock().unwrap();
        if state.properties.is_some() && state.link_up != up {
            state.link_up = up;
            if !up {
                self.seam.discard_frames();
                self.seam.close();
                state.events.retain(|event| {
                    !matches!(
                        event,
                        EthernetDeviceEvent::ReceiveReady | EthernetDeviceEvent::TransmitReady
                    )
                });
            }
            push_event(&mut state.events, EthernetDeviceEvent::LinkStateChanged(up));
        }
    }

    pub(crate) fn teardown(&mut self) {
        let mut state = self.lifecycle.lock().unwrap();
        if state.properties.is_none() {
            return;
        }
        state.events.clear();
        if state.link_up {
            push_event(
                &mut state.events,
                EthernetDeviceEvent::LinkStateChanged(false),
            );
        }
        state.link_up = false;
        state.properties = None;
        self.seam.close();
    }
}

impl Drop for DriverEthernetPort {
    fn drop(&mut self) {
        self.teardown();
    }
}

fn push_event(events: &mut VecDeque<EthernetDeviceEvent>, event: EthernetDeviceEvent) {
    if let EthernetDeviceEvent::LinkStateChanged(_) = event {
        events.retain(|queued| !matches!(queued, EthernetDeviceEvent::LinkStateChanged(_)));
    } else if events.contains(&event) {
        return;
    }
    events.push_back(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use netstack3_port_spike::{EthernetRunner, StackEthernetEndpoint};

    fn frame(ether_type: [u8; 2], marker: u8) -> EthernetFrame {
        let mut bytes = vec![0; 14 + 32];
        bytes[..6].copy_from_slice(&[0xff; 6]);
        bytes[6..12].copy_from_slice(&[2, 0, 0, 0, 0, 1]);
        bytes[12..14].copy_from_slice(&ether_type);
        bytes[14] = marker;
        EthernetFrame::try_from(bytes).unwrap()
    }

    #[derive(Default)]
    struct TxTarget {
        frames: Vec<Vec<u8>>,
        blocked: bool,
    }

    impl AssociatedSoftmacTx for TxTarget {
        type Error = ();
        fn transmit_ethernet(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
            if self.blocked {
                Err(())
            } else {
                self.frames.push(frame.to_vec());
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct StackEndpoint(Vec<EthernetFrame>);

    impl StackEthernetEndpoint for StackEndpoint {
        fn receive_frame(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
            self.0.push(frame);
            Ok(())
        }

        fn take_transmit(&mut self) -> Option<EthernetFrame> {
            None
        }
    }

    #[test]
    fn mlme_rx_enters_existing_netstack_device_boundary() {
        let (device, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 2).unwrap();
        let mut runner = EthernetRunner::new(StackEndpoint::default(), device);
        sink.set_link(true);
        let ipv4 = frame([0x08, 0x00], 7);
        sink.deliver(ipv4.as_bytes()).unwrap();
        assert_eq!(
            runner.device_mut().take_event(),
            Some(EthernetDeviceEvent::LinkStateChanged(true))
        );
        assert_eq!(
            runner.device_mut().take_event(),
            Some(EthernetDeviceEvent::ReceiveReady)
        );
        assert_eq!(runner.pump().received, 1);
        assert_eq!(runner.stack().0, vec![ipv4]);
    }

    #[test]
    fn arp_dhcp_and_data_leave_through_softmac_tx_facade() {
        let (mut device, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 3).unwrap();
        sink.set_link(true);
        let expected = [
            frame([0x08, 0x06], 1),
            frame([0x08, 0x00], 2),
            frame([0x86, 0xdd], 3),
        ];
        for frame in expected.clone() {
            device.transmit(frame).unwrap()
        }
        let mut target = TxTarget::default();
        for _ in 0..3 {
            let frame = sink.take_transmit().unwrap().unwrap();
            target.transmit_ethernet(frame.as_bytes()).unwrap();
        }
        assert_eq!(sink.take_transmit(), Ok(None));
        assert_eq!(target.frames, expected.map(EthernetFrame::into_vec));
    }

    #[test]
    fn backpressure_link_lifecycle_and_teardown_are_bounded() {
        let (mut device, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        assert_eq!(device.properties().unwrap().mtu, 1500);
        let arp = frame([0x08, 0x06], 1);
        assert_eq!(device.transmit(arp.clone()), Err(arp.clone()));
        sink.set_link(true);
        device.transmit(arp.clone()).unwrap();
        let data = frame([0x08, 0x00], 2);
        let mut queued = 1;
        loop {
            match device.transmit(data.clone()) {
                Ok(()) => queued += 1,
                Err(frame) => {
                    assert_eq!(frame, data);
                    break;
                }
            }
        }
        assert!(queued > 1);
        let mut target = TxTarget {
            blocked: true,
            ..Default::default()
        };
        let frame = sink.take_transmit().unwrap().unwrap();
        assert_eq!(target.transmit_ethernet(frame.as_bytes()), Err(()));
        sink.teardown();
        assert_eq!(device.properties(), None);
        assert_eq!(device.receive(), None);
        assert_eq!(device.transmit(arp.clone()), Err(arp));
        assert_eq!(sink.take_transmit(), Err(EthernetIngressError::Closed));
    }

    #[test]
    fn link_down_discards_frames_and_stale_readiness_from_old_association() {
        let (mut device, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 2).unwrap();
        sink.set_link(true);
        sink.deliver(frame([0x08, 0x00], 1).as_bytes()).unwrap();
        device.transmit(frame([0x08, 0x06], 2)).unwrap();

        sink.set_link(false);

        assert_eq!(
            device.take_event(),
            Some(EthernetDeviceEvent::LinkStateChanged(false))
        );
        assert_eq!(device.take_event(), None);
        assert_eq!(device.receive(), None);
        assert_eq!(sink.take_transmit(), Err(EthernetIngressError::LinkDown));
    }

    #[test]
    fn invalid_frames_and_addresses_do_not_cross_the_boundary() {
        assert!(matches!(
            ethernet_port([0; 6], 1),
            Err(EthernetPortConfigError::InvalidMacAddress)
        ));
        let (_, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        sink.set_link(true);
        assert_eq!(
            sink.deliver(&[0; 13]),
            Err(EthernetIngressError::InvalidFrame(
                FrameSizeError::TooShort { len: 13 }
            ))
        );
        assert_eq!(
            sink.deliver(&[0; 1515]),
            Err(EthernetIngressError::InvalidFrame(
                FrameSizeError::TooLong { len: 1515 }
            ))
        );
    }

    #[test]
    fn dropping_netstack_endpoint_closes_driver_peer() {
        let (device, mut driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        driver.set_link(true);
        drop(device);
        assert_eq!(
            driver.deliver(frame([0x08, 0x00], 1).as_bytes()),
            Err(EthernetIngressError::Closed)
        );
    }

    #[test]
    fn dropping_driver_endpoint_tears_down_netstack_facade() {
        let (mut device, mut driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        driver.set_link(true);
        drop(driver);
        assert_eq!(device.properties(), None);
        assert_eq!(
            device.take_event(),
            Some(EthernetDeviceEvent::LinkStateChanged(false))
        );
        let frame = frame([0x08, 0x00], 1);
        assert_eq!(device.transmit(frame.clone()), Err(frame));
    }
}

#[cfg(test)]
#[path = "associated_runtime_test.rs"]
mod associated_runtime_test;
