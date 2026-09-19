// SPDX-License-Identifier: GPL-2.0-only
//! Namespace-scoped netlink delegation. Payloads remain opaque to the kernel.
#![forbid(unsafe_code)]
use crate::{
    endpoint_file::{self, Endpoint, Poll},
    frontend::Deadline,
    linux::NativeSock,
};
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::{
    bindings as b, fs::file::FileDescriptorReservation, prelude::*,
    sync::{poll::PollCondVar, Arc, CondVarTimeoutResult, Mutex},
    uaccess::{UserPtr, UserSlice, UserSliceReader, UserSliceWriter},
};
const MAX_MESSAGE: usize = 65536;
const MAX_BYTES: usize = 256 * 1024;
const MAX_RECORDS: usize = 32;
const MAX_SOCKETS: usize = 256;
const HEADER: usize = 40;
struct Registry {
    next_id: u64,
    sockets: KVec<Arc<Channel>>,
}
#[pin_data]
pub(crate) struct Namespace {
    #[pin] registry: Mutex<Registry>,
    #[pin] changed: PollCondVar,
    lifecycle: Arc<crate::namespace::Lifecycle>,
    count: AtomicUsize,
}
impl Namespace {
    pub(crate) fn new(lifecycle: Arc<crate::namespace::Lifecycle>) -> Result<Arc<Self>> {
        Arc::pin_init(try_pin_init!(Self {
            registry <- kernel::new_mutex!(Registry {
                next_id: 0, sockets: KVec::new(),
            }),
            changed <- kernel::new_poll_condvar!(),
            lifecycle, count: AtomicUsize::new(0),
        }), GFP_KERNEL)
    }
    pub(crate) fn abort_generation(&self) {
        let registry = self.registry.lock();
        for channel in &registry.sockets { channel.abort(&mut channel.state.lock()); }
        self.changed.notify_all();
    }
    pub(crate) fn socket(ns: Arc<Self>, native: NativeSock, generation: u64) -> Result<Arc<Channel>> {
        let mut registry = ns.registry.lock();
        if ns.lifecycle.live() != generation { return Err(ENETDOWN); }
        if ns.count.load(Ordering::Relaxed) >= MAX_SOCKETS { return Err(ENFILE); }
        let id = registry.next_id.checked_add(1).ok_or(EOVERFLOW)?;
        registry.next_id = id;
        ns.count.fetch_add(1, Ordering::Relaxed);
        let lease = Lease(ns.clone());
        let channel = Arc::pin_init(try_pin_init!(Channel {
            native, lease, id, generation,
            state <- kernel::new_mutex!(State {
                claimed: false, dead: false, bytes: 0, requests: KVec::new(),
            }),
            changed <- kernel::new_poll_condvar!(),
        }), GFP_KERNEL)?;
        registry.sockets.push(channel.clone(), GFP_KERNEL)?;
        ns.changed.notify_all();
        Ok(channel)
    }
}
struct Lease(Arc<Namespace>);
impl Drop for Lease {
    fn drop(&mut self) { self.0.count.fetch_sub(1, Ordering::Relaxed); }
}
struct State {
    claimed: bool,
    dead: bool,
    bytes: usize,
    requests: KVec<KVec<u8>>,
}
#[pin_data]
pub(crate) struct Channel {
    native: NativeSock,
    lease: Lease,
    id: u64,
    generation: u64,
    #[pin] state: Mutex<State>,
    #[pin] changed: PollCondVar,
}
impl Channel {
    fn alive(&self, state: &State) -> bool {
        !state.dead && self.lease.0.lifecycle.live() == self.generation
    }
    fn abort(&self, state: &mut State) {
        state.dead = true;
        state.requests.clear();
        state.bytes = 0;
        self.native.set_error(b::ENETDOWN as i32);
        self.native.shutdown(3);
        self.changed.notify_all();
    }
    pub(crate) fn close_app(&self) {
        let mut registry = self.lease.0.registry.lock();
        registry.sockets.retain(|channel| channel.id != self.id);
        self.abort(&mut self.state.lock());
    }
    pub(crate) fn rx_drained(&self) { self.changed.notify_all(); }
    pub(crate) fn send(&self, data: &[u8], context: &[u8; 24], nonblock: bool) -> Result<usize> {
        if data.len() > MAX_MESSAGE { return Err(EMSGSIZE); }
        let deadline = Deadline::new(self.native.timeout(true, nonblock));
        let mut state = self.state.lock();
        loop {
            if !self.alive(&state) { return Err(ENETDOWN); }
            if state.requests.len() < MAX_RECORDS && state.bytes + HEADER + data.len() <= MAX_BYTES { break; }
            let remaining = deadline.remaining();
            if remaining == 0 { return Err(EAGAIN); }
            if let CondVarTimeoutResult::Signal { .. } =
                self.changed.wait_interruptible_timeout(&mut state, remaining)
            { return Err(EINTR); }
        }
        let mut record = KVec::new();
        record.extend_from_slice(&1u32.to_le_bytes(), GFP_KERNEL)?;
        record.extend_from_slice(&1u32.to_le_bytes(), GFP_KERNEL)?; // request
        record.extend_from_slice(&(data.len() as u32).to_le_bytes(), GFP_KERNEL)?;
        record.extend_from_slice(&0u32.to_le_bytes(), GFP_KERNEL)?;
        record.extend_from_slice(context, GFP_KERNEL)?;
        record.extend_from_slice(data, GFP_KERNEL)?;
        let len = record.len();
        state.requests.push(record, GFP_KERNEL)?;
        state.bytes += len;
        self.changed.notify_all();
        Ok(data.len())
    }
    pub(crate) fn app_poll(&self, poll: &Poll<'_>) -> u32 {
        poll.register(&self.changed);
        let state = self.state.lock();
        if !self.alive(&state) { return b::POLLERR | b::POLLHUP; }
        if state.requests.len() < MAX_RECORDS && state.bytes + HEADER + MAX_MESSAGE <= MAX_BYTES {
            b::POLLOUT | b::POLLWRNORM
        } else { 0 }
    }
}
impl Namespace {
    pub(crate) fn publish(&self, generation: u64, input: &mut UserSliceReader) -> Result<usize> {
        let len = input.len();
        if len < 8 || len > MAX_MESSAGE + 8 { return Err(EMSGSIZE); }
        let mut header = [0; 8];
        input.read_slice(&mut header)?;
        let group = u32::from_le_bytes(header[..4].try_into().unwrap());
        if group == 0 || group > 128 || header[4..] != [0; 4] { return Err(EINVAL); }
        let mut data = KVec::new();
        data.resize(len - 8, 0, GFP_KERNEL)?;
        input.read_slice(&mut data)?;
        let registry = self.registry.lock();
        if self.lifecycle.live() != generation { return Err(ENETDOWN); }
        for channel in &registry.sockets {
            let state = channel.state.lock();
            if channel.alive(&state) {
                match channel.native.netlink_deliver(&data, group) {
                    Ok(()) => {},
                    Err(e) if e == ENOENT => {}, // Not subscribed.
                    // A multicast cannot block all listeners behind one slow
                    // reader. Signal loss; consumers must resynchronize.
                    Err(_) => channel.native.netlink_loss(),
                }
            }
        }
        Ok(len)
    }
    pub(crate) fn poll(&self, generation: u64, poll: &Poll<'_>) -> u32 {
        poll.register(&self.changed);
        if self.lifecycle.live() != generation { return b::POLLHUP | b::POLLERR; }
        let registry = self.registry.lock();
        for channel in &registry.sockets {
            let state = channel.state.lock();
            if !state.claimed && channel.alive(&state) { return b::POLLIN; }
        }
        0
    }
    pub(crate) fn claim(&self, generation: u64, arg: usize) -> Result<isize> {
        let reserved = FileDescriptorReservation::get_unused_fd_flags(b::O_CLOEXEC)?;
        let registry = self.registry.lock();
        if self.lifecycle.live() != generation { return Err(ENETDOWN); }
        for channel in &registry.sockets {
            let mut state = channel.state.lock();
            if state.claimed || !channel.alive(&state) { continue; }
            UserSlice::new(UserPtr::from_addr(arg), 8).writer().write_slice(&channel.id.to_le_bytes())?;
            let file = endpoint_file::create(Arc::new(Claimed(channel.clone()), GFP_KERNEL)?)?;
            state.claimed = true;
            let fd = reserved.reserved_fd();
            reserved.fd_install(file);
            return Ok(fd as isize);
        }
        Err(EAGAIN)
    }
}
struct Claimed(Arc<Channel>);
impl Endpoint for Claimed {
    fn release(&self) { self.0.abort(&mut self.0.state.lock()); }
    fn read(&self, out: &mut UserSliceWriter) -> Result<usize> {
        let channel = &self.0;
        let mut state = channel.state.lock();
        if !channel.alive(&state) { return Err(ENETDOWN); }
        let record = state.requests.first().ok_or(EAGAIN)?;
        if out.len() < record.len() { return Err(EMSGSIZE); }
        out.write_slice(record)?;
        let len = record.len();
        state.requests.remove(0).unwrap();
        state.bytes -= len;
        channel.changed.notify_all();
        Ok(len)
    }
    fn write(&self, input: &mut UserSliceReader) -> Result<usize> {
        let len = input.len();
        if len < 8 || len > MAX_MESSAGE + 8 { return Err(EMSGSIZE); }
        let mut header = [0; 8];
        input.read_slice(&mut header)?;
        let group = u32::from_le_bytes(header[..4].try_into().unwrap());
        if header[4..] != [0; 4] || group > 128 { return Err(EINVAL); }
        let mut data = KVec::new();
        data.resize(len - 8, 0, GFP_KERNEL)?;
        input.read_slice(&mut data)?;
        let channel = &self.0;
        let state = channel.state.lock();
        if !channel.alive(&state) { return Err(ENETDOWN); }
        channel.native.netlink_deliver(&data, group)?;
        Ok(len)
    }
    fn poll(&self, poll: &Poll<'_>) -> u32 {
        let channel = &self.0;
        poll.register(&channel.changed);
        let state = channel.state.lock();
        if !channel.alive(&state) { return b::POLLERR | b::POLLHUP; }
        let mut mask = if !state.requests.is_empty() { b::POLLIN } else { 0 };
        mask |= channel.native.netlink_writable();
        mask
    }
}
