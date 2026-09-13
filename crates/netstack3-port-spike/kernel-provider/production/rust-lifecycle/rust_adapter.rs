// SPDX-License-Identifier: GPL-2.0-only
//! Foreign ownership boundary. Linux callback contracts are in linux_adapter.c.
//! All state transitions and locking live in safe lifecycle.rs.

mod lifecycle;

use core::{ffi::c_void, ptr};
use kernel::{
    bindings,
    fs::File,
    prelude::*,
    sync::{poll::PollTable, Arc},
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
    drop(unsafe { KBox::<Socket>::from_foreign(p) });
}
#[no_mangle]
unsafe extern "C" fn nsrl_socket_id(p: *mut c_void) -> u64 {
    // SAFETY: ioctl holds the socket file alive, excluding final release.
    unsafe { KBox::<Socket>::borrow(p) }.id()
}
#[no_mangle]
unsafe extern "C" fn nsrl_socket_poll(
    p: *mut c_void,
    file: *const bindings::file,
    table: *mut bindings::poll_table,
) -> u32 {
    // SAFETY: Linux poll holds file/socket alive and provides a valid or null
    // poll_table for this call. Socket files are positionless streams; this
    // callback does not run inside fdget_pos. The borrows never escape.
    let socket = unsafe { KBox::<Socket>::borrow(p) };
    let file = unsafe { File::from_raw_file(file) };
    let table = unsafe { PollTable::from_raw(table) };
    socket.poll(file, &table)
}
#[no_mangle]
extern "C" fn nsrl_live_sockets() -> usize {
    lifecycle::live_sockets()
}
#[no_mangle]
extern "C" fn nsrl_live_namespaces() -> usize {
    lifecycle::live_namespaces()
}
