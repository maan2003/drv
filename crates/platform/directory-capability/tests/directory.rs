// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

use directory_capability::Directory;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "drv-directory-capability-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn capability(&self) -> Directory {
        Directory::new(OpenOptions::new().read(true).open(&self.0).unwrap()).unwrap()
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn rejects_traversal_and_non_components() {
    let temp = TempDir::new();
    let directory = temp.capability();
    for name in ["", ".", "..", "../escape", "child/name", "nul\0name"] {
        assert_eq!(
            directory.open_existing_regular(name).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            directory
                .create_new_regular(name, 0o600)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            directory.unlink(name).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn rejects_symlinks_and_special_files_without_blocking() {
    let temp = TempDir::new();
    std::fs::write(temp.0.join("target"), b"secret").unwrap();
    symlink("target", temp.0.join("link")).unwrap();
    let fifo = std::ffi::CString::new(temp.0.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

    let directory = temp.capability();
    assert!(directory.open_existing_regular("link").is_err());
    assert!(directory.replace("link", "installed").is_err());
    assert!(directory.open_existing_regular("fifo").is_err());
}

#[test]
fn creates_replaces_unlinks_and_returns_owned_files() {
    let temp = TempDir::new();
    let directory = temp.capability();
    let mut replacement = directory.create_new_regular("temporary", 0o600).unwrap();
    replacement.write_all(b"new data").unwrap();
    replacement.sync_all().unwrap();
    directory.replace("temporary", "data").unwrap();
    directory.sync().unwrap();

    let mut installed = directory.open_existing_regular("data").unwrap();
    let mut bytes = Vec::new();
    installed.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"new data");
    directory.unlink("data").unwrap();
    directory.sync().unwrap();
    assert!(!temp.0.join("data").exists());
    drop(File::open(&temp.0).unwrap());
}
