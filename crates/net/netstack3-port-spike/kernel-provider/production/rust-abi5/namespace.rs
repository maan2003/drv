// SPDX-License-Identifier: GPL-2.0-only
//! Namespace lifecycle and provider capabilities. No process IDs or FD slots are authority.
#![forbid(unsafe_code)]
use crate::{
    endpoint_file::{self, Endpoint, Poll},
    frontend::{Deadline, Namespace, Session},
    netlink,
};
use kernel::{
    bindings as b, fs::file::FileDescriptorReservation, prelude::*,
    sync::{poll::PollCondVar, Arc, CondVarTimeoutResult, Mutex},
    uaccess::{UserPtr, UserSlice, UserSliceReader},
};

pub(crate) const CLAIM: u32 = 0x8008B501;
const STATE: u32 = 0x8010B502;
const ACK: u32 = 0x4008B503;
const SET_UP: u32 = 0x4004B504;
const REVOKE: u32 = 0xB505;
const MAX_NAMESPACES: usize = 64;
const START_TIMEOUT_MS: u32 = 10000;

#[derive(Clone, Copy, PartialEq)]
enum Phase { Dormant, Manual, Pending, Claimed, Ready, Failed, Dying }
struct State {
    phase: Phase,
    generation: u64,
    revision: u64,
    applied: Option<u64>,
    flags: u32,
}
#[pin_data]
pub(crate) struct Lifecycle {
    #[pin] state: Mutex<State>,
    #[pin] changed: PollCondVar,
}
impl Lifecycle {
    pub(crate) fn new() -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            state <- kernel::new_mutex!(State {
                phase: Phase::Dormant, generation: 0, revision: 0, applied: None, flags: 0,
            }),
            changed <- kernel::new_poll_condvar!(),
        })
    }
    pub(crate) fn flags_changed(&self, flags: u32) {
        let mut state = self.state.lock();
        if state.flags == flags { return; }
        state.flags = flags;
        // Overflow cannot be confused with a previous acknowledged revision.
        if let Some(revision) = state.revision.checked_add(1) { state.revision = revision; }
        else { state.phase = Phase::Failed; }
        self.changed.notify_all();
    }
    pub(crate) fn dying(&self) {
        self.state.lock().phase = Phase::Dying;
        self.changed.notify_all();
    }
    /// An explicit registration reserves this realm for its existing supervisor,
    /// including across its provider's failure/restart interval.
    pub(crate) fn register_manual<T>(&self, register: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut state = self.state.lock();
        if !matches!(state.phase, Phase::Dormant | Phase::Manual) { return Err(EBUSY); }
        let registration = register()?;
        state.phase = Phase::Manual;
        Ok(registration)
    }
    pub(crate) fn managed(&self) -> bool {
        !matches!(self.state.lock().phase, Phase::Dormant | Phase::Manual)
    }
    pub(crate) fn wait_applied(&self) -> Result {
        let deadline = Deadline::new(kernel::time::msecs_to_jiffies(START_TIMEOUT_MS) as usize);
        let mut state = self.state.lock();
        if matches!(state.phase, Phase::Dormant | Phase::Manual) { return Ok(()); }
        let revision = state.revision;
        loop {
            if matches!(state.phase, Phase::Failed | Phase::Dying) { return Err(ENETDOWN); }
            if state.applied.is_some_and(|applied| applied >= revision) { return Ok(()); }
            let remaining = deadline.remaining();
            if remaining == 0 { return Err(ETIMEDOUT); }
            if let CondVarTimeoutResult::Signal { .. } =
                self.changed.wait_interruptible_timeout(&mut state, remaining)
            { return Err(EINTR); }
        }
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
    pub(crate) fn connected(&self) -> bool { self.registry.lock().connected }
    pub(crate) fn remove(&self, id: u64) {
        self.registry.lock().namespaces.retain(|ns| ns.id != id);
    }
    pub(crate) fn ensure(&self, ns: &Arc<Namespace>) -> Result {
        let generation;
        {
            // Global-to-namespace is the only nested lifecycle lock order.
            let mut registry = self.registry.lock();
            let mut state = ns.lifecycle.state.lock();
            match state.phase {
                Phase::Manual | Phase::Ready => return Ok(()),
                Phase::Dying => return Err(ENETDOWN),
                Phase::Dormant | Phase::Failed => {
                    if !registry.connected { return Err(ENETDOWN); }
                    if registry.namespaces.len() >= MAX_NAMESPACES { return Err(ENOSPC); }
                    let next = state.generation.checked_add(1).ok_or(EOVERFLOW)?;
                    registry.namespaces.push(ns.clone(), GFP_KERNEL)?;
                    state.generation = next;
                    state.applied = None;
                    state.phase = Phase::Pending;
                    self.changed.notify_all();
                }
                Phase::Pending | Phase::Claimed => {}
            }
            generation = state.generation;
        }
        let deadline = Deadline::new(kernel::time::msecs_to_jiffies(START_TIMEOUT_MS) as usize);
        let mut state = ns.lifecycle.state.lock();
        loop {
            if state.generation != generation { return Err(ENETDOWN); }
            match state.phase {
                Phase::Manual | Phase::Ready => return Ok(()),
                Phase::Failed | Phase::Dying => return Err(ENETDOWN),
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

/// A provisioner has global spawning authority, but no packet or hardware capability.
pub(crate) struct Provisioner(Arc<Broker>);
impl Endpoint for Provisioner {
    fn release(&self) {
        let mut registry = self.0.registry.lock();
        registry.connected = false;
        for ns in &registry.namespaces {
            let mut state = ns.lifecycle.state.lock();
            if state.phase != Phase::Dying { state.phase = Phase::Failed; }
            ns.abort_generation();
            ns.lifecycle.changed.notify_all();
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
        let (ns, generation, sessions) = {
            let registry = self.0.registry.lock();
            let mut request = None;
            for ns in &registry.namespaces {
                let mut state = ns.lifecycle.state.lock();
                if state.phase != Phase::Pending { continue; }
                state.phase = Phase::Claimed;
                request = Some((ns.clone(), state.generation));
                break;
            }
            let (ns, generation) = request.ok_or(EAGAIN)?;
            // Registration is fenced with failure/replacement. Unwind partial
            // sessions before releasing this guard; file release may call fail.
            let sessions = (|| -> Result<_> {
                let inet = Session::new(ns.clone())?;
                let route = netlink::Session::new(ns.netlink.clone())?;
                Ok((inet, route))
            })();
            (ns, generation, sessions)
        };
        // On every failure, discard unpublished file owners before failing the request.
        let result = (|| -> Result<isize> {
            let (inet, route) = sessions?;
            let serving = Arc::new(Provider { ns: ns.clone(), generation, inet, route }, GFP_KERNEL)?;
            let monitor = Arc::new(Monitor { ns: ns.clone(), generation }, GFP_KERNEL)?;
            let serving = endpoint_file::create(serving)?;
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
    if state.generation != generation || matches!(state.phase, Phase::Dying | Phase::Failed) {
        return;
    }
    state.phase = Phase::Failed;
    ns.abort_generation();
    ns.lifecycle.changed.notify_all();
    registry.namespaces.retain(|old| old.id != ns.id);
}

/// One serving object; dup/SCM_RIGHTS share this generation, never a namespace lookup.
struct Provider {
    ns: Arc<Namespace>,
    generation: u64,
    inet: Arc<Session>,
    route: Arc<netlink::Session>,
}
impl Endpoint for Provider {
    fn release(&self) { fail(&self.ns, self.generation); }
    fn write(&self, input: &mut UserSliceReader) -> Result<usize> { self.route.write(input) }
    fn poll(&self, poll: &Poll<'_>) -> u32 {
        poll.register(&self.ns.lifecycle.changed);
        let state = self.ns.lifecycle.state.lock();
        if state.generation != self.generation || matches!(state.phase, Phase::Failed | Phase::Dying) {
            return b::POLLHUP | b::POLLERR;
        }
        let control = if state.applied != Some(state.revision) { b::POLLPRI } else { 0 };
        drop(state);
        control | self.inet.poll(poll) | self.route.poll(poll)
    }
    fn ioctl(&self, cmd: u32, arg: usize) -> Result<isize> {
        if cmd == SET_UP {
            // Serialize native mutation with fail/ensure without holding the
            // lifecycle lock across the synchronous native flag notifier.
            let mut input = [0u8; 4];
            UserSlice::new(UserPtr::from_addr(arg), 4).reader().read_slice(&mut input)?;
            let up = u32::from_ne_bytes(input);
            if up > 1 { return Err(EINVAL); }
            let broker = crate::broker();
            let _registry = broker.registry.lock();
            let state = self.ns.lifecycle.state.lock();
            if state.generation != self.generation || matches!(state.phase, Phase::Failed | Phase::Dying) {
                return Err(ENETDOWN);
            }
            drop(state);
            self.ns.native.set_loopback(up != 0)?;
            return Ok(0);
        }
        let state = self.ns.lifecycle.state.lock();
        if state.generation != self.generation || matches!(state.phase, Phase::Failed | Phase::Dying) {
            return Err(ENETDOWN);
        }
        match cmd {
            STATE => {
                let mut response = [0u8; 16];
                response[..8].copy_from_slice(&state.revision.to_ne_bytes());
                response[8..12].copy_from_slice(&state.flags.to_ne_bytes());
                UserSlice::new(UserPtr::from_addr(arg), 16).writer().write_slice(&response)?;
                Ok(0)
            }
            ACK => {
                let mut input = [0u8; 8];
                UserSlice::new(UserPtr::from_addr(arg), 8).reader().read_slice(&mut input)?;
                let revision = u64::from_ne_bytes(input);
                if revision != state.revision { return Err(EAGAIN); }
                drop(state);
                let mut state = self.ns.lifecycle.state.lock();
                if state.generation != self.generation || state.phase != Phase::Claimed && state.phase != Phase::Ready {
                    return Err(ENETDOWN);
                }
                if state.revision != revision { return Err(EAGAIN); }
                state.applied = Some(revision);
                state.phase = Phase::Ready;
                self.ns.lifecycle.changed.notify_all();
                Ok(0)
            }
            _ => {
                drop(state);
                if cmd == 0x8008B301 { self.inet.ioctl(cmd, arg) }
                else if cmd == 0x8008B401 { self.route.ioctl(cmd, arg) }
                else { Err(ENOTTY) }
            }
        }
    }
}

/// Separate lifecycle authority: retaining this never retains serving ownership.
struct Monitor { ns: Arc<Namespace>, generation: u64 }
impl Endpoint for Monitor {
    fn release(&self) { fail(&self.ns, self.generation); }
    fn poll(&self, poll: &Poll<'_>) -> u32 {
        poll.register(&self.ns.lifecycle.changed);
        let state = self.ns.lifecycle.state.lock();
        if state.generation != self.generation || matches!(state.phase, Phase::Failed | Phase::Dying) {
            b::POLLHUP
        } else { 0 }
    }
    fn ioctl(&self, cmd: u32, _arg: usize) -> Result<isize> {
        if cmd != REVOKE { return Err(ENOTTY); }
        fail(&self.ns, self.generation);
        Ok(0)
    }
}
