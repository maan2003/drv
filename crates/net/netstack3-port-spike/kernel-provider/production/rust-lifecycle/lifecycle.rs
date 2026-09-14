// SPDX-License-Identifier: GPL-2.0-only
//! Safe state/ownership half of the kernel-Rust feasibility experiment.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::{
    prelude::*,
    sync::{
        poll::PollCondVar,
        Arc, Mutex,
    },
};

const SOCKETS: usize = 256;
// Diagnostic only: never used to make a lifetime or admission decision.
static LIVE_SOCKETS: AtomicUsize = AtomicUsize::new(0);
static LIVE_NAMESPACES: AtomicUsize = AtomicUsize::new(0);

struct Entry {
    id: u64,
    generation: u64,
    ready: bool,
}
struct State {
    online: Option<u64>,
    next_generation: u64,
    next_socket: u64,
    sockets: KVec<Entry>,
}
#[pin_data]
pub(crate) struct Namespace {
    #[pin]
    state: Mutex<State>,
    #[pin]
    changed: PollCondVar,
    _count: NamespaceCount,
}
// Keep diagnostic destruction in an unpinned field: current PinnedDrop is an
// unsafe trait, and lifecycle policy must continue to forbid unsafe code.
struct NamespaceCount;
impl Drop for NamespaceCount {
    fn drop(&mut self) {
        LIVE_NAMESPACES.fetch_sub(1, Ordering::Relaxed);
    }
}
impl Namespace {
    pub(crate) fn new() -> Result<Arc<Self>> {
        let namespace = Arc::pin_init(
            try_pin_init!(Self {
                _count: {
                    LIVE_NAMESPACES.fetch_add(1, Ordering::Relaxed);
                    NamespaceCount
                },
                changed <- kernel::new_poll_condvar!(),
                state <- kernel::new_mutex!(State {
                    online: None, next_generation: 0, next_socket: 0,
                    sockets: KVec::new(),
                }),
            }),
            GFP_KERNEL,
        )?;
        Ok(namespace)
    }
}
/// One owner per open provider file, not per descriptor. Dup/fork share it.
pub(crate) struct Session {
    namespace: Arc<Namespace>,
    generation: Option<u64>,
}
impl Session {
    pub(crate) fn open(namespace: Arc<Namespace>) -> Result<KBox<Self>> {
        let mut session = KBox::new(
            Self {
                namespace,
                generation: None,
            },
            GFP_KERNEL,
        )?;
        let mut state = session.namespace.state.lock();
        if state.online.is_some() {
            return Err(EBUSY);
        }
        let generation = state.next_generation.checked_add(1).ok_or(EOVERFLOW)?;
        state.next_generation = generation;
        state.online = Some(generation);
        drop(state);
        session.generation = Some(generation);
        Ok(session)
    }
    pub(crate) fn ready(&self, id: u64, ready: bool) -> Result {
        let mut state = self.namespace.state.lock();
        if state.online != self.generation {
            return Err(ENETDOWN);
        }
        let entry = state
            .sockets
            .iter_mut()
            .find(|entry| entry.id == id && Some(entry.generation) == self.generation)
            .ok_or(ENOENT)?;
        entry.ready = ready;
        drop(state);
        self.namespace.changed.notify_all();
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(generation) = self.generation {
            let mut state = self.namespace.state.lock();
            if state.online == Some(generation) {
                state.online = None;
                drop(state);
                self.namespace.changed.notify_all();
            }
        }
    }
}

/// One owner per socket file, released only on final VFS release.
/// Namespace Arc has no back-reference to Socket, so there is no owner cycle.
pub(crate) struct Socket {
    namespace: Arc<Namespace>,
    identity: Option<(u64, u64)>,
}
impl Socket {
    pub(crate) fn create(namespace: Arc<Namespace>) -> Result<Arc<Self>> {
        let mut socket = Self {
            namespace,
            identity: None,
        };
        let mut state = socket.namespace.state.lock();
        let generation = state.online.ok_or(ENETDOWN)?;
        if state.sockets.len() == SOCKETS {
            return Err(ENFILE);
        }
        let id = state.next_socket.checked_add(1).ok_or(EOVERFLOW)?;
        state.sockets.push(
            Entry {
                id,
                generation,
                ready: false,
            },
            GFP_KERNEL,
        )?;
        state.next_socket = id;
        drop(state);
        socket.identity = Some((id, generation));
        LIVE_SOCKETS.fetch_add(1, Ordering::Relaxed);
        Arc::new(socket, GFP_KERNEL).map_err(Into::into)
    }
    pub(crate) fn id(&self) -> u64 {
        self.identity.unwrap().0
    }
    /// 0 = pending, 1 = readable/writable, 2 = provider generation dead.
    pub(crate) fn poll(&self, poll: &crate::endpoint_file::Poll<'_>) -> u32 {
        // Register before sampling. Updates release the same mutex before wakeup.
        // Upstream PollCondVar owns pollfree notification and the RCU grace period.
        poll.register(&self.namespace.changed);
        let (id, generation) = self.identity.unwrap();
        let state = self.namespace.state.lock();
        if state.online != Some(generation) {
            return 2;
        }
        u32::from(
            state
                .sockets
                .iter()
                .find(|entry| entry.id == id)
                .unwrap()
                .ready,
        )
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        if let Some((id, _)) = self.identity {
            self.namespace
                .state
                .lock()
                .sockets
                .retain(|entry| entry.id != id);
            LIVE_SOCKETS.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

pub(crate) fn live_sockets() -> usize {
    LIVE_SOCKETS.load(Ordering::Relaxed)
}
pub(crate) fn live_namespaces() -> usize {
    LIVE_NAMESPACES.load(Ordering::Relaxed)
}
