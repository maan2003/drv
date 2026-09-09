// SPDX-License-Identifier: GPL-2.0-only

use linux_self_sandbox::{Error, Profile, Sandbox};
use std::ffi::CString;
use std::os::fd::RawFd;

fn main() {
    match run() {
        Ok(()) => {}
        Err(error) if error.is_namespace_permission_denied() => {
            println!(
                "sandbox_self_test=SKIP reason=kernel_namespace_permission_denied detail={error}"
            );
            std::process::exit(77);
        }
        Err(error) => {
            eprintln!("sandbox_self_test=FAIL detail={error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), Error> {
    let mode = std::env::var("SANDBOX_HELPER_MODE").expect("helper mode");
    let retained = fd_env("SANDBOX_RETAINED_FD");
    let unwanted = std::env::var("SANDBOX_UNWANTED_FD")
        .ok()
        .map(|v| v.parse().unwrap());
    let persistence = (mode == "wlancfg").then_some(retained);
    let setup = Sandbox::new().setup(&[retained], persistence)?;
    assert!(
        unsafe { libc::fcntl(retained, libc::F_GETFD) } >= 0,
        "retained fd was closed"
    );
    if let Some(fd) = unwanted {
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) },
            -1,
            "unwanted fd remained open"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
    }

    match mode.as_str() {
        "simulated" => {
            setup.lockdown(Profile::WifiSimulated)?.run(|| {
                let mut byte = 0u8;
                let mut iovec = libc::iovec { iov_base: (&mut byte as *mut u8).cast(), iov_len: 1 };
                let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
                message.msg_iov = &mut iovec;
                message.msg_iovlen = 1;
                assert_eq!(unsafe { libc::recvmsg(retained, &mut message, libc::MSG_DONTWAIT) }, 1);
                assert_eq!(byte, b'X', "setup consumed or changed socket data");
                std::thread::spawn(|| {}).join().expect("sandboxed worker thread");
                denied_probes(retained);
                println!("sandbox_self_test=PASS profile=wifi-simulated retained_fd=true unwanted_fd_closed=true setup_socket_unread=true worker_thread=true open_denied=true socket_denied=true ioctl_denied=true");
            });
        }
        "wlancfg" => setup.lockdown(Profile::Wlancfg { persistence_dir_fd: retained })?.run(|| {
            denied_open();
            let temporary = CString::new("saved-networks.tmp").unwrap();
            let installed = CString::new("saved-networks.bin").unwrap();
            let create_flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW;
            let file = unsafe { libc::openat(retained, temporary.as_ptr(), create_flags, 0o600) };
            assert!(file >= 0, "directory-relative create failed: {}", std::io::Error::last_os_error());
            assert_eq!(unsafe { libc::write(file, b"state".as_ptr().cast(), 5) }, 5);
            assert_eq!(unsafe { libc::fsync(file) }, 0);
            assert_eq!(unsafe { libc::close(file) }, 0);
            assert_eq!(unsafe { libc::renameat(retained, temporary.as_ptr(), retained, installed.as_ptr()) }, 0);
            assert_eq!(unsafe { libc::fsync(retained) }, 0);
            let read_flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
            for escape in ["../sibling/secret", "escape/secret"] {
                let escape = CString::new(escape).unwrap();
                assert_eq!(unsafe { libc::openat(retained, escape.as_ptr(), read_flags) }, -1);
                assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ENOENT));
            }
            let file = unsafe { libc::openat(retained, installed.as_ptr(), read_flags) };
            assert!(file >= 0, "directory-relative reopen failed: {}", std::io::Error::last_os_error());
            let mut bytes = [0u8; 5];
            assert_eq!(unsafe { libc::read(file, bytes.as_mut_ptr().cast(), bytes.len()) }, 5);
            assert_eq!(&bytes, b"state");
            assert_eq!(unsafe { libc::close(file) }, 0);
            assert_eq!(unsafe { libc::unlinkat(retained, installed.as_ptr(), 0) }, 0);
            println!("sandbox_self_test=PASS profile=wlancfg ambient_open_denied=true parent_escape_denied=true symlink_escape_denied=true directory_create=true atomic_replace=true");
        }),
        _ => panic!("unknown helper mode"),
    }
    Ok(())
}

fn fd_env(name: &str) -> RawFd {
    std::env::var(name).unwrap().parse().unwrap()
}

fn denied_open() {
    let path = c"/etc/passwd";
    assert_eq!(
        unsafe { libc::openat(libc::AT_FDCWD, path.as_ptr(), libc::O_RDONLY) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    );
}

fn denied_probes(fd: RawFd) {
    denied_open();
    assert_eq!(
        unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    );
    assert_eq!(
        unsafe { libc::ioctl(fd, 0x5413, std::ptr::null_mut::<libc::c_void>()) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    );
}
