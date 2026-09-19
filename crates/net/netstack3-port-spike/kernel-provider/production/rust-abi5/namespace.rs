// SPDX-License-Identifier: GPL-2.0-only
//! One namespace generation owns all socket, metadata and control transports.
#![forbid(unsafe_code)]
use crate::{endpoint_file::{self, Endpoint, Poll}, frontend::{Deadline, Namespace}};
use core::sync::atomic::{AtomicU64, Ordering};
use kernel::{
    bindings as b, fs::file::FileDescriptorReservation, prelude::*,
    sync::{poll::PollCondVar, Arc, CondVarTimeoutResult, Mutex},
    uaccess::{UserPtr, UserSlice, UserSliceReader, UserSliceWriter},
};

const CLAIM: u32 = 0x8008B501;
const READY: u32 = 0xB502;
const CLAIM_CONTROL: u32 = 0xB503;
const REVOKE: u32 = 0xB505;
const MAX_NAMESPACES: usize = 64;
const MAX_CONTROLS: usize = 32;
const MAX_REPLY: usize = 4096;
const TIMEOUT_MS: u32 = 10000;

#[derive(Clone, Copy, PartialEq)]
enum Owner { Unassigned, Explicit, Lazy }
#[derive(Clone, Copy, PartialEq)]
enum Phase { Idle, Pending, Serving, Ready, Dead }
struct State {
    owner: Owner,
    phase: Phase,
    generation: u64,
    controls: KVec<Arc<Control>>,
}
#[pin_data]
pub(crate) struct Lifecycle {
    #[pin] state: Mutex<State>,
    #[pin] changed: PollCondVar,
    live: AtomicU64,
}
impl Lifecycle {
    pub(crate) fn new() -> Result<Arc<Self>> {
        Arc::pin_init(try_pin_init!(Self {
            state <- kernel::new_mutex!(State {
                owner: Owner::Unassigned, phase: Phase::Idle, generation: 0,
                controls: KVec::new(),
            }),
            changed <- kernel::new_poll_condvar!(),
            live: AtomicU64::new(0),
        }), GFP_KERNEL)
    }
    pub(crate) fn live(&self) -> u64 { self.live.load(Ordering::Acquire) }
    fn stop(&self, state: &mut State, dead: bool) {
        self.live.store(0, Ordering::Release);
        state.phase = if dead { Phase::Dead } else { Phase::Idle };
        for control in &state.controls { control.cancel(ENETDOWN); }
        state.controls.clear();
        self.changed.notify_all();
    }
    /// Opaque interface requests. Linux supplies authenticated authority and
    /// owns marshalling/waiting; the worker owns interpretation and configuration.
    pub(crate) fn control(&self, generation: u64, data: [u8; 48]) -> Result<KVec<u8>> {
        let control = Arc::pin_init(try_pin_init!(Control {
            data,
            state <- kernel::new_mutex!(ControlState {
                claimed: false, read: false, reply: None,
            }),
            changed <- kernel::new_poll_condvar!(),
        }), GFP_KERNEL)?;
        {
            let mut state = self.state.lock();
            if self.live() != generation || state.phase != Phase::Ready { return Err(ENETDOWN); }
            if state.controls.len() >= MAX_CONTROLS { return Err(ENOBUFS); }
            state.controls.push(control.clone(), GFP_KERNEL)?;
        }
        self.changed.notify_all();
        let result = control.wait();
        self.state.lock().controls.retain(|old| !core::ptr::eq(&**old, &*control));
        result
    }
}

struct Registry { connected: bool, namespaces: KVec<Arc<Namespace>> }
#[pin_data]
pub(crate) struct Broker {
    #[pin] registry: Mutex<Registry>,
    #[pin] changed: PollCondVar,
}
impl Broker {
    pub(crate) fn new() -> Result<Arc<Self>> {
        Arc::pin_init(try_pin_init!(Self {
            registry <- kernel::new_mutex!(Registry { connected: false, namespaces: KVec::new() }),
            changed <- kernel::new_poll_condvar!(),
        }), GFP_KERNEL)
    }
    pub(crate) fn open(self: &Arc<Self>) -> Result<Arc<Provisioner>> {
        let owner = Arc::new(Provisioner(self.clone()), GFP_KERNEL)?;
        let mut registry = self.registry.lock();
        if registry.connected { return Err(EBUSY); }
        registry.connected = true;
        Ok(owner)
    }
    pub(crate) fn register(&self, ns: Arc<Namespace>) -> Result<Arc<Provider>> {
        let _registry = self.registry.lock();
        let mut state = ns.lifecycle.state.lock();
        if state.phase == Phase::Dead { return Err(ENETDOWN); }
        if state.owner == Owner::Lazy || state.phase != Phase::Idle { return Err(EBUSY); }
        let generation = state.generation.checked_add(1).ok_or(EOVERFLOW)?;
        let provider = Arc::new(Provider { ns: ns.clone(), generation }, GFP_KERNEL)?;
        state.owner = Owner::Explicit;
        state.generation = generation;
        state.phase = Phase::Serving;
        ns.lifecycle.live.store(generation, Ordering::Release);
        Ok(provider)
    }
    pub(crate) fn revoke(&self, ns: &Namespace) {
        let mut registry = self.registry.lock();
        let mut state = ns.lifecycle.state.lock();
        ns.lifecycle.stop(&mut state, true);
        ns.abort_generation();
        registry.namespaces.retain(|old| !core::ptr::eq(&**old, ns));
    }
    pub(crate) fn ensure(&self, ns: &Arc<Namespace>) -> Result<u64> {
        let generation;
        {
            let mut registry = self.registry.lock();
            let mut state = ns.lifecycle.state.lock();
            match state.phase {
                Phase::Ready => return Ok(state.generation),
                Phase::Dead => return Err(ENETDOWN),
                Phase::Idle => {
                    if state.owner == Owner::Explicit || !registry.connected { return Err(ENETDOWN); }
                    if registry.namespaces.len() >= MAX_NAMESPACES { return Err(ENOSPC); }
                    let next = state.generation.checked_add(1).ok_or(EOVERFLOW)?;
                    registry.namespaces.push(ns.clone(), GFP_KERNEL)?;
                    state.owner = Owner::Lazy;
                    state.generation = next;
                    state.phase = Phase::Pending;
                    self.changed.notify_all();
                }
                Phase::Pending | Phase::Serving => {}
            }
            generation = state.generation;
        }
        let deadline = Deadline::new(kernel::time::msecs_to_jiffies(TIMEOUT_MS) as usize);
        let mut state = ns.lifecycle.state.lock();
        loop {
            if state.generation != generation { return Err(ENETDOWN); }
            match state.phase {
                Phase::Ready => return Ok(generation),
                Phase::Idle | Phase::Dead => return Err(ENETDOWN),
                _ => {}
            }
            let remaining = deadline.remaining();
            if remaining == 0 {
                drop(state);
                fail(ns, generation);
                return Err(ETIMEDOUT);
            }
            if let CondVarTimeoutResult::Signal { .. } =
                ns.lifecycle.changed.wait_interruptible_timeout(&mut state, remaining)
            { return Err(EINTR); }
        }
    }
}

pub(crate) struct Provisioner(Arc<Broker>);
impl Endpoint for Provisioner {
    fn release(&self) {
        let mut registry = self.0.registry.lock();
        registry.connected = false;
        for ns in &registry.namespaces {
            let mut state = ns.lifecycle.state.lock();
            ns.lifecycle.stop(&mut state, false);
            ns.abort_generation();
        }
        registry.namespaces.clear();
    }
    fn poll(&self, poll: &Poll<'_>) -> u32 {
        poll.register(&self.0.changed);
        let registry = self.0.registry.lock();
        if registry.namespaces.iter().any(|ns| ns.lifecycle.state.lock().phase == Phase::Pending) {
            b::POLLIN
        } else { 0 }
    }
    fn ioctl(&self, cmd: u32, arg: usize) -> Result<isize> {
        if cmd != CLAIM { return Err(ENOTTY); }
        let serving_fd = FileDescriptorReservation::get_unused_fd_flags(b::O_CLOEXEC)?;
        let monitor_fd = FileDescriptorReservation::get_unused_fd_flags(b::O_CLOEXEC)?;
        let provider = {
            let registry = self.0.registry.lock();
            let mut request = None;
            for ns in &registry.namespaces {
                let mut state = ns.lifecycle.state.lock();
                if state.phase != Phase::Pending { continue; }
                let provider = Arc::new(Provider { ns: ns.clone(), generation: state.generation }, GFP_KERNEL)?;
                ns.lifecycle.live.store(state.generation, Ordering::Release);
                state.phase = Phase::Serving;
                request = Some(provider);
                break;
            }
            request.ok_or(EAGAIN)?
        };
        let ns = provider.ns.clone();
        let generation = provider.generation;
        let result = (|| -> Result<isize> {
            let monitor = Arc::new(Monitor { ns: ns.clone(), generation }, GFP_KERNEL)?;
            let serving = endpoint_file::create(provider)?;
            let monitor = endpoint_file::create(monitor)?;
            let mut response = [0u8; 8];
            response[..4].copy_from_slice(&serving_fd.reserved_fd().to_ne_bytes());
            response[4..].copy_from_slice(&monitor_fd.reserved_fd().to_ne_bytes());
            UserSlice::new(UserPtr::from_addr(arg), 8).writer().write_slice(&response)?;
            serving_fd.fd_install(serving);
            monitor_fd.fd_install(monitor);
            Ok(0)
        })();
        if result.is_err() { fail(&ns, generation); }
        result
    }
}
fn fail(ns: &Namespace, generation: u64) {
    let broker = crate::broker();
    let mut registry = broker.registry.lock();
    let mut state = ns.lifecycle.state.lock();
    if state.generation != generation || matches!(state.phase, Phase::Dead | Phase::Idle) { return; }
    ns.lifecycle.stop(&mut state, false);
    ns.abort_generation();
    registry.namespaces.retain(|old| !core::ptr::eq(&**old, ns));
}

/// Sole generation owner, shared by dup/SCM_RIGHTS. Protocols retain separate
/// queues, but cannot independently activate, replace or revoke this generation.
pub(crate) struct Provider { ns: Arc<Namespace>, generation: u64 }
impl Endpoint for Provider {
    fn release(&self) { fail(&self.ns, self.generation); }
    fn write(&self, input: &mut UserSliceReader) -> Result<usize> {
        self.ns.netlink.publish(self.generation, input)
    }
    fn poll(&self, poll: &Poll<'_>) -> u32 {
        poll.register(&self.ns.lifecycle.changed);
        let state = self.ns.lifecycle.state.lock();
        if self.ns.lifecycle.live() != self.generation { return b::POLLHUP | b::POLLERR; }
        let pending = state.controls.iter().any(|control| {
            let state = control.state.lock();
            !state.claimed && state.reply.is_none()
        });
        drop(state);
        (if pending { b::POLLPRI } else { 0 })
            | self.ns.poll(self.generation, poll) | self.ns.netlink.poll(self.generation, poll)
    }
    fn ioctl(&self, cmd: u32, arg: usize) -> Result<isize> {
        if cmd == 0x8008B301 { return self.ns.claim(self.generation, arg); }
        if cmd == 0x8008B401 { return self.ns.netlink.claim(self.generation, arg); }
        let mut state = self.ns.lifecycle.state.lock();
        if self.ns.lifecycle.live() != self.generation { return Err(ENETDOWN); }
        match cmd {
            READY => {
                state.phase = Phase::Ready;
                self.ns.lifecycle.changed.notify_all();
                Ok(0)
            }
            CLAIM_CONTROL => {
                let reserved = FileDescriptorReservation::get_unused_fd_flags(b::O_CLOEXEC)?;
                for control in &state.controls {
                    let mut inner = control.state.lock();
                    if inner.claimed || inner.reply.is_some() { continue; }
                    let file = endpoint_file::create(control.clone())?;
                    inner.claimed = true;
                    let fd = reserved.reserved_fd();
                    reserved.fd_install(file);
                    return Ok(fd as isize);
                }
                Err(EAGAIN)
            }
            _ => Err(ENOTTY),
        }
    }
}
struct Monitor { ns: Arc<Namespace>, generation: u64 }
impl Endpoint for Monitor {
    fn release(&self) { fail(&self.ns, self.generation); }
    fn poll(&self, poll: &Poll<'_>) -> u32 {
        poll.register(&self.ns.lifecycle.changed);
        if self.ns.lifecycle.live() != self.generation { b::POLLHUP } else { 0 }
    }
    fn ioctl(&self, cmd: u32, _arg: usize) -> Result<isize> {
        if cmd != REVOKE { return Err(ENOTTY); }
        fail(&self.ns, self.generation);
        Ok(0)
    }
}

struct ControlState { claimed: bool, read: bool, reply: Option<Result<KVec<u8>>> }
#[pin_data]
struct Control {
    data: [u8; 48],
    #[pin] state: Mutex<ControlState>,
    #[pin] changed: PollCondVar,
}
impl Control {
    fn cancel(&self, error: Error) {
        let mut state = self.state.lock();
        if state.reply.is_none() { state.reply = Some(Err(error)); }
        self.changed.notify_all();
    }
    fn wait(&self) -> Result<KVec<u8>> {
        let deadline = Deadline::new(kernel::time::msecs_to_jiffies(TIMEOUT_MS) as usize);
        let mut state = self.state.lock();
        loop {
            if state.reply.is_some() { return state.reply.replace(Err(ENOENT)).unwrap(); }
            let remaining = deadline.remaining();
            if remaining == 0 { state.reply = Some(Err(ETIMEDOUT)); return Err(ETIMEDOUT); }
            if let CondVarTimeoutResult::Signal { .. } =
                self.changed.wait_interruptible_timeout(&mut state, remaining)
            { state.reply = Some(Err(EINTR)); return Err(EINTR); }
        }
    }
}
impl Endpoint for Control {
    fn release(&self) { self.cancel(ENETDOWN); }
    fn read(&self, out: &mut UserSliceWriter) -> Result<usize> {
        let mut state = self.state.lock();
        if state.reply.is_some() { return Err(ENOENT); }
        if state.read { return Ok(0); }
        out.write_slice(&self.data)?;
        state.read = true;
        Ok(self.data.len())
    }
    fn write(&self, input: &mut UserSliceReader) -> Result<usize> {
        let len = input.len();
        if len < 4 || len > MAX_REPLY + 4 { return Err(EMSGSIZE); }
        let mut status = [0u8; 4];
        input.read_slice(&mut status)?;
        let status = i32::from_le_bytes(status);
        if status > 0 || status < -4095 || status != 0 && len != 4 { return Err(EPROTO); }
        // Internal kernel statuses are control flow, not worker-selected errno.
        // Complete the caller with a protocol error rather than replaying its syscall.
        let status = if status <= -512 { EPROTO.to_errno() } else { status };
        let mut data = KVec::new();
        data.resize(len - 4, 0, GFP_KERNEL)?;
        input.read_slice(&mut data)?;
        let mut state = self.state.lock();
        if state.reply.is_some() || !state.read { return Err(ENOENT); }
        state.reply = Some(if status == 0 { Ok(data) } else { Err(Error::from_errno(status)) });
        self.changed.notify_all();
        Ok(len)
    }
}
