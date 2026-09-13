// SPDX-License-Identifier: GPL-2.0-only
//! Linux ABI callback boundary. Frontend state and protocol code forbids unsafe.
mod connection;
mod endpoint_file;
mod frontend;
mod linux;
use core::{ffi::c_void, ptr};
use frontend::{Namespace, Session, Socket};
use kernel::{
    bindings,
    prelude::*,
    sync::Arc,
    types::ForeignOwnable,
};

// Every callback borrows a live C/VFS-owned Arc. Borrowed references never
// escape the callback; only explicit Arc clones may outlive it. C release
// consumes the exact foreign owner created for its object.
#[no_mangle]
extern "C" fn ns3_net_new() -> *mut c_void {
    Namespace::new()
        .map(ForeignOwnable::into_foreign)
        .unwrap_or(ptr::null_mut())
}
#[no_mangle]
unsafe extern "C" fn ns3_net_drop(p: *mut c_void) {
    // SAFETY: pernet exit consumes its unique foreign namespace owner.
    drop(unsafe { Arc::<Namespace>::from_foreign(p) });
}
#[no_mangle]
unsafe extern "C" fn ns3_socket_new(
    ns: *mut c_void,
    sk: *mut c_void,
    family: i32,
    kind: i32,
    claimed: bool,
    out: *mut *mut c_void,
) -> i32 {
    // SAFETY: C supplies a live pernet owner and initialized native sock;
    // acquire retains a native reference independent of the application file.
    let namespace = unsafe { Arc::<Namespace>::borrow(ns) }.into();
    let native = unsafe { linux::NativeSock::acquire(sk) };
    match Namespace::socket(namespace, native, family, kind, claimed) {
        Ok(socket) => {
            unsafe { out.write(socket.into_foreign()) };
            0
        }
        Err(e) => e.to_errno(),
    }
}
#[no_mangle]
unsafe extern "C" fn ns3_socket_release(p: *mut c_void) {
    // SAFETY: final native socket file release consumes the application owner.
    let socket = unsafe { Arc::<Socket>::from_foreign(p) };
    socket.close_app();
    drop(socket);
}
pub(crate) fn accepted_socket(child: &linux::Accepted) -> Arc<Socket> {
    // SAFETY: Accepted owns the private native socket containing this Arc.
    unsafe { Arc::<Socket>::borrow(child.state()) }.into()
}
unsafe fn address(p: *const c_void, len: i32) -> Result<linux::Address> {
    if len < 0 {
        return Err(EINVAL);
    }
    // SAFETY: bind/connect callbacks receive kernel-copied sockaddr memory
    // valid for len bytes. Linux validates the overall sockaddr bound.
    linux::Address::from_native(unsafe { core::slice::from_raw_parts(p.cast(), len as usize) })
}
#[no_mangle]
unsafe extern "C" fn ns3_bind(p: *mut c_void, a: *const c_void, len: i32) -> i32 {
    let socket = unsafe { Arc::<Socket>::borrow(p) };
    unsafe { address(a, len) }
        .and_then(|a| socket.bind(a))
        .map(|()| 0)
        .unwrap_or_else(|e| e.to_errno())
}
#[no_mangle]
unsafe extern "C" fn ns3_connect(p: *mut c_void, a: *const c_void, len: i32, flags: i32) -> i32 {
    let socket = unsafe { Arc::<Socket>::borrow(p) };
    unsafe { address(a, len) }
        .and_then(|a| socket.connect(a, flags))
        .map(|()| 0)
        .unwrap_or_else(|e| e.to_errno())
}
#[no_mangle]
unsafe extern "C" fn ns3_listen(p: *mut c_void, backlog: i32) -> i32 {
    unsafe { Arc::<Socket>::borrow(p) }
        .listen(backlog)
        .map(|()| 0)
        .unwrap_or_else(|e| e.to_errno())
}
#[no_mangle]
unsafe extern "C" fn ns3_accept(p: *mut c_void, target: *mut c_void, flags: i32) -> i32 {
    let mut target = unsafe { linux::AcceptTarget::new(target) };
    unsafe { Arc::<Socket>::borrow(p) }
        .accept(&mut target, flags)
        .map(|()| 0)
        .unwrap_or_else(|e| e.to_errno())
}
#[no_mangle]
unsafe extern "C" fn ns3_send(p: *mut c_void, m: *mut bindings::msghdr, len: usize) -> i32 {
    let mut msg = unsafe { linux::Message::new(m) };
    unsafe { Arc::<Socket>::borrow(p) }
        .send(&mut msg, len)
        .map(|n| n as i32)
        .unwrap_or_else(|e| e.to_errno())
}
#[no_mangle]
unsafe extern "C" fn ns3_recv(
    p: *mut c_void,
    m: *mut bindings::msghdr,
    len: usize,
    flags: i32,
) -> i32 {
    let mut msg = unsafe { linux::Message::new(m) };
    unsafe { Arc::<Socket>::borrow(p) }
        .recv(&mut msg, len, flags as u32)
        .map(|n| n as i32)
        .unwrap_or_else(|e| e.to_errno())
}
#[no_mangle]
unsafe extern "C" fn ns3_name(p: *mut c_void, a: *mut c_void, peer: i32) -> i32 {
    match unsafe { Arc::<Socket>::borrow(p) }.name(peer != 0) {
        Ok(address) => {
            let (bytes, len) = address.native();
            // SAFETY: getname provides writable kernel sockaddr_storage.
            unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), a.cast(), len) };
            len as i32
        }
        Err(e) => e.to_errno(),
    }
}
#[no_mangle]
unsafe extern "C" fn ns3_shutdown(p: *mut c_void, how: i32) -> i32 {
    unsafe { Arc::<Socket>::borrow(p) }
        .shutdown(how)
        .map(|()| 0)
        .unwrap_or_else(|e| e.to_errno())
}
#[no_mangle]
unsafe extern "C" fn ns3_poll(
    p: *mut c_void,
    f: *mut bindings::file,
    t: *mut bindings::poll_table,
) -> u32 {
    // SAFETY: VFS keeps socket, file and table live throughout this callback.
    unsafe { Arc::<Socket>::borrow(p) }.poll(&unsafe { endpoint_file::Poll::new(f, t) })
}
unsafe extern "C" fn provider_open(_inode: *mut bindings::inode, file: *mut bindings::file) -> i32 {
    let result = (|| -> Result {
        let net = linux::NetRef::current()?;
        // SAFETY: NetRef retains the namespace and its pernet Arc during borrow.
        let namespace = unsafe { Arc::<Namespace>::borrow(net.state()) }.into();
        let session = Session::new(namespace, net)?;
        // SAFETY: open exclusively initializes this file's private data.
        unsafe { (*file).private_data = session.into_foreign() };
        Ok(())
    })();
    result.map(|()| 0).unwrap_or_else(|e| e.to_errno())
}
const REGISTRATION: bindings::file_operations = bindings::file_operations {
    open: Some(provider_open),
    ..endpoint_file::operations::<Session>()
};
#[no_mangle]
extern "C" fn ns3_registration_ops() -> *const bindings::file_operations {
    &REGISTRATION
}
