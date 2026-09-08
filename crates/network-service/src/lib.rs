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
use std::collections::VecDeque;
use std::io::{ErrorKind, Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};
use std::time::Duration;

mod child;
mod ethernet_device;

pub use child::{run, run_lab};
use ethernet_device::ServiceEthernetDevice;

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
    fn new(
        device: ServiceEthernetDevice,
        config: NetstackProofConfig,
    ) -> Result<Self, &'static str> {
        let mac = device.mac_address();
        let runtime = Runtime::new(
            32,
            (0u8..=255).cycle().take(8192),
            NonZeroU64::new(1).unwrap(),
            mac,
            u32::from(SOFTMAC_ETHERNET_MTU),
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

    fn drive(&mut self, deadline: Option<std::time::Instant>) -> Result<(), &'static str> {
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Err("Netstack service deadline");
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
        let mut clients = Vec::new();
        println!("internet_proxy_ready=true listen={listen}");
        while !stop_requested()
            && deadline.is_none_or(|deadline| std::time::Instant::now() < deadline)
        {
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
                        if stream.set_nonblocking(true).is_err() {
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
                    if bytes.len() < needed && Self::read_host_once(client, &mut bytes)? {
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
