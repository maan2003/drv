//! One common glibc pointer adapter; exported aliases contain no policy.
//! Safety contract: valid NUL-terminated input and writable, disjoint output
//! objects/buffer, as required by glibc NSS. Arbitrary foreign pointers cannot
//! be validated by Rust. All returned pointers refer to caller-owned storage.
#![deny(unsafe_op_in_unsafe_fn)]
use crate::safe::{self, Failure};
use std::{
    ffi::{CStr, c_char, c_int},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
};

unsafe fn dispatch(
    name: *const c_char,
    family: c_int,
    result: *mut libc::hostent,
    buffer: *mut c_char,
    buflen: usize,
    errnop: *mut c_int,
    herrnop: *mut c_int,
    ttlp: *mut i32,
    canonp: *mut *mut c_char,
) -> c_int {
    // SAFETY: the NSS caller supplies the valid, nonoverlapping objects described
    // above. Null and oversized inputs are rejected before conversion. Safe Rust
    // computes every offset, checks capacity and initializes address/name bytes;
    // only the typed C pointer tables and output objects are written here.
    unsafe {
        let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), Failure> {
            if name.is_null()
                || result.is_null()
                || buffer.is_null()
                || errnop.is_null()
                || herrnop.is_null()
                || buflen > isize::MAX as usize
            {
                return Err(safe::INVALID);
            }
            let length = (0..=254)
                .find(|offset| name.add(*offset).read() == 0)
                .ok_or(safe::INVALID)?;
            let bytes = std::slice::from_raw_parts(name.cast::<u8>(), length + 1);
            let name = CStr::from_bytes_with_nul(bytes)
                .map_err(|_| safe::INVALID)?
                .to_str()
                .map_err(|_| safe::INVALID)?;
            let host = safe::resolve(name, family)?;
            let bytes =
                std::slice::from_raw_parts_mut(buffer.cast::<std::mem::MaybeUninit<u8>>(), buflen);
            let layout = safe::pack(host, bytes)?;
            let aliases = buffer.add(layout.aliases).cast::<*mut c_char>();
            let list = buffer.add(layout.list).cast::<*mut c_char>();
            aliases.write(ptr::null_mut());
            for (i, offset) in layout.addresses.iter().enumerate() {
                list.add(i).write(buffer.add(*offset));
            }
            list.add(layout.addresses.len()).write(ptr::null_mut());
            let name = buffer.add(layout.name);
            result.write(libc::hostent {
                h_name: name,
                h_aliases: aliases,
                h_addrtype: layout.family,
                h_length: layout.address_length,
                h_addr_list: list,
            });
            if !ttlp.is_null() {
                ttlp.write(0);
            }
            if !canonp.is_null() {
                canonp.write(name);
            }
            Ok(())
        }))
        .unwrap_or(Err(safe::INTERNAL));
        let (status, errno, herrno) = match outcome {
            Ok(()) => (1, 0, 0),
            Err(Failure {
                status,
                errno,
                herrno,
            }) => (status, errno, herrno),
        };
        if !errnop.is_null() {
            errnop.write(errno);
        }
        if !herrnop.is_null() {
            herrnop.write(herrno);
        }
        status
    }
}

/// Safety: see this module's glibc NSS caller contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _nss_drv_gethostbyname3_r(
    name: *const c_char,
    family: c_int,
    result: *mut libc::hostent,
    buffer: *mut c_char,
    buflen: usize,
    errnop: *mut c_int,
    herrnop: *mut c_int,
    ttlp: *mut i32,
    canonp: *mut *mut c_char,
) -> c_int {
    unsafe {
        dispatch(
            name, family, result, buffer, buflen, errnop, herrnop, ttlp, canonp,
        )
    }
}
/// Safety: see this module's glibc NSS caller contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _nss_drv_gethostbyname2_r(
    name: *const c_char,
    family: c_int,
    result: *mut libc::hostent,
    buffer: *mut c_char,
    buflen: usize,
    errnop: *mut c_int,
    herrnop: *mut c_int,
) -> c_int {
    unsafe {
        dispatch(
            name,
            family,
            result,
            buffer,
            buflen,
            errnop,
            herrnop,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    }
}
/// Safety: see this module's glibc NSS caller contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _nss_drv_gethostbyname_r(
    name: *const c_char,
    result: *mut libc::hostent,
    buffer: *mut c_char,
    buflen: usize,
    errnop: *mut c_int,
    herrnop: *mut c_int,
) -> c_int {
    unsafe {
        dispatch(
            name,
            libc::AF_INET,
            result,
            buffer,
            buflen,
            errnop,
            herrnop,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    }
}
