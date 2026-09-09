// SPDX-License-Identifier: GPL-2.0-only

#[cfg(test)]
use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::os::fd::{AsRawFd, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const FRAME_FD: RawFd = 3;
const LISTENER_FD: RawFd = 4;
const BOOTSTRAP_FD: RawFd = 5;
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

fn bootstrap(stream: &mut std::os::unix::net::UnixStream) -> Result<(), String> {
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
    let mut started = [0; 7];
    stream
        .read_exact(&mut started)
        .map_err(|error| format!("network service STARTED: {error}"))?;
    if &started != b"STARTED" {
        return Err("invalid network-service STARTED".into());
    }
    Ok(())
}

/// An observed service-process exit, tagged with the revoked Ethernet generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkServiceProcessExit {
    pub generation: u64,
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

/// Trusted launcher for independently replaceable network-service generations.
///
/// Each installed frame capability starts a fresh sandboxed process. Replacing
/// it terminates the preceding process before transferring the new generation;
/// no Ethernet status or control messages cross the frame-only seam.
pub struct NetworkServiceSupervisor {
    binary: PathBuf,
    listener: TcpListener,
    listen: SocketAddr,
    mac_address: [u8; 6],
    next_generation: u64,
    installed: Option<InstalledGeneration>,
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
            listener,
            listen,
            mac_address,
            next_generation: 1,
            installed: None,
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
        self.start_installed()?;
        Ok(generation)
    }

    /// Restarts the process for the currently installed Ethernet generation.
    ///
    /// The caller must first observe the preceding process exit with
    /// [`Self::poll_exit`]. A restart retains the generation number because no
    /// new Ethernet capability has crossed the Wi-Fi service boundary.
    pub fn restart_generation(&mut self) -> Result<u64, String> {
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
        let listener = duplicate_capability(self.listener.as_raw_fd())?;
        let frame = duplicate_capability(installed.frame.as_raw_fd())?;
        let (mut bootstrap_parent, bootstrap_child) = std::os::unix::net::UnixStream::pair()
            .map_err(|error| format!("create network-service bootstrap: {error}"))?;
        let bootstrap_pass = duplicate_capability(bootstrap_child.as_raw_fd())?;
        let frame_fd = frame.as_raw_fd();
        let listener_fd = listener.as_raw_fd();
        let bootstrap_fd = bootstrap_pass.as_raw_fd();
        let mut command = Command::new(&self.binary);
        command
            .env_clear()
            .env(
                "DRV_SAE_CLIENT_MAC",
                self.mac_address
                    .map(|octet| format!("{octet:02x}"))
                    .join(":"),
            )
            .env("DRV_SOCKS5_LISTEN", self.listen.to_string())
            .env("DRV_NETSTACK_PARENT_PID", std::process::id().to_string())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        #[cfg(test)]
        {
            command.args(&self.arguments);
            if self.fixture {
                command.env("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE", "1");
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
                for (source, target) in [
                    (frame_fd, FRAME_FD),
                    (listener_fd, LISTENER_FD),
                    (bootstrap_fd, BOOTSTRAP_FD),
                ] {
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
        drop((frame, listener, bootstrap_pass, bootstrap_child));
        self.installed.as_mut().unwrap().running = Some(RunningProcess { child });
        if let Err(error) = bootstrap(&mut bootstrap_parent) {
            return match self.terminate_process() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; cleanup failed: {cleanup}")),
            };
        }
        Ok(())
    }

    pub fn poll_exit(&mut self) -> Result<Option<NetworkServiceProcessExit>, String> {
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
            generation: installed.generation,
            success: status.success(),
        };
        installed.running = None;
        Ok(Some(exit))
    }

    pub fn terminate(&mut self) -> Result<(), String> {
        self.terminate_process()?;
        self.installed = None;
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

impl Drop for NetworkServiceSupervisor {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::ErrorKind;
    use std::os::unix::net::UnixStream;
    use wlan_softmac_host::ethernet::{EthernetIngressError, ethernet_port};

    unsafe extern "C" {
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
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
        let mut bootstrap = unsafe { UnixStream::from_raw_fd(BOOTSTRAP_FD) };
        bootstrap.write_all(b"READY").unwrap();
        let mut go = [0; 2];
        bootstrap.read_exact(&mut go).unwrap();
        assert_eq!(&go, b"GO");
        bootstrap.write_all(b"STARTED").unwrap();
        drop(bootstrap);
        if std::env::var_os("DRV_NETWORK_SERVICE_SUPERVISOR_FIXTURE_EXIT").is_some() {
            return;
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
        bootstrap(&mut parent).unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn bootstrap_rejects_non_protocol_bytes() {
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        child.write_all(b"NOPE!").unwrap();
        assert_eq!(
            bootstrap(&mut parent),
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
            bootstrap(&mut parent),
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
        let waited = unsafe { waitpid(first_pid, std::ptr::null_mut(), 1) };
        assert_eq!(
            waited, -1,
            "replaced child remained waitable instead of being reaped"
        );
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(10));
        assert_eq!(
            first_driver.deliver(&[0; 14]),
            Err(EthernetIngressError::Closed),
            "old Ethernet generation remained open after replacement"
        );

        drop(second_driver);
        assert_eq!(wait_for_exit(&mut supervisor).generation, 2);
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
        assert_eq!(first_exit.generation, 1);

        supervisor.fixture_exit_after_start = false;
        assert_eq!(supervisor.restart_generation(), Ok(1));
        drop(driver);
        let restarted_exit = wait_for_exit(&mut supervisor);
        assert_eq!(restarted_exit.generation, 1);
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
        assert_eq!(wait_for_exit(&mut supervisor).generation, 1);
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
        assert_eq!(
            second_driver.deliver(&second_frame),
            Err(EthernetIngressError::Closed)
        );
    }
}
