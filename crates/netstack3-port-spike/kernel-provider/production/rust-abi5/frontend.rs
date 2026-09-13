// SPDX-License-Identifier: GPL-2.0-only
//! ABI5 frontend state. Linux owns native object mechanics; Netstack3 owns TCP/IP.
#![forbid(unsafe_code)]
use crate::{
    endpoint_file::{self, Endpoint},
    linux::{AcceptTarget, Accepted, Address, Message, NativeSock, NetRef},
};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use kernel::{
    bindings as b,
    fs::{file::FileDescriptorReservation, File},
    prelude::*,
    sync::{
        poll::{PollCondVar, PollTable},
        Arc, CondVarTimeoutResult, Mutex,
    },
    uaccess::{UserPtr, UserSlice, UserSliceReader, UserSliceWriter},
};
const VERSION: u32 = 5;
const PAYLOAD: usize = 16384;
const LIMIT: usize = 256 * 1024;
const SOCKETS: usize = 256;
const REQUESTS: usize = 32;
const OPEN: u32 = 1;
const BIND: u32 = 2;
const LISTEN: u32 = 3;
const CONNECT: u32 = 4;
const ACCEPT: u32 = 5;
const SEND: u32 = 6;
const SHUTDOWN: u32 = 7;
const CLOSE: u32 = 8;
const RX: u32 = 9;
const STATE: u32 = 10;
const GETNAME: u32 = 11;
const CREDIT: u32 = 13;
const CLAIM: u32 = 0x8008B301;
const PUBLISH: u32 = 0xC038B302;

fn bytes(data: &[u8]) -> Result<KVec<u8>> {
    let mut out = KVec::new();
    out.extend_from_slice(data, GFP_KERNEL)?;
    Ok(out)
}
fn word(data: &[u8]) -> Result<u32> {
    Ok(u32::from_le_bytes(data.try_into().map_err(|_| EPROTO)?))
}
fn frame(op: u32, socket: u64, request: u64, len: usize) -> [u8; 32] {
    let mut header = [0; 32];
    header[..4].copy_from_slice(&VERSION.to_le_bytes());
    header[4..8].copy_from_slice(&op.to_le_bytes());
    header[8..16].copy_from_slice(&socket.to_le_bytes());
    header[16..24].copy_from_slice(&request.to_le_bytes());
    header[24..28].copy_from_slice(&(len as u32).to_le_bytes());
    header
}
struct Registry {
    next_id: u64,
    next_generation: u64,
    sockets: KVec<Arc<Socket>>,
}
#[pin_data]
pub(crate) struct Namespace {
    #[pin]
    registry: Mutex<Registry>,
    #[pin]
    changed: PollCondVar,
    live: AtomicU64,
    count: AtomicUsize,
}
impl Namespace {
    pub(crate) fn new() -> Result<Arc<Self>> {
        Arc::pin_init(
            try_pin_init!(Self {
                registry <- kernel::new_mutex!(Registry {next_id:0,next_generation:0,sockets:KVec::new()}),
                changed <- kernel::new_poll_condvar!(), live:AtomicU64::new(0),count:AtomicUsize::new(0),
            }),
            GFP_KERNEL,
        )
    }
    pub(crate) fn socket(
        ns: Arc<Self>,
        native: NativeSock,
        family: i32,
        kind: i32,
        claimed: bool,
    ) -> Result<Arc<Socket>> {
        let mut registry = ns.registry.lock();
        let generation = ns.live.load(Ordering::Acquire);
        if generation == 0 {
            return Err(ENETDOWN);
        }
        if ns.count.load(Ordering::Relaxed) >= SOCKETS {
            return Err(ENFILE);
        }
        let id = registry.next_id.checked_add(1).ok_or(EOVERFLOW)?;
        registry.next_id = id;
        ns.count.fetch_add(1, Ordering::Relaxed);
        let lease = Lease(ns.clone());
        let mut local = Address::default();
        local.0[0] = if family == 2 { 4 } else { 6 };
        let socket = Arc::pin_init(
            try_pin_init!(Socket {
                native, id, generation, family, kind, lease,
                state <- kernel::new_mutex!(SocketState {
                    claimed,opened:claimed,connected:claimed,connecting:false,listening:false,
                    dead:false,app_closed:false,close_read:false,eof:false,shutdown:0,
                    local,peer:Address::default(),next_request:0,requests:KVec::new(),
                    reply:None,tx_bytes:0,rx:KVec::new(),rx_offset:0,rx_credit:4,credit_return:0,
                    accepted:KVec::new(),backlog:0,accept_space:false,
                }),
                changed <- kernel::new_poll_condvar!(),
                provider_changed <- kernel::new_poll_condvar!(),
                control <- kernel::new_mutex!(()), transmit <- kernel::new_mutex!(()),receive <- kernel::new_mutex!(()),
            }),
            GFP_KERNEL,
        )?;
        registry.sockets.push(socket.clone(), GFP_KERNEL)?;
        drop(registry);
        ns.changed.notify_all();
        Ok(socket)
    }
}
struct Lease(Arc<Namespace>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.count.fetch_sub(1, Ordering::Relaxed);
    }
}
struct Request {
    id: u64,
    op: u32,
    data: KVec<u8>,
    read: bool,
    synchronous: bool,
    credit: usize,
}
struct Packet {
    address: Address,
    data: KVec<u8>,
}
struct SocketState {
    claimed: bool,
    opened: bool,
    connected: bool,
    connecting: bool,
    listening: bool,
    dead: bool,
    app_closed: bool,
    close_read: bool,
    eof: bool,
    shutdown: u32,
    local: Address,
    peer: Address,
    next_request: u64,
    requests: KVec<Request>,
    reply: Option<Result<KVec<u8>>>,
    tx_bytes: usize,
    rx: KVec<Packet>,
    rx_offset: usize,
    rx_credit: u32,
    credit_return: u32,
    accepted: KVec<Accepted>,
    backlog: usize,
    accept_space: bool,
}
#[pin_data]
pub(crate) struct Socket {
    id: u64,
    generation: u64,
    family: i32,
    kind: i32,
    #[pin]
    state: Mutex<SocketState>,
    #[pin]
    changed: PollCondVar,
    #[pin]
    provider_changed: PollCondVar,
    #[pin]
    control: Mutex<()>,
    #[pin]
    transmit: Mutex<()>,
    #[pin]
    receive: Mutex<()>,
    lease: Lease,
    native: NativeSock,
}
impl Socket {
    fn alive(&self, s: &SocketState) -> bool {
        !s.dead && self.lease.0.live.load(Ordering::Acquire) == self.generation
    }
    fn wake(&self) {
        self.changed.notify_all();
        self.provider_changed.notify_all();
    }
    fn abort_locked(&self, s: &mut SocketState) {
        s.dead = true;
        s.requests.clear();
        s.tx_bytes = 0;
        s.reply = Some(Err(ENETDOWN));
        self.native.set_error(b::ENETDOWN as i32);
        self.wake();
    }
    fn abort(&self) {
        self.abort_locked(&mut self.state.lock());
    }
    pub(crate) fn close_app(&self) {
        let mut registry = self.lease.0.registry.lock();
        registry.sockets.retain(|s| s.id != self.id);
        let mut s = self.state.lock();
        s.app_closed = true;
        s.rx.clear();
        s.rx_offset = 0;
        let accepted = core::mem::replace(&mut s.accepted, KVec::new());
        if !s.claimed {
            self.abort_locked(&mut s);
        }
        self.provider_changed.notify_all();
        drop(s);
        drop(registry);
        drop(accepted);
    }
    fn submit(
        &self,
        s: &mut SocketState,
        op: u32,
        data: KVec<u8>,
        synchronous: bool,
        credit: usize,
    ) -> Result {
        if !self.alive(s) {
            return Err(ENETDOWN);
        }
        if s.requests.len() >= if credit != 0 { REQUESTS - 8 } else { REQUESTS }
            || s.tx_bytes + credit > LIMIT
        {
            return Err(EAGAIN);
        }
        let id = s.next_request.checked_add(1).ok_or(EOVERFLOW)?;
        s.requests.push(
            Request {
                id,
                op,
                data,
                read: false,
                synchronous,
                credit,
            },
            GFP_KERNEL,
        )?;
        s.next_request = id;
        s.tx_bytes += credit;
        self.provider_changed.notify_all();
        Ok(())
    }
    // control mutex must be held by callers: exactly one synchronous result slot.
    fn open(&self) -> Result {
        let mut s = self.state.lock();
        if s.opened {
            return Ok(());
        }
        let mut data = bytes(&(self.kind as u32).to_le_bytes())?;
        data.extend_from_slice(
            &(if self.family == 2 { 4u32 } else { 6u32 }).to_le_bytes(),
            GFP_KERNEL,
        )?;
        self.submit(&mut s, OPEN, data, false, 0)?;
        s.opened = true;
        Ok(())
    }
    fn wait(
        &self,
        s: &mut kernel::sync::lock::Guard<'_, SocketState, kernel::sync::lock::mutex::MutexBackend>,
        timeout: &mut usize,
    ) -> Result {
        match self.changed.wait_interruptible_timeout(s, *timeout) {
            CondVarTimeoutResult::Signal { .. } => Err(ERESTARTSYS),
            CondVarTimeoutResult::Timeout => {
                *timeout = 0;
                Err(EAGAIN)
            }
            CondVarTimeoutResult::Woken { jiffies } => {
                *timeout = jiffies;
                Ok(())
            }
        }
    }
    fn call(&self, op: u32, data: KVec<u8>) -> Result<KVec<u8>> {
        let mut s = self.state.lock();
        s.reply = None;
        self.submit(&mut s, op, data, true, 0)?;
        let mut timeout = kernel::time::msecs_to_jiffies(10000);
        loop {
            if let Some(reply) = s.reply.take() {
                return reply;
            }
            if !self.alive(&s) {
                return Err(ENETDOWN);
            }
            if let Err(error) = self.wait(&mut s, &mut timeout) {
                if let Some(reply) = s.reply.take() {
                    return reply;
                }
                if op == GETNAME {
                    // A name query has no remote side effect. Keep its ID
                    // until completion, but detach this interrupted waiter.
                    // A late result cannot overwrite the next control result.
                    for request in s.requests.iter_mut() {
                        if request.synchronous {
                            request.synchronous = false;
                        }
                    }
                } else {
                    // Mutating control may already have taken effect remotely.
                    self.abort_locked(&mut s);
                }
                return Err(if error == EAGAIN { ETIMEDOUT } else { error });
            }
        }
    }
    pub(crate) fn bind(&self, a: Address) -> Result {
        if a.family() != self.family {
            return Err(EAFNOSUPPORT);
        }
        let _control = self.control.lock();
        self.open()?;
        let local = Address::from_wire(&self.call(BIND, bytes(&a.0)?)?)?;
        self.state.lock().local = local;
        Ok(())
    }
    pub(crate) fn listen(&self, backlog: i32) -> Result {
        if self.kind != 1 {
            return Err(EOPNOTSUPP);
        }
        let _control = self.control.lock();
        self.open()?;
        let value = (backlog.clamp(0, SOCKETS as i32) as u32).to_le_bytes();
        let local = Address::from_wire(&self.call(LISTEN, bytes(&value)?)?)?;
        let mut s = self.state.lock();
        s.local = local;
        s.listening = true;
        s.backlog = backlog.max(1) as usize;
        s.accept_space = true;
        self.provider_changed.notify_all();
        Ok(())
    }
    pub(crate) fn connect(&self, a: Address, flags: i32) -> Result {
        if a.family() != self.family {
            return Err(EAFNOSUPPORT);
        }
        let _control = self.control.lock();
        {
            let s = self.state.lock();
            if s.connected {
                return Err(EISCONN);
            }
            if s.connecting {
                return Err(EALREADY);
            }
        }
        self.open()?;
        let mut s = self.state.lock();
        self.submit(&mut s, CONNECT, bytes(&a.0)?, false, 0)?;
        s.peer = a;
        s.connecting = true;
        if self.kind == 1 && flags & b::O_NONBLOCK as i32 != 0 {
            return Err(EINPROGRESS);
        }
        let mut timeout = self.native.timeout(true, false);
        while s.connecting && self.alive(&s) {
            if let Err(e) = self.wait(&mut s, &mut timeout) {
                return Err(if e == EAGAIN { EINPROGRESS } else { e });
            }
        }
        if !self.alive(&s) {
            Err(ENETDOWN)
        } else if s.connected {
            Ok(())
        } else {
            let error = self.native.error(true);
            // SO_ERROR may already have been consumed by another thread.
            if error == 0 {
                Ok(())
            } else {
                Err(Error::from_errno(error))
            }
        }
    }
    pub(crate) fn name(&self, peer: bool) -> Result<Address> {
        let _control = self.control.lock();
        if peer && !self.state.lock().opened {
            return Err(ENOTCONN);
        }
        self.open()?;
        Address::from_wire(&self.call(GETNAME, bytes(&(peer as u32).to_le_bytes())?)?)
    }
    pub(crate) fn shutdown(&self, how: i32) -> Result {
        if !(0..=2).contains(&how) {
            return Err(EINVAL);
        }
        let _transmit = self.transmit.lock();
        let _control = self.control.lock();
        let reply = self.call(SHUTDOWN, bytes(&((how + 1) as u32).to_le_bytes())?)?;
        if !reply.is_empty() {
            return Err(EPROTO);
        }
        self.state.lock().shutdown |= (how + 1) as u32;
        self.native.shutdown(how + 1);
        self.changed.notify_all();
        Ok(())
    }
    pub(crate) fn accept(&self, target: &mut AcceptTarget<'_>, flags: i32) -> Result {
        let mut timeout = self
            .native
            .timeout(false, flags & b::O_NONBLOCK as i32 != 0);
        let mut s = self.state.lock();
        loop {
            if !s.listening {
                return Err(EINVAL);
            }
            if !self.alive(&s) {
                return Err(ENETDOWN);
            }
            if !s.accepted.is_empty() {
                let child = s.accepted.remove(0).unwrap();
                s.accept_space = true;
                self.provider_changed.notify_all();
                drop(s);
                child.transfer(target);
                return Ok(());
            }
            if timeout == 0 {
                return Err(EAGAIN);
            }
            self.wait(&mut s, &mut timeout)?;
        }
    }
    pub(crate) fn send(&self, msg: &mut Message<'_>, len: usize) -> Result<usize> {
        let flags = msg.flags();
        if flags & !(b::MSG_DONTWAIT | b::MSG_NOSIGNAL | b::MSG_MORE | b::MSG_BATCH) != 0 {
            return Err(EOPNOTSUPP);
        }
        if self.kind == 2 && len > PAYLOAD {
            return Err(EMSGSIZE);
        }
        let _transmit = self.transmit.lock();
        if self.state.lock().shutdown & 2 != 0 {
            if flags & b::MSG_NOSIGNAL == 0 {
                crate::linux::sigpipe();
            }
            return Err(EPIPE);
        }
        let dest = msg.name()?.unwrap_or_default();
        {
            let _control = self.control.lock();
            self.open()?;
        }
        if self.kind == 1 && !self.state.lock().connected {
            return Err(ENOTCONN);
        }
        let mut done = 0;
        let mut timeout = self.native.timeout(true, flags & b::MSG_DONTWAIT != 0);
        loop {
            let count = (len - done).min(PAYLOAD);
            let mut s = self.state.lock();
            if !self.alive(&s) {
                return if done > 0 { Ok(done) } else { Err(ENETDOWN) };
            }
            if s.requests.len() >= REQUESTS - 8 || s.tx_bytes + count.max(1) > LIMIT {
                if done > 0 {
                    return Ok(done);
                }
                if timeout == 0 {
                    return Err(EAGAIN);
                }
                self.wait(&mut s, &mut timeout)?;
                continue;
            }
            // Reserve both allocations before consuming the iterator. Holding
            // the socket mutex prevents admission from changing after the copy.
            let allocation = (|| -> Result<KVec<u8>> {
                s.requests.reserve(1, GFP_KERNEL)?;
                let mut data = bytes(&dest.0)?;
                data.resize(24 + count, 0, GFP_KERNEL)?;
                Ok(data)
            })();
            let mut data = match allocation {
                Ok(data) => data,
                Err(error) => return if done > 0 { Ok(done) } else { Err(error) },
            };
            if let Err(e) = msg.read(&mut data[24..]) {
                return if done > 0 { Ok(done) } else { Err(e) };
            }
            if let Err(error) = self.submit(&mut s, SEND, data, false, count.max(1)) {
                msg.rollback_read();
                return if done > 0 { Ok(done) } else { Err(error) };
            }
            msg.commit_read();
            done += count;
            if done == len {
                return Ok(done);
            }
        }
    }
    pub(crate) fn recv(&self, msg: &mut Message<'_>, len: usize, flags: u32) -> Result<usize> {
        if flags & !(b::MSG_DONTWAIT | b::MSG_PEEK | b::MSG_WAITALL | b::MSG_TRUNC) != 0 {
            return Err(EOPNOTSUPP);
        }
        if len == 0 && self.kind == 1 {
            return Ok(0);
        }
        let _receive = self.receive.lock();
        let mut done = 0;
        let mut timeout = self.native.timeout(false, flags & b::MSG_DONTWAIT != 0);
        let mut s = self.state.lock();
        loop {
            if let Some(packet) = s.rx.first() {
                let available = packet.data.len() - s.rx_offset;
                let count = (len - done).min(available);
                if let Err(e) = msg.write(&packet.data[s.rx_offset..s.rx_offset + count]) {
                    return if done > 0 { Ok(done) } else { Err(e) };
                }
                done += count;
                if self.kind == 2 {
                    msg.set_name(packet.address);
                    if count < available {
                        msg.truncated();
                    }
                    if flags & b::MSG_TRUNC != 0 {
                        done = available;
                    }
                }
                if flags & b::MSG_PEEK == 0 {
                    s.rx_offset += count;
                    if count == available || self.kind == 2 {
                        s.rx.remove(0).unwrap();
                        s.rx_offset = 0;
                        s.credit_return += 1;
                    }
                    self.provider_changed.notify_all();
                }
                if self.kind == 2
                    || flags & b::MSG_PEEK != 0
                    || done == len
                    || flags & b::MSG_WAITALL == 0
                {
                    return Ok(done);
                }
                continue;
            }
            if done > 0
                && (!self.alive(&s)
                    || self.native.error(false) != 0
                    || s.eof
                    || s.shutdown & 1 != 0)
            {
                return Ok(done);
            }
            if !self.alive(&s) {
                return Err(ENETDOWN);
            }
            let error = self.native.error(true);
            if error != 0 {
                return Err(Error::from_errno(error));
            }
            if s.eof || s.shutdown & 1 != 0 {
                return Ok(done);
            }
            if timeout == 0 || (done > 0 && flags & b::MSG_WAITALL == 0) {
                return if done > 0 { Ok(done) } else { Err(EAGAIN) };
            }
            if let Err(e) = self.wait(&mut s, &mut timeout) {
                return if done > 0 { Ok(done) } else { Err(e) };
            }
        }
    }
    pub(crate) fn poll(&self, file: &File, table: &PollTable<'_>) -> u32 {
        table.register_wait(file, &self.changed);
        table.register_wait(file, &self.lease.0.changed);
        let s = self.state.lock();
        let mut mask = 0;
        let alive = self.alive(&s);
        if !alive || self.native.error(false) != 0 {
            mask |= b::POLLERR
        }
        if !alive {
            mask |= b::POLLHUP
        }
        if !s.rx.is_empty() || s.eof || !s.accepted.is_empty() || s.shutdown & 1 != 0 {
            mask |= b::POLLIN | b::POLLRDNORM
        }
        if s.eof && !s.listening {
            mask |= b::POLLRDHUP
        }
        if alive
            && !s.connecting
            && !s.listening
            && (s.connected || self.kind == 2)
            && s.tx_bytes < LIMIT
            && s.requests.len() < REQUESTS - 8
        {
            mask |= b::POLLOUT | b::POLLWRNORM
        }
        mask
    }
}

pub(crate) struct Session {
    namespace: Arc<Namespace>,
    generation: u64,
    _net: NetRef,
}
impl Session {
    pub(crate) fn new(namespace: Arc<Namespace>, net: NetRef) -> Result<Arc<Self>> {
        let mut session = kernel::sync::UniqueArc::new(
            Self {
                namespace: namespace.clone(),
                generation: 0,
                _net: net,
            },
            GFP_KERNEL,
        )?;
        let mut registry = namespace.registry.lock();
        if namespace.live.load(Ordering::Acquire) != 0 {
            return Err(EBUSY);
        }
        let generation = registry.next_generation.checked_add(1).ok_or(EOVERFLOW)?;
        session.generation = generation;
        registry.next_generation = generation;
        namespace.live.store(generation, Ordering::Release);
        Ok(session.into())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if self.generation == 0 {
            return;
        }
        let registry = self.namespace.registry.lock();
        if self.namespace.live.load(Ordering::Acquire) == self.generation {
            self.namespace.live.store(0, Ordering::Release);
            for socket in registry.sockets.iter() {
                socket.abort();
            }
            self.namespace.changed.notify_all();
        }
    }
}
impl Endpoint for Session {
    fn poll(&self, file: &File, table: &PollTable<'_>) -> u32 {
        table.register_wait(file, &self.namespace.changed);
        let registry = self.namespace.registry.lock();
        if self.namespace.live.load(Ordering::Acquire) != self.generation {
            return b::POLLERR | b::POLLHUP;
        }
        for socket in registry.sockets.iter() {
            let s = socket.state.lock();
            if !s.claimed && socket.alive(&s) {
                return b::POLLIN;
            }
        }
        0
    }
    fn ioctl(&self, cmd: u32, arg: usize) -> Result<isize> {
        if cmd != CLAIM {
            return Err(ENOTTY);
        }
        let reserved = FileDescriptorReservation::get_unused_fd_flags(b::O_CLOEXEC)?;
        let registry = self.namespace.registry.lock();
        if self.namespace.live.load(Ordering::Acquire) != self.generation {
            return Err(ENETDOWN);
        }
        for socket in registry.sockets.iter() {
            let mut s = socket.state.lock();
            if s.claimed || !socket.alive(&s) {
                continue;
            }
            UserSlice::new(UserPtr::from_addr(arg), 8)
                .writer()
                .write_slice(&socket.id.to_le_bytes())?;
            let file =
                endpoint_file::create(Arc::new(SocketEndpoint(socket.clone()), GFP_KERNEL)?)?;
            s.claimed = true;
            let fd = reserved.reserved_fd();
            reserved.fd_install(file);
            return Ok(fd as isize);
        }
        Err(EAGAIN)
    }
}
pub(crate) struct SocketEndpoint(pub(crate) Arc<Socket>);
impl Endpoint for SocketEndpoint {
    fn release(&self) {
        self.0.abort();
    }
    fn read(&self, out: &mut UserSliceWriter) -> Result<usize> {
        let socket = &self.0;
        let mut s = socket.state.lock();
        if !socket.alive(&s) {
            return Err(ENETDOWN);
        }
        if let Some(request) = s.requests.iter_mut().find(|r| !r.read) {
            let header = frame(request.op, socket.id, request.id, request.data.len());
            let len = header.len() + request.data.len();
            if out.len() < len {
                return Err(EMSGSIZE);
            }
            out.write_slice(&header)?;
            out.write_slice(&request.data)?;
            request.read = true;
            return Ok(len);
        }
        let op = if s.accept_space {
            ACCEPT
        } else if s.credit_return > 0 {
            CREDIT
        } else if s.app_closed && !s.close_read {
            CLOSE
        } else {
            return Err(EAGAIN);
        };
        let credit = s.credit_return.to_le_bytes();
        let data = if op == CLOSE { &[][..] } else { &credit[..] };
        let header = frame(op, socket.id, 0, data.len());
        let len = header.len() + data.len();
        if out.len() < len {
            return Err(EMSGSIZE);
        }
        out.write_slice(&header)?;
        out.write_slice(data)?;
        match op {
            ACCEPT => s.accept_space = false,
            CLOSE => s.close_read = true,
            _ => {
                s.rx_credit += s.credit_return;
                s.credit_return = 0;
            }
        }
        Ok(len)
    }
    fn write(&self, input: &mut UserSliceReader) -> Result<usize> {
        let len = input.len();
        if !(32..=32 + 24 + PAYLOAD).contains(&len) {
            return Err(EMSGSIZE);
        }
        let mut raw = KVec::new();
        raw.resize(len, 0, GFP_KERNEL)?;
        input.read_slice(&mut raw)?;
        let version = word(&raw[..4])?;
        let op = word(&raw[4..8])?;
        let id = u64::from_le_bytes(raw[8..16].try_into().unwrap());
        let request = u64::from_le_bytes(raw[16..24].try_into().unwrap());
        let size = word(&raw[24..28])? as usize;
        let status = word(&raw[28..32])?;
        if version != VERSION || id != self.0.id || size != len - 32 || status > 4095 {
            return Err(EPROTO);
        }
        let data = &raw[32..];
        let socket = &self.0;
        let mut s = socket.state.lock();
        if !socket.alive(&s) {
            return Err(ENETDOWN);
        }
        if request == 0 {
            if op == RX && size >= 24 {
                if s.rx_credit == 0 {
                    return Err(EAGAIN);
                }
                let address = if socket.kind == 1 && data[..24] == [0; 24] {
                    Address::default()
                } else {
                    Address::from_wire(&data[..24])?
                };
                let packet = Packet {
                    address,
                    data: bytes(&data[24..])?,
                };
                s.rx.push(packet, GFP_KERNEL)?;
                s.rx_credit -= 1;
            } else if op == STATE && size == 4 {
                let state = word(data)?;
                if state & 1 != 0 {
                    s.connected = true;
                    s.connecting = false;
                }
                if state & 4 != 0 {
                    s.eof = true;
                }
                if status != 0 {
                    s.connecting = false;
                    socket.native.set_error(status as i32);
                }
            } else {
                socket.abort_locked(&mut s);
                return Err(EPROTO);
            }
            socket.wake();
            return Ok(len);
        }
        let index = s.requests.iter().position(|r| r.id == request);
        let Some(index) = index else {
            socket.abort_locked(&mut s);
            return Err(EPROTO);
        };
        if !s.requests[index].read || s.requests[index].op != op {
            socket.abort_locked(&mut s);
            return Err(EPROTO);
        }
        let reply = if status == 0 {
            Ok(bytes(data)?)
        } else {
            Err(Error::from_errno(-(status as i32)))
        };
        let r = s.requests.remove(index).unwrap();
        s.tx_bytes -= r.credit;
        if r.synchronous {
            s.reply = Some(reply);
        } else if status != 0 && op != GETNAME && !(op == CONNECT && status == b::EINPROGRESS) {
            socket.native.set_error(status as i32);
            s.connecting = false;
        }
        if op == CONNECT && status == 0 && socket.kind == 2 {
            s.connected = true;
            s.connecting = false;
        }
        socket.wake();
        Ok(len)
    }
    fn poll(&self, file: &File, table: &PollTable<'_>) -> u32 {
        let socket = &self.0;
        table.register_wait(file, &socket.provider_changed);
        table.register_wait(file, &socket.lease.0.changed);
        let s = socket.state.lock();
        if !socket.alive(&s) {
            return b::POLLERR | b::POLLHUP;
        }
        b::POLLOUT
            | if s.requests.iter().any(|r| !r.read)
                || s.accept_space
                || s.credit_return != 0
                || (s.app_closed && !s.close_read)
            {
                b::POLLIN
            } else {
                0
            }
    }
    fn ioctl(&self, cmd: u32, arg: usize) -> Result<isize> {
        if cmd != PUBLISH {
            return Err(ENOTTY);
        }
        let mut info = [0; 56];
        UserSlice::new(UserPtr::from_addr(arg), 56)
            .reader()
            .read_slice(&mut info)?;
        let local = Address::from_wire(&info[..24])?;
        let peer = Address::from_wire(&info[24..48])?;
        let listener = &self.0;
        if local.family() != listener.family || peer.family() != listener.family {
            return Err(EAFNOSUPPORT);
        }
        {
            let s = listener.state.lock();
            if !listener.alive(&s) || s.app_closed {
                return Err(ENETDOWN);
            }
            if !s.listening {
                return Err(EINVAL);
            }
            if s.accepted.len() >= s.backlog {
                return Err(EAGAIN);
            }
        }
        let reserved = FileDescriptorReservation::get_unused_fd_flags(b::O_CLOEXEC)?;
        let child = listener.native.accepted(listener.family)?;
        let socket = crate::accepted_socket(&child);
        {
            let mut s = socket.state.lock();
            s.local = local;
            s.peer = peer;
        }
        info[48..56].copy_from_slice(&socket.id.to_le_bytes());
        UserSlice::new(UserPtr::from_addr(arg), 56)
            .writer()
            .write_slice(&info)?;
        let file = endpoint_file::create(Arc::new(SocketEndpoint(socket), GFP_KERNEL)?)?;
        let mut s = listener.state.lock();
        if !listener.alive(&s) || s.app_closed || s.accepted.len() >= s.backlog {
            drop(s);
            return Err(EAGAIN);
        }
        // Reserve while child/file are still local; their destructors run only
        // after releasing the listener mutex on error.
        if let Err(e) = s.accepted.reserve(1, GFP_KERNEL) {
            drop(s);
            return Err(e.into());
        }
        s.accepted.push(child, GFP_KERNEL)?;
        listener.changed.notify_all();
        let fd = reserved.reserved_fd();
        reserved.fd_install(file);
        Ok(fd as isize)
    }
}
