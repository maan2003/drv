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
#[derive(Clone, Debug)]
pub enum Profile {
    /// WLAN policy IPC and the sole-writer saved-network directory capability.
    Wlancfg {
        control_fd: RawFd,
        persistence_dir_fd: RawFd,
        /// Pre-bound application listener. `None` is retained only by legacy
        /// filter tests; the production service always supplies it.
        application_listener_fd: Option<RawFd>,
    },
    /// Simulated Wi-Fi IPC and single-threaded runtime mechanics.
    WifiSimulated,
    /// One MT7921 PCI function, its precreated IRQ eventfd, and optionally
    /// the same-process protocol runtime's exact descriptor inventory.
    Mt7921Vfio {
        pci_config_fd: RawFd,
        vfio_fd: RawFd,
        iommufd: RawFd,
        irq_eventfd: RawFd,
        service: Option<WifiServiceFds>,
    },
    /// One WCN6750 VFIO-platform device, its QRTR control plane, and the two
    /// process IPC seams. All interrupt eventfds must be created before setup.
    Ath11kWcn6750 {
        control_fd: RawFd,
        supervisor_fd: RawFd,
        vfio_fd: RawFd,
        dma: Wcn6750Dma,
        qrtr_fd: RawFd,
        irq_eventfds: [RawFd; WCN6750_IRQ_EVENTFD_COUNT],
        ethernet_fds: Vec<RawFd>,
        runtime_fds: Vec<RawFd>,
        remoteproc_state_fd: Option<RawFd>,
    },
}

/// Precreated IPC, Ethernet and reactor descriptors for the same-process
/// protocol runtime. Setup checks the complete inventory before lockdown.
#[derive(Clone, Debug)]
pub struct WifiServiceFds {
    pub control_fd: RawFd,
    pub supervisor_fd: RawFd,
    pub regulatory_fd: Option<RawFd>,
    pub ethernet_fds: Vec<RawFd>,
    pub runtime_fds: Vec<RawFd>,
}

pub const WCN6750_IRQ_EVENTFD_COUNT: usize = 16;

#[derive(Clone, Copy, Debug)]
pub enum Wcn6750Dma {
    Coherent { iommufd: RawFd },
    Broker,
}

pub const ATH11K_WCN6750_AUTHORITY_INVENTORY: &str = "fds=stdio,policy-seqpacket,network-lifecycle-seqpacket,vfio-platform-cdev,qrtr,16-selected-irq-eventfds,ethernet-socketpair,precreated-runtime-reactor,optional-iommufd,remoteproc-state; vfio-ioctl=DEVICE_BIND_IOMMUFD(coherent),DEVICE_ATTACH_IOMMUFD_PT(coherent),DEVICE_GET_INFO,DEVICE_GET_REGION_INFO,DEVICE_GET_IRQ_INFO,DEVICE_SET_IRQS,DEVICE_RESET,DEVICE_FEATURE(broker); iommufd-ioctl=IOAS_ALLOC,IOAS_MAP,IOAS_UNMAP,IOMMU_DESTROY(coherent); qrtr=bind,connect,getsockname,getpeername,sendto,recvfrom,ppoll; ipc=bounded-policy-sendmsg-recvmsg,supervisor-sendmsg,ethernet-sendto-recvfrom; runtime=fd-bound-epoll-and-wake; memory=mmap-rw-private-anon-or-shared-vfio,noexec; denied=fd-creators,open,socket,dup,exec,clone,other-ioctl,other-fd-io,executable-memory";

/// Review trace for the MT7921 profile. Request values are owned by
/// `userspace-vfio::mt7921_seccomp`; this records the corresponding names.
pub const MT7921_VFIO_AUTHORITY_INVENTORY: &str = "fds=stdio,pci-config-rw,vfio-cdev,iommufd-rw,irq-eventfd; optional-service=fd-bound-policy-recvmsg-sendmsg,supervisor-sendmsg,ethernet-sendto-recvfrom,precreated-reactor-epoll-read-write,regulatory-database-read,readiness-fd-F_GETFD; vfio-ioctl=DEVICE_BIND_IOMMUFD,DEVICE_ATTACH_IOMMUFD_PT,DEVICE_GET_INFO,DEVICE_GET_REGION_INFO,DEVICE_GET_IRQ_INFO,DEVICE_SET_IRQS,DEVICE_RESET; iommufd-ioctl=IOAS_ALLOC,IOAS_MAP,IOAS_UNMAP,IOMMU_DESTROY; syscalls=read-pci-or-irq,write-pci-or-stdout-stderr,close,ppoll-max-one,mmap-rw-private-anon-offset-zero-or-shared-vfio,mprotect-noexec,munmap,madvise,brk,futex,sched_yield,clock_gettime-monotonic,clock_nanosleep,nanosleep,getrandom,getpid,gettid,sigaltstack-new-only,lseek-pci-only,prctl-GET_AUXV-max512-reserved-zero,exit,exit_group; denied=fcntl-except-owned-fd-F_GETFD,dup,fd-creators,open,socket,exec,clone,clone3,signal-handler-or-mask-management,signal-send,sendmsg-without-service,recvmsg-without-service,recvmmsg,ioctl-other,mmap-other,mmap-exec,mprotect-exec";

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
        if let Some(fd) = persistence_dir_fd {
            // Make the capability itself the filesystem root. This preserves
            // kernel-enforced `..` and symlink confinement without requiring
            // bind-mount support from a minimal kernel.
            syscall_ok(
                unsafe { libc::fchdir(fd) },
                "enter persistence directory capability",
            )?;
            syscall_ok(
                unsafe { libc::chroot(c".".as_ptr()) },
                "chroot persistence directory capability",
            )?;
            syscall_ok(unsafe { libc::chdir(c"/".as_ptr()) }, "enter jailed root")?;
            reopen_state_capability(fd)?;
        } else {
            syscall_ok(
                unsafe {
                    libc::mount(
                        c"tmpfs".as_ptr(),
                        c"/tmp".as_ptr(),
                        c"tmpfs".as_ptr(),
                        (libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC) as c_ulong,
                        c"size=1048576,mode=0755".as_ptr().cast(),
                    )
                },
                "mount empty root",
            )?;
            syscall_ok(
                unsafe { libc::chmod(c"/tmp".as_ptr(), 0o555) },
                "seal empty root",
            )?;
            syscall_ok(unsafe { libc::chdir(c"/tmp".as_ptr()) }, "enter empty root")?;
            syscall_ok(unsafe { libc::chroot(c".".as_ptr()) }, "chroot empty root")?;
            syscall_ok(unsafe { libc::chdir(c"/".as_ptr()) }, "enter jailed root")?;
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
        if matches!(&profile, Profile::WifiSimulated) && self.persistence_dir_fd.is_some() {
            return Err(Error::ProfileAuthorityMismatch);
        }
        if let Profile::Wlancfg {
            control_fd,
            persistence_dir_fd,
            application_listener_fd,
        } = &profile
        {
            if self.persistence_dir_fd != Some(*persistence_dir_fd) {
                return Err(Error::PersistenceFdNotInherited(*persistence_dir_fd));
            }
            let mut expected = vec![*control_fd, *persistence_dir_fd];
            expected.extend(application_listener_fd);
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
            service,
        } = &profile
        {
            if self.persistence_dir_fd.is_some() {
                return Err(Error::ProfileAuthorityMismatch);
            }
            let mut expected = vec![*pci_config_fd, *vfio_fd, *iommufd, *irq_eventfd];
            if let Some(service) = service {
                expected.extend([service.control_fd, service.supervisor_fd]);
                expected.extend(service.regulatory_fd);
                expected.extend(&service.ethernet_fds);
                expected.extend(&service.runtime_fds);
            }
            expected.sort_unstable();
            if expected != self.inherited {
                return Err(Error::ProfileAuthorityMismatch);
            }
        }
        if let Profile::Ath11kWcn6750 {
            control_fd,
            supervisor_fd,
            vfio_fd,
            dma,
            qrtr_fd,
            irq_eventfds,
            ethernet_fds,
            runtime_fds,
            remoteproc_state_fd,
        } = &profile
        {
            if self.persistence_dir_fd.is_some() {
                return Err(Error::ProfileAuthorityMismatch);
            }
            let mut expected = vec![*control_fd, *supervisor_fd, *vfio_fd, *qrtr_fd];
            expected.extend(irq_eventfds);
            expected.extend(ethernet_fds);
            expected.extend(runtime_fds);
            if let Wcn6750Dma::Coherent { iommufd } = dma {
                expected.push(*iommufd);
            }
            expected.extend(remoteproc_state_fd.iter().copied());
            expected.sort_unstable();
            if expected != self.inherited {
                return Err(Error::ProfileAuthorityMismatch);
            }
        }
        install_filter(&profile)?;
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

/// Snapshot live descriptors around inert runtime preparation. The caller must
/// keep unrelated descriptor creation outside the interval and retain the
/// resulting inventory for exact sandbox setup.
pub fn open_fd_snapshot() -> Result<BTreeSet<RawFd>, io::Error> {
    let entries = std::fs::read_dir("/proc/self/fd")?
        .map(|entry| {
            entry?
                .file_name()
                .to_string_lossy()
                .parse::<std::os::fd::RawFd>()
                .map_err(std::io::Error::other)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // The directory stream's own descriptor is closed when read_dir is
    // dropped. Exclude that now-stale number from the retained inventory.
    Ok(entries
        .into_iter()
        .filter(|fd| std::fs::read_link(format!("/proc/self/fd/{fd}")).is_ok())
        .collect())
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

fn reopen_state_capability(inherited_fd: RawFd) -> Result<(), Error> {
    let reopened = unsafe {
        libc::open(
            c"/".as_ptr(),
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

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule,
};
use std::collections::BTreeMap;

type Rules = BTreeMap<i64, Vec<SeccompRule>>;

fn condition(index: u8, width: SeccompCmpArgLen, op: SeccompCmpOp, value: u64) -> SeccompCondition {
    SeccompCondition::new(index, width, op, value).expect("valid static argument index")
}
fn eq(index: u8, value: u64) -> SeccompCondition {
    condition(index, SeccompCmpArgLen::Qword, SeccompCmpOp::Eq, value)
}
fn low_eq(index: u8, value: u32) -> SeccompCondition {
    condition(
        index,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::Eq,
        value.into(),
    )
}
fn allow(rules: &mut Rules, syscall: libc::c_long, conditions: Vec<SeccompCondition>) {
    assert!(
        !conditions.is_empty(),
        "conditional rules must not become unconditional"
    );
    rules
        .entry(syscall)
        .or_default()
        .push(SeccompRule::new(conditions).expect("nonempty rule"));
}
fn allow_fds(rules: &mut Rules, syscall: libc::c_long, fds: &[RawFd]) {
    for &fd in fds {
        allow(rules, syscall, vec![eq(0, fd as u64)]);
    }
}
fn allow_ioctl(rules: &mut Rules, fd: RawFd, requests: &[u64]) {
    for &request in requests {
        allow(
            rules,
            libc::SYS_ioctl,
            vec![eq(0, fd as u64), eq(1, request)],
        );
    }
}
fn allow_service_messages(rules: &mut Rules, control: RawFd, supervisor: RawFd) {
    for fd in [control, supervisor] {
        allow(
            rules,
            libc::SYS_sendmsg,
            vec![
                eq(0, fd as u64),
                eq(2, (libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) as u64),
            ],
        );
    }
    allow(
        rules,
        libc::SYS_recvmsg,
        vec![
            eq(0, control as u64),
            eq(2, (libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC) as u64),
        ],
    );
}
fn allow_frames(rules: &mut Rules, fds: &[RawFd]) {
    for &fd in fds {
        for (syscall, flags) in [
            (libc::SYS_sendto, libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL),
            (libc::SYS_recvfrom, libc::MSG_DONTWAIT | libc::MSG_TRUNC),
        ] {
            allow(rules, syscall, vec![eq(0, fd as u64), eq(3, flags as u64)]);
        }
    }
}
fn allow_runtime_wait(rules: &mut Rules, fds: &[RawFd]) {
    #[cfg(target_arch = "x86_64")]
    allow_fds(rules, libc::SYS_epoll_wait, fds);
    for &fd in fds {
        for size in [0, 8] {
            allow(
                rules,
                libc::SYS_epoll_pwait,
                vec![eq(0, fd as u64), eq(4, 0), eq(5, size)],
            );
        }
    }
}
fn compile_filter(profile: &Profile) -> Result<BpfProgram, Error> {
    use SeccompCmpArgLen::{Dword, Qword};
    use SeccompCmpOp::{Ge, Le, MaskedEq, Ne};
    let mut rules = Rules::new();

    // Preserve the existing syscall ABI widths. Qword comparisons constrain
    // upper words too; Dword comparisons intentionally retain low-word checks.
    allow(
        &mut rules,
        libc::SYS_mprotect,
        vec![condition(2, Dword, MaskedEq(libc::PROT_EXEC as u64), 0)],
    );
    allow(
        &mut rules,
        libc::SYS_clock_gettime,
        vec![eq(0, libc::CLOCK_MONOTONIC as u64)],
    );
    allow(
        &mut rules,
        libc::SYS_clock_nanosleep,
        vec![eq(0, libc::CLOCK_MONOTONIC as u64), low_eq(1, 0)],
    );
    for advice in [
        libc::MADV_DONTNEED,
        libc::MADV_DONTDUMP,
        libc::MADV_FREE,
        libc::MADV_NOHUGEPAGE,
    ] {
        allow(
            &mut rules,
            libc::SYS_madvise,
            vec![low_eq(2, advice as u32)],
        );
    }
    for flags in [0, libc::GRND_NONBLOCK] {
        allow(&mut rules, libc::SYS_getrandom, vec![eq(2, flags as u64)]);
    }
    // Seccomp cannot inspect SS_DISABLE, but excludes querying the old stack.
    allow(
        &mut rules,
        libc::SYS_sigaltstack,
        vec![condition(0, Qword, Ne, 0), eq(1, 0)],
    );
    // libc supplies mmap's 32-bit -1 with an otherwise zero upper word.
    allow(
        &mut rules,
        libc::SYS_mmap,
        vec![
            eq(2, (libc::PROT_READ | libc::PROT_WRITE) as u64),
            eq(3, (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64),
            eq(4, u32::MAX as u64),
            eq(5, 0),
        ],
    );

    match profile {
        Profile::Wlancfg {
            control_fd,
            persistence_dir_fd,
            application_listener_fd,
        } => {
            // OwnedFd debug-drop checks include dynamically accepted clients
            // and directory-relative persistence files. This query cannot
            // create, duplicate or modify a descriptor.
            allow(
                &mut rules,
                libc::SYS_fcntl,
                vec![eq(1, libc::F_GETFD as u64)],
            );
            let fd = *persistence_dir_fd as u32;
            // The sealed filesystem, not seccomp, confines pointed-to names.
            allow(
                &mut rules,
                libc::SYS_statx,
                vec![eq(2, libc::AT_EMPTY_PATH as u64)],
            );
            for flags in [
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
            ] {
                allow(
                    &mut rules,
                    libc::SYS_openat,
                    vec![low_eq(0, fd), low_eq(2, flags as u32)],
                );
            }
            #[cfg(target_arch = "x86_64")]
            let renameat = libc::SYS_renameat;
            // The aarch64 libc constant is omitted, but Linux's ABI is syscall 38.
            #[cfg(target_arch = "aarch64")]
            let renameat = 38;
            allow(&mut rules, renameat, vec![low_eq(0, fd), low_eq(2, fd)]);
            allow(
                &mut rules,
                libc::SYS_unlinkat,
                vec![low_eq(0, fd), low_eq(2, 0)],
            );
            for (syscall, flags) in [
                (libc::SYS_sendto, libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL),
                (libc::SYS_recvfrom, libc::MSG_DONTWAIT | libc::MSG_TRUNC),
            ] {
                allow(
                    &mut rules,
                    syscall,
                    vec![
                        eq(0, *control_fd as u64),
                        condition(2, Qword, Le, 8192),
                        eq(3, flags as u64),
                        eq(4, 0),
                        eq(5, 0),
                    ],
                );
            }
            if let Some(listener) = application_listener_fd {
                allow(
                    &mut rules,
                    libc::SYS_accept4,
                    vec![
                        low_eq(0, *listener as u32),
                        eq(1, 0),
                        low_eq(2, 0),
                        low_eq(3, (libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK) as u32),
                    ],
                );
            }
            #[cfg(target_arch = "x86_64")]
            allow(&mut rules, libc::SYS_poll, vec![eq(1, 2)]);
            #[cfg(target_arch = "aarch64")]
            allow(
                &mut rules,
                libc::SYS_ppoll,
                vec![eq(1, 2), eq(2, 0), eq(3, 0)],
            );
            allow(&mut rules, libc::SYS_epoll_pwait, vec![eq(4, 0)]);
            allow(
                &mut rules,
                libc::SYS_epoll_ctl,
                vec![condition(1, Qword, Ge, 1), condition(1, Qword, Le, 3)],
            );
            allow(
                &mut rules,
                libc::SYS_rt_sigprocmask,
                vec![
                    eq(0, libc::SIG_BLOCK as u64),
                    condition(1, Qword, Ne, 0),
                    eq(2, 0),
                    eq(3, 8),
                ],
            );
        }
        Profile::WifiSimulated => {}
        Profile::Mt7921Vfio {
            pci_config_fd,
            vfio_fd,
            iommufd,
            irq_eventfd,
            service,
        } => {
            // Rust's lazy CPU/runtime discovery reads the process's own
            // startup auxiliary vector. All mutating prctl operations remain
            // forbidden; the kernel bounds writes by the caller's buffer.
            allow(
                &mut rules,
                libc::SYS_prctl,
                vec![
                    eq(0, 0x41555856),
                    condition(2, Qword, Le, 512),
                    eq(3, 0),
                    eq(4, 0),
                ],
            );
            allow_ioctl(
                &mut rules,
                *vfio_fd,
                userspace_vfio::mt7921_seccomp::VFIO_REQUESTS,
            );
            allow_ioctl(
                &mut rules,
                *iommufd,
                userspace_vfio::mt7921_seccomp::IOMMUFD_REQUESTS,
            );
            for fd in [*pci_config_fd, *vfio_fd, *iommufd, *irq_eventfd] {
                allow(
                    &mut rules,
                    libc::SYS_fcntl,
                    vec![eq(0, fd as u64), eq(1, libc::F_GETFD as u64)],
                );
            }
            allow_fds(&mut rules, libc::SYS_lseek, &[*pci_config_fd]);
            allow(
                &mut rules,
                libc::SYS_ppoll,
                vec![condition(1, Qword, Le, 1), eq(3, 0)],
            );
            let mut readable = vec![*pci_config_fd, *irq_eventfd];
            let mut writable = vec![*pci_config_fd, 1, 2];
            if let Some(service) = service {
                allow_service_messages(&mut rules, service.control_fd, service.supervisor_fd);
                allow_frames(&mut rules, &service.ethernet_fds);
                allow_runtime_wait(&mut rules, &service.runtime_fds);
                let mut readiness = vec![*irq_eventfd, service.control_fd, service.supervisor_fd];
                readiness.extend(&service.ethernet_fds);
                readiness.extend(&service.runtime_fds);
                for &target in &readiness {
                    // Read-only OwnedFd debug-drop validity check, not duplication.
                    allow(
                        &mut rules,
                        libc::SYS_fcntl,
                        vec![eq(0, target as u64), eq(1, libc::F_GETFD as u64)],
                    );
                    for &reactor in &service.runtime_fds {
                        allow(
                            &mut rules,
                            libc::SYS_epoll_ctl,
                            vec![
                                eq(0, reactor as u64),
                                eq(2, target as u64),
                                condition(1, Qword, Ge, 1),
                                condition(1, Qword, Le, 3),
                            ],
                        );
                    }
                }
                readable.extend(service.regulatory_fd);
                if let Some(fd) = service.regulatory_fd {
                    allow(
                        &mut rules,
                        libc::SYS_fcntl,
                        vec![eq(0, fd as u64), eq(1, libc::F_GETFD as u64)],
                    );
                }
                readable.extend(&service.runtime_fds);
                writable.extend(&service.runtime_fds);
            }
            allow_fds(&mut rules, libc::SYS_read, &readable);
            allow_fds(&mut rules, libc::SYS_write, &writable);
        }
        Profile::Ath11kWcn6750 {
            control_fd,
            supervisor_fd,
            vfio_fd,
            dma,
            qrtr_fd,
            irq_eventfds,
            ethernet_fds,
            runtime_fds,
            remoteproc_state_fd,
        } => {
            match dma {
                Wcn6750Dma::Coherent { iommufd } => {
                    allow_ioctl(
                        &mut rules,
                        *vfio_fd,
                        userspace_vfio::wcn6750_seccomp::COHERENT_VFIO_REQUESTS,
                    );
                    allow_ioctl(
                        &mut rules,
                        *iommufd,
                        userspace_vfio::wcn6750_seccomp::COHERENT_IOMMUFD_REQUESTS,
                    );
                }
                Wcn6750Dma::Broker => allow_ioctl(
                    &mut rules,
                    *vfio_fd,
                    userspace_vfio::wcn6750_seccomp::BROKER_VFIO_REQUESTS,
                ),
            }
            allow_service_messages(&mut rules, *control_fd, *supervisor_fd);
            for syscall in [
                libc::SYS_bind,
                libc::SYS_connect,
                libc::SYS_getsockname,
                libc::SYS_getpeername,
            ] {
                allow_fds(&mut rules, syscall, &[*qrtr_fd]);
            }
            allow_frames(&mut rules, ethernet_fds);
            for flags in [0, libc::MSG_DONTWAIT] {
                allow(
                    &mut rules,
                    libc::SYS_sendto,
                    vec![eq(0, *qrtr_fd as u64), eq(3, flags as u64)],
                );
            }
            allow(
                &mut rules,
                libc::SYS_recvfrom,
                vec![
                    eq(0, *qrtr_fd as u64),
                    eq(3, (libc::MSG_TRUNC | libc::MSG_DONTWAIT) as u64),
                ],
            );
            allow_runtime_wait(&mut rules, runtime_fds);
            let mut readable = irq_eventfds.to_vec();
            readable.extend(runtime_fds);
            let mut writable = vec![1, 2];
            writable.extend(runtime_fds);
            if let Some(fd) = remoteproc_state_fd {
                allow_fds(&mut rules, libc::SYS_lseek, &[*fd]);
                readable.push(*fd);
                writable.push(*fd);
            }
            allow_fds(&mut rules, libc::SYS_read, &readable);
            allow_fds(&mut rules, libc::SYS_write, &writable);
            allow(
                &mut rules,
                libc::SYS_ppoll,
                vec![
                    condition(1, Qword, Le, (WCN6750_IRQ_EVENTFD_COUNT + 1) as u64),
                    eq(3, 0),
                ],
            );
        }
    }
    if let Profile::Mt7921Vfio { vfio_fd, .. } | Profile::Ath11kWcn6750 { vfio_fd, .. } = profile {
        allow(
            &mut rules,
            libc::SYS_mmap,
            vec![
                eq(2, (libc::PROT_READ | libc::PROT_WRITE) as u64),
                eq(3, libc::MAP_SHARED as u64),
                eq(4, *vfio_fd as u64),
            ],
        );
    }
    for number in allowed(profile) {
        // An empty rule list means unconditional ALLOW, not deny.
        assert!(
            rules.insert(number, Vec::new()).is_none(),
            "unconditional syscall overlaps constrained rule"
        );
    }
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::KillProcess,
        SeccompAction::Allow,
        std::env::consts::ARCH
            .try_into()
            .expect("supported target architecture"),
    )
    .map_err(|error| system("construct seccomp policy", io::Error::other(error)))?;
    filter
        .try_into()
        .map_err(|error| system("compile seccomp policy", io::Error::other(error)))
}

fn install_filter(profile: &Profile) -> Result<(), Error> {
    seccompiler::apply_filter_all_threads(&compile_filter(profile)?)
        .map_err(|error| system("install synchronized seccomp", io::Error::other(error)))
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
    install_filter(&profile)
}

fn allowed(profile: &Profile) -> Vec<libc::c_long> {
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
        Profile::Mt7921Vfio { .. } | Profile::Ath11kWcn6750 { .. } => {}
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
    fn compiled_profiles_retain_instruction_headroom() {
        // Current maximum: four Ethernet generations and the three Tokio
        // reactor descriptors observed on Linux. Dynamic inventories remain
        // exact sets; oversized future profiles must fail compilation closed.
        let profiles = [
            Profile::WifiSimulated,
            Profile::Wlancfg {
                control_fd: 3,
                persistence_dir_fd: 4,
                application_listener_fd: Some(5),
            },
            Profile::Mt7921Vfio {
                pci_config_fd: 3,
                vfio_fd: 4,
                iommufd: 5,
                irq_eventfd: 6,
                service: None,
            },
            Profile::Mt7921Vfio {
                pci_config_fd: 3,
                vfio_fd: 4,
                iommufd: 5,
                irq_eventfd: 6,
                service: Some(WifiServiceFds {
                    regulatory_fd: None,
                    control_fd: 7,
                    supervisor_fd: 8,
                    ethernet_fds: (9..17).collect(),
                    runtime_fds: vec![17, 18, 19],
                }),
            },
            Profile::Ath11kWcn6750 {
                control_fd: 3,
                supervisor_fd: 4,
                vfio_fd: 5,
                dma: Wcn6750Dma::Coherent { iommufd: 6 },
                qrtr_fd: 7,
                irq_eventfds: std::array::from_fn(|n| n as RawFd + 8),
                ethernet_fds: (24..32).collect(),
                runtime_fds: vec![32, 33, 34],
                remoteproc_state_fd: Some(35),
            },
            Profile::Ath11kWcn6750 {
                control_fd: 3,
                supervisor_fd: 4,
                vfio_fd: 5,
                dma: Wcn6750Dma::Broker,
                qrtr_fd: 7,
                irq_eventfds: std::array::from_fn(|n| n as RawFd + 8),
                ethernet_fds: (24..32).collect(),
                runtime_fds: vec![32, 33, 34],
                remoteproc_state_fd: None,
            },
        ];
        for (index, profile) in profiles.iter().enumerate() {
            let program = compile_filter(profile).unwrap();
            println!("profile={index} instructions={}", program.len());
            assert!(
                program.len() < 3072,
                "profile {index} needs instruction-budget review: {}",
                program.len()
            );
        }
    }

    #[test]
    fn mt7921_service_io_is_fd_scoped() {
        const TEST: &str = "filter_tests::mt7921_service_io_is_fd_scoped";
        if let Ok(probe) = std::env::var("DRV_MT_SERVICE_IO_PROBE") {
            let hardware = resources();
            let policy = resources();
            let supervisor = resources();
            let ethernet = resources();
            let epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
            assert!(epoll >= 0);
            let byte = [1u8];
            assert_eq!(
                unsafe { libc::send(policy[1], byte.as_ptr().cast(), 1, 0) },
                1
            );
            enable_filter(Profile::Mt7921Vfio {
                pci_config_fd: hardware[0],
                vfio_fd: hardware[1],
                iommufd: hardware[2],
                irq_eventfd: hardware[3],
                service: Some(WifiServiceFds {
                    regulatory_fd: None,
                    control_fd: policy[0],
                    supervisor_fd: supervisor[0],
                    ethernet_fds: vec![ethernet[0], ethernet[1]],
                    runtime_fds: vec![epoll, policy[3]],
                }),
            });
            let mut payload = [0u8];
            let mut iov = libc::iovec {
                iov_base: payload.as_mut_ptr().cast(),
                iov_len: 1,
            };
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            let receive_fd = if probe == "wrong-fd" {
                supervisor[0]
            } else {
                policy[0]
            };
            assert_eq!(
                unsafe {
                    libc::recvmsg(
                        receive_fd,
                        &mut message,
                        libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
                    )
                },
                1
            );
            for fd in [policy[0], supervisor[0]] {
                assert_eq!(
                    unsafe { libc::sendmsg(fd, &message, libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) },
                    1
                );
            }
            let flags = if probe == "bad-flags" {
                0
            } else {
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL
            };
            assert_eq!(
                unsafe {
                    libc::sendto(
                        ethernet[0],
                        byte.as_ptr().cast(),
                        1,
                        flags,
                        std::ptr::null(),
                        0,
                    )
                },
                1
            );
            assert_eq!(
                unsafe {
                    libc::recvfrom(
                        ethernet[1],
                        payload.as_mut_ptr().cast(),
                        1,
                        libc::MSG_DONTWAIT | libc::MSG_TRUNC,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                1
            );
            let mut event: libc::epoll_event = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::epoll_wait(epoll, &mut event, 1, 0) }, 0);
            let mut count = 0u64;
            assert_eq!(
                unsafe { libc::read(policy[3], (&mut count as *mut u64).cast(), 8) },
                8
            );
            assert_eq!(
                unsafe { libc::write(policy[3], (&count as *const u64).cast(), 8) },
                8
            );
            assert_eq!(
                unsafe { libc::read(hardware[3], (&mut count as *mut u64).cast(), 8) },
                8
            );
            unsafe { libc::_exit(0) }
        }
        for probe in ["positive", "wrong-fd", "bad-flags"] {
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST])
                .env("DRV_MT_SERVICE_IO_PROBE", probe)
                .status()
                .unwrap();
            if probe == "positive" {
                assert!(status.success(), "{probe}: {status}");
            } else {
                assert_eq!(status.signal(), Some(libc::SIGSYS), "{probe}: {status}");
            }
        }
    }

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
                service: None,
            }),
            Err(Error::ProfileAuthorityMismatch)
        ));
    }

    #[test]
    fn wcn6750_profile_requires_its_exact_non_aliasing_capabilities() {
        let irq_eventfds = std::array::from_fn(|index| 8 + index as RawFd);
        let profile = Profile::Ath11kWcn6750 {
            control_fd: 3,
            supervisor_fd: 4,
            vfio_fd: 5,
            dma: Wcn6750Dma::Coherent { iommufd: 6 },
            qrtr_fd: 7,
            irq_eventfds,
            ethernet_fds: vec![24, 25],
            runtime_fds: Vec::new(),
            remoteproc_state_fd: Some(26),
        };
        for inherited in [
            (3..26).collect(),
            (3..=27).collect(),
            (3..=25).chain(std::iter::once(3)).collect(),
        ] {
            let setup = Sandbox::<SetupComplete> {
                inherited,
                persistence_dir_fd: None,
                _state: PhantomData,
            };
            assert!(matches!(
                setup.lockdown(profile.clone()),
                Err(Error::ProfileAuthorityMismatch)
            ));
        }
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
                    application_listener_fd: None,
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
    fn wlancfg_application_listener_accept_is_filter_admitted() {
        const TEST: &str = "filter_tests::wlancfg_application_listener_accept_is_filter_admitted";
        if std::env::var_os("DRV_WLANCFG_ACCEPT_PROBE").is_some() {
            wlancfg_accept_body();
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(TEST)
            .env("DRV_WLANCFG_ACCEPT_PROBE", "1")
            .status()
            .unwrap();
        assert!(status.success(), "wlancfg accept probe failed: {status}");
    }

    fn wlancfg_accept_body() -> ! {
        let listener =
            unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
        let client =
            unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
        assert!(listener >= 0 && client >= 0);
        let name = format!("drv-wlancfg-filter-{}", unsafe { libc::getpid() });
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as _;
        for (target, source) in address.sun_path[1..].iter_mut().zip(name.bytes()) {
            *target = source as _;
        }
        let address_len = std::mem::size_of::<libc::sa_family_t>() + 1 + name.len();
        assert_eq!(
            unsafe {
                libc::bind(
                    listener,
                    (&address as *const libc::sockaddr_un).cast(),
                    address_len as _,
                )
            },
            0
        );
        assert_eq!(unsafe { libc::listen(listener, 1) }, 0);
        let mut sync = [0; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, sync.as_mut_ptr()) },
            0
        );
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            let mut byte = 0;
            assert_eq!(
                unsafe { libc::read(sync[1], &mut byte as *mut u8 as _, 1) },
                1
            );
            assert_eq!(
                unsafe {
                    libc::connect(
                        client,
                        (&address as *const libc::sockaddr_un).cast(),
                        address_len as _,
                    )
                },
                0
            );
            assert_eq!(unsafe { libc::write(client, b"x".as_ptr().cast(), 1) }, 1);
            assert_eq!(
                unsafe { libc::read(client, &mut byte as *mut u8 as _, 1) },
                1
            );
            unsafe { libc::_exit(if byte == b'y' { 0 } else { 1 }) }
        }

        let mut control = [0; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, control.as_mut_ptr())
            },
            0
        );
        let state = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
        assert!(state >= 0);
        enable_filter(Profile::Wlancfg {
            control_fd: control[0],
            persistence_dir_fd: state,
            application_listener_fd: Some(listener),
        });
        assert_eq!(unsafe { libc::write(sync[0], b"s".as_ptr().cast(), 1) }, 1);
        let accepted = unsafe {
            libc::accept4(
                listener,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            )
        };
        assert!(accepted >= 0);
        let mut byte = 0;
        while unsafe { libc::read(accepted, &mut byte as *mut u8 as _, 1) } < 0 {
            assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
            unsafe { libc::sched_yield() };
        }
        assert_eq!(byte, b'x');
        assert_eq!(unsafe { libc::write(accepted, b"y".as_ptr().cast(), 1) }, 1);
        unsafe { libc::_exit(0) }
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

    #[test]
    fn mt7921_tokio_readiness_and_drop_work_after_lockdown() {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        const TEST: &str = "filter_tests::mt7921_tokio_readiness_and_drop_work_after_lockdown";
        if std::env::var_os("DRV_MT7921_TOKIO_PROBE").is_some() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .unwrap();
            let local = tokio::task::LocalSet::new();
            let runtime_fds = std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .map(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .parse::<RawFd>()
                        .unwrap()
                })
                .filter(|fd| {
                    std::fs::read_link(format!("/proc/self/fd/{fd}")).is_ok_and(|path| {
                        path == std::path::Path::new("anon_inode:[eventpoll]")
                            || path == std::path::Path::new("anon_inode:[eventfd]")
                    })
                })
                .collect::<Vec<_>>();
            let raw = unsafe { libc::eventfd(1, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
            assert!(raw >= 0);
            let event = {
                let _enter = runtime.enter();
                tokio::io::unix::AsyncFd::with_interest(
                    unsafe { OwnedFd::from_raw_fd(raw) },
                    tokio::io::Interest::READABLE,
                )
                .unwrap()
            };
            let device = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
            enable_filter(Profile::Mt7921Vfio {
                pci_config_fd: device,
                vfio_fd: device,
                iommufd: device,
                irq_eventfd: raw,
                service: Some(WifiServiceFds {
                    regulatory_fd: None,
                    control_fd: device,
                    supervisor_fd: device,
                    ethernet_fds: Vec::new(),
                    runtime_fds,
                }),
            });
            local.block_on(&runtime, async {
                tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    loop {
                        let mut ready = event.readable().await.unwrap();
                        let result = ready.try_io(|fd| {
                            let mut count = 0u64;
                            let n = unsafe {
                                libc::read(fd.as_raw_fd(), (&mut count as *mut u64).cast(), 8)
                            };
                            if n < 0 {
                                Err(io::Error::last_os_error())
                            } else {
                                assert_eq!(n, 8);
                                Ok(count)
                            }
                        });
                        if let Ok(count) = result {
                            assert_eq!(count.unwrap(), 1);
                            break;
                        }
                    }
                })
                .await
                .unwrap();
            });
            drop(event);
            drop(local);
            drop(runtime);
            unsafe { libc::_exit(0) }
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(TEST)
            .env("DRV_MT7921_TOKIO_PROBE", "1")
            .status()
            .unwrap();
        assert!(status.success(), "Tokio under MT7921 filter: {status}");
    }

    #[test]
    fn mt7921_auxv_query_does_not_allow_prctl_mutation() {
        const TEST: &str = "filter_tests::mt7921_auxv_query_does_not_allow_prctl_mutation";
        if let Ok(mode) = std::env::var("DRV_MT7921_AUXV_PROBE") {
            let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
            assert!(fd >= 0);
            enable_filter(Profile::Mt7921Vfio {
                pci_config_fd: fd,
                vfio_fd: fd,
                iommufd: fd,
                irq_eventfd: fd,
                service: None,
            });
            let mut auxv = [0u8; 512];
            let operation = if mode != "mutation" {
                0x41555856
            } else {
                libc::PR_SET_DUMPABLE as u64
            };
            let result = unsafe {
                libc::syscall(
                    libc::SYS_prctl,
                    operation,
                    auxv.as_mut_ptr(),
                    if mode == "oversized" { 513 } else { auxv.len() },
                    if mode == "reserved" { 1 } else { 0 },
                    0,
                )
            };
            // Older kernels lack PR_GET_AUXV; ENOSYS/EINVAL is not SIGSYS.
            assert!(
                result >= 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL)
            );
            unsafe { libc::_exit(0) }
        }
        for mode in ["query", "mutation", "oversized", "reserved"] {
            let status = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg(TEST)
                .env("DRV_MT7921_AUXV_PROBE", mode)
                .status()
                .unwrap();
            if mode == "query" {
                assert!(status.success());
            } else {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), Some(libc::SIGSYS));
            }
        }
    }

    #[test]
    fn mt7921_regulatory_database_is_read_only_and_fd_scoped() {
        const TEST: &str = "filter_tests::mt7921_regulatory_database_is_read_only_and_fd_scoped";
        if let Ok(mode) = std::env::var("DRV_MT7921_REGDB_PROBE") {
            let database = unsafe { libc::open(c"/dev/zero".as_ptr(), libc::O_RDWR) };
            let foreign = unsafe { libc::open(c"/dev/zero".as_ptr(), libc::O_RDWR) };
            let device = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
            assert!(database >= 0 && foreign >= 0 && device >= 0);
            enable_filter(Profile::Mt7921Vfio {
                pci_config_fd: device,
                vfio_fd: device,
                iommufd: device,
                irq_eventfd: device,
                service: Some(WifiServiceFds {
                    regulatory_fd: Some(database),
                    control_fd: device,
                    supervisor_fd: device,
                    ethernet_fds: Vec::new(),
                    runtime_fds: Vec::new(),
                }),
            });
            let mut byte = 1u8;
            match mode.as_str() {
                "read" => assert_eq!(
                    unsafe { libc::read(database, (&mut byte as *mut u8).cast(), 1) },
                    1
                ),
                "write" => unsafe {
                    libc::write(database, (&byte as *const u8).cast(), 1);
                },
                "foreign" => unsafe {
                    libc::read(foreign, (&mut byte as *mut u8).cast(), 1);
                },
                "seek" => unsafe {
                    libc::lseek(database, 0, libc::SEEK_SET);
                },
                "duplicate" => unsafe {
                    libc::fcntl(database, libc::F_DUPFD_CLOEXEC, 0);
                },
                _ => unreachable!(),
            }
            unsafe { libc::_exit(0) }
        }
        for mode in ["read", "write", "foreign", "seek", "duplicate"] {
            let status = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg(TEST)
                .env("DRV_MT7921_REGDB_PROBE", mode)
                .status()
                .unwrap();
            if mode == "read" {
                assert!(status.success());
            } else {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), Some(libc::SIGSYS), "{mode}: {status}");
            }
        }
    }

    #[test]
    fn mt7921_runtime_registration_is_fd_scoped() {
        const TEST: &str = "filter_tests::mt7921_runtime_registration_is_fd_scoped";
        if let Ok(mode) = std::env::var("DRV_MT7921_REACTOR_PROBE") {
            let reactor = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
            let foreign = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
            let irq = unsafe { libc::eventfd(1, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
            let device = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
            assert!(reactor >= 0 && foreign >= 0 && irq >= 0 && device >= 0);
            enable_filter(Profile::Mt7921Vfio {
                pci_config_fd: device,
                vfio_fd: device,
                iommufd: device,
                irq_eventfd: irq,
                service: Some(WifiServiceFds {
                    regulatory_fd: None,
                    control_fd: device,
                    supervisor_fd: device,
                    ethernet_fds: Vec::new(),
                    runtime_fds: vec![reactor],
                }),
            });
            let mut event = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: 7,
            };
            assert_eq!(unsafe { libc::fcntl(irq, libc::F_GETFD) }, libc::FD_CLOEXEC);
            match mode.as_str() {
                "getfd-foreign" => unsafe {
                    libc::fcntl(foreign, libc::F_GETFD);
                },
                "setfd" => unsafe {
                    libc::fcntl(irq, libc::F_SETFD, 0);
                },
                "dupfd" => unsafe {
                    libc::fcntl(irq, libc::F_DUPFD_CLOEXEC, 0);
                },
                "getfd-high" => unsafe {
                    libc::syscall(libc::SYS_fcntl, (1u64 << 32) | irq as u64, libc::F_GETFD);
                },
                "getfd-command-high" => unsafe {
                    libc::syscall(libc::SYS_fcntl, irq, (1u64 << 32) | libc::F_GETFD as u64);
                },
                _ => {}
            }
            let (epoll, op, target) = match mode.as_str() {
                "allowed" => (reactor as u64, libc::EPOLL_CTL_ADD as u64, irq as u64),
                "foreign-reactor" => (foreign as u64, libc::EPOLL_CTL_ADD as u64, irq as u64),
                "foreign-target" => (reactor as u64, libc::EPOLL_CTL_ADD as u64, foreign as u64),
                "bad-operation" => (reactor as u64, 99, irq as u64),
                "reactor-high" => (
                    (1u64 << 32) | reactor as u64,
                    libc::EPOLL_CTL_ADD as u64,
                    irq as u64,
                ),
                "operation-high" => (
                    reactor as u64,
                    (1u64 << 32) | libc::EPOLL_CTL_ADD as u64,
                    irq as u64,
                ),
                "target-high" => (
                    reactor as u64,
                    libc::EPOLL_CTL_ADD as u64,
                    (1u64 << 32) | irq as u64,
                ),
                _ => unreachable!(),
            };
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_epoll_ctl, epoll, op, target, &mut event) },
                0
            );
            assert_eq!(
                unsafe { libc::epoll_ctl(reactor, libc::EPOLL_CTL_MOD, irq, &mut event) },
                0
            );
            assert_eq!(
                unsafe { libc::epoll_pwait(reactor, &mut event, 1, 0, std::ptr::null()) },
                1
            );
            assert_eq!(
                unsafe { libc::epoll_ctl(reactor, libc::EPOLL_CTL_DEL, irq, std::ptr::null_mut()) },
                0
            );
            unsafe { libc::_exit(0) }
        }
        for mode in [
            "allowed",
            "foreign-reactor",
            "foreign-target",
            "bad-operation",
            "reactor-high",
            "operation-high",
            "target-high",
            "getfd-foreign",
            "setfd",
            "dupfd",
            "getfd-high",
            "getfd-command-high",
        ] {
            let status = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg(TEST)
                .env("DRV_MT7921_REACTOR_PROBE", mode)
                .status()
                .unwrap();
            if mode == "allowed" {
                assert!(status.success(), "{mode}: {status}");
            } else {
                assert_eq!(status.signal(), Some(libc::SIGSYS), "{mode}: {status}");
            }
        }
    }

    #[test]
    fn wcn6750_filtered_activation_and_argument_denials() {
        if let Ok(mode) = std::env::var("DRV_WCN6750_FILTER_PROBE") {
            wcn6750_filter_body(&mode);
        }
        wcn6750_filter_child("allowed", true);
        for operation in [
            "wrong-ioctl-request",
            "ioctl-wrong-fd",
            "qrtr-wrong-fd",
            "qrtr-wrong-flags",
            "supervisor-wrong-flags",
            "ppoll-too-many",
            "epoll-wrong-fd",
            "eventfd",
            "socketpair",
        ] {
            wcn6750_filter_child(operation, false);
        }
    }

    fn wcn6750_filter_child(operation: &str, succeeds: bool) {
        const TEST: &str = "filter_tests::wcn6750_filtered_activation_and_argument_denials";
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(TEST)
            .env("DRV_WCN6750_FILTER_PROBE", operation)
            .status()
            .unwrap();
        if succeeds {
            assert!(status.success(), "WCN6750 allowed probe failed: {status}");
        } else {
            assert_eq!(
                status.signal(),
                Some(libc::SIGSYS),
                "WCN6750 {operation} did not die with SIGSYS: {status}"
            );
        }
    }

    fn wcn6750_filter_body(operation: &str) -> ! {
        let mut pairs = [[0; 2]; 7];
        for pair in &mut pairs {
            assert_eq!(
                unsafe {
                    libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, pair.as_mut_ptr())
                },
                0
            );
        }
        let vfio = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
        let iommufd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
        let remoteproc_state =
            unsafe { libc::memfd_create(c"remoteproc-state".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(remoteproc_state >= 0);
        assert_eq!(
            unsafe { libc::write(remoteproc_state, b"running\n".as_ptr().cast(), 8) },
            8
        );
        let runtime_epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        let runtime_wake = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(runtime_epoll >= 0 && runtime_wake >= 0);
        let mut registration = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: runtime_wake as u64,
        };
        assert_eq!(
            unsafe {
                libc::epoll_ctl(
                    runtime_epoll,
                    libc::EPOLL_CTL_ADD,
                    runtime_wake,
                    &mut registration,
                )
            },
            0
        );
        let irq_eventfds = std::array::from_fn(|_| {
            let fd = unsafe { libc::eventfd(1, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
            assert!(fd >= 0);
            fd
        });
        let profile = Profile::Ath11kWcn6750 {
            control_fd: pairs[0][0],
            supervisor_fd: pairs[1][0],
            vfio_fd: vfio,
            dma: Wcn6750Dma::Coherent { iommufd },
            qrtr_fd: pairs[2][0],
            irq_eventfds,
            ethernet_fds: pairs[3..].iter().flatten().copied().collect(),
            runtime_fds: vec![runtime_epoll, runtime_wake],
            remoteproc_state_fd: Some(remoteproc_state),
        };
        enable_filter(profile);
        let request = userspace_vfio::wcn6750_seccomp::COHERENT_VFIO_REQUESTS[2];
        unsafe {
            match operation {
                "allowed" => {
                    assert_eq!(libc::ioctl(vfio, request, 0), -1);
                    assert_eq!(
                        io::Error::last_os_error().raw_os_error(),
                        Some(libc::ENOTTY)
                    );
                    let byte = b'X';
                    assert_eq!(
                        libc::sendto(
                            pairs[2][0],
                            (&byte as *const u8).cast(),
                            1,
                            0,
                            std::ptr::null(),
                            0,
                        ),
                        1
                    );
                    for ethernet in &pairs[3..] {
                        assert_eq!(
                            libc::sendto(
                                ethernet[0],
                                (&byte as *const u8).cast(),
                                1,
                                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                                std::ptr::null(),
                                0,
                            ),
                            1
                        );
                        let mut received = 0_u8;
                        assert_eq!(
                            libc::recvfrom(
                                ethernet[1],
                                (&mut received as *mut u8).cast(),
                                1,
                                libc::MSG_TRUNC | libc::MSG_DONTWAIT,
                                std::ptr::null_mut(),
                                std::ptr::null_mut(),
                            ),
                            1
                        );
                        assert_eq!(received, byte);
                    }
                    let mut iov = libc::iovec {
                        iov_base: (&byte as *const u8).cast_mut().cast(),
                        iov_len: 1,
                    };
                    let mut message: libc::msghdr = std::mem::zeroed();
                    message.msg_iov = &mut iov;
                    message.msg_iovlen = 1;
                    assert_eq!(
                        libc::sendmsg(
                            pairs[1][0],
                            &message,
                            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                        ),
                        1
                    );
                    let mut pollfds = irq_eventfds.map(|fd| libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    });
                    let timeout = libc::timespec {
                        tv_sec: 0,
                        tv_nsec: 0,
                    };
                    assert_eq!(
                        libc::ppoll(
                            pollfds.as_mut_ptr(),
                            pollfds.len() as libc::nfds_t,
                            &timeout,
                            std::ptr::null(),
                        ),
                        WCN6750_IRQ_EVENTFD_COUNT as i32
                    );
                    assert_eq!(libc::lseek(remoteproc_state, 0, libc::SEEK_SET), 0);
                    assert_eq!(
                        libc::write(remoteproc_state, b"stop\n".as_ptr().cast(), 5),
                        5
                    );
                    assert_eq!(libc::lseek(remoteproc_state, 0, libc::SEEK_SET), 0);
                    let mut state = [0_u8; 8];
                    assert!(libc::read(remoteproc_state, state.as_mut_ptr().cast(), 8) > 0);
                    let one = 1_u64;
                    assert_eq!(libc::write(runtime_wake, (&one as *const u64).cast(), 8), 8);
                    let mut event: libc::epoll_event = std::mem::zeroed();
                    assert_eq!(libc::epoll_wait(runtime_epoll, &mut event, 1, 0), 1);
                    let mut observed = 0_u64;
                    assert_eq!(
                        libc::read(runtime_wake, (&mut observed as *mut u64).cast(), 8),
                        8
                    );
                }
                "wrong-ioctl-request" => {
                    libc::ioctl(vfio, 0, 0);
                }
                "ioctl-wrong-fd" => {
                    libc::ioctl(pairs[2][0], request, 0);
                }
                "qrtr-wrong-fd" => {
                    libc::sendto(vfio, std::ptr::null(), 0, 0, std::ptr::null(), 0);
                }
                "qrtr-wrong-flags" => {
                    libc::sendto(
                        pairs[2][0],
                        std::ptr::null(),
                        0,
                        libc::MSG_NOSIGNAL,
                        std::ptr::null(),
                        0,
                    );
                }
                "supervisor-wrong-flags" => {
                    let message: libc::msghdr = std::mem::zeroed();
                    libc::sendmsg(pairs[1][0], &message, 0);
                }
                "ppoll-too-many" => {
                    let mut pollfds = [libc::pollfd {
                        fd: irq_eventfds[0],
                        events: libc::POLLIN,
                        revents: 0,
                    }; WCN6750_IRQ_EVENTFD_COUNT + 2];
                    libc::ppoll(
                        pollfds.as_mut_ptr(),
                        pollfds.len() as libc::nfds_t,
                        std::ptr::null(),
                        std::ptr::null(),
                    );
                }
                "epoll-wrong-fd" => {
                    let mut event: libc::epoll_event = std::mem::zeroed();
                    libc::epoll_wait(vfio, &mut event, 1, 0);
                }
                "eventfd" => {
                    libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK);
                }
                "socketpair" => {
                    let mut fds = [-1; 2];
                    libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, fds.as_mut_ptr());
                }
                _ => unreachable!(),
            }
            let disable_stack = libc::stack_t {
                ss_sp: std::ptr::null_mut(),
                ss_flags: libc::SS_DISABLE,
                ss_size: libc::SIGSTKSZ,
            };
            assert_eq!(libc::sigaltstack(&disable_stack, std::ptr::null_mut()), 0);
            libc::_exit(0)
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
                assert_eq!(unsafe { libc::poll(pollfds.as_mut_ptr(), 2, 0) }, 2);
                assert_eq!(unsafe { libc::poll(pollfds.as_mut_ptr(), 2, 32000) }, 2);
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
                application_listener_fd: None,
            },
            "mt" => Profile::Mt7921Vfio {
                pci_config_fd: fds[0],
                vfio_fd: fds[1],
                iommufd: fds[2],
                irq_eventfd: fds[3],
                service: None,
            },
            _ => unreachable!(),
        }
    }

    fn enable_filter(profile: Profile) {
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
            0
        );
        install_filter(&profile).unwrap();
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
                    libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
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
