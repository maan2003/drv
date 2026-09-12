//! Revocable, quota-bounded application socket capability.
//!
//! All protocol state remains in Netstack3. This module adapts opaque handles,
//! queue/error semantics, and per-client ownership for a host kernel proxy.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::num::{NonZeroU16, NonZeroUsize};
use std::rc::Rc;

use netstack3_port_spike::{
    RemoteIpAddress, RemoteIpVersion, RemoteSocketAddress, RemoteSocketError, RemoteSocketHandle,
    RemoteSocketProvider, RemoteSocketReadiness, SocketClientId,
    provider_dispatch_v2::ProviderAcceptV2,
    provider_transport_v2::{
        ProviderNameV2, ProviderOptionV2, ProviderReadinessSnapshotV2, ProviderReadinessV2,
        ProviderRecvMsgV2, ProviderShutdownV2, ProviderSocketAddressV2, ProviderSocketKindV2,
    },
};

use crate::{
    NativeIpAddress, NativeSocketAddress, NativeUdpDatagram, Runtime, RuntimeError, TcpShutdown,
    TcpSocketHandle, UdpSocketHandle,
};

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
        staged: Option<NativeUdpDatagram>,
        v2: SocketV2,
    },
    Tcp {
        client: SocketClientId,
        raw: VersionedTcp,
        v2: SocketV2,
    },
}

#[derive(Default)]
struct SocketV2 {
    local: Option<ProviderSocketAddressV2>,
    peer: Option<ProviderSocketAddressV2>,
    read_closed: bool,
    write_closed: bool,
    listening: bool,
    connecting: bool,
    connect_failed: bool,
    sequence: u64,
    emitted_sequence: u64,
    last_readiness: Option<(ProviderReadinessV2, Option<RemoteSocketError>)>,
    pending_error: Option<RemoteSocketError>,
}

struct Client {
    quota: usize,
    sockets: usize,
}

struct State {
    next_client: u64,
    next_socket: u64,
    next_ephemeral_port: u16,
    clients: HashMap<SocketClientId, Client>,
    revoked_clients: HashSet<SocketClientId>,
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
                next_ephemeral_port: 49152,
                clients: HashMap::new(),
                revoked_clients: HashSet::new(),
                sockets: HashMap::new(),
            })),
        }
    }

    /// Samples all live sockets and returns only readiness states that changed
    /// since the previous call. The daemon uses this to produce unsolicited
    /// ABI-v2 readiness events without lossy edge bookkeeping.
    pub fn take_readiness_changes(
        &mut self,
    ) -> Vec<(
        SocketClientId,
        RemoteSocketHandle,
        ProviderReadinessSnapshotV2,
    )> {
        let handles: Vec<_> = self.state.borrow().sockets.keys().copied().collect();
        let mut changes = Vec::new();
        for handle in handles {
            let Ok(snapshot) =
                netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::readiness(
                    self, handle,
                )
            else {
                continue;
            };
            let mut state = self.state.borrow_mut();
            let Some(socket) = state.sockets.get_mut(&handle) else {
                continue;
            };
            let (client, v2) = match socket {
                Socket::Udp { client, v2, .. } | Socket::Tcp { client, v2, .. } => (*client, v2),
            };
            if v2.emitted_sequence < snapshot.sequence {
                v2.emitted_sequence = snapshot.sequence;
                changes.push((client, handle, snapshot));
            }
        }
        changes
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

    fn v2(
        &self,
        handle: RemoteSocketHandle,
    ) -> Result<std::cell::Ref<'_, SocketV2>, RemoteSocketError> {
        std::cell::Ref::filter_map(self.state.borrow(), |state| {
            state.sockets.get(&handle).map(|socket| match socket {
                Socket::Udp { v2, .. } | Socket::Tcp { v2, .. } => v2,
            })
        })
        .map_err(|_| RemoteSocketError::StaleHandle)
    }

    fn next_ephemeral_port(&self) -> u16 {
        let mut state = self.state.borrow_mut();
        let port = state.next_ephemeral_port;
        state.next_ephemeral_port = if port == u16::MAX { 49152 } else { port + 1 };
        port
    }

    fn unspecified(
        &self,
        handle: RemoteSocketHandle,
        port: u16,
    ) -> Result<ProviderSocketAddressV2, RemoteSocketError> {
        Ok(ProviderSocketAddressV2 {
            address: match self.state.borrow().sockets.get(&handle) {
                Some(Socket::Udp {
                    raw: VersionedUdp::V4(_),
                    ..
                })
                | Some(Socket::Tcp {
                    raw: VersionedTcp::V4(_),
                    ..
                }) => RemoteIpAddress::V4([0; 4]),
                Some(Socket::Udp {
                    raw: VersionedUdp::V6(_),
                    ..
                })
                | Some(Socket::Tcp {
                    raw: VersionedTcp::V6(_),
                    ..
                }) => RemoteIpAddress::V6([0; 16]),
                None => return Err(RemoteSocketError::StaleHandle),
            },
            port,
        })
    }

    fn refresh_tcp_connection(&self, handle: RemoteSocketHandle) -> Result<(), RemoteSocketError> {
        let raw = match self.state.borrow().sockets.get(&handle) {
            Some(Socket::Tcp { raw, v2, .. }) => Some((*raw, v2.connecting)),
            Some(_) => None,
            None => return Err(RemoteSocketError::StaleHandle),
        };
        let Some((raw, connecting)) = raw else {
            return Ok(());
        };
        let (tcp_state, error) = match raw {
            VersionedTcp::V4(raw) => {
                let mut runtime = self.runtime.borrow_mut();
                let state = runtime.tcp_state(raw).map_err(map_error)?;
                let error = connecting
                    .then(|| runtime.tcp_take_socket_error(raw).map_err(map_error))
                    .transpose()?
                    .flatten();
                (state, error)
            }
            VersionedTcp::V6(raw) => {
                let mut runtime = self.runtime.borrow_mut();
                let state = runtime.tcp_state_ipv6(raw).map_err(map_error)?;
                let error = connecting
                    .then(|| runtime.tcp_take_socket_error_ipv6(raw).map_err(map_error))
                    .transpose()?
                    .flatten();
                (state, error)
            }
        };
        let mut state = self.state.borrow_mut();
        let Socket::Tcp { v2, .. } = state.sockets.get_mut(&handle).expect("socket remains live")
        else {
            unreachable!()
        };
        use netstack3_base::TcpSocketState;
        match tcp_state {
            TcpSocketState::Established => v2.connecting = false,
            TcpSocketState::CloseWait => {
                v2.connecting = false;
                v2.read_closed = true;
            }
            TcpSocketState::FinWait1 | TcpSocketState::FinWait2 => v2.write_closed = true,
            TcpSocketState::Closing
            | TcpSocketState::LastAck
            | TcpSocketState::TimeWait => {
                v2.read_closed = true;
                v2.write_closed = true;
            }
            TcpSocketState::Close => {
                // Unbound sockets are also in Close; polling an unopened
                // connection must not permanently shut down its data paths.
                if v2.peer.is_some() && !v2.connecting {
                    v2.read_closed = true;
                    v2.write_closed = true;
                }
            }
            TcpSocketState::SynSent | TcpSocketState::SynRecv | TcpSocketState::Listen => {}
        }
        if let Some(error) = error {
            v2.connecting = false;
            v2.connect_failed = true;
            v2.pending_error = Some(map_error(error));
        }
        Ok(())
    }
}

fn provider_address(address: NativeSocketAddress) -> ProviderSocketAddressV2 {
    ProviderSocketAddressV2 {
        address: match address.address {
            NativeIpAddress::V4(address) => RemoteIpAddress::V4(address),
            NativeIpAddress::V6(address) => RemoteIpAddress::V6(address),
        },
        port: address.port,
    }
}

impl RemoteSocketProvider for NativeSocketProvider {
    fn open_client(
        &mut self,
        max_sockets: NonZeroUsize,
    ) -> Result<SocketClientId, RemoteSocketError> {
        let mut state = self.state.borrow_mut();
        let id = loop {
            let id = SocketClientId::from_raw(state.next_client);
            state.next_client = state
                .next_client
                .checked_add(1)
                .expect("client id exhausted");
            if !state.clients.contains_key(&id) && !state.revoked_clients.contains(&id) {
                break id;
            }
        };
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
            state.revoked_clients.insert(client);
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
                v2: SocketV2::default(),
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
        if let Some(staged) = staged {
            return Ok(Some(staged.body));
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
            Ok(raw) => Ok(self.insert(Socket::Tcp {
                client,
                raw,
                v2: SocketV2::default(),
            })),
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
            Some(Socket::Tcp { client, raw, .. }) => (*client, *raw),
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
            Ok(raw) => Ok(self.insert(Socket::Tcp {
                client,
                raw,
                v2: SocketV2::default(),
            })),
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
                        .udp_receive_msg(handle)
                        .map_err(map_error)?,
                    VersionedUdp::V6(handle) => self
                        .runtime
                        .borrow_mut()
                        .udp_receive_msg_ipv6(handle)
                        .map_err(map_error)?,
                };
                let readable = packet.is_some();
                if let Some(packet) = packet
                    && let Some(Socket::Udp { staged, .. }) =
                        self.state.borrow_mut().sockets.get_mut(&socket)
                {
                    *staged = Some(packet);
                }
                Ok(RemoteSocketReadiness {
                    readable,
                    writable: true,
                    incoming: false,
                })
            }
            EitherSocket::Tcp(raw) => {
                let (readable, writable, incoming) = match raw {
                    VersionedTcp::V4(handle) => {
                        let runtime = self.runtime.borrow();
                        let (readable, writable) =
                            runtime.tcp_readiness(handle).map_err(map_error)?;
                        let incoming =
                            runtime.tcp_pending_connections(handle).map_err(map_error)? != 0;
                        (readable, writable, incoming)
                    }
                    VersionedTcp::V6(handle) => {
                        let runtime = self.runtime.borrow();
                        let (readable, writable) =
                            runtime.tcp_readiness_ipv6(handle).map_err(map_error)?;
                        let incoming = runtime
                            .tcp_pending_connections_ipv6(handle)
                            .map_err(map_error)?
                            != 0;
                        (readable, writable, incoming)
                    }
                };
                Ok(RemoteSocketReadiness {
                    readable,
                    writable,
                    incoming,
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

impl netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2 for NativeSocketProvider {
    fn open_client(&mut self, client: SocketClientId, quota: u32) -> Result<(), RemoteSocketError> {
        let quota = usize::try_from(quota).map_err(|_| RemoteSocketError::ResourceExhausted)?;
        if quota == 0 {
            return Err(RemoteSocketError::InvalidState);
        }
        let mut state = self.state.borrow_mut();
        if state.clients.contains_key(&client) || state.revoked_clients.contains(&client) {
            return Err(RemoteSocketError::InvalidState);
        }
        state.clients.insert(client, Client { quota, sockets: 0 });
        Ok(())
    }

    fn close_client(&mut self, client: SocketClientId) -> Result<(), RemoteSocketError> {
        RemoteSocketProvider::close_client(self, client)
    }

    fn open_socket(
        &mut self,
        client: SocketClientId,
        kind: ProviderSocketKindV2,
        version: RemoteIpVersion,
    ) -> Result<RemoteSocketHandle, RemoteSocketError> {
        match kind {
            ProviderSocketKindV2::Udp => self.udp_socket(client, version),
            ProviderSocketKindV2::Tcp => self.tcp_socket(client, version),
        }
    }

    fn bind(
        &mut self,
        handle: RemoteSocketHandle,
        local: Option<RemoteIpAddress>,
        port: u16,
    ) -> Result<ProviderSocketAddressV2, RemoteSocketError> {
        let port = if port == 0 {
            self.next_ephemeral_port()
        } else {
            port
        };
        let nonzero = NonZeroU16::new(port).unwrap();
        let udp = match self.state.borrow().sockets.get(&handle) {
            Some(Socket::Udp { .. }) => true,
            Some(Socket::Tcp { .. }) => false,
            None => return Err(RemoteSocketError::StaleHandle),
        };
        if udp {
            self.udp_bind(handle, local, nonzero)?;
        } else {
            self.tcp_bind(handle, local, nonzero)?;
        }
        let address = match local {
            Some(address) => ProviderSocketAddressV2 { address, port },
            None => self.unspecified(handle, port)?,
        };
        match self.state.borrow_mut().sockets.get_mut(&handle).unwrap() {
            Socket::Udp { v2, .. } | Socket::Tcp { v2, .. } => v2.local = Some(address.clone()),
        }
        Ok(address)
    }

    fn connect(
        &mut self,
        handle: RemoteSocketHandle,
        peer: ProviderSocketAddressV2,
    ) -> Result<(), RemoteSocketError> {
        let port = NonZeroU16::new(peer.port).ok_or(RemoteSocketError::InvalidState)?;
        if self.v2(handle)?.local.is_none() {
            // Let core select the source from the destination route. A configured
            // Ethernet address must not become the source of a localhost flow.
            self.bind(handle, None, 0)?;
        }
        let result = match (self.state.borrow().sockets.get(&handle), peer.address) {
            (
                Some(Socket::Udp {
                    raw: VersionedUdp::V4(raw),
                    ..
                }),
                RemoteIpAddress::V4(address),
            ) => self
                .runtime
                .borrow_mut()
                .udp_connect(*raw, address, port)
                .map_err(map_error),
            (
                Some(Socket::Udp {
                    raw: VersionedUdp::V6(raw),
                    ..
                }),
                RemoteIpAddress::V6(address),
            ) => self
                .runtime
                .borrow_mut()
                .udp_connect_ipv6(*raw, address, port)
                .map_err(map_error),
            (
                Some(Socket::Tcp {
                    raw: VersionedTcp::V4(raw),
                    ..
                }),
                RemoteIpAddress::V4(address),
            ) => self
                .runtime
                .borrow_mut()
                .tcp_connect(*raw, address, port)
                .map_err(map_error),
            (
                Some(Socket::Tcp {
                    raw: VersionedTcp::V6(raw),
                    ..
                }),
                RemoteIpAddress::V6(address),
            ) => self
                .runtime
                .borrow_mut()
                .tcp_connect_ipv6(*raw, address, port)
                .map_err(map_error),
            (Some(_), _) => Err(RemoteSocketError::AddressFamilyMismatch),
            (None, _) => Err(RemoteSocketError::StaleHandle),
        };
        let tcp = matches!(
            self.state.borrow().sockets.get(&handle),
            Some(Socket::Tcp { .. })
        );
        let deferred_udp = !tcp
            && matches!(
                result,
                Err(RemoteSocketError::NetworkUnreachable | RemoteSocketError::HostUnreachable)
            );
        if result.is_ok() || result == Err(RemoteSocketError::InProgress) || deferred_udp {
            match self.state.borrow_mut().sockets.get_mut(&handle).unwrap() {
                Socket::Udp { v2, .. } | Socket::Tcp { v2, .. } => {
                    v2.peer = Some(peer);
                    v2.connecting = tcp;
                }
            }
        }
        if tcp && result.is_ok() {
            Err(RemoteSocketError::InProgress)
        } else if deferred_udp {
            Ok(())
        } else {
            result
        }
    }

    fn disconnect(
        &mut self,
        handle: RemoteSocketHandle,
    ) -> Result<ProviderSocketAddressV2, RemoteSocketError> {
        match self.udp(handle)? {
            VersionedUdp::V4(raw) => self
                .runtime
                .borrow_mut()
                .udp_disconnect(raw)
                .map_err(map_error)?,
            VersionedUdp::V6(raw) => self
                .runtime
                .borrow_mut()
                .udp_disconnect_ipv6(raw)
                .map_err(map_error)?,
        }
        let mut state = self.state.borrow_mut();
        let Socket::Udp { v2, .. } = state.sockets.get_mut(&handle).unwrap() else {
            unreachable!()
        };
        v2.peer = None;
        Ok(v2.local.clone().expect("connected socket is bound"))
    }

    fn listen(
        &mut self,
        handle: RemoteSocketHandle,
        backlog: u32,
    ) -> Result<ProviderSocketAddressV2, RemoteSocketError> {
        if self.v2(handle)?.local.is_none() {
            self.bind(handle, None, 0)?;
        }
        let backlog =
            NonZeroUsize::new(usize::try_from(backlog).unwrap_or(usize::MAX).max(1)).unwrap();
        self.tcp_listen(handle, backlog)?;
        let mut state = self.state.borrow_mut();
        let Socket::Tcp { v2, .. } = state
            .sockets
            .get_mut(&handle)
            .ok_or(RemoteSocketError::StaleHandle)?
        else {
            return Err(RemoteSocketError::WrongSocketKind);
        };
        v2.listening = true;
        Ok(v2.local.clone().unwrap())
    }

    fn accept(
        &mut self,
        handle: RemoteSocketHandle,
    ) -> Result<ProviderAcceptV2, RemoteSocketError> {
        let (client, raw) = match self.state.borrow().sockets.get(&handle) {
            Some(Socket::Tcp { client, raw, v2 }) if v2.listening => (*client, *raw),
            Some(Socket::Tcp { .. }) => return Err(RemoteSocketError::InvalidState),
            Some(Socket::Udp { .. }) => return Err(RemoteSocketError::WrongSocketKind),
            None => return Err(RemoteSocketError::StaleHandle),
        };
        self.reserve(client)?;
        let accepted = match raw {
            VersionedTcp::V4(raw) => self
                .runtime
                .borrow_mut()
                .tcp_accept_with_peer(raw)
                .map(|(raw, local, peer)| (VersionedTcp::V4(raw), local, peer)),
            VersionedTcp::V6(raw) => self
                .runtime
                .borrow_mut()
                .tcp_accept_ipv6_with_peer(raw)
                .map(|(raw, local, peer)| (VersionedTcp::V6(raw), local, peer)),
        };
        match accepted {
            Ok((raw, local, peer)) => {
                let local = provider_address(local);
                let peer = provider_address(peer);
                let child = self.insert(Socket::Tcp {
                    client,
                    raw,
                    v2: SocketV2 {
                        local: Some(local.clone()),
                        peer: Some(peer.clone()),
                        ..SocketV2::default()
                    },
                });
                Ok(ProviderAcceptV2 {
                    handle: child,
                    local,
                    peer,
                })
            }
            Err(error) => {
                self.release(client);
                Err(map_error(error))
            }
        }
    }

    fn send_msg(
        &mut self,
        handle: RemoteSocketHandle,
        flags: u32,
        peer: Option<ProviderSocketAddressV2>,
        bytes: &[u8],
    ) -> Result<usize, RemoteSocketError> {
        if flags != 0 {
            return Err(RemoteSocketError::NotSupported);
        }
        let tcp = match self.state.borrow().sockets.get(&handle) {
            Some(Socket::Tcp { v2, .. }) => Some((
                v2.peer.is_some(),
                v2.connecting,
                v2.connect_failed,
                v2.write_closed,
            )),
            Some(Socket::Udp { .. }) => None,
            None => return Err(RemoteSocketError::StaleHandle),
        };
        if let Some((connected, connecting, connect_failed, write_closed)) = tcp {
            if peer.is_some() {
                return Err(RemoteSocketError::AlreadyConnected);
            }
            if connecting {
                return Err(RemoteSocketError::InProgress);
            }
            if !connected || connect_failed || write_closed {
                return Err(RemoteSocketError::InvalidState);
            }
            return self.tcp_write(handle, bytes);
        }
        // Keep the binding's local-name state in sync with implicit UDP binds.
        // Otherwise a later connect tries to bind the already-bound core socket.
        if self.v2(handle)?.local.is_none() {
            self.bind(handle, None, 0)?;
        }
        match self.state.borrow().sockets.get(&handle) {
            Some(Socket::Udp {
                raw: VersionedUdp::V4(raw),
                v2,
                ..
            }) => match peer.or_else(|| v2.peer.clone()) {
                Some(ProviderSocketAddressV2 {
                    address: RemoteIpAddress::V4(address),
                    port,
                }) => self
                    .runtime
                    .borrow_mut()
                    .udp_send_to(
                        *raw,
                        address,
                        NonZeroU16::new(port).ok_or(RemoteSocketError::InvalidState)?,
                        bytes,
                    )
                    .map(|()| bytes.len())
                    .map_err(map_error),
                Some(_) => Err(RemoteSocketError::AddressFamilyMismatch),
                None => Err(RemoteSocketError::InvalidState),
            },
            Some(Socket::Udp {
                raw: VersionedUdp::V6(raw),
                v2,
                ..
            }) => match peer.or_else(|| v2.peer.clone()) {
                Some(ProviderSocketAddressV2 {
                    address: RemoteIpAddress::V6(address),
                    port,
                }) => self
                    .runtime
                    .borrow_mut()
                    .udp_send_to_ipv6(
                        *raw,
                        address,
                        NonZeroU16::new(port).ok_or(RemoteSocketError::InvalidState)?,
                        bytes,
                    )
                    .map(|()| bytes.len())
                    .map_err(map_error),
                Some(_) => Err(RemoteSocketError::AddressFamilyMismatch),
                None => Err(RemoteSocketError::InvalidState),
            },
            Some(Socket::Tcp { .. }) => unreachable!(),
            None => Err(RemoteSocketError::StaleHandle),
        }
    }

    fn recv_msg(
        &mut self,
        handle: RemoteSocketHandle,
        max_len: u32,
        flags: u32,
    ) -> Result<ProviderRecvMsgV2, RemoteSocketError> {
        if flags != 0 {
            return Err(RemoteSocketError::NotSupported);
        }
        self.refresh_tcp_connection(handle)?;
        let max_len = usize::try_from(max_len).map_err(|_| RemoteSocketError::ResourceExhausted)?;
        let raw = match self.state.borrow_mut().sockets.get_mut(&handle) {
            Some(Socket::Udp { raw, staged, .. }) => {
                let packet = if let Some(packet) = staged.take() {
                    Some(packet)
                } else {
                    match raw {
                        VersionedUdp::V4(raw) => self
                            .runtime
                            .borrow_mut()
                            .udp_receive_msg(*raw)
                            .map_err(map_error)?,
                        VersionedUdp::V6(raw) => self
                            .runtime
                            .borrow_mut()
                            .udp_receive_msg_ipv6(*raw)
                            .map_err(map_error)?,
                    }
                }
                .ok_or(RemoteSocketError::WouldBlock)?;
                let original_len = u32::try_from(packet.body.len())
                    .map_err(|_| RemoteSocketError::ResourceExhausted)?;
                return Ok(ProviderRecvMsgV2 {
                    source: Some(provider_address(packet.source)),
                    original_len,
                    flags: 0,
                    eof: false,
                    data: packet.body.into_iter().take(max_len).collect(),
                });
            }
            Some(Socket::Tcp { raw, v2, .. }) => {
                if v2.connecting {
                    return Err(RemoteSocketError::InProgress);
                }
                if v2.peer.is_none() || v2.connect_failed || v2.listening {
                    return Err(RemoteSocketError::InvalidState);
                }
                (*raw, v2.read_closed)
            }
            None => return Err(RemoteSocketError::StaleHandle),
        };
        let mut data =
            vec![0; max_len.min(netstack3_port_spike::provider_transport::MAX_PROVIDER_PAYLOAD)];
        let read = match raw.0 {
            VersionedTcp::V4(raw) => self
                .runtime
                .borrow_mut()
                .tcp_read(raw, &mut data)
                .map_err(map_error)?,
            VersionedTcp::V6(raw) => self
                .runtime
                .borrow_mut()
                .tcp_read_ipv6(raw, &mut data)
                .map_err(map_error)?,
        };
        data.truncate(read);
        if read == 0 && !raw.1 {
            return Err(RemoteSocketError::WouldBlock);
        }
        Ok(ProviderRecvMsgV2 {
            source: None,
            original_len: read as u32,
            flags: 0,
            eof: read == 0,
            data,
        })
    }

    fn shutdown(
        &mut self,
        handle: RemoteSocketHandle,
        how: ProviderShutdownV2,
    ) -> Result<(), RemoteSocketError> {
        let v2 = self.v2(handle)?;
        if v2.connecting {
            return Err(RemoteSocketError::InProgress);
        }
        if v2.peer.is_none() || v2.connect_failed {
            return Err(RemoteSocketError::InvalidState);
        }
        drop(v2);
        let how_runtime = match how {
            ProviderShutdownV2::Read => TcpShutdown::Receive,
            ProviderShutdownV2::Write => TcpShutdown::Send,
            ProviderShutdownV2::ReadWrite => TcpShutdown::SendAndReceive,
        };
        match self.state.borrow().sockets.get(&handle) {
            Some(Socket::Udp {
                raw: VersionedUdp::V4(raw),
                ..
            }) => self
                .runtime
                .borrow_mut()
                .udp_shutdown(*raw, how_runtime)
                .map_err(map_error)?,
            Some(Socket::Udp {
                raw: VersionedUdp::V6(raw),
                ..
            }) => self
                .runtime
                .borrow_mut()
                .udp_shutdown_ipv6(*raw, how_runtime)
                .map_err(map_error)?,
            Some(Socket::Tcp {
                raw: VersionedTcp::V4(raw),
                ..
            }) => self
                .runtime
                .borrow_mut()
                .tcp_shutdown(*raw, how_runtime)
                .map_err(map_error)?,
            Some(Socket::Tcp {
                raw: VersionedTcp::V6(raw),
                ..
            }) => self
                .runtime
                .borrow_mut()
                .tcp_shutdown_ipv6(*raw, how_runtime)
                .map_err(map_error)?,
            None => return Err(RemoteSocketError::StaleHandle),
        }
        match self.state.borrow_mut().sockets.get_mut(&handle).unwrap() {
            Socket::Udp { v2, .. } | Socket::Tcp { v2, .. } => {
                v2.read_closed |= matches!(
                    how,
                    ProviderShutdownV2::Read | ProviderShutdownV2::ReadWrite
                );
                v2.write_closed |= matches!(
                    how,
                    ProviderShutdownV2::Write | ProviderShutdownV2::ReadWrite
                );
            }
        }
        Ok(())
    }

    fn get_name(
        &mut self,
        handle: RemoteSocketHandle,
        which: ProviderNameV2,
    ) -> Result<ProviderSocketAddressV2, RemoteSocketError> {
        let v2 = self.v2(handle)?;
        match which {
            ProviderNameV2::Local => v2
                .local
                .clone()
                .or_else(|| self.unspecified(handle, 0).ok())
                .ok_or(RemoteSocketError::InvalidState),
            ProviderNameV2::Peer => v2.peer.clone().ok_or(RemoteSocketError::InvalidState),
        }
    }

    fn take_socket_error(
        &mut self,
        handle: RemoteSocketHandle,
    ) -> Result<Option<RemoteSocketError>, RemoteSocketError> {
        self.refresh_tcp_connection(handle)?;
        match self.state.borrow_mut().sockets.get_mut(&handle) {
            Some(Socket::Udp { v2, .. }) | Some(Socket::Tcp { v2, .. }) => {
                Ok(v2.pending_error.take())
            }
            None => Err(RemoteSocketError::StaleHandle),
        }
    }

    fn set_option(
        &mut self,
        handle: RemoteSocketHandle,
        _option: ProviderOptionV2,
        _value: &[u8],
    ) -> Result<(), RemoteSocketError> {
        self.v2(handle)?;
        Err(RemoteSocketError::NotSupported)
    }

    fn get_option(
        &mut self,
        handle: RemoteSocketHandle,
        _option: ProviderOptionV2,
    ) -> Result<Vec<u8>, RemoteSocketError> {
        self.v2(handle)?;
        Err(RemoteSocketError::NotSupported)
    }

    fn readiness(
        &mut self,
        handle: RemoteSocketHandle,
    ) -> Result<ProviderReadinessSnapshotV2, RemoteSocketError> {
        self.refresh_tcp_connection(handle)?;
        let old = RemoteSocketProvider::readiness(self, handle)?;
        let mut state = self.state.borrow_mut();
        let v2 = match state.sockets.get_mut(&handle) {
            Some(Socket::Udp { v2, .. }) | Some(Socket::Tcp { v2, .. }) => v2,
            None => return Err(RemoteSocketError::StaleHandle),
        };
        let mut bits = 0;
        if old.readable || v2.read_closed {
            bits |= ProviderReadinessV2::READABLE;
        }
        if old.writable {
            bits |= ProviderReadinessV2::WRITABLE;
        }
        if old.incoming {
            bits |= ProviderReadinessV2::INCOMING;
        }
        if v2.read_closed {
            bits |= ProviderReadinessV2::READ_CLOSED;
        }
        if v2.write_closed {
            bits |= ProviderReadinessV2::WRITE_CLOSED;
        }
        if v2.pending_error.is_some() {
            bits |= ProviderReadinessV2::ERROR;
        }
        if v2.peer.is_some() && !v2.connecting && !v2.connect_failed {
            bits |= ProviderReadinessV2::CONNECTED;
        }
        if v2.connect_failed {
            bits |= ProviderReadinessV2::CONNECT_FAILED;
        }
        let current = (ProviderReadinessV2(bits), v2.pending_error);
        if v2.last_readiness != Some(current) {
            v2.sequence = v2
                .sequence
                .checked_add(1)
                .ok_or(RemoteSocketError::ResourceExhausted)?;
            v2.last_readiness = Some(current);
        }
        Ok(ProviderReadinessSnapshotV2 {
            sequence: v2.sequence,
            readiness: current.0,
            error: v2.pending_error,
        })
    }

    fn close(&mut self, handle: RemoteSocketHandle) -> Result<(), RemoteSocketError> {
        RemoteSocketProvider::close(self, handle)
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

    #[test]
    fn v2_uses_endpoint_identity_and_reports_actual_ephemeral_bind() {
        let mut provider = provider();
        let client = SocketClientId::from_raw(42);
        netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::open_client(
            &mut provider,
            client,
            1,
        )
        .unwrap();
        assert_eq!(
            netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::open_client(
                &mut provider,
                client,
                1
            ),
            Err(RemoteSocketError::InvalidState)
        );
        let socket =
            netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::open_socket(
                &mut provider,
                client,
                ProviderSocketKindV2::Udp,
                RemoteIpVersion::V4,
            )
            .unwrap();
        let local = netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::bind(
            &mut provider,
            socket,
            None,
            0,
        )
        .unwrap();
        assert_eq!(local.address, RemoteIpAddress::V4([0; 4]));
        assert_ne!(local.port, 0);
        assert_eq!(
            netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::get_name(
                &mut provider,
                socket,
                ProviderNameV2::Local,
            )
            .unwrap(),
            local
        );
        let first = netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::readiness(
            &mut provider,
            socket,
        )
        .unwrap();
        let second = netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::readiness(
            &mut provider,
            socket,
        )
        .unwrap();
        assert_eq!(second.sequence, first.sequence);
        assert_eq!(provider.take_readiness_changes(), [(client, socket, first)]);
        assert!(provider.take_readiness_changes().is_empty());
        assert_eq!(
            netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::set_option(
                &mut provider,
                socket,
                ProviderOptionV2::Broadcast,
                &[1],
            ),
            Err(RemoteSocketError::NotSupported)
        );
        netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::close_client(
            &mut provider,
            client,
        )
        .unwrap();
        assert_eq!(
            netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::open_client(
                &mut provider,
                client,
                1,
            ),
            Err(RemoteSocketError::InvalidState)
        );
        assert_eq!(
            netstack3_port_spike::provider_dispatch_v2::RemoteSocketProviderV2::close(
                &mut provider,
                socket
            ),
            Err(RemoteSocketError::StaleHandle)
        );
    }
}
