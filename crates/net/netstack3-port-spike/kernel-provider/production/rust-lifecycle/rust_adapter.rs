// SPDX-License-Identifier: GPL-2.0-only
//! Foreign ownership boundary. Linux callback contracts are in linux_adapter.c.
//! All state transitions and locking live in safe lifecycle.rs.

mod endpoint_file;
mod lifecycle;

use core::{ffi::c_void, ptr};
use kernel::{
    bindings,
    fs::file::FileDescriptorReservation,
    prelude::*,
    sync::Arc,
    types::ForeignOwnable,
};
use lifecycle::{Namespace, Session, Socket};

// Every pointer passed to a borrow callback is owned by a live C pernet object,
// socket file or provider file. VFS excludes final release while a callback is
// active. No callback manufactures a mutable reference to shared Rust state.
// Destructors run in sleepable context and consume their foreign owner once.

#[no_mangle]
extern "C" fn nsrl_namespace_new() -> *mut c_void {
    Namespace::new()
        .map(ForeignOwnable::into_foreign)
        .unwrap_or(ptr::null_mut())
}
#[no_mangle]
unsafe extern "C" fn nsrl_namespace_drop(p: *mut c_void) {
    // SAFETY: pernet exit consumes exactly the owner returned by namespace_new.
    drop(unsafe { Arc::<Namespace>::from_foreign(p) });
}
#[no_mangle]
unsafe extern "C" fn nsrl_session_new(p: *mut c_void, out: *mut *mut c_void) -> i32 {
    // SAFETY: caller holds a net namespace reference throughout this callback.
    let namespace = unsafe { Arc::<Namespace>::borrow(p) };
    match Session::open(namespace.into()) {
        Ok(session) => {
            // SAFETY: C passes a valid writable pointer-sized output slot.
            unsafe { out.write(session.into_foreign()) };
            0
        }
        Err(error) => error.to_errno(),
    }
}
#[no_mangle]
unsafe extern "C" fn nsrl_session_drop(p: *mut c_void) {
    // SAFETY: final provider file release consumes its unique foreign owner.
    drop(unsafe { KBox::<Session>::from_foreign(p) });
}
#[no_mangle]
unsafe extern "C" fn nsrl_set_ready(p: *mut c_void, id: u64, ready: bool) -> i32 {
    // SAFETY: ioctl holds the provider file alive, excluding final release.
    unsafe { KBox::<Session>::borrow(p) }
        .ready(id, ready)
        .map(|()| 0)
        .unwrap_or_else(|error| error.to_errno())
}
#[no_mangle]
unsafe extern "C" fn nsrl_socket_new(p: *mut c_void, out: *mut *mut c_void) -> i32 {
    // SAFETY: socket creation holds a live net namespace reference.
    let namespace = unsafe { Arc::<Namespace>::borrow(p) };
    match Socket::create(namespace.into()) {
        Ok(socket) => {
            // SAFETY: C passes a valid writable pointer-sized output slot.
            unsafe { out.write(socket.into_foreign()) };
            0
        }
        Err(error) => error.to_errno(),
    }
}
#[no_mangle]
unsafe extern "C" fn nsrl_socket_drop(p: *mut c_void) {
    // SAFETY: final socket file release consumes its unique foreign owner.
    drop(unsafe { Arc::<Socket>::from_foreign(p) });
}
#[no_mangle]
unsafe extern "C" fn nsrl_socket_id(p: *mut c_void) -> u64 {
    // SAFETY: ioctl holds the socket file alive, excluding final release.
    unsafe { Arc::<Socket>::borrow(p) }.id()
}
#[no_mangle]
unsafe extern "C" fn nsrl_socket_poll(
    p: *mut c_void,
    file: *const bindings::file,
    table: *mut bindings::poll_table,
) -> u32 {
    // SAFETY: Linux holds socket/file/table live throughout the callback.
    let socket = unsafe { Arc::<Socket>::borrow(p) };
    socket.poll(&unsafe { endpoint_file::Poll::new(file.cast_mut(), table) })
}
#[no_mangle]
extern "C" fn nsrl_live_sockets() -> usize {
    lifecycle::live_sockets()
}
#[no_mangle]
extern "C" fn nsrl_live_namespaces() -> usize {
    lifecycle::live_namespaces()
}

// Endpoint consumer retains the socket independently of the application file.
struct SocketEndpoint(Arc<Socket>);
impl endpoint_file::Endpoint for SocketEndpoint {
    fn read(&self, out: &mut kernel::uaccess::UserSliceWriter) -> Result<usize> {
        out.write_slice(&self.0.id().to_le_bytes())?;
        Ok(8)
    }
    fn poll(&self, poll: &endpoint_file::Poll<'_>) -> u32 {
        match self.0.poll(poll) {
            0 => 0,
            1 => bindings::POLLIN | bindings::POLLOUT,
            _ => bindings::POLLERR | bindings::POLLHUP,
        }
    }
}
#[no_mangle]
unsafe extern "C" fn nsrl_socket_endpoint(p: *mut c_void) -> i32 {
    // SAFETY: socket ioctl retains the live Arc owner for this callback.
    let socket: Arc<Socket> = unsafe { Arc::<Socket>::borrow(p) }.into();
    Arc::new(SocketEndpoint(socket), GFP_KERNEL)
        .map_err(Error::from)
        .and_then(|endpoint| {
            let reserved = FileDescriptorReservation::get_unused_fd_flags(bindings::O_CLOEXEC)?;
            let file = endpoint_file::create(endpoint)?;
            let fd = reserved.reserved_fd();
            reserved.fd_install(file);
            Ok(fd as i32)
        })
        .unwrap_or_else(|e| e.to_errno())
}
