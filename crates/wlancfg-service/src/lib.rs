// SPDX-License-Identifier: GPL-2.0-only

//! wlancfg-side binding for the bounded WLAN control seqpacket channel.
//!
//! This library owns transport mechanics only. Policy, persistence, process
//! setup, and sandboxing remain with the eventual service binary.

pub mod policy;

use anyhow::{anyhow, Context as _};
use async_trait::async_trait;
use fidl_fuchsia_wlan_sme as sme;
use futures::{
    channel::{mpsc, oneshot},
    stream,
    StreamExt as _,
};
use std::{
    collections::{HashMap, VecDeque},
    io,
    mem::{self, MaybeUninit},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc as sync_mpsc, Arc, Mutex,
    },
    thread,
};
use wlan_control_wire::{
    self as wire, CommandReply, ConnectReply, GenerationEndReason, Message, Packet,
    SessionValidator,
};
use wlancfg_selection::mode_management::{
    ClientSmeEventStream, ClientSmeScanResult, ClientSmeTransport,
    ConnectTransactionEventStream,
};

const QUEUE_PACKETS: usize = 64;
const QUEUE_BYTES: usize = 64 * 1024;

enum OwnerCommand {
    Connect {
        request: sme::ConnectRequest,
        reply: oneshot::Sender<anyhow::Result<(sme::ConnectResult, EventReceiver)>>,
    },
    Disconnect {
        reason: sme::UserDisconnectReason,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    Roam {
        request: sme::RoamRequest,
    },
    Scan {
        request: sme::ScanRequest,
        reply: oneshot::Sender<anyhow::Result<ClientSmeScanResult>>,
    },
}

type EventReceiver = mpsc::Receiver<anyhow::Result<sme::ConnectTransactionEvent>>;

enum Pending {
    Connect {
        reply: oneshot::Sender<anyhow::Result<(sme::ConnectResult, EventReceiver)>>,
        events: EventReceiver,
    },
    Disconnect(oneshot::Sender<anyhow::Result<()>>),
    Roam,
    Scan(oneshot::Sender<anyhow::Result<ClientSmeScanResult>>),
}

struct ClientInner {
    commands: sync_mpsc::SyncSender<OwnerCommand>,
    wake: Arc<OwnedFd>,
    force_terminal: Arc<AtomicBool>,
    event_stream: Mutex<Option<mpsc::Receiver<anyhow::Result<()>>>>,
    owner: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Drop for ClientInner {
    fn drop(&mut self) {
        self.force_terminal.store(true, Ordering::Release);
        wake(self.wake.as_raw_fd());
        if let Some(owner) = self.owner.lock().expect("owner lock poisoned").take() {
            let _ = owner.join();
        }
    }
}

/// Cloneable wlancfg-side control client. All socket I/O is owned by one
/// bounded worker, so replies and unsolicited events may be arbitrarily
/// interleaved without concurrent reads.
#[derive(Clone)]
pub struct HostControlClient(Arc<ClientInner>);

/// Setup-phase policy endpoint. Construction only validates and adopts the
/// inherited descriptor; this type has no operation that can receive bytes.
pub struct PreparedHostControlClient {
    fd: OwnedFd,
    generation: [u8; 16],
}

impl PreparedHostControlClient {
    /// Takes ownership of and validates an already-connected inherited AF_UNIX
    /// SOCK_SEQPACKET endpoint without starting I/O or creating a thread.
    pub fn from_inherited_socket(fd: OwnedFd, generation: [u8; 16]) -> anyhow::Result<Self> {
        validate_socket(&fd)?;
        set_nonblocking(&fd)?;

        Ok(Self { fd, generation })
    }

    /// Creates the bounded I/O owner thread while setup syscalls are still
    /// available, but parks it before it can poll or receive from the socket.
    /// Call after namespace/capability setup and before seccomp lockdown so the
    /// runtime profile needs no process-creation syscall.
    pub fn spawn_parked_after_setup(self) -> anyhow::Result<ParkedHostControlClient> {
        HostControlClient::park(self.fd, self.generation)
    }
}

/// An owner thread parked outside the control receive loop. Lockdown must be
/// installed with TSYNC before this value is opened.
pub struct ParkedHostControlClient {
    client: HostControlClient,
    start: Option<sync_mpsc::SyncSender<()>>,
    owner: Option<thread::JoinHandle<()>>,
}

impl ParkedHostControlClient {
    /// Releases the already-confined owner thread into its poll/receive loop.
    /// Call only from the service's locked-down run phase.
    pub fn activate_after_persistence(mut self) -> anyhow::Result<HostControlClient> {
        self.start
            .take()
            .expect("parked owner start is single-use")
            .send(())
            .map_err(|_| anyhow!("WLAN control owner ended before lockdown opened"))?;
        *self.client.0.owner.lock().expect("owner lock poisoned") = self.owner.take();
        Ok(self.client.clone())
    }
}

impl Drop for ParkedHostControlClient {
    fn drop(&mut self) {
        // Closing the one-shot start channel cancels a never-activated owner.
        self.start.take();
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

impl HostControlClient {
    #[cfg(test)]
    fn from_inherited_socket(fd: OwnedFd, generation: [u8; 16]) -> anyhow::Result<Self> {
        PreparedHostControlClient::from_inherited_socket(fd, generation)?
            .spawn_parked_after_setup()?
            .activate_after_persistence()
    }

    fn park(fd: OwnedFd, generation: [u8; 16]) -> anyhow::Result<ParkedHostControlClient> {

        let wake = wake_event()?;
        let (command_tx, command_rx) = sync_mpsc::sync_channel(QUEUE_PACKETS);
        // futures mpsc reserves one slot per sender in addition to this
        // buffer; there is exactly one owner-side sender.
        let (event_tx, event_rx) = mpsc::channel(QUEUE_PACKETS - 1);
        let force_terminal = Arc::new(AtomicBool::new(false));
        let owner_terminal = force_terminal.clone();
        let owner_wake = wake.clone();
        let (start_tx, start_rx) = sync_mpsc::sync_channel(0);
        let (ready_tx, ready_rx) = sync_mpsc::sync_channel(0);
        let owner = thread::Builder::new()
            .name("wlancfg-control-io".into())
            .spawn(move || {
                let owner = Owner::new(fd, owner_wake, generation, command_rx, event_tx);
                if ready_tx.send(()).is_err() {
                    return;
                }
                if start_rx.recv().is_ok() {
                    owner.run(owner_terminal);
                }
            })
            .context("spawn WLAN control I/O owner")?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("WLAN control owner ended before reaching start gate"))?;

        let client = Self(Arc::new(ClientInner {
            commands: command_tx,
            wake,
            force_terminal,
            event_stream: Mutex::new(Some(event_rx)),
            owner: Mutex::new(None),
        }));
        Ok(ParkedHostControlClient {
            client,
            start: Some(start_tx),
            owner: Some(owner),
        })
    }

    fn submit(&self, command: OwnerCommand) -> anyhow::Result<()> {
        if self.0.force_terminal.load(Ordering::Acquire) {
            return Err(anyhow!("WLAN control generation ended"));
        }
        match self.0.commands.try_send(command) {
            Ok(()) => {
                wake(self.0.wake.as_raw_fd());
                Ok(())
            }
            Err(sync_mpsc::TrySendError::Full(_)) => {
                self.0.force_terminal.store(true, Ordering::Release);
                wake(self.0.wake.as_raw_fd());
                Err(anyhow!("WLAN control command backpressure"))
            }
            Err(sync_mpsc::TrySendError::Disconnected(_)) => {
                Err(anyhow!("WLAN control generation ended"))
            }
        }
    }
}

#[async_trait(?Send)]
impl ClientSmeTransport for HostControlClient {
    async fn connect(
        &self,
        request: &sme::ConnectRequest,
    ) -> anyhow::Result<(sme::ConnectResult, ConnectTransactionEventStream)> {
        let (tx, rx) = oneshot::channel();
        self.submit(OwnerCommand::Connect { request: request.clone(), reply: tx })?;
        let (result, events) = rx.await.context("control generation ended during connect")??;
        Ok((result, events.boxed_local().fuse()))
    }

    async fn disconnect(&self, reason: sme::UserDisconnectReason) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.submit(OwnerCommand::Disconnect { reason, reply: tx })?;
        rx.await.context("control generation ended during disconnect")?
    }

    fn roam(&self, request: &sme::RoamRequest) -> anyhow::Result<()> {
        // The pinned interface defines acceptance synchronously. The bounded
        // owner accepts the command here; its wire acknowledgement is still
        // correlated and a rejection ends the liveness stream.
        self.submit(OwnerCommand::Roam { request: request.clone() })
    }

    async fn scan(&self, request: &sme::ScanRequest) -> anyhow::Result<ClientSmeScanResult> {
        let (tx, rx) = oneshot::channel();
        self.submit(OwnerCommand::Scan { request: request.clone(), reply: tx })?;
        rx.await.context("control generation ended during scan")?
    }

    fn take_event_stream(&self) -> ClientSmeEventStream {
        match self.0.event_stream.lock().expect("mutex poisoned").take() {
            Some(receiver) => receiver.boxed_local().fuse(),
            None => stream::once(async { Err(anyhow!("liveness stream already taken")) })
                .boxed_local()
                .fuse(),
        }
    }
}

struct Outgoing {
    bytes: Vec<u8>,
}

struct Owner {
    socket: OwnedFd,
    wake: Arc<OwnedFd>,
    generation: [u8; 16],
    validator: SessionValidator,
    next_sequence: u64,
    commands: sync_mpsc::Receiver<OwnerCommand>,
    pending: HashMap<u64, Pending>,
    outgoing: VecDeque<Outgoing>,
    outgoing_bytes: usize,
    transaction: Option<mpsc::Sender<anyhow::Result<sme::ConnectTransactionEvent>>>,
    liveness: mpsc::Sender<anyhow::Result<()>>,
}

impl Owner {
    fn new(
        socket: OwnedFd,
        wake: Arc<OwnedFd>,
        generation: [u8; 16],
        commands: sync_mpsc::Receiver<OwnerCommand>,
        liveness: mpsc::Sender<anyhow::Result<()>>,
    ) -> Self {
        Self {
            socket,
            wake,
            generation,
            validator: SessionValidator::new(generation),
            next_sequence: 1,
            commands,
            pending: HashMap::new(),
            outgoing: VecDeque::new(),
            outgoing_bytes: 0,
            transaction: None,
            liveness,
        }
    }

    fn run(mut self, force_terminal: Arc<AtomicBool>) {
        let result = self.run_inner(&force_terminal);
        let reason = result.err().unwrap_or_else(|| "control generation ended".into());
        force_terminal.store(true, Ordering::Release);
        self.finish(reason);
    }

    fn run_inner(&mut self, force_terminal: &AtomicBool) -> Result<(), String> {
        loop {
            if force_terminal.load(Ordering::Acquire) {
                return Err("control backpressure".into());
            }
            let mut fds = [
                libc::pollfd { fd: self.socket.as_raw_fd(), events: libc::POLLIN | if self.outgoing.is_empty() { 0 } else { libc::POLLOUT }, revents: 0 },
                libc::pollfd { fd: self.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
            if rc < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted { continue; }
                return Err(format!("control poll failed: {error}"));
            }
            if fds[1].revents != 0 {
                drain_wake(self.wake.as_raw_fd());
                self.drain_commands()?;
            }
            if fds[0].revents & libc::POLLOUT != 0 { self.flush_outgoing()?; }
            if fds[0].revents & libc::POLLIN != 0 {
                loop {
                    match recv_packet(self.socket.as_raw_fd()) {
                        Ok(Some(received)) => self.handle_received(received)?,
                        Ok(None) => break,
                        Err(error) => return Err(error),
                    }
                }
            }
            if fds[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err("control socket closed".into());
            }
        }
    }

    fn drain_commands(&mut self) -> Result<(), String> {
        loop {
            match self.commands.try_recv() {
                Ok(command) => self.queue_command(command)?,
                Err(sync_mpsc::TryRecvError::Empty) => return Ok(()),
                Err(sync_mpsc::TryRecvError::Disconnected) => return Err("control client dropped".into()),
            }
        }
    }

    fn queue_command(&mut self, command: OwnerCommand) -> Result<(), String> {
        if self.pending.len() >= QUEUE_PACKETS { return Err("pending request backpressure".into()); }
        let id = self.next_sequence;
        self.next_sequence = self.next_sequence.checked_add(1).ok_or("outgoing sequence exhausted")?;
        let (message, pending) = match command {
            OwnerCommand::Connect { request, reply } => {
                if self.transaction.as_ref().is_some_and(|sender| sender.is_closed()) {
                    self.transaction = None;
                }
                if self.transaction.is_some() { let _ = reply.send(Err(anyhow!("connect transaction already active"))); return Ok(()); }
                let (tx, rx) = mpsc::channel(QUEUE_PACKETS - 1);
                self.transaction = Some(tx);
                (Message::Connect(request), Pending::Connect { reply, events: rx })
            }
            OwnerCommand::Disconnect { reason, reply } => (Message::Disconnect(reason), Pending::Disconnect(reply)),
            OwnerCommand::Roam { request } => (Message::Roam(request), Pending::Roam),
            OwnerCommand::Scan { request, reply } => (Message::Scan(request), Pending::Scan(reply)),
        };
        let bytes = wire::encode(&Packet { generation: self.generation, request_id: id, message })
            .map_err(|e| format!("cannot encode control request: {e}"))?;
        if self.outgoing.len() >= QUEUE_PACKETS || self.outgoing_bytes + bytes.len() > QUEUE_BYTES {
            return Err("outgoing control backpressure".into());
        }
        self.outgoing_bytes += bytes.len();
        self.outgoing.push_back(Outgoing { bytes });
        self.pending.insert(id, pending);
        self.flush_outgoing()
    }

    fn flush_outgoing(&mut self) -> Result<(), String> {
        while let Some(packet) = self.outgoing.front() {
            let sent = unsafe { libc::sendto(self.socket.as_raw_fd(), packet.bytes.as_ptr().cast(), packet.bytes.len(), libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL, std::ptr::null(), 0) };
            if sent < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock { return Ok(()); }
                return Err(format!("control send failed: {error}"));
            }
            if sent as usize != packet.bytes.len() { return Err("partial seqpacket send".into()); }
            self.outgoing_bytes -= packet.bytes.len();
            self.outgoing.pop_front();
        }
        Ok(())
    }

    fn handle_received(&mut self, received: Received) -> Result<(), String> {
        let packet = wire::decode(&received.bytes).map_err(|e| format!("invalid control packet: {e}"))?;
        self.validator.validate(&packet).map_err(|e| format!("invalid control sequence: {e}"))?;
        match packet.message {
            Message::Ready => try_send(&mut self.liveness, Ok(()), "liveness")?,
            Message::Event(event) => {
                let ends_transaction = matches!(
                    &event,
                    sme::ConnectTransactionEvent::OnDisconnect { info }
                        if !info.is_sme_reconnecting
                );
                let transaction = self.transaction.as_mut().ok_or("event without connect transaction")?;
                try_send(transaction, Ok(event), "connect event")?;
                if ends_transaction { self.transaction = None; }
            }
            Message::GenerationEnd(reason) => return Err(format!("peer ended generation: {}", reason_name(reason))),
            message if message.in_reply_to().is_some() => self.handle_reply(message)?,
            _ => return Err("peer sent command on client channel".into()),
        }
        Ok(())
    }

    fn handle_reply(&mut self, message: Message) -> Result<(), String> {
        let id = message.in_reply_to().expect("reply checked");
        let pending = self.pending.remove(&id).ok_or("reply for unknown request")?;
        match (pending, message) {
            (Pending::Connect { reply, events }, Message::ConnectReply(reply_body)) => {
                let ConnectReply::Completed(result) = reply_body.result;
                if result.code.into_primitive() != 0 {
                    self.transaction = None;
                }
                if reply.send(Ok((result, events))).is_err() {
                    // The connect future was cancelled before its reply. Do
                    // not leave an unreachable transaction blocking a later
                    // policy attempt.
                    self.transaction = None;
                }
            }
            (Pending::Disconnect(reply), Message::DisconnectReply(reply_body)) => {
                let result = command_result(reply_body.result, "disconnect");
                if result.is_ok() { self.transaction = None; }
                let _ = reply.send(result);
            }
            (Pending::Roam, Message::RoamReply(reply_body)) => {
                match reply_body.result {
                    CommandReply::Success | CommandReply::Unsupported => {}
                    other => return Err(format!("roam rejected: {}", command_reply_name(other))),
                }
            }
            (Pending::Scan(reply), Message::ScanReply(reply_body)) => {
                let result = reply_body.result.map(|results| sme::ScanResultVector { results });
                let _ = reply.send(Ok(result));
            }
            _ => return Err("reply type does not match request".into()),
        }
        Ok(())
    }

    fn finish(&mut self, reason: String) {
        for (_, pending) in self.pending.drain() {
            match pending {
                Pending::Connect { reply, .. } => { let _ = reply.send(Err(anyhow!(reason.clone()))); }
                Pending::Disconnect(reply) => { let _ = reply.send(Err(anyhow!(reason.clone()))); }
                Pending::Scan(reply) => { let _ = reply.send(Err(anyhow!(reason.clone()))); }
                Pending::Roam => {}
            }
        }
        if let Some(mut tx) = self.transaction.take() { let _ = tx.try_send(Err(anyhow!(reason.clone()))); }
        let _ = self.liveness.try_send(Err(anyhow!(reason)));
    }
}

fn command_result(result: CommandReply, operation: &str) -> anyhow::Result<()> {
    match result {
        CommandReply::Success => Ok(()),
        other => Err(anyhow!("{operation} rejected: {}", command_reply_name(other))),
    }
}

fn command_reply_name(result: CommandReply) -> &'static str {
    match result { CommandReply::Success => "success", CommandReply::Busy => "busy", CommandReply::NotConnected => "not connected", CommandReply::Unsupported => "unsupported" }
}
fn reason_name(reason: GenerationEndReason) -> &'static str {
    match reason { GenerationEndReason::Shutdown => "shutdown", GenerationEndReason::Timeout => "timeout", GenerationEndReason::DriverFault => "driver fault", GenerationEndReason::ContainmentFault => "containment fault", GenerationEndReason::Backpressure => "backpressure", GenerationEndReason::ProtocolViolation => "protocol violation" }
}

fn try_send<T>(sender: &mut mpsc::Sender<anyhow::Result<T>>, value: anyhow::Result<T>, name: &str) -> Result<(), String> {
    sender.try_send(value).map_err(|_| format!("{name} backpressure"))
}

struct Received { bytes: Vec<u8> }

fn recv_packet(fd: RawFd) -> Result<Option<Received>, String> {
    let mut bytes = [0u8; wire::MAX_PACKET];
    // recvfrom has no ancillary-data interface: Linux discards SCM_RIGHTS
    // without installing descriptors in this process. The policy channel is
    // therefore capability-free at the syscall boundary, even if its peer is
    // compromised.
    let received = unsafe { libc::recvfrom(fd, bytes.as_mut_ptr().cast(), bytes.len(), libc::MSG_DONTWAIT | libc::MSG_TRUNC, std::ptr::null_mut(), std::ptr::null_mut()) };
    if received < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock { return Ok(None); }
        return Err(format!("control recvfrom failed: {error}"));
    }
    if received as usize > bytes.len() { return Err("truncated control packet".into()); }
    if received == 0 { return Err("control socket closed".into()); }
    Ok(Some(Received { bytes: bytes[..received as usize].to_vec() }))
}

fn validate_socket(fd: &OwnedFd) -> anyhow::Result<()> {
    let mut socket_type = 0i32;
    let mut len = mem::size_of::<i32>() as libc::socklen_t;
    if unsafe { libc::getsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, libc::SO_TYPE, (&mut socket_type as *mut i32).cast(), &mut len) } != 0 {
        return Err(io::Error::last_os_error()).context("inspect inherited control socket");
    }
    if socket_type != libc::SOCK_SEQPACKET { return Err(anyhow!("control fd is not SOCK_SEQPACKET")); }
    let mut address = MaybeUninit::<libc::sockaddr_storage>::zeroed();
    let mut address_len = mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    if unsafe { libc::getpeername(fd.as_raw_fd(), address.as_mut_ptr().cast(), &mut address_len) } != 0 {
        return Err(io::Error::last_os_error()).context("control socket is not connected");
    }
    let address = unsafe { address.assume_init() };
    if address.ss_family as i32 != libc::AF_UNIX { return Err(anyhow!("control socket is not AF_UNIX")); }
    Ok(())
}

fn set_nonblocking(fd: &OwnedFd) -> anyhow::Result<()> {
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error()).context("make control socket nonblocking");
    }
    Ok(())
}

fn wake_event() -> anyhow::Result<Arc<OwnedFd>> {
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("create control wake event");
    }
    Ok(Arc::new(unsafe { OwnedFd::from_raw_fd(fd) }))
}

fn wake(fd: RawFd) {
    let value = 1u64.to_ne_bytes();
    unsafe {
        libc::write(fd, value.as_ptr().cast(), value.len());
    }
}
fn drain_wake(fd: RawFd) {
    let mut value = [0u8; std::mem::size_of::<u64>()];
    while unsafe { libc::read(fd, value.as_mut_ptr().cast(), value.len()) } > 0 {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_wlan_common::ScanType;
    use fidl_fuchsia_wlan_ieee80211 as ieee;
    use fidl_fuchsia_wlan_internal as internal;
    use futures::FutureExt as _;
    use std::ptr;
    use wlan_control_wire::Reply;

    const GENERATION: [u8; 16] = [7; 16];

    fn sockets() -> (OwnedFd, OwnedFd) {
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) }, 0);
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn connect_request() -> sme::ConnectRequest {
        let channel = |number| ieee::ChannelNumber { band: ieee::WlanBand::FiveGhz, number };
        sme::ConnectRequest {
            ssid: b"network".to_vec(),
            bss_description: ieee::BssDescription {
                bssid: [1, 2, 3, 4, 5, 6], bss_type: ieee::BssType::Infrastructure,
                beacon_period: 100, capability_info: 0, ies: vec![], primary: channel(36),
                bandwidth: ieee::ChannelBandwidth::Cbw20, vht_secondary_80_channel: channel(0),
                rssi_dbm: -40, snr_db: 30,
            },
            multiple_bss_candidates: false,
            authentication: internal::Authentication { protocol: internal::Protocol::Open, credentials: None },
            deprecated_scan_type: ScanType::Active,
        }
    }

    fn receive(fd: RawFd) -> Packet {
        let mut bytes = [0; wire::MAX_PACKET];
        let size = unsafe { libc::recv(fd, bytes.as_mut_ptr().cast(), bytes.len(), 0) };
        assert!(size > 0, "recv: {}", io::Error::last_os_error());
        wire::decode(&bytes[..size as usize]).unwrap()
    }

    #[test]
    fn prepared_client_leaves_inbound_bytes_queued_until_started() {
        let (client_fd, server_fd) = sockets();
        let prepared =
            PreparedHostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        assert_eq!(
            unsafe { libc::send(server_fd.as_raw_fd(), b"bad".as_ptr().cast(), 3, 0) },
            3
        );
        // A setup-phase prepared endpoint has no worker and cannot consume the
        // queued untrusted packet. Starting the owner later observes it and
        // terminates the generation.
        let parked = prepared.spawn_parked_after_setup().unwrap();
        let client = parked.activate_after_persistence().unwrap();
        let mut liveness = client.take_event_stream();
        assert!(futures::executor::block_on(liveness.next()).unwrap().is_err());
    }

    #[test]
    fn dropping_parked_client_cancels_and_joins_owner() {
        let (client_fd, server_fd) = sockets();
        let parked = PreparedHostControlClient::from_inherited_socket(client_fd, GENERATION)
            .unwrap()
            .spawn_parked_after_setup()
            .unwrap();
        drop(parked);
        let mut byte = 0u8;
        assert_eq!(
            unsafe {
                libc::recv(
                    server_fd.as_raw_fd(),
                    (&mut byte as *mut u8).cast(),
                    1,
                    0,
                )
            },
            0,
            "cancelled parked owner retained the control socket"
        );
    }

    fn send_packet(fd: RawFd, sequence: u64, message: Message, rights: &[RawFd]) {
        let bytes = wire::encode(&Packet { generation: GENERATION, request_id: sequence, message }).unwrap();
        let mut iov = libc::iovec { iov_base: bytes.as_ptr().cast_mut().cast(), iov_len: bytes.len() };
        let mut control = [0usize; 8];
        let mut header: libc::msghdr = unsafe { mem::zeroed() };
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        if !rights.is_empty() {
            header.msg_control = control.as_mut_ptr().cast();
            header.msg_controllen = unsafe { libc::CMSG_SPACE(mem::size_of_val(rights) as _) as usize };
            unsafe {
                let cmsg = libc::CMSG_FIRSTHDR(&header);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN(mem::size_of_val(rights) as _) as usize;
                ptr::copy_nonoverlapping(rights.as_ptr(), libc::CMSG_DATA(cmsg).cast(), rights.len());
            }
        }
        assert_eq!(unsafe { libc::sendmsg(fd, &header, libc::MSG_NOSIGNAL) }, bytes.len() as isize);
    }

    #[test]
    fn interleaved_event_and_overlapping_disconnect_are_correlated() {
        let (client_fd, server_fd) = sockets();
        let client = HostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        let server = thread::spawn(move || {
            let first = receive(server_fd.as_raw_fd());
            let second = receive(server_fd.as_raw_fd());
            let (connect_id, disconnect_id) = match (&first.message, &second.message) {
                (Message::Connect(_), Message::Disconnect(_)) => (first.request_id, second.request_id),
                other => panic!("unexpected requests: {other:?}"),
            };
            let event = sme::ConnectTransactionEvent::OnSignalReport { ind: internal::SignalReportIndication { rssi_dbm: -47, snr_db: 22 } };
            send_packet(server_fd.as_raw_fd(), 91, Message::Event(event.clone()), &[]);
            send_packet(server_fd.as_raw_fd(), 92, Message::DisconnectReply(Reply { in_reply_to: disconnect_id, result: CommandReply::Success }), &[]);
            let result = sme::ConnectResult { code: ieee::StatusCode::Success, is_credential_rejected: false, is_reconnect: false };
            send_packet(server_fd.as_raw_fd(), 93, Message::ConnectReply(Reply { in_reply_to: connect_id, result: ConnectReply::Completed(result) }), &[]);
            (event, result)
        });
        let request = connect_request();
        let connect = client.connect(&request);
        let disconnect = client.disconnect(sme::UserDisconnectReason::WlanstackUnitTesting);
        let ((result, mut events), disconnected) = futures::executor::block_on(async {
            let (connected, disconnected) = futures::join!(connect, disconnect);
            (connected.unwrap(), disconnected)
        });
        disconnected.unwrap();
        let (expected_event, expected_result) = server.join().unwrap();
        assert_eq!(result, expected_result);
        assert_eq!(futures::executor::block_on(events.next()).unwrap().unwrap(), expected_event);
    }

    #[test]
    fn event_immediately_after_success_reply_reaches_transaction() {
        let (client_fd, server_fd) = sockets();
        let client = HostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        let server = thread::spawn(move || {
            let connect = receive(server_fd.as_raw_fd());
            let result = sme::ConnectResult { code: ieee::StatusCode::Success, is_credential_rejected: false, is_reconnect: false };
            let event = sme::ConnectTransactionEvent::OnSignalReport { ind: internal::SignalReportIndication { rssi_dbm: -48, snr_db: 21 } };
            send_packet(server_fd.as_raw_fd(), 1, Message::ConnectReply(Reply { in_reply_to: connect.request_id, result: ConnectReply::Completed(result) }), &[]);
            send_packet(server_fd.as_raw_fd(), 2, Message::Event(event.clone()), &[]);
            event
        });
        let (_result, mut events) = futures::executor::block_on(client.connect(&connect_request())).unwrap();
        assert_eq!(futures::executor::block_on(events.next()).unwrap().unwrap(), server.join().unwrap());
    }

    #[test]
    fn scan_preserves_exact_policy_result() {
        let (client_fd, server_fd) = sockets();
        let client = HostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        let server = thread::spawn(move || {
            let request = receive(server_fd.as_raw_fd());
            assert!(matches!(request.message, Message::Scan(_)));
            send_packet(server_fd.as_raw_fd(), 1, Message::ScanReply(Reply { in_reply_to: request.request_id, result: Err(sme::ScanErrorCode::ShouldWait) }), &[]);
        });
        let request = sme::ScanRequest::Passive(sme::PassiveScanRequest { channels: vec![1, 6, 11] });
        assert_eq!(futures::executor::block_on(client.scan(&request)).unwrap(), Err(sme::ScanErrorCode::ShouldWait));
        server.join().unwrap();
    }

    #[test]
    fn cancelled_connect_future_does_not_block_overlapping_disconnect() {
        let (client_fd, server_fd) = sockets();
        let client = HostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        let request = connect_request();
        let mut cancelled = Box::pin(client.connect(&request));
        assert!(cancelled.as_mut().now_or_never().is_none());
        drop(cancelled);
        let server = thread::spawn(move || {
            let connect = receive(server_fd.as_raw_fd());
            let disconnect = receive(server_fd.as_raw_fd());
            assert!(matches!(connect.message, Message::Connect(_)));
            assert!(matches!(disconnect.message, Message::Disconnect(_)));
            let result = sme::ConnectResult { code: ieee::StatusCode::Success, is_credential_rejected: false, is_reconnect: false };
            send_packet(server_fd.as_raw_fd(), 1, Message::ConnectReply(Reply { in_reply_to: connect.request_id, result: ConnectReply::Completed(result) }), &[]);
            send_packet(server_fd.as_raw_fd(), 2, Message::DisconnectReply(Reply { in_reply_to: disconnect.request_id, result: CommandReply::Success }), &[]);
        });
        futures::executor::block_on(client.disconnect(sme::UserDisconnectReason::WlanstackUnitTesting)).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn successful_disconnect_allows_reconnect() {
        let (client_fd, server_fd) = sockets();
        let client = HostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        let server = thread::spawn(move || {
            let first = receive(server_fd.as_raw_fd());
            let success = sme::ConnectResult { code: ieee::StatusCode::Success, is_credential_rejected: false, is_reconnect: false };
            send_packet(server_fd.as_raw_fd(), 1, Message::ConnectReply(Reply { in_reply_to: first.request_id, result: ConnectReply::Completed(success) }), &[]);
            let disconnect = receive(server_fd.as_raw_fd());
            send_packet(server_fd.as_raw_fd(), 2, Message::DisconnectReply(Reply { in_reply_to: disconnect.request_id, result: CommandReply::Success }), &[]);
            let second = receive(server_fd.as_raw_fd());
            assert!(matches!(second.message, Message::Connect(_)));
            send_packet(server_fd.as_raw_fd(), 3, Message::ConnectReply(Reply { in_reply_to: second.request_id, result: ConnectReply::Completed(success) }), &[]);
        });
        let request = connect_request();
        let (_result, _first_events) = futures::executor::block_on(client.connect(&request)).unwrap();
        futures::executor::block_on(client.disconnect(sme::UserDisconnectReason::Startup)).unwrap();
        let (_result, _second_events) = futures::executor::block_on(client.connect(&request)).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn unsupported_roam_does_not_end_policy_generation() {
        let (client_fd, server_fd) = sockets();
        let client = HostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        let server = thread::spawn(move || {
            let roam = receive(server_fd.as_raw_fd());
            assert!(matches!(roam.message, Message::Roam(_)));
            send_packet(server_fd.as_raw_fd(), 1, Message::RoamReply(Reply { in_reply_to: roam.request_id, result: CommandReply::Unsupported }), &[]);
            let disconnect = receive(server_fd.as_raw_fd());
            assert!(matches!(disconnect.message, Message::Disconnect(_)));
            send_packet(server_fd.as_raw_fd(), 2, Message::DisconnectReply(Reply { in_reply_to: disconnect.request_id, result: CommandReply::Success }), &[]);
        });
        client.roam(&sme::RoamRequest { bss_description: connect_request().bss_description }).unwrap();
        futures::executor::block_on(client.disconnect(sme::UserDisconnectReason::Startup)).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn stale_event_after_failed_attempt_is_terminal() {
        let (client_fd, server_fd) = sockets();
        let client = HostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        let mut liveness = client.take_event_stream();
        let server = thread::spawn(move || {
            let connect = receive(server_fd.as_raw_fd());
            let failed = sme::ConnectResult { code: ieee::StatusCode::RefusedReasonUnspecified, is_credential_rejected: false, is_reconnect: false };
            send_packet(server_fd.as_raw_fd(), 1, Message::ConnectReply(Reply { in_reply_to: connect.request_id, result: ConnectReply::Completed(failed) }), &[]);
            send_packet(server_fd.as_raw_fd(), 2, Message::Event(sme::ConnectTransactionEvent::OnSignalReport { ind: internal::SignalReportIndication { rssi_dbm: -50, snr_db: 20 } }), &[]);
            failed
        });
        let (result, mut events) = futures::executor::block_on(client.connect(&connect_request())).unwrap();
        assert_eq!(result, server.join().unwrap());
        assert!(futures::executor::block_on(events.next()).is_none());
        assert!(futures::executor::block_on(liveness.next()).unwrap().is_err());
        assert!(client.roam(&sme::RoamRequest { bss_description: connect_request().bss_description }).is_err());
    }

    fn terminal_after(send_bad: impl FnOnce(RawFd)) {
        let (client_fd, server_fd) = sockets();
        let client = HostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        let mut liveness = client.take_event_stream();
        send_bad(server_fd.as_raw_fd());
        assert!(futures::executor::block_on(liveness.next()).unwrap().is_err());
        assert!(futures::executor::block_on(liveness.next()).is_none());
    }

    #[test]
    fn malformed_and_truncated_packets_are_terminal() {
        terminal_after(|fd| assert_eq!(unsafe { libc::send(fd, b"bad".as_ptr().cast(), 3, 0) }, 3));
        terminal_after(|fd| {
            let oversized = vec![0u8; wire::MAX_PACKET + 1];
            assert_eq!(unsafe { libc::send(fd, oversized.as_ptr().cast(), oversized.len(), 0) }, oversized.len() as isize);
        });
    }

    #[test]
    fn wrong_generation_ends_pending_future_and_liveness() {
        let (client_fd, server_fd) = sockets();
        let client = HostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        let mut liveness = client.take_event_stream();
        let request = sme::ScanRequest::Passive(sme::PassiveScanRequest { channels: vec![] });
        let server = thread::spawn(move || {
            let scan = receive(server_fd.as_raw_fd());
            let bytes = wire::encode(&Packet {
                generation: [8; 16],
                request_id: 1,
                message: Message::ScanReply(Reply {
                    in_reply_to: scan.request_id,
                    result: Err(sme::ScanErrorCode::InternalError),
                }),
            }).unwrap();
            assert_eq!(unsafe { libc::send(server_fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len(), 0) }, bytes.len() as isize);
        });
        assert!(futures::executor::block_on(client.scan(&request)).is_err());
        assert!(futures::executor::block_on(liveness.next()).unwrap().is_err());
        server.join().unwrap();
    }

    #[test]
    fn ancillary_rights_are_discarded_without_installation() {
        let (client_fd, server_fd) = sockets();
        let parked = PreparedHostControlClient::from_inherited_socket(client_fd, GENERATION)
            .unwrap()
            .spawn_parked_after_setup()
            .unwrap();
        let capabilities: Vec<_> = (0..8).map(|_| sockets()).collect();
        let raw: Vec<_> = capabilities.iter().map(|(passed, _)| passed.as_raw_fd()).collect();
        send_packet(server_fd.as_raw_fd(), 1, Message::Ready, &raw);
        let peers: Vec<_> = capabilities.into_iter().map(|(passed, peer)| { drop(passed); peer }).collect();
        let client = parked.activate_after_persistence().unwrap();
        let mut liveness = client.take_event_stream();
        assert!(futures::executor::block_on(liveness.next()).unwrap().is_ok());
        for peer in peers {
            assert_eq!(unsafe { libc::send(peer.as_raw_fd(), b"x".as_ptr().cast(), 1, libc::MSG_NOSIGNAL) }, -1);
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPIPE));
        }
    }

    #[test]
    fn zero_length_packet_with_fd_is_terminal_without_leaking_fd() {
        let (client_fd, server_fd) = sockets();
        let client = HostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        let mut liveness = client.take_event_stream();
        let (passed, peer) = sockets();
        let mut control = [0usize; 4];
        let mut header: libc::msghdr = unsafe { mem::zeroed() };
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = unsafe { libc::CMSG_SPACE(mem::size_of::<RawFd>() as _) as usize };
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&header);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(mem::size_of::<RawFd>() as _) as usize;
            *libc::CMSG_DATA(cmsg).cast::<RawFd>() = passed.as_raw_fd();
        }
        assert_eq!(unsafe { libc::sendmsg(server_fd.as_raw_fd(), &header, libc::MSG_NOSIGNAL) }, 0);
        drop(passed);
        assert!(futures::executor::block_on(liveness.next()).unwrap().is_err());
        assert_eq!(unsafe { libc::send(peer.as_raw_fd(), b"x".as_ptr().cast(), 1, libc::MSG_NOSIGNAL) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPIPE));
    }

    #[test]
    fn mixed_credentials_and_rights_are_discarded_without_installation() {
        let (client_fd, server_fd) = sockets();
        let enabled = 1i32;
        assert_eq!(unsafe {
            libc::setsockopt(
                client_fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                (&enabled as *const i32).cast(),
                mem::size_of::<i32>() as _,
            )
        }, 0);
        let client = HostControlClient::from_inherited_socket(client_fd, GENERATION).unwrap();
        let mut liveness = client.take_event_stream();
        let (passed, peer) = sockets();
        send_packet(server_fd.as_raw_fd(), 1, Message::Ready, &[passed.as_raw_fd()]);
        drop(passed);
        assert!(futures::executor::block_on(liveness.next()).unwrap().is_ok());
        assert_eq!(unsafe { libc::send(peer.as_raw_fd(), b"x".as_ptr().cast(), 1, libc::MSG_NOSIGNAL) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPIPE));
    }
}
