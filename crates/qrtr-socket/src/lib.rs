#![cfg(target_os = "linux")]
#![deny(unsafe_op_in_unsafe_fn)]
//! Safe, owned access to Linux Qualcomm IPC Router datagram sockets.
//! Raw socket-address layouts deliberately remain private.

use std::{
    io, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    time::{Duration, Instant},
};

// libc does not expose AF_QIPCRTR on every Linux libc target.
const AF_QIPCRTR: libc::c_int = 42;
/// The distinguished QRTR name-service port.
pub const CONTROL_PORT: u32 = 0xffff_fffe;
const QRTR_TYPE_BYE: u32 = 3;
const QRTR_TYPE_NEW_SERVER: u32 = 4;
const QRTR_TYPE_DEL_SERVER: u32 = 5;
const QRTR_TYPE_NEW_LOOKUP: u32 = 10;
const CONTROL_PACKET_LEN: usize = 20;

/// A typed QRTR endpoint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct QrtrAddr {
    pub node: u32,
    pub port: u32,
}

/// A QRTR control-port event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlEvent {
    /// Marks the end of the name service's initial matching-server list.
    LookupComplete,
    ServerAdded {
        service: u32,
        instance: u32,
        addr: QrtrAddr,
    },
    ServerRemoved {
        service: u32,
        instance: u32,
        addr: QrtrAddr,
    },
    Bye {
        node: u32,
    },
}

impl ControlEvent {
    /// Parses a control datagram, validating both its source and wire length.
    pub fn parse(packet: &[u8], source: QrtrAddr) -> io::Result<Self> {
        if source.port != CONTROL_PORT {
            return Err(invalid_data("control packet came from a non-control port"));
        }
        if packet.len() != CONTROL_PACKET_LEN {
            return Err(invalid_data("QRTR control packet must be exactly 20 bytes"));
        }
        let command = get_u32(packet, 0);
        if command == QRTR_TYPE_BYE {
            return Ok(Self::Bye {
                node: get_u32(packet, 4),
            });
        }
        let service = get_u32(packet, 4);
        let instance = get_u32(packet, 8);
        let addr = QrtrAddr {
            node: get_u32(packet, 12),
            port: get_u32(packet, 16),
        };
        if command == QRTR_TYPE_NEW_SERVER
            && service == 0
            && instance == 0
            && addr.node == 0
            && addr.port == 0
        {
            return Ok(Self::LookupComplete);
        }
        match command {
            QRTR_TYPE_NEW_SERVER => Ok(Self::ServerAdded {
                service,
                instance,
                addr,
            }),
            QRTR_TYPE_DEL_SERVER => Ok(Self::ServerRemoved {
                service,
                instance,
                addr,
            }),
            _ => Err(invalid_data("unsupported QRTR control command")),
        }
    }
}

/// One received QRTR datagram, classified without exposing raw addresses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecvEvent {
    Data { len: usize, source: QrtrAddr },
    Control(ControlEvent),
}

/// An owned `AF_QIPCRTR`, datagram-mode socket.
#[derive(Debug)]
pub struct QrtrSocket {
    fd: OwnedFd,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockAddrQrtr {
    family: libc::sa_family_t,
    // Explicit initialized bytes for the C ABI's hole before the u32 fields.
    padding: [u8; 2],
    node: u32,
    port: u32,
}

impl SockAddrQrtr {
    fn new(addr: QrtrAddr) -> Self {
        Self {
            family: AF_QIPCRTR as libc::sa_family_t,
            padding: [0; 2],
            node: addr.node,
            port: addr.port,
        }
    }

    fn empty() -> Self {
        Self {
            family: 0,
            padding: [0; 2],
            node: 0,
            port: 0,
        }
    }

    fn typed(self) -> io::Result<QrtrAddr> {
        if libc::c_int::from(self.family) != AF_QIPCRTR {
            return Err(invalid_data("kernel returned a non-QRTR address"));
        }
        Ok(QrtrAddr {
            node: self.node,
            port: self.port,
        })
    }
}

impl QrtrSocket {
    /// Adopts an already-open socket without inspecting or operating on it.
    ///
    /// The caller is responsible for ensuring that `fd` is an appropriate
    /// `AF_QIPCRTR`, datagram-mode socket with the desired descriptor flags.
    pub fn adopt(fd: OwnedFd) -> Self {
        Self { fd }
    }

    /// Opens an owned close-on-exec QRTR datagram socket.
    pub fn open() -> io::Result<Self> {
        // SAFETY: socket takes no pointer arguments; success returns a new fd
        // whose ownership is transferred immediately to OwnedFd.
        let fd = unsafe { libc::socket(AF_QIPCRTR, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful socket call returned a fresh owned descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Self::adopt(fd))
    }

    /// Returns the descriptor identity for an fd-bound sandbox policy.
    pub fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Binds locally. `addr.node` must equal `local_addr()?.node`; port zero
    /// requests a kernel-selected ephemeral port.
    pub fn bind(&self, addr: QrtrAddr) -> io::Result<()> {
        let raw = SockAddrQrtr::new(addr);
        // SAFETY: raw is a live repr(C) sockaddr_qrtr of the supplied length.
        let result = unsafe {
            libc::bind(
                self.fd.as_raw_fd(),
                (&raw as *const SockAddrQrtr).cast(),
                addr_len(),
            )
        };
        cvt_zero(result)
    }

    pub fn local_addr(&self) -> io::Result<QrtrAddr> {
        get_socket_addr(self.fd.as_raw_fd(), false)
    }

    pub fn connect(&self, addr: QrtrAddr) -> io::Result<()> {
        let raw = SockAddrQrtr::new(addr);
        // SAFETY: raw is a live repr(C) sockaddr_qrtr of the supplied length.
        let result = unsafe {
            libc::connect(
                self.fd.as_raw_fd(),
                (&raw as *const SockAddrQrtr).cast(),
                addr_len(),
            )
        };
        cvt_zero(result)
    }

    pub fn peer_addr(&self) -> io::Result<QrtrAddr> {
        get_socket_addr(self.fd.as_raw_fd(), true)
    }

    pub fn send(&self, bytes: &[u8]) -> io::Result<usize> {
        send_connected(self.fd.as_raw_fd(), bytes)
    }

    pub fn send_to(&self, bytes: &[u8], addr: QrtrAddr) -> io::Result<usize> {
        send_to_fd(self.fd.as_raw_fd(), bytes, addr, 0)
    }

    /// Receives from a connected peer within a monotonic timeout.
    pub fn recv(&self, bytes: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let deadline = deadline_after(timeout)?;
        loop {
            wait_readable(self.fd.as_raw_fd(), deadline)?;
            match recv_connected(self.fd.as_raw_fd(), bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }

    /// Receives a datagram and typed source within a monotonic timeout.
    pub fn recv_from(&self, bytes: &mut [u8], timeout: Duration) -> io::Result<(usize, QrtrAddr)> {
        self.recv_from_until(bytes, deadline_after(timeout)?)
    }

    /// Receives either application data or a parsed control notification.
    pub fn recv_event(&self, bytes: &mut [u8], timeout: Duration) -> io::Result<RecvEvent> {
        let (len, source) = self.recv_from(bytes, timeout)?;
        if source.port == CONTROL_PORT {
            Ok(RecvEvent::Control(ControlEvent::parse(
                &bytes[..len],
                source,
            )?))
        } else {
            Ok(RecvEvent::Data { len, source })
        }
    }

    /// Registers a persistent lookup for a packed UAPI instance word.
    /// The kernel treats service zero and instance zero as wildcards.
    pub fn subscribe(&self, service: u32, instance: u32) -> io::Result<()> {
        let packet = encode_lookup(service, instance);
        let destination = QrtrAddr {
            node: self.local_addr()?.node,
            port: CONTROL_PORT,
        };
        self.send_to(&packet, destination)?;
        Ok(())
    }

    fn subscribe_until(&self, service: u32, instance: u32, deadline: Instant) -> io::Result<()> {
        let packet = encode_lookup(service, instance);
        let destination = QrtrAddr {
            node: self.local_addr()?.node,
            port: CONTROL_PORT,
        };
        loop {
            wait_ready(self.fd.as_raw_fd(), libc::POLLOUT, deadline)?;
            match send_to_fd(
                self.fd.as_raw_fd(),
                &packet,
                destination,
                libc::MSG_DONTWAIT,
            ) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Ok(_) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }

    /// Registers a lookup and returns its first matching advertised endpoint.
    /// The registration remains active; later removal is returned by
    /// `recv_event` as `ControlEvent::ServerRemoved`.
    pub fn lookup(&self, service: u32, instance: u32, timeout: Duration) -> io::Result<QrtrAddr> {
        let deadline = deadline_after(timeout)?;
        self.subscribe_until(service, instance, deadline)?;
        let mut packet = [0_u8; CONTROL_PACKET_LEN];
        loop {
            let (len, source) = self.recv_from_until(&mut packet, deadline)?;
            if let ControlEvent::ServerAdded {
                service: found_service,
                instance: found_instance,
                addr,
            } = ControlEvent::parse(&packet[..len], source)?
                && found_service == service
                && found_instance == instance
            {
                return Ok(addr);
            }
        }
    }

    fn recv_from_until(
        &self,
        bytes: &mut [u8],
        deadline: Instant,
    ) -> io::Result<(usize, QrtrAddr)> {
        loop {
            wait_readable(self.fd.as_raw_fd(), deadline)?;
            match recv_from_fd(self.fd.as_raw_fd(), bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}

/// Whether an I/O failure denotes loss of a connected QRTR endpoint or node.
pub fn is_disconnect_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::BrokenPipe
    ) || matches!(error.raw_os_error(), Some(libc::ENETRESET | libc::ENODEV))
}

fn encode_lookup(service: u32, instance: u32) -> [u8; CONTROL_PACKET_LEN] {
    let mut packet = [0_u8; CONTROL_PACKET_LEN];
    put_u32(&mut packet, 0, QRTR_TYPE_NEW_LOOKUP);
    put_u32(&mut packet, 4, service);
    put_u32(&mut packet, 8, instance);
    packet
}

fn put_u32(packet: &mut [u8], offset: usize, value: u32) {
    packet[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(packet: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        packet[offset..offset + 4]
            .try_into()
            .expect("four-byte field"),
    )
}

fn get_socket_addr(fd: RawFd, peer: bool) -> io::Result<QrtrAddr> {
    let mut raw = SockAddrQrtr::empty();
    let mut length = addr_len();
    // SAFETY: raw is fully initialized and writable for length bytes; the
    // descriptor remains valid throughout the call.
    let result = unsafe {
        if peer {
            libc::getpeername(fd, (&mut raw as *mut SockAddrQrtr).cast(), &mut length)
        } else {
            libc::getsockname(fd, (&mut raw as *mut SockAddrQrtr).cast(), &mut length)
        }
    };
    cvt_zero(result)?;
    validate_addr_len(length)?;
    raw.typed()
}

fn recv_from_fd(fd: RawFd, bytes: &mut [u8]) -> io::Result<(usize, QrtrAddr)> {
    let mut raw = SockAddrQrtr::empty();
    let mut length = addr_len();
    // SAFETY: both fully initialized buffers are writable for their supplied
    // lengths and remain live throughout recvfrom.
    let result = unsafe {
        libc::recvfrom(
            fd,
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            libc::MSG_TRUNC | libc::MSG_DONTWAIT,
            (&mut raw as *mut SockAddrQrtr).cast(),
            &mut length,
        )
    };
    let received = received_len(result, bytes.len())?;
    validate_addr_len(length)?;
    let source = raw.typed()?;
    Ok((received, source))
}

fn send_to_fd(fd: RawFd, bytes: &[u8], addr: QrtrAddr, flags: libc::c_int) -> io::Result<usize> {
    let raw = SockAddrQrtr::new(addr);
    // SAFETY: slice and address are readable for their supplied lengths and
    // remain live for the duration of sendto.
    let result = unsafe {
        libc::sendto(
            fd,
            bytes.as_ptr().cast(),
            bytes.len(),
            flags,
            (&raw as *const SockAddrQrtr).cast(),
            addr_len(),
        )
    };
    complete_datagram(result, bytes.len())
}

fn send_connected(fd: RawFd, bytes: &[u8]) -> io::Result<usize> {
    // SAFETY: bytes is readable for its length and send does not retain it.
    let result = unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), 0) };
    complete_datagram(result, bytes.len())
}

fn recv_connected(fd: RawFd, bytes: &mut [u8]) -> io::Result<usize> {
    // SAFETY: bytes is writable for its length and recv does not retain it.
    let result = unsafe {
        libc::recv(
            fd,
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            libc::MSG_TRUNC | libc::MSG_DONTWAIT,
        )
    };
    received_len(result, bytes.len())
}

fn wait_readable(fd: RawFd, deadline: Instant) -> io::Result<()> {
    wait_ready(fd, libc::POLLIN, deadline)
}

fn wait_ready(fd: RawFd, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    loop {
        let mut poll_fd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        // SAFETY: poll_fd is one initialized, exclusively borrowed pollfd.
        let result = unsafe { libc::poll(&mut poll_fd, 1, poll_timeout(deadline)) };
        if result > 0 {
            if poll_fd.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
            return Ok(());
        }
        if result == 0 {
            if Instant::now() < deadline {
                // Very distant deadlines are polled in c_int::MAX-ms chunks.
                continue;
            }
            return Err(io::Error::new(io::ErrorKind::TimedOut, "receive timeout"));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn deadline_after(timeout: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "timeout is too large"))
}

fn poll_timeout(deadline: Instant) -> libc::c_int {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return 0;
    }
    // Round up so poll cannot report timeout before the monotonic deadline.
    let milliseconds =
        remaining.as_millis() + u128::from(!remaining.subsec_nanos().is_multiple_of(1_000_000));
    milliseconds.min(libc::c_int::MAX as u128) as libc::c_int
}

fn complete_datagram(result: libc::ssize_t, expected: usize) -> io::Result<usize> {
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if result as usize != expected {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short datagram send",
        ));
    }
    Ok(result as usize)
}

fn received_len(result: libc::ssize_t, capacity: usize) -> io::Result<usize> {
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if result as usize > capacity {
        return Err(invalid_data("datagram exceeds supplied receive buffer"));
    }
    Ok(result as usize)
}

fn cvt_zero(result: libc::c_int) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn addr_len() -> libc::socklen_t {
    mem::size_of::<SockAddrQrtr>() as libc::socklen_t
}

fn validate_addr_len(length: libc::socklen_t) -> io::Result<()> {
    if length as usize == mem::size_of::<SockAddrQrtr>() {
        Ok(())
    } else {
        Err(invalid_data(
            "kernel returned an invalid QRTR address length",
        ))
    }
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram;

    fn control_packet(command: u32) -> [u8; CONTROL_PACKET_LEN] {
        let mut packet = [0_u8; CONTROL_PACKET_LEN];
        put_u32(&mut packet, 0, command);
        put_u32(&mut packet, 4, 0x2a);
        put_u32(&mut packet, 8, 0x0102_0307);
        put_u32(&mut packet, 12, 9);
        put_u32(&mut packet, 16, 0x4000);
        packet
    }

    fn control_source() -> QrtrAddr {
        QrtrAddr {
            node: 3,
            port: CONTROL_PORT,
        }
    }

    #[test]
    fn lookup_is_hand_coded_little_endian() {
        let packet = encode_lookup(0x1122_3344, 0x5566_7788);
        assert_eq!(&packet[0..4], &[10, 0, 0, 0]);
        assert_eq!(&packet[4..8], &[0x44, 0x33, 0x22, 0x11]);
        assert_eq!(&packet[8..12], &[0x88, 0x77, 0x66, 0x55]);
        assert_eq!(&packet[12..], &[0; 8]);
    }

    #[test]
    fn adoption_is_inert_and_preserves_descriptor_identity() {
        let (socket, _peer) = UnixDatagram::pair().unwrap();
        let expected = socket.as_raw_fd();
        let socket = QrtrSocket::adopt(socket.into());

        assert_eq!(socket.raw_fd(), expected);
        // Adoption accepts ownership without querying or validating the fd;
        // an operation is where the non-QRTR descriptor is rejected.
        assert!(socket.local_addr().is_err());
    }

    #[test]
    fn parses_server_lifecycle_and_bye() {
        let addr = QrtrAddr {
            node: 9,
            port: 0x4000,
        };
        assert_eq!(
            ControlEvent::parse(&control_packet(QRTR_TYPE_NEW_SERVER), control_source()).unwrap(),
            ControlEvent::ServerAdded {
                service: 0x2a,
                instance: 0x0102_0307,
                addr
            }
        );
        assert_eq!(
            ControlEvent::parse(&control_packet(QRTR_TYPE_DEL_SERVER), control_source()).unwrap(),
            ControlEvent::ServerRemoved {
                service: 0x2a,
                instance: 0x0102_0307,
                addr
            }
        );
        assert_eq!(
            ControlEvent::parse(&control_packet(QRTR_TYPE_BYE), control_source()).unwrap(),
            ControlEvent::Bye { node: 0x2a }
        );
        let mut complete = [0_u8; CONTROL_PACKET_LEN];
        put_u32(&mut complete, 0, QRTR_TYPE_NEW_SERVER);
        assert_eq!(
            ControlEvent::parse(&complete, control_source()).unwrap(),
            ControlEvent::LookupComplete
        );
    }

    #[test]
    fn rejects_bad_control_source_length_and_command() {
        let mut non_control = control_source();
        non_control.port = 1;
        assert!(ControlEvent::parse(&control_packet(QRTR_TYPE_BYE), non_control).is_err());
        assert!(ControlEvent::parse(&[0; 19], control_source()).is_err());
        assert!(
            ControlEvent::parse(&control_packet(QRTR_TYPE_NEW_LOOKUP), control_source()).is_err()
        );
    }

    #[test]
    fn unix_datagram_helpers_preserve_boundaries() {
        let (left, right) = UnixDatagram::pair().unwrap();
        let payload = b"one complete datagram";
        assert_eq!(
            send_connected(left.as_raw_fd(), payload).unwrap(),
            payload.len()
        );
        wait_readable(
            right.as_raw_fd(),
            deadline_after(Duration::from_secs(1)).unwrap(),
        )
        .unwrap();
        let mut output = [0_u8; 64];
        let len = recv_connected(right.as_raw_fd(), &mut output).unwrap();
        assert_eq!(&output[..len], payload);
    }

    #[test]
    fn truncation_is_an_error() {
        let (left, right) = UnixDatagram::pair().unwrap();
        left.send(b"too long").unwrap();
        assert_eq!(
            recv_connected(right.as_raw_fd(), &mut [0_u8; 2])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn poll_honors_monotonic_deadline() {
        let (_left, right) = UnixDatagram::pair().unwrap();
        let error = wait_readable(
            right.as_raw_fd(),
            deadline_after(Duration::from_millis(10)).unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn classifies_disconnect_errors() {
        assert!(is_disconnect_error(&io::Error::from_raw_os_error(
            libc::ENODEV
        )));
        assert!(is_disconnect_error(&io::Error::from(
            io::ErrorKind::ConnectionReset
        )));
        assert!(!is_disconnect_error(&io::Error::from(
            io::ErrorKind::TimedOut
        )));
    }
}
