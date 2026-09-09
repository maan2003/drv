// SPDX-License-Identifier: GPL-2.0-only

//! Self-sandboxing for the capability-scoped WLAN policy processes.
//!
//! The type states make the intended startup sequence explicit: callers can
//! only reach [`Sandbox<LockedDown>::run`] after setup and a named lockdown
//! profile. Setup never reads an inherited descriptor, so no untrusted byte is
//! consumed before seccomp is active. See `ARCH-wlan-stack-topology`.

#![cfg(target_os = "linux")]

use libc::{c_int, c_ulong};
use std::collections::BTreeSet;
use std::io;
use std::marker::PhantomData;
use std::os::fd::RawFd;

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("WLAN self-sandbox seccomp supports only x86_64 and aarch64");

/// The only supported runtime authority sets.
#[derive(Clone, Copy, Debug)]
pub enum Profile {
    /// WLAN policy IPC and the sole-writer saved-network directory capability.
    Wlancfg {
        control_fd: RawFd,
        persistence_dir_fd: RawFd,
    },
    /// Simulated Wi-Fi IPC and single-threaded runtime mechanics.
    WifiSimulated,
    /// One MT7921 PCI function and its precreated inert IRQ eventfd.
    Mt7921Vfio {
        pci_config_fd: RawFd,
        vfio_fd: RawFd,
        iommufd: RawFd,
        irq_eventfd: RawFd,
    },
}

/// Review trace for the MT7921 profile. Request values are owned by
/// `userspace-vfio::mt7921_seccomp`; this records the corresponding names.
pub const MT7921_VFIO_AUTHORITY_INVENTORY: &str = "fds=stdio,pci-config-rw,vfio-cdev,iommufd-rw,irq-eventfd; vfio-ioctl=DEVICE_BIND_IOMMUFD,DEVICE_ATTACH_IOMMUFD_PT,DEVICE_GET_INFO,DEVICE_GET_REGION_INFO,DEVICE_GET_IRQ_INFO,DEVICE_SET_IRQS,DEVICE_RESET; iommufd-ioctl=IOAS_ALLOC,IOAS_MAP,IOAS_UNMAP,IOMMU_DESTROY; syscalls=read-pci-or-irq,write-pci-or-stdout-stderr,close,ppoll-max-one,mmap-rw-private-anon-offset-zero-or-shared-vfio,mprotect-noexec,munmap,madvise,brk,futex,sched_yield,clock_gettime-monotonic,clock_nanosleep,nanosleep,getrandom,getpid,gettid,sigaltstack-new-only,lseek-pci-only,exit,exit_group; denied=fcntl,dup,fd-creators,open,socket,exec,clone,clone3,signal-handler-or-mask-management,signal-send,sendmsg,recvmsg,recvmmsg,ioctl-other,mmap-other,mmap-exec,mprotect-exec";

#[derive(Debug)]
pub enum Error {
    InvalidCapabilityFd(RawFd),
    PersistenceFdNotInherited(RawFd),
    ProfileAuthorityMismatch,
    ParentChanged,
    NamespacePermissionDenied(io::Error),
    System {
        operation: &'static str,
        source: io::Error,
    },
}

impl Error {
    /// True only when the kernel/outer sandbox refused namespace creation.
    pub fn is_namespace_permission_denied(&self) -> bool {
        matches!(self, Self::NamespacePermissionDenied(_))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCapabilityFd(fd) => write!(f, "invalid or duplicate capability fd {fd}"),
            Self::PersistenceFdNotInherited(fd) => {
                write!(f, "persistence directory fd {fd} was not retained by setup")
            }
            Self::ProfileAuthorityMismatch => {
                f.write_str("lockdown profile does not match setup authority")
            }
            Self::ParentChanged => f.write_str("parent changed during sandbox setup"),
            Self::NamespacePermissionDenied(error) => {
                write!(
                    f,
                    "kernel denied mount/network namespace isolation: {error}"
                )
            }
            Self::System { operation, source } => write!(f, "sandbox {operation}: {source}"),
        }
    }
}

impl std::error::Error for Error {}

pub struct Initial;
pub struct SetupComplete;
pub struct LockedDown;

/// A WLAN service sandbox in state `S`.
pub struct Sandbox<S> {
    inherited: Vec<RawFd>,
    persistence_dir_fd: Option<RawFd>,
    _state: PhantomData<S>,
}

impl Default for Sandbox<Initial> {
    fn default() -> Self {
        Self::new()
    }
}

impl Sandbox<Initial> {
    pub fn new() -> Self {
        Self {
            inherited: Vec::new(),
            persistence_dir_fd: None,
            _state: PhantomData,
        }
    }

    /// Retains exactly `capability_fds` in addition to standard input/output/error.
    /// No retained descriptor is read, written, or sought. When supplied, the
    /// persistence directory is used only as an fd-direct mount source, then
    /// replaced at the same fd number by the `/state` view inside the jail.
    pub fn setup(
        self,
        capability_fds: &[RawFd],
        persistence_dir_fd: Option<RawFd>,
    ) -> Result<Sandbox<SetupComplete>, Error> {
        let mut unique = BTreeSet::new();
        for &fd in capability_fds {
            if fd < 3 || !unique.insert(fd) {
                return Err(Error::InvalidCapabilityFd(fd));
            }
        }
        let inherited: Vec<_> = unique.into_iter().collect();
        if let Some(fd) = persistence_dir_fd
            && !inherited.contains(&fd)
        {
            return Err(Error::PersistenceFdNotInherited(fd));
        }
        require_single_threaded()?;
        let parent = unsafe { libc::getppid() };
        syscall_ok(
            unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) },
            "set parent-death signal",
        )?;
        if unsafe { libc::getppid() } != parent {
            return Err(Error::ParentChanged);
        }
        close_unretained(&inherited)?;

        let result = unsafe { libc::unshare(libc::CLONE_NEWNS | libc::CLONE_NEWNET) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EACCES)) {
                return Err(Error::NamespacePermissionDenied(error));
            }
            return Err(system("create mount/network namespaces", error));
        }
        syscall_ok(
            unsafe {
                libc::mount(
                    std::ptr::null(),
                    c"/".as_ptr(),
                    std::ptr::null(),
                    (libc::MS_REC | libc::MS_PRIVATE) as c_ulong,
                    std::ptr::null(),
                )
            },
            "make mounts private",
        )?;
        let state_mount = persistence_dir_fd.map(clone_directory_mount).transpose()?;
        let empty_root = unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                c"/tmp".as_ptr(),
                c"tmpfs".as_ptr(),
                (libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC) as c_ulong,
                c"size=1048576,mode=0755".as_ptr().cast(),
            )
        };
        if empty_root != 0 {
            let error = system("mount empty root", io::Error::last_os_error());
            if let Some(state_mount) = state_mount {
                let _ = unsafe { libc::close(state_mount) };
            }
            return Err(error);
        }
        if let Some(state_mount) = state_mount {
            if let Err(error) = attach_state_mount(state_mount) {
                unsafe {
                    libc::close(state_mount);
                }
                return Err(error);
            }
            syscall_ok(
                unsafe { libc::close(state_mount) },
                "close detached state mount",
            )?;
        }
        syscall_ok(
            unsafe { libc::chmod(c"/tmp".as_ptr(), 0o555) },
            "seal empty root",
        )?;
        syscall_ok(unsafe { libc::chdir(c"/tmp".as_ptr()) }, "enter empty root")?;
        syscall_ok(unsafe { libc::chroot(c".".as_ptr()) }, "chroot empty root")?;
        syscall_ok(unsafe { libc::chdir(c"/".as_ptr()) }, "enter jailed root")?;
        if let Some(fd) = persistence_dir_fd {
            reopen_state_capability(fd)?;
        }

        syscall_ok(
            unsafe { libc::setgroups(0, std::ptr::null()) },
            "clear supplementary groups",
        )?;
        for capability in 0..64 {
            if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) } == 0 {
                continue;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            return Err(system("clear capability bounding set", error));
        }
        syscall_ok(
            unsafe { libc::setresgid(65534, 65534, 65534) },
            "drop group identity",
        )?;
        syscall_ok(
            unsafe { libc::setresuid(65534, 65534, 65534) },
            "drop user identity",
        )?;
        clear_and_verify_capabilities()?;
        // Linux clears PDEATHSIG during credential changes.
        syscall_ok(
            unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) },
            "re-arm parent-death signal",
        )?;
        if unsafe { libc::getppid() } != parent {
            return Err(Error::ParentChanged);
        }
        syscall_ok(
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
            "set no_new_privs",
        )?;

        Ok(Sandbox {
            inherited,
            persistence_dir_fd,
            _state: PhantomData,
        })
    }
}

impl Sandbox<SetupComplete> {
    /// Installs the architecture-checked seccomp program for a named service.
    pub fn lockdown(self, profile: Profile) -> Result<Sandbox<LockedDown>, Error> {
        if matches!(profile, Profile::WifiSimulated) && self.persistence_dir_fd.is_some() {
            return Err(Error::ProfileAuthorityMismatch);
        }
        if let Profile::Wlancfg {
            control_fd,
            persistence_dir_fd,
        } = profile
        {
            if self.persistence_dir_fd != Some(persistence_dir_fd) {
                return Err(Error::PersistenceFdNotInherited(persistence_dir_fd));
            }
            let mut expected = vec![control_fd, persistence_dir_fd];
            expected.sort_unstable();
            if expected != self.inherited {
                return Err(Error::ProfileAuthorityMismatch);
            }
        }
        if let Profile::Mt7921Vfio {
            pci_config_fd,
            vfio_fd,
            iommufd,
            irq_eventfd,
        } = profile
        {
            if self.persistence_dir_fd.is_some() {
                return Err(Error::ProfileAuthorityMismatch);
            }
            let mut expected = vec![pci_config_fd, vfio_fd, iommufd, irq_eventfd];
            expected.sort_unstable();
            if expected != self.inherited {
                return Err(Error::ProfileAuthorityMismatch);
            }
        }
        install_filter(profile)?;
        Ok(Sandbox {
            inherited: self.inherited,
            persistence_dir_fd: self.persistence_dir_fd,
            _state: PhantomData,
        })
    }
}

impl Sandbox<LockedDown> {
    /// Runs service code only after setup and lockdown have both succeeded.
    pub fn run<R>(self, service: impl FnOnce() -> R) -> R {
        service()
    }
}

fn system(operation: &'static str, source: io::Error) -> Error {
    Error::System { operation, source }
}

fn syscall_ok(result: c_int, operation: &'static str) -> Result<(), Error> {
    if result == 0 {
        Ok(())
    } else {
        Err(system(operation, io::Error::last_os_error()))
    }
}

fn require_single_threaded() -> Result<(), Error> {
    let tasks = std::fs::read_dir("/proc/self/task")
        .map_err(|error| system("inspect process threads", error))?
        .take(2)
        .count();
    if tasks == 1 {
        Ok(())
    } else {
        Err(system(
            "require single-threaded setup",
            io::Error::from_raw_os_error(libc::EBUSY),
        ))
    }
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

fn clear_and_verify_capabilities() -> Result<(), Error> {
    const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
    let mut header = CapabilityHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapabilityData::default(), CapabilityData::default()];
    if unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) } != 0 {
        return Err(system(
            "clear process capabilities",
            io::Error::last_os_error(),
        ));
    }
    syscall_ok(
        unsafe {
            libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_CLEAR_ALL,
                0,
                0,
                0,
            )
        },
        "clear ambient capabilities",
    )?;
    if unsafe { libc::syscall(libc::SYS_capget, &mut header, data.as_mut_ptr()) } != 0 {
        return Err(system(
            "verify process capabilities",
            io::Error::last_os_error(),
        ));
    }
    if data
        .iter()
        .all(|word| word.effective == 0 && word.permitted == 0 && word.inheritable == 0)
    {
        Ok(())
    } else {
        Err(system(
            "verify empty process capabilities",
            io::Error::from_raw_os_error(libc::EPERM),
        ))
    }
}

fn close_unretained(retained: &[RawFd]) -> Result<(), Error> {
    let mut first = 3u32;
    for &fd in retained {
        let fd = fd as u32;
        if first < fd {
            close_range(first, fd - 1)?;
        }
        first = fd.saturating_add(1);
    }
    close_range(first, u32::MAX)
}

fn close_range(first: u32, last: u32) -> Result<(), Error> {
    if first > last {
        return Ok(());
    }
    if unsafe { libc::syscall(libc::SYS_close_range, first, last, 0u32) } == 0 {
        Ok(())
    } else {
        // Iterating only to RLIMIT_NOFILE is not exact: an inherited descriptor
        // can remain open above a limit that was lowered after it was created.
        Err(system(
            "close unretained descriptors",
            io::Error::last_os_error(),
        ))
    }
}

const OPEN_TREE_CLONE: u32 = 1;
const OPEN_TREE_CLOEXEC: u32 = libc::O_CLOEXEC as u32;
const AT_EMPTY_PATH: u32 = 0x1000;
const MOVE_MOUNT_F_EMPTY_PATH: u32 = 0x0000_0004;

fn clone_directory_mount(directory_fd: RawFd) -> Result<RawFd, Error> {
    let fd = unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            directory_fd,
            c"".as_ptr(),
            OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH,
        )
    } as RawFd;
    if fd >= 0 {
        Ok(fd)
    } else {
        Err(system(
            "clone persistence directory mount",
            io::Error::last_os_error(),
        ))
    }
}

fn attach_state_mount(state_mount: RawFd) -> Result<(), Error> {
    syscall_ok(
        unsafe { libc::mkdir(c"/tmp/state".as_ptr(), 0o000) },
        "create state mountpoint",
    )?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_move_mount,
            state_mount,
            c"".as_ptr(),
            libc::AT_FDCWD,
            c"/tmp/state".as_ptr(),
            MOVE_MOUNT_F_EMPTY_PATH,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(system(
            "attach persistence directory mount",
            io::Error::last_os_error(),
        ))
    }
}

fn reopen_state_capability(inherited_fd: RawFd) -> Result<(), Error> {
    let reopened = unsafe {
        libc::open(
            c"/state".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if reopened < 0 {
        return Err(system(
            "reopen jailed state capability",
            io::Error::last_os_error(),
        ));
    }
    let result = unsafe { libc::dup3(reopened, inherited_fd, libc::O_CLOEXEC) };
    let replace_error = io::Error::last_os_error();
    let close_result = unsafe { libc::close(reopened) };
    if result != inherited_fd {
        return Err(system("replace external state capability", replace_error));
    }
    syscall_ok(close_result, "close temporary jailed state capability")
}

const LD_W_ABS: u16 = 0x20;
const JMP_JEQ_K: u16 = 0x15;
const RET_K: u16 = 0x06;
const ALLOW: u32 = 0x7fff_0000;
const KILL_PROCESS: u32 = 0x8000_0000;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

#[repr(C)]
#[derive(Clone, Copy)]
struct Filter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}
#[repr(C)]
struct Program {
    len: u16,
    filter: *const Filter,
}

fn stmt(code: u16, k: u32) -> Filter {
    Filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}
fn jump(k: u32, jt: u8, jf: u8) -> Filter {
    Filter {
        code: JMP_JEQ_K,
        jt,
        jf,
        k,
    }
}
fn arg(index: usize) -> Filter {
    stmt(LD_W_ABS, 16 + (index * 8) as u32)
}

fn arg_high(index: usize) -> Filter {
    stmt(LD_W_ABS, 20 + (index * 8) as u32)
}

fn install_filter(profile: Profile) -> Result<(), Error> {
    let mut f = vec![
        stmt(LD_W_ABS, 4),
        jump(AUDIT_ARCH, 1, 0),
        stmt(RET_K, KILL_PROCESS),
        stmt(LD_W_ABS, 0),
    ];
    if let Profile::Wlancfg {
        control_fd,
        persistence_dir_fd: fd,
    } = profile
    {
        append_openat(&mut f, fd);
        append_renameat(&mut f, fd);
        append_unlinkat(&mut f, fd);
        append_wlancfg_packet_io(&mut f, control_fd);
    }
    if let Profile::Mt7921Vfio {
        pci_config_fd,
        vfio_fd,
        iommufd,
        irq_eventfd,
    } = profile
    {
        append_mt7921_ioctl(&mut f, vfio_fd, iommufd);
        append_fd_only(&mut f, libc::SYS_lseek, pci_config_fd);
        append_fd_set(&mut f, libc::SYS_read, &[pci_config_fd, irq_eventfd]);
        append_fd_set(&mut f, libc::SYS_write, &[pci_config_fd, 1, 2]);
        append_mt_ppoll(&mut f);
    }
    append_runtime_mmap(
        &mut f,
        match profile {
            Profile::Mt7921Vfio { vfio_fd, .. } => Some(vfio_fd),
            _ => None,
        },
    );
    append_no_exec_mprotect(&mut f);
    append_monotonic_clock(&mut f);
    append_monotonic_sleep(&mut f);
    append_madvise(&mut f);
    append_getrandom(&mut f);
    append_sigaltstack_teardown(&mut f);
    #[cfg(target_arch = "x86_64")]
    if matches!(profile, Profile::Wlancfg { .. }) {
        append_wlancfg_poll(&mut f);
    }
    if matches!(profile, Profile::Wlancfg { .. }) {
        append_epoll_pwait(&mut f);
        append_wlancfg_thread_exit_sigmask(&mut f);
    }
    #[cfg(target_arch = "aarch64")]
    if matches!(profile, Profile::Wlancfg { .. }) {
        append_wlancfg_ppoll(&mut f);
    }
    match profile {
        Profile::Wlancfg { .. } => {
            append_epoll_ctl(&mut f);
        }
        Profile::WifiSimulated | Profile::Mt7921Vfio { .. } => {}
    }
    for number in allowed(profile) {
        f.push(jump(number as u32, 0, 1));
        f.push(stmt(RET_K, ALLOW));
    }
    f.push(stmt(RET_K, KILL_PROCESS));
    let program = Program {
        len: f.len().try_into().expect("small seccomp program"),
        filter: f.as_ptr(),
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_TSYNC,
            &program,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(system(
            "install synchronized seccomp",
            io::Error::last_os_error(),
        ))
    }
}

/// Installs only the runtime filter for cross-crate integration tests.
///
/// Production services must use [`Sandbox`] so namespace, filesystem,
/// identity, capability, and descriptor setup cannot be bypassed. This hook is
/// feature-gated solely to exercise a real consumer under the fatal filter on
/// build hosts that prohibit namespace creation.
#[cfg(feature = "filter-integration-test")]
#[doc(hidden)]
pub fn install_runtime_filter_for_integration_test(profile: Profile) -> Result<(), Error> {
    syscall_ok(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
        "set integration-test no_new_privs",
    )?;
    install_filter(profile)
}

fn append_mt7921_ioctl(f: &mut Vec<Filter>, vfio_fd: RawFd, iommufd: RawFd) {
    let dispatch = f.len();
    f.push(jump(libc::SYS_ioctl as u32, 0, 0));
    f.push(arg_high(0));
    let fd_high = f.len();
    f.push(jump(0, 0, 0));
    append_ioctl_fd_requests(f, vfio_fd, userspace_vfio::mt7921_seccomp::VFIO_REQUESTS);
    append_ioctl_fd_requests(f, iommufd, userspace_vfio::mt7921_seccomp::IOMMUFD_REQUESTS);
    let denied = f.len();
    f.push(stmt(RET_K, KILL_PROCESS));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small ioctl filter");
    f[fd_high].jf = (denied - fd_high - 1)
        .try_into()
        .expect("small ioctl filter");
}

fn append_ioctl_fd_requests(f: &mut Vec<Filter>, fd: RawFd, requests: &[u64]) {
    for &request in requests {
        f.push(arg(0));
        f.push(jump(fd as u32, 0, 5));
        f.push(arg_high(1));
        f.push(jump(0, 0, 3));
        f.push(arg(1));
        f.push(jump(request as u32, 0, 1));
        f.push(stmt(RET_K, ALLOW));
    }
}

fn append_runtime_mmap(f: &mut Vec<Filter>, device_fd: Option<RawFd>) {
    let dispatch = f.len();
    f.push(jump(libc::SYS_mmap as u32, 0, 0));

    // Linux's mmap ABI consumes a 32-bit fd and libc leaves the seccomp
    // argument's upper word zero, including for fd -1.
    f.push(arg_high(4));
    f.push(jump(0, 1, 0));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(arg(4));
    let anonymous_fd = f.len();
    f.push(jump(u32::MAX, 0, 0));
    let device_fd_check = device_fd.map(|fd| {
        let check = f.len();
        f.push(jump(fd as u32, 0, 0));
        check
    });
    f.push(stmt(RET_K, KILL_PROCESS));

    let anonymous = f.len();
    let mut anonymous_failures = Vec::new();
    for (argument, expected) in [
        (2, (libc::PROT_READ | libc::PROT_WRITE) as u32),
        (3, (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u32),
        (5, 0),
    ] {
        f.push(arg_high(argument));
        anonymous_failures.push(f.len());
        f.push(jump(0, 0, 0));
        f.push(arg(argument));
        anonymous_failures.push(f.len());
        f.push(jump(expected, 0, 0));
    }
    f.push(stmt(RET_K, ALLOW));
    let anonymous_denied = f.len();
    f.push(stmt(RET_K, KILL_PROCESS));

    let device = f.len();
    let mut device_failures = Vec::new();
    for (argument, expected) in [
        (2, (libc::PROT_READ | libc::PROT_WRITE) as u32),
        (3, libc::MAP_SHARED as u32),
    ] {
        f.push(arg_high(argument));
        device_failures.push(f.len());
        f.push(jump(0, 0, 0));
        f.push(arg(argument));
        device_failures.push(f.len());
        f.push(jump(expected, 0, 0));
    }
    f.push(stmt(RET_K, ALLOW));
    let device_denied = f.len();
    f.push(stmt(RET_K, KILL_PROCESS));

    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small mmap filter");
    f[anonymous_fd].jt = (anonymous - anonymous_fd - 1)
        .try_into()
        .expect("small mmap filter");
    for failure in anonymous_failures {
        f[failure].jf = (anonymous_denied - failure - 1)
            .try_into()
            .expect("small mmap filter");
    }
    if let Some(check) = device_fd_check {
        f[check].jt = (device - check - 1).try_into().expect("small mmap filter");
    }
    for failure in device_failures {
        f[failure].jf = (device_denied - failure - 1)
            .try_into()
            .expect("small mmap filter");
    }
}

fn append_no_exec_mprotect(f: &mut Vec<Filter>) {
    f.push(jump(libc::SYS_mprotect as u32, 0, 4));
    f.push(arg(2));
    f.push(Filter {
        code: 0x45,
        jt: 0,
        jf: 1,
        k: libc::PROT_EXEC as u32,
    });
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(RET_K, ALLOW));
    f.push(stmt(LD_W_ABS, 0));
}

fn append_monotonic_clock(f: &mut Vec<Filter>) {
    f.push(jump(libc::SYS_clock_gettime as u32, 0, 6));
    f.push(arg_high(0));
    f.push(jump(0, 0, 2));
    f.push(arg(0));
    f.push(jump(libc::CLOCK_MONOTONIC as u32, 1, 0));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(RET_K, ALLOW));
    f.push(stmt(LD_W_ABS, 0));
}

fn append_monotonic_sleep(f: &mut Vec<Filter>) {
    let dispatch = f.len();
    f.push(jump(libc::SYS_clock_nanosleep as u32, 0, 0));
    f.push(arg_high(0));
    f.push(jump(0, 0, 4));
    f.push(arg(0));
    f.push(jump(libc::CLOCK_MONOTONIC as u32, 0, 2));
    f.push(arg(1));
    f.push(jump(0, 1, 0));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(RET_K, ALLOW));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small sleep filter");
}

fn append_madvise(f: &mut Vec<Filter>) {
    let dispatch = f.len();
    f.push(jump(libc::SYS_madvise as u32, 0, 0));
    f.push(arg(2));
    for advice in [
        libc::MADV_DONTNEED,
        libc::MADV_DONTDUMP,
        libc::MADV_FREE,
        libc::MADV_NOHUGEPAGE,
    ] {
        f.push(jump(advice as u32, 0, 1));
        f.push(stmt(RET_K, ALLOW));
    }
    f.push(stmt(RET_K, KILL_PROCESS));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small madvise filter");
}

fn append_getrandom(f: &mut Vec<Filter>) {
    let dispatch = f.len();
    f.push(jump(libc::SYS_getrandom as u32, 0, 0));
    f.push(arg_high(2));
    f.push(jump(0, 0, 4));
    f.push(arg(2));
    f.push(jump(0, 1, 0));
    f.push(jump(libc::GRND_NONBLOCK, 0, 1));
    f.push(stmt(RET_K, ALLOW));
    f.push(stmt(RET_K, KILL_PROCESS));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small getrandom filter");
}

#[cfg(target_arch = "x86_64")]
fn append_wlancfg_poll(f: &mut Vec<Filter>) {
    let dispatch = f.len();
    f.push(jump(libc::SYS_poll as u32, 0, 0));
    f.push(arg_high(1));
    f.push(jump(0, 0, 4));
    f.push(arg(1));
    f.push(jump(2, 0, 2));
    f.push(arg(2));
    f.push(jump(u32::MAX, 1, 0));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(RET_K, ALLOW));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small poll filter");
}

fn append_fd_set(f: &mut Vec<Filter>, syscall: libc::c_long, fds: &[RawFd]) {
    let dispatch = f.len();
    f.push(jump(syscall as u32, 0, 0));
    f.push(arg_high(0));
    f.push(jump(0, 0, (fds.len() * 2 + 1) as u8));
    f.push(arg(0));
    for &fd in fds {
        f.push(jump(fd as u32, 0, 1));
        f.push(stmt(RET_K, ALLOW));
    }
    f.push(stmt(RET_K, KILL_PROCESS));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    f[dispatch].jf = (reload - dispatch - 1).try_into().expect("small fd filter");
}

fn append_mt_ppoll(f: &mut Vec<Filter>) {
    let dispatch = f.len();
    f.push(jump(libc::SYS_ppoll as u32, 0, 0));
    f.push(arg_high(1));
    f.push(jump(0, 0, 6));
    f.push(arg(1));
    // One precreated IRQ eventfd is the complete MT interrupt inventory.
    f.push(Filter {
        code: 0x25,
        jt: 4,
        jf: 0,
        k: 1,
    });
    f.push(arg_high(3));
    f.push(jump(0, 0, 2));
    f.push(arg(3));
    f.push(jump(0, 1, 0));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(RET_K, ALLOW));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small ppoll filter");
}

fn append_epoll_ctl(f: &mut Vec<Filter>) {
    let dispatch = f.len();
    f.push(jump(libc::SYS_epoll_ctl as u32, 0, 0));
    f.push(arg_high(1));
    f.push(jump(0, 0, 4));
    f.push(arg(1));
    f.push(jump(libc::EPOLL_CTL_ADD as u32, 3, 0));
    f.push(jump(libc::EPOLL_CTL_MOD as u32, 2, 0));
    f.push(jump(libc::EPOLL_CTL_DEL as u32, 1, 0));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(RET_K, ALLOW));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small epoll_ctl filter");
}

fn append_epoll_pwait(f: &mut Vec<Filter>) {
    let dispatch = f.len();
    f.push(jump(libc::SYS_epoll_pwait as u32, 0, 0));
    f.push(arg_high(4));
    f.push(jump(0, 0, 2));
    f.push(arg(4));
    f.push(jump(0, 1, 0));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(RET_K, ALLOW));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small epoll_pwait filter");
}

fn append_wlancfg_packet_io(f: &mut Vec<Filter>, control_fd: RawFd) {
    // The policy protocol is capped at MAX_PACKET=8192. Connected seqpacket
    // transport needs no address and deliberately supplies no ancillary buffer.
    for (syscall, flags) in [
        (
            libc::SYS_sendto,
            (libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) as u32,
        ),
        (
            libc::SYS_recvfrom,
            (libc::MSG_DONTWAIT | libc::MSG_TRUNC) as u32,
        ),
    ] {
        let dispatch = f.len();
        f.push(jump(syscall as u32, 0, 0));
        let mut failures = Vec::new();
        for (argument, expected) in [(0, control_fd as u32), (3, flags), (4, 0), (5, 0)] {
            f.push(arg_high(argument));
            failures.push(f.len());
            f.push(jump(0, 0, 0));
            f.push(arg(argument));
            failures.push(f.len());
            f.push(jump(expected, 0, 0));
        }
        f.push(arg_high(2));
        failures.push(f.len());
        f.push(jump(0, 0, 0));
        f.push(arg(2));
        let oversized = f.len();
        f.push(Filter {
            code: 0x25,
            jt: 0,
            jf: 1,
            k: 8192,
        });
        f.push(stmt(RET_K, KILL_PROCESS));
        f.push(stmt(RET_K, ALLOW));
        let denied = f.len();
        f.push(stmt(RET_K, KILL_PROCESS));
        let reload = f.len();
        f.push(stmt(LD_W_ABS, 0));
        for failure in failures {
            f[failure].jf = (denied - failure - 1)
                .try_into()
                .expect("small packet I/O filter");
        }
        f[oversized].jt = 0;
        f[dispatch].jf = (reload - dispatch - 1)
            .try_into()
            .expect("small packet I/O filter");
    }
}

fn append_sigaltstack_teardown(f: &mut Vec<Filter>) {
    // Rust's stack-overflow guard removes its alternate stack while returning
    // from main. Classic seccomp cannot inspect the pointed-to SS_DISABLE
    // structure, but it can require a new-stack pointer and forbid querying
    // the old stack. Handler installation and signal delivery remain denied.
    let dispatch = f.len();
    f.push(jump(libc::SYS_sigaltstack as u32, 0, 0));
    f.push(arg_high(0));
    let new_high = f.len();
    f.push(jump(0, 0, 0));
    let old_stack = f.len();
    f.push(arg_high(1));
    f.push(jump(0, 0, 2));
    f.push(arg(1));
    f.push(jump(0, 1, 0));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(RET_K, ALLOW));
    let check_new_low = f.len();
    f.push(arg(0));
    f.push(jump(0, 0, 1));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(arg_high(1));
    f.push(jump(0, 0, 2));
    f.push(arg(1));
    f.push(jump(0, 1, 0));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(RET_K, ALLOW));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    f[new_high].jt = (check_new_low - new_high - 1)
        .try_into()
        .expect("small sigaltstack filter");
    f[new_high].jf = (old_stack - new_high - 1)
        .try_into()
        .expect("small sigaltstack filter");
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small sigaltstack filter");
}

fn append_wlancfg_thread_exit_sigmask(f: &mut Vec<Filter>) {
    // glibc blocks all signals except its internal cancellation signal while
    // the pre-lockdown owner thread exits. Seccomp cannot inspect the mask's
    // pointee, but every scalar and pointer direction remains constrained.
    let dispatch = f.len();
    f.push(jump(libc::SYS_rt_sigprocmask as u32, 0, 0));
    let mut failures = Vec::new();
    for (argument, expected) in [(0, libc::SIG_BLOCK as u32), (2, 0), (3, 8)] {
        f.push(arg_high(argument));
        failures.push(f.len());
        f.push(jump(0, 0, 0));
        f.push(arg(argument));
        failures.push(f.len());
        f.push(jump(expected, 0, 0));
    }
    // arg1 is the one required non-null pointer. Either half may carry it.
    f.push(arg_high(1));
    let set_high = f.len();
    f.push(jump(0, 0, 0));
    f.push(stmt(RET_K, ALLOW));
    let set_low = f.len();
    f.push(arg(1));
    f.push(jump(0, 0, 1));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(RET_K, ALLOW));
    let denied = f.len();
    f.push(stmt(RET_K, KILL_PROCESS));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    for failure in failures {
        f[failure].jf = (denied - failure - 1)
            .try_into()
            .expect("small sigprocmask filter");
    }
    f[set_high].jt = (set_low - set_high - 1)
        .try_into()
        .expect("small sigprocmask filter");
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small sigprocmask filter");
}

#[cfg(target_arch = "aarch64")]
fn append_wlancfg_ppoll(f: &mut Vec<Filter>) {
    let dispatch = f.len();
    f.push(jump(libc::SYS_ppoll as u32, 0, 0));
    let mut failures = Vec::new();
    for (argument, expected) in [(1, 2_u32), (2, 0), (3, 0)] {
        f.push(arg_high(argument));
        failures.push(f.len());
        f.push(jump(0, 0, 0));
        f.push(arg(argument));
        failures.push(f.len());
        f.push(jump(expected, 0, 0));
    }
    f.push(stmt(RET_K, ALLOW));
    let denied = f.len();
    f.push(stmt(RET_K, KILL_PROCESS));
    let reload = f.len();
    f.push(stmt(LD_W_ABS, 0));
    for failure in failures {
        f[failure].jf = (denied - failure - 1)
            .try_into()
            .expect("small ppoll filter");
    }
    f[dispatch].jf = (reload - dispatch - 1)
        .try_into()
        .expect("small ppoll filter");
}

fn append_fd_only(f: &mut Vec<Filter>, syscall: libc::c_long, fd: RawFd) {
    f.push(jump(syscall as u32, 0, 6));
    f.push(arg_high(0));
    f.push(jump(0, 0, 2));
    f.push(arg(0));
    f.push(jump(fd as u32, 1, 0));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(RET_K, ALLOW));
    f.push(stmt(LD_W_ABS, 0));
}

fn append_openat(f: &mut Vec<Filter>, fd: RawFd) {
    // Exactly the read and atomic-create flag sets used by HostPolicyStorage at
    // revision 96e37e09. O_NOFOLLOW is mandatory; O_PATH/device-style expansion
    // and all ambient/absolute opens are therefore excluded.
    const READ_FLAGS: u32 =
        (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK) as u32;
    const CREATE_FLAGS: u32 = (libc::O_WRONLY
        | libc::O_CREAT
        | libc::O_EXCL
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK) as u32;
    f.push(jump(libc::SYS_openat as u32, 0, 7));
    f.push(arg(0));
    f.push(jump(fd as u32, 0, 4));
    f.push(arg(2));
    f.push(jump(READ_FLAGS, 1, 0));
    f.push(jump(CREATE_FLAGS, 0, 1));
    f.push(stmt(RET_K, ALLOW));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(LD_W_ABS, 0));
}

fn append_renameat(f: &mut Vec<Filter>, fd: RawFd) {
    #[cfg(target_arch = "x86_64")]
    {
        f.push(jump(libc::SYS_renameat as u32, 0, 7));
        f.push(arg(0));
        f.push(jump(fd as u32, 0, 4));
        f.push(arg(2));
        f.push(jump(fd as u32, 0, 1));
        f.push(stmt(RET_K, ALLOW));
        f.push(stmt(RET_K, KILL_PROCESS));
    }
    #[cfg(target_arch = "aarch64")]
    {
        // libc renameat uses generic Linux syscall 38 on LP64 aarch64. The
        // libc Rust constant is omitted even though the kernel ABI has it.
        const SYS_RENAMEAT: u32 = 38;
        f.push(jump(SYS_RENAMEAT, 0, 7));
        f.push(arg(0));
        f.push(jump(fd as u32, 0, 4));
        f.push(arg(2));
        f.push(jump(fd as u32, 0, 1));
        f.push(stmt(RET_K, ALLOW));
        f.push(stmt(RET_K, KILL_PROCESS));
    }
    f.push(stmt(LD_W_ABS, 0));
}

fn append_unlinkat(f: &mut Vec<Filter>, fd: RawFd) {
    f.push(jump(libc::SYS_unlinkat as u32, 0, 7));
    f.push(arg(0));
    f.push(jump(fd as u32, 0, 4));
    f.push(arg(2));
    f.push(jump(0, 0, 1));
    f.push(stmt(RET_K, ALLOW));
    f.push(stmt(RET_K, KILL_PROCESS));
    f.push(stmt(LD_W_ABS, 0));
}

fn allowed(profile: Profile) -> Vec<libc::c_long> {
    // Authority-bearing calls are dispatched above;
    // role-specific IPC and waits are appended below. open/socket/connect,
    // fcntl/dup, executable mappings, exec, and process creation stay denied.
    let mut calls = vec![
        libc::SYS_close,
        libc::SYS_munmap,
        libc::SYS_brk,
        libc::SYS_futex,
        libc::SYS_sched_yield,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ];
    match profile {
        Profile::Wlancfg { .. } => calls.extend([
            libc::SYS_read,
            libc::SYS_write,
            // Tokio's current-thread time driver and the bounded control owner.
            libc::SYS_fstat,
            // HostPolicyStorage reads/writes newly constrained regular files
            // and durably syncs the file and inherited state directory.
            libc::SYS_fsync,
        ]),
        Profile::WifiSimulated => calls.extend([
            libc::SYS_read,
            libc::SYS_write,
            // Its two prevalidated seqpacket seams are its only runtime I/O.
            libc::SYS_recvmsg,
            libc::SYS_sendmsg,
        ]),
        Profile::Mt7921Vfio { .. } => {}
    }
    #[cfg(target_arch = "x86_64")]
    if matches!(profile, Profile::Wlancfg { .. }) {
        calls.push(libc::SYS_epoll_wait);
    }
    calls
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;

    #[test]
    fn mt7921_profile_requires_exactly_four_capabilities() {
        let setup = Sandbox::<SetupComplete> {
            inherited: vec![3, 4, 5, 6],
            persistence_dir_fd: Some(3),
            _state: PhantomData,
        };
        assert!(matches!(
            setup.lockdown(Profile::Mt7921Vfio {
                pci_config_fd: 3,
                vfio_fd: 4,
                iommufd: 5,
                irq_eventfd: 6,
            }),
            Err(Error::ProfileAuthorityMismatch)
        ));
    }

    #[test]
    fn wlancfg_profile_rejects_missing_aliasing_or_extra_capabilities() {
        for (inherited, control_fd, persistence_dir_fd) in [
            (vec![3, 4], 5, 4),
            (vec![3, 4], 4, 4),
            (vec![3, 4, 5], 3, 4),
        ] {
            let setup = Sandbox::<SetupComplete> {
                inherited,
                persistence_dir_fd: Some(4),
                _state: PhantomData,
            };
            assert!(matches!(
                setup.lockdown(Profile::Wlancfg {
                    control_fd,
                    persistence_dir_fd,
                }),
                Err(Error::ProfileAuthorityMismatch)
            ));
        }
    }

    #[test]
    fn role_positive_paths_execute() {
        if let Ok(role) = std::env::var("DRV_SANDBOX_POSITIVE") {
            positive_body(&role);
        }
        for role in ["wifi", "wlancfg", "mt"] {
            positive_child(role);
        }
    }

    #[test]
    fn forbidden_syscalls_and_arguments_kill_the_process() {
        if let Ok(probe) = std::env::var("DRV_SANDBOX_KILL_PROBE") {
            kill_body(&probe);
        }
        for probe in [
            "wifi:open",
            "wifi:eventfd",
            "wifi:clone3",
            "wlancfg:open",
            "wlancfg:bad-eventfd-flags",
            "wlancfg:bad-epoll-flags",
            "wlancfg:bad-epoll-operation",
            "wlancfg:bad-epoll-operation-high",
            "wlancfg:epoll-pwait-signal-mask",
            "wlancfg:sigmask-wrong-how",
            "wlancfg:sigmask-null-set",
            "wlancfg:sigmask-old-set",
            "wlancfg:sigmask-wrong-size",
            "wlancfg:poll-one",
            "wlancfg:poll-count-high",
            "wlancfg:recvmsg",
            "wlancfg:recvmmsg",
            "wlancfg:sendmsg",
            "wlancfg:fcntl",
            "wlancfg:sendto-wrong-fd",
            "wlancfg:sendto-fd-high",
            "wlancfg:sendto-wrong-flags",
            "wlancfg:sendto-address",
            "wlancfg:sendto-oversized",
            "wlancfg:recvfrom-wrong-fd",
            "wlancfg:recvfrom-wrong-flags",
            "wlancfg:recvfrom-address",
            "wlancfg:recvfrom-oversized",
            "mt:wrong-ioctl-fd",
            "mt:fcntl",
            "mt:dup",
            "mt:eventfd",
            "mt:timerfd",
            "mt:clone",
            "mt:clone3",
            "mt:rt-sigaction",
            "mt:sigaltstack",
            "mt:sigaltstack-query",
            "mt:tgkill",
            "mt:mmap-exec",
            "mt:mmap-fd-high-alias",
            "mt:mmap-vfio-cross-alias",
            "mt:mmap-anonymous-offset",
            "mt:mmap-anonymous-read-only",
            "mt:realtime-sleep",
            "mt:sleep-clock-high",
            "mt:ppoll-two",
            "mt:recvmsg",
        ] {
            kill_child(probe);
        }
    }

    fn positive_child(role: &str) {
        const TEST: &str = "filter_tests::role_positive_paths_execute";
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(TEST)
            .env("DRV_SANDBOX_POSITIVE", role)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{role} positive child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn positive_body(role: &str) -> ! {
        let fds = resources();
        if role == "wlancfg" {
            let byte = b'R';
            assert_eq!(
                unsafe { libc::send(fds[1], (&byte as *const u8).cast(), 1, 0) },
                1
            );
        }
        enable_filter(profile(role, fds));
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        assert_eq!(unsafe { libc::munmap(mapping, 4096) }, 0);
        match role {
            "wifi" => {
                let byte = b'W';
                let mut send_iov = libc::iovec {
                    iov_base: (&byte as *const u8).cast_mut().cast(),
                    iov_len: 1,
                };
                let mut send: libc::msghdr = unsafe { std::mem::zeroed() };
                send.msg_iov = &mut send_iov;
                send.msg_iovlen = 1;
                assert_eq!(unsafe { libc::sendmsg(fds[0], &send, 0) }, 1);
                let mut received = 0_u8;
                let mut recv_iov = libc::iovec {
                    iov_base: (&mut received as *mut u8).cast(),
                    iov_len: 1,
                };
                let mut recv: libc::msghdr = unsafe { std::mem::zeroed() };
                recv.msg_iov = &mut recv_iov;
                recv.msg_iovlen = 1;
                assert_eq!(unsafe { libc::recvmsg(fds[1], &mut recv, 0) }, 1);
                assert_eq!(received, byte);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            "wlancfg" => {
                let mut received = 0_u8;
                assert_eq!(
                    unsafe {
                        libc::recvfrom(
                            fds[0],
                            (&mut received as *mut u8).cast(),
                            1,
                            libc::MSG_DONTWAIT | libc::MSG_TRUNC,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        )
                    },
                    1
                );
                assert_eq!(received, b'R');
                let sent = b'S';
                assert_eq!(
                    unsafe {
                        libc::sendto(
                            fds[0],
                            (&sent as *const u8).cast(),
                            1,
                            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                            std::ptr::null(),
                            0,
                        )
                    },
                    1
                );
                let mut epoll_event: libc::epoll_event = unsafe { std::mem::zeroed() };
                assert_eq!(
                    unsafe { libc::epoll_pwait(fds[1], &mut epoll_event, 1, 0, std::ptr::null()) },
                    -1
                );
                let pollfd = libc::pollfd {
                    fd: fds[3],
                    events: libc::POLLIN,
                    revents: 0,
                };
                let mut pollfds = [pollfd; 2];
                assert_eq!(unsafe { libc::poll(pollfds.as_mut_ptr(), 2, -1) }, 2);
                let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
                assert_eq!(unsafe { libc::sigfillset(&mut mask) }, 0);
                assert_eq!(
                    unsafe {
                        libc::syscall(
                            libc::SYS_rt_sigprocmask,
                            libc::SIG_BLOCK,
                            &mask,
                            std::ptr::null_mut::<libc::sigset_t>(),
                            8,
                        )
                    },
                    0
                );
                assert_eq!(unsafe { libc::close(fds[2]) }, 0);
                let read_flags =
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
                let replacement =
                    unsafe { libc::openat(fds[2], c"/dev/null".as_ptr(), read_flags) };
                assert_eq!(
                    replacement, fds[2],
                    "absolute openat should demonstrate slot reuse"
                );
                assert_eq!(
                    unsafe { libc::openat(replacement, c"relative".as_ptr(), read_flags) },
                    -1
                );
                assert_eq!(
                    io::Error::last_os_error().raw_os_error(),
                    Some(libc::ENOTDIR)
                );
                assert_eq!(
                    unsafe {
                        libc::renameat(replacement, c"old".as_ptr(), replacement, c"new".as_ptr())
                    },
                    -1
                );
                assert_eq!(
                    io::Error::last_os_error().raw_os_error(),
                    Some(libc::ENOTDIR)
                );
                assert_eq!(
                    unsafe { libc::unlinkat(replacement, c"relative".as_ptr(), 0) },
                    -1
                );
                assert_eq!(
                    io::Error::last_os_error().raw_os_error(),
                    Some(libc::ENOTDIR)
                );
                assert_eq!(unsafe { libc::close(replacement) }, 0);
            }
            "mt" => {
                let request = userspace_vfio::mt7921_seccomp::VFIO_REQUESTS[2];
                assert_eq!(unsafe { libc::ioctl(fds[1], request, 0) }, -1);
                assert_eq!(
                    io::Error::last_os_error().raw_os_error(),
                    Some(libc::ENOTTY)
                );
                let mut pollfd = libc::pollfd {
                    fd: fds[3],
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
                    unsafe { libc::read(fds[3], (&mut count as *mut u64).cast(), 8) },
                    8
                );
                assert_eq!(count, 1);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            _ => unreachable!(),
        }
        let disable_stack = libc::stack_t {
            ss_sp: std::ptr::null_mut(),
            ss_flags: libc::SS_DISABLE,
            ss_size: libc::SIGSTKSZ,
        };
        assert_eq!(
            unsafe { libc::sigaltstack(&disable_stack, std::ptr::null_mut()) },
            0
        );
        unsafe { libc::_exit(0) }
    }

    fn kill_child(probe: &str) {
        const TEST: &str = "filter_tests::forbidden_syscalls_and_arguments_kill_the_process";
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(TEST)
            .env("DRV_SANDBOX_KILL_PROBE", probe)
            .status()
            .unwrap();
        assert_eq!(
            status.signal(),
            Some(libc::SIGSYS),
            "{probe} did not die with SIGSYS: {status}"
        );
    }

    fn kill_body(probe: &str) -> ! {
        let (role, operation) = probe.split_once(':').unwrap();
        let fds = resources();
        enable_filter(profile(role, fds));
        violate(operation, fds);
        unsafe { libc::_exit(99) }
    }

    fn resources() -> [RawFd; 4] {
        let mut sockets = [0; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, sockets.as_mut_ptr())
            },
            0
        );
        let third = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
        let irq = unsafe { libc::eventfd(1, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(third >= 0 && irq >= 0);
        [sockets[0], sockets[1], third, irq]
    }

    fn profile(role: &str, fds: [RawFd; 4]) -> Profile {
        match role {
            "wifi" => Profile::WifiSimulated,
            "wlancfg" => Profile::Wlancfg {
                control_fd: fds[0],
                persistence_dir_fd: fds[2],
            },
            "mt" => Profile::Mt7921Vfio {
                pci_config_fd: fds[0],
                vfio_fd: fds[1],
                iommufd: fds[2],
                irq_eventfd: fds[3],
            },
            _ => unreachable!(),
        }
    }

    fn enable_filter(profile: Profile) {
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
            0
        );
        install_filter(profile).unwrap();
    }

    fn violate(operation: &str, fds: [RawFd; 4]) {
        unsafe {
            match operation {
                "open" => {
                    libc::open(c"/etc/passwd".as_ptr(), libc::O_RDONLY);
                }
                "bad-eventfd-flags" => {
                    libc::eventfd(0, libc::EFD_CLOEXEC);
                }
                "bad-epoll-flags" => {
                    libc::epoll_create1(0);
                }
                "bad-epoll-operation" => {
                    libc::epoll_ctl(fds[1], 99, fds[0], std::ptr::null_mut());
                }
                "bad-epoll-operation-high" => {
                    let operation =
                        ((libc::SYS_write as u64) << 32) | libc::EPOLL_CTL_ADD as u32 as u64;
                    libc::syscall(
                        libc::SYS_epoll_ctl,
                        fds[1],
                        operation,
                        fds[0],
                        std::ptr::null_mut::<libc::epoll_event>(),
                    );
                }
                "epoll-pwait-signal-mask" => {
                    let mask: libc::sigset_t = std::mem::zeroed();
                    libc::syscall(
                        libc::SYS_epoll_pwait,
                        fds[1],
                        std::ptr::null_mut::<libc::epoll_event>(),
                        1,
                        0,
                        &mask,
                        8,
                    );
                }
                "sigmask-wrong-how" => {
                    let mask: libc::sigset_t = std::mem::zeroed();
                    libc::syscall(libc::SYS_rt_sigprocmask, libc::SIG_SETMASK, &mask, 0, 8);
                }
                "sigmask-null-set" => {
                    libc::syscall(libc::SYS_rt_sigprocmask, libc::SIG_BLOCK, 0, 0, 8);
                }
                "sigmask-old-set" => {
                    let mask: libc::sigset_t = std::mem::zeroed();
                    let mut old: libc::sigset_t = std::mem::zeroed();
                    libc::syscall(
                        libc::SYS_rt_sigprocmask,
                        libc::SIG_BLOCK,
                        &mask,
                        &mut old,
                        8,
                    );
                }
                "sigmask-wrong-size" => {
                    let mask: libc::sigset_t = std::mem::zeroed();
                    libc::syscall(libc::SYS_rt_sigprocmask, libc::SIG_BLOCK, &mask, 0, 16);
                }
                #[cfg(target_arch = "x86_64")]
                "poll-one" => {
                    let mut pollfd = libc::pollfd {
                        fd: fds[3],
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    libc::syscall(libc::SYS_poll, &mut pollfd, 1_u64, -1_i64);
                }
                #[cfg(target_arch = "aarch64")]
                "poll-one" => {
                    let mut pollfd = libc::pollfd {
                        fd: fds[3],
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    libc::syscall(
                        libc::SYS_ppoll,
                        &mut pollfd,
                        1_u64,
                        std::ptr::null::<libc::timespec>(),
                        std::ptr::null::<libc::sigset_t>(),
                        8,
                    );
                }
                #[cfg(target_arch = "x86_64")]
                "poll-count-high" => {
                    let mut pollfds = [libc::pollfd {
                        fd: fds[3],
                        events: libc::POLLIN,
                        revents: 0,
                    }; 2];
                    libc::syscall(
                        libc::SYS_poll,
                        pollfds.as_mut_ptr(),
                        (1_u64 << 32) | 2,
                        -1_i64,
                    );
                }
                #[cfg(target_arch = "aarch64")]
                "poll-count-high" => {
                    let mut pollfds = [libc::pollfd {
                        fd: fds[3],
                        events: libc::POLLIN,
                        revents: 0,
                    }; 2];
                    libc::syscall(
                        libc::SYS_ppoll,
                        pollfds.as_mut_ptr(),
                        (1_u64 << 32) | 2,
                        std::ptr::null::<libc::timespec>(),
                        std::ptr::null::<libc::sigset_t>(),
                        8,
                    );
                }
                "recvmsg" => {
                    let mut message: libc::msghdr = std::mem::zeroed();
                    libc::recvmsg(fds[0], &mut message, libc::MSG_DONTWAIT);
                }
                "recvmmsg" => {
                    let mut messages: [libc::mmsghdr; 1] = std::mem::zeroed();
                    libc::recvmmsg(
                        fds[0],
                        messages.as_mut_ptr(),
                        1,
                        libc::MSG_DONTWAIT,
                        std::ptr::null_mut(),
                    );
                }
                "sendmsg" => {
                    let message: libc::msghdr = std::mem::zeroed();
                    libc::sendmsg(fds[0], &message, libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL);
                }
                "sendto-wrong-fd" => {
                    libc::sendto(
                        fds[1],
                        std::ptr::null(),
                        0,
                        libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                        std::ptr::null(),
                        0,
                    );
                }
                "sendto-fd-high" => {
                    libc::syscall(
                        libc::SYS_sendto,
                        (1_u64 << 32) | fds[0] as u32 as u64,
                        std::ptr::null::<u8>(),
                        0,
                        libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                        std::ptr::null::<libc::sockaddr>(),
                        0,
                    );
                }
                "sendto-wrong-flags" => {
                    libc::sendto(
                        fds[0],
                        std::ptr::null(),
                        0,
                        libc::MSG_DONTWAIT,
                        std::ptr::null(),
                        0,
                    );
                }
                "sendto-address" => {
                    let address: libc::sockaddr = std::mem::zeroed();
                    libc::sendto(
                        fds[0],
                        std::ptr::null(),
                        0,
                        libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                        &address,
                        0,
                    );
                }
                "sendto-oversized" => {
                    libc::sendto(
                        fds[0],
                        std::ptr::null(),
                        8193,
                        libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                        std::ptr::null(),
                        0,
                    );
                }
                "recvfrom-wrong-fd" => {
                    libc::recvfrom(
                        fds[1],
                        std::ptr::null_mut(),
                        0,
                        libc::MSG_DONTWAIT | libc::MSG_TRUNC,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    );
                }
                "recvfrom-wrong-flags" => {
                    libc::recvfrom(
                        fds[0],
                        std::ptr::null_mut(),
                        0,
                        libc::MSG_DONTWAIT,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    );
                }
                "recvfrom-address" => {
                    let mut address: libc::sockaddr = std::mem::zeroed();
                    libc::recvfrom(
                        fds[0],
                        std::ptr::null_mut(),
                        0,
                        libc::MSG_DONTWAIT | libc::MSG_TRUNC,
                        &mut address,
                        std::ptr::null_mut(),
                    );
                }
                "recvfrom-oversized" => {
                    libc::recvfrom(
                        fds[0],
                        std::ptr::null_mut(),
                        8193,
                        libc::MSG_DONTWAIT | libc::MSG_TRUNC,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    );
                }
                "wrong-ioctl-fd" => {
                    libc::ioctl(fds[0], userspace_vfio::mt7921_seccomp::VFIO_REQUESTS[2], 0);
                }
                "fcntl" => {
                    libc::fcntl(fds[0], libc::F_GETFD);
                }
                "dup" => {
                    libc::dup(fds[0]);
                }
                "eventfd" => {
                    libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK);
                }
                "timerfd" => {
                    libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_CLOEXEC);
                }
                "clone" => {
                    libc::syscall(libc::SYS_clone, 0, 0, 0, 0, 0);
                }
                "clone3" => {
                    libc::syscall(libc::SYS_clone3, std::ptr::null::<u8>(), 0);
                }
                "rt-sigaction" => {
                    libc::syscall(libc::SYS_rt_sigaction, libc::SIGRTMIN() + 1, 0, 0, 8);
                }
                "sigaltstack" => {
                    libc::sigaltstack(std::ptr::null(), std::ptr::null_mut());
                }
                "sigaltstack-query" => {
                    let new_stack = libc::stack_t {
                        ss_sp: std::ptr::null_mut(),
                        ss_flags: libc::SS_DISABLE,
                        ss_size: libc::SIGSTKSZ,
                    };
                    let mut old_stack: libc::stack_t = std::mem::zeroed();
                    libc::sigaltstack(&new_stack, &mut old_stack);
                }
                "tgkill" => {
                    libc::syscall(libc::SYS_tgkill, libc::getpid(), libc::gettid(), 0);
                }
                "mmap-exec" => {
                    libc::mmap(
                        std::ptr::null_mut(),
                        4096,
                        libc::PROT_READ | libc::PROT_EXEC,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        -1,
                        0,
                    );
                }
                "mmap-fd-high-alias" => {
                    libc::syscall(
                        libc::SYS_mmap,
                        std::ptr::null_mut::<u8>(),
                        4096,
                        libc::PROT_READ,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        (1_u64 << 32) | u32::MAX as u64,
                        0,
                    );
                }
                "mmap-vfio-cross-alias" => {
                    libc::syscall(
                        libc::SYS_mmap,
                        std::ptr::null_mut::<u8>(),
                        4096,
                        libc::PROT_READ,
                        libc::MAP_SHARED,
                        (u32::MAX as u64) << 32 | fds[1] as u32 as u64,
                        0,
                    );
                }
                "mmap-anonymous-offset" => {
                    libc::syscall(
                        libc::SYS_mmap,
                        std::ptr::null_mut::<u8>(),
                        4096,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        u32::MAX as u64,
                        4096,
                    );
                }
                "mmap-anonymous-read-only" => {
                    libc::mmap(
                        std::ptr::null_mut(),
                        4096,
                        libc::PROT_READ,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        -1,
                        0,
                    );
                }
                "realtime-sleep" => {
                    let timeout = libc::timespec {
                        tv_sec: 0,
                        tv_nsec: 1,
                    };
                    libc::syscall(
                        libc::SYS_clock_nanosleep,
                        libc::CLOCK_REALTIME,
                        0,
                        &timeout,
                        0,
                    );
                }
                "sleep-clock-high" => {
                    let timeout = libc::timespec {
                        tv_sec: 0,
                        tv_nsec: 1,
                    };
                    libc::syscall(
                        libc::SYS_clock_nanosleep,
                        (1_u64 << 32) | libc::CLOCK_MONOTONIC as u32 as u64,
                        0,
                        &timeout,
                        0,
                    );
                }
                "ppoll-two" => {
                    let mut pollfds = [libc::pollfd {
                        fd: fds[3],
                        events: libc::POLLIN,
                        revents: 0,
                    }; 2];
                    libc::ppoll(pollfds.as_mut_ptr(), 2, std::ptr::null(), std::ptr::null());
                }
                _ => unreachable!(),
            };
        }
    }
}
