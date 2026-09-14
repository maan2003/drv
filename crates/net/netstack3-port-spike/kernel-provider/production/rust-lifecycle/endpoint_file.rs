// SPDX-License-Identifier: GPL-2.0-only
//! Typed anonymous endpoint files. FD reservation/install uses upstream RAII.
use core::{ffi::c_void, marker::PhantomData, ptr::NonNull};
use kernel::ffi;
use kernel::{
    bindings,
    error::from_err_ptr,
    fs::File,
    prelude::*,
    sync::{aref::ARef, poll::{PollTable, PollCondVar}, Arc},
    types::ForeignOwnable,
    uaccess::{UserPtr, UserSlice, UserSliceReader, UserSliceWriter},
};

/// Poll callback borrow of a live file, with no file-position access.
/// Unlike &File this does not assert exclusion of concurrent fdget_pos calls.
pub(crate) struct Poll<'a> {
    file: *mut bindings::file,
    table: PollTable<'a>,
}
impl<'a> Poll<'a> {
    /// Both pointers are valid for this callback; table may be null.
    pub(crate) unsafe fn new(file: *mut bindings::file, table: *mut bindings::poll_table) -> Self {
        Self { file, table: unsafe { PollTable::from_raw(table) } }
    }
    pub(crate) fn register(&self, cv: &PollCondVar) {
        // SAFETY: callback's VFS file reference stays live throughout this borrow.
        unsafe { self.table.register_wait_raw(self.file, cv) }
    }
}

pub(crate) trait Endpoint: Send + Sync + 'static {
    fn release(&self) {}
    fn read(&self, _out: &mut UserSliceWriter) -> Result<usize> {
        Err(EOPNOTSUPP)
    }
    fn write(&self, _input: &mut UserSliceReader) -> Result<usize> {
        Err(EOPNOTSUPP)
    }
    fn poll(&self, _poll: &Poll<'_>) -> u32 {
        0
    }
    fn ioctl(&self, _cmd: u32, _arg: usize) -> Result<isize> {
        Err(ENOTTY)
    }
}
unsafe extern "C" {
    fn nsrl_anon_file(ops: *const c_void, data: *mut c_void) -> *mut c_void;
}
pub(crate) fn create<T: Endpoint>(endpoint: Arc<T>) -> Result<ARef<File>> {
    let data = endpoint.into_foreign();
    // SAFETY: VTable matches Arc<T>; successful file creation owns data until release.
    let ops: &'static bindings::file_operations = &const { operations::<T>() };
    let raw = unsafe { nsrl_anon_file(core::ptr::from_ref(ops).cast(), data) };
    match from_err_ptr(raw) {
        Ok(raw) => {
            // SAFETY: anon_inode_getfile returned one live file reference.
            Ok(unsafe { ARef::from_raw(NonNull::new_unchecked(raw.cast())) })
        }
        Err(error) => {
            // SAFETY: failure did not consume private data or invoke release.
            drop(unsafe { Arc::<T>::from_foreign(data) });
            Err(error)
        }
    }
}
pub(crate) const fn operations<T: Endpoint>() -> bindings::file_operations {
    VTable::<T>::OPS
}

struct VTable<T>(PhantomData<T>);
impl<T: Endpoint> VTable<T> {
    // SAFETY of all callbacks: VFS keeps file alive and excludes final release.
    // private_data was created from Arc<T> by create(); borrows never escape.
    unsafe extern "C" fn read(
        file: *mut bindings::file,
        buf: *mut ffi::c_char,
        len: usize,
        _offset: *mut i64,
    ) -> isize {
        let value = unsafe { Arc::<T>::borrow((*file).private_data) };
        value
            .read(&mut UserSlice::new(UserPtr::from_ptr(buf.cast()), len).writer())
            .map(|n| n as isize)
            .unwrap_or_else(|e| e.to_errno() as isize)
    }
    unsafe extern "C" fn write(
        file: *mut bindings::file,
        buf: *const ffi::c_char,
        len: usize,
        _offset: *mut i64,
    ) -> isize {
        let value = unsafe { Arc::<T>::borrow((*file).private_data) };
        value
            .write(&mut UserSlice::new(UserPtr::from_addr(buf as usize), len).reader())
            .map(|n| n as isize)
            .unwrap_or_else(|e| e.to_errno() as isize)
    }
    unsafe extern "C" fn poll(file: *mut bindings::file, table: *mut bindings::poll_table) -> u32 {
        let value = unsafe { Arc::<T>::borrow((*file).private_data) };
        // SAFETY: VFS keeps file/table live until the callback returns.
        value.poll(&unsafe { Poll::new(file, table) })
    }
    unsafe extern "C" fn ioctl(file: *mut bindings::file, cmd: u32, arg: usize) -> isize {
        let value = unsafe { Arc::<T>::borrow((*file).private_data) };
        value
            .ioctl(cmd, arg)
            .unwrap_or_else(|e| e.to_errno() as isize)
    }
    unsafe extern "C" fn release(_inode: *mut bindings::inode, file: *mut bindings::file) -> i32 {
        // SAFETY: the final file release consumes its single foreign owner.
        let owner = unsafe { Arc::<T>::from_foreign((*file).private_data) };
        owner.release();
        drop(owner);
        0
    }
    const OPS: bindings::file_operations = bindings::file_operations {
        read: Some(Self::read),
        write: Some(Self::write),
        poll: Some(Self::poll),
        unlocked_ioctl: Some(Self::ioctl),
        release: Some(Self::release),
        // SAFETY: optional callbacks and flags may all be zero.
        ..unsafe { core::mem::zeroed() }
    };
}
