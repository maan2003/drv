// SPDX-License-Identifier: GPL-2.0-only
//! ABI6 frontend state. Linux owns native object mechanics; Netstack3 owns TCP/IP.
#![forbid(unsafe_code)]
use crate::{
    connection::{ConnectAttempt, Names},
    endpoint_file::{self, Endpoint},
    linux::{AcceptTarget, Accepted, Address, Message, NativeSock, NetRef},
};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use kernel::{
    bindings as b,
    fs::file::FileDescriptorReservation,
    prelude::*,
    sync::{poll::PollCondVar, Arc, CondVarTimeoutResult, Mutex},
    uaccess::{UserPtr, UserSlice, UserSliceReader, UserSliceWriter},
};
const VERSION: u32 = 6;
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
const CONNECTION: u32 = 14;
const ACTIVATE: u32 = 13;
const CLAIM: u32 = 0x8008B301;
const PUBLISH: u32 = 0xC038B302;
const READ_CONTROL: u32 = 0x8080B303;

/// An absolute monotonic budget. Mutex reacquisition and processing count,
/// unlike carrying schedule_timeout's sleep-only remainder between waits.
struct Deadline {
    start: kernel::time::Instant<kernel::time::Monotonic>,
    ticks: usize,
}
impl Deadline {
    fn new(ticks: usize) -> Self {
        Self {
            start: kernel::time::Instant::now(),
            ticks,
        }
    }
    fn remaining(&self) -> usize {
        if self.ticks == kernel::task::MAX_SCHEDULE_TIMEOUT as usize {
            return self.ticks;
        }
        let micros = self.start.elapsed().as_micros_ceil().max(0) as u64;
        let hz = kernel::time::msecs_to_jiffies(1000) as u64;
        let elapsed = (micros / 1_000_000)
            .saturating_mul(hz)
            .saturating_add((micros % 1_000_000).saturating_mul(hz) / 1_000_000);
        self.ticks
            .saturating_sub(elapsed.min(usize::MAX as u64) as usize)
    }
}

fn bytes(data: &[u8]) -> Result<KVec<u8>> {
    let mut out = KVec::new();
    out.extend_from_slice(data, GFP_KERNEL)?;
    Ok(out)
}
fn word(data: &[u8]) -> Result<u32> {
    Ok(u32::from_le_bytes(data.try_into().map_err(|_| EPROTO)?))
}
fn frame(op: u32, request: u64, len: usize) -> [u8; 24] {
    let mut header = [0; 24];
    header[..4].copy_from_slice(&VERSION.to_le_bytes());
    header[4..8].copy_from_slice(&op.to_le_bytes());
    header[8..16].copy_from_slice(&request.to_le_bytes());
    header[16..20].copy_from_slice(&(len as u32).to_le_bytes());
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
                    claimed,opened:claimed,connected:claimed,listening:false,
                    dead:false,app_closed:false,close_read:false,eof:false,shutdown:0,
                    local,peer:Address::default(),next_request:0,requests:KVec::new(),
                    attempt:None,tx_bytes:0,tx:KVec::new(),tx_offset:0,tx_tail:0,tx_head:0,rx:KVec::new(),rx_offset:0,rx_bytes:0,
                    accepted:KVec::new(),backlog:0,accept_space:false,
                }),
                changed <- kernel::new_poll_condvar!(),
                provider_changed <- kernel::new_poll_condvar!(),
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
    reply: Option<Result<KVec<u8>>>,
}
struct Packet {
    address: Address,
    data: KVec<u8>,
}
struct SocketState {
    claimed: bool,
    opened: bool,
    connected: bool,
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
    attempt: Option<ConnectAttempt>,
    tx_bytes: usize,
    tx: KVec<Packet>,
    tx_offset: usize,
    tx_tail: u64,
    tx_head: u64,
    rx: KVec<Packet>,
    rx_offset: usize,
    rx_bytes: usize,
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
        s.tx.clear();
        s.tx_offset = 0;
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
        s.rx_bytes = 0;
        s.shutdown |= 2; // atomic producer seal: tx_tail cannot advance after this point
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
    ) -> Result<u64> {
        if !self.alive(s) {
            return Err(ENETDOWN);
        }
        if s.requests.len() >= REQUESTS {
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
                reply: None,
            },
            GFP_KERNEL,
        )?;
        s.next_request = id;
        self.provider_changed.notify_all();
        Ok(id)
    }
    // OPEN precedes later control/data preparation in the endpoint control lane.
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
        self.submit(&mut s, OPEN, data, false)?;
        s.opened = true;
        Ok(())
    }
    fn wait(
        &self,
        s: &mut kernel::sync::lock::Guard<'_, SocketState, kernel::sync::lock::mutex::MutexBackend>,
        timeout: &Deadline,
    ) -> Result {
        let remaining = timeout.remaining();
        if remaining == 0 {
            return Err(EAGAIN);
        }
        match self.changed.wait_interruptible_timeout(s, remaining) {
            CondVarTimeoutResult::Signal { .. } => Err(ERESTARTSYS),
            CondVarTimeoutResult::Timeout => Err(EAGAIN),
            CondVarTimeoutResult::Woken { .. } => Ok(()),
        }
    }

    fn call(&self, op: u32, mut data: KVec<u8>, timeout: &Deadline) -> Result<KVec<u8>> {
        self.open()?;
        let mut s = self.state.lock();
        while s.requests.iter().any(|r| r.op != OPEN) {
            if !self.alive(&s) {
                return Err(ENETDOWN);
            }
            if timeout.remaining() == 0 {
                return Err(EAGAIN);
            }
            self.wait(&mut s, timeout)?;
        }
        let shutdown = if op == SHUTDOWN {
            let how = word(&data)?;
            data.extend_from_slice(&s.tx_tail.to_le_bytes(), GFP_KERNEL)?;
            Some(how)
        } else {
            None
        };
        let id = self.submit(&mut s, op, data, true)?;
        if let Some(how) = shutdown {
            s.shutdown |= how;
        }

        loop {
            if !self.alive(&s) {
                return Err(ENETDOWN);
            }
            let index = s.requests.iter().position(|r| r.id == id).ok_or(EPROTO)?;
            if let Some(reply) = s.requests[index].reply.take() {
                s.requests.remove(index).unwrap();
                self.wake();
                return reply;
            }
            if let Err(error) = self.wait(&mut s, timeout) {
                // A completion racing interruption wins; it has already committed.
                if s.requests.iter().any(|r| r.id == id && r.reply.is_some()) {
                    continue;
                }
                if let Some(index) = s.requests.iter().position(|r| r.id == id) {
                    if s.requests[index].read || op == SHUTDOWN {
                        self.abort_locked(&mut s);
                    } else {
                        s.requests.remove(index).unwrap();
                        self.wake();
                    }
                }
                return Err(error);
            }
        }
    }
    pub(crate) fn bind(&self, a: Address) -> Result {
        let timeout = Deadline::new(kernel::time::msecs_to_jiffies(10000));
        if a.family() != self.family {
            return Err(EAFNOSUPPORT);
        }
        self.open()?;

        self.call(BIND, bytes(&a.0)?, &timeout)?;
        Ok(())
    }
    pub(crate) fn listen(&self, backlog: i32) -> Result {
        let timeout = Deadline::new(kernel::time::msecs_to_jiffies(10000));
        if self.kind != 1 {
            return Err(EOPNOTSUPP);
        }
        self.open()?;
        let value = (backlog.clamp(0, SOCKETS as i32) as u32).to_le_bytes();

        self.call(LISTEN, bytes(&value)?, &timeout)?;
        Ok(())
    }
    pub(crate) fn connect(&self, a: Address, flags: i32) -> Result {
        let timeout = Deadline::new(
            self.native
                .timeout(true, self.kind == 1 && flags & b::O_NONBLOCK as i32 != 0),
        );
        if a.family() != self.family {
            return Err(EAFNOSUPPORT);
        }
        self.open()?;

        let mut s = self.state.lock();
        loop {
            if !self.alive(&s) {
                return Err(ENETDOWN);
            }
            if s.connected && self.kind == 1 {
                return Err(EISCONN);
            }
            if s.attempt
                .as_ref()
                .is_some_and(ConnectAttempt::blocks_replacement)
            {
                return Err(EALREADY);
            }
            if !s.requests.iter().any(|r| r.op != OPEN) {
                break;
            }
            if timeout.remaining() == 0 {
                return Err(EAGAIN);
            }
            self.wait(&mut s, &timeout)?;
        }
        let id = self.submit(&mut s, CONNECT, bytes(&a.0)?, false)?;
        let blocking = self.kind == 2 || flags & b::O_NONBLOCK as i32 == 0;
        ConnectAttempt::begin(&mut s.attempt, id, blocking)?;
        if !blocking {
            return Err(EINPROGRESS);
        }
        loop {
            if !self.alive(&s) {
                return Err(ENETDOWN);
            }
            if let Some(result) = s.attempt.as_mut().ok_or(EPROTO)?.claim(id)? {
                self.wake();
                return result.map(|_| ());
            }
            if let Err(error) = self.wait(&mut s, &timeout) {
                if !self.alive(&s) {
                    return Err(ENETDOWN);
                }
                let result = s
                    .attempt
                    .as_mut()
                    .ok_or(EPROTO)?
                    .finish_wait(id, if error == EAGAIN { EINPROGRESS } else { error });
                self.wake();
                return result.map(|_| ());
            }
        }
    }
    pub(crate) fn name(&self, peer: bool) -> Result<Address> {
        let s = self.state.lock();
        if !self.alive(&s) {
            return Err(ENETDOWN);
        }
        if peer {
            if !s.connected {
                return Err(ENOTCONN);
            }
            Ok(s.peer)
        } else {
            Ok(s.local)
        }
    }
    pub(crate) fn shutdown(&self, how: i32) -> Result {
        let timeout = Deadline::new(self.native.timeout(true, false));
        if !(0..=2).contains(&how) {
            return Err(EINVAL);
        }
        let data = bytes(&((how + 1) as u32).to_le_bytes())?;

        let reply = self.call(SHUTDOWN, data, &timeout)?;
        if !reply.is_empty() {
            return Err(EPROTO);
        }
        self.state.lock().shutdown |= (how + 1) as u32;
        self.native.shutdown(how + 1);
        self.changed.notify_all();
        Ok(())
    }
    pub(crate) fn accept(&self, target: &mut AcceptTarget<'_>, flags: i32) -> Result {
        let timeout = Deadline::new(
            self.native
                .timeout(false, flags & b::O_NONBLOCK as i32 != 0),
        );
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
            if timeout.remaining() == 0 {
                return Err(EAGAIN);
            }
            self.wait(&mut s, &timeout)?;
        }
    }
    pub(crate) fn send(&self, msg: &mut Message<'_>, len: usize) -> Result<usize> {
        msg.validate_control(self.kind == 2)?;
        let flags = msg.flags();
        let timeout = Deadline::new(self.native.timeout(true, flags & b::MSG_DONTWAIT != 0));
        if flags & !(b::MSG_DONTWAIT | b::MSG_NOSIGNAL | b::MSG_MORE | b::MSG_BATCH) != 0 {
            return Err(EOPNOTSUPP);
        }
        if self.kind == 1 && len == 0 {
            return Ok(0);
        }
        if self.kind == 2 && len > PAYLOAD {
            return Err(EMSGSIZE);
        }
        if self.state.lock().shutdown & 2 != 0 {
            if flags & b::MSG_NOSIGNAL == 0 {
                crate::linux::sigpipe();
            }
            return Err(EPIPE);
        }
        let dest = msg.name()?.unwrap_or_default();
        if dest.family() != 0 && dest.family() != self.family {
            return Err(EAFNOSUPPORT);
        }
        if self.kind == 2 && dest.family() == 0 && !self.state.lock().connected {
            return Err(EDESTADDRREQ);
        }
        self.open()?;
        if self.kind == 1 && !self.state.lock().connected {
            return Err(ENOTCONN);
        }
        let mut done = 0;

        if self.kind == 2 {
            let mut s = self.state.lock();
            loop {
                if !self.alive(&s) {
                    return Err(ENETDOWN);
                }
                if s.local.0[2..4] != [0; 2] {
                    break;
                }
                let error = self.native.error(true);
                if error != 0 {
                    return Err(Error::from_errno(error));
                }
                if !s.requests.iter().any(|r| r.op != OPEN) {
                    self.submit(&mut s, ACTIVATE, KVec::new(), false)?;
                }
                if timeout.remaining() == 0 {
                    return Err(EAGAIN);
                }
                self.wait(&mut s, &timeout)?;
            }
        }
        loop {
            let count = (len - done).min(PAYLOAD);
            let mut s = self.state.lock();
            if !self.alive(&s) {
                return if done > 0 { Ok(done) } else { Err(ENETDOWN) };
            }
            if s.shutdown & 2 != 0 {
                return if done > 0 { Ok(done) } else { Err(EPIPE) };
            }
            if s.tx.len() >= 32 || s.tx_bytes + count.max(1) > LIMIT {
                if done > 0 {
                    return Ok(done);
                }
                if timeout.remaining() == 0 {
                    return Err(EAGAIN);
                }
                self.wait(&mut s, &timeout)?;
                continue;
            }
            // Reserve both allocations before consuming the iterator. Holding
            // the socket mutex prevents admission from changing after the copy.
            let allocation = (|| -> Result<KVec<u8>> {
                s.tx.reserve(1, GFP_KERNEL)?;
                let mut data = KVec::new();
                data.resize(count, 0, GFP_KERNEL)?;
                Ok(data)
            })();
            let mut data = match allocation {
                Ok(data) => data,
                Err(error) => return if done > 0 { Ok(done) } else { Err(error) },
            };
            if let Err(e) = msg.read(&mut data) {
                return if done > 0 { Ok(done) } else { Err(e) };
            }
            let step = if self.kind == 1 { count as u64 } else { 1 };
            let Some(tail) = s.tx_tail.checked_add(step) else {
                msg.rollback_read();
                return if done > 0 { Ok(done) } else { Err(EOVERFLOW) };
            };
            // Snapshot the UDP destination at admission; later CONNECT must
            // never retarget already-owned datagrams.
            let address = if self.kind == 2 && dest.family() == 0 {
                s.peer
            } else {
                dest
            };
            s.tx.push(Packet { address, data }, GFP_KERNEL)?;
            s.tx_bytes += count.max(1);
            s.tx_tail = tail;
            self.provider_changed.notify_all();
            msg.commit_read();
            done += count;
            if done == len {
                return Ok(done);
            }
        }
    }
    pub(crate) fn recv(&self, msg: &mut Message<'_>, len: usize, flags: u32) -> Result<usize> {
        let timeout = Deadline::new(self.native.timeout(false, flags & b::MSG_DONTWAIT != 0));
        if flags & !(b::MSG_DONTWAIT | b::MSG_PEEK | b::MSG_WAITALL | b::MSG_TRUNC) != 0 {
            return Err(EOPNOTSUPP);
        }
        if len == 0 && self.kind == 1 {
            return Ok(0);
        }
        let mut done = 0;

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
                        let packet = s.rx.remove(0).unwrap();
                        s.rx_bytes -= packet.data.len().max(1);
                        s.rx_offset = 0;
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
            if timeout.remaining() == 0 || (done > 0 && flags & b::MSG_WAITALL == 0) {
                return if done > 0 { Ok(done) } else { Err(EAGAIN) };
            }
            if let Err(e) = self.wait(&mut s, &timeout) {
                return if done > 0 { Ok(done) } else { Err(e) };
            }
        }
    }
    pub(crate) fn poll(&self, poll: &endpoint_file::Poll<'_>) -> u32 {
        poll.register(&self.changed);
        poll.register(&self.lease.0.changed);
        let s = self.state.lock();
        let mut mask = 0;
        let alive = self.alive(&s);
        if !alive || self.native.error(false) != 0 {
            mask |= b::POLLERR
        }
        if !alive {
            // Terminal I/O cannot block, including accept on a dead listener.
            // Report operation readiness as well as the unconditional error/HUP
            // bits so event loops can dispatch the operation and observe ENETDOWN.
            mask |= b::POLLHUP | b::POLLIN | b::POLLRDNORM | b::POLLOUT | b::POLLWRNORM
        }
        // SO_ERROR is consumable by another caller; terminal connect readiness
        // belongs to the retained attempt, not the native error slot.
        if alive && self.kind == 1 && s.attempt.as_ref().is_some_and(ConnectAttempt::failed) {
            mask |= b::POLLOUT | b::POLLWRNORM | b::POLLHUP;
        }
        if !s.rx.is_empty() || s.eof || !s.accepted.is_empty() || s.shutdown & 1 != 0 {
            mask |= b::POLLIN | b::POLLRDNORM
        }
        if s.eof && !s.listening {
            mask |= b::POLLRDHUP
        }
        if alive
            && !s.attempt.as_ref().is_some_and(ConnectAttempt::pending)
            && !s.listening
            && !s.requests.iter().any(|r| r.op == ACTIVATE)
            && (s.connected || self.kind == 2)
            && s.tx_bytes < LIMIT
            && s.tx.len() < 32
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
    fn poll(&self, poll: &endpoint_file::Poll<'_>) -> u32 {
        poll.register(&self.namespace.changed);
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
impl SocketEndpoint {
    fn read_control(&self, out: &mut UserSliceWriter) -> Result<usize> {
        let socket = &self.0;
        let mut s = socket.state.lock();
        if !socket.alive(&s) {
            return Err(ENETDOWN);
        }
        if let Some(request) = s.requests.iter_mut().find(|r| !r.read) {
            let header = frame(request.op, request.id, request.data.len());
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
        } else if s.app_closed && !s.close_read {
            CLOSE
        } else {
            return Err(EAGAIN);
        };
        let seal = s.tx_tail.to_le_bytes();
        let data = if op == CLOSE { &seal[..] } else { &[][..] };
        let header = frame(op, 0, data.len());
        let len = header.len() + data.len();
        if out.len() < len {
            return Err(EMSGSIZE);
        }
        out.write_slice(&header)?;
        out.write_slice(data)?;
        match op {
            ACCEPT => s.accept_space = false,
            _ => s.close_read = true,
        }
        Ok(len)
    }
}
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
        {
            let Some(packet) = s.tx.first() else {
                return Err(EAGAIN);
            };
            if out.len() < 48 {
                return Err(EMSGSIZE);
            }
            let remaining = packet.data.len() - s.tx_offset;
            let count = remaining.min(out.len() - 48);
            if socket.kind == 2 && count != remaining {
                return Err(EMSGSIZE);
            }
            if socket.kind == 1 && count == 0 {
                return Err(EMSGSIZE);
            }
            let header = frame(SEND, 0, 24 + count);
            out.write_slice(&header)?;
            out.write_slice(&packet.address.0)?;
            out.write_slice(&packet.data[s.tx_offset..s.tx_offset + count])?;
            // Transaction commits only after every user copy succeeds.
            s.tx_offset += count;
            s.tx_head += if socket.kind == 1 { count as u64 } else { 1 };
            if count == remaining {
                let packet = s.tx.remove(0).unwrap();
                s.tx_bytes -= packet.data.len().max(1);
                s.tx_offset = 0;
            }
            socket.changed.notify_all();
            return Ok(48 + count);
        }
    }

    fn write(&self, input: &mut UserSliceReader) -> Result<usize> {
        let len = input.len();
        if !(24..=24 + 24 + PAYLOAD).contains(&len) {
            return Err(EMSGSIZE);
        }
        let mut raw = KVec::new();
        raw.resize(len, 0, GFP_KERNEL)?;
        input.read_slice(&mut raw)?;
        let version = word(&raw[..4])?;
        let op = word(&raw[4..8])?;
        let request = u64::from_le_bytes(raw[8..16].try_into().unwrap());
        let size = word(&raw[16..20])? as usize;
        let status = word(&raw[20..24])?;
        if version != VERSION || size != len - 24 || status > 4095 {
            return Err(EPROTO);
        }
        let data = &raw[24..];
        let socket = &self.0;
        let mut s = socket.state.lock();
        if !socket.alive(&s) {
            return Err(ENETDOWN);
        }
        if request == 0 {
            if op == RX && size >= 24 && status == 0 {
                if s.rx.len() >= 32 || s.rx_bytes + (size - 24).max(1) > LIMIT {
                    return Err(EAGAIN);
                }
                let address = if socket.kind == 1 && data[..24] == [0; 24] {
                    Address::default()
                } else {
                    Address::from_wire(&data[..24])?
                };
                if socket.kind == 2 && address.family() != socket.family {
                    return Err(EPROTO);
                }
                let packet = Packet {
                    address,
                    data: bytes(&data[24..])?,
                };
                if s.shutdown & 1 != 0 || s.app_closed {
                    return Ok(len);
                }
                s.rx.push(packet, GFP_KERNEL)?;
                s.rx_bytes += (size - 24).max(1);
            } else if op == STATE && size == 4 && word(data)? & !4 == 0 {
                if word(data)? & 4 != 0 {
                    s.eof = true;
                }
                if status != 0 {
                    socket.native.set_error(status as i32);
                }
            } else {
                socket.abort_locked(&mut s);
                return Err(EPROTO);
            }
            socket.wake();
            return Ok(len);
        }
        // Connection completion is a retained attempt outcome, not SO_ERROR
        // and not a reply to the already-retired CONNECT acknowledgement.
        if op == CONNECTION {
            let result = (|| -> Result<Names> {
                if status != 0 {
                    return if size == 0 {
                        Err(Error::from_errno(-(status as i32)))
                    } else {
                        Err(EPROTO)
                    };
                }
                if size != 48 {
                    return Err(EPROTO);
                }
                let local = Address::from_wire(&data[..24])?;
                let peer = Address::from_wire(&data[24..])?;
                if local.family() != socket.family || peer.family() != socket.family {
                    return Err(EPROTO);
                }
                Ok(Names { local, peer })
            })();
            if (status == 0 && result.is_err()) || (status != 0 && size != 0) {
                socket.abort_locked(&mut s);
                return Err(EPROTO);
            }
            if s.attempt
                .as_mut()
                .ok_or(EPROTO)
                .and_then(|a| a.complete(request, result))
                .is_err()
            {
                socket.abort_locked(&mut s);
                return Err(EPROTO);
            }
            match result {
                Ok(names) => {
                    s.local = names.local;
                    s.peer = names.peer;
                    s.connected = true;
                }
                Err(_) => socket.native.set_error(status as i32),
            }
            socket.wake();
            return Ok(len);
        }
        let Some(index) = s.requests.iter().position(|r| r.id == request) else {
            socket.abort_locked(&mut s);
            return Err(EPROTO);
        };
        if !s.requests[index].read
            || s.requests[index].op != op
            || s.requests[index].reply.is_some()
            || s.requests[..index].iter().any(|r| r.reply.is_none())
        {
            socket.abort_locked(&mut s);
            return Err(EPROTO);
        }
        // Validate the matched operation completely before committing metadata,
        // retiring accounting, publishing a result, or waking any waiter.
        let validated = (|| -> Result<Option<(Address, Option<Address>)>> {
            if status != 0 {
                return if size == 0 { Ok(None) } else { Err(EPROTO) };
            }
            match op {
                OPEN | BIND | LISTEN | ACTIVATE => {
                    let local = Address::from_wire(data)?;
                    if local.family() != socket.family {
                        return Err(EPROTO);
                    }
                    if op == ACTIVATE && local.0[2..4] == [0; 2] {
                        return Err(EPROTO);
                    }
                    Ok(Some((local, None)))
                }
                CONNECT => {
                    if size != 48 {
                        return Err(EPROTO);
                    }
                    let local = Address::from_wire(&data[..24])?;
                    let peer = Address::from_wire(&data[24..])?;
                    if local.family() != socket.family || peer.family() != socket.family {
                        return Err(EPROTO);
                    }
                    Ok(Some((local, Some(peer))))
                }
                SHUTDOWN if size == 0 => {
                    let request = &s.requests[index].data;
                    if request.len() != 12 {
                        return Err(EPROTO);
                    }
                    let how = word(&request[..4])?;
                    let seal = u64::from_le_bytes(request[4..].try_into().unwrap());
                    if how & 2 != 0 && s.tx_head != seal {
                        return Err(EPROTO);
                    }
                    Ok(None)
                }
                _ => Err(EPROTO),
            }
        })();
        let names = match validated {
            Ok(names) => names,
            Err(error) => {
                socket.abort_locked(&mut s);
                return Err(error);
            }
        };
        let reply = if status == 0 {
            Ok(bytes(data)?)
        } else {
            Err(Error::from_errno(-(status as i32)))
        };
        if op == CONNECT {
            let result = if status == 0 {
                Ok(Names {
                    local: names.ok_or(EPROTO)?.0,
                    peer: names.ok_or(EPROTO)?.1.ok_or(EPROTO)?,
                })
            } else {
                Err(Error::from_errno(-(status as i32)))
            };
            if s.attempt
                .as_mut()
                .ok_or(EPROTO)
                .and_then(|a| a.acknowledge(request, socket.kind == 1, result))
                .is_err()
            {
                socket.abort_locked(&mut s);
                return Err(EPROTO);
            }
            if socket.kind == 2 {
                s.connected = result.is_ok();
            }
        }
        if let Some((local, peer)) = names {
            s.local = local;
            if let Some(peer) = peer {
                s.peer = peer;
            }
        }
        if op == LISTEN && status == 0 {
            s.backlog = word(&s.requests[index].data)?.max(1) as usize;
            s.listening = true;
            s.accept_space = true;
        }
        if matches!(op, OPEN | SHUTDOWN) && status != 0 {
            socket.abort_locked(&mut s);
            return Ok(len);
        }
        if s.requests[index].synchronous {
            s.requests[index].reply = Some(reply);
        } else {
            s.requests.remove(index).unwrap();
            if status != 0 {
                socket.native.set_error(status as i32);
            }
        }
        socket.wake();
        Ok(len)
    }
    fn poll(&self, poll: &endpoint_file::Poll<'_>) -> u32 {
        let socket = &self.0;
        poll.register(&socket.provider_changed);
        poll.register(&socket.lease.0.changed);
        let s = socket.state.lock();
        if !socket.alive(&s) {
            return b::POLLERR | b::POLLHUP;
        }
        (if s.rx.len() < 32 && s.rx_bytes < LIMIT {
            b::POLLOUT
        } else {
            0
        }) | if s.requests.iter().any(|r| !r.read)
            || s.accept_space
            || !s.tx.is_empty()
            || (s.app_closed && !s.close_read)
        {
            b::POLLIN
        } else {
            0
        }
    }
    fn ioctl(&self, cmd: u32, arg: usize) -> Result<isize> {
        if cmd == READ_CONTROL {
            return self
                .read_control(&mut UserSlice::new(UserPtr::from_addr(arg), 128).writer())
                .map(|n| n as isize);
        }
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
