// SPDX-License-Identifier: GPL-2.0-only

//! Bounded application-to-wlancfg command contract.
//!
//! Credentials occur only in [`Request::Connect`] and custom `Debug` output
//! redacts them. Each accepted `SOCK_SEQPACKET` connection carries exactly one
//! request and one reply.

use std::fmt;

pub const MAX_PACKET: usize = 4096;
const MAGIC: &[u8; 4] = b"WLP1";
const MAX_SSID: usize = 32;
const MAX_SECRET: usize = 63;
const MAX_ITEMS: usize = 64;
const MAX_APPLICATION_CLIENTS: usize = 16;

use futures::channel::mpsc;
use std::{
    io, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::mpsc as sync_mpsc,
    thread,
    time::{Duration, Instant},
};

pub struct ApplicationCommand {
    pub deadline: wlan_control_wire::MonotonicDeadline,
    pub request: Request,
    pub responder: sync_mpsc::SyncSender<Reply>,
}

/// Validated setup-only holder for a pre-bound application listener.
pub struct PreparedApplicationServer {
    listener: OwnedFd,
}

impl PreparedApplicationServer {
    pub fn from_inherited_listener(listener: OwnedFd) -> io::Result<Self> {
        let fd = listener.as_raw_fd();
        if socket_option(fd, libc::SO_DOMAIN)? != libc::AF_UNIX
            || socket_option(fd, libc::SO_TYPE)? != libc::SOCK_SEQPACKET
            || socket_option(fd, libc::SO_ACCEPTCONN)? != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "application fd is not a listening Unix seqpacket socket",
            ));
        }
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { listener })
    }

    /// Creates the owner during setup, parked before any application byte can
    /// be accepted or read. `activate_after_persistence` is the only start gate.
    pub fn spawn_parked(
        self,
        commands: mpsc::Sender<ApplicationCommand>,
    ) -> io::Result<ParkedApplicationServer> {
        let (start_tx, start_rx) = sync_mpsc::sync_channel(0);
        let (ready_tx, ready_rx) = sync_mpsc::sync_channel(0);
        let owner = thread::Builder::new()
            .name("wlancfg-application-io".into())
            .spawn(move || {
                if ready_tx.send(()).is_err() {
                    return;
                }
                if start_rx.recv().is_ok() {
                    serve_applications(self.listener, commands);
                }
            })?;
        ready_rx.recv().map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "application owner ended before reaching start gate",
            )
        })?;
        Ok(ParkedApplicationServer {
            start: Some(start_tx),
            _owner: owner,
        })
    }
}

pub struct ParkedApplicationServer {
    start: Option<sync_mpsc::SyncSender<()>>,
    // The production owner is intentionally process-lifetime. Closing the
    // listener is the supervisor's generation teardown.
    _owner: thread::JoinHandle<()>,
}

impl ParkedApplicationServer {
    pub fn activate_after_persistence(mut self) -> io::Result<()> {
        self.start
            .take()
            .expect("application start is single-use")
            .send(())
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "application owner ended before lockdown",
                )
            })
    }
}

enum ApplicationExchange {
    Receiving { deadline: Instant },
    Waiting(sync_mpsc::Receiver<Reply>),
    Sending { packet: Vec<u8>, deadline: Instant },
}

struct ApplicationClient {
    fd: OwnedFd,
    exchange: ApplicationExchange,
}

impl ApplicationClient {
    fn new(fd: OwnedFd) -> Self {
        Self {
            fd,
            exchange: ApplicationExchange::Receiving {
                deadline: Instant::now() + Duration::from_secs(2),
            },
        }
    }

    fn reply(&mut self, reply: Reply) -> bool {
        let Ok(packet) = encode_reply(&reply) else {
            return false;
        };
        self.exchange = ApplicationExchange::Sending {
            packet,
            deadline: Instant::now() + Duration::from_secs(2),
        };
        true
    }

    /// Do at most one nonblocking operation per client, so a slow request,
    /// policy operation or reply reader cannot stall other applications.
    fn poll(&mut self, commands: &mut mpsc::Sender<ApplicationCommand>) -> bool {
        match &mut self.exchange {
            ApplicationExchange::Receiving { deadline } => {
                let mut packet = [0u8; MAX_PACKET + 1];
                let count = unsafe {
                    libc::read(
                        self.fd.as_raw_fd(),
                        packet.as_mut_ptr().cast(),
                        packet.len(),
                    )
                };
                if count < 0 {
                    return io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock
                        && Instant::now() < *deadline;
                }
                let request = usize::try_from(count)
                    .ok()
                    .filter(|count| (1..=MAX_PACKET).contains(count))
                    .and_then(|count| decode_request(&packet[..count]).ok());
                let Some(request) = request else {
                    return self.reply(Reply::Error("invalid application request".into()));
                };
                // The policy executor must never block on the socket owner
                // being scheduled; each one-shot reply has one reserved slot.
                let (reply_tx, reply_rx) = sync_mpsc::sync_channel(1);
                let budget = if matches!(request, Request::Disconnect) {
                    10
                } else {
                    30
                };
                let deadline = match wlan_control_wire::MonotonicDeadline::after(
                    Duration::from_secs(budget),
                ) {
                    Ok(deadline) => deadline,
                    Err(_) => {
                        return self.reply(Reply::Error("operation clock unavailable".into()));
                    }
                };
                match commands.try_send(ApplicationCommand {
                    deadline,
                    request,
                    responder: reply_tx,
                }) {
                    Ok(()) => {
                        self.exchange = ApplicationExchange::Waiting(reply_rx);
                        true
                    }
                    Err(_) => self.reply(Reply::Error("policy command queue unavailable".into())),
                }
            }
            ApplicationExchange::Waiting(receiver) => match receiver.try_recv() {
                Ok(reply) => self.reply(reply),
                Err(sync_mpsc::TryRecvError::Empty) => true,
                Err(sync_mpsc::TryRecvError::Disconnected) => {
                    self.reply(Reply::Error("policy generation ended".into()))
                }
            },
            ApplicationExchange::Sending { packet, deadline } => {
                let count = unsafe {
                    libc::write(self.fd.as_raw_fd(), packet.as_ptr().cast(), packet.len())
                };
                if count >= 0 {
                    return false; // Complete or short send: never send a second packet.
                }
                io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock
                    && Instant::now() < *deadline
            }
        }
    }
}

fn serve_applications(listener: OwnedFd, mut commands: mpsc::Sender<ApplicationCommand>) {
    let mut clients = Vec::with_capacity(MAX_APPLICATION_CLIENTS);
    loop {
        if clients.len() < MAX_APPLICATION_CLIENTS {
            let client = unsafe {
                libc::accept4(
                    listener.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                )
            };
            if client >= 0 {
                clients.push(ApplicationClient::new(unsafe {
                    OwnedFd::from_raw_fd(client)
                }));
            } else if !matches!(
                io::Error::last_os_error().kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                return;
            }
        }
        clients.retain_mut(|client| client.poll(&mut commands));
        thread::sleep(Duration::from_millis(1));
    }
}

fn receive_one(fd: RawFd) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut packet = [0u8; MAX_PACKET + 1];
    loop {
        let count = unsafe { libc::read(fd, packet.as_mut_ptr().cast(), packet.len()) };
        if count >= 0 {
            let count = count as usize;
            if count == 0 || count > MAX_PACKET {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid packet length",
                ));
            }
            return Ok(packet[..count].to_vec());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::WouldBlock || Instant::now() >= deadline {
            return Err(error);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn send_one(fd: RawFd, packet: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let count = unsafe { libc::write(fd, packet.as_ptr().cast(), packet.len()) };
        if count == packet.len() as isize {
            return Ok(());
        }
        if count >= 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short seqpacket send",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::WouldBlock || Instant::now() >= deadline {
            return Err(error);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn socket_option(fd: RawFd, name: i32) -> io::Result<i32> {
    let mut value = 0i32;
    let mut length = mem::size_of::<i32>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            name,
            (&mut value as *mut i32).cast(),
            &mut length,
        )
    } != 0
    {
        Err(io::Error::last_os_error())
    } else if length as usize != mem::size_of::<i32>() {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid socket option length",
        ))
    } else {
        Ok(value)
    }
}

/// One-shot client used by `wlanctl`.
pub fn transact(path: &std::path::Path, request: &Request) -> io::Result<Reply> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.len() >= 108 || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid application socket path",
        ));
    }
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut address: libc::sockaddr_un = unsafe { mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (destination, source) in address.sun_path.iter_mut().zip(bytes.iter().copied()) {
        *destination = source as libc::c_char;
    }
    let length = mem::size_of::<libc::sa_family_t>() + bytes.len() + 1;
    if unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length as libc::socklen_t,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    send_one(
        fd.as_raw_fd(),
        &encode_request(request)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid command"))?,
    )?;
    decode_reply(&receive_one(fd.as_raw_fd())?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid policy reply"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Security {
    Open,
    Wpa2,
    Wpa3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowerSaveMode {
    Performance,
    Balanced,
}

#[derive(Clone, Eq, PartialEq)]
pub enum Request {
    Scan,
    Connect {
        ssid: Vec<u8>,
        security: Security,
        credential: Vec<u8>,
    },
    Status,
    Disconnect,
    PowerSave(PowerSaveMode),
    Saved,
    Forget {
        ssid: Vec<u8>,
        security: Security,
    },
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect { ssid, security, .. } => f
                .debug_struct("Connect")
                .field("ssid_len", &ssid.len())
                .field("security", security)
                .field("credential", &"<redacted>")
                .finish(),
            Self::Forget { ssid, security } => f
                .debug_struct("Forget")
                .field("ssid_len", &ssid.len())
                .field("security", security)
                .finish(),
            other => f.write_str(match other {
                Self::Scan => "Scan",
                Self::Status => "Status",
                Self::Disconnect => "Disconnect",
                Self::PowerSave(_) => "PowerSave",
                Self::Saved => "Saved",
                _ => unreachable!(),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Network {
    pub ssid: Vec<u8>,
    pub security: Security,
    pub rssi_dbm: i8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Association {
    Disconnected,
    Disconnecting,
    Connecting,
    Connected {
        channel: u8,
        rssi_dbm: i8,
        snr_db: i8,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Status {
    pub association: Association,
    pub ssid: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reply {
    Ok,
    Error(String),
    Scan(Vec<Network>),
    Status(Status),
    Saved(Vec<(Vec<u8>, Security)>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    Truncated,
    Invalid,
    BoundExceeded,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

pub fn encode_request(request: &Request) -> Result<Vec<u8>, Error> {
    let mut out = MAGIC.to_vec();
    match request {
        Request::Scan => out.push(1),
        Request::Connect {
            ssid,
            security,
            credential,
        } => {
            validate_ssid(ssid)?;
            if *security == Security::Open {
                if !credential.is_empty() {
                    return Err(Error::Invalid);
                }
            } else if !(8..=MAX_SECRET).contains(&credential.len()) {
                return Err(Error::Invalid);
            }
            out.push(2);
            bytes(&mut out, ssid)?;
            out.push(security_code(*security));
            bytes(&mut out, credential)?;
        }
        Request::Status => out.push(3),
        Request::Disconnect => out.push(4),
        Request::PowerSave(mode) => {
            out.push(7);
            out.push(match mode {
                PowerSaveMode::Performance => 0,
                PowerSaveMode::Balanced => 1,
            });
        }
        Request::Saved => out.push(5),
        Request::Forget { ssid, security } => {
            validate_ssid(ssid)?;
            out.push(6);
            bytes(&mut out, ssid)?;
            out.push(security_code(*security));
        }
    }
    bounded(out)
}

pub fn decode_request(packet: &[u8]) -> Result<Request, Error> {
    let mut r = Reader::new(packet)?;
    let request = match r.u8()? {
        1 => Request::Scan,
        2 => {
            let ssid = r.bytes(MAX_SSID)?;
            validate_ssid(&ssid)?;
            let security = decode_security(r.u8()?)?;
            let credential = r.bytes(MAX_SECRET)?;
            if security == Security::Open {
                if !credential.is_empty() {
                    return Err(Error::Invalid);
                }
            } else if credential.len() < 8 {
                return Err(Error::Invalid);
            }
            Request::Connect {
                ssid,
                security,
                credential,
            }
        }
        3 => Request::Status,
        4 => Request::Disconnect,
        5 => Request::Saved,
        6 => {
            let ssid = r.bytes(MAX_SSID)?;
            validate_ssid(&ssid)?;
            Request::Forget {
                ssid,
                security: decode_security(r.u8()?)?,
            }
        }
        7 => Request::PowerSave(match r.u8()? {
            0 => PowerSaveMode::Performance,
            1 => PowerSaveMode::Balanced,
            _ => return Err(Error::Invalid),
        }),
        _ => return Err(Error::Invalid),
    };
    if !r.done() {
        return Err(Error::Invalid);
    }
    Ok(request)
}

pub fn encode_reply(reply: &Reply) -> Result<Vec<u8>, Error> {
    let mut out = MAGIC.to_vec();
    match reply {
        Reply::Ok => out.push(20),
        Reply::Error(message) => {
            out.push(21);
            bytes(&mut out, message.as_bytes())?;
        }
        Reply::Scan(networks) => {
            if networks.len() > MAX_ITEMS {
                return Err(Error::BoundExceeded);
            }
            out.push(22);
            out.push(networks.len() as u8);
            for network in networks {
                validate_ssid(&network.ssid)?;
                bytes(&mut out, &network.ssid)?;
                out.push(security_code(network.security));
                out.push(network.rssi_dbm as u8);
            }
        }
        Reply::Status(status) => {
            out.push(23);
            match status.association {
                Association::Disconnected => out.push(0),
                Association::Disconnecting => out.push(1),
                Association::Connecting => out.push(2),
                Association::Connected {
                    channel,
                    rssi_dbm,
                    snr_db,
                } => {
                    out.extend_from_slice(&[3, channel, rssi_dbm as u8, snr_db as u8]);
                }
            }
            match &status.ssid {
                Some(ssid) => {
                    validate_ssid(ssid)?;
                    out.push(1);
                    bytes(&mut out, ssid)?;
                }
                None => out.push(0),
            }
        }
        Reply::Saved(networks) => {
            if networks.len() > MAX_ITEMS {
                return Err(Error::BoundExceeded);
            }
            out.push(24);
            out.push(networks.len() as u8);
            for (ssid, security) in networks {
                validate_ssid(ssid)?;
                bytes(&mut out, ssid)?;
                out.push(security_code(*security));
            }
        }
    }
    bounded(out)
}

pub fn decode_reply(packet: &[u8]) -> Result<Reply, Error> {
    let mut r = Reader::new(packet)?;
    let reply = match r.u8()? {
        20 => Reply::Ok,
        21 => Reply::Error(String::from_utf8(r.bytes(1024)?).map_err(|_| Error::Invalid)?),
        22 => {
            let count = r.u8()? as usize;
            if count > MAX_ITEMS {
                return Err(Error::BoundExceeded);
            }
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(Network {
                    ssid: r.bytes(MAX_SSID)?,
                    security: decode_security(r.u8()?)?,
                    rssi_dbm: r.u8()? as i8,
                });
            }
            Reply::Scan(values)
        }
        23 => {
            let association = match r.u8()? {
                0 => Association::Disconnected,
                1 => Association::Disconnecting,
                2 => Association::Connecting,
                3 => Association::Connected {
                    channel: r.u8()?,
                    rssi_dbm: r.u8()? as i8,
                    snr_db: r.u8()? as i8,
                },
                _ => return Err(Error::Invalid),
            };
            let ssid = match r.u8()? {
                0 => None,
                1 => Some(r.bytes(MAX_SSID)?),
                _ => return Err(Error::Invalid),
            };
            Reply::Status(Status { association, ssid })
        }
        24 => {
            let count = r.u8()? as usize;
            if count > MAX_ITEMS {
                return Err(Error::BoundExceeded);
            }
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push((r.bytes(MAX_SSID)?, decode_security(r.u8()?)?));
            }
            Reply::Saved(values)
        }
        _ => return Err(Error::Invalid),
    };
    if !r.done() {
        return Err(Error::Invalid);
    }
    Ok(reply)
}

fn validate_ssid(value: &[u8]) -> Result<(), Error> {
    if value.is_empty() || value.len() > MAX_SSID {
        Err(Error::Invalid)
    } else {
        Ok(())
    }
}
fn security_code(value: Security) -> u8 {
    match value {
        Security::Open => 0,
        Security::Wpa2 => 2,
        Security::Wpa3 => 3,
    }
}
fn decode_security(value: u8) -> Result<Security, Error> {
    match value {
        0 => Ok(Security::Open),
        2 => Ok(Security::Wpa2),
        3 => Ok(Security::Wpa3),
        _ => Err(Error::Invalid),
    }
}
fn bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), Error> {
    let length: u16 = value.len().try_into().map_err(|_| Error::BoundExceeded)?;
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(value);
    Ok(())
}
fn bounded(out: Vec<u8>) -> Result<Vec<u8>, Error> {
    if out.len() <= MAX_PACKET {
        Ok(out)
    } else {
        Err(Error::BoundExceeded)
    }
}

struct Reader<'a> {
    packet: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn new(packet: &'a [u8]) -> Result<Self, Error> {
        if packet.len() > MAX_PACKET {
            return Err(Error::BoundExceeded);
        }
        if packet.len() < 5 || &packet[..4] != MAGIC {
            return Err(Error::Invalid);
        }
        Ok(Self { packet, at: 4 })
    }
    fn done(&self) -> bool {
        self.at == self.packet.len()
    }
    fn u8(&mut self) -> Result<u8, Error> {
        let value = *self.packet.get(self.at).ok_or(Error::Truncated)?;
        self.at += 1;
        Ok(value)
    }
    fn bytes(&mut self, max: usize) -> Result<Vec<u8>, Error> {
        let low = self.u8()?;
        let high = self.u8()?;
        let length = u16::from_le_bytes([low, high]) as usize;
        if length > max {
            return Err(Error::BoundExceeded);
        }
        let end = self.at.checked_add(length).ok_or(Error::BoundExceeded)?;
        let value = self
            .packet
            .get(self.at..end)
            .ok_or(Error::Truncated)?
            .to_vec();
        self.at = end;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn client_pair() -> (ApplicationClient, OwnedFd) {
        let mut fds = [-1; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                    0,
                    fds.as_mut_ptr(),
                )
            },
            0
        );
        unsafe {
            (
                ApplicationClient::new(OwnedFd::from_raw_fd(fds[0])),
                OwnedFd::from_raw_fd(fds[1]),
            )
        }
    }

    #[test]
    fn pending_connect_reply_does_not_block_another_client_status() {
        let (mut connect, connect_peer) = client_pair();
        let (mut status, status_peer) = client_pair();
        let (mut commands, mut requests) = mpsc::channel(4);
        send_one(
            connect_peer.as_raw_fd(),
            &encode_request(&Request::Connect {
                ssid: b"ap".to_vec(),
                security: Security::Open,
                credential: vec![],
            })
            .unwrap(),
        )
        .unwrap();
        assert!(connect.poll(&mut commands));
        let pending_connect = requests.try_recv().unwrap();
        assert!(matches!(pending_connect.request, Request::Connect { .. }));
        // No connect response is available; this poll must return immediately.
        assert!(connect.poll(&mut commands));

        send_one(
            status_peer.as_raw_fd(),
            &encode_request(&Request::Status).unwrap(),
        )
        .unwrap();
        assert!(status.poll(&mut commands));
        let status_request = requests.try_recv().unwrap();
        assert_eq!(status_request.request, Request::Status);
        status_request.responder.send(Reply::Ok).unwrap();
        assert!(status.poll(&mut commands));
        assert!(!status.poll(&mut commands));
        assert_eq!(
            decode_reply(&receive_one(status_peer.as_raw_fd()).unwrap()).unwrap(),
            Reply::Ok
        );

        pending_connect
            .responder
            .send(Reply::Error("cancelled".into()))
            .unwrap();
        assert!(connect.poll(&mut commands));
        assert!(!connect.poll(&mut commands));
        assert_eq!(
            decode_reply(&receive_one(connect_peer.as_raw_fd()).unwrap()).unwrap(),
            Reply::Error("cancelled".into())
        );
    }

    #[test]
    fn incomplete_client_expires_without_stalling_a_complete_request() {
        let (mut slow, _slow_peer) = client_pair();
        let (mut ready, ready_peer) = client_pair();
        let (mut commands, mut requests) = mpsc::channel(4);
        assert!(slow.poll(&mut commands));
        send_one(
            ready_peer.as_raw_fd(),
            &encode_request(&Request::Disconnect).unwrap(),
        )
        .unwrap();
        assert!(ready.poll(&mut commands));
        assert_eq!(requests.try_recv().unwrap().request, Request::Disconnect);
        slow.exchange = ApplicationExchange::Receiving {
            deadline: Instant::now(),
        };
        assert!(!slow.poll(&mut commands));
    }

    #[test]
    fn request_round_trip_and_debug_redacts_secret() {
        let request = Request::Connect {
            ssid: b"ap".to_vec(),
            security: Security::Wpa3,
            credential: b"topsecret".to_vec(),
        };
        assert_eq!(
            decode_request(&encode_request(&request).unwrap()).unwrap(),
            request
        );
        assert!(!format!("{request:?}").contains("topsecret"));
    }
    #[test]
    fn power_save_modes_round_trip_without_unsupported_values() {
        for mode in [PowerSaveMode::Performance, PowerSaveMode::Balanced] {
            let request = Request::PowerSave(mode);
            assert_eq!(
                decode_request(&encode_request(&request).unwrap()),
                Ok(request)
            );
        }
        let mut invalid = MAGIC.to_vec();
        invalid.extend([7, 2]);
        assert_eq!(decode_request(&invalid), Err(Error::Invalid));
    }

    #[test]
    fn rejects_unbounded_and_malformed_values() {
        assert!(
            encode_request(&Request::Connect {
                ssid: vec![b'x'; 33],
                security: Security::Wpa2,
                credential: vec![b'x'; 8]
            })
            .is_err()
        );
        assert!(decode_request(b"WLP1\x02\x02\x00x").is_err());
    }
}
