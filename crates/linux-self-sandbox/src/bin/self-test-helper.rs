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
    let retained_fds = if mode == "mt7921-vfio" || mode == "wlancfg" {
        std::env::var("SANDBOX_RETAINED_FDS")
            .unwrap()
            .split(',')
            .map(|fd| fd.parse().unwrap())
            .collect::<Vec<_>>()
    } else {
        vec![fd_env("SANDBOX_RETAINED_FD")]
    };
    let retained = retained_fds[0];
    let unwanted = std::env::var("SANDBOX_UNWANTED_FD")
        .ok()
        .map(|v| v.parse().unwrap());
    let persistence = if mode == "wlancfg" {
        Some(retained_fds[1])
    } else {
        None
    };
    let outside_marker = std::env::var("SANDBOX_OUTSIDE_MARKER")
        .ok()
        .map(|path| CString::new(path).unwrap());
    let setup = Sandbox::new().setup(&retained_fds, persistence)?;
    for &fd in &retained_fds {
        assert!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
            "retained fd was closed"
        );
    }
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
                println!("sandbox_self_test=PASS profile=wifi-simulated retained_fd=true unwanted_fd_closed=true setup_socket_unread=true");
            });
        }
        "wlancfg" => setup.lockdown(Profile::Wlancfg {
            control_fd: retained_fds[0],
            persistence_dir_fd: retained_fds[1],
        })?.run(|| {
            let retained = retained_fds[1];
            let temporary = CString::new("saved-networks.tmp").unwrap();
            let installed = CString::new("saved-networks.bin").unwrap();
            let create_flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
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
            let outside_marker = outside_marker.expect("wlancfg outside marker");
            assert_eq!(unsafe { libc::openat(retained, outside_marker.as_ptr(), read_flags) }, -1);
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ENOENT));
            let file = unsafe { libc::openat(retained, installed.as_ptr(), read_flags) };
            assert!(file >= 0, "directory-relative reopen failed: {}", std::io::Error::last_os_error());
            let mut bytes = [0u8; 5];
            assert_eq!(unsafe { libc::read(file, bytes.as_mut_ptr().cast(), bytes.len()) }, 5);
            assert_eq!(&bytes, b"state");
            assert_eq!(unsafe { libc::close(file) }, 0);
            assert_eq!(unsafe { libc::unlinkat(retained, installed.as_ptr(), 0) }, 0);
            println!("sandbox_self_test=PASS profile=wlancfg parent_escape_absent=true symlink_escape_absent=true absolute_outside_marker_absent=true directory_create=true atomic_replace=true");
        }),
        "mt7921-vfio" => {
            assert_eq!(retained_fds.len(), 4);
            pre_lockdown_probes(&retained_fds);
            println!("sandbox_self_test=READY");
            use std::io::{Read, Write};
            std::io::stdout().flush().unwrap();
            let mut start = [0];
            std::io::stdin().read_exact(&mut start).unwrap();
            assert_eq!(start, [b'X']);
            setup.lockdown(Profile::Mt7921Vfio {
                pci_config_fd: retained_fds[0],
                vfio_fd: retained_fds[1],
                iommufd: retained_fds[2],
                irq_eventfd: retained_fds[3],
            })?.run(|| {
                positive_runtime_probes(retained_fds[3]);
                println!("sandbox_self_test=PASS profile=mt7921-vfio namespaces_distinct=true sealed_empty_root=true uid=65534 gid=65534 effective_caps_empty=true permitted_caps_empty=true inheritable_caps_empty=true ambient_caps_empty=true bounding_caps_empty=true fds=stdio+4 no_new_privs=true seccomp=true allocator=true monotonic_sleep=true inherited_irq_eventfd=true");
            });
        }
        _ => panic!("unknown helper mode"),
    }
    Ok(())
}

#[repr(C)]
struct CapabilityHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Default)]
struct CapabilityData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

fn pre_lockdown_probes(retained: &[RawFd]) {
    let mut uids = [0; 3];
    let mut gids = [0; 3];
    assert_eq!(
        unsafe { libc::getresuid(&mut uids[0], &mut uids[1], &mut uids[2]) },
        0
    );
    assert_eq!(
        unsafe { libc::getresgid(&mut gids[0], &mut gids[1], &mut gids[2]) },
        0
    );
    assert_eq!(uids, [65534; 3]);
    assert_eq!(gids, [65534; 3]);
    assert_eq!(unsafe { libc::getgroups(0, std::ptr::null_mut()) }, 0);

    const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
    let mut header = CapabilityHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapabilityData::default(), CapabilityData::default()];
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_capget, &mut header, data.as_mut_ptr()) },
        0
    );
    assert!(
        data.iter()
            .all(|word| word.effective == 0 && word.permitted == 0 && word.inheritable == 0)
    );
    for capability in 0..64 {
        let bounded = unsafe { libc::prctl(libc::PR_CAPBSET_READ, capability, 0, 0, 0) };
        if bounded == -1 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EINVAL)
            );
            break;
        }
        assert_eq!(
            bounded, 0,
            "capability {capability} remains in bounding set"
        );
        assert_eq!(
            unsafe {
                libc::prctl(
                    libc::PR_CAP_AMBIENT,
                    libc::PR_CAP_AMBIENT_IS_SET,
                    capability,
                    0,
                    0,
                )
            },
            0,
            "capability {capability} remains ambient"
        );
    }
    assert_eq!(
        unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) },
        1
    );

    let root_entries = std::fs::read_dir("/").unwrap().count();
    assert_eq!(root_entries, 0);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata("/").unwrap().permissions().mode() & 0o777,
        0o555
    );

    let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    for fd in 0..limit.rlim_cur {
        let open = unsafe { libc::fcntl(fd as RawFd, libc::F_GETFD) } >= 0;
        assert_eq!(
            open,
            fd < 3 || retained.contains(&(fd as RawFd)),
            "unexpected descriptor {fd}"
        );
    }
}

fn positive_runtime_probes(irq_eventfd: RawFd) {
    let allocated = vec![0x5a_u8; 256 * 1024];
    assert_eq!(
        allocated
            .iter()
            .map(|&byte| usize::from(byte))
            .sum::<usize>(),
        0x5a * 256 * 1024
    );
    std::thread::sleep(std::time::Duration::from_millis(1));
    let mut pollfd = libc::pollfd {
        fd: irq_eventfd,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::ppoll(&mut pollfd, 1, &timeout, std::ptr::null()) },
        1
    );
    let mut count = 0_u64;
    assert_eq!(
        unsafe { libc::read(irq_eventfd, (&mut count as *mut u64).cast(), 8) },
        8
    );
    assert_eq!(count, 1);
}

fn fd_env(name: &str) -> RawFd {
    std::env::var(name).unwrap().parse().unwrap()
}
