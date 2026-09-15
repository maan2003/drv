// SPDX-License-Identifier: GPL-2.0-only

use crate::{Socks5Service, NetworkPoller, ServiceEthernetDevice};
use std::env;
use std::ffi::{c_char, c_void};
use std::net::{SocketAddr, TcpListener};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
#[cfg(test)]
use std::time::{Duration, Instant};

const FRAME_FD: i32 = 3;
const LISTENER_FD: i32 = 4;
const EPOLL_FD: i32 = 6;
const EPOLL_EVENT_BATCH: u32 = 64;
const CLONE_NEWNS: i32 = 0x0002_0000;
const CLONE_NEWNET: i32 = 0x4000_0000;
const MS_REC: usize = 0x4000;
const MS_PRIVATE: usize = 1 << 18;
const MS_NOSUID: usize = 2;
const MS_NODEV: usize = 4;
const MS_NOEXEC: usize = 8;
const PR_SET_PDEATHSIG: i32 = 1;
const PR_SET_NO_NEW_PRIVS: i32 = 38;
const PR_SET_SECCOMP: i32 = 22;
const PR_CAPBSET_DROP: i32 = 24;
const SECCOMP_MODE_FILTER: usize = 2;
const SIGKILL: usize = 9;
const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

#[cfg(target_arch = "x86_64")]
const SYS_READ: i64 = 0;
#[cfg(target_arch = "x86_64")]
const SYS_WRITE: i64 = 1;
#[cfg(target_arch = "x86_64")]
const SYS_IOCTL: i64 = 16;
#[cfg(target_arch = "x86_64")]
const SYS_SOCKET: i64 = 41;
#[cfg(target_arch = "x86_64")]
const SYS_OPENAT: i64 = 257;
#[cfg(target_arch = "aarch64")]
const SYS_READ: i64 = 63;
#[cfg(target_arch = "aarch64")]
const SYS_WRITE: i64 = 64;
#[cfg(target_arch = "aarch64")]
const SYS_IOCTL: i64 = 29;
#[cfg(target_arch = "aarch64")]
const SYS_SOCKET: i64 = 198;
#[cfg(target_arch = "aarch64")]
const SYS_OPENAT: i64 = 56;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("wlan netstack seccomp is supported only on x86_64 and aarch64");

#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}
#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

fn stmt(code: u16, k: u32) -> SockFilter {
    SockFilter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(k: u32, jt: u8, jf: u8) -> SockFilter {
    SockFilter {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt,
        jf,
        k,
    }
}

fn arg(index: usize) -> SockFilter {
    stmt(BPF_LD | BPF_W | BPF_ABS, 16 + (index * 8) as u32)
}

fn arg_high(index: usize) -> SockFilter {
    stmt(BPF_LD | BPF_W | BPF_ABS, 20 + (index * 8) as u32)
}

fn append_kill(filter: &mut Vec<SockFilter>, syscall: i64) {
    filter.push(jump(syscall as u32, 0, 1));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
}

fn append_fd_and_flags(filter: &mut Vec<SockFilter>, syscall: i64, fd: RawFd, flags: i32) {
    filter.push(jump(syscall as u32, 0, 20));
    filter.push(arg(0));
    filter.push(jump(fd as u32, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg(3));
    filter.push(jump(flags as u32, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    // Connected frame send/recv never supplies an alternate address buffer.
    filter.push(arg(4));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg_high(4));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg(5));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg_high(5));
    filter.push(jump(0, 0, 1));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
}

fn append_accept4(filter: &mut Vec<SockFilter>, listener_fd: RawFd) {
    filter.push(jump(libc::SYS_accept4 as u32, 0, 7));
    filter.push(arg(0));
    filter.push(jump(listener_fd as u32, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg(3));
    filter.push(jump(
        (libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK) as u32,
        1,
        0,
    ));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    filter.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
}

fn append_io_except_capabilities(
    filter: &mut Vec<SockFilter>,
    syscall: i64,
    frame_fd: RawFd,
    listener_fd: RawFd,
) {
    filter.push(jump(syscall as u32, 0, 6));
    filter.push(arg(0));
    filter.push(jump(frame_fd as u32, 2, 0));
    filter.push(jump(listener_fd as u32, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
}

fn append_fcntl_commands(filter: &mut Vec<SockFilter>) {
    filter.push(jump(libc::SYS_fcntl as u32, 0, 5));
    filter.push(arg(1));
    // Rust's owned-socket teardown checks that the descriptor is still open.
    filter.push(jump(libc::F_GETFD as u32, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    filter.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
}

fn append_epoll_ctl(filter: &mut Vec<SockFilter>, epoll_fd: RawFd) {
    filter.push(jump(libc::SYS_epoll_ctl as u32, 0, 21));
    filter.push(arg(0));
    filter.push(jump(epoll_fd as u32, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg_high(0));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg(1));
    filter.push(jump(libc::EPOLL_CTL_ADD as u32, 3, 0));
    filter.push(jump(libc::EPOLL_CTL_MOD as u32, 2, 0));
    filter.push(jump(libc::EPOLL_CTL_DEL as u32, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg_high(1));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg(2));
    filter.push(jump(epoll_fd as u32, 0, 1));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg_high(2));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    filter.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
}

fn append_epoll_pwait(filter: &mut Vec<SockFilter>, epoll_fd: RawFd) {
    filter.push(jump(libc::SYS_epoll_pwait as u32, 0, 25));
    filter.push(arg(0));
    filter.push(jump(epoll_fd as u32, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg_high(0));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg(2));
    filter.push(jump(EPOLL_EVENT_BATCH, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg_high(2));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg(4));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg_high(4));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg(5));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(arg_high(5));
    filter.push(jump(0, 1, 0));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    filter.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
}

fn append_no_exec_memory(filter: &mut Vec<SockFilter>, syscall: i64) {
    const BPF_JSET_K: u16 = BPF_JMP | 0x40 | BPF_K;
    filter.push(jump(syscall as u32, 0, 5));
    filter.push(arg(2));
    filter.push(SockFilter {
        code: BPF_JSET_K,
        jt: 0,
        jf: 1,
        k: libc::PROT_EXEC as u32,
    });
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    filter.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
}

unsafe extern "C" {
    fn close(fd: i32) -> i32;
    fn unshare(flags: i32) -> i32;
    fn mount(
        source: *const c_char,
        target: *const c_char,
        filesystem: *const c_char,
        flags: usize,
        data: *const c_void,
    ) -> i32;
    fn chdir(path: *const c_char) -> i32;
    fn chroot(path: *const c_char) -> i32;
    fn setgroups(size: usize, groups: *const u32) -> i32;
    fn setresgid(real: u32, effective: u32, saved: u32) -> i32;
    fn setresuid(real: u32, effective: u32, saved: u32) -> i32;
    fn prctl(option: i32, ...) -> i32;
    fn syscall(number: i64, ...) -> i64;
    fn getpid() -> i32;
    fn getppid() -> i32;
}

fn syscall_ok(result: i32, operation: &'static str) -> Result<(), String> {
    (result == 0).then_some(()).ok_or_else(|| {
        format!(
            "netstack sandbox {operation}: {}",
            std::io::Error::last_os_error()
        )
    })
}

fn setup(expected_parent: i32, first_close: u32) -> Result<(), String> {
    unsafe {
        if syscall(436, first_close, u32::MAX, 0u32) != 0 {
            for fd in first_close as i32..65536 {
                close(fd);
            }
        }
        syscall_ok(
            prctl(PR_SET_PDEATHSIG, SIGKILL, 0usize, 0usize, 0usize),
            "parent-death signal",
        )?;
        syscall_ok(unshare(CLONE_NEWNS | CLONE_NEWNET), "namespace isolation")?;
        syscall_ok(
            mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                MS_REC | MS_PRIVATE,
                std::ptr::null(),
            ),
            "private mounts",
        )?;
        syscall_ok(
            mount(
                c"tmpfs".as_ptr(),
                c"/run".as_ptr(),
                c"tmpfs".as_ptr(),
                MS_NOSUID | MS_NODEV | MS_NOEXEC,
                c"size=1048576,mode=0555".as_ptr().cast(),
            ),
            "empty root",
        )?;
        syscall_ok(chdir(c"/run".as_ptr()), "chdir empty root")?;
        syscall_ok(chroot(c".".as_ptr()), "chroot empty root")?;
        syscall_ok(chdir(c"/".as_ptr()), "chdir jailed root")?;
        let root_entries = std::fs::read_dir("/")
            .map_err(|error| format!("netstack sandbox inspect empty root: {error}"))?
            .map(|entry| {
                entry
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !root_entries.is_empty() {
            return Err(format!(
                "netstack sandbox root is not empty: {root_entries:?}"
            ));
        }
        syscall_ok(setgroups(0, std::ptr::null()), "clear groups")?;
        for capability in 0usize..64 {
            let _ = prctl(PR_CAPBSET_DROP, capability, 0usize, 0usize, 0usize);
        }
        syscall_ok(setresgid(65534, 65534, 65534), "drop gid")?;
        syscall_ok(setresuid(65534, 65534, 65534), "drop uid")?;
        // Credential changes clear PDEATHSIG on Linux; re-arm it only after
        // the final credentials are installed.
        syscall_ok(
            prctl(PR_SET_PDEATHSIG, SIGKILL, 0usize, 0usize, 0usize),
            "re-arm parent-death signal",
        )?;
        if getppid() != expected_parent {
            return Err("netstack parent changed during sandbox setup".into());
        }
        syscall_ok(
            prctl(PR_SET_NO_NEW_PRIVS, 1usize, 0usize, 0usize, 0usize),
            "no new privileges",
        )?;
    }
    Ok(())
}

fn network_filter(frame_fd: RawFd, listener_fd: RawFd, epoll_fd: RawFd) -> Vec<SockFilter> {
    let mut filter = vec![
        stmt(BPF_LD | BPF_W | BPF_ABS, 4),
        jump(AUDIT_ARCH, 1, 0),
        stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        stmt(BPF_LD | BPF_W | BPF_ABS, 0),
    ];

    // Forbidden ambient authority is fatal rather than an errno fallback.
    for denied in [SYS_IOCTL, SYS_SOCKET, SYS_OPENAT] {
        append_kill(&mut filter, denied);
    }
    // Netstack3's only kernel packet transport is inherited frame fd 3.
    append_fd_and_flags(
        &mut filter,
        libc::SYS_sendto,
        frame_fd,
        libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
    );
    append_fd_and_flags(
        &mut filter,
        libc::SYS_recvfrom,
        frame_fd,
        libc::MSG_DONTWAIT | libc::MSG_TRUNC,
    );
    // Owned applications enter only through the inherited loopback listener.
    append_accept4(&mut filter, listener_fd);
    // Bootstrap, logs, and dynamic accepted clients use byte-stream I/O. The
    // frame and listener capabilities cannot bypass their role-specific calls.
    append_io_except_capabilities(&mut filter, libc::SYS_read, frame_fd, listener_fd);
    append_io_except_capabilities(&mut filter, libc::SYS_write, frame_fd, listener_fd);
    // Accepted streams are made nonblocking; no descriptor duplication,
    // ownership, or advisory-lock fcntl commands are needed at runtime.
    append_fcntl_commands(&mut filter);
    append_epoll_ctl(&mut filter, epoll_fd);
    append_epoll_pwait(&mut filter, epoll_fd);
    // Heap growth/reclamation is allowed, but executable memory is not.
    append_no_exec_memory(&mut filter, libc::SYS_mmap);
    append_no_exec_memory(&mut filter, libc::SYS_mprotect);

    // Runtime and teardown operations, grouped by the role that requires them.
    // read/write/close cover bootstrap fd 5, logs, and dynamic accepted clients.
    let allowed = [
        libc::SYS_close,
        // Rust allocation and deallocation after executable mappings are denied.
        libc::SYS_munmap,
        libc::SYS_brk,
        libc::SYS_mremap,
        libc::SYS_madvise,
        // Rust signal runtime state is installed before confinement but may be
        // queried/restored during failures and process teardown.
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        // Single-process synchronization and interrupted sleep restart.
        libc::SYS_futex,
        libc::SYS_restart_syscall,
        // Service deadlines, idle timeouts, and bounded loop sleeps.
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        // RandomState initialization; protocol entropy is otherwise injected.
        libc::SYS_getrandom,
        // Readiness diagnostics and normal/error termination.
        libc::SYS_getpid,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ];
    for number in allowed {
        filter.push(jump(number as u32, 0, 1));
        filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    }
    // Unknown syscalls are fatal: adding a runtime operation requires naming
    // and justifying it above instead of silently expanding ambient authority.
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter
}

fn lockdown() -> Result<(), String> {
    install_filter(FRAME_FD, LISTENER_FD, EPOLL_FD)
}

fn install_filter(frame_fd: RawFd, listener_fd: RawFd, epoll_fd: RawFd) -> Result<(), String> {
    let filter = network_filter(frame_fd, listener_fd, epoll_fd);
    let program = SockFprog {
        len: filter.len() as u16,
        filter: filter.as_ptr(),
    };
    syscall_ok(
        unsafe {
            prctl(
                PR_SET_SECCOMP,
                SECCOMP_MODE_FILTER,
                &program,
                0usize,
                0usize,
            )
        },
        "seccomp",
    )
}

#[cfg(test)]
pub(crate) fn install_test_filter(
    frame_fd: RawFd,
    listener_fd: RawFd,
    epoll_fd: RawFd,
) -> Result<(), String> {
    syscall_ok(
        unsafe { prctl(PR_SET_NO_NEW_PRIVS, 1usize, 0usize, 0usize, 0usize) },
        "test no-new-privileges",
    )?;
    install_filter(frame_fd, listener_fd, epoll_fd)
}

fn write_all_fd(fd: i32, mut bytes: &[u8], operation: &'static str) -> Result<(), String> {
    while !bytes.is_empty() {
        let written = unsafe { syscall(SYS_WRITE, fd, bytes.as_ptr(), bytes.len()) };
        if written > 0 {
            bytes = &bytes[written as usize..];
        } else if written == -1
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
        {
            continue;
        } else {
            return Err(format!("{operation}: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

fn read_exact_fd(fd: i32, mut bytes: &mut [u8], operation: &'static str) -> Result<(), String> {
    while !bytes.is_empty() {
        let read = unsafe { syscall(SYS_READ, fd, bytes.as_mut_ptr(), bytes.len()) };
        if read > 0 {
            bytes = &mut bytes[read as usize..];
        } else if read == -1
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
        {
            continue;
        } else if read == 0 {
            return Err(format!("{operation}: unexpected EOF"));
        } else {
            return Err(format!("{operation}: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

/// Starts the production service without gating availability on external
/// reachability and without imposing a lab lifetime.
pub fn run() -> Result<(), String> {
    let expected_parent = env::var("DRV_NETSTACK_PARENT_PID")
        .map_err(|_| "missing expected parent PID")?
        .parse::<i32>()
        .map_err(|_| "invalid expected parent PID")?;
    let mac: [u8; 6] = env::var("DRV_SAE_CLIENT_MAC")
        .map_err(|_| "missing client MAC")?
        .split(':')
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "invalid client MAC")?
        .try_into()
        .map_err(|_| "invalid client MAC")?;
    let listen: SocketAddr = env::var("DRV_SOCKS5_LISTEN")
        .map_err(|_| "missing listen address")?
        .parse()
        .map_err(|_| "invalid listen address")?;
    let frame = unsafe { OwnedFd::from_raw_fd(FRAME_FD) };
    let listener = unsafe { TcpListener::from_raw_fd(LISTENER_FD) };
    setup(expected_parent, 6)?;
    let poller = NetworkPoller::new().map_err(str::to_string)?;
    if poller.raw_fd() != EPOLL_FD {
        return Err(format!(
            "network epoll descriptor mismatch: expected {EPOLL_FD}, got {}",
            poller.raw_fd()
        ));
    }
    // RLIMIT_NOFILE is ambient process state, so turn it into a bounded
    // admission capability before the default-kill filter is installed.
    let resources = crate::Socks5ResourceBudget::from_process_limit().map_err(str::to_string)?;
    crate::bound_listener_socket_memory(&listener)?;
    lockdown()?;
    println!(
        "netstack_sandbox_ready=true pid={} uid=65534 gid=65534 no_new_privs=true seccomp_default=kill empty_root=true own_netns=true inherited_frame_only=true inherited_listener_only=true",
        unsafe { getpid() }
    );
    write_all_fd(5, b"READY", "bootstrap READY failed")?;
    let mut go = [0u8; 2];
    read_exact_fd(5, &mut go, "bootstrap GO failed")?;
    if &go != b"GO" {
        return Err("invalid bootstrap GO".into());
    }
    let device = unsafe { ServiceEthernetDevice::from_frame_fd(frame, mac) };
    let mut service = Socks5Service::new_with_poller(
        device,
        poller,
        resources,
    )
    .map_err(str::to_string)?;
    write_all_fd(5, b"STARTED", "bootstrap STARTED failed")?;
    unsafe {
        close(5);
    }
    println!("network_service_ready=true reachability=acquiring");
    service
        .serve_socks5_listener(listener, listen, None, || false)
        .map_err(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Socks5ResourceBudget;
    use std::io::{Read as _, Write as _};
    use std::net::TcpStream;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    use std::thread;
    use wlan_softmac_host::ethernet::{EthernetIngressError, ethernet_port};

    fn duplicate(fd: RawFd) -> OwnedFd {
        let fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
        assert!(fd >= 0);
        unsafe { OwnedFd::from_raw_fd(fd) }
    }

    fn child_exit(code: i32) -> ! {
        unsafe { libc::_exit(code) }
    }

    fn child_require(condition: bool, code: i32) {
        if !condition {
            child_exit(code);
        }
    }

    fn thread_cpu_time() -> Option<Duration> {
        let mut time = std::mem::MaybeUninit::<libc::timespec>::zeroed();
        (unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, time.as_mut_ptr()) } == 0)
            .then(|| {
                let time = unsafe { time.assume_init() };
                Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
            })
    }

    #[test]
    fn network_filter_fixture() {
        if std::env::var_os("DRV_NETWORK_FILTER_FIXTURE").is_none() {
            return;
        }
        let poller = NetworkPoller::new().unwrap();
        child_require(
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == 0,
            10,
        );
        child_require(
            install_filter(FRAME_FD, LISTENER_FD, poller.raw_fd()).is_ok(),
            11,
        );

        let mut frame = [0u8; 14];
        child_require(
            unsafe {
                libc::recvfrom(
                    FRAME_FD,
                    frame.as_mut_ptr().cast(),
                    frame.len(),
                    libc::MSG_DONTWAIT | libc::MSG_TRUNC,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            } == frame.len() as isize,
            21,
        );
        child_require(
            unsafe {
                libc::sendto(
                    FRAME_FD,
                    frame.as_ptr().cast(),
                    frame.len(),
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                    std::ptr::null(),
                    0,
                )
            } == frame.len() as isize,
            22,
        );

        let accepted = unsafe {
            libc::accept4(
                LISTENER_FD,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            )
        };
        child_require(accepted >= 0, 23);
        child_require(unsafe { libc::fcntl(accepted, libc::F_GETFD) } >= 0, 24);
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: 7,
        };
        child_require(
            unsafe { libc::epoll_ctl(poller.raw_fd(), libc::EPOLL_CTL_ADD, accepted, &mut event) }
                == 0,
            25,
        );
        let mut ready = [libc::epoll_event { events: 0, u64: 0 }; EPOLL_EVENT_BATCH as usize];
        child_require(
            unsafe {
                libc::syscall(
                    libc::SYS_epoll_pwait,
                    poller.raw_fd(),
                    ready.as_mut_ptr(),
                    EPOLL_EVENT_BATCH,
                    0,
                    std::ptr::null::<libc::sigset_t>(),
                    0usize,
                )
            } >= 0,
            41,
        );
        let mut request = [0u8; 4];
        child_require(
            unsafe { libc::read(accepted, request.as_mut_ptr().cast(), request.len()) }
                == request.len() as isize
                && request == *b"PING",
            26,
        );
        child_require(
            unsafe { libc::write(accepted, b"PONG".as_ptr().cast(), 4) } == 4,
            27,
        );
        child_require(
            unsafe { libc::epoll_ctl(poller.raw_fd(), libc::EPOLL_CTL_DEL, accepted, &mut event) }
                == 0,
            42,
        );

        let mut go = [0u8; 2];
        child_require(
            unsafe { libc::read(5, go.as_mut_ptr().cast(), go.len()) } == 2 && go == *b"GO",
            28,
        );
        child_require(unsafe { libc::write(5, b"OK".as_ptr().cast(), 2) } == 2, 29);

        let memory = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        child_require(memory != libc::MAP_FAILED, 30);
        child_require(
            unsafe { libc::mprotect(memory, 4096, libc::PROT_READ) } == 0,
            31,
        );
        child_require(
            unsafe { libc::madvise(memory, 4096, libc::MADV_DONTNEED) } == 0,
            32,
        );
        child_require(unsafe { libc::munmap(memory, 4096) } == 0, 33);
        let mut now: libc::timespec = unsafe { std::mem::zeroed() };
        child_require(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } == 0,
            34,
        );
        let delay = libc::timespec {
            tv_sec: 0,
            tv_nsec: 1,
        };
        child_require(
            unsafe { libc::nanosleep(&delay, std::ptr::null_mut()) } == 0,
            35,
        );
        let mut random = [0u8; 8];
        child_require(
            unsafe { libc::syscall(libc::SYS_getrandom, random.as_mut_ptr(), random.len(), 0) }
                == random.len() as i64,
            36,
        );
        child_require(unsafe { libc::close(accepted) } == 0, 37);
        child_require(unsafe { libc::close(FRAME_FD) } == 0, 38);
        child_require(unsafe { libc::close(LISTENER_FD) } == 0, 39);
        child_require(unsafe { libc::close(5) } == 0, 40);
        child_exit(0);
    }

    #[test]
    fn network_service_filter_fixture() {
        if std::env::var_os("DRV_NETWORK_SERVICE_FILTER_FIXTURE").is_none() {
            return;
        }
        let poller = NetworkPoller::new().unwrap();
        let resources = Socks5ResourceBudget::from_process_limit().unwrap();
        child_require(
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == 0,
            45,
        );
        child_require(
            install_filter(FRAME_FD, LISTENER_FD, poller.raw_fd()).is_ok(),
            46,
        );
        let mut dns = netstack3_port_integration::dns_bridge::NativeDnsBridge::new();
        child_require(dns.configure(&["192.0.2.53".parse().unwrap()]).is_ok(), 49);
        drop(dns);
        let frame = unsafe { OwnedFd::from_raw_fd(FRAME_FD) };
        let listener = unsafe { TcpListener::from_raw_fd(LISTENER_FD) };
        let device = unsafe { ServiceEthernetDevice::from_frame_fd(frame, [2, 0, 0, 0, 0, 1]) };
        let mut service = match Socks5Service::new_with_poller(
            device,
            poller,
            resources,
        ) {
            Ok(service) => service,
            Err(_) => child_exit(47),
        };
        child_require(
            service
                .serve_socks5_listener(
                    listener,
                    "127.0.0.1:0".parse().unwrap(),
                    Some(Instant::now() + Duration::from_millis(100)),
                    || false,
                )
                .is_ok(),
            48,
        );
        drop(service);
        child_exit(0);
    }

    #[test]
    fn resource_admission_filter_fixture() {
        let Some(mode) = std::env::var_os("DRV_NETWORK_RESOURCE_ADMISSION_FIXTURE") else {
            return;
        };
        let poller = NetworkPoller::new().unwrap();
        let resources = Socks5ResourceBudget::from_process_limit().unwrap();
        child_require(resources.admission_capacity == 72, 120);
        let restore_limit = (mode == "emfile").then(|| {
            let lowered = libc::rlimit {
                rlim_cur: 72,
                rlim_max: 80,
            };
            child_require(
                unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lowered) } == 0,
                121,
            );
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(300));
                let restored = libc::rlimit {
                    rlim_cur: 80,
                    rlim_max: 80,
                };
                assert_eq!(
                    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &restored) },
                    0
                );
            })
        });
        let frame = unsafe { OwnedFd::from_raw_fd(FRAME_FD) };
        let listener = unsafe { TcpListener::from_raw_fd(LISTENER_FD) };
        crate::bound_listener_socket_memory(&listener).unwrap();
        let listen = listener.local_addr().unwrap();
        let device = unsafe { ServiceEthernetDevice::from_frame_fd(frame, [2, 0, 0, 0, 0, 1]) };
        let mut service = Socks5Service::new_with_poller(
            device,
            poller,
            resources,
        )
        .unwrap();
        child_require(
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == 0,
            122,
        );
        child_require(
            install_filter(FRAME_FD, LISTENER_FD, service.poller_fd()).is_ok(),
            123,
        );
        let before = service.poller_wait_counts();
        let before_cpu = thread_cpu_time();
        child_require(before_cpu.is_some(), 127);
        child_require(
            service
                .serve_socks5_listener(
                    listener,
                    listen,
                    Some(Instant::now() + Duration::from_millis(800)),
                    || false,
                )
                .is_ok(),
            124,
        );
        let after = service.poller_wait_counts();
        let after_cpu = thread_cpu_time();
        child_require(after_cpu.is_some(), 127);
        let waits = after.0 - before.0;
        let blocking = after.1 - before.1;
        let cpu = after_cpu.unwrap().saturating_sub(before_cpu.unwrap());
        eprintln!(
            "resource_admission mode={} capacity={} waits={waits} blocking_waits={blocking} cpu_us={}",
            mode.to_string_lossy(),
            resources.admission_capacity,
            cpu.as_micros(),
        );
        // Client readiness can split across an arbitrary number of epoll
        // batches. CPU time catches an immediate-return spin without imposing
        // a scheduling-sensitive cap on otherwise blocking-capable waits.
        child_require(
            blocking != 0 && blocking == waits && cpu < Duration::from_millis(30),
            125,
        );
        if let Some(restore_limit) = restore_limit {
            child_require(restore_limit.join().is_ok(), 126);
        }
        drop(service);
        child_exit(0);
    }

    #[test]
    fn provider_filter_denial_fixture() {
        let Some(operation) = std::env::var_os("DRV_PROVIDER_FILTER_DENIAL") else { return };
        let syscall_args = match operation.to_str().unwrap() {
            "socket" => [libc::SYS_socket, libc::AF_INET as i64, libc::SOCK_STREAM as i64, 0],
            "read-registration" => [libc::SYS_read, 3, 0, 0],
            "write-registration" => [libc::SYS_write, 3, 0, 0],
            "read-epoll" => [libc::SYS_read, 6, 0, 0],
            "claim-wrong-fd" => [libc::SYS_ioctl, 4, 0x8008B301, 0],
            "control-registration" => [libc::SYS_ioctl, 3, 0x8080B303, 0],
            "control-epoll" => [libc::SYS_ioctl, 6, 0x8080B303, 0],
            "publish-registration" => [libc::SYS_ioctl, 3, 0xC038B302, 0],
            "publish-epoll" => [libc::SYS_ioctl, 6, 0xC038B302, 0],
            "unknown-ioctl" => [libc::SYS_ioctl, 4, 0x1234, 0],
            "dup" => [libc::SYS_fcntl, 4, libc::F_DUPFD_CLOEXEC as i64, 7],
            "ethernet-read" => [libc::SYS_read, 4, 0, 0],
            "ethernet-write" => [libc::SYS_write, 4, 0, 0],
            "ethernet-control" => [libc::SYS_ioctl, 4, 0x8080B303, 0],
            "ethernet-publish" => [libc::SYS_ioctl, 4, 0xC038B302, 0],
            "ethernet-send-wrong-flags" => [libc::SYS_sendto, 4, 0, 0],
            "ethernet-recv-wrong-flags" => [libc::SYS_recvfrom, 4, 0, 0],
            _ => panic!("unknown denial"),
        };
        child_require(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == 0, 50);
        let filter = provider_filter(operation.to_str().unwrap().starts_with("ethernet-"));
        let program = SockFprog { len: filter.len() as u16, filter: filter.as_ptr() };
        child_require(unsafe { prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program, 0usize, 0usize) } == 0, 51);
        unsafe { libc::syscall(syscall_args[0], syscall_args[1], syscall_args[2], syscall_args[3], 0usize, 0usize, 0usize); }
        child_exit(52);
    }

    #[test]
    fn provider_filter_forbidden_operations_are_fatal() {
        use std::os::unix::process::ExitStatusExt as _;
        for operation in ["socket", "read-registration", "write-registration", "read-epoll",
            "claim-wrong-fd", "control-registration", "control-epoll", "publish-registration", "publish-epoll", "unknown-ioctl", "dup",
            "ethernet-read", "ethernet-write", "ethernet-publish", "ethernet-control",
            "ethernet-send-wrong-flags", "ethernet-recv-wrong-flags"] {
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "child::tests::provider_filter_denial_fixture", "--nocapture"])
                .env("DRV_PROVIDER_FILTER_DENIAL", operation)
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
                .status().unwrap();
            assert_eq!(status.signal(), Some(libc::SIGSYS), "provider operation survived: {operation}; {status}");
        }
    }

    #[test]
    fn provider_ethernet_filter_fixture() {
        if std::env::var_os("DRV_PROVIDER_ETHERNET_FIXTURE").is_none() { return; }
        let mut pair = [-1; 2];
        child_require(unsafe { libc::socketpair(libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK, 0, pair.as_mut_ptr()) } == 0, 60);
        let frame = duplicate(pair[0]);
        let _peer = duplicate(pair[1]); // dup2 below may replace the original peer FD.
        child_require(unsafe { libc::send(pair[1], b"ARP".as_ptr().cast(), 3, 0) } == 3, 61);
        child_require(unsafe { libc::dup2(frame.as_raw_fd(), 4) } == 4, 62);
        let filter = provider_filter(true);
        let program = SockFprog { len: filter.len() as u16, filter: filter.as_ptr() };
        child_require(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == 0, 63);
        child_require(unsafe { prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program, 0usize, 0usize) } == 0, 64);
        let mut data = [0u8; 3];
        child_require(unsafe { libc::recv(4, data.as_mut_ptr().cast(), 3,
            libc::MSG_DONTWAIT | libc::MSG_TRUNC) } == 3, 65);
        child_require(&data == b"ARP", 66);
        child_require(unsafe { libc::send(4, data.as_ptr().cast(), 3,
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) } == 3, 67);
        child_exit(0);
    }

    #[test]
    fn provider_filter_allows_only_scoped_ethernet_frames() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child::tests::provider_ethernet_filter_fixture", "--nocapture"])
            .env("DRV_PROVIDER_ETHERNET_FIXTURE", "1")
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit())
            .status().unwrap();
        assert!(status.success(), "Ethernet capability operations failed: {status}");
    }

    #[test]
    fn network_filter_denial_fixture() {
        let Some(operation) = std::env::var_os("DRV_NETWORK_FILTER_DENIAL") else {
            return;
        };
        child_require(
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == 0,
            50,
        );
        child_require(lockdown().is_ok(), 51);
        unsafe {
            match operation.to_str().unwrap() {
                "socket" => {
                    libc::syscall(libc::SYS_socket, libc::AF_INET, libc::SOCK_STREAM, 0);
                }
                "openat" => {
                    libc::syscall(
                        libc::SYS_openat,
                        libc::AT_FDCWD,
                        c"/etc/passwd".as_ptr(),
                        libc::O_RDONLY,
                    );
                }
                "ioctl" => {
                    libc::syscall(libc::SYS_ioctl, FRAME_FD, 0x5413, 0);
                }
                "wrong-send-fd" => {
                    libc::syscall(libc::SYS_sendto, LISTENER_FD, b"x".as_ptr(), 1, 0, 0, 0);
                }
                "send-flags" => {
                    libc::syscall(libc::SYS_sendto, FRAME_FD, b"x".as_ptr(), 1, 0, 0, 0);
                }
                "recv-flags" => {
                    libc::syscall(libc::SYS_recvfrom, FRAME_FD, 0, 0, 0, 0, 0);
                }
                "frame-address" => {
                    libc::syscall(
                        libc::SYS_sendto,
                        FRAME_FD,
                        b"x".as_ptr(),
                        1,
                        libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                        1,
                        1,
                    );
                }
                "read-frame" => {
                    libc::syscall(libc::SYS_read, FRAME_FD, 0, 0);
                }
                "write-listener" => {
                    libc::syscall(libc::SYS_write, LISTENER_FD, 0, 0);
                }
                "wrong-accept-fd" => {
                    libc::syscall(
                        libc::SYS_accept4,
                        FRAME_FD,
                        0,
                        0,
                        libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                    );
                }
                "accept-flags" => {
                    libc::syscall(libc::SYS_accept4, LISTENER_FD, 0, 0, 0);
                }
                "fcntl-dup" => {
                    libc::syscall(libc::SYS_fcntl, FRAME_FD, libc::F_DUPFD_CLOEXEC, 10);
                }
                "fcntl-getfl" => {
                    libc::syscall(libc::SYS_fcntl, FRAME_FD, libc::F_GETFL);
                }
                "epoll-ctl-fd" => {
                    libc::syscall(
                        libc::SYS_epoll_ctl,
                        LISTENER_FD,
                        libc::EPOLL_CTL_ADD,
                        FRAME_FD,
                        0,
                    );
                }
                "epoll-ctl-operation" => {
                    libc::syscall(libc::SYS_epoll_ctl, EPOLL_FD, 99, FRAME_FD, 0);
                }
                "epoll-ctl-self" => {
                    libc::syscall(
                        libc::SYS_epoll_ctl,
                        EPOLL_FD,
                        libc::EPOLL_CTL_ADD,
                        EPOLL_FD,
                        0,
                    );
                }
                "epoll-wait-fd" => {
                    libc::syscall(
                        libc::SYS_epoll_pwait,
                        LISTENER_FD,
                        0,
                        EPOLL_EVENT_BATCH,
                        0,
                        0,
                        0,
                    );
                }
                "epoll-wait-batch" => {
                    libc::syscall(libc::SYS_epoll_pwait, EPOLL_FD, 0, 1, 0, 0, 0);
                }
                "epoll-wait-signal-mask" => {
                    libc::syscall(
                        libc::SYS_epoll_pwait,
                        EPOLL_FD,
                        0,
                        EPOLL_EVENT_BATCH,
                        0,
                        1,
                        8,
                    );
                }
                "executable-memory" => {
                    libc::syscall(
                        libc::SYS_mmap,
                        0,
                        4096,
                        libc::PROT_READ | libc::PROT_EXEC,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        -1,
                        0,
                    );
                }
                "executable-mprotect" => {
                    libc::syscall(libc::SYS_mprotect, 0, 4096, libc::PROT_EXEC);
                }
                "unknown" => {
                    libc::syscall(libc::SYS_getuid);
                }
                _ => child_exit(52),
            }
        }
        child_exit(53);
    }

    #[test]
    fn network_filter_forbidden_operations_are_fatal() {
        use std::os::unix::process::ExitStatusExt as _;

        for operation in [
            "socket",
            "openat",
            "ioctl",
            "wrong-send-fd",
            "send-flags",
            "recv-flags",
            "frame-address",
            "read-frame",
            "write-listener",
            "wrong-accept-fd",
            "accept-flags",
            "fcntl-dup",
            "fcntl-getfl",
            "epoll-ctl-fd",
            "epoll-ctl-operation",
            "epoll-ctl-self",
            "epoll-wait-fd",
            "epoll-wait-batch",
            "epoll-wait-signal-mask",
            "executable-memory",
            "executable-mprotect",
            "unknown",
        ] {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "child::tests::network_filter_denial_fixture",
                    "--nocapture",
                ])
                .env("DRV_NETWORK_FILTER_DENIAL", operation)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert_eq!(
                status.signal(),
                Some(libc::SIGSYS),
                "forbidden network operation survived: {operation}; status={status}"
            );
        }
    }

    #[test]
    fn production_service_core_runs_under_network_filter() {
        let (host, driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        let frame = duplicate(host.into_frame_fd().as_raw_fd());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.write_all(&[5, 1, 0]).unwrap();
        let listener = duplicate(listener.as_raw_fd());
        let raw = [frame.as_raw_fd(), listener.as_raw_fd()];
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "child::tests::network_service_filter_fixture",
                "--nocapture",
            ])
            .env("DRV_NETWORK_SERVICE_FILTER_FIXTURE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                for (source, target) in raw.into_iter().zip([3, 4]) {
                    if libc::dup2(source, target) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        drop((frame, listener));
        let mut greeting = [0u8; 2];
        client.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting, [5, 0]);
        let status = child.wait().unwrap();
        drop(driver);
        assert!(
            status.success(),
            "filtered production core failed: {status}"
        );
    }

    #[test]
    fn production_socks_relay_runs_under_network_filter() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "integration_test::socks_connect_relays_application_bytes_over_ethernet",
                "--nocapture",
            ])
            .env("DRV_NETWORK_RELAY_FILTER_FIXTURE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .unwrap();
        assert!(status.success(), "filtered SOCKS relay failed: {status}");
    }

    fn run_resource_admission_fixture(mode: &str, initially_admitted: usize, release_client: bool) {
        let (host, driver) = ethernet_port([2, 0, 0, 0, 0, 1], 256).unwrap();
        let frame = duplicate(host.into_frame_fd().as_raw_fd());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        crate::bound_listener_socket_memory(&listener).unwrap();
        let listen = listener.local_addr().unwrap();
        let listener = duplicate(listener.as_raw_fd());
        let raw = [frame.as_raw_fd(), listener.as_raw_fd()];
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "child::tests::resource_admission_filter_fixture",
                "--nocapture",
            ])
            .env("DRV_NETWORK_RESOURCE_ADMISSION_FIXTURE", mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        unsafe {
            command.pre_exec(move || {
                for (source, target) in raw.into_iter().zip([FRAME_FD, LISTENER_FD]) {
                    if libc::dup2(source, target) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                let limit = libc::rlimit {
                    rlim_cur: 80,
                    rlim_max: 80,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        drop((frame, listener));
        let mut clients: Vec<_> = (0..=initially_admitted)
            .map(|_| {
                let mut client = TcpStream::connect(listen).unwrap();
                client
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                client.write_all(&[5, 1, 0]).unwrap();
                client
            })
            .collect();
        let mut queued = clients.pop().unwrap();
        for client in &mut clients {
            let mut greeting = [0; 2];
            client.read_exact(&mut greeting).unwrap();
            assert_eq!(greeting, [5, 0]);
        }
        queued
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut greeting = [0; 2];
        let error = queued.read_exact(&mut greeting).unwrap_err();
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "queued client failed unexpectedly: {error}"
        );
        if release_client {
            drop(clients.swap_remove(0));
        }
        queued
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        queued.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting, [5, 0]);
        drop((queued, clients));
        let status = child.wait().unwrap();
        drop(driver);
        assert!(
            status.success(),
            "resource admission fixture failed: {status}"
        );
    }

    #[test]
    fn filtered_resource_derived_admission_exceeds_sixty_four_and_recovers_slot() {
        // RLIMIT_NOFILE=80 minus seven retained descriptors and one spare.
        run_resource_admission_fixture("boundary", 72, true);
    }

    #[test]
    fn filtered_emfile_pause_recovers_on_external_resource_timer() {
        // The child snapshots capacity at 80, then temporarily lowers its soft
        // limit to 72. Six retained descriptors leave 66 successful accepts;
        // restoring the limit exercises timer-driven recovery without closing
        // a locally admitted client.
        run_resource_admission_fixture("emfile", 66, false);
    }

    #[test]
    fn epoll_scaling_idle_and_deadline_run_under_network_filter() {
        for test in [
            "integration_test::epoll_serves_more_than_twenty_four_clients_with_isolated_failures",
            "integration_test::idle_epoll_waits_for_deadline_without_busy_polling",
        ] {
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture"])
                .env("DRV_NETWORK_EPOLL_FILTER_FIXTURE", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .status()
                .unwrap();
            assert!(
                status.success(),
                "filtered epoll fixture failed: {test}: {status}"
            );
        }
    }

    #[test]
    fn network_filter_allows_runtime_and_teardown() {
        let (host, mut driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        driver.set_link(true);
        driver.deliver(&[0xa5; 14]).unwrap();
        let frame = duplicate(host.into_frame_fd().as_raw_fd());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.write_all(b"PING").unwrap();
        let listener = duplicate(listener.as_raw_fd());
        let (mut bootstrap_parent, bootstrap_child) = UnixStream::pair().unwrap();
        bootstrap_parent
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        bootstrap_parent.write_all(b"GO").unwrap();
        let bootstrap = duplicate(bootstrap_child.as_raw_fd());
        let raw = [
            frame.as_raw_fd(),
            listener.as_raw_fd(),
            bootstrap.as_raw_fd(),
        ];
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "child::tests::network_filter_fixture",
                "--nocapture",
            ])
            .env("DRV_NETWORK_FILTER_FIXTURE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        unsafe {
            command.pre_exec(move || {
                for (source, target) in raw.into_iter().zip([3, 4, 5]) {
                    if libc::dup2(source, target) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        drop((frame, listener, bootstrap, bootstrap_child));

        let deadline = Instant::now() + Duration::from_secs(2);
        let echoed = loop {
            match driver.take_transmit() {
                Ok(Some(frame)) => break frame,
                Ok(None) | Err(EthernetIngressError::Backpressure) => {}
                Err(error) => panic!("network filter frame path failed: {error:?}"),
            }
            assert!(Instant::now() < deadline, "network filter frame timed out");
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(echoed.as_bytes(), [0xa5; 14]);
        let mut response = [0u8; 4];
        client.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"PONG");
        let mut ready = [0u8; 2];
        bootstrap_parent.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"OK");
        assert!(child.wait().unwrap().success());
    }
}


// The provider endpoint is opened in the served network namespace before
// setup creates the child's empty network namespace. Possession of fd 3,
// not the child's namespace or privilege, authorizes this single session.
pub(crate) fn provider_setup(ethernet: bool, bootstrap: bool, resolver: bool) -> Result<(), String> {
    unsafe {
        if !ethernet { close(4); }
        if !bootstrap { close(5); }
    }
    setup(unsafe { getppid() }, if resolver { 8 } else { 6 })?;
    let epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    if epoll < 0 { return Err(std::io::Error::last_os_error().to_string()); }
    if epoll != EPOLL_FD {
        if unsafe { libc::dup3(epoll, EPOLL_FD, libc::O_CLOEXEC) } < 0 { return Err(std::io::Error::last_os_error().to_string()); }
        unsafe { close(epoll); }
    }
    let mut filter = provider_filter(ethernet);
    if resolver {
        // Add the only new syscall authority: accept from the pre-bound resolver listener.
        let mut accept = Vec::new();
        append_accept4(&mut accept, 7);
        filter.splice(4..4, accept);
    }
    let program = SockFprog { len: filter.len() as u16, filter: filter.as_ptr() };
    syscall_ok(unsafe { prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program, 0usize, 0usize) }, "provider seccomp")
}

fn provider_filter(ethernet: bool) -> Vec<SockFilter> {
    let mut filter = vec![
        stmt(BPF_LD | BPF_W | BPF_ABS, 4),
        jump(AUDIT_ARCH, 1, 0),
        stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        stmt(BPF_LD | BPF_W | BPF_ABS, 0),
    ];
    if ethernet {
        append_fd_and_flags(&mut filter, libc::SYS_recvfrom, 4, libc::MSG_DONTWAIT | libc::MSG_TRUNC);
        append_fd_and_flags(&mut filter, libc::SYS_sendto, 4, libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL);
        // Frame authority is only usable through bounded datagram operations,
        // never endpoint ioctl or unframed read/write.
        for syscall in [libc::SYS_ioctl, libc::SYS_read, libc::SYS_write] {
            filter.push(jump(syscall as u32, 0, 3));
            filter.push(arg(0));
            filter.push(jump(4, 0, 1));
            filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
            filter.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
        }
    }
    filter.push(jump(libc::SYS_ioctl as u32, 0, 12));
    filter.push(arg(1));
    filter.push(jump(0x8008B301, 0, 3));
    filter.push(arg(0));
    filter.push(jump(3, 6, 7));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(jump(0xC038B302, 1, 0));
    filter.push(jump(0x8080B303, 0, 4));
    filter.push(arg(0));
    filter.push(jump(6, 2, 0));
    filter.push(SockFilter { code: BPF_JMP | 0x30 | BPF_K, jt: 0, jf: 1, k: 4 });
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
    for syscall in [libc::SYS_read, libc::SYS_write] {
        filter.push(jump(syscall as u32, 0, 7));
        filter.push(arg(0));
        filter.push(jump(6, 4, 0));
        // Apart from reserved frame FD4 (guarded above), FD4+ comes from
        // CLAIM/PUBLISH_ACCEPT: open/socket/dup are forbidden.
        filter.push(SockFilter { code: BPF_JMP | 0x30 | BPF_K, jt: 2, jf: 0, k: 4 });
        filter.push(jump(if syscall == libc::SYS_write { 1 } else { u32::MAX }, 1, 0));
        filter.push(jump(if syscall == libc::SYS_write { 2 } else { u32::MAX }, 0, 1));
        filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
        filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
        filter.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
    }
    append_fcntl_commands(&mut filter);
    append_epoll_ctl(&mut filter, EPOLL_FD);
    append_epoll_pwait(&mut filter, EPOLL_FD);
    append_no_exec_memory(&mut filter, libc::SYS_mmap);
    append_no_exec_memory(&mut filter, libc::SYS_mprotect);
    for syscall in [libc::SYS_close, libc::SYS_munmap, libc::SYS_brk,
        libc::SYS_mremap, libc::SYS_madvise, libc::SYS_futex, libc::SYS_getrandom,
        libc::SYS_clock_gettime, libc::SYS_rt_sigaction, libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn, libc::SYS_sigaltstack, libc::SYS_restart_syscall,
        libc::SYS_exit, libc::SYS_exit_group] {
        filter.push(jump(syscall as u32, 0, 1));
        filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    }
    filter.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    filter
}

pub(crate) fn provider_bootstrap_ready() -> Result<(), String> {
    write_all_fd(5, b"READY", "provider bootstrap READY")?;
    let mut go = [0; 2];
    read_exact_fd(5, &mut go, "provider bootstrap GO")?;
    if &go != b"GO" { return Err("invalid provider bootstrap GO".into()); }
    Ok(())
}

pub(crate) fn provider_bootstrap_network_ready() -> Result<(), String> {
    write_all_fd(5, b"NETWORK_READY", "provider network readiness")?;
    let mut serve = [0; 5];
    read_exact_fd(5, &mut serve, "provider bootstrap SERVE")?;
    if &serve != b"SERVE" { return Err("invalid provider bootstrap SERVE".into()); }
    unsafe { close(5); }
    Ok(())
}
