// SPDX-License-Identifier: GPL-2.0-only

//! Wi-Fi-side policy control service.
//!
//! This crate owns the bounded Unix transport and runtime dispatch only. The
//! forthcoming `wlancfg-service` crate is the peer/client owner. Physical
//! construction and self-sandbox setup remain with the Wi-Fi executable.

#![allow(async_fn_in_trait)]

use fidl_fuchsia_wlan_sme as sme;
use std::collections::VecDeque;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};
use wifi_supervisor_wire::{LifecycleKind, LifecycleMessage};
use wlan_control_wire::{
    CommandReply, ConnectReply, GenerationEndReason, MAX_PACKET, Message, Packet, Reply,
    SessionValidator, decode, encode, required_fd_count,
};

pub const MAX_OUTBOUND_PACKETS: usize = 64;
pub const MAX_OUTBOUND_BYTES: usize = 64 * 1024;
const NORMAL_PACKET_LIMIT: usize = MAX_OUTBOUND_PACKETS - 1;
const NORMAL_BYTE_LIMIT: usize = MAX_OUTBOUND_BYTES - MAX_PACKET;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    /// Exact SME failure after retry-safe cleanup. Returning this certifies
    /// that the old transaction and event route are quiescent.
    Failed(sme::ConnectResult),
    Timeout,
    DriverFault,
    ContainmentFault,
}

/// Narrow service seam matching the retained production client operations.
///
/// An adapter for `Mt7921ProductionClient` only converts its
/// `HostEthernetDevice::into_frame_fd()` result in `take_ethernet_device`; it
/// must not reproduce runtime state or physical construction here.
pub trait WifiRuntime {
    fn public_mac(&self) -> [u8; 6];
    fn take_ethernet_device(&mut self) -> Option<OwnedFd>;
    fn begin_connect(
        &mut self,
        request: sme::ConnectRequest,
        deadline: Instant,
    ) -> Result<(), RuntimeError>;
    async fn drive_connect_once(&mut self) -> Result<Option<sme::ConnectResult>, RuntimeError>;
    async fn cancel_connect(
        &mut self,
        reason: sme::UserDisconnectReason,
        deadline: Instant,
    ) -> Result<sme::ConnectResult, RuntimeError>;
    fn roam(&mut self, request: sme::RoamRequest) -> Result<(), RuntimeError>;
    fn begin_scan(
        &mut self,
        request: sme::ScanRequest,
        deadline: Instant,
    ) -> Result<(), RuntimeError>;
    async fn drive_scan_once(
        &mut self,
    ) -> Result<Option<Result<Vec<sme::ScanResult>, sme::ScanErrorCode>>, RuntimeError>;
    async fn drive_once(&mut self) -> Result<bool, RuntimeError>;
    fn next_connection_event(
        &mut self,
    ) -> Result<Option<sme::ConnectTransactionEvent>, RuntimeError>;
    async fn disconnect(
        &mut self,
        reason: sme::UserDisconnectReason,
        deadline: Instant,
    ) -> Result<(), RuntimeError>;
}

#[derive(Debug)]
pub enum EndpointError {
    Io(io::Error),
    NotUnix,
    NotSeqpacket,
    NotConnected,
    TruncatedPacket,
    TruncatedAncillary,
    WrongFdCount { expected: usize, actual: usize },
    Wire(wlan_control_wire::Error),
}

impl From<io::Error> for EndpointError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<wlan_control_wire::Error> for EndpointError {
    fn from(value: wlan_control_wire::Error) -> Self {
        Self::Wire(value)
    }
}

impl std::fmt::Display for EndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for EndpointError {}

pub struct ReceivedPacket {
    pub packet: Packet,
    pub fds: Vec<OwnedFd>,
}

struct OutboundPacket {
    bytes: Vec<u8>,
    fd: Option<OwnedFd>,
}

/// Validated connected AF_UNIX/SOCK_SEQPACKET endpoint.
///
/// Validation performs no receive and is suitable for pre-lockdown setup.
pub struct UnixSeqpacketEndpoint {
    fd: OwnedFd,
}

impl UnixSeqpacketEndpoint {
    pub fn from_inherited_fd(fd: OwnedFd) -> Result<Self, EndpointError> {
        let raw = fd.as_raw_fd();
        if socket_option(raw, libc::SOL_SOCKET, libc::SO_DOMAIN)? != libc::AF_UNIX {
            return Err(EndpointError::NotUnix);
        }
        if socket_option(raw, libc::SOL_SOCKET, libc::SO_TYPE)? != libc::SOCK_SEQPACKET {
            return Err(EndpointError::NotSeqpacket);
        }
        let mut address: libc::sockaddr_storage = unsafe { zeroed() };
        let mut length = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        if unsafe {
            libc::getpeername(
                raw,
                (&mut address as *mut libc::sockaddr_storage).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(EndpointError::NotConnected);
        }
        if address.ss_family as i32 != libc::AF_UNIX {
            return Err(EndpointError::NotConnected);
        }
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
            return Err(EndpointError::Io(io::Error::last_os_error()));
        }
        Ok(Self { fd })
    }

    pub fn try_receive_packet(&self) -> Result<Option<ReceivedPacket>, EndpointError> {
        self.try_receive()
    }

    pub fn try_send_packet(
        &self,
        packet: &Packet,
        fd: Option<&OwnedFd>,
    ) -> Result<bool, EndpointError> {
        let expected = required_fd_count(&packet.message);
        if expected != usize::from(fd.is_some()) {
            return Err(EndpointError::WrongFdCount {
                expected,
                actual: usize::from(fd.is_some()),
            });
        }
        self.try_send(&OutboundPacket {
            bytes: encode(packet)?,
            fd: fd.map(OwnedFd::try_clone).transpose()?,
        })
    }

    fn try_receive(&self) -> Result<Option<ReceivedPacket>, EndpointError> {
        let mut bytes = [0u8; MAX_PACKET];
        let mut control = [0usize; 8];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut header: libc::msghdr = unsafe { zeroed() };
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = size_of::<[usize; 8]>();
        let received = unsafe {
            libc::recvmsg(
                self.fd.as_raw_fd(),
                &mut header,
                libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
            )
        };
        if received < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(EndpointError::Io(error))
            };
        }
        if received == 0 {
            return Err(EndpointError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "control peer closed",
            )));
        }
        let mut fds = Vec::new();
        let mut invalid_ancillary = false;
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&header);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
                    invalid_ancillary = true;
                } else {
                    let header_len = libc::CMSG_LEN(0) as usize;
                    if (*cmsg).cmsg_len < header_len {
                        return Err(EndpointError::TruncatedAncillary);
                    }
                    let payload = (*cmsg).cmsg_len - header_len;
                    if !payload.is_multiple_of(size_of::<RawFd>()) {
                        return Err(EndpointError::TruncatedAncillary);
                    }
                    let count = payload / size_of::<RawFd>();
                    let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                    for index in 0..count {
                        // SAFETY: SCM_RIGHTS installs each received descriptor
                        // into this process. Owning every one before reporting
                        // any ancillary error ensures all error paths close it.
                        fds.push(OwnedFd::from_raw_fd(*data.add(index)));
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&header, cmsg);
            }
        }
        if header.msg_flags & libc::MSG_TRUNC != 0 {
            return Err(EndpointError::TruncatedPacket);
        }
        if header.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(EndpointError::TruncatedAncillary);
        }
        if invalid_ancillary {
            return Err(EndpointError::WrongFdCount {
                expected: 0,
                actual: usize::MAX,
            });
        }
        let packet = decode(&bytes[..received as usize])?;
        let expected = required_fd_count(&packet.message);
        if fds.len() != expected {
            return Err(EndpointError::WrongFdCount {
                expected,
                actual: fds.len(),
            });
        }
        Ok(Some(ReceivedPacket { packet, fds }))
    }

    fn try_send(&self, packet: &OutboundPacket) -> Result<bool, EndpointError> {
        let mut iov = libc::iovec {
            iov_base: packet.bytes.as_ptr().cast_mut().cast(),
            iov_len: packet.bytes.len(),
        };
        let mut control = [0usize; 4];
        let mut header: libc::msghdr = unsafe { zeroed() };
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        if let Some(fd) = &packet.fd {
            header.msg_control = control.as_mut_ptr().cast();
            header.msg_controllen = unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as u32) } as usize;
            unsafe {
                let cmsg = libc::CMSG_FIRSTHDR(&header);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as usize;
                *libc::CMSG_DATA(cmsg).cast::<RawFd>() = fd.as_raw_fd();
            }
        }
        let sent = unsafe {
            libc::sendmsg(
                self.fd.as_raw_fd(),
                &header,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if sent < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::WouldBlock {
                Ok(false)
            } else {
                Err(error.into())
            };
        }
        if sent as usize != packet.bytes.len() {
            return Err(EndpointError::Io(io::Error::new(
                io::ErrorKind::WriteZero,
                "partial seqpacket send",
            )));
        }
        Ok(true)
    }
}

fn socket_option(fd: RawFd, level: i32, name: i32) -> io::Result<i32> {
    let mut value = 0i32;
    let mut length = size_of::<i32>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            level,
            name,
            (&mut value as *mut i32).cast(),
            &mut length,
        )
    } != 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

#[derive(Debug)]
pub enum ServiceError {
    Endpoint(EndpointError),
    Runtime(RuntimeError),
}
impl From<EndpointError> for ServiceError {
    fn from(v: EndpointError) -> Self {
        Self::Endpoint(v)
    }
}
impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ServiceError {}

/// Setup-phase service. This type has no method capable of receiving bytes.
pub struct PreparedServer<R> {
    policy_endpoint: UnixSeqpacketEndpoint,
    supervisor_endpoint: UnixSeqpacketEndpoint,
    runtime: R,
    generation: [u8; 16],
}

impl<R: WifiRuntime> PreparedServer<R> {
    pub fn new(
        policy_fd: OwnedFd,
        supervisor_fd: OwnedFd,
        generation: [u8; 16],
        runtime: R,
    ) -> Result<Self, EndpointError> {
        Ok(Self {
            policy_endpoint: UnixSeqpacketEndpoint::from_inherited_fd(policy_fd)?,
            supervisor_endpoint: UnixSeqpacketEndpoint::from_inherited_fd(supervisor_fd)?,
            runtime,
            generation,
        })
    }

    /// Caller-controlled lifecycle gate. Call only after self-lockdown is open.
    pub fn post_lockdown_open_complete(self) -> Result<ControlServer<R>, ServiceError> {
        let mut server = ControlServer {
            policy_endpoint: self.policy_endpoint,
            supervisor_endpoint: self.supervisor_endpoint,
            runtime: self.runtime,
            validator: SessionValidator::new(self.generation),
            generation: self.generation,
            next_request_id: 1,
            outbound: VecDeque::new(),
            outbound_bytes: 0,
            supervisor_outbound: VecDeque::new(),
            supervisor_outbound_bytes: 0,
            connect_request: None,
            scan_request: None,
            ethernet_generation: 0,
            terminal: false,
        };
        server
            .queue(Message::Ready, None)
            .map_err(terminal_service)?;
        Ok(server)
    }
}

pub struct ControlServer<R> {
    policy_endpoint: UnixSeqpacketEndpoint,
    supervisor_endpoint: UnixSeqpacketEndpoint,
    runtime: R,
    validator: SessionValidator,
    generation: [u8; 16],
    next_request_id: u64,
    outbound: VecDeque<OutboundPacket>,
    outbound_bytes: usize,
    supervisor_outbound: VecDeque<OutboundPacket>,
    supervisor_outbound_bytes: usize,
    connect_request: Option<u64>,
    scan_request: Option<u64>,
    ethernet_generation: u64,
    terminal: bool,
}

impl<R: WifiRuntime> ControlServer<R> {
    pub fn is_terminal(&self) -> bool {
        self.terminal && self.outbound.is_empty() && self.supervisor_outbound.is_empty()
    }

    pub async fn drive_once(&mut self) -> Result<bool, ServiceError> {
        let mut progressed = self.flush()?;
        if !self.terminal {
            match self.policy_endpoint.try_receive() {
                Ok(Some(received)) => {
                    progressed = true;
                    if self.validator.validate(&received.packet).is_err()
                        || !received.fds.is_empty()
                    {
                        self.end_generation(GenerationEndReason::ProtocolViolation)?;
                    } else if let Err(reason) = self.dispatch(received.packet).await {
                        self.end_generation(reason)?;
                    }
                }
                Ok(None) => {}
                Err(EndpointError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    self.terminal = true;
                }
                Err(_) => self.end_generation(GenerationEndReason::ProtocolViolation)?,
            }
        }
        if !self.terminal {
            progressed |= self.drive_runtime().await?;
        }
        progressed |= self.flush()?;
        Ok(progressed)
    }

    pub fn run(mut self) -> Result<(), ServiceError> {
        while !self.is_terminal() {
            if !futures::executor::block_on(self.drive_once())? {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(())
    }

    async fn dispatch(&mut self, packet: Packet) -> Result<(), GenerationEndReason> {
        let id = packet.request_id;
        match packet.message {
            Message::Scan(request) => {
                if self.scan_request.is_some() {
                    self.queue_scan_reply(id, Err(sme::ScanErrorCode::ShouldWait))?;
                } else {
                    match self
                        .runtime
                        .begin_scan(request, Instant::now() + Duration::from_secs(30))
                    {
                        Ok(()) => self.scan_request = Some(id),
                        Err(error) => return Err(runtime_end(error)),
                    }
                }
            }
            Message::Connect(request) => {
                if self.connect_request.is_some() {
                    return Err(GenerationEndReason::ProtocolViolation);
                } else {
                    match self
                        .runtime
                        .begin_connect(request, Instant::now() + Duration::from_secs(30))
                    {
                        Ok(()) => self.connect_request = Some(id),
                        Err(error) => return Err(runtime_end(error)),
                    }
                }
            }
            Message::Disconnect(reason) => {
                let result = if let Some(connect_id) = self.connect_request.take() {
                    match self
                        .runtime
                        .cancel_connect(reason, Instant::now() + Duration::from_secs(10))
                        .await
                    {
                        Ok(result) => {
                            self.queue_connect_result(connect_id, result)?;
                            CommandReply::Success
                        }
                        Err(error) => return Err(runtime_end(error)),
                    }
                } else {
                    match self
                        .runtime
                        .disconnect(reason, Instant::now() + Duration::from_secs(10))
                        .await
                    {
                        Ok(()) => CommandReply::Success,
                        Err(error) => return Err(runtime_end(error)),
                    }
                };
                self.queue(
                    Message::DisconnectReply(Reply {
                        in_reply_to: id,
                        result,
                    }),
                    None,
                )?;
            }
            Message::Roam(request) => {
                let reply = match self.runtime.roam(request) {
                    Ok(()) => CommandReply::Success,
                    Err(error) => return Err(runtime_end(error)),
                };
                self.queue(
                    Message::RoamReply(Reply {
                        in_reply_to: id,
                        result: reply,
                    }),
                    None,
                )?;
            }
            _ => return Err(GenerationEndReason::ProtocolViolation),
        }
        Ok(())
    }

    async fn drive_runtime(&mut self) -> Result<bool, ServiceError> {
        let mut progressed = false;
        if let Some(id) = self.connect_request {
            match self.runtime.drive_connect_once().await {
                Ok(Some(result)) => {
                    self.connect_request = None;
                    if let Err(reason) = self.queue_connect_result(id, result) {
                        self.end_generation(reason)?;
                        return Ok(true);
                    }
                    if let Err(reason) = self.queue(
                        Message::Event(sme::ConnectTransactionEvent::OnConnectResult { result }),
                        None,
                    ) {
                        self.end_generation(reason)?;
                        return Ok(true);
                    }
                    progressed = true;
                }
                Ok(None) => {}
                Err(RuntimeError::Failed(result)) => {
                    self.connect_request = None;
                    match self.runtime.next_connection_event() {
                        Ok(None) => {}
                        Ok(Some(_)) => {
                            self.end_generation(GenerationEndReason::DriverFault)?;
                            return Ok(true);
                        }
                        Err(error) => {
                            self.end_generation(runtime_end(error))?;
                            return Ok(true);
                        }
                    }
                    if let Err(reason) = self.queue_connect_result(id, result) {
                        self.end_generation(reason)?;
                        return Ok(true);
                    }
                    progressed = true;
                }
                Err(error) => {
                    self.end_generation(runtime_end(error))?;
                    return Ok(true);
                }
            }
        }
        if let Some(id) = self.scan_request {
            match self.runtime.drive_scan_once().await {
                Ok(Some(result)) => {
                    self.scan_request = None;
                    if let Err(reason) = self.queue_scan_reply(id, result) {
                        self.end_generation(reason)?;
                        return Ok(true);
                    }
                    progressed = true;
                }
                Ok(None) => {}
                Err(error) => {
                    self.end_generation(runtime_end(error))?;
                    return Ok(true);
                }
            }
        }
        match self.runtime.drive_once().await {
            Ok(value) => progressed |= value,
            Err(error) => {
                self.end_generation(runtime_end(error))?;
                return Ok(true);
            }
        }
        loop {
            let event = match self.runtime.next_connection_event() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    self.end_generation(runtime_end(error))?;
                    return Ok(true);
                }
            };
            if let Err(reason) = self.queue(Message::Event(event), None) {
                self.end_generation(reason)?;
                return Ok(true);
            }
            progressed = true;
        }
        if let Some(fd) = self.runtime.take_ethernet_device() {
            let Some(generation) = self.ethernet_generation.checked_add(1) else {
                self.end_generation(GenerationEndReason::ContainmentFault)?;
                return Ok(true);
            };
            self.ethernet_generation = generation;
            if let Err(reason) = self.queue_supervisor_install(
                self.ethernet_generation,
                self.runtime.public_mac(),
                fd,
            ) {
                self.end_generation(reason)?;
                return Ok(true);
            }
            progressed = true;
        }
        Ok(progressed)
    }

    fn queue_connect_result(
        &mut self,
        id: u64,
        result: sme::ConnectResult,
    ) -> Result<(), GenerationEndReason> {
        self.queue(
            Message::ConnectReply(Reply {
                in_reply_to: id,
                result: ConnectReply::Completed(result),
            }),
            None,
        )
    }
    fn queue_scan_reply(
        &mut self,
        id: u64,
        result: Result<Vec<sme::ScanResult>, sme::ScanErrorCode>,
    ) -> Result<(), GenerationEndReason> {
        self.queue(
            Message::ScanReply(Reply {
                in_reply_to: id,
                result,
            }),
            None,
        )
    }
    fn queue(&mut self, message: Message, fd: Option<OwnedFd>) -> Result<(), GenerationEndReason> {
        if fd.is_some() || required_fd_count(&message) != 0 {
            return Err(GenerationEndReason::ProtocolViolation);
        }
        if required_fd_count(&message) != usize::from(fd.is_some()) {
            return Err(GenerationEndReason::ContainmentFault);
        }
        let packet = Packet {
            generation: self.generation,
            request_id: self.next_request_id,
            message,
        };
        let bytes = encode(&packet).map_err(|_| GenerationEndReason::ProtocolViolation)?;
        if self.outbound.len() >= NORMAL_PACKET_LIMIT
            || self.outbound_bytes + bytes.len() > NORMAL_BYTE_LIMIT
        {
            return Err(GenerationEndReason::Backpressure);
        }
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(GenerationEndReason::ProtocolViolation)?;
        self.outbound_bytes += bytes.len();
        self.outbound.push_back(OutboundPacket { bytes, fd });
        Ok(())
    }
    fn queue_supervisor_install(
        &mut self,
        ethernet_generation: u64,
        mac: [u8; 6],
        fd: OwnedFd,
    ) -> Result<(), GenerationEndReason> {
        let bytes = LifecycleMessage {
            kind: LifecycleKind::Install,
            wifi_generation: self.generation,
            ethernet_generation,
            mac_address: mac,
        }
        .encode()
        .to_vec();
        if self.supervisor_outbound.len() >= NORMAL_PACKET_LIMIT
            || self.supervisor_outbound_bytes + bytes.len() > NORMAL_BYTE_LIMIT
        {
            return Err(GenerationEndReason::Backpressure);
        }
        self.supervisor_outbound_bytes += bytes.len();
        self.supervisor_outbound.push_back(OutboundPacket {
            bytes,
            fd: Some(fd),
        });
        Ok(())
    }

    fn end_generation(&mut self, reason: GenerationEndReason) -> Result<(), ServiceError> {
        if self.terminal {
            return Ok(());
        }
        let packet = Packet {
            generation: self.generation,
            request_id: self.next_request_id,
            message: Message::GenerationEnd(reason),
        };
        let bytes =
            encode(&packet).map_err(|error| ServiceError::Endpoint(EndpointError::Wire(error)))?;
        self.next_request_id += 1;
        self.outbound_bytes += bytes.len();
        self.outbound.push_back(OutboundPacket { bytes, fd: None });
        debug_assert!(self.outbound.len() <= MAX_OUTBOUND_PACKETS);

        debug_assert!(self.outbound_bytes <= MAX_OUTBOUND_BYTES);
        self.terminal = true;
        Ok(())
    }
    fn flush(&mut self) -> Result<bool, ServiceError> {
        let mut progressed = false;
        while let Some(packet) = self.outbound.front() {
            if !self.policy_endpoint.try_send(packet)? {
                break;
            }
            let packet = self.outbound.pop_front().unwrap();
            self.outbound_bytes -= packet.bytes.len();
            progressed = true;
        }
        while let Some(packet) = self.supervisor_outbound.front() {
            if !self.supervisor_endpoint.try_send(packet)? {
                break;
            }
            let packet = self.supervisor_outbound.pop_front().unwrap();
            self.supervisor_outbound_bytes -= packet.bytes.len();
            progressed = true;
        }
        Ok(progressed)
    }
}

fn runtime_end(error: RuntimeError) -> GenerationEndReason {
    match error {
        RuntimeError::Failed(_) | RuntimeError::DriverFault => GenerationEndReason::DriverFault,
        RuntimeError::Timeout => GenerationEndReason::Timeout,
        RuntimeError::ContainmentFault => GenerationEndReason::ContainmentFault,
    }
}
fn terminal_service(reason: GenerationEndReason) -> ServiceError {
    ServiceError::Runtime(match reason {
        GenerationEndReason::Timeout => RuntimeError::Timeout,
        GenerationEndReason::ContainmentFault => RuntimeError::ContainmentFault,
        _ => RuntimeError::DriverFault,
    })
}

/// Deterministic hardware-independent runtime for service and peer testing.
pub struct SimulatedWifiRuntime {
    mac: [u8; 6],
    connect: bool,
    scan: bool,
    events: VecDeque<sme::ConnectTransactionEvent>,
    ethernet: Option<OwnedFd>,
    ethernet_after_connect: Option<OwnedFd>,
    connect_mode: SimulatedConnectMode,
}

#[derive(Clone, Copy)]
enum SimulatedConnectMode {
    None,
    Success,
    RetrySafeFailure,
    StaleFailure,
    Timeout,
    DriverFault,
    ContainmentFault,
    Hold,
}

impl SimulatedWifiRuntime {
    pub fn new(mac: [u8; 6]) -> Self {
        Self {
            mac,
            connect: false,
            scan: false,
            events: VecDeque::new(),
            ethernet: None,
            ethernet_after_connect: None,
            connect_mode: SimulatedConnectMode::None,
        }
    }
    pub fn publish_ethernet(&mut self, fd: OwnedFd) {
        self.ethernet = Some(fd);
    }
    pub fn publish_ethernet_after_connect(&mut self, fd: OwnedFd) {
        self.ethernet_after_connect = Some(fd);
    }
    pub fn inject_connection_event(&mut self, event: sme::ConnectTransactionEvent) {
        self.events.push_back(event);
    }
}

impl WifiRuntime for SimulatedWifiRuntime {
    fn public_mac(&self) -> [u8; 6] {
        self.mac
    }
    fn take_ethernet_device(&mut self) -> Option<OwnedFd> {
        self.ethernet.take()
    }
    fn begin_connect(
        &mut self,
        request: sme::ConnectRequest,
        _: Instant,
    ) -> Result<(), RuntimeError> {
        self.connect = true;
        self.connect_mode = match request.ssid.as_slice() {
            b"fail" => SimulatedConnectMode::RetrySafeFailure,
            b"stale" => SimulatedConnectMode::StaleFailure,
            b"timeout" => SimulatedConnectMode::Timeout,
            b"driver" => SimulatedConnectMode::DriverFault,
            b"containment" => SimulatedConnectMode::ContainmentFault,
            b"hold" => SimulatedConnectMode::Hold,
            _ => SimulatedConnectMode::Success,
        };
        Ok(())
    }
    async fn drive_connect_once(&mut self) -> Result<Option<sme::ConnectResult>, RuntimeError> {
        if !self.connect {
            return Ok(None);
        }
        if matches!(self.connect_mode, SimulatedConnectMode::Hold) {
            return Ok(None);
        }
        self.connect = false;
        match self.connect_mode {
            SimulatedConnectMode::Timeout => return Err(RuntimeError::Timeout),
            SimulatedConnectMode::DriverFault => return Err(RuntimeError::DriverFault),
            SimulatedConnectMode::ContainmentFault => return Err(RuntimeError::ContainmentFault),
            _ => {}
        }
        if matches!(
            self.connect_mode,
            SimulatedConnectMode::RetrySafeFailure | SimulatedConnectMode::StaleFailure
        ) {
            let stale = matches!(self.connect_mode, SimulatedConnectMode::StaleFailure);
            self.connect_mode = SimulatedConnectMode::None;
            if stale {
                self.events
                    .push_back(sme::ConnectTransactionEvent::OnConnectResult {
                        result: sme::ConnectResult {
                            code: fidl_fuchsia_wlan_ieee80211::StatusCode::Success,
                            is_credential_rejected: false,
                            is_reconnect: false,
                        },
                    });
            }
            return Err(RuntimeError::Failed(sme::ConnectResult {
                code: fidl_fuchsia_wlan_ieee80211::StatusCode::RefusedReasonUnspecified,
                is_credential_rejected: true,
                is_reconnect: false,
            }));
        }
        self.connect_mode = SimulatedConnectMode::None;
        let result = sme::ConnectResult {
            code: fidl_fuchsia_wlan_ieee80211::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        };
        self.ethernet = self.ethernet_after_connect.take();
        Ok(Some(result))
    }
    async fn cancel_connect(
        &mut self,
        _: sme::UserDisconnectReason,
        _: Instant,
    ) -> Result<sme::ConnectResult, RuntimeError> {
        self.connect = false;
        self.connect_mode = SimulatedConnectMode::None;
        Ok(sme::ConnectResult {
            code: fidl_fuchsia_wlan_ieee80211::StatusCode::Canceled,
            is_credential_rejected: false,
            is_reconnect: false,
        })
    }
    fn roam(&mut self, _: sme::RoamRequest) -> Result<(), RuntimeError> {
        Ok(())
    }
    fn begin_scan(&mut self, _: sme::ScanRequest, _: Instant) -> Result<(), RuntimeError> {
        self.scan = true;
        Ok(())
    }
    async fn drive_scan_once(
        &mut self,
    ) -> Result<Option<Result<Vec<sme::ScanResult>, sme::ScanErrorCode>>, RuntimeError> {
        if std::mem::take(&mut self.scan) {
            Ok(Some(Ok(Vec::new())))
        } else {
            Ok(None)
        }
    }
    async fn drive_once(&mut self) -> Result<bool, RuntimeError> {
        Ok(false)
    }
    fn next_connection_event(
        &mut self,
    ) -> Result<Option<sme::ConnectTransactionEvent>, RuntimeError> {
        Ok(self.events.pop_front())
    }
    async fn disconnect(
        &mut self,
        _: sme::UserDisconnectReason,
        _: Instant,
    ) -> Result<(), RuntimeError> {
        Ok(())
    }
}
