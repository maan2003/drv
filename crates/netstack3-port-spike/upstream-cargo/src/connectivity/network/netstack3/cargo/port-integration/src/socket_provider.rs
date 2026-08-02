//! Revocable, quota-bounded application socket capability.
//!
//! All protocol state remains in Netstack3. This module adapts opaque handles,
//! queue/error semantics, and per-client ownership for a host kernel proxy.

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::{NonZeroU16, NonZeroUsize};
use std::rc::Rc;

use netstack3_port_spike::{
    RemoteIpAddress, RemoteIpVersion, RemoteSocketAddress, RemoteSocketError, RemoteSocketHandle,
    RemoteSocketProvider, RemoteSocketReadiness, SocketClientId,
};

use crate::{Runtime, RuntimeError, TcpShutdown, TcpSocketHandle, UdpSocketHandle};

#[derive(Clone, Copy)]
enum VersionedUdp {
    V4(UdpSocketHandle),
    V6(UdpSocketHandle),
}

#[derive(Clone, Copy)]
enum VersionedTcp {
    V4(TcpSocketHandle),
    V6(TcpSocketHandle),
}

enum Socket {
    Udp {
        client: SocketClientId,
        raw: VersionedUdp,
        staged: Option<Vec<u8>>,
    },
    Tcp {
        client: SocketClientId,
        raw: VersionedTcp,
    },
}

struct Client {
    quota: usize,
    sockets: usize,
}

struct State {
    next_client: u64,
    next_socket: u64,
    clients: HashMap<SocketClientId, Client>,
    sockets: HashMap<RemoteSocketHandle, Socket>,
}

#[derive(Clone)]
pub struct NativeSocketProvider {
    runtime: Rc<RefCell<Runtime>>,
    state: Rc<RefCell<State>>,
}

impl NativeSocketProvider {
    pub fn new(runtime: Rc<RefCell<Runtime>>) -> Self {
        Self {
            runtime,
            state: Rc::new(RefCell::new(State {
                next_client: 0,
                next_socket: 0,
                clients: HashMap::new(),
                sockets: HashMap::new(),
            })),
        }
    }

    fn reserve(&self, client: SocketClientId) -> Result<(), RemoteSocketError> {
        let mut state = self.state.borrow_mut();
        let client = state
            .clients
            .get_mut(&client)
            .ok_or(RemoteSocketError::UnknownClient)?;
        if client.sockets == client.quota {
            return Err(RemoteSocketError::QuotaExceeded);
        }
        client.sockets += 1;
        Ok(())
    }

    fn release(&self, client: SocketClientId) {
        let mut state = self.state.borrow_mut();
        let client = state
            .clients
            .get_mut(&client)
            .expect("socket client exists");
        client.sockets -= 1;
    }

    fn insert(&self, socket: Socket) -> RemoteSocketHandle {
        let mut state = self.state.borrow_mut();
        let handle = RemoteSocketHandle::from_raw(state.next_socket);
        state.next_socket = state
            .next_socket
            .checked_add(1)
            .expect("socket handle exhausted");
        assert!(state.sockets.insert(handle, socket).is_none());
        handle
    }

    fn udp(&self, handle: RemoteSocketHandle) -> Result<VersionedUdp, RemoteSocketError> {
        match self.state.borrow().sockets.get(&handle) {
            Some(Socket::Udp { raw, .. }) => Ok(*raw),
            Some(Socket::Tcp { .. }) => Err(RemoteSocketError::WrongSocketKind),
            None => Err(RemoteSocketError::StaleHandle),
        }
    }

    fn tcp(&self, handle: RemoteSocketHandle) -> Result<VersionedTcp, RemoteSocketError> {
        match self.state.borrow().sockets.get(&handle) {
            Some(Socket::Tcp { raw, .. }) => Ok(*raw),
            Some(Socket::Udp { .. }) => Err(RemoteSocketError::WrongSocketKind),
            None => Err(RemoteSocketError::StaleHandle),
        }
    }

    fn close_raw(&self, socket: &Socket) {
        let mut runtime = self.runtime.borrow_mut();
        match socket {
            Socket::Udp {
                raw: VersionedUdp::V4(handle),
                ..
            } => {
                let _ = runtime.udp_close(*handle);
            }
            Socket::Udp {
                raw: VersionedUdp::V6(handle),
                ..
            } => {
                let _ = runtime.udp_close_ipv6(*handle);
            }
            Socket::Tcp {
                raw: VersionedTcp::V4(handle),
                ..
            } => {
                let _ = runtime.tcp_close(*handle);
            }
            Socket::Tcp {
                raw: VersionedTcp::V6(handle),
                ..
            } => {
                let _ = runtime.tcp_close_ipv6(*handle);
            }
        }
    }
}

impl RemoteSocketProvider for NativeSocketProvider {
    fn open_client(
        &mut self,
        max_sockets: NonZeroUsize,
    ) -> Result<SocketClientId, RemoteSocketError> {
        let mut state = self.state.borrow_mut();
        let id = SocketClientId::from_raw(state.next_client);
        state.next_client = state
            .next_client
            .checked_add(1)
            .expect("client id exhausted");
        state.clients.insert(
            id,
            Client {
                quota: max_sockets.get(),
                sockets: 0,
            },
        );
        Ok(id)
    }

    fn close_client(&mut self, client: SocketClientId) -> Result<(), RemoteSocketError> {
        let sockets = {
            let mut state = self.state.borrow_mut();
            if state.clients.remove(&client).is_none() {
                return Err(RemoteSocketError::UnknownClient);
            }
            let handles: Vec<_> = state
                .sockets
                .iter()
                .filter_map(|(handle, socket)| {
                    let owner = match socket {
                        Socket::Udp { client, .. } | Socket::Tcp { client, .. } => *client,
                    };
                    (owner == client).then_some(*handle)
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|handle| state.sockets.remove(&handle))
                .collect::<Vec<_>>()
        };
        for socket in &sockets {
            self.close_raw(socket);
        }
        Ok(())
    }

    fn udp_socket(
        &mut self,
        client: SocketClientId,
        version: RemoteIpVersion,
    ) -> Result<RemoteSocketHandle, RemoteSocketError> {
        self.reserve(client)?;
        let result = match version {
            RemoteIpVersion::V4 => self.runtime.borrow_mut().udp_socket().map(VersionedUdp::V4),
            RemoteIpVersion::V6 => self
                .runtime
                .borrow_mut()
                .udp_socket_ipv6()
                .map(VersionedUdp::V6),
        };
        match result {
            Ok(raw) => Ok(self.insert(Socket::Udp {
                client,
                raw,
                staged: None,
            })),
            Err(error) => {
                self.release(client);
                Err(map_error(error))
            }
        }
    }

    fn udp_bind(
        &mut self,
        socket: RemoteSocketHandle,
        local: Option<RemoteIpAddress>,
        port: NonZeroU16,
    ) -> Result<(), RemoteSocketError> {
        match (self.udp(socket)?, local) {
            (VersionedUdp::V4(handle), None) => self
                .runtime
                .borrow_mut()
                .udp_bind(handle, None, port)
                .map_err(map_error),
            (VersionedUdp::V4(handle), Some(RemoteIpAddress::V4(address))) => self
                .runtime
                .borrow_mut()
                .udp_bind(handle, Some(address), port)
                .map_err(map_error),
            (VersionedUdp::V6(handle), None) => self
                .runtime
                .borrow_mut()
                .udp_bind_ipv6(handle, None, port)
                .map_err(map_error),
            (VersionedUdp::V6(handle), Some(RemoteIpAddress::V6(address))) => self
                .runtime
                .borrow_mut()
                .udp_bind_ipv6(handle, Some(address), port)
                .map_err(map_error),
            _ => Err(RemoteSocketError::AddressFamilyMismatch),
        }
    }

    fn udp_send_to(
        &mut self,
        socket: RemoteSocketHandle,
        remote: RemoteSocketAddress,
        payload: &[u8],
    ) -> Result<(), RemoteSocketError> {
        match (self.udp(socket)?, remote.address) {
            (VersionedUdp::V4(handle), RemoteIpAddress::V4(address)) => self
                .runtime
                .borrow_mut()
                .udp_send_to(handle, address, remote.port, payload)
                .map_err(map_error),
            (VersionedUdp::V6(handle), RemoteIpAddress::V6(address)) => self
                .runtime
                .borrow_mut()
                .udp_send_to_ipv6(handle, address, remote.port, payload)
                .map_err(map_error),
            _ => Err(RemoteSocketError::AddressFamilyMismatch),
        }
    }

    fn udp_receive(
        &mut self,
        socket: RemoteSocketHandle,
    ) -> Result<Option<Vec<u8>>, RemoteSocketError> {
        let staged = {
            let mut state = self.state.borrow_mut();
            match state.sockets.get_mut(&socket) {
                Some(Socket::Udp { staged, .. }) => staged.take(),
                Some(Socket::Tcp { .. }) => return Err(RemoteSocketError::WrongSocketKind),
                None => return Err(RemoteSocketError::StaleHandle),
            }
        };
        if staged.is_some() {
            return Ok(staged);
        }
        match self.udp(socket)? {
            VersionedUdp::V4(handle) => self
                .runtime
                .borrow_mut()
                .udp_receive(handle)
                .map_err(map_error),
            VersionedUdp::V6(handle) => self
                .runtime
                .borrow_mut()
                .udp_receive_ipv6(handle)
                .map_err(map_error),
        }
    }

    fn tcp_socket(
        &mut self,
        client: SocketClientId,
        version: RemoteIpVersion,
    ) -> Result<RemoteSocketHandle, RemoteSocketError> {
        self.reserve(client)?;
        let result = match version {
            RemoteIpVersion::V4 => self.runtime.borrow_mut().tcp_socket().map(VersionedTcp::V4),
            RemoteIpVersion::V6 => self
                .runtime
                .borrow_mut()
                .tcp_socket_ipv6()
                .map(VersionedTcp::V6),
        };
        match result {
            Ok(raw) => Ok(self.insert(Socket::Tcp { client, raw })),
            Err(error) => {
                self.release(client);
                Err(map_error(error))
            }
        }
    }

    fn tcp_bind(
        &mut self,
        socket: RemoteSocketHandle,
        local: Option<RemoteIpAddress>,
        port: NonZeroU16,
    ) -> Result<(), RemoteSocketError> {
        match (self.tcp(socket)?, local) {
            (VersionedTcp::V4(handle), None) => self
                .runtime
                .borrow_mut()
                .tcp_bind(handle, None, port)
                .map_err(map_error),
            (VersionedTcp::V4(handle), Some(RemoteIpAddress::V4(address))) => self
                .runtime
                .borrow_mut()
                .tcp_bind(handle, Some(address), port)
                .map_err(map_error),
            (VersionedTcp::V6(handle), None) => self
                .runtime
                .borrow_mut()
                .tcp_bind_ipv6(handle, None, port)
                .map_err(map_error),
            (VersionedTcp::V6(handle), Some(RemoteIpAddress::V6(address))) => self
                .runtime
                .borrow_mut()
                .tcp_bind_ipv6(handle, Some(address), port)
                .map_err(map_error),
            _ => Err(RemoteSocketError::AddressFamilyMismatch),
        }
    }

    fn tcp_connect(
        &mut self,
        socket: RemoteSocketHandle,
        remote: RemoteSocketAddress,
    ) -> Result<(), RemoteSocketError> {
        match (self.tcp(socket)?, remote.address) {
            (VersionedTcp::V4(handle), RemoteIpAddress::V4(address)) => self
                .runtime
                .borrow_mut()
                .tcp_connect(handle, address, remote.port)
                .map_err(map_error),
            (VersionedTcp::V6(handle), RemoteIpAddress::V6(address)) => self
                .runtime
                .borrow_mut()
                .tcp_connect_ipv6(handle, address, remote.port)
                .map_err(map_error),
            _ => Err(RemoteSocketError::AddressFamilyMismatch),
        }
    }

    fn tcp_listen(
        &mut self,
        socket: RemoteSocketHandle,
        backlog: NonZeroUsize,
    ) -> Result<(), RemoteSocketError> {
        match self.tcp(socket)? {
            VersionedTcp::V4(handle) => self
                .runtime
                .borrow_mut()
                .tcp_listen(handle, backlog)
                .map_err(map_error),
            VersionedTcp::V6(handle) => self
                .runtime
                .borrow_mut()
                .tcp_listen_ipv6(handle, backlog)
                .map_err(map_error),
        }
    }

    fn tcp_accept(
        &mut self,
        socket: RemoteSocketHandle,
    ) -> Result<RemoteSocketHandle, RemoteSocketError> {
        let (client, raw) = match self.state.borrow().sockets.get(&socket) {
            Some(Socket::Tcp { client, raw }) => (*client, *raw),
            Some(Socket::Udp { .. }) => return Err(RemoteSocketError::WrongSocketKind),
            None => return Err(RemoteSocketError::StaleHandle),
        };
        self.reserve(client)?;
        let accepted = match raw {
            VersionedTcp::V4(handle) => self
                .runtime
                .borrow_mut()
                .tcp_accept(handle)
                .map(VersionedTcp::V4),
            VersionedTcp::V6(handle) => self
                .runtime
                .borrow_mut()
                .tcp_accept_ipv6(handle)
                .map(VersionedTcp::V6),
        };
        match accepted {
            Ok(raw) => Ok(self.insert(Socket::Tcp { client, raw })),
            Err(error) => {
                self.release(client);
                Err(map_error(error))
            }
        }
    }

    fn tcp_write(
        &mut self,
        socket: RemoteSocketHandle,
        payload: &[u8],
    ) -> Result<usize, RemoteSocketError> {
        match self.tcp(socket)? {
            VersionedTcp::V4(handle) => self
                .runtime
                .borrow_mut()
                .tcp_write(handle, payload)
                .map_err(map_error),
            VersionedTcp::V6(handle) => self
                .runtime
                .borrow_mut()
                .tcp_write_ipv6(handle, payload)
                .map_err(map_error),
        }
    }

    fn tcp_read(
        &mut self,
        socket: RemoteSocketHandle,
        output: &mut [u8],
    ) -> Result<usize, RemoteSocketError> {
        match self.tcp(socket)? {
            VersionedTcp::V4(handle) => self
                .runtime
                .borrow_mut()
                .tcp_read(handle, output)
                .map_err(map_error),
            VersionedTcp::V6(handle) => self
                .runtime
                .borrow_mut()
                .tcp_read_ipv6(handle, output)
                .map_err(map_error),
        }
    }

    fn tcp_shutdown(&mut self, socket: RemoteSocketHandle) -> Result<(), RemoteSocketError> {
        match self.tcp(socket)? {
            VersionedTcp::V4(handle) => self
                .runtime
                .borrow_mut()
                .tcp_shutdown(handle, TcpShutdown::SendAndReceive)
                .map_err(map_error),
            VersionedTcp::V6(handle) => self
                .runtime
                .borrow_mut()
                .tcp_shutdown_ipv6(handle, TcpShutdown::SendAndReceive)
                .map_err(map_error),
        }
    }

    fn readiness(
        &mut self,
        socket: RemoteSocketHandle,
    ) -> Result<RemoteSocketReadiness, RemoteSocketError> {
        let raw = {
            let state = self.state.borrow();
            match state.sockets.get(&socket) {
                Some(Socket::Udp {
                    raw: _,
                    staged: Some(_),
                    ..
                }) => {
                    return Ok(RemoteSocketReadiness {
                        readable: true,
                        writable: true,
                        incoming: false,
                    });
                }
                Some(Socket::Udp { raw, .. }) => EitherSocket::Udp(*raw),
                Some(Socket::Tcp { raw, .. }) => EitherSocket::Tcp(*raw),
                None => return Err(RemoteSocketError::StaleHandle),
            }
        };
        match raw {
            EitherSocket::Udp(raw) => {
                let packet = match raw {
                    VersionedUdp::V4(handle) => self
                        .runtime
                        .borrow_mut()
                        .udp_receive(handle)
                        .map_err(map_error)?,
                    VersionedUdp::V6(handle) => self
                        .runtime
                        .borrow_mut()
                        .udp_receive_ipv6(handle)
                        .map_err(map_error)?,
                };
                let readable = packet.is_some();
                if let Some(packet) = packet {
                    if let Some(Socket::Udp { staged, .. }) =
                        self.state.borrow_mut().sockets.get_mut(&socket)
                    {
                        *staged = Some(packet);
                    }
                }
                Ok(RemoteSocketReadiness {
                    readable,
                    writable: true,
                    incoming: false,
                })
            }
            EitherSocket::Tcp(raw) => {
                let (readable, writable) = match raw {
                    VersionedTcp::V4(handle) => self
                        .runtime
                        .borrow()
                        .tcp_readiness(handle)
                        .map_err(map_error)?,
                    VersionedTcp::V6(handle) => self
                        .runtime
                        .borrow()
                        .tcp_readiness_ipv6(handle)
                        .map_err(map_error)?,
                };
                Ok(RemoteSocketReadiness {
                    readable,
                    writable,
                    incoming: false,
                })
            }
        }
    }

    fn close(&mut self, socket: RemoteSocketHandle) -> Result<(), RemoteSocketError> {
        let socket = self
            .state
            .borrow_mut()
            .sockets
            .remove(&socket)
            .ok_or(RemoteSocketError::StaleHandle)?;
        let client = match &socket {
            Socket::Udp { client, .. } | Socket::Tcp { client, .. } => *client,
        };
        self.close_raw(&socket);
        self.release(client);
        Ok(())
    }
}

enum EitherSocket {
    Udp(VersionedUdp),
    Tcp(VersionedTcp),
}

fn map_error(error: RuntimeError) -> RemoteSocketError {
    match error {
        RuntimeError::SocketLimit => RemoteSocketError::ResourceExhausted,
        RuntimeError::UnknownSocket => RemoteSocketError::StaleHandle,
        RuntimeError::AddressInUse => RemoteSocketError::AddressInUse,
        RuntimeError::WouldBlock => RemoteSocketError::WouldBlock,
        RuntimeError::PayloadTooLarge => RemoteSocketError::PayloadTooLarge,
        RuntimeError::InvalidAddress => RemoteSocketError::AddressFamilyMismatch,
        RuntimeError::NetworkUnreachable => RemoteSocketError::NetworkUnreachable,
        RuntimeError::HostUnreachable => RemoteSocketError::HostUnreachable,
        RuntimeError::ConnectionRefused => RemoteSocketError::ConnectionRefused,
        RuntimeError::ConnectionPending => RemoteSocketError::InProgress,
        RuntimeError::AlreadyConnected => RemoteSocketError::AlreadyConnected,
        RuntimeError::TimedOut => RemoteSocketError::TimedOut,
        RuntimeError::PermissionDenied => RemoteSocketError::PermissionDenied,
        RuntimeError::NotSupported => RemoteSocketError::NotSupported,
        RuntimeError::InvalidCapacity
        | RuntimeError::InvalidMac
        | RuntimeError::InvalidMtu
        | RuntimeError::InvalidState
        | RuntimeError::SendFailed
        | RuntimeError::InvalidLease => RemoteSocketError::InvalidState,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    fn provider() -> NativeSocketProvider {
        let runtime = Runtime::new(
            8,
            (0u8..=255).cycle().take(8192),
            NonZeroU64::new(1).unwrap(),
            [2, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        NativeSocketProvider::new(Rc::new(RefCell::new(runtime)))
    }

    #[test]
    fn per_client_quota_is_released_only_by_close() {
        let mut provider = provider();
        let client = provider.open_client(NonZeroUsize::new(1).unwrap()).unwrap();
        let udp = provider.udp_socket(client, RemoteIpVersion::V4).unwrap();
        assert_eq!(
            provider.tcp_socket(client, RemoteIpVersion::V4),
            Err(RemoteSocketError::QuotaExceeded)
        );
        provider.close(udp).unwrap();
        provider.tcp_socket(client, RemoteIpVersion::V4).unwrap();
    }

    #[test]
    fn close_and_client_revocation_make_handles_stale() {
        let mut provider = provider();
        let client = provider.open_client(NonZeroUsize::new(2).unwrap()).unwrap();
        let udp = provider.udp_socket(client, RemoteIpVersion::V4).unwrap();
        provider.close(udp).unwrap();
        assert_eq!(provider.close(udp), Err(RemoteSocketError::StaleHandle));

        let tcp = provider.tcp_socket(client, RemoteIpVersion::V6).unwrap();
        provider.close_client(client).unwrap();
        assert_eq!(
            provider.tcp_write(tcp, b"revoked"),
            Err(RemoteSocketError::StaleHandle)
        );
        assert_eq!(
            provider.close_client(client),
            Err(RemoteSocketError::UnknownClient)
        );
    }

    #[test]
    fn kinds_and_address_families_do_not_cross() {
        let mut provider = provider();
        let client = provider.open_client(NonZeroUsize::new(2).unwrap()).unwrap();
        let udp = provider.udp_socket(client, RemoteIpVersion::V4).unwrap();
        assert_eq!(
            provider.tcp_read(udp, &mut [0; 1]),
            Err(RemoteSocketError::WrongSocketKind)
        );
        assert_eq!(
            provider.udp_bind(
                udp,
                Some(RemoteIpAddress::V6([0; 16])),
                NonZeroU16::new(1000).unwrap(),
            ),
            Err(RemoteSocketError::AddressFamilyMismatch)
        );
    }

    #[test]
    fn no_route_matches_upstream_network_unreachable_semantics() {
        let mut provider = provider();
        let client = provider.open_client(NonZeroUsize::new(1).unwrap()).unwrap();
        let tcp = provider.tcp_socket(client, RemoteIpVersion::V4).unwrap();
        assert_eq!(
            provider.tcp_connect(
                tcp,
                RemoteSocketAddress {
                    address: RemoteIpAddress::V4([192, 0, 2, 1]),
                    port: NonZeroU16::new(443).unwrap(),
                },
            ),
            Err(RemoteSocketError::NetworkUnreachable)
        );
    }
}
