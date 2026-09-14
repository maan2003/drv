// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Minimal file operations relative to an owned Linux directory capability.
//!
//! This crate deliberately provides no ambient paths, traversal, directory
//! enumeration, or generic open-options framework. Names are exactly one UTF-8
//! relative component. The directory must be controlled against concurrent
//! mutation by untrusted writers.
//!
//! This API is defense in depth for pathname confinement. It does not replace a
//! mount namespace, cleanup of inherited file descriptors, or seccomp policy.

#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};

/// An owned, validated directory descriptor.
#[derive(Debug)]
pub struct Directory {
    file: File,
}

impl Directory {
    /// Takes ownership of a directory that supports durable synchronization.
    pub fn new(file: File) -> io::Result<Self> {
        if !file.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "capability is not a directory",
            ));
        }
        file.sync_all()?;
        Ok(Self { file })
    }

    /// Opens an existing regular file without following a final symlink.
    ///
    /// Nonblocking open prevents a substituted FIFO or device from blocking the
    /// caller before its file type can be checked.
    pub fn open_existing_regular(&self, name: &str) -> io::Result<File> {
        let name = component(name)?;
        let file = self.openat(
            &name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )?;
        require_regular(file)
    }

    /// Creates a new mode-restricted regular file, failing if the name exists.
    pub fn create_new_regular(&self, name: &str, mode: u32) -> io::Result<File> {
        let name = component(name)?;
        if mode & !0o777 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mode contains bits other than permissions",
            ));
        }
        let file = self.openat(
            &name,
            libc::O_WRONLY
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK,
            mode as libc::mode_t,
        )?;
        require_regular(file)
    }

    /// Atomically replaces `destination` with the existing regular `source`.
    pub fn replace(&self, source: &str, destination: &str) -> io::Result<()> {
        let source = component(source)?;
        let destination = component(destination)?;
        // Reject a source symlink or special file. The capability directory is
        // required to exclude concurrent untrusted writers after this check.
        drop(require_regular(self.openat(
            &source,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )?)?);
        let result = unsafe {
            // SAFETY: both validated C strings are single relative components.
            libc::renameat(
                self.file.as_raw_fd(),
                source.as_ptr(),
                self.file.as_raw_fd(),
                destination.as_ptr(),
            )
        };
        cvt(result)
    }

    /// Removes one directory entry without following it.
    pub fn unlink(&self, name: &str) -> io::Result<()> {
        let name = component(name)?;
        let result = unsafe {
            // SAFETY: name is a validated single relative component.
            libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0)
        };
        cvt(result)
    }

    /// Durably synchronizes directory entry changes.
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    fn openat(&self, name: &CString, flags: i32, mode: libc::mode_t) -> io::Result<File> {
        let fd = unsafe {
            // SAFETY: name is NUL-terminated and the returned descriptor is
            // transferred exactly once to File.
            libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags, mode)
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe {
                // SAFETY: openat returned a new owned descriptor.
                File::from_raw_fd(fd)
            })
        }
    }
}

fn component(name: &str) -> io::Result<CString> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "name is not one relative path component",
        ));
    }
    CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains an interior NUL"))
}

fn require_regular(file: File) -> io::Result<File> {
    if file.metadata()?.is_file() {
        Ok(file)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory entry is not a regular file",
        ))
    }
}

fn cvt(result: i32) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
