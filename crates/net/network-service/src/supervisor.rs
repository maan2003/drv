// SPDX-License-Identifier: GPL-2.0-only

#[cfg(test)]
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::os::fd::{AsFd as _, AsRawFd, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const FRAME_FD: RawFd = 3;
const LISTENER_FD: RawFd = 4;
const BOOTSTRAP_FD: RawFd = 5;
const LINK_CONTROL_FD: RawFd = crate::link_control::CONTROL_FD;
const POLLERR: i16 = 0x008;
const POLLHUP: i16 = 0x010;
const POLLNVAL: i16 = 0x020;

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn dup2(old: i32, new: i32) -> i32;
    fn fcntl(fd: i32, command: i32, ...) -> i32;
    fn poll(fds: *mut PollFd, count: usize, timeout_ms: i32) -> i32;
}

fn capability_revoked(frame: &OwnedFd) -> Result<bool, String> {
    let mut descriptor = PollFd {
        fd: frame.as_raw_fd(),
        events: 0,
        revents: 0,
    };
    let result = unsafe { poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return Err(format!(
            "inspect network-service Ethernet generation: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(descriptor.revents & (POLLERR | POLLHUP | POLLNVAL) != 0)
}

fn duplicate_capability(fd: RawFd) -> Result<OwnedFd, String> {
    const F_DUPFD_CLOEXEC: i32 = 1030;
    let duplicate = unsafe { fcntl(fd, F_DUPFD_CLOEXEC, 10) };
    if duplicate < 0 {
        return Err(format!(
            "duplicate network-service capability: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn bootstrap(
    stream: &mut std::os::unix::net::UnixStream,
    kernel_provider: bool,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("set network-service bootstrap timeout: {error}"))?;
    let mut ready = [0; 5];
    stream
        .read_exact(&mut ready)
        .map_err(|error| format!("network-service READY: {error}"))?;
    if &ready != b"READY" {
        return Err("invalid network-service READY".into());
    }
    stream
        .write_all(b"GO")
        .map_err(|error| format!("network-service GO: {error}"))?;
    if kernel_provider {
        stream
            .set_read_timeout(Some(Duration::from_secs(45)))
            .map_err(|error| format!("set network-provider readiness timeout: {error}"))?;
        let mut ready = [0; 13];
        stream
            .read_exact(&mut ready)
            .map_err(|error| format!("network provider readiness: {error}"))?;
        if &ready != b"NETWORK_READY" {
            return Err("invalid network-provider NETWORK_READY".into());
        }
        stream
            .write_all(b"SERVE")
            .map_err(|error| format!("network provider SERVE: {error}"))?;
        stream
            .shutdown(std::net::Shutdown::Write)
            .map_err(|error| format!("network provider SERVE shutdown: {error}"))?;
    } else {
        let mut started = [0; 7];
        stream
            .read_exact(&mut started)
            .map_err(|error| format!("network service STARTED: {error}"))?;
        if &started != b"STARTED" {
            return Err("invalid network-service STARTED".into());
        }
    }
    Ok(())
}

/// An observed process exit, with independent provider and link generations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkServiceProcessExit {
    pub provider_generation: u64,
    pub ethernet_generation: Option<u64>,
    pub success: bool,
}

struct RunningProcess {
    child: Child,
}

struct InstalledGeneration {
    generation: u64,
    frame: OwnedFd,
    running: Option<RunningProcess>,
}

struct KernelProcess {
    generation: u64,
    child: Child,
    control: OwnedFd,
}

enum KernelLink {
    Offline,
    Attached { generation: u64, frame: OwnedFd },
}

/// Trusted launcher for sandboxed network-service generations.
///
/// Legacy SOCKS starts a process per Ethernet generation. The production
/// kernel frontend starts offline and changes only its narrowly controlled
/// Ethernet capability until the provider itself exits or is terminated.
enum NetworkFrontend {
    Socks {
        listener: TcpListener,
        listen: SocketAddr,
    },
    Kernel {
        registration_path: PathBuf,
        resolver_listener: Option<std::os::unix::net::UnixListener>,
    },
}

pub struct NetworkServiceSupervisor {
    binary: PathBuf,
    frontend: NetworkFrontend,
    mac_address: [u8; 6],
    next_generation: u64,
    installed: Option<InstalledGeneration>,
    next_provider_generation: u64,
    kernel_process: Option<KernelProcess>,
    kernel_link: KernelLink,
    #[cfg(test)]
    arguments: Vec<OsString>,
    #[cfg(test)]
    fixture: bool,
    #[cfg(test)]
    fixture_exit_after_start: bool,
    #[cfg(test)]
    fixture_echo_frames: bool,
}

impl NetworkServiceSupervisor {
    pub(crate) fn mac_address(&self) -> [u8; 6] {
        self.mac_address
    }

    pub fn new(
        binary: impl AsRef<Path>,
        listener: TcpListener,
        mac_address: [u8; 6],
    ) -> Result<Self, String> {
        let listen = listener
            .local_addr()
            .map_err(|error| format!("inspect network-service listener: {error}"))?;
        if !listen.ip().is_loopback() {
            return Err("network-service SOCKS listener must be loopback".into());
        }
        if mac_address == [0; 6] || mac_address[0] & 1 != 0 {
            return Err("network-service MAC must be nonzero unicast".into());
        }
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("make network-service listener nonblocking: {error}"))?;
        Ok(Self {
            binary: binary.as_ref().to_owned(),
            frontend: NetworkFrontend::Socks { listener, listen },
            mac_address,
            next_generation: 1,
            installed: None,
            next_provider_generation: 1,
            kernel_process: None,
            kernel_link: KernelLink::Offline,
            #[cfg(test)]
            arguments: Vec::new(),
            #[cfg(test)]
            fixture: false,
            #[cfg(test)]
            fixture_exit_after_start: false,
            #[cfg(test)]
            fixture_echo_frames: false,
        })
    }

    /// Construct the production kernel-socket provider supervisor. A fresh
    /// registration session is opened for each independently replaceable
    /// provider generation after its predecessor has been reaped. The optional
    /// resolver listener survives those generations; its caller owns pathname locking.
    pub fn new_kernel(
        binary: impl AsRef<Path>,
        registration_path: impl AsRef<Path>,
        mac_address: [u8; 6],
        resolver_listener: Option<std::os::unix::net::UnixListener>,
    ) -> Result<Self, String> {
        if mac_address == [0; 6] || mac_address[0] & 1 != 0 {
            return Err("network-service MAC must be nonzero unicast".into());
        }
        Ok(Self {
            binary: binary.as_ref().to_owned(),
            frontend: NetworkFrontend::Kernel {
                registration_path: registration_path.as_ref().to_owned(),
                resolver_listener,
            },
            mac_address,
            next_generation: 1,
            installed: None,
            next_provider_generation: 1,
            kernel_process: None,
            kernel_link: KernelLink::Offline,
            #[cfg(test)]
            arguments: Vec::new(),
            #[cfg(test)]
            fixture: false,
            #[cfg(test)]
            fixture_exit_after_start: false,
            #[cfg(test)]
            fixture_echo_frames: false,
        })
    }

    pub fn install_generation(&mut self, frame: OwnedFd) -> Result<u64, String> {
        if matches!(self.frontend, NetworkFrontend::Kernel { .. }) {
            let generation = self.next_generation;
            self.next_generation = generation
                .checked_add(1)
                .ok_or("Ethernet generation exhausted")?;
            return self.attach_generation(generation, frame);
        }
        let generation = self.next_generation;
        let next_generation = generation
            .checked_add(1)
            .ok_or("network-service generation exhausted")?;
        self.terminate()?;
        self.installed = Some(InstalledGeneration {
            generation,
            frame,
            running: None,
        });
        self.next_generation = next_generation;
        match self.start_installed() {
            Ok(()) => Ok(generation),
            Err(error) => match self.terminate() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; cleanup failed: {cleanup}")),
            },
        }
    }

    /// Restarts the process for the currently installed Ethernet generation.
    ///
    /// The caller must first observe the preceding process exit with
    /// [`Self::poll_exit`]. The Ethernet generation remains unchanged, but a
    /// production restart creates a fresh provider namespace; old sockets stay
    /// revoked. Legacy SOCKS retains its process-generation convention.
    pub fn restart_generation(&mut self) -> Result<u64, String> {
        if matches!(self.frontend, NetworkFrontend::Kernel { .. }) {
            if self.kernel_process.is_some() {
                return Err("network provider generation is still running".into());
            }
            let retained = match &self.kernel_link {
                KernelLink::Offline => None,
                KernelLink::Attached { generation, frame } => {
                    Some((*generation, duplicate_capability(frame.as_raw_fd())?))
                }
            };
            self.start_provider()?;
            if let Some((generation, frame)) = retained {
                self.attach_generation(generation, frame)?;
            }
            return Ok(self.kernel_process.as_ref().unwrap().generation);
        }
        let generation = self
            .installed
            .as_ref()
            .ok_or("no network-service generation installed")?
            .generation;
        if self
            .installed
            .as_ref()
            .is_some_and(|installed| installed.running.is_some())
        {
            return Err("network-service generation is still running".into());
        }
        if capability_revoked(&self.installed.as_ref().unwrap().frame)? {
            self.installed = None;
            return Err("network-service Ethernet generation is revoked".into());
        }
        self.start_installed()?;
        Ok(generation)
    }

    fn start_installed(&mut self) -> Result<(), String> {
        let installed = self
            .installed
            .as_ref()
            .ok_or("no network-service generation installed")?;
        let (frontend, kernel_provider) = match &self.frontend {
            NetworkFrontend::Socks { listener, .. } => {
                (duplicate_capability(listener.as_raw_fd())?, false)
            }
            NetworkFrontend::Kernel { .. } => {
                return Err("kernel provider uses persistent process startup".into());
            }
        };
        let frame = duplicate_capability(installed.frame.as_raw_fd())?;
        let (mut bootstrap_parent, bootstrap_child) = std::os::unix::net::UnixStream::pair()
            .map_err(|error| format!("create network-service bootstrap: {error}"))?;
        let bootstrap_pass = duplicate_capability(bootstrap_child.as_raw_fd())?;
        let frame_fd = frame.as_raw_fd();
        let frontend_fd = frontend.as_raw_fd();
        let bootstrap_fd = bootstrap_pass.as_raw_fd();
        let mac = self
            .mac_address
            .map(|octet| format!("{octet:02x}"))
            .join(":");
        let mut command = Command::new(&self.binary);
        command
            .env_clear()
            .env("DRV_SAE_CLIENT_MAC", &mac)
            .env("DRV_NETSTACK_PARENT_PID", std::process::id().to_string())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        match &self.frontend {
            NetworkFrontend::Socks { listen, .. } => {
                command.env("DRV_SOCKS5_LISTEN", listen.to_string());
            }
            NetworkFrontend::Kernel { .. } => {
                #[cfg(not(test))]
                command.args(["--ethernet-mac", &mac, "--bootstrap"]);
                #[cfg(test)]
                if !self.fixture {
                    command.args(["--ethernet-mac", &mac, "--bootstrap"]);
                }
            }
        }
        #[cfg(test)]
        {
            command.args(&self.arguments);
            if self.fixture {
                command.env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE", "1");
                if kernel_provider {
                    command
                        .env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_KERNEL", "1")
                        .env(
                            "DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_REGISTRATION",
                            match &self.frontend {
                                NetworkFrontend::Kernel { registration_path, .. } => registration_path,
                                NetworkFrontend::Socks { .. } => unreachable!(),
                            },
                        );
                }
            }
            if self.fixture_exit_after_start {
                command.env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_EXIT", "1");
            }
            if self.fixture_echo_frames {
                command.env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_ECHO", "1");
            }
        }
        unsafe {
            command.pre_exec(move || {
                let pass = if kernel_provider {
                    [
                        (frontend_fd, FRAME_FD),
                        (frame_fd, LISTENER_FD),
                        (bootstrap_fd, BOOTSTRAP_FD),
                    ]
                } else {
                    [
                        (frame_fd, FRAME_FD),
                        (frontend_fd, LISTENER_FD),
                        (bootstrap_fd, BOOTSTRAP_FD),
                    ]
                };
                for (source, target) in pass {
                    if dup2(source, target) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .map_err(|error| format!("spawn network service: {error}"))?;
        drop((frame, frontend, bootstrap_pass, bootstrap_child));
        self.installed.as_mut().unwrap().running = Some(RunningProcess { child });
        if let Err(error) = bootstrap(&mut bootstrap_parent, kernel_provider) {
            return match self.terminate_process() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; cleanup failed: {cleanup}")),
            };
        }
        Ok(())
    }

    /// Starts a fresh production provider generation while offline.
    pub fn start_provider(&mut self) -> Result<u64, String> {
        let NetworkFrontend::Kernel { registration_path, resolver_listener } = &self.frontend else {
            return Err("SOCKS frontend starts with an Ethernet generation".into());
        };
        if self.kernel_process.is_some() {
            return Err("network provider generation is already running".into());
        }
        let registration = OpenOptions::new()
            .read(true)
            .write(true)
            .open(registration_path)
            .map_err(|error| format!("open fresh kernel registration: {error}"))?;
        let registration = duplicate_capability(registration.as_raw_fd())?;
        let placeholder = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .map_err(|error| format!("reserve provider frame slot: {error}"))?;
        let placeholder = duplicate_capability(placeholder.as_raw_fd())?;
        let (control_parent, control_child) = seqpacket_pair()?;
        let control_pass = duplicate_capability(control_child.as_raw_fd())?;
        let (mut bootstrap_parent, bootstrap_child) = std::os::unix::net::UnixStream::pair()
            .map_err(|error| format!("create network-provider bootstrap: {error}"))?;
        let bootstrap_pass = duplicate_capability(bootstrap_child.as_raw_fd())?;
        let generation = self.next_provider_generation;
        let next = generation.checked_add(1).ok_or("provider generation exhausted")?;
        let mac = self.mac_address.map(|octet| format!("{octet:02x}")).join(":");
        let resolver_pass = resolver_listener.as_ref()
            .map(|listener| duplicate_capability(listener.as_raw_fd())).transpose()?;
        let mut inherited = vec![
            (registration.as_raw_fd(), FRAME_FD),
            (placeholder.as_raw_fd(), LISTENER_FD),
            (bootstrap_pass.as_raw_fd(), BOOTSTRAP_FD),
            (control_pass.as_raw_fd(), LINK_CONTROL_FD),
            (placeholder.as_raw_fd(), crate::link_control::FRAME_RESERVATION_FD),
        ];
        if let Some(listener) = &resolver_pass {
            inherited.push((listener.as_raw_fd(), 7));
        }
        let mut command = Command::new(&self.binary);
        command
            .env_clear()
            .env("DRV_SAE_CLIENT_MAC", &mac)
            .env("DRV_NETSTACK_PARENT_PID", std::process::id().to_string())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        #[cfg(not(test))]
        command.args(["--ethernet-mac", &mac, "--bootstrap", "--link-control"]);
        #[cfg(test)]
        {
            command.args(&self.arguments);
            if self.fixture {
                command
                    .env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE", "1")
                    .env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_KERNEL", "1")
                    .env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_LINK_CONTROL", "1")
                    .env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_REGISTRATION", registration_path);
            } else {
                command.args(["--ethernet-mac", &mac, "--bootstrap", "--link-control"]);
            }
            if self.fixture_exit_after_start {
                command.env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_EXIT", "1");
            }
            if self.fixture_echo_frames {
                command.env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_ECHO", "1");
            }
        }
        if resolver_pass.is_some() {
            #[cfg(not(test))]
            command.arg("--resolver-fd");
            #[cfg(test)]
            if self.fixture {
                let address = resolver_listener.as_ref().unwrap().local_addr().unwrap();
                command.env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_RESOLVER",
                    address.as_pathname().unwrap());
            } else {
                command.arg("--resolver-fd");
            }
        }
        unsafe {
            command.pre_exec(move || {
                for &(source, target) in &inherited {
                    if dup2(source, target) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = command.spawn()
            .map_err(|error| format!("spawn network provider: {error}"))?;
        drop((registration, placeholder, bootstrap_pass, bootstrap_child, control_child, control_pass));
        self.kernel_process = Some(KernelProcess {
            generation,
            child,
            control: control_parent,
        });
        self.next_provider_generation = next;
        if let Err(error) = bootstrap(&mut bootstrap_parent, true) {
            let cleanup = self.terminate_kernel_process();
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => format!("{error}; cleanup failed: {cleanup}"),
            });
        }
        Ok(generation)
    }

    pub fn attach_generation(&mut self, generation: u64, frame: OwnedFd) -> Result<u64, String> {
        if generation == 0 {
            return Err("Ethernet generation must be nonzero".into());
        }
        if !matches!(self.frontend, NetworkFrontend::Kernel { .. }) {
            return self.install_generation(frame);
        }
        crate::lifecycle::validate_seqpacket(frame.as_raw_fd())?;
        if self.kernel_process.is_none() {
            self.start_provider()?;
        }
        crate::link_control::send_attach(
            self.kernel_process.as_ref().unwrap().control.as_fd(),
            generation,
            frame.as_fd(),
        )?;
        let status = self.wait_link_ack(generation);
        match status {
            Ok(crate::link_control::AckStatus::Applied) => {
                self.kernel_link = KernelLink::Attached { generation, frame };
                Ok(self.kernel_process.as_ref().unwrap().generation)
            }
            Ok(crate::link_control::AckStatus::Rejected) => {
                Err("network provider rejected Ethernet generation".into())
            }
            Err(error) => {
                // The request was admitted by sendmsg. Without its matching
                // completion the parent cannot know which capability is live.
                let cleanup = self.terminate_kernel_process();
                self.kernel_link = KernelLink::Offline;
                Err(match cleanup {
                    Ok(()) => error,
                    Err(cleanup) => format!("{error}; fail-closed cleanup failed: {cleanup}"),
                })
            }
        }
    }

    pub fn revoke_generation(&mut self, generation: u64) -> Result<(), String> {
        if !matches!(self.frontend, NetworkFrontend::Kernel { .. }) {
            let active = self.installed.as_ref()
                .ok_or("network service has no active Ethernet generation")?
                .generation;
            if active != generation {
                return Err("Ethernet revocation does not match active generation".into());
            }
            return self.terminate();
        }
        let KernelLink::Attached { generation: active, .. } = &self.kernel_link else {
            return Err("network provider has no active Ethernet generation".into());
        };
        if *active != generation {
            return Err("Ethernet revocation does not match active generation".into());
        }
        let process = self.kernel_process.as_ref()
            .ok_or("network provider is not running")?;
        crate::link_control::send_detach(
            process.control.as_fd(),
            generation,
        )?;
        match self.wait_link_ack(generation) {
            Ok(crate::link_control::AckStatus::Applied) => {
                self.kernel_link = KernelLink::Offline;
                Ok(())
            }
            Ok(crate::link_control::AckStatus::Rejected) => {
                Err("network provider rejected Ethernet revocation".into())
            }
            Err(error) => {
                let cleanup = self.terminate_kernel_process();
                self.kernel_link = KernelLink::Offline;
                Err(match cleanup {
                    Ok(()) => error,
                    Err(cleanup) => format!("{error}; fail-closed cleanup failed: {cleanup}"),
                })
            }
        }
    }

    fn wait_link_ack(&mut self, generation: u64) -> Result<crate::link_control::AckStatus, String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let process = self.kernel_process.as_mut()
                .ok_or("network provider is not running")?;
            if let Some(status) = process.child.try_wait()
                .map_err(|error| format!("poll provider during link transfer: {error}"))?
            {
                return Err(format!("network provider exited during link transfer: {status}"));
            }
            let mut descriptor = libc::pollfd {
                fd: process.control.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err("network provider link-control completion timed out".into());
            }
            let timeout = remaining.as_millis().min(100) as i32;
            let ready = unsafe { libc::poll(&mut descriptor, 1, timeout.max(1)) };
            if ready < 0 {
                return Err(format!("poll network provider link control: {}", std::io::Error::last_os_error()));
            }
            if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err("network provider link-control channel closed".into());
            }
            if descriptor.revents & libc::POLLIN != 0 {
                return crate::link_control::receive_ack(
                    process.control.as_fd(),
                    generation,
                );
            }
        }
    }

    fn terminate_kernel_process(&mut self) -> Result<(), String> {
        let Some(mut process) = self.kernel_process.take() else {
            return Ok(());
        };
        match process.child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) => {
                self.kernel_process = Some(process);
                return Err(format!("poll network provider before termination: {error}"));
            }
        }
        if let Err(error) = process.child.kill() {
            self.kernel_process = Some(process);
            return Err(format!("terminate network provider: {error}"));
        }
        match process.child.wait() {
            Ok(_) => Ok(()),
            Err(error) => {
                self.kernel_process = Some(process);
                Err(format!("reap network provider: {error}"))
            }
        }
    }

    pub fn poll_exit(&mut self) -> Result<Option<NetworkServiceProcessExit>, String> {
        if let Some(process) = self.kernel_process.as_mut() {
            let Some(status) = process.child.try_wait()
                .map_err(|error| format!("poll network provider: {error}"))?
            else {
                return Ok(None);
            };
            let generation = process.generation;
            self.kernel_process = None;
            return Ok(Some(NetworkServiceProcessExit {
                provider_generation: generation,
                ethernet_generation: match &self.kernel_link {
                    KernelLink::Offline => None,
                    KernelLink::Attached { generation, .. } => Some(*generation),
                },
                success: status.success(),
            }));
        }
        let Some(installed) = self.installed.as_mut() else {
            return Ok(None);
        };
        let Some(running) = installed.running.as_mut() else {
            return Ok(None);
        };
        let Some(status) = running
            .child
            .try_wait()
            .map_err(|error| format!("poll network service: {error}"))?
        else {
            return Ok(None);
        };
        let exit = NetworkServiceProcessExit {
            provider_generation: installed.generation,
            ethernet_generation: Some(installed.generation),
            success: status.success(),
        };
        installed.running = None;
        Ok(Some(exit))
    }

    pub fn terminate(&mut self) -> Result<(), String> {
        self.terminate_process()?;
        self.terminate_kernel_process()?;
        self.installed = None;
        self.kernel_link = KernelLink::Offline;
        Ok(())
    }

    fn terminate_process(&mut self) -> Result<(), String> {
        let Some(installed) = self.installed.as_mut() else {
            return Ok(());
        };
        let Some(mut running) = installed.running.take() else {
            return Ok(());
        };
        match running.child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) => {
                installed.running = Some(running);
                return Err(format!("poll network service before termination: {error}"));
            }
        }
        if let Err(error) = running.child.kill() {
            installed.running = Some(running);
            return Err(format!("terminate network service: {error}"));
        }
        match running.child.wait() {
            Ok(_) => Ok(()),
            Err(error) => {
                installed.running = Some(running);
                Err(format!("reap network service: {error}"))
            }
        }
    }
}


fn seqpacket_pair() -> Result<(OwnedFd, OwnedFd), String> {
    let mut descriptors = [-1; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
            descriptors.as_mut_ptr(),
        )
    } != 0
    {
        return Err(format!(
            "create provider link-control capability: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe {
        (
            OwnedFd::from_raw_fd(descriptors[0]),
            OwnedFd::from_raw_fd(descriptors[1]),
        )
    })
}

impl Drop for NetworkServiceSupervisor {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WifiLifecycleReceiver, WifiLifecycleUpdate};
    use std::fs::File;
    use std::io::ErrorKind;
    use std::mem::size_of;
    use std::os::fd::IntoRawFd as _;
    use std::os::unix::net::UnixStream;
    use wlan_softmac_host::ethernet::{EthernetIngressError, ethernet_port};

    fn lifecycle_channel() -> (OwnedFd, OwnedFd) {
        let mut sockets = [-1; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                    0,
                    sockets.as_mut_ptr(),
                )
            },
            0
        );
        unsafe {
            (
                OwnedFd::from_raw_fd(sockets[0]),
                OwnedFd::from_raw_fd(sockets[1]),
            )
        }
    }

    fn send_lifecycle(channel: &OwnedFd, bytes: &[u8], descriptors: &[RawFd]) {
        let mut bytes = bytes.to_vec();
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let control_len = if descriptors.is_empty() {
            0
        } else {
            unsafe { libc::CMSG_SPACE(size_of_val(descriptors) as u32) as usize }
        };
        let mut control = vec![0usize; control_len.div_ceil(size_of::<usize>())];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control_len as _;
        if !descriptors.is_empty() {
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&message);
                (*header).cmsg_len = libc::CMSG_LEN(size_of_val(descriptors) as u32) as _;
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                std::ptr::copy_nonoverlapping(
                    descriptors.as_ptr(),
                    libc::CMSG_DATA(header).cast::<RawFd>(),
                    descriptors.len(),
                );
            }
        }
        assert_eq!(
            unsafe { libc::sendmsg(channel.as_raw_fd(), &message, 0) },
            bytes.len() as isize
        );
    }

    fn lifecycle_message(
        kind: wifi_supervisor_wire::LifecycleKind,
        ethernet_generation: u64,
    ) -> [u8; wifi_supervisor_wire::MESSAGE_LEN] {
        wifi_supervisor_wire::LifecycleMessage {
            kind,
            wifi_generation: [7; 16],
            ethernet_generation,
            mac_address: [2, 0, 0, 0, 0, 1],
        }
        .encode()
    }

    fn wait_for_exit(supervisor: &mut NetworkServiceSupervisor) -> NetworkServiceProcessExit {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(exit) = supervisor.poll_exit().unwrap() {
                return exit;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "network-service fixture did not exit"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_transmit(
        driver: &mut wlan_softmac_host::ethernet::DriverEthernetPort,
    ) -> netstack3_port_spike::EthernetFrame {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(frame) = driver.take_transmit().unwrap() {
                return frame;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "network-service fixture did not return a frame"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_lifecycle_update(
        receiver: &mut WifiLifecycleReceiver,
        supervisor: &mut NetworkServiceSupervisor,
    ) -> WifiLifecycleUpdate {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(update) = receiver.receive(supervisor).unwrap() {
                return update;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Wi-Fi lifecycle receiver did not observe an update"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_driver_closed(driver: &mut wlan_softmac_host::ethernet::DriverEthernetPort) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match driver.deliver(&[0; 14]) {
                Err(EthernetIngressError::Closed) => return,
                Ok(()) | Err(EthernetIngressError::Backpressure) => {}
                Err(error) => panic!("unexpected driver state while waiting for close: {error:?}"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Ethernet driver peer did not close"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn supervisor_fixture_child() {
        if std::env::var_os("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE").is_none() {
            return;
        }
        const F_GETFD: i32 = 1;
        const FD_CLOEXEC: i32 = 1;
        for fd in [FRAME_FD, LISTENER_FD, BOOTSTRAP_FD] {
            let flags = unsafe { fcntl(fd, F_GETFD) };
            assert!(flags >= 0, "missing inherited fd {fd}");
            assert_eq!(flags & FD_CLOEXEC, 0, "inherited fd {fd} is CLOEXEC");
        }
        if let Some(path) = std::env::var_os("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_RESOLVER") {
            let resolver = unsafe { std::os::unix::net::UnixListener::from_raw_fd(7) };
            assert_eq!(resolver.local_addr().unwrap().as_pathname(), Some(Path::new(&path)));
            assert_eq!(unsafe { fcntl(7, F_GETFD) } & FD_CLOEXEC, 0);
            // This fixture checks inheritance; the supervisor retains its
            // own listener for the replacement process.
            drop(resolver);
        }
        let mut bootstrap = unsafe { UnixStream::from_raw_fd(BOOTSTRAP_FD) };
        bootstrap.write_all(b"READY").unwrap();
        let mut go = [0; 2];
        bootstrap.read_exact(&mut go).unwrap();
        assert_eq!(&go, b"GO");
        if std::env::var_os("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_KERNEL").is_some() {
            assert_eq!(
                std::fs::read_link("/proc/self/fd/3").unwrap(),
                PathBuf::from(
                    std::env::var_os("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_REGISTRATION")
                        .unwrap()
                )
            );
            bootstrap.write_all(b"NETWORK_READY").unwrap();
            let mut serve = [0; 5];
            bootstrap.read_exact(&mut serve).unwrap();
            assert_eq!(&serve, b"SERVE");
        } else {
            bootstrap.write_all(b"STARTED").unwrap();
        }
        drop(bootstrap);
        if std::env::var_os("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_EXIT").is_some() {
            return;
        }
        if std::env::var_os("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_LINK_CONTROL").is_some() {
            for fd in [4, crate::link_control::FRAME_RESERVATION_FD] {
                assert_eq!(
                    std::fs::read_link(format!("/proc/self/fd/{fd}")).unwrap(),
                    PathBuf::from("/dev/null")
                );
            }
            crate::lifecycle::validate_seqpacket(LINK_CONTROL_FD).unwrap();
            let control = unsafe { std::os::fd::BorrowedFd::borrow_raw(LINK_CONTROL_FD) };
            let mut active: Option<(u64, OwnedFd)> = None;
            let mut last = 0;
            loop {
                let frame_fd = active.as_ref().map_or(-1, |(_, frame)| frame.as_raw_fd());
                let mut poll = [
                    libc::pollfd { fd: LINK_CONTROL_FD, events: libc::POLLIN, revents: 0 },
                    libc::pollfd { fd: frame_fd, events: libc::POLLIN, revents: 0 },
                ];
                assert!(unsafe { libc::poll(poll.as_mut_ptr(), poll.len() as _, -1) } >= 0);
                if poll[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                    return;
                }
                if poll[0].revents & libc::POLLIN != 0 {
                    let Some(request) = crate::link_control::receive_request(control).unwrap() else {
                        continue;
                    };
                    let (generation, accepted) = match request {
                        crate::link_control::Request::Attach { generation, frame }
                            if generation > last =>
                        {
                            active = Some((generation, frame));
                            last = generation;
                            (generation, true)
                        }
                        crate::link_control::Request::Detach { generation }
                            if active.as_ref().is_some_and(|(active, _)| *active == generation) =>
                        {
                            active = None;
                            (generation, true)
                        }
                        crate::link_control::Request::Attach { generation, .. }
                        | crate::link_control::Request::Detach { generation } => {
                            (generation, false)
                        }
                    };
                    crate::link_control::send_ack(
                        control,
                        generation,
                        if accepted {
                            crate::link_control::AckStatus::Applied
                        } else {
                            crate::link_control::AckStatus::Rejected
                        },
                    ).unwrap();
                }
                if poll[1].revents & libc::POLLIN != 0 {
                    let (_, frame) = active.as_ref().unwrap();
                    let mut bytes = [0; 1514];
                    let read = unsafe {
                        libc::recv(frame.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len(), libc::MSG_DONTWAIT)
                    };
                    if read > 0 && std::env::var_os("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_ECHO").is_some() {
                        assert_eq!(unsafe {
                            libc::send(frame.as_raw_fd(), bytes.as_ptr().cast(), read as _, libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL)
                        }, read);
                    }
                }
            }
        }
        if std::env::var_os("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_KERNEL").is_some() {
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }

        let mut frame = unsafe { File::from_raw_fd(FRAME_FD) };
        let mut bytes = [0; 1514];
        let echo = std::env::var_os("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_ECHO").is_some();
        loop {
            match frame.read(&mut bytes) {
                Ok(0) => break,
                Ok(read) if echo => frame.write_all(&bytes[..read]).unwrap(),
                Ok(_) => panic!("fixture received an unexpected frame"),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("fixture frame read failed: {error}"),
            }
        }
    }

    #[test]
    fn bootstrap_requires_exact_ready_and_acknowledges_go() {
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            child.write_all(b"READY").unwrap();
            let mut go = [0; 2];
            child.read_exact(&mut go).unwrap();
            assert_eq!(&go, b"GO");
            child.write_all(b"STARTED").unwrap();
        });
        bootstrap(&mut parent, false).unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn accepted_clients_inherit_accounted_listener_socket_buffers() {
        fn buffer(fd: RawFd, option: i32) -> i32 {
            let mut value = 0i32;
            let mut length = size_of::<i32>() as libc::socklen_t;
            assert_eq!(
                unsafe {
                    libc::getsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        option,
                        (&mut value as *mut i32).cast(),
                        &mut length,
                    )
                },
                0
            );
            value
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        crate::bound_listener_socket_memory(&listener).unwrap();
        let _client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        for option in [libc::SO_RCVBUF, libc::SO_SNDBUF] {
            assert_eq!(
                buffer(accepted.as_raw_fd(), option),
                buffer(listener.as_raw_fd(), option)
            );
        }
    }

    #[test]
    fn bootstrap_rejects_non_protocol_bytes() {
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        child.write_all(b"NOPE!").unwrap();
        assert_eq!(
            bootstrap(&mut parent, false),
            Err("invalid network-service READY".into())
        );
    }

    #[test]
    fn bootstrap_rejects_eof_before_started() {
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            child.write_all(b"READY").unwrap();
            let mut go = [0; 2];
            child.read_exact(&mut go).unwrap();
        });
        assert!(matches!(
            bootstrap(&mut parent, false),
            Err(error) if error.contains("STARTED")
        ));
        peer.join().unwrap();
    }

    #[test]
    fn supervisor_starts_without_a_generation_and_rejects_invalid_mac() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut supervisor = NetworkServiceSupervisor::new(
            "/not/spawned/until-a-generation-arrives",
            listener,
            [2, 0, 0, 0, 0, 1],
        )
        .unwrap();
        assert_eq!(supervisor.poll_exit(), Ok(None));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(matches!(
            NetworkServiceSupervisor::new("/unused", listener, [0; 6]),
            Err(error) if error == "network-service MAC must be nonzero unicast"
        ));
    }

    #[test]
    fn supervisor_reaps_revoked_generation_before_installing_replacement() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut supervisor = NetworkServiceSupervisor::new(
            std::env::current_exe().unwrap(),
            listener,
            [2, 0, 0, 0, 0, 1],
        )
        .unwrap();
        supervisor.arguments = [
            "--exact",
            "supervisor::tests::supervisor_fixture_child",
            "--nocapture",
        ]
        .map(OsString::from)
        .into();
        supervisor.fixture = true;

        let (first, mut first_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        first_driver.set_link(true);
        assert_eq!(supervisor.install_generation(first.into_frame_fd()), Ok(1));
        let first_pid = supervisor
            .installed
            .as_ref()
            .unwrap()
            .running
            .as_ref()
            .unwrap()
            .child
            .id() as i32;

        let (second, second_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        assert_eq!(supervisor.install_generation(second.into_frame_fd()), Ok(2));
        let waited = unsafe { libc::waitpid(first_pid, std::ptr::null_mut(), libc::WNOHANG) };
        assert_eq!(
            waited, -1,
            "replaced child remained waitable instead of being reaped"
        );
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(10));
        wait_for_driver_closed(&mut first_driver);

        drop(second_driver);
        assert_eq!(wait_for_exit(&mut supervisor).provider_generation, 2);
        supervisor.terminate().unwrap();
        assert_eq!(supervisor.poll_exit(), Ok(None));
    }

    #[test]
    fn supervisor_restarts_exited_process_with_retained_generation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut supervisor = NetworkServiceSupervisor::new(
            std::env::current_exe().unwrap(),
            listener,
            [2, 0, 0, 0, 0, 1],
        )
        .unwrap();
        supervisor.arguments = [
            "--exact",
            "supervisor::tests::supervisor_fixture_child",
            "--nocapture",
        ]
        .map(OsString::from)
        .into();
        supervisor.fixture = true;
        supervisor.fixture_exit_after_start = true;

        let (capability, driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        assert_eq!(
            supervisor.install_generation(capability.into_frame_fd()),
            Ok(1)
        );
        let first_exit = wait_for_exit(&mut supervisor);
        assert_eq!(first_exit.provider_generation, 1);

        supervisor.fixture_exit_after_start = false;
        assert_eq!(supervisor.restart_generation(), Ok(1));
        drop(driver);
        let restarted_exit = wait_for_exit(&mut supervisor);
        assert_eq!(restarted_exit.provider_generation, 1);
        assert!(restarted_exit.success);
        supervisor.terminate().unwrap();
    }

    #[test]
    fn wifi_capability_revocation_requires_a_fresh_generation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut supervisor = NetworkServiceSupervisor::new(
            std::env::current_exe().unwrap(),
            listener,
            [2, 0, 0, 0, 0, 1],
        )
        .unwrap();
        supervisor.arguments = [
            "--exact",
            "supervisor::tests::supervisor_fixture_child",
            "--nocapture",
        ]
        .map(OsString::from)
        .into();
        supervisor.fixture = true;
        supervisor.fixture_echo_frames = true;

        let (first, mut first_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        first_driver.set_link(true);
        assert_eq!(supervisor.install_generation(first.into_frame_fd()), Ok(1));
        let first_frame = [0xa5; 14];
        first_driver.deliver(&first_frame).unwrap();
        assert_eq!(wait_for_transmit(&mut first_driver).as_bytes(), first_frame);

        // Production Wi-Fi DOWN closes its driver-side peer. Once the child
        // observes that generation end, the supervisor must not restart the
        // retained-but-revoked endpoint.
        first_driver.set_link(false);
        drop(first_driver);
        assert_eq!(wait_for_exit(&mut supervisor).provider_generation, 1);
        assert_eq!(
            supervisor.restart_generation(),
            Err("network-service Ethernet generation is revoked".into())
        );

        let (second, mut second_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        second_driver.set_link(true);
        assert_eq!(supervisor.install_generation(second.into_frame_fd()), Ok(2));
        let second_frame = [0x5a; 14];
        second_driver.deliver(&second_frame).unwrap();
        assert_eq!(
            wait_for_transmit(&mut second_driver).as_bytes(),
            second_frame
        );
        supervisor.terminate().unwrap();
        wait_for_driver_closed(&mut second_driver);
    }

    #[test]
    fn lifecycle_receiver_installs_revokes_and_replaces_real_process_capabilities() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut supervisor = NetworkServiceSupervisor::new(
            std::env::current_exe().unwrap(),
            listener,
            [2, 0, 0, 0, 0, 1],
        )
        .unwrap();
        supervisor.arguments = [
            "--exact",
            "supervisor::tests::supervisor_fixture_child",
            "--nocapture",
        ]
        .map(OsString::from)
        .into();
        supervisor.fixture = true;
        supervisor.fixture_echo_frames = true;
        let (sender, receiver) = lifecycle_channel();
        let mut receiver = WifiLifecycleReceiver::new(receiver).unwrap();

        let (first, mut first_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        first_driver.set_link(true);
        let first = first.into_frame_fd();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 1),
            &[first.as_raw_fd()],
        );
        drop(first);
        assert!(matches!(
            receiver.receive(&mut supervisor).unwrap(),
            Some(WifiLifecycleUpdate::Installed {
                ethernet_generation: 1,
                provider_generation: 1,
                ..
            })
        ));
        first_driver.deliver(&[0x11; 14]).unwrap();
        assert_eq!(wait_for_transmit(&mut first_driver).as_bytes(), [0x11; 14]);

        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Revoke, 1),
            &[],
        );
        assert!(matches!(
            receiver.receive(&mut supervisor).unwrap(),
            Some(WifiLifecycleUpdate::Revoked {
                ethernet_generation: 1,
                ..
            })
        ));
        wait_for_driver_closed(&mut first_driver);

        let (stale, mut stale_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        stale_driver.set_link(true);
        let stale = stale.into_frame_fd();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 1),
            &[stale.as_raw_fd()],
        );
        drop(stale);
        assert_eq!(
            receiver.receive(&mut supervisor),
            Err("nonmonotonic Wi-Fi lifecycle generation".into())
        );
        wait_for_driver_closed(&mut stale_driver);

        let (second, mut second_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        second_driver.set_link(true);
        let second = second.into_frame_fd();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 2),
            &[second.as_raw_fd()],
        );
        drop(second);
        assert!(matches!(
            receiver.receive(&mut supervisor).unwrap(),
            Some(WifiLifecycleUpdate::Installed {
                ethernet_generation: 2,
                provider_generation: 2,
                ..
            })
        ));
        second_driver.deliver(&[0x22; 14]).unwrap();
        assert_eq!(wait_for_transmit(&mut second_driver).as_bytes(), [0x22; 14]);

        assert_eq!(
            unsafe { libc::shutdown(sender.as_raw_fd(), libc::SHUT_WR) },
            0
        );
        assert_eq!(
            wait_for_lifecycle_update(&mut receiver, &mut supervisor),
            WifiLifecycleUpdate::ChannelClosed
        );
        wait_for_driver_closed(&mut second_driver);
    }

    #[test]
    fn lifecycle_receiver_rejects_bad_records_and_ancillary_descriptors() {
        let supervisor = || {
            NetworkServiceSupervisor::new(
                "/not-spawned-for-invalid-message",
                TcpListener::bind("127.0.0.1:0").unwrap(),
                [2, 0, 0, 0, 0, 1],
            )
            .unwrap()
        };

        let (sender, receiver) = lifecycle_channel();
        let mut receiver = WifiLifecycleReceiver::new(receiver).unwrap();
        let mut service = supervisor();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 1),
            &[],
        );
        assert_eq!(
            receiver.receive(&mut service),
            Err("Wi-Fi lifecycle Install requires exactly one descriptor".into())
        );

        let (sender, receiver) = lifecycle_channel();
        let mut receiver = WifiLifecycleReceiver::new(receiver).unwrap();
        let mut service = supervisor();
        let (one, _one_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        let (two, _two_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        let one = one.into_frame_fd();
        let two = two.into_frame_fd();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 1),
            &[one.as_raw_fd(), two.as_raw_fd()],
        );
        assert_eq!(
            receiver.receive(&mut service),
            Err("Wi-Fi lifecycle Install requires exactly one descriptor".into())
        );

        let (sender, receiver) = lifecycle_channel();
        let mut receiver = WifiLifecycleReceiver::new(receiver).unwrap();
        let mut service = supervisor();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Revoke, 1)[..39],
            &[one.as_raw_fd()],
        );
        assert!(matches!(
            receiver.receive(&mut service),
            Err(error) if error.contains("InvalidLength")
        ));

        let (sender, receiver) = lifecycle_channel();
        let mut receiver = WifiLifecycleReceiver::new(receiver).unwrap();
        let mut service = supervisor();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 1),
            &[one.as_raw_fd(); 16],
        );
        assert_eq!(
            receiver.receive(&mut service),
            Err("truncated Wi-Fi lifecycle message or ancillary data".into())
        );

        let (sender, receiver) = lifecycle_channel();
        let credential = 1i32;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    receiver.as_raw_fd(),
                    1,
                    16,
                    (&credential as *const i32).cast(),
                    size_of::<i32>() as u32,
                )
            },
            0
        );
        let mut receiver = WifiLifecycleReceiver::new(receiver).unwrap();
        let mut service = supervisor();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Revoke, 1),
            &[],
        );
        assert_eq!(
            receiver.receive(&mut service),
            Err("unknown or malformed Wi-Fi lifecycle ancillary data".into())
        );

        let (sender, receiver) = lifecycle_channel();
        let mut receiver = WifiLifecycleReceiver::new(receiver).unwrap();
        let mut service = supervisor();
        let (stream, _peer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        let stream = unsafe { OwnedFd::from_raw_fd(stream.into_raw_fd()) };
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 1),
            &[stream.as_raw_fd()],
        );
        assert_eq!(
            receiver.receive(&mut service),
            Err("Wi-Fi supervisor capability must be AF_UNIX SOCK_SEQPACKET".into())
        );

        let (sender, receiver) = lifecycle_channel();
        let mut receiver = WifiLifecycleReceiver::new(receiver).unwrap();
        let mut service = supervisor();
        send_lifecycle(&sender, &[], &[]);
        assert!(matches!(
            receiver.receive(&mut service),
            Err(error) if error.contains("InvalidLength")
        ));

        let (sender, receiver) = lifecycle_channel();
        let mut receiver = WifiLifecycleReceiver::new(receiver).unwrap();
        let mut service = supervisor();
        let (zero_frame, mut zero_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        zero_driver.set_link(true);
        let zero_frame = zero_frame.into_frame_fd();
        send_lifecycle(&sender, &[], &[zero_frame.as_raw_fd()]);
        drop(zero_frame);
        assert_eq!(
            receiver.receive(&mut service),
            Err("zero-length Wi-Fi lifecycle record carried descriptors".into())
        );
        wait_for_driver_closed(&mut zero_driver);
    }

    #[test]
    fn lifecycle_receiver_replaces_an_active_generation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut supervisor = NetworkServiceSupervisor::new(
            std::env::current_exe().unwrap(),
            listener,
            [2, 0, 0, 0, 0, 1],
        )
        .unwrap();
        supervisor.arguments = [
            "--exact",
            "supervisor::tests::supervisor_fixture_child",
            "--nocapture",
        ]
        .map(OsString::from)
        .into();
        supervisor.fixture = true;
        supervisor.fixture_echo_frames = true;
        let (sender, receiver) = lifecycle_channel();
        let mut receiver = WifiLifecycleReceiver::new(receiver).unwrap();

        let (first, mut first_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        first_driver.set_link(true);
        let first = first.into_frame_fd();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 1),
            &[first.as_raw_fd()],
        );
        drop(first);
        receiver.receive(&mut supervisor).unwrap().unwrap();

        let (second, mut second_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        second_driver.set_link(true);
        let second = second.into_frame_fd();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 2),
            &[second.as_raw_fd()],
        );
        drop(second);
        assert!(matches!(
            receiver.receive(&mut supervisor).unwrap(),
            Some(WifiLifecycleUpdate::Installed {
                ethernet_generation: 2,
                provider_generation: 2,
                ..
            })
        ));
        wait_for_driver_closed(&mut first_driver);
        second_driver.deliver(&[0x44; 14]).unwrap();
        assert_eq!(wait_for_transmit(&mut second_driver).as_bytes(), [0x44; 14]);
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Revoke, 2),
            &[],
        );
        receiver.receive(&mut supervisor).unwrap().unwrap();

        let (third, mut third_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        third_driver.set_link(true);
        let third = third.into_frame_fd();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 3),
            &[third.as_raw_fd()],
        );
        drop(third);
        receiver.receive(&mut supervisor).unwrap().unwrap();
        drop(sender);
        assert_eq!(
            wait_for_lifecycle_update(&mut receiver, &mut supervisor),
            WifiLifecycleUpdate::ChannelClosed
        );
        wait_for_driver_closed(&mut third_driver);
    }

    #[test]
    fn lifecycle_receiver_commits_identity_and_closes_failed_install() {
        let (sender, receiver) = lifecycle_channel();
        let mut receiver = WifiLifecycleReceiver::new(receiver).unwrap();
        let mut supervisor = NetworkServiceSupervisor::new(
            "/network-service-does-not-exist",
            TcpListener::bind("127.0.0.1:0").unwrap(),
            [2, 0, 0, 0, 0, 1],
        )
        .unwrap();
        let (frame, mut driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        driver.set_link(true);
        let frame = frame.into_frame_fd();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 1),
            &[frame.as_raw_fd()],
        );
        drop(frame);
        assert!(matches!(
            receiver.receive(&mut supervisor),
            Err(error) if error.contains("spawn network service")
        ));
        wait_for_driver_closed(&mut driver);

        let (replay, _replay_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        let replay = replay.into_frame_fd();
        send_lifecycle(
            &sender,
            &lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 1),
            &[replay.as_raw_fd()],
        );
        assert_eq!(
            receiver.receive(&mut supervisor),
            Err("nonmonotonic Wi-Fi lifecycle generation".into())
        );

        let (different, _different_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        let different = different.into_frame_fd();
        let mut message = lifecycle_message(wifi_supervisor_wire::LifecycleKind::Install, 2);
        message[8..24].copy_from_slice(&[8; 16]);
        send_lifecycle(&sender, &message, &[different.as_raw_fd()]);
        assert_eq!(
            receiver.receive(&mut supervisor),
            Err("nonmonotonic Wi-Fi lifecycle generation".into())
        );
    }
    #[test]
    fn fresh_kernel_registration_survives_an_initial_fd3_open() {
        const INNER: &str = "DRV_NETWORK_SERVICE_SUPERVISOR_FD3_INNER";
        if std::env::var_os(INNER).is_none() {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "supervisor::tests::fresh_kernel_registration_survives_an_initial_fd3_open",
                    "--nocapture",
                ])
                .env(INNER, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }

        unsafe {
            libc::close(3);
        }
        let path = std::env::temp_dir().join(format!(
            "network-supervisor-registration-{}",
            std::process::id()
        ));
        let registration_guard = File::create(&path).unwrap();
        let mut supervisor = NetworkServiceSupervisor::new_kernel(
            std::env::current_exe().unwrap(),
            &path,
            [2, 0, 0, 0, 0, 1],
            None,
        )
        .unwrap();
        supervisor.arguments = [
            "--exact",
            "supervisor::tests::supervisor_fixture_child",
            "--nocapture",
        ]
        .map(OsString::from)
        .into();
        supervisor.fixture = true;

        let (frame, _driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        let frame = frame.into_frame_fd();
        let frame_pass = duplicate_capability(frame.as_raw_fd()).unwrap();
        drop(frame);
        drop(registration_guard);
        assert_eq!(unsafe { libc::fcntl(3, 1) }, -1);
        assert_eq!(supervisor.install_generation(frame_pass), Ok(1));
        supervisor.terminate().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn kernel_replacement_preserves_provider_namespace_and_unclaimed_socket() {
        if std::env::var_os("DRV_KERNEL_PROVIDER_ETHERNET_GUEST").is_none() {
            return;
        }
        let mut supervisor = NetworkServiceSupervisor::new_kernel(
            std::env::current_exe().unwrap(),
            "/dev/netstack3",
            [2, 0, 0, 0, 0, 1],
            None,
        )
        .unwrap();
        supervisor.arguments = [
            "--exact",
            "supervisor::tests::supervisor_fixture_child",
            "--nocapture",
        ]
        .map(OsString::from)
        .into();
        supervisor.fixture = true;

        let (first, _first_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        assert_eq!(supervisor.install_generation(first.into_frame_fd()), Ok(1));
        let stale = unsafe {
            libc::socket(
                libc::AF_INET,
                libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        };
        assert!(
            stale >= 0,
            "first provider session did not own application socket"
        );

        let first_pid = supervisor.kernel_process.as_ref().unwrap().child.id();
        let (second, _second_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        assert_eq!(supervisor.install_generation(second.into_frame_fd()), Ok(2));
        assert_eq!(
            supervisor.kernel_process.as_ref().unwrap().child.id(),
            first_pid,
            "Ethernet replacement changed the provider namespace generation"
        );

        let fresh = unsafe {
            libc::socket(
                libc::AF_INET,
                libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        };
        assert!(
            fresh >= 0,
            "replacement did not open a fresh provider session"
        );
        let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        address.sin_family = libc::AF_INET as _;
        assert_eq!(
            unsafe {
                libc::bind(
                    stale,
                    (&address as *const libc::sockaddr_in).cast(),
                    size_of::<libc::sockaddr_in>() as _,
                )
            },
            0,
            "replacement destroyed the existing provider socket namespace"
        );
        assert_eq!(unsafe { libc::close(stale) }, 0);
        assert_eq!(unsafe { libc::close(fresh) }, 0);
        supervisor.terminate().unwrap();
    }


    #[test]
    fn kernel_provider_stays_online_as_a_namespace_while_links_change() {
        let path = std::env::temp_dir().join(format!(
            "persistent-network-provider-registration-{}",
            std::process::id()
        ));
        let registration = File::create(&path).unwrap();
        let resolver_path = path.with_extension("resolver.sock");
        let resolver = std::os::unix::net::UnixListener::bind(&resolver_path).unwrap();
        let mut supervisor = NetworkServiceSupervisor::new_kernel(
            std::env::current_exe().unwrap(),
            &path,
            [2, 0, 0, 0, 0, 1],
            Some(resolver),
        )
        .unwrap();
        supervisor.arguments = [
            "--exact",
            "supervisor::tests::supervisor_fixture_child",
            "--nocapture",
        ]
        .map(OsString::from)
        .into();
        supervisor.fixture = true;
        supervisor.fixture_echo_frames = true;

        assert_eq!(supervisor.start_provider(), Ok(1));
        let provider_pid = supervisor.kernel_process.as_ref().unwrap().child.id();
        let (invalid, _peer) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        assert_eq!(
            supervisor.attach_generation(0, invalid.into_frame_fd()),
            Err("Ethernet generation must be nonzero".into())
        );
        assert_eq!(supervisor.kernel_process.as_ref().unwrap().child.id(), provider_pid);

        let (first, mut first_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        first_driver.set_link(true);
        assert_eq!(supervisor.attach_generation(1, first.into_frame_fd()), Ok(1));
        first_driver.deliver(&[0x31; 14]).unwrap();
        assert_eq!(wait_for_transmit(&mut first_driver).as_bytes(), [0x31; 14]);

        supervisor.revoke_generation(1).unwrap();
        assert_eq!(
            supervisor.kernel_process.as_ref().unwrap().child.id(),
            provider_pid
        );
        wait_for_driver_closed(&mut first_driver);

        let (second, mut second_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        second_driver.set_link(true);
        assert_eq!(supervisor.attach_generation(2, second.into_frame_fd()), Ok(1));
        assert_eq!(
            supervisor.kernel_process.as_ref().unwrap().child.id(),
            provider_pid
        );

        let (stale, mut stale_driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        stale_driver.set_link(true);
        assert_eq!(
            supervisor.attach_generation(2, stale.into_frame_fd()),
            Err("network provider rejected Ethernet generation".into())
        );
        wait_for_driver_closed(&mut stale_driver);
        second_driver.deliver(&[0x42; 14]).unwrap();
        assert_eq!(wait_for_transmit(&mut second_driver).as_bytes(), [0x42; 14]);

        supervisor.kernel_process.as_mut().unwrap().child.kill().unwrap();
        let exit = wait_for_exit(&mut supervisor);
        assert_eq!(exit.provider_generation, 1);
        assert_eq!(exit.ethernet_generation, Some(2));
        assert!(!exit.success);
        assert_eq!(supervisor.restart_generation(), Ok(2));
        assert_ne!(
            supervisor.kernel_process.as_ref().unwrap().child.id(),
            provider_pid
        );
        second_driver.deliver(&[0x53; 14]).unwrap();
        assert_eq!(wait_for_transmit(&mut second_driver).as_bytes(), [0x53; 14]);

        supervisor.terminate().unwrap();
        wait_for_driver_closed(&mut second_driver);
        drop(registration);
        std::fs::remove_file(path).unwrap();
        drop(supervisor);
        std::fs::remove_file(resolver_path).unwrap();
    }

    #[test]
    fn kernel_provider_bootstrap_waits_for_network_and_activates_service() {
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            child.write_all(b"READY").unwrap();
            let mut go = [0; 2];
            child.read_exact(&mut go).unwrap();
            assert_eq!(&go, b"GO");
            child.write_all(b"NETWORK_READY").unwrap();
            let mut serve = [0; 5];
            child.read_exact(&mut serve).unwrap();
            assert_eq!(&serve, b"SERVE");
        });
        bootstrap(&mut parent, true).unwrap();
        peer.join().unwrap();
    }
}
