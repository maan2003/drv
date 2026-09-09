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
    Wlancfg { persistence_dir_fd: RawFd },
    /// Simulated Wi-Fi IPC, memory, timers, and worker threads.
    WifiSimulated,
    /// One MT7921 PCI function, represented only by its three inert descriptors.
    Mt7921Vfio {
        pci_config_fd: RawFd,
        vfio_fd: RawFd,
        iommufd: RawFd,
    },
}

/// Review trace for the MT7921 profile. Request values are owned by
/// `userspace-vfio::mt7921_seccomp`; this records the corresponding names.
pub const MT7921_VFIO_AUTHORITY_INVENTORY: &str = "fds=stdio,pci-config-rw,vfio-cdev,iommufd-rw; vfio-ioctl=DEVICE_BIND_IOMMUFD,DEVICE_ATTACH_IOMMUFD_PT,DEVICE_GET_INFO,DEVICE_GET_REGION_INFO,DEVICE_GET_IRQ_INFO,DEVICE_SET_IRQS,DEVICE_RESET; iommufd-ioctl=IOAS_ALLOC,IOAS_MAP,IOAS_UNMAP,IOMMU_DESTROY; syscalls=read,write,close,fstat,poll,ppoll,mmap-noexec-anon-or-vfio,mprotect-noexec,munmap,madvise,brk,rt_sigaction,rt_sigprocmask,rt_sigreturn,sigaltstack,futex,set_robust_list,rseq,sched_yield,clock_gettime,clock_nanosleep,nanosleep,timerfd_create,timerfd_settime,timerfd_gettime,epoll_create1,epoll_ctl,epoll_pwait,epoll_wait-x86_64,eventfd2,fcntl,getrandom,getpid,gettid,tgkill-self-tgid,clone-thread-only,clone3-enosys,lseek-pci-only,exit,exit_group; denied=open,openat,socket,connect,exec,fork,process-clone,sendmsg,recvmsg,tgkill-other-tgid,ioctl-other,mmap-other,mmap-exec,mprotect-exec";

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
        if let Profile::Wlancfg { persistence_dir_fd } = profile
            && self.persistence_dir_fd != Some(persistence_dir_fd)
        {
            return Err(Error::PersistenceFdNotInherited(persistence_dir_fd));
        }
        if let Profile::Mt7921Vfio {
            pci_config_fd,
            vfio_fd,
            iommufd,
        } = profile
        {
            if self.persistence_dir_fd.is_some() {
                return Err(Error::ProfileAuthorityMismatch);
            }
            let mut expected = vec![pci_config_fd, vfio_fd, iommufd];
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
const KILL: u32 = 0x8000_0000;
const EPERM: u32 = 0x0005_0000 | libc::EPERM as u32;
const ENOSYS: u32 = 0x0005_0000 | libc::ENOSYS as u32;

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
        stmt(RET_K, KILL),
        stmt(LD_W_ABS, 0),
    ];
    if let Profile::Wlancfg {
        persistence_dir_fd: fd,
    } = profile
    {
        append_protected_close(&mut f, fd);
        append_openat(&mut f, fd);
        append_renameat(&mut f, fd);
        append_unlinkat(&mut f, fd);
    }
    if let Profile::Mt7921Vfio {
        pci_config_fd,
        vfio_fd,
        iommufd,
    } = profile
    {
        append_tgkill_self(&mut f, unsafe { libc::getpid() });
        append_mt7921_ioctl(&mut f, vfio_fd, iommufd);
        append_mt7921_mmap(&mut f, vfio_fd);
        append_no_exec_mprotect(&mut f);
        append_fd_only(&mut f, libc::SYS_lseek, pci_config_fd);
    }
    append_errno(&mut f, libc::SYS_clone3, ENOSYS);
    append_clone_thread_only(&mut f);
    for number in allowed(profile) {
        f.push(jump(number as u32, 0, 1));
        f.push(stmt(RET_K, ALLOW));
    }
    f.push(stmt(RET_K, EPERM));
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

fn append_tgkill_self(f: &mut Vec<Filter>, tgid: libc::pid_t) {
    append_fd_only(f, libc::SYS_tgkill, tgid);
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
    f.push(stmt(RET_K, EPERM));
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

fn append_mt7921_mmap(f: &mut Vec<Filter>, vfio_fd: RawFd) {
    f.push(jump(libc::SYS_mmap as u32, 0, 12));
    f.push(arg(2));
    f.push(Filter {
        code: 0x45,
        jt: 0,
        jf: 1,
        k: libc::PROT_EXEC as u32,
    });
    f.push(stmt(RET_K, EPERM));
    f.push(arg_high(4));
    f.push(jump(0, 2, 0));
    f.push(jump(u32::MAX, 1, 0));
    f.push(stmt(RET_K, EPERM));
    f.push(arg(4));
    f.push(jump(u32::MAX, 2, 0));
    f.push(jump(vfio_fd as u32, 1, 0));
    f.push(stmt(RET_K, EPERM));
    f.push(stmt(RET_K, ALLOW));
    f.push(stmt(LD_W_ABS, 0));
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
    f.push(stmt(RET_K, EPERM));
    f.push(stmt(RET_K, ALLOW));
    f.push(stmt(LD_W_ABS, 0));
}

fn append_fd_only(f: &mut Vec<Filter>, syscall: libc::c_long, fd: RawFd) {
    f.push(jump(syscall as u32, 0, 6));
    f.push(arg_high(0));
    f.push(jump(0, 0, 2));
    f.push(arg(0));
    f.push(jump(fd as u32, 1, 0));
    f.push(stmt(RET_K, EPERM));
    f.push(stmt(RET_K, ALLOW));
    f.push(stmt(LD_W_ABS, 0));
}

fn append_protected_close(f: &mut Vec<Filter>, fd: RawFd) {
    f.push(jump(libc::SYS_close as u32, 0, 5));
    f.push(arg(0));
    f.push(jump(fd as u32, 0, 1));
    f.push(stmt(RET_K, EPERM));
    f.push(stmt(RET_K, ALLOW));
    f.push(stmt(LD_W_ABS, 0));
}

fn append_errno(f: &mut Vec<Filter>, syscall: libc::c_long, errno: u32) {
    f.push(jump(syscall as u32, 0, 1));
    f.push(stmt(RET_K, errno));
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
    f.push(stmt(RET_K, EPERM));
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
        f.push(stmt(RET_K, EPERM));
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
        f.push(stmt(RET_K, EPERM));
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
    f.push(stmt(RET_K, EPERM));
    f.push(stmt(LD_W_ABS, 0));
}

fn append_clone_thread_only(f: &mut Vec<Filter>) {
    // clone is admitted only with CLONE_THREAD, never as fork/process creation.
    f.push(jump(libc::SYS_clone as u32, 0, 5));
    f.push(arg(0));
    f.push(Filter {
        code: 0x45,
        jt: 0,
        jf: 1,
        k: libc::CLONE_THREAD as u32,
    });
    f.push(stmt(RET_K, ALLOW));
    f.push(stmt(RET_K, EPERM));
    f.push(stmt(LD_W_ABS, 0));
}

fn allowed(profile: Profile) -> Vec<libc::c_long> {
    // Explicitly excludes open/openat, socket/connect, ioctl, exec and fork.
    let mut calls = vec![
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_close,
        libc::SYS_fstat,
        libc::SYS_ppoll,
        // MT7921's mmap/mprotect and lseek have argument filters above.
        libc::SYS_munmap,
        libc::SYS_madvise,
        libc::SYS_brk,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_futex,
        libc::SYS_set_robust_list,
        libc::SYS_rseq,
        libc::SYS_sched_yield,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_timerfd_create,
        libc::SYS_timerfd_settime,
        libc::SYS_timerfd_gettime,
        libc::SYS_epoll_create1,
        libc::SYS_epoll_ctl,
        libc::SYS_epoll_pwait,
        libc::SYS_eventfd2,
        libc::SYS_fcntl,
        libc::SYS_getrandom,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ];
    // aarch64 has no legacy poll syscall; libc implements poll through ppoll.
    #[cfg(target_arch = "x86_64")]
    calls.push(libc::SYS_poll);
    if !matches!(profile, Profile::Mt7921Vfio { .. }) {
        calls.extend([
            libc::SYS_recvmsg,
            libc::SYS_sendmsg,
            libc::SYS_mmap,
            libc::SYS_mprotect,
            libc::SYS_tgkill,
        ]);
    }
    #[cfg(target_arch = "x86_64")]
    calls.push(libc::SYS_epoll_wait);
    if matches!(profile, Profile::Wlancfg { .. }) {
        calls.extend([
            libc::SYS_lseek,
            libc::SYS_pread64,
            libc::SYS_pwrite64,
            libc::SYS_fsync,
            libc::SYS_fdatasync,
            libc::SYS_ftruncate,
        ]);
    }
    calls
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn mt7921_profile_rejects_persistence_authority() {
        let setup = Sandbox::<SetupComplete> {
            inherited: vec![3, 4, 5],
            persistence_dir_fd: Some(3),
            _state: PhantomData,
        };
        assert!(matches!(
            setup.lockdown(Profile::Mt7921Vfio {
                pci_config_fd: 3,
                vfio_fd: 4,
                iommufd: 5,
            }),
            Err(Error::ProfileAuthorityMismatch)
        ));
    }

    #[test]
    fn wifi_filter_denials_and_thread_fallback_execute() {
        subprocess(
            "wifi",
            "filter_tests::wifi_filter_denials_and_thread_fallback_execute",
            || {
                enable_filter(Profile::WifiSimulated);
                assert_errno(
                    unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe { libc::open(c"/etc/passwd".as_ptr(), libc::O_RDONLY) } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe { libc::ioctl(0, 0x5413, std::ptr::null_mut::<libc::c_void>()) } as i64,
                    libc::EPERM,
                );
                std::thread::spawn(|| {}).join().unwrap();
            },
        );
    }

    #[test]
    fn wlancfg_filter_checks_every_persistence_argument() {
        subprocess(
            "wlancfg",
            "filter_tests::wlancfg_filter_checks_every_persistence_argument",
            || {
                let fd =
                    unsafe { libc::open(c"/tmp".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
                assert!(fd >= 0);
                let temporary =
                    std::ffi::CString::new(format!("drv-filter-{}.tmp", std::process::id()))
                        .unwrap();
                let installed =
                    std::ffi::CString::new(format!("drv-filter-{}.bin", std::process::id()))
                        .unwrap();
                enable_filter(Profile::Wlancfg {
                    persistence_dir_fd: fd,
                });
                let read_flags =
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
                let create_flags = libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK;
                assert_errno(
                    unsafe { libc::openat(libc::AT_FDCWD, temporary.as_ptr(), read_flags) } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe { libc::openat(fd, temporary.as_ptr(), libc::O_RDONLY) } as i64,
                    libc::EPERM,
                );
                let file = unsafe { libc::openat(fd, temporary.as_ptr(), create_flags, 0o600) };
                assert!(file >= 0);
                assert_eq!(unsafe { libc::write(file, b"x".as_ptr().cast(), 1) }, 1);
                assert_eq!(unsafe { libc::fsync(file) }, 0);
                assert_eq!(unsafe { libc::close(file) }, 0);
                assert_errno(
                    unsafe {
                        libc::renameat(fd, temporary.as_ptr(), libc::AT_FDCWD, installed.as_ptr())
                    } as i64,
                    libc::EPERM,
                );
                assert_eq!(
                    unsafe { libc::renameat(fd, temporary.as_ptr(), fd, installed.as_ptr()) },
                    0
                );
                assert_eq!(unsafe { libc::fsync(fd) }, 0);
                assert_errno(
                    unsafe { libc::unlinkat(fd, installed.as_ptr(), libc::AT_REMOVEDIR) } as i64,
                    libc::EPERM,
                );
                assert_errno(unsafe { libc::close(fd) } as i64, libc::EPERM);
                assert_eq!(unsafe { libc::unlinkat(fd, installed.as_ptr(), 0) }, 0);
            },
        );
    }

    #[test]
    fn mt7921_filter_is_exact_by_fd_request_and_mapping_protection() {
        subprocess(
            "mt7921",
            "filter_tests::mt7921_filter_is_exact_by_fd_request_and_mapping_protection",
            || {
                let vfio_get_info = userspace_vfio::mt7921_seccomp::VFIO_REQUESTS[2];
                let ioas_alloc = userspace_vfio::mt7921_seccomp::IOMMUFD_REQUESTS[0];
                let pci = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
                let vfio = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
                let iommu = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
                assert!(pci >= 0 && vfio >= 0 && iommu >= 0);
                enable_filter(Profile::Mt7921Vfio {
                    pci_config_fd: pci,
                    vfio_fd: vfio,
                    iommufd: iommu,
                });
                // Allowed pairs reach /dev/null and therefore fail with ENOTTY,
                // proving seccomp did not replace the result with EPERM.
                assert_errno(
                    unsafe { libc::ioctl(vfio, vfio_get_info, 0) } as i64,
                    libc::ENOTTY,
                );
                assert_errno(
                    unsafe { libc::ioctl(iommu, ioas_alloc, 0) } as i64,
                    libc::ENOTTY,
                );
                assert_errno(
                    unsafe { libc::ioctl(vfio, ioas_alloc, 0) } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe { libc::ioctl(iommu, vfio_get_info, 0) } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe { libc::ioctl(pci, vfio_get_info, 0) } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe { libc::ioctl(vfio, 0xffff_u64, 0) } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe { libc::ioctl(vfio, vfio_get_info | (1_u64 << 32), 0) } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe {
                        libc::mmap(
                            std::ptr::null_mut(),
                            4096,
                            libc::PROT_READ | libc::PROT_EXEC,
                            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                            -1,
                            0,
                        )
                    } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe {
                        libc::mmap(
                            std::ptr::null_mut(),
                            4096,
                            libc::PROT_READ,
                            libc::MAP_PRIVATE,
                            pci,
                            0,
                        )
                    } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) } as i64,
                    libc::EPERM,
                );
                let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
                assert_errno(
                    unsafe { libc::recvmsg(vfio, &mut message, libc::MSG_DONTWAIT) } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe { libc::sendmsg(vfio, &message, libc::MSG_DONTWAIT) } as i64,
                    libc::EPERM,
                );
                assert_errno(
                    unsafe { libc::open(c"/etc/passwd".as_ptr(), libc::O_RDONLY) } as i64,
                    libc::EPERM,
                );
                assert_errno(unsafe { libc::fork() } as i64, libc::EPERM);
                let argv = [c"/bin/true".as_ptr(), std::ptr::null()];
                assert_errno(
                    unsafe {
                        libc::execve(c"/bin/true".as_ptr(), argv.as_ptr(), argv[1..].as_ptr())
                    } as i64,
                    libc::EPERM,
                );
                let mut now: libc::timespec = unsafe { std::mem::zeroed() };
                assert_eq!(
                    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) },
                    0
                );
                std::thread::spawn(|| {}).join().unwrap();
                let event = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
                assert!(event >= 0);
                assert_eq!(unsafe { libc::close(event) }, 0);
            },
        );
    }

    fn enable_filter(profile: Profile) {
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
            0
        );
        install_filter(profile).unwrap();
    }

    fn assert_errno(result: i64, errno: i32) {
        assert_eq!(result, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(errno));
    }

    fn subprocess(name: &str, test_name: &str, body: impl FnOnce()) {
        let variable = format!("DRV_SANDBOX_FILTER_CHILD_{name}");
        if std::env::var_os(&variable).is_some() {
            body();
            unsafe { libc::_exit(0) };
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(test_name)
            .env(variable, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "filter child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
