// SPDX-License-Identifier: GPL-2.0-only
//! DNS-local Linux boundary. Setup is single-threaded; runtime is not.
//! Keeps the served network namespace, unlike the device/WLAN profiles.
//! Only stdio, bounded read-only config/CA and local listeners survive setup.
use std::{
    fs::File,
    io,
    net::{TcpListener, UdpSocket},
    os::fd::FromRawFd,
    os::unix::net::UnixListener,
};

pub struct Capabilities {
    pub config: File,
    pub ca: File,
    pub udp: Vec<UdpSocket>,
    pub tcp: Vec<TcpListener>,
    pub nss: UnixListener,
}
fn check(result: libc::c_long) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn input(fd: i32, max: i64) -> io::Result<()> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    unsafe {
        check(libc::fstat(fd, stat.as_mut_ptr()) as _)?;
        let stat = stat.assume_init();
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG
            || stat.st_size <= 0
            || stat.st_size > max
            || flags < 0
            || flags & libc::O_ACCMODE != libc::O_RDONLY
        {
            return Err(io::Error::other("invalid config/CA descriptor"));
        }
    }
    Ok(())
}
pub fn lockdown() -> io::Result<Capabilities> {
    if std::fs::read_dir("/proc/self/task")?.take(2).count() != 1 {
        return Err(io::Error::other("sandbox installation requires one thread"));
    }
    // Only pipe-backed stdout/stderr: no inherited directory/device/file authority.
    for fd in [1, 2] {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        unsafe {
            check(libc::fstat(fd, stat.as_mut_ptr()) as _)?;
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if stat.assume_init().st_mode & libc::S_IFMT != libc::S_IFIFO
                || flags < 0
                || flags & libc::O_ACCMODE != libc::O_WRONLY
            {
                return Err(io::Error::other("stdout/stderr must be write-only pipes"));
            }
            // Replace arbitrary stdin with an EOF pipe, without consuming it.
        }
    }
    input(3, 8192)?;
    input(4, 2097152)?;
    let inherited_nss = std::env::var_os("DRV_DNS_INHERIT_NSS").is_some();
    let nss = if inherited_nss {
        // FD 5 is a pre-bound listener from the trusted service manager.
        for (option, expected) in [
            (libc::SO_DOMAIN, libc::AF_UNIX),
            (libc::SO_TYPE, libc::SOCK_STREAM),
            (libc::SO_ACCEPTCONN, 1),
        ] {
            let mut value = 0i32;
            let mut len = std::mem::size_of_val(&value) as libc::socklen_t;
            unsafe {
                check(libc::getsockopt(5, libc::SOL_SOCKET, option,
                    (&mut value as *mut i32).cast(), &mut len) as _)?;
            }
            if value != expected {
                return Err(io::Error::other("invalid inherited NSS listener"));
            }
        }
        Some(unsafe { UnixListener::from_raw_fd(5) })
    } else {
        None
    };
    unsafe {
        check(libc::syscall(libc::SYS_close_range,
            if inherited_nss { 6u32 } else { 5u32 }, u32::MAX, 0))?;
    }
    unsafe {
        let mut pipe = [-1; 2];
        check(libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) as _)?;
        check(libc::dup2(pipe[0], 0) as _)?;
        check(libc::close(pipe[0]) as _)?;
        check(libc::close(pipe[1]) as _)?;
    }
    let mut udp = Vec::new();
    let mut tcp = Vec::new();
    for address in ["127.0.0.1:53", "[::1]:53"] {
        let u = UdpSocket::bind(address)?;
        u.set_nonblocking(true)?;
        udp.push(u);
        let t = TcpListener::bind(address)?;
        t.set_nonblocking(true)?;
        tcp.push(t);
    }
    // The trusted launcher owns stale-path removal after the previous process exits.
    let nss = match nss {
        Some(listener) => listener,
        None => {
            let listener = UnixListener::bind(drv_dns_wire::PATH)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(drv_dns_wire::PATH, std::fs::Permissions::from_mode(0o666))?;
            listener
        }
    };
    nss.set_nonblocking(true)?;
    unsafe {
        check(libc::unshare(libc::CLONE_NEWNS) as _)?;
        check(libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            (libc::MS_REC | libc::MS_PRIVATE) as _,
            std::ptr::null(),
        ) as _)?;
        check(libc::mount(
            c"tmpfs".as_ptr(),
            c"/tmp".as_ptr(),
            c"tmpfs".as_ptr(),
            (libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC) as _,
            c"size=4096,mode=0555".as_ptr().cast(),
        ) as _)?;
        check(libc::chdir(c"/tmp".as_ptr()) as _)?;
        check(libc::chroot(c".".as_ptr()) as _)?;
        check(libc::chdir(c"/".as_ptr()) as _)?;
        for (resource, value) in [
            (libc::RLIMIT_NOFILE, 256),
            (libc::RLIMIT_CORE, 0),
            (libc::RLIMIT_AS, 512 * 1024 * 1024),
        ] {
            check(libc::setrlimit(
                resource,
                &libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                },
            ) as _)?;
        }
        check(libc::setgroups(0, std::ptr::null()) as _)?;
        for cap in 0..64 {
            if libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) == 0 {
                continue;
            }
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            return Err(io::Error::last_os_error());
        }
        check(libc::setresgid(65534, 65534, 65534) as _)?;
        check(libc::setresuid(65534, 65534, 65534) as _)?;
        #[repr(C)]
        struct Header {
            version: u32,
            pid: i32,
        }
        #[repr(C)]
        #[derive(Default, Clone, Copy)]
        struct Caps {
            effective: u32,
            permitted: u32,
            inheritable: u32,
        }
        let mut header = Header {
            version: 0x20080522,
            pid: 0,
        };
        let mut caps = [Caps::default(); 2];
        check(libc::syscall(libc::SYS_capset, &header, caps.as_ptr()))?;
        check(libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        ) as _)?;
        check(libc::syscall(
            libc::SYS_capget,
            &mut header,
            caps.as_mut_ptr(),
        ))?;
        if caps
            .iter()
            .any(|c| c.effective | c.permitted | c.inheritable != 0)
        {
            return Err(io::Error::other("capabilities not cleared"));
        }
        check(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) as _)?;
    }
    install()?;
    Ok(Capabilities {
        config: unsafe { File::from_raw_fd(3) },
        ca: unsafe { File::from_raw_fd(4) },
        udp,
        tcp,
        nss,
    })
}
const ALLOW: u32 = 0x7fff0000;
const KILL: u32 = 0x80000000;
fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}
fn eq(k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: 0x15,
        jt,
        jf,
        k,
    }
}
fn arg(n: u32) -> libc::sock_filter {
    stmt(0x20, 16 + 8 * n)
}
fn allow(f: &mut Vec<libc::sock_filter>, nr: libc::c_long) {
    f.extend([eq(nr as u32, 0, 1), stmt(6, ALLOW)]);
}
fn rule(f: &mut Vec<libc::sock_filter>, nr: libc::c_long, body: Vec<libc::sock_filter>) {
    f.push(eq(nr as u32, 0, body.len().try_into().unwrap()));
    f.extend(body);
    f.push(stmt(0x20, 0));
}
fn install() -> io::Result<()> {
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000003e;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc00000b7;
    let mut f = vec![stmt(0x20, 4), eq(ARCH, 1, 0), stmt(6, KILL), stmt(0x20, 0)];
    for nr in [libc::SYS_openat, libc::SYS_readlinkat, libc::SYS_statx] {
        rule(&mut f, nr, vec![stmt(6, 0x50000 | libc::ENOENT as u32)]);
    }
    #[cfg(target_arch = "x86_64")]
    for nr in [libc::SYS_open, libc::SYS_readlink] {
        rule(&mut f, nr, vec![stmt(6, 0x50000 | libc::ENOENT as u32)]);
    }
    rule(
        &mut f,
        libc::SYS_clone3,
        vec![stmt(6, 0x50000 | libc::ENOSYS as u32)],
    );
    rule(
        &mut f,
        libc::SYS_prctl,
        vec![stmt(6, 0x50000 | libc::EINVAL as u32)],
    );
    let clone_flags = libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_SETTLS
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_CLEARTID;
    rule(
        &mut f,
        libc::SYS_clone,
        vec![
            arg(0),
            eq(clone_flags as u32, 0, 1),
            stmt(6, ALLOW),
            stmt(6, KILL),
        ],
    );
    rule(
        &mut f,
        libc::SYS_socket,
        vec![
            arg(0),
            eq(libc::AF_INET as _, 2, 0),
            eq(libc::AF_INET6 as _, 1, 0),
            stmt(6, KILL),
            arg(1),
            stmt(0x54, !(libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC) as u32),
            eq(libc::SOCK_STREAM as _, 1, 0),
            eq(libc::SOCK_DGRAM as _, 0, 1),
            stmt(6, ALLOW),
            stmt(6, KILL),
        ],
    );
    // Possession of a listening socket authorizes accept; its FD number is irrelevant.
    allow(&mut f, libc::SYS_accept4);
    let mut fcntl = vec![arg(1)];
    for cmd in [
        libc::F_GETFD,
        libc::F_SETFD,
        libc::F_GETFL,
        libc::F_SETFL,
        libc::F_DUPFD_CLOEXEC,
    ] {
        fcntl.extend([eq(cmd as _, 0, 1), stmt(6, ALLOW)]);
    }
    fcntl.push(stmt(6, KILL));
    rule(&mut f, libc::SYS_fcntl, fcntl);
    rule(
        &mut f,
        libc::SYS_ioctl,
        vec![
            arg(1),
            eq(libc::FIONBIO as _, 0, 1),
            stmt(6, ALLOW),
            stmt(6, KILL),
        ],
    );
    // glibc probes MAP_DROPPABLE for vDSO getrandom state. Decline the
    // optional mapping; ordinary getrandom remains permitted.
    rule(
        &mut f,
        libc::SYS_mmap,
        vec![
            arg(3),
            eq(0x08 | libc::MAP_ANONYMOUS as u32, 0, 1),
            stmt(6, 0x50000 | libc::EINVAL as u32),
            arg(2),
            libc::sock_filter {
                code: 0x45,
                k: libc::PROT_EXEC as _,
                jt: 0,
                jf: 1,
            },
            stmt(6, KILL),
            arg(3),
            stmt(
                0x54,
                !(libc::MAP_PRIVATE
                    | libc::MAP_ANONYMOUS
                    | libc::MAP_STACK
                    | libc::MAP_FIXED
                    | libc::MAP_NORESERVE) as u32,
            ),
            eq(0, 0, 3),
            arg(3),
            stmt(0x54, (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as _),
            eq((libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as _, 1, 0),
            stmt(6, KILL),
            stmt(6, ALLOW),
        ],
    );
    rule(
        &mut f,
        libc::SYS_mremap,
        vec![
            arg(3),
            stmt(0x54, !(libc::MREMAP_MAYMOVE as u32)),
            eq(0, 0, 1),
            stmt(6, ALLOW),
            stmt(6, KILL),
        ],
    );
    rule(
        &mut f,
        libc::SYS_mprotect,
        vec![
            arg(2),
            libc::sock_filter {
                code: 0x45,
                k: libc::PROT_EXEC as _,
                jt: 0,
                jf: 1,
            },
            stmt(6, KILL),
            stmt(6, ALLOW),
        ],
    );
    rule(
        &mut f,
        libc::SYS_prlimit64,
        vec![
            arg(0),
            eq(0, 0, 5),
            arg(2),
            eq(0, 0, 3),
            stmt(0x20, 16 + 8 * 2 + 4),
            eq(0, 0, 1),
            stmt(6, ALLOW),
            stmt(6, KILL),
        ],
    );
    rule(
        &mut f,
        libc::SYS_tgkill,
        vec![
            arg(0),
            eq(unsafe { libc::getpid() } as _, 0, 1),
            stmt(6, ALLOW),
            stmt(6, KILL),
        ],
    );
    for nr in [
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_close,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_lseek,
        libc::SYS_munmap,
        libc::SYS_madvise,
        libc::SYS_brk,
        libc::SYS_futex,
        libc::SYS_clock_gettime,
        libc::SYS_nanosleep,
        libc::SYS_clock_nanosleep,
        libc::SYS_getrandom,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_sched_yield,
        libc::SYS_sched_getaffinity,
        libc::SYS_sigaltstack,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_set_robust_list,
        libc::SYS_rseq,
        libc::SYS_restart_syscall,
        libc::SYS_epoll_create1,
        libc::SYS_epoll_ctl,
        libc::SYS_epoll_pwait,
        libc::SYS_eventfd2,
        libc::SYS_connect,
        libc::SYS_shutdown,
        libc::SYS_getsockname,
        libc::SYS_getpeername,
        libc::SYS_getsockopt,
        libc::SYS_setsockopt,
        libc::SYS_sendto,
        libc::SYS_recvfrom,
        libc::SYS_ppoll,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ] {
        allow(&mut f, nr);
    }
    #[cfg(target_arch = "x86_64")]
    for nr in [libc::SYS_epoll_wait, libc::SYS_poll] {
        allow(&mut f, nr);
    }
    f.push(stmt(6, KILL));
    let program = libc::sock_fprog {
        len: f.len() as _,
        filter: f.as_mut_ptr(),
    };
    let result = unsafe { libc::syscall(libc::SYS_seccomp, 1, 1, &program) };
    if result == 0 {
        Ok(())
    } else if result > 0 {
        Err(io::Error::other("seccomp thread synchronization failed"))
    } else {
        Err(io::Error::last_os_error())
    }
}
