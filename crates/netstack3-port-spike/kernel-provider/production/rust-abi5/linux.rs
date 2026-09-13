// SPDX-License-Identifier: GPL-2.0-only
//! Audited native lifetime, iterator and address boundary. No transport policy.
use core::{ffi::c_void, ptr::NonNull};
use kernel::{
    bindings,
    error::from_err_ptr,
    iov::{IovIterDest, IovIterSource},
    prelude::*,
};
unsafe extern "C" {
    fn ns3_hold(p: *mut c_void);
    fn ns3_put(p: *mut c_void);
    fn ns3_error(p: *mut c_void, consume: bool) -> i32;
    fn ns3_set_error(p: *mut c_void, error: i32);
    fn ns3_timeout(p: *mut c_void, send: bool, nonblock: bool) -> usize;
    fn ns3_set_shutdown(p: *mut c_void, how: i32);
    fn ns3_current_net() -> *mut c_void;
    fn ns3_put_net(p: *mut c_void);
    fn ns3_net_state(p: *mut c_void) -> *mut c_void;
    fn ns3_new_accepted(p: *mut c_void, family: i32) -> *mut c_void;
    fn ns3_accepted_state(p: *mut c_void) -> *mut c_void;
    fn ns3_accept_transfer(p: *mut c_void, new: *mut c_void);
    fn ns3_accept_drop(p: *mut c_void);
    fn ns3_sigpipe();
}
pub(crate) struct NativeSock(NonNull<c_void>);
// SAFETY: holds a native sock reference; helpers use native atomic/locked
// interfaces. In particular ns3_set_shutdown serializes its own shared callers.
unsafe impl Send for NativeSock {}
unsafe impl Sync for NativeSock {}
impl NativeSock {
    /// Caller supplies a live initialized struct sock. Acquires its own ref.
    pub(crate) unsafe fn acquire(p: *mut c_void) -> Self {
        unsafe { ns3_hold(p) };
        Self(unsafe { NonNull::new_unchecked(p) })
    }
    pub(crate) fn error(&self, consume: bool) -> i32 {
        unsafe { ns3_error(self.0.as_ptr(), consume) }
    }
    pub(crate) fn set_error(&self, error: i32) {
        unsafe { ns3_set_error(self.0.as_ptr(), error) }
    }
    pub(crate) fn timeout(&self, send: bool, nonblock: bool) -> usize {
        unsafe { ns3_timeout(self.0.as_ptr(), send, nonblock) }
    }
    pub(crate) fn shutdown(&self, how: i32) {
        unsafe { ns3_set_shutdown(self.0.as_ptr(), how) }
    }
    pub(crate) fn accepted(&self, family: i32) -> Result<Accepted> {
        let p = from_err_ptr(unsafe { ns3_new_accepted(self.0.as_ptr(), family) })?;
        Ok(Accepted(unsafe { NonNull::new_unchecked(p) }))
    }
}
impl Drop for NativeSock {
    fn drop(&mut self) {
        unsafe { ns3_put(self.0.as_ptr()) }
    }
}
pub(crate) struct NetRef(NonNull<c_void>);
// SAFETY: get_net/put_net references may cross threads; namespace data is locked.
unsafe impl Send for NetRef {}
unsafe impl Sync for NetRef {}
impl NetRef {
    pub(crate) fn current() -> Result<Self> {
        let p = from_err_ptr(unsafe { ns3_current_net() })?;
        Ok(Self(unsafe { NonNull::new_unchecked(p) }))
    }
    pub(crate) fn state(&self) -> *mut c_void {
        unsafe { ns3_net_state(self.0.as_ptr()) }
    }
}
impl Drop for NetRef {
    fn drop(&mut self) {
        unsafe { ns3_put_net(self.0.as_ptr()) }
    }
}
pub(crate) struct Accepted(NonNull<c_void>);
// SAFETY: private native socket owner; shared access only returns its pinned
// Rust state. Transfer/drop are consuming operations, serialized by Rust.
unsafe impl Send for Accepted {}
unsafe impl Sync for Accepted {}
impl Accepted {
    pub(crate) fn state(&self) -> *mut c_void {
        unsafe { ns3_accepted_state(self.0.as_ptr()) }
    }
    pub(crate) fn transfer(self, target: &mut AcceptTarget<'_>) {
        unsafe { ns3_accept_transfer(self.0.as_ptr(), target.0) };
        core::mem::forget(self);
    }
}
impl Drop for Accepted {
    fn drop(&mut self) {
        unsafe { ns3_accept_drop(self.0.as_ptr()) }
    }
}
pub(crate) struct AcceptTarget<'a>(*mut c_void, core::marker::PhantomData<&'a mut c_void>);
impl AcceptTarget<'_> {
    pub(crate) unsafe fn new(p: *mut c_void) -> Self {
        Self(p, core::marker::PhantomData)
    }
}
pub(crate) fn sigpipe() {
    unsafe { ns3_sigpipe() }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Address(pub(crate) [u8; 24]);
impl Address {
    pub(crate) fn family(&self) -> i32 {
        match u16::from_le_bytes(self.0[..2].try_into().unwrap()) {
            4 => 2,
            6 => 10,
            _ => 0,
        }
    }
    pub(crate) fn from_wire(bytes: &[u8]) -> Result<Self> {
        let value = Self(bytes.try_into().map_err(|_| EPROTO)?);
        if !matches!(value.family(), 2 | 10) || value.0[20..24] != [0; 4] {
            return Err(EPROTO);
        }
        Ok(value)
    }
    pub(crate) fn from_native(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 2 {
            return Err(EINVAL);
        }
        let family = u16::from_ne_bytes(bytes[..2].try_into().unwrap());
        let mut a = Self::default();
        match family {
            2 if bytes.len() >= 16 => {
                a.0[0] = 4;
                a.0[4..8].copy_from_slice(&bytes[4..8]);
            }
            10 if bytes.len() >= 28 => {
                if bytes[24..28] != [0; 4] {
                    return Err(EOPNOTSUPP);
                }
                a.0[0] = 6;
                a.0[4..20].copy_from_slice(&bytes[8..24]);
            }
            _ => return Err(EINVAL),
        }
        a.0[2..4]
            .copy_from_slice(&u16::from_be_bytes(bytes[2..4].try_into().unwrap()).to_le_bytes());
        Ok(a)
    }
    pub(crate) fn native(&self) -> ([u8; 28], usize) {
        let mut out = [0; 28];
        out[..2].copy_from_slice(&(self.family() as u16).to_ne_bytes());
        out[2..4]
            .copy_from_slice(&u16::from_le_bytes(self.0[2..4].try_into().unwrap()).to_be_bytes());
        if self.family() == 2 {
            out[4..8].copy_from_slice(&self.0[4..8]);
            (out, 16)
        } else {
            out[8..24].copy_from_slice(&self.0[4..20]);
            (out, 28)
        }
    }
}
pub(crate) struct Message<'a> {
    header: &'a mut bindings::msghdr,
    pending_read: usize,
}
impl<'a> Message<'a> {
    /// Caller guarantees exclusive valid msghdr and iterator for this callback.
    pub(crate) unsafe fn new(p: *mut bindings::msghdr) -> Self {
        Self {
            header: unsafe { &mut *p },
            pending_read: 0,
        }
    }
    /// Reject ancillary semantics we cannot execute before any payload is admitted.
    pub(crate) fn validate_control(&self, udp: bool) -> Result {
        let len = self.header.msg_controllen;
        if len == 0 {
            return Ok(());
        }
        // SAFETY: this method is used only by the send callback; the union's
        // active arm is the kernel control pointer (never the receive user arm).
        let pointer = unsafe { self.header.__bindgen_anon_1.msg_control };
        if pointer.is_null() {
            return Err(EINVAL);
        }
        // SAFETY: sendmsg supplies a kernel-owned copied control buffer of this
        // length for the duration of the callback. No userspace pointers escape.
        let control = unsafe { core::slice::from_raw_parts(pointer.cast::<u8>(), len) };
        let header_len = core::mem::size_of::<bindings::cmsghdr>();
        let alignment = core::mem::size_of::<usize>();
        let mut offset = 0;
        let mut gso = false;
        let mut unsupported = false;
        while offset < len {
            let remaining = &control[offset..];
            if remaining.len() < header_len {
                return Err(EINVAL);
            }
            // SAFETY: header bounds were checked; unaligned read avoids assuming
            // alignment of each control message supplied by an application.
            let header = unsafe {
                remaining
                    .as_ptr()
                    .cast::<bindings::cmsghdr>()
                    .read_unaligned()
            };
            if header.cmsg_len < header_len || header.cmsg_len > remaining.len() {
                return Err(EINVAL);
            }
            // Linux SOL_UDP/UDP_SEGMENT carries a native-endian u16 segment size.
            if udp && header.cmsg_level == bindings::IPPROTO_UDP as i32 && header.cmsg_type == 103 {
                if header.cmsg_len != header_len + 2 {
                    return Err(EINVAL);
                }
                let size = u16::from_ne_bytes([remaining[header_len], remaining[header_len + 1]]);
                if size != 0 {
                    gso = true;
                } else {
                    unsupported = true;
                }
            } else {
                unsupported = true;
            }
            let step = header.cmsg_len.checked_add(alignment - 1).ok_or(EINVAL)? & !(alignment - 1);
            // Padding after the final complete message may be omitted.
            if step >= remaining.len() {
                break;
            }
            offset += step;
        }
        if unsupported {
            Err(EOPNOTSUPP)
        } else if gso {
            // Linux reports an unexecutable GSO request as EIO. curl then resends
            // individual datagrams. Never silently send the concatenated buffer.
            Err(EIO)
        } else {
            Ok(())
        }
    }
    pub(crate) fn flags(&self) -> u32 {
        self.header.msg_flags
    }
    pub(crate) fn name(&self) -> Result<Option<Address>> {
        if self.header.msg_name.is_null() {
            return Ok(None);
        }
        // SAFETY: kernel sendmsg bounds and copies msg_name before proto callback.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                self.header.msg_name.cast(),
                self.header.msg_namelen as usize,
            )
        };
        Address::from_native(bytes).map(Some)
    }
    pub(crate) fn read(&mut self, out: &mut [u8]) -> Result {
        // SAFETY: sendmsg supplies a source iterator exclusively borrowed here.
        let source = unsafe { IovIterSource::from_raw(&mut self.header.msg_iter) };
        let n = source.copy_from_iter(out);
        if n != out.len() {
            // SAFETY: reverting exactly the bytes this call just consumed.
            unsafe { source.revert(n) };
            return Err(EFAULT);
        }
        self.pending_read = n;
        Ok(())
    }
    pub(crate) fn commit_read(&mut self) {
        self.pending_read = 0;
    }
    pub(crate) fn rollback_read(&mut self) {
        // SAFETY: this count records only this Message's last uncommitted read.
        let source = unsafe { IovIterSource::from_raw(&mut self.header.msg_iter) };
        unsafe { source.revert(self.pending_read) };
        self.pending_read = 0;
    }
    pub(crate) fn write(&mut self, input: &[u8]) -> Result {
        // SAFETY: recvmsg supplies a destination iterator exclusively borrowed here.
        let dest = unsafe { IovIterDest::from_raw(&mut self.header.msg_iter) };
        let n = dest.copy_to_iter(input);
        if n != input.len() {
            // SAFETY: reverting precisely the bytes this operation consumed.
            unsafe { dest.revert(n) };
            return Err(EFAULT);
        }
        Ok(())
    }
    pub(crate) fn set_name(&mut self, a: Address) {
        if self.header.msg_name.is_null() {
            return;
        }
        let (bytes, len) = a.native();
        // SAFETY: recvmsg supplies kernel sockaddr_storage space when non-null.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.header.msg_name.cast(), len) };
        self.header.msg_namelen = len as i32;
    }
    pub(crate) fn truncated(&mut self) {
        self.header.msg_flags |= bindings::MSG_TRUNC;
    }
}
