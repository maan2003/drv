use mt7921_softmac_adapter::ethernet::{
    BoundedNetstackProof, Mt7921EthernetDevice, NetstackProofConfig,
};
use std::env;
use std::ffi::c_void;
use std::net::{SocketAddr, TcpListener};
use std::num::NonZeroU16;
use std::os::fd::{FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

const FRAME_FD: i32 = 3;
const LISTENER_FD: i32 = 4;
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
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
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
#[cfg(target_arch = "x86_64")]
const ALLOWED_SYSCALLS: &[u32] = &[
    0, 1, 3, 7, 9, 10, 11, 12, 13, 14, 15, 23, 24, 25, 28, 35, 39, 44, 45, 47, 60, 72, 96, 131,
    202, 219, 228, 230, 231, 288, 318,
];

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
#[cfg(target_arch = "aarch64")]
const ALLOWED_SYSCALLS: &[u32] = &[
    25, 57, 63, 64, 72, 73, 93, 94, 98, 101, 113, 115, 124, 128, 132, 134, 135, 139, 169, 172, 198,
    206, 207, 212, 214, 215, 216, 222, 226, 233, 242, 278,
];

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("mt7921-netstack seccomp is supported only on x86_64 and aarch64");

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

unsafe extern "C" {
    fn close(fd: i32) -> i32;
    fn unshare(flags: i32) -> i32;
    fn mount(
        source: *const i8,
        target: *const i8,
        filesystem: *const i8,
        flags: usize,
        data: *const c_void,
    ) -> i32;
    fn chdir(path: *const i8) -> i32;
    fn chroot(path: *const i8) -> i32;
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

fn setup(expected_parent: i32) -> Result<(), String> {
    unsafe {
        if syscall(436, 6u32, u32::MAX, 0u32) != 0 {
            for fd in 6..65536 {
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

fn lockdown() -> Result<(), String> {
    // Target-specific Linux syscall numbers. There are deliberately no open, socket,
    // connect, ioctl, exec, fork, mount, namespace, or privilege syscalls.
    let mut filter = Vec::with_capacity(12 + ALLOWED_SYSCALLS.len() * 2);
    filter.push(SockFilter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: 4,
    });
    filter.push(SockFilter {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt: 1,
        jf: 0,
        k: AUDIT_ARCH,
    });
    filter.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_KILL_PROCESS,
    });
    filter.push(SockFilter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: 0,
    });
    for denied in [SYS_IOCTL as u32, SYS_SOCKET as u32, SYS_OPENAT as u32] {
        filter.push(SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 1,
            k: denied,
        });
        filter.push(SockFilter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ERRNO | 1,
        });
    }
    for number in ALLOWED_SYSCALLS {
        filter.push(SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 1,
            k: *number,
        });
        filter.push(SockFilter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        });
    }
    filter.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_KILL_PROCESS,
    });
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

fn denied_probe(number: i64, arg: *const i8) -> bool {
    unsafe {
        syscall(number, -100i32, arg, 0i32) == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(1)
    }
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

fn run() -> Result<(), String> {
    let sandbox_only = env::var_os("DRV_NETSTACK_SANDBOX_SELF_TEST").is_some();
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
    let seconds = env::var("DRV_DAEMON_MAX_SECONDS")
        .map_err(|_| "missing daemon deadline")?
        .parse::<u64>()
        .map_err(|_| "invalid daemon deadline")?;
    let frame = unsafe { OwnedFd::from_raw_fd(FRAME_FD) };
    let listener = unsafe { TcpListener::from_raw_fd(LISTENER_FD) };
    setup(expected_parent)?;
    lockdown()?;
    let fs_denied = denied_probe(SYS_OPENAT, c"/etc/passwd".as_ptr());
    let vfio_denied = denied_probe(SYS_OPENAT, c"/dev/vfio/vfio".as_ptr());
    let iommu_denied = denied_probe(SYS_OPENAT, c"/dev/iommu".as_ptr());
    let socket_denied = unsafe { syscall(SYS_SOCKET, 2i32, 1i32, 0i32) } == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(1);
    let ioctl_denied = unsafe { syscall(SYS_IOCTL, FRAME_FD, 0x3b67u64, 0usize) } == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(1);
    println!(
        "netstack_sandbox_ready=true pid={} uid=65534 gid=65534 no_new_privs=true seccomp=true empty_root=true own_netns=true fs_open_denied={fs_denied} vfio_open_denied={vfio_denied} iommu_open_denied={iommu_denied} socket_denied={socket_denied} vfio_ioctl_denied={ioctl_denied}",
        unsafe { getpid() }
    );
    if !(fs_denied && vfio_denied && iommu_denied && socket_denied && ioctl_denied) {
        return Err("sandbox denial self-proof failed".into());
    }
    write_all_fd(5, b"READY", "bootstrap READY failed")?;
    let mut go = [0u8; 2];
    read_exact_fd(5, &mut go, "bootstrap GO failed")?;
    if &go != b"GO" {
        return Err("invalid bootstrap GO".into());
    }
    if sandbox_only {
        unsafe {
            close(5);
        }
        println!("netstack_sandbox_self_test=true");
        return Ok(());
    }
    let device = unsafe { Mt7921EthernetDevice::from_frame_fd(frame, mac) };
    let mut proof = BoundedNetstackProof::new(
        device,
        NetstackProofConfig {
            dns_name: "example.com.".into(),
            server_port: NonZeroU16::new(80).unwrap(),
        },
    )
    .map_err(str::to_string)?;
    let initial_deadline = Instant::now() + Duration::from_secs(45);
    proof.prove_dhcp(initial_deadline).map_err(str::to_string)?;
    println!("internet_proof_dhcp=true");
    proof.prove_dns(initial_deadline).map_err(str::to_string)?;
    println!("internet_proof_dns=true");
    proof.prove_tcp(initial_deadline).map_err(str::to_string)?;
    println!("internet_proof_tcp=true");
    if !proof.network_ready() {
        return Err("network readiness proof incomplete".into());
    }
    write_all_fd(5, b"NETWORK_READY", "network-ready signal failed")?;
    let mut serve = [0u8; 5];
    read_exact_fd(5, &mut serve, "listener activation acknowledgment failed")?;
    if &serve != b"SERVE" {
        return Err("invalid listener activation acknowledgment".into());
    }
    unsafe {
        close(5);
    }
    println!("internet_network_ready=true");
    proof
        .serve_socks5_listener(
            listener,
            listen,
            Instant::now() + Duration::from_secs(seconds),
            || false,
        )
        .map_err(str::to_string)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("mt7921-netstack: {error}");
        std::process::exit(1);
    }
}
