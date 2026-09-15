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
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};
use wifi_supervisor_wire::{LifecycleKind, LifecycleMessage};
use wlan_control_wire::{
    CommandReply, ConnectReply, GenerationEndReason, MAX_PACKET, Message, Packet, Reply,
    SessionValidator, decode, encode, required_fd_count,
};

pub use wlan_softmac_host::runtime::DisconnectOutcome;

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
    Unsupported,
    Busy,
    NotConnected,
    DriverFault,
    ContainmentFault,
}

/// Narrow service seam matching the retained production client operations.
///
/// [`wlan_softmac_host::runtime::ClientRuntime`] implements this seam directly;
/// chip-specific construction and all SME policy remain outside this service.
/// The Ethernet method only converts the runtime's one-shot generation into
/// the descriptor transferred to the network supervisor.
pub trait WifiRuntime {
    fn public_mac(&self) -> [u8; 6];
    fn take_ethernet_device(&mut self) -> Option<OwnedFd>;
    async fn begin_connect(
        &mut self,
        request: sme::ConnectRequest,
        deadline: Instant,
    ) -> Result<(), RuntimeError>;
    async fn drive_connect_once(&mut self) -> Result<Option<sme::ConnectResult>, RuntimeError>;
    fn roam(&mut self, request: sme::RoamRequest) -> Result<(), RuntimeError>;
    async fn begin_scan(
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
    fn begin_disconnect(
        &mut self,
        reason: sme::UserDisconnectReason,
        deadline: Instant,
    ) -> Result<(), RuntimeError>;
    async fn drive_disconnect_once(&mut self) -> Result<Option<DisconnectOutcome>, RuntimeError>;
    fn begin_power_save(
        &mut self,
        _mode: wlan_control_wire::PowerSaveMode,
        _deadline: Instant,
    ) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unsupported)
    }
    async fn drive_power_save_once(&mut self) -> Result<Option<()>, RuntimeError> {
        Err(RuntimeError::Unsupported)
    }
}

fn host_runtime_error(error: wlan_softmac_host::runtime::ConnectError) -> RuntimeError {
    match error {
        wlan_softmac_host::runtime::ConnectError::Failed(result) => RuntimeError::Failed(result),
        wlan_softmac_host::runtime::ConnectError::Timeout => RuntimeError::Timeout,
        wlan_softmac_host::runtime::ConnectError::Driver(
            wlan_softmac_host::runtime::DriverError::RoamUnsupported,
        ) => RuntimeError::Unsupported,
        wlan_softmac_host::runtime::ConnectError::Driver(_) => RuntimeError::DriverFault,
        wlan_softmac_host::runtime::ConnectError::Containment => RuntimeError::ContainmentFault,
    }
}

impl<D> WifiRuntime for wlan_softmac_host::runtime::ClientRuntime<D>
where
    D: wlan_softmac_host::WlanSoftmac
        + wlan_softmac_host::WlanSoftmacLifecycle
        + wlan_softmac_host::ClientRuntimeDriver,
{
    fn public_mac(&self) -> [u8; 6] {
        self.public_mac()
    }

    fn take_ethernet_device(&mut self) -> Option<OwnedFd> {
        self.take_ethernet_device()
            .map(|device| device.into_frame_fd())
    }

    async fn begin_connect(
        &mut self,
        request: sme::ConnectRequest,
        deadline: Instant,
    ) -> Result<(), RuntimeError> {
        self.begin_connect(request, deadline)
            .await
            .map_err(host_runtime_error)
    }

    async fn drive_connect_once(&mut self) -> Result<Option<sme::ConnectResult>, RuntimeError> {
        self.drive_connect_once().await.map_err(host_runtime_error)
    }

    fn roam(&mut self, request: sme::RoamRequest) -> Result<(), RuntimeError> {
        self.roam(request).map_err(host_runtime_error)
    }

    async fn begin_scan(
        &mut self,
        request: sme::ScanRequest,
        deadline: Instant,
    ) -> Result<(), RuntimeError> {
        self.begin_scan(request, deadline)
            .await
            .map_err(host_runtime_error)
    }

    async fn drive_scan_once(
        &mut self,
    ) -> Result<Option<Result<Vec<sme::ScanResult>, sme::ScanErrorCode>>, RuntimeError> {
        self.drive_scan_once().await.map_err(host_runtime_error)
    }

    async fn drive_once(&mut self) -> Result<bool, RuntimeError> {
        self.drive_service_once().await.map_err(host_runtime_error)
    }

    fn next_connection_event(
        &mut self,
    ) -> Result<Option<sme::ConnectTransactionEvent>, RuntimeError> {
        self.next_connection_event().map_err(host_runtime_error)
    }

    fn begin_disconnect(
        &mut self,
        reason: sme::UserDisconnectReason,
        deadline: Instant,
    ) -> Result<(), RuntimeError> {
        self.begin_disconnect(reason, deadline)
            .map_err(host_runtime_error)
    }
    async fn drive_disconnect_once(&mut self) -> Result<Option<DisconnectOutcome>, RuntimeError> {
        self.drive_disconnect_once()
            .await
            .map_err(host_runtime_error)
    }

    fn begin_power_save(
        &mut self,
        mode: wlan_control_wire::PowerSaveMode,
        deadline: Instant,
    ) -> Result<(), RuntimeError> {
        self.begin_power_save(
            matches!(mode, wlan_control_wire::PowerSaveMode::Balanced),
            deadline,
        )
        .map_err(power_runtime_error)
    }

    async fn drive_power_save_once(&mut self) -> Result<Option<()>, RuntimeError> {
        self.drive_power_save_once()
            .await
            .map_err(power_runtime_error)
    }
}

fn power_runtime_error(status: zx::Status) -> RuntimeError {
    match status {
        zx::Status::SHOULD_WAIT => RuntimeError::Busy,
        zx::Status::BAD_STATE => RuntimeError::NotConnected,
        zx::Status::NOT_SUPPORTED => RuntimeError::Unsupported,
        zx::Status::TIMED_OUT => RuntimeError::Timeout,
        _ => RuntimeError::DriverFault,
    }
}

#[derive(Debug)]
pub enum EndpointError {
    Io(io::Error),
    NotUnix,
    NotSeqpacket,
    NotConnected,
    AliasedEndpoints,
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
    drain_deadline: Instant,
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

    /// Returns the validated inherited capability without reading from it.
    pub fn into_owned_fd(self) -> OwnedFd {
        self.fd
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
            drain_deadline: Instant::now() + Duration::from_secs(2),
            bytes: encode(packet)?,
            fd: fd.map(OwnedFd::try_clone).transpose()?,
        })
    }

    fn try_receive(&self) -> Result<Option<ReceivedPacket>, EndpointError> {
        let mut bytes = [0u8; MAX_PACKET];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut header: libc::msghdr = unsafe { zeroed() };
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        // Policy messages never carry capabilities. Leaving ancillary storage
        // null makes the kernel discard SCM_RIGHTS instead of installing an
        // attacker-supplied descriptor in this locked process.
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
        if header.msg_flags & libc::MSG_TRUNC != 0 {
            return Err(EndpointError::TruncatedPacket);
        }
        if header.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(EndpointError::TruncatedAncillary);
        }
        let packet = decode(&bytes[..received as usize])?;
        let expected = required_fd_count(&packet.message);
        if expected != 0 {
            return Err(EndpointError::WrongFdCount {
                expected,
                actual: 0,
            });
        }
        Ok(Some(ReceivedPacket {
            packet,
            fds: Vec::new(),
        }))
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
            header.msg_controllen = unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as u32) } as _;
            unsafe {
                let cmsg = libc::CMSG_FIRSTHDR(&header);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as _;
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

/// Validated inert IPC endpoints retained across lockdown before a physical
/// runtime exists. Construction never receives a policy packet.
pub struct PreparedServerEndpoints {
    policy_endpoint: UnixSeqpacketEndpoint,
    supervisor_endpoint: UnixSeqpacketEndpoint,
    generation: [u8; 16],
}

impl PreparedServerEndpoints {
    pub fn new(
        policy_fd: OwnedFd,
        supervisor_fd: OwnedFd,
        generation: [u8; 16],
    ) -> Result<Self, EndpointError> {
        let policy_endpoint = UnixSeqpacketEndpoint::from_inherited_fd(policy_fd)?;
        let supervisor_endpoint = UnixSeqpacketEndpoint::from_inherited_fd(supervisor_fd)?;
        if fd_identity(policy_endpoint.fd.as_raw_fd())?
            == fd_identity(supervisor_endpoint.fd.as_raw_fd())?
        {
            return Err(EndpointError::AliasedEndpoints);
        }
        Ok(Self {
            policy_endpoint,
            supervisor_endpoint,
            generation,
        })
    }

    pub fn fd_identities(&self) -> [RawFd; 2] {
        [
            self.policy_endpoint.fd.as_raw_fd(),
            self.supervisor_endpoint.fd.as_raw_fd(),
        ]
    }

    /// Bind a post-lockdown physical runtime without further endpoint syscalls.
    pub fn bind_runtime<R>(self, runtime: R) -> PreparedServer<R> {
        PreparedServer {
            policy_endpoint: self.policy_endpoint,
            supervisor_endpoint: self.supervisor_endpoint,
            runtime,
            generation: self.generation,
        }
    }
}

fn fd_identity(fd: RawFd) -> io::Result<(libc::dev_t, libc::ino_t)> {
    let mut stat: libc::stat = unsafe { zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok((stat.st_dev, stat.st_ino))
    }
}

impl<R: WifiRuntime> PreparedServer<R> {
    pub fn new(
        policy_fd: OwnedFd,
        supervisor_fd: OwnedFd,
        generation: [u8; 16],
        runtime: R,
    ) -> Result<Self, EndpointError> {
        Ok(
            PreparedServerEndpoints::new(policy_fd, supervisor_fd, generation)?
                .bind_runtime(runtime),
        )
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
            disconnect_request: None,
            power_save_request: None,
            ethernet_generation: 0,
            terminal: false,
        };
        server
            .queue(Message::Ready, None)
            .map_err(terminal_service)?;
        Ok(server)
    }
}

struct PendingDisconnect {
    requests: Vec<(u64, Instant)>,
    connect_request: Option<u64>,
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
    disconnect_request: Option<PendingDisconnect>,
    power_save_request: Option<(u64, Instant)>,
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
                    eprintln!("wifi_control_lifecycle stage=policy_eof terminal=true");
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

    pub async fn run(mut self) -> Result<(), ServiceError> {
        self.run_to_terminal().await
    }

    /// Drive until the generation is terminal while retaining runtime
    /// ownership for hardware-specific orderly shutdown.
    pub async fn run_to_terminal(&mut self) -> Result<(), ServiceError> {
        while !self.is_terminal() {
            if !self.drive_once().await? {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            tokio::task::yield_now().await;
        }
        Ok(())
    }

    /// Recover the runtime after [`Self::run_to_terminal`] so a physical
    /// service can stop firmware before releasing device capabilities.
    pub fn into_runtime(self) -> R {
        self.runtime
    }

    async fn dispatch(&mut self, packet: Packet) -> Result<(), GenerationEndReason> {
        let id = packet.request_id;
        let admitted_deadline = if let Some(deadline) = packet.message.deadline() {
            let local_now = Instant::now();
            let now = wlan_control_wire::monotonic_time_ns()
                .map_err(|_| GenerationEndReason::DriverFault)?;
            let Some(remaining) = deadline.remaining(now) else {
                self.queue(
                    Message::DeadlineExceeded(Reply {
                        in_reply_to: id,
                        result: (),
                    }),
                    None,
                )?;
                return Ok(());
            };
            if remaining > Duration::from_secs(30) {
                return Err(GenerationEndReason::ProtocolViolation);
            }
            Some(local_now + remaining)
        } else {
            None
        };
        match packet.message {
            Message::Scan { request, .. } => {
                if self.scan_request.is_some()
                    || self.disconnect_request.is_some()
                    || self.power_save_request.is_some()
                {
                    self.queue_scan_reply(id, Err(sme::ScanErrorCode::ShouldWait))?;
                } else {
                    match self
                        .runtime
                        .begin_scan(
                            request,
                            admitted_deadline.expect("command deadline admitted"),
                        )
                        .await
                    {
                        Ok(()) => self.scan_request = Some(id),
                        Err(error) => return Err(runtime_end(error)),
                    }
                }
            }
            Message::Connect { request, .. } => {
                if self.connect_request.is_some()
                    || self.disconnect_request.is_some()
                    || self.power_save_request.is_some()
                {
                    return Err(GenerationEndReason::ProtocolViolation);
                } else {
                    match self
                        .runtime
                        .begin_connect(
                            request,
                            admitted_deadline.expect("command deadline admitted"),
                        )
                        .await
                    {
                        Ok(()) => self.connect_request = Some(id),
                        Err(error) => return Err(runtime_end(error)),
                    }
                }
            }
            Message::Disconnect { reason, .. } => {
                let deadline = admitted_deadline.expect("command deadline admitted");
                if self.power_save_request.is_some() {
                    return Err(GenerationEndReason::ProtocolViolation);
                }
                if let Some(pending) = &mut self.disconnect_request {
                    if pending.requests.len() == NORMAL_PACKET_LIMIT {
                        return Err(GenerationEndReason::Backpressure);
                    }
                    // Additional waiters do not reissue cleanup or renew its
                    // original deadline. All replies remain owned and bounded.
                    pending.requests.push((id, deadline));
                } else {
                    self.runtime
                        .begin_disconnect(reason, deadline)
                        .map_err(runtime_end)?;
                    self.disconnect_request = Some(PendingDisconnect {
                        requests: vec![(id, deadline)],
                        connect_request: self.connect_request.take(),
                    });
                }
            }
            Message::Roam { request, .. } => {
                let reply =
                    if self.disconnect_request.is_some() || self.power_save_request.is_some() {
                        CommandReply::Busy
                    } else {
                        match self.runtime.roam(request) {
                            Ok(()) => CommandReply::Success,
                            Err(RuntimeError::Unsupported) => CommandReply::Unsupported,
                            Err(error) => return Err(runtime_end(error)),
                        }
                    };
                self.queue(
                    Message::RoamReply(Reply {
                        in_reply_to: id,
                        result: reply,
                    }),
                    None,
                )?;
            }
            Message::SetPowerSave { mode, .. } => {
                let deadline = admitted_deadline.expect("command deadline admitted");
                let reply = if self.power_save_request.is_some()
                    || self.disconnect_request.is_some()
                    || self.connect_request.is_some()
                    || self.scan_request.is_some()
                {
                    CommandReply::Busy
                } else {
                    match self.runtime.begin_power_save(mode, deadline) {
                        Ok(()) => {
                            self.power_save_request = Some((id, deadline));
                            return Ok(());
                        }
                        Err(RuntimeError::Busy) => CommandReply::Busy,
                        Err(RuntimeError::NotConnected) => CommandReply::NotConnected,
                        Err(RuntimeError::Unsupported) => CommandReply::Unsupported,
                        Err(error) => return Err(runtime_end(error)),
                    }
                };
                self.queue(
                    Message::SetPowerSaveReply(Reply {
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

    fn queue_connection_events(&mut self) -> Result<bool, GenerationEndReason> {
        let mut progressed = false;
        while let Some(event) = self.runtime.next_connection_event().map_err(runtime_end)? {
            self.queue(Message::Event(event), None)?;
            progressed = true;
        }
        Ok(progressed)
    }

    async fn drive_runtime(&mut self) -> Result<bool, ServiceError> {
        let mut progressed = false;
        match self.queue_connection_events() {
            Ok(value) => progressed |= value,
            Err(reason) => {
                self.end_generation(reason)?;
                return Ok(true);
            }
        }
        if let Some((id, deadline)) = self.power_save_request {
            if Instant::now() >= deadline {
                self.end_generation(GenerationEndReason::Timeout)?;
                return Ok(true);
            }
            let result = match self.runtime.drive_power_save_once().await {
                Ok(Some(())) => CommandReply::Success,
                Ok(None) => return Ok(progressed),
                Err(RuntimeError::Busy) => CommandReply::Busy,
                Err(RuntimeError::Unsupported) => CommandReply::Unsupported,
                Err(RuntimeError::NotConnected) => CommandReply::NotConnected,
                Err(error) => {
                    self.end_generation(runtime_end(error))?;
                    return Ok(true);
                }
            };
            self.power_save_request = None;
            self.queue(
                Message::SetPowerSaveReply(Reply {
                    in_reply_to: id,
                    result,
                }),
                None,
            )
            .map_err(terminal_service)?;
            return Ok(true);
        }
        if let Some(pending) = &self.disconnect_request {
            if pending
                .requests
                .iter()
                .any(|(_, deadline)| Instant::now() >= *deadline)
            {
                self.end_generation(GenerationEndReason::Timeout)?;
                return Ok(true);
            }
            let outcome = match self.runtime.drive_disconnect_once().await {
                Ok(Some(outcome)) => outcome,
                Ok(None) => return Ok(progressed),
                Err(error) => {
                    self.end_generation(runtime_end(error))?;
                    return Ok(true);
                }
            };
            let pending = self.disconnect_request.take().expect("pending disconnect");
            let result = (|| {
                match (pending.connect_request, outcome) {
                    (Some(id), DisconnectOutcome::ConnectCanceled(result)) => {
                        self.queue_connect_result(id, result)?;
                    }
                    (None, DisconnectOutcome::Disconnected) => {}
                    _ => return Err(GenerationEndReason::DriverFault),
                }
                self.queue_connection_events()?;
                for (id, _) in pending.requests {
                    self.queue(
                        Message::DisconnectReply(Reply {
                            in_reply_to: id,
                            result: CommandReply::Success,
                        }),
                        None,
                    )?;
                }
                Ok(())
            })();
            if let Err(reason) = result {
                self.end_generation(reason)?;
            }
            return Ok(true);
        }
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
                    eprintln!(
                        "wifi_control_scan stage=completed request_id={id} result_count={}",
                        result.as_ref().map_or(0, Vec::len)
                    );
                    if let Err(reason) = self.queue_scan_reply(id, result) {
                        self.end_generation(reason)?;
                        return Ok(true);
                    }
                    eprintln!("wifi_control_scan stage=reply_queued request_id={id}");
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
        self.outbound.push_back(OutboundPacket {
            bytes,
            fd,
            drain_deadline: Instant::now() + Duration::from_secs(2),
        });
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
            drain_deadline: Instant::now() + Duration::from_secs(2),
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
        self.outbound.push_back(OutboundPacket {
            bytes,
            fd: None,
            drain_deadline: Instant::now() + Duration::from_secs(2),
        });
        debug_assert!(self.outbound.len() <= MAX_OUTBOUND_PACKETS);

        debug_assert!(self.outbound_bytes <= MAX_OUTBOUND_BYTES);
        self.terminal = true;
        Ok(())
    }
    fn flush(&mut self) -> Result<bool, ServiceError> {
        let mut progressed = false;
        // Per-packet absolute bounds also cover terminal messages and retained
        // Ethernet descriptors. A stalled consumer cannot hold this owner
        // forever. Returning an error preserves runtime ownership for cleanup.
        if self
            .outbound
            .front()
            .into_iter()
            .chain(self.supervisor_outbound.front())
            .any(|packet| Instant::now() >= packet.drain_deadline)
        {
            self.terminal = true;
            return Err(ServiceError::Runtime(RuntimeError::Timeout));
        }
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
        RuntimeError::Unsupported | RuntimeError::Busy | RuntimeError::NotConnected => {
            GenerationEndReason::ProtocolViolation
        }
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
    scan_result: Result<Vec<sme::ScanResult>, sme::ScanErrorCode>,
    events: VecDeque<sme::ConnectTransactionEvent>,
    ethernet: Option<OwnedFd>,
    ethernet_after_connect: Option<OwnedFd>,
    connect_mode: SimulatedConnectMode,
    disconnect: Option<DisconnectOutcome>,
    power_save: Option<Result<(), RuntimeError>>,
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
            scan_result: Ok(Vec::new()),
            events: VecDeque::new(),
            ethernet: None,
            ethernet_after_connect: None,
            connect_mode: SimulatedConnectMode::None,
            disconnect: None,
            power_save: None,
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
    async fn begin_connect(
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
    fn roam(&mut self, _: sme::RoamRequest) -> Result<(), RuntimeError> {
        Ok(())
    }
    async fn begin_scan(
        &mut self,
        request: sme::ScanRequest,
        _: Instant,
    ) -> Result<(), RuntimeError> {
        self.scan = true;
        self.scan_result = match request {
            sme::ScanRequest::Passive(request) if request.channels == [149] => {
                Err(sme::ScanErrorCode::NotSupported)
            }
            _ => Ok(Vec::new()),
        };
        Ok(())
    }
    async fn drive_scan_once(
        &mut self,
    ) -> Result<Option<Result<Vec<sme::ScanResult>, sme::ScanErrorCode>>, RuntimeError> {
        if std::mem::take(&mut self.scan) {
            Ok(Some(std::mem::replace(
                &mut self.scan_result,
                Ok(Vec::new()),
            )))
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
    fn begin_disconnect(
        &mut self,
        _: sme::UserDisconnectReason,
        _: Instant,
    ) -> Result<(), RuntimeError> {
        self.disconnect = Some(if std::mem::take(&mut self.connect) {
            self.connect_mode = SimulatedConnectMode::None;
            DisconnectOutcome::ConnectCanceled(sme::ConnectResult {
                code: fidl_fuchsia_wlan_ieee80211::StatusCode::Canceled,
                is_credential_rejected: false,
                is_reconnect: false,
            })
        } else {
            DisconnectOutcome::Disconnected
        });
        Ok(())
    }
    async fn drive_disconnect_once(&mut self) -> Result<Option<DisconnectOutcome>, RuntimeError> {
        Ok(self.disconnect.take())
    }
    fn begin_power_save(
        &mut self,
        _mode: wlan_control_wire::PowerSaveMode,
        _: Instant,
    ) -> Result<(), RuntimeError> {
        if self.power_save.is_some() {
            return Err(RuntimeError::Busy);
        }
        self.power_save = Some(Ok(()));
        Ok(())
    }
    async fn drive_power_save_once(&mut self) -> Result<Option<()>, RuntimeError> {
        self.power_save.take().transpose()
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use std::os::fd::FromRawFd;

    fn pair() -> (OwnedFd, OwnedFd) {
        let mut fds = [-1; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                    0,
                    fds.as_mut_ptr(),
                )
            },
            0
        );
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    #[test]
    fn held_cleanup_keeps_mailbox_live_and_preserves_all_terminal_replies() {
        for expire in [false, true] {
            let executor = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&executor, async {
                let (policy, policy_peer) = pair();
                let peer = UnixSeqpacketEndpoint::from_inherited_fd(policy_peer).unwrap();
                let (supervisor, _supervisor_peer) = pair();
                let mut runtime = SimulatedWifiRuntime::new([2; 6]);
                runtime.connect = true;
                runtime.connect_mode = SimulatedConnectMode::Hold;
                let mut server = PreparedServer::new(policy, supervisor, [1; 16], runtime)
                    .unwrap().post_lockdown_open_complete().unwrap();
                server.connect_request = Some(77);
                let deadline = wlan_control_wire::MonotonicDeadline::after(Duration::from_secs(2)).unwrap();
                server.dispatch(Packet {
                    generation: [1; 16],
                    request_id: 1,
                    message: Message::Disconnect {
                        deadline,
                        reason: sme::UserDisconnectReason::FailedToConnect,
                    },
                }).await.unwrap();
                let original_deadline = server.disconnect_request.as_ref().unwrap().requests[0].1;
                // Hold the admitted completion, not the request handler.
                let completion = server.runtime.disconnect.take();
                server.drive_once().await.unwrap();
                assert!(matches!(peer.try_receive_packet().unwrap().unwrap().packet.message, Message::Ready));
                assert!(peer.try_receive_packet().unwrap().is_none());
                peer.try_send_packet(&Packet {
                    generation: [1; 16],
                    request_id: 2,
                    message: Message::Scan {
                        deadline,
                        request: sme::ScanRequest::Passive(sme::PassiveScanRequest { channels: vec![] }),
                    },
                }, None).unwrap();
                server.drive_once().await.unwrap();
                assert!(matches!(peer.try_receive_packet().unwrap().unwrap().packet.message,
                    Message::ScanReply(Reply { in_reply_to: 2, result: Err(sme::ScanErrorCode::ShouldWait) })));
                peer.try_send_packet(&Packet {
                    generation: [1; 16],
                    request_id: 3,
                    message: Message::Disconnect {
                        deadline: deadline.checked_add(Duration::from_secs(1)).unwrap(),
                        reason: sme::UserDisconnectReason::FailedToConnect,
                    },
                }, None).unwrap();
                server.drive_once().await.unwrap();
                assert!(server.runtime.disconnect.is_none(), "duplicate restarted cleanup");
                let pending = server.disconnect_request.as_ref().unwrap();
                assert_eq!(pending.requests.len(), 2);
                assert_eq!(pending.requests[0].1, original_deadline);
                assert_eq!(pending.connect_request, Some(77));
                assert!(peer.try_receive_packet().unwrap().is_none());
                if expire {
                    server.disconnect_request.as_mut().unwrap().requests[0].1 = Instant::now();
                    server.drive_once().await.unwrap();
                    assert!(matches!(peer.try_receive_packet().unwrap().unwrap().packet.message,
                        Message::GenerationEnd(GenerationEndReason::Timeout)));
                    assert!(server.terminal);
                } else {
                    server.runtime.disconnect = completion;
                    server.drive_once().await.unwrap();
                    assert!(matches!(peer.try_receive_packet().unwrap().unwrap().packet.message,
                        Message::ConnectReply(Reply { in_reply_to: 77, .. })));
                    for id in [1, 3] {
                        assert!(matches!(peer.try_receive_packet().unwrap().unwrap().packet.message,
                            Message::DisconnectReply(Reply { in_reply_to, result: CommandReply::Success })
                            if in_reply_to == id));
                    }
                    assert!(server.disconnect_request.is_none());
                    server.drive_once().await.unwrap();
                }
                assert!(peer.try_receive_packet().unwrap().is_none());
            });
        }
    }

    #[test]
    fn power_save_reply_waits_for_runtime_completion() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&executor, async {
            for (completion, expected) in [
                (Ok(()), CommandReply::Success),
                (Err(RuntimeError::Busy), CommandReply::Busy),
                (Err(RuntimeError::Unsupported), CommandReply::Unsupported),
                (Err(RuntimeError::NotConnected), CommandReply::NotConnected),
            ] {
                let (policy, _policy_peer) = pair();
                let (supervisor, _supervisor_peer) = pair();
                let runtime = SimulatedWifiRuntime::new([2; 6]);
                let mut server = PreparedServer::new(policy, supervisor, [1; 16], runtime)
                    .unwrap()
                    .post_lockdown_open_complete()
                    .unwrap();
                server
                    .dispatch(Packet {
                        generation: [1; 16],
                        request_id: 7,
                        message: Message::SetPowerSave {
                            deadline: wlan_control_wire::MonotonicDeadline::after(
                                Duration::from_secs(1),
                            )
                            .unwrap(),
                            mode: wlan_control_wire::PowerSaveMode::Balanced,
                        },
                    })
                    .await
                    .unwrap();
                assert_eq!(server.power_save_request.map(|(id, _)| id), Some(7));
                assert_eq!(
                    server.outbound.len(),
                    1,
                    "only Ready is queued before completion"
                );
                server.runtime.power_save = Some(completion);
                server.drive_runtime().await.unwrap();
                assert!(!server.is_terminal());
                assert!(server.power_save_request.is_none());
                assert!(matches!(
                    decode(&server.outbound.back().unwrap().bytes)
                        .unwrap()
                        .message,
                    Message::SetPowerSaveReply(Reply {
                        in_reply_to: 7,
                        result
                    }) if result == expected
                ));
            }
        });
    }

    #[test]
    fn retained_terminal_event_precedes_explicit_disconnect_reply() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&executor, async {
            let (policy, _policy_peer) = pair();
            let (supervisor, _supervisor_peer) = pair();
            let mut runtime = SimulatedWifiRuntime::new([2; 6]);
            runtime.inject_connection_event(sme::ConnectTransactionEvent::OnDisconnect {
                info: sme::DisconnectInfo {
                    is_sme_reconnecting: false,
                    disconnect_source: sme::DisconnectSource::User(
                        sme::UserDisconnectReason::FailedToConnect,
                    ),
                },
            });
            let mut server = PreparedServer::new(policy, supervisor, [1; 16], runtime)
                .unwrap()
                .post_lockdown_open_complete()
                .unwrap();
            server
                .dispatch(Packet {
                    generation: [1; 16],
                    request_id: 1,
                    message: Message::Disconnect {
                        deadline: wlan_control_wire::MonotonicDeadline::after(Duration::from_secs(
                            1,
                        ))
                        .unwrap(),
                        reason: sme::UserDisconnectReason::FailedToConnect,
                    },
                })
                .await
                .unwrap();
            server.drive_runtime().await.unwrap();
            let messages: Vec<_> = server
                .outbound
                .iter()
                .map(|packet| decode(&packet.bytes).unwrap().message)
                .collect();
            assert!(matches!(
                messages.as_slice(),
                [
                    Message::Ready,
                    Message::Event(sme::ConnectTransactionEvent::OnDisconnect { .. }),
                    Message::DisconnectReply(_),
                ]
            ));
            assert!(!server.queue_connection_events().unwrap());
        });
    }

    #[test]
    fn expired_outbound_returns_owner_for_cleanup_even_after_terminal() {
        for supervisor_queue in [false, true] {
            let (policy, _policy_peer) = pair();
            let (supervisor, _supervisor_peer) = pair();
            let mut server = PreparedServer::new(
                policy,
                supervisor,
                [1; 16],
                SimulatedWifiRuntime::new([2; 6]),
            )
            .unwrap()
            .post_lockdown_open_complete()
            .unwrap();
            server.flush().unwrap();
            if supervisor_queue {
                let (ethernet, _peer) = pair();
                server
                    .queue_supervisor_install(1, [2; 6], ethernet)
                    .unwrap();
                server
                    .supervisor_outbound
                    .front_mut()
                    .unwrap()
                    .drain_deadline = Instant::now();
            } else {
                server
                    .end_generation(GenerationEndReason::Shutdown)
                    .unwrap();
                server.outbound.front_mut().unwrap().drain_deadline = Instant::now();
            }
            assert!(matches!(
                server.flush(),
                Err(ServiceError::Runtime(RuntimeError::Timeout))
            ));
            let runtime = server.into_runtime();
            assert_eq!(runtime.public_mac(), [2; 6]);
        }
    }
}
