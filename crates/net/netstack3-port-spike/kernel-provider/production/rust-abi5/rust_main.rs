// SPDX-License-Identifier: GPL-2.0-only
//! Linux ABI callback boundary. Frontend state and protocol code forbids unsafe.
mod connection;
mod endpoint_file;
mod frontend;
mod linux;
mod netlink;
mod namespace;
use core::pin::Pin;
use core::{ffi::c_void, ptr};
use frontend::{Namespace, Socket};
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
    let namespace = unsafe { Arc::<Namespace>::from_foreign(p) };
    namespace.revoke();
    drop(namespace);
}
#[no_mangle]
unsafe extern "C" fn ns3_socket_new(
    ns: *mut c_void,
    sk: *mut c_void,
    family: i32,
    kind: i32,
    generation: u64,
    out: *mut *mut c_void,
) -> i32 {
    // SAFETY: C supplies a live pernet owner and initialized native sock;
    // acquire retains a native reference independent of the application file.
    let namespace = unsafe { Arc::<Namespace>::borrow(ns) }.into();
    let native = unsafe { linux::NativeSock::acquire(sk) };
    match Namespace::socket(namespace, native, family, kind, generation) {
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
        let namespace: Arc<Namespace> = unsafe { Arc::<Namespace>::borrow(net.state()) }.into();
        let session = broker().register(namespace)?;
        // SAFETY: open exclusively initializes this file's private data.
        unsafe { (*file).private_data = session.into_foreign() };
        Ok(())
    })();
    result.map(|()| 0).unwrap_or_else(|e| e.to_errno())
}
const REGISTRATION: bindings::file_operations = bindings::file_operations {
    open: Some(provider_open),
    ..endpoint_file::operations::<namespace::Provider>()
};
#[no_mangle]
extern "C" fn ns3_registration_ops() -> *const bindings::file_operations {
    &REGISTRATION
}


// NETLINK_ROUTE transport hooks. Every borrowed pointer is socket-owned;
// final application release consumes exactly that foreign Arc.
#[no_mangle]
unsafe extern "C" fn ns3_nl_socket_new(ns: *mut c_void, sk: *mut c_void, out: *mut *mut c_void) -> i32 {
    let namespace = unsafe { Arc::<Namespace>::borrow(ns) };
    let namespace: Arc<Namespace> = namespace.into();
    let generation = match broker().ensure(&namespace) {
        Ok(generation) => generation,
        Err(error) => return error.to_errno(),
    };
    let native = unsafe { linux::NativeSock::acquire(sk) };
    match netlink::Namespace::socket(namespace.netlink.clone(), native, generation) {
        Ok(channel) => { unsafe { out.write(channel.into_foreign()) }; 0 }
        Err(e) => e.to_errno(),
    }
}
#[no_mangle]
unsafe extern "C" fn ns3_nl_close(p: *mut c_void) {
    let channel = unsafe { Arc::<netlink::Channel>::from_foreign(p) };
    channel.close_app();
}
#[no_mangle]
unsafe extern "C" fn ns3_nl_drained(p: *mut c_void) {
    unsafe { Arc::<netlink::Channel>::borrow(p) }.rx_drained();
}
#[no_mangle]
unsafe extern "C" fn ns3_nl_send(p: *mut c_void, data: *const u8, len: usize,
    context: *const u8, nonblock: bool) -> i32
{
    let channel = unsafe { Arc::<netlink::Channel>::borrow(p) };
    let data = unsafe { core::slice::from_raw_parts(data, len) };
    let context = unsafe { &*context.cast::<[u8; 24]>() };
    channel.send(data, context, nonblock).map(|n| n as i32).unwrap_or_else(|e| e.to_errno())
}
#[no_mangle]
unsafe extern "C" fn ns3_nl_poll(p: *mut c_void, f: *mut bindings::file,
    t: *mut bindings::poll_table) -> u32
{
    unsafe { Arc::<netlink::Channel>::borrow(p) }.app_poll(&unsafe { endpoint_file::Poll::new(f, t) })
}
kernel::sync::global_lock! {
    // SAFETY: initialized once from the C initcall before pernet registration.
    unsafe(uninit) static NAMESPACE_BROKER: Mutex<Option<Arc<namespace::Broker>>> = None;
}
pub(crate) fn broker() -> Arc<namespace::Broker> {
    NAMESPACE_BROKER.lock().as_ref().expect("initialized before pernet").clone()
}
#[no_mangle]
unsafe extern "C" fn ns3_broker_init() -> i32 {
    unsafe { NAMESPACE_BROKER.init() };
    match namespace::Broker::new() {
        Ok(broker) => { *NAMESPACE_BROKER.lock() = Some(broker); 0 }
        Err(error) => error.to_errno(),
    }
}
unsafe extern "C" {
    fn ns3_provisioner_allowed() -> bool;
}
unsafe extern "C" fn broker_open(_inode: *mut bindings::inode, file: *mut bindings::file) -> i32 {
    if !unsafe { ns3_provisioner_allowed() } { return EPERM.to_errno(); }
    match broker().open() {
        Ok(owner) => { unsafe { (*file).private_data = owner.into_foreign() }; 0 }
        Err(error) => error.to_errno(),
    }
}
const BROKER_REGISTRATION: bindings::file_operations = bindings::file_operations {
    open: Some(broker_open), ..endpoint_file::operations::<namespace::Provisioner>()
};
#[no_mangle]
extern "C" fn ns3_broker_ops() -> *const bindings::file_operations { &BROKER_REGISTRATION }
/// Native caller holds a socket/net reference throughout this synchronous call.
/// Only fixed, pointer-free bytes cross the worker boundary.
#[no_mangle]
unsafe extern "C" fn ns3_interface_request(ns: *mut c_void, input: *const u8,
    output: *mut u8, capacity: usize) -> i32
{
    let namespace: Arc<Namespace> = unsafe { Arc::<Namespace>::borrow(ns) }.into();
    let request = unsafe { *input.cast::<[u8; 48]>() };
    let result = (|| -> Result<i32> {
        let generation = broker().ensure(&namespace)?;
        let reply = namespace.lifecycle.control(generation, request)?;
        if reply.len() > capacity { return Err(EMSGSIZE); }
        unsafe { core::ptr::copy_nonoverlapping(reply.as_ptr(), output, reply.len()) };
        Ok(reply.len() as i32)
    })();
    result.unwrap_or_else(|error| error.to_errno())
}
