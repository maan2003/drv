//! Owned application sockets. Runtime alone owns core IDs and buffer notification hooks.
//! No client IDs, remote handles, wire types, or observer sequence numbers here.
use crate::{
    NativeIpAddress as Ip, NativeSocketAddress as Address, NativeSocketInfo, NativeUdpDatagram,
    Runtime, RuntimeError as Error, TcpShutdown, TcpSocketHandle, UdpSocketHandle,
};
use std::{
    cell::RefCell,
    num::{NonZeroU16, NonZeroUsize},
    rc::Rc,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpVersion {
    V4,
    V6,
}

#[derive(Clone, Copy)]
enum Identity {
    Tcp(IpVersion, TcpSocketHandle),
    Udp(IpVersion, UdpSocketHandle),
}
impl Identity {
    fn close(self, rt: &mut Runtime) {
        let result = match self {
            Self::Tcp(IpVersion::V4, h) => rt.tcp_close(h),
            Self::Tcp(IpVersion::V6, h) => rt.tcp_close_ipv6(h),
            Self::Udp(IpVersion::V4, h) => rt.udp_close(h),
            Self::Udp(IpVersion::V6, h) => rt.udp_close_ipv6(h),
        };
        debug_assert!(result.is_ok());
    }
}

/// Cloning the factory never duplicates socket ownership.
/// Deferred closes retain the Runtime registry's resource charge until reaped.
#[derive(Clone)]
pub struct Sockets {
    pub(crate) runtime: Rc<RefCell<Runtime>>,
    closing: Rc<RefCell<Vec<Identity>>>,
}
impl Sockets {
    pub(crate) fn new(runtime: Rc<RefCell<Runtime>>) -> Self {
        Self {
            runtime,
            closing: Rc::default(),
        }
    }
    /// Called before admission and each service turn, outside Runtime borrows.
    pub fn reap(&self) {
        let mut rt = self.runtime.borrow_mut();
        for id in self.closing.borrow_mut().drain(..) {
            id.close(&mut rt);
        }
    }
    pub fn tcp(&self, version: IpVersion) -> Result<TcpSocket, Error> {
        self.reap();
        let h = match version {
            IpVersion::V4 => self.runtime.borrow_mut().tcp_socket()?,
            IpVersion::V6 => self.runtime.borrow_mut().tcp_socket_ipv6()?,
        };
        Ok(TcpSocket {
            lease: Lease {
                sockets: self.clone(),
                id: Identity::Tcp(version, h),
            },
            connection: Connection::Idle,
            error: None,
            read_closed: false,
            write_closed: false,
        })
    }
    pub fn udp(&self, version: IpVersion) -> Result<UdpSocket, Error> {
        self.reap();
        let h = match version {
            IpVersion::V4 => self.runtime.borrow_mut().udp_socket()?,
            IpVersion::V6 => self.runtime.borrow_mut().udp_socket_ipv6()?,
        };
        Ok(UdpSocket {
            lease: Lease {
                sockets: self.clone(),
                id: Identity::Udp(version, h),
            },
            peer: None,
            read_closed: false,
            write_closed: false,
        })
    }
}
struct Lease {
    sockets: Sockets,
    id: Identity,
}
impl Drop for Lease {
    fn drop(&mut self) {
        // Owners may be dropped while service.runtime() is borrowed. Never
        // reenter RefCell::borrow_mut from Drop; pending IDs are still charged.
        if let Ok(mut rt) = self.sockets.runtime.try_borrow_mut() {
            self.id.close(&mut rt);
        } else {
            self.sockets.closing.borrow_mut().push(self.id);
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Connection {
    Idle,
    Pending,
    Finished(Result<NativeSocketInfo, Error>),
}

pub struct TcpSocket {
    lease: Lease,
    connection: Connection,
    error: Option<Error>,
    read_closed: bool,
    write_closed: bool,
}
pub struct TcpListener {
    socket: TcpSocket,
}
pub struct UdpSocket {
    lease: Lease,
    peer: Option<Address>,
    read_closed: bool,
    write_closed: bool,
}

impl TcpSocket {
    fn raw(&self) -> (IpVersion, TcpSocketHandle) {
        match self.lease.id {
            Identity::Tcp(v, h) => (v, h),
            _ => unreachable!(),
        }
    }
    pub fn info(&self) -> Result<NativeSocketInfo, Error> {
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        match v {
            IpVersion::V4 => rt.tcp_socket_info(h),
            IpVersion::V6 => rt.tcp_socket_info_ipv6(h),
        }
    }
    pub fn bind(&mut self, address: Address) -> Result<(), Error> {
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        match (v, address.address) {
            (IpVersion::V4, Ip::V4(ip)) => rt.tcp_bind(
                h,
                (ip != [0; 4]).then_some(ip),
                NonZeroU16::new(address.port),
            ),
            (IpVersion::V6, Ip::V6(ip)) => rt.tcp_bind_ipv6(
                h,
                (ip != [0; 16]).then_some(ip),
                NonZeroU16::new(address.port),
            ),
            _ => Err(Error::InvalidAddress),
        }
    }
}

impl UdpSocket {
    fn raw(&self) -> (IpVersion, UdpSocketHandle) {
        match self.lease.id {
            Identity::Udp(v, h) => (v, h),
            _ => unreachable!(),
        }
    }
    pub fn info(&self) -> Result<NativeSocketInfo, Error> {
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        match v {
            IpVersion::V4 => rt.udp_socket_info(h),
            IpVersion::V6 => rt.udp_socket_info_ipv6(h),
        }
    }
    pub fn bind(&mut self, address: Address) -> Result<(), Error> {
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        match (v, address.address) {
            (IpVersion::V4, Ip::V4(ip)) => rt.udp_bind(
                h,
                (ip != [0; 4]).then_some(ip),
                NonZeroU16::new(address.port),
            ),
            (IpVersion::V6, Ip::V6(ip)) => rt.udp_bind_ipv6(
                h,
                (ip != [0; 16]).then_some(ip),
                NonZeroU16::new(address.port),
            ),
            _ => Err(Error::InvalidAddress),
        }
    }
}
impl TcpSocket {
    pub fn connect(&mut self, address: Address) -> Result<(), Error> {
        if self.connection != Connection::Idle {
            return Err(Error::InvalidState);
        }
        let port = NonZeroU16::new(address.port).ok_or(Error::InvalidAddress)?;
        let (v, h) = self.raw();
        let result = {
            let mut rt = self.lease.sockets.runtime.borrow_mut();
            match (v, address.address) {
                (IpVersion::V4, Ip::V4(ip)) => rt.tcp_connect(h, ip, port),
                (IpVersion::V6, Ip::V6(ip)) => rt.tcp_connect_ipv6(h, ip, port),
                _ => Err(Error::InvalidAddress),
            }
        };
        match result {
            Ok(()) | Err(Error::ConnectionPending) => {
                self.connection = Connection::Pending;
                Ok(())
            }
            Err(error) => {
                self.connection = Connection::Finished(Err(error));
                self.error = Some(error);
                Err(error)
            }
        }
    }
    /// A retained attempt result, deliberately independent of take_error().
    pub fn connection(&mut self) -> Result<Connection, Error> {
        self.observe()?;
        Ok(self.connection)
    }
    fn observe(&mut self) -> Result<netstack3_base::TcpSocketState, Error> {
        use netstack3_base::TcpSocketState as State;
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        let (state, error) = match v {
            IpVersion::V4 => (rt.tcp_state(h)?, rt.tcp_take_socket_error(h)?),
            IpVersion::V6 => (rt.tcp_state_ipv6(h)?, rt.tcp_take_socket_error_ipv6(h)?),
        };
        if let Some(error) = error {
            self.error = Some(error);
        }
        if self.connection == Connection::Pending {
            if let Some(error) = error {
                self.connection = Connection::Finished(Err(error));
            } else if matches!(state, State::Established | State::CloseWait) {
                let info = match v {
                    IpVersion::V4 => rt.tcp_socket_info(h)?,
                    IpVersion::V6 => rt.tcp_socket_info_ipv6(h)?,
                };
                self.connection = Connection::Finished(Ok(info));
            }
        }
        Ok(state)
    }
    pub fn take_error(&mut self) -> Result<Option<Error>, Error> {
        self.observe()?;
        Ok(self.error.take())
    }
    pub fn listen(self, backlog: NonZeroUsize) -> Result<TcpListener, (Self, Error)> {
        let (v, h) = self.raw();
        let result = {
            let mut rt = self.lease.sockets.runtime.borrow_mut();
            match v {
                IpVersion::V4 => rt.tcp_listen(h, backlog),
                IpVersion::V6 => rt.tcp_listen_ipv6(h, backlog),
            }
        };
        match result {
            Ok(()) => Ok(TcpListener { socket: self }),
            Err(e) => Err((self, e)),
        }
    }
    pub fn write(&mut self, bytes: &[u8]) -> Result<usize, Error> {
        if self.write_closed {
            return Err(Error::InvalidState);
        }
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        let result = match v {
            IpVersion::V4 => rt.tcp_write(h, bytes),
            IpVersion::V6 => rt.tcp_write_ipv6(h, bytes),
        }?;

        Ok(result)
    }
    pub fn read(&mut self, bytes: &mut [u8]) -> Result<usize, Error> {
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        let result = match v {
            IpVersion::V4 => rt.tcp_read(h, bytes),
            IpVersion::V6 => rt.tcp_read_ipv6(h, bytes),
        }?;

        Ok(result)
    }
    pub fn finish_write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.write_closed {
            return if bytes.is_empty() {
                Ok(())
            } else {
                Err(Error::InvalidState)
            };
        }
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        let result = match v {
            IpVersion::V4 => rt.tcp_finish_write(h, bytes),
            IpVersion::V6 => rt.tcp_finish_write_ipv6(h, bytes),
        }?;
        self.write_closed = true;
        Ok(result)
    }
    pub fn shutdown(&mut self, how: TcpShutdown) -> Result<(), Error> {
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        let result = match v {
            IpVersion::V4 => rt.tcp_shutdown(h, how),
            IpVersion::V6 => rt.tcp_shutdown_ipv6(h, how),
        }?;
        if matches!(how, TcpShutdown::Send | TcpShutdown::SendAndReceive) {
            self.write_closed = true;
        }
        if matches!(how, TcpShutdown::Receive | TcpShutdown::SendAndReceive) {
            self.read_closed = true;
        }
        Ok(result)
    }
    pub fn readiness(&mut self) -> Result<(bool, bool, bool), Error> {
        use netstack3_base::TcpSocketState as State;
        let state = self.observe()?;
        let (v, h) = self.raw();
        let rt = self.lease.sockets.runtime.borrow();
        let (readable, writable) = match v {
            IpVersion::V4 => rt.tcp_readiness(h)?,
            IpVersion::V6 => rt.tcp_readiness_ipv6(h)?,
        };
        let eof = self.read_closed
            || matches!(
                state,
                State::CloseWait | State::Closing | State::LastAck | State::TimeWait
            )
            || (state == State::Close && matches!(self.connection, Connection::Finished(_)));
        Ok((
            readable,
            writable
                && !self.write_closed
                && matches!(self.connection, Connection::Finished(Ok(_))),
            eof,
        ))
    }
}
impl TcpListener {
    pub fn info(&self) -> Result<NativeSocketInfo, Error> {
        self.socket.info()
    }
    pub fn accept(&mut self) -> Result<TcpSocket, Error> {
        self.socket.lease.sockets.reap();
        let (v, h) = self.socket.raw();
        let (h, local, peer) = {
            let mut rt = self.socket.lease.sockets.runtime.borrow_mut();
            match v {
                IpVersion::V4 => rt.tcp_accept_with_peer(h)?,
                IpVersion::V6 => rt.tcp_accept_ipv6_with_peer(h)?,
            }
        };
        Ok(TcpSocket {
            lease: Lease {
                sockets: self.socket.lease.sockets.clone(),
                id: Identity::Tcp(v, h),
            },
            connection: Connection::Finished(Ok(NativeSocketInfo {
                local,
                peer: Some(peer),
            })),
            error: None,
            read_closed: false,
            write_closed: false,
        })
    }
    pub fn pending(&self) -> Result<usize, Error> {
        let (v, h) = self.socket.raw();
        let rt = self.socket.lease.sockets.runtime.borrow();
        match v {
            IpVersion::V4 => rt.tcp_pending_connections(h),
            IpVersion::V6 => rt.tcp_pending_connections_ipv6(h),
        }
    }
}

impl UdpSocket {
    pub fn connect(&mut self, peer: Address) -> Result<(), Error> {
        let port = NonZeroU16::new(peer.port).ok_or(Error::InvalidAddress)?;
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        let result = match (v, peer.address) {
            (IpVersion::V4, Ip::V4(ip)) => rt.udp_connect(h, ip, port),
            (IpVersion::V6, Ip::V6(ip)) => rt.udp_connect_ipv6(h, ip, port),
            _ => Err(Error::InvalidAddress),
        };
        // Preserve deferred UDP route resolution: desired peer is not evidence
        // that core successfully connected. send_to retries normal routing.
        match result {
            Ok(()) | Err(Error::NetworkUnreachable | Error::HostUnreachable) => {
                self.peer = Some(peer);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
    pub fn peer(&self) -> Option<Address> {
        self.peer
    }
    pub fn send(&mut self, peer: Option<Address>, bytes: &[u8]) -> Result<(), Error> {
        if self.write_closed {
            return Err(Error::InvalidState);
        }
        let peer = peer.or(self.peer).ok_or(Error::InvalidState)?;
        let port = NonZeroU16::new(peer.port).ok_or(Error::InvalidAddress)?;
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        match (v, peer.address) {
            (IpVersion::V4, Ip::V4(ip)) => rt.udp_send_to(h, ip, port, bytes),
            (IpVersion::V6, Ip::V6(ip)) => rt.udp_send_to_ipv6(h, ip, port, bytes),
            _ => Err(Error::InvalidAddress),
        }
    }
    pub fn receive(&mut self) -> Result<Option<NativeUdpDatagram>, Error> {
        if self.read_closed {
            return Ok(None);
        }
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        match v {
            IpVersion::V4 => rt.udp_receive_msg(h),
            IpVersion::V6 => rt.udp_receive_msg_ipv6(h),
        }
    }
    /// Queue observation never dequeues or stages a datagram.
    pub fn readable(&self) -> Result<bool, Error> {
        if self.read_closed {
            return Ok(false);
        }
        let (v, h) = self.raw();
        let rt = self.lease.sockets.runtime.borrow();
        match v {
            IpVersion::V4 => rt.udp_readable(h),
            IpVersion::V6 => rt.udp_readable_ipv6(h),
        }
    }
    pub fn shutdown(&mut self, how: TcpShutdown) -> Result<(), Error> {
        let (v, h) = self.raw();
        let mut rt = self.lease.sockets.runtime.borrow_mut();
        match v {
            IpVersion::V4 => rt.udp_shutdown(h, how)?,
            IpVersion::V6 => rt.udp_shutdown_ipv6(h, how)?,
        }
        if matches!(how, TcpShutdown::Send | TcpShutdown::SendAndReceive) {
            self.write_closed = true;
        }
        if matches!(how, TcpShutdown::Receive | TcpShutdown::SendAndReceive) {
            self.read_closed = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;
    fn factory() -> Sockets {
        let mut rt = Runtime::new(
            8,
            (0..=255).cycle().take(65536),
            NonZeroU64::new(1).unwrap(),
            [2, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        rt.enable_loopback();
        Sockets::new(Rc::new(RefCell::new(rt)))
    }
    fn local(port: u16) -> Address {
        Address {
            address: Ip::V4([127, 0, 0, 1]),
            port,
        }
    }
    fn pump(s: &Sockets) {
        for _ in 0..32 {
            s.runtime.borrow_mut().dispatch_due(128);
        }
    }
    #[test]
    fn drop_under_runtime_borrow_defers_without_losing_charge() {
        let s = factory();
        let tcp = s.tcp(IpVersion::V4).unwrap();
        let udp = s.udp(IpVersion::V6).unwrap();
        let rt = s.runtime.borrow();
        let count = rt.socket_count();
        drop(tcp);
        drop(udp);
        assert_eq!(rt.socket_count(), count);
        assert_eq!(s.closing.borrow().len(), 2);
        drop(rt);
        s.reap();
        assert_eq!(s.runtime.borrow().socket_count(), count - 2);
    }
    #[test]
    fn udp_observation_preserves_zero_length_record_and_autobind() {
        let s = factory();
        let mut receiver = s.udp(IpVersion::V4).unwrap();
        receiver.bind(local(4010)).unwrap();
        let mut sender = s.udp(IpVersion::V4).unwrap();
        sender.send(Some(local(4010)), &[]).unwrap();
        pump(&s);
        assert_ne!(sender.info().unwrap().local.port, 0);
        assert!(receiver.readable().unwrap());
        assert!(receiver.readable().unwrap());
        assert!(receiver.receive().unwrap().unwrap().body.is_empty());
        assert!(!receiver.readable().unwrap());
    }
    #[test]
    fn tcp_accept_transfers_owner_and_preserves_buffer_hooks() {
        let s = factory();
        let mut socket = s.tcp(IpVersion::V4).unwrap();
        socket.bind(local(4020)).unwrap();
        let mut listener = socket
            .listen(NonZeroUsize::new(2).unwrap())
            .map_err(|(_, e)| e)
            .unwrap();
        let mut client = s.tcp(IpVersion::V4).unwrap();
        client.connect(local(4020)).unwrap();
        pump(&s);
        let mut accepted = listener.accept().unwrap();
        assert!(matches!(
            client.connection().unwrap(),
            Connection::Finished(Ok(_))
        ));
        assert_eq!(client.take_error().unwrap(), None);
        assert!(matches!(
            client.connection().unwrap(),
            Connection::Finished(Ok(_))
        ));
        drop(listener);
        assert_eq!(client.write(b"owned stream").unwrap(), 12);
        pump(&s);
        let mut bytes = [0; 32];
        assert_eq!(accepted.read(&mut bytes).unwrap(), 12);
        assert_eq!(&bytes[..12], b"owned stream");
        client.finish_write(b"tail").unwrap();
        client.shutdown(TcpShutdown::Send).unwrap();
        pump(&s);
        assert_eq!(accepted.read(&mut bytes).unwrap(), 4);
        assert_eq!(&bytes[..4], b"tail");
    }
    #[test]
    fn unconnected_stream_io_and_final_close_respect_core_state() {
        let s = factory();
        for version in [IpVersion::V4, IpVersion::V6] {
            for bound in [false, true] {
                let mut socket = s.tcp(version).unwrap();
                if bound {
                    socket
                        .bind(Address {
                            address: match version {
                                IpVersion::V4 => Ip::V4([127, 0, 0, 1]),
                                IpVersion::V6 => {
                                    Ip::V6([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1])
                                }
                            },
                            port: 0,
                        })
                        .unwrap();
                }
                assert_eq!(socket.write(b"unconnected"), Err(Error::InvalidState));
                assert_eq!(
                    socket.finish_write(b"unconnected"),
                    Err(Error::InvalidState)
                );
                socket.finish_write(&[]).unwrap();
                socket.finish_write(&[]).unwrap();
                drop(socket);
            }
        }
    }
    #[test]
    fn connecting_stream_can_seal_before_completion() {
        let s = factory();
        let mut socket = s.tcp(IpVersion::V4).unwrap();
        socket.bind(local(4021)).unwrap();
        let mut listener = socket
            .listen(NonZeroUsize::new(1).unwrap())
            .map_err(|(_, e)| e)
            .unwrap();
        let mut client = s.tcp(IpVersion::V4).unwrap();
        client.connect(local(4021)).unwrap();
        client.finish_write(b"admitted while connecting").unwrap();
        client.finish_write(&[]).unwrap();
        pump(&s);
        let mut accepted = listener.accept().unwrap();
        let mut bytes = [0; 64];
        let n = accepted.read(&mut bytes).unwrap();
        assert_eq!(&bytes[..n], b"admitted while connecting");
    }
    #[test]
    fn failed_attempt_survives_error_consumption() {
        let s = factory();
        let mut client = s.tcp(IpVersion::V4).unwrap();
        client.connect(local(4030)).unwrap();
        pump(&s);
        assert_eq!(
            client.connection().unwrap(),
            Connection::Finished(Err(Error::ConnectionRefused))
        );
        assert_eq!(client.take_error().unwrap(), Some(Error::ConnectionRefused));
        assert_eq!(client.take_error().unwrap(), None);
        assert_eq!(
            client.connection().unwrap(),
            Connection::Finished(Err(Error::ConnectionRefused))
        );
        client.finish_write(&[]).unwrap();
        client.finish_write(&[]).unwrap();
    }
}
