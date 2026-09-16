// SPDX-License-Identifier: GPL-2.0-only

//! Trusted physical-lab launcher for the production WLAN service graph.
//!
//! This process creates capabilities and starts independently sandboxed
//! services. It owns no association policy and never reads a credential.

use drv_network_service::NetworkServiceSupervisor;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::mem::size_of;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const POLICY_FD: RawFd = 3;
const REGULATORY_FD: RawFd = 4;
const DRIVER_POLICY_FD: RawFd = 5;
const DRIVER_SUPERVISOR_FD: RawFd = 6;
const APPLICATION_FD: RawFd = 5;

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
static SUSPEND_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn request_stop(_: libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::Release);
}

extern "C" fn request_suspend(_: libc::c_int) {
    SUSPEND_REQUESTED.store(true, Ordering::Release);
}

fn install_signal_handlers() -> Result<(), String> {
    for (signal, handler) in [
        (libc::SIGINT, request_stop as *const () as usize),
        (libc::SIGTERM, request_stop as *const () as usize),
        (libc::SIGUSR1, request_suspend as *const () as usize),
    ] {
        if unsafe { libc::signal(signal, handler) } == libc::SIG_ERR {
            return Err(format!(
                "install launcher signal handler: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShutdownCause {
    Ordinary,
    SuspendPreparation,
}

fn requested_shutdown(now: Instant, deadline: Option<Instant>) -> Option<ShutdownCause> {
    if SUSPEND_REQUESTED.load(Ordering::Acquire) {
        Some(ShutdownCause::SuspendPreparation)
    } else if STOP_REQUESTED.load(Ordering::Acquire) || deadline.is_some_and(|deadline| now >= deadline) {
        Some(ShutdownCause::Ordinary)
    } else {
        None
    }
}

fn completion_marker(
    cause: Option<ShutdownCause>,
    succeeded: bool,
    network_revoked: bool,
) -> Option<&'static str> {
    match (cause, succeeded, network_revoked) {
        (Some(ShutdownCause::SuspendPreparation), true, true) => {
            Some("wlan_stack_suspend_ready=true hardware_stopped=true network_revoked=true")
        }
        (Some(ShutdownCause::Ordinary), true, true) => {
            Some("wlan_stack_driver_exit=0 hardware_stopped=true")
        }
        _ => None,
    }
}

fn main() {
    if let Err(error) = run() {
        let _ = writeln!(std::io::stderr(), "wlan_stack_kvm=REFUSED detail={error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    install_signal_handlers()?;
    let mut args = std::env::args().skip(1);
    let driver = PathBuf::from(args.next().ok_or_else(usage)?);
    let wlancfg_binary = PathBuf::from(args.next().ok_or_else(usage)?);
    let network_binary = PathBuf::from(args.next().ok_or_else(usage)?);
    let netcfg_binary = PathBuf::from(args.next().ok_or_else(usage)?);
    let state_directory = PathBuf::from(args.next().ok_or_else(usage)?);
    if args.next().is_some() {
        return Err(usage());
    }
    for binary in [&driver, &wlancfg_binary, &network_binary, &netcfg_binary] {
        if !binary.is_file() {
            return Err(format!(
                "service binary does not exist: {}",
                binary.display()
            ));
        }
    }

    let regulatory_fd = std::env::var("DRV_REGULATORY_DATABASE_FD")
        .map_err(|_| "DRV_REGULATORY_DATABASE_FD is required".to_string())?
        .parse::<RawFd>()
        .map_err(|_| "DRV_REGULATORY_DATABASE_FD is invalid".to_string())?;
    if regulatory_fd != REGULATORY_FD {
        return Err("physical launcher requires regulatory database on FD4".into());
    }
    if unsafe { libc::fcntl(regulatory_fd, libc::F_GETFD) } < 0 {
        return Err(format!(
            "inspect regulatory database: {}",
            std::io::Error::last_os_error()
        ));
    }

    let generation = generation()?;
    let generation_hex = generation
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mac = parse_mac(
        &std::env::var("DRV_SAE_CLIENT_MAC")
            .map_err(|_| "DRV_SAE_CLIENT_MAC is required".to_string())?,
    )?;
    // Continuous service by default; an explicit deadline is useful for KVM
    // qualification but is not a production lifetime/resource limit.
    let deadline = std::env::var("DRV_STACK_MAX_SECONDS").ok().map(|value| {
        let seconds = value.parse::<std::num::NonZeroU64>()
            .map_err(|_| "DRV_STACK_MAX_SECONDS must be positive")?;
        Instant::now().checked_add(Duration::from_secs(seconds.get()))
            .ok_or("DRV_STACK_MAX_SECONDS exceeds clock range")
    }).transpose()?;
    std::fs::create_dir_all("/run/drv").map_err(|error| format!("create /run/drv: {error}"))?;
    std::fs::create_dir_all(&state_directory)
        .map_err(|error| format!("create saved-network directory: {error}"))?;
    let (_application_lock, application) =
        bind_listener(Path::new("/run/drv/wlancfg.sock"), ListenerKind::Policy)?;
    // Standalone DNS owns NSS and wire DNS in the host deployment. Keep the
    // integrated endpoint for fixtures that do not launch the DNS service.
    let resolver = if std::env::var_os("DRV_EXTERNAL_DNS").is_some() {
        None
    } else {
        Some(bind_listener(Path::new(drv_dns_wire::PATH), ListenerKind::Resolver)?)
    };
    let (_resolver_lock, resolver) = match resolver {
        Some((lock, listener)) => (Some(lock), Some(std::os::unix::net::UnixListener::from(listener))),
        None => (None, None),
    };
    let integrated_resolver = resolver.is_some();
    let (_netcfg_lock, netcfg_listener) =
        bind_listener(Path::new(drv_network_service::netcfg::STATUS_PATH), ListenerKind::NetworkStatus)?;
    let state = File::open(&state_directory)
        .map_err(|error| format!("open saved-network directory: {error}"))?;
    if unsafe { libc::fchown(state.as_raw_fd(), 65534, 65534) } != 0 {
        return Err(format!(
            "assign saved-network directory to sandbox identity: {}",
            std::io::Error::last_os_error()
        ));
    }
    if unsafe { libc::fchmod(state.as_raw_fd(), 0o700) } != 0 {
        return Err(format!(
            "restrict saved-network directory permissions: {}",
            std::io::Error::last_os_error()
        ));
    }
    let (driver_policy, policy) = socket_pair()?;
    let (driver_supervisor, supervisor) = socket_pair()?;

    let mut driver_child = spawn_driver(
        &driver,
        &generation_hex,
        regulatory_fd,
        &driver_policy,
        &driver_supervisor,
    )?;
    let mut policy_child = match spawn_policy(
        &wlancfg_binary,
        &generation_hex,
        &policy,
        &state,
        &application,
    ) {
        Ok(child) => child,
        Err(error) => {
            drop(driver_policy);
            drop(policy);
            return wait_driver_after_policy_close(driver_child, error);
        }
    };
    drop((driver_policy, policy, driver_supervisor, application, state));

    let mut network =
        match NetworkServiceSupervisor::new_kernel(
            network_binary, "/dev/netstack3", mac, resolver,
        ) {
            Ok(network) => network,
            Err(error) => {
                let _ = policy_child.kill();
                let _ = policy_child.wait();
                return wait_driver_after_policy_close(driver_child, error);
            }
        };
    // The socket namespace exists before Wi-Fi produces an Ethernet link and
    // remains the same provider generation through ordinary link replacement.
    if let Err(error) = network.start_provider() {
        let _ = policy_child.kill();
        let _ = policy_child.wait();
        return wait_driver_after_policy_close(driver_child, error);
    }
    let mut netcfg_child = match spawn_netcfg(&netcfg_binary, &supervisor, &network, mac, &netcfg_listener) {
        Ok(child) => child,
        Err(error) => {
            let _ = policy_child.kill();
            let _ = policy_child.wait();
            // Keep Netstack and both lifecycle peers alive during hardware stop.
            return wait_driver_after_policy_close(driver_child, error);
        }
    };
    // Diagnostic output must not unwind past live hardware-owning children.
    let _ = writeln!(
        std::io::stdout(),
        "wlan_stack_launcher_ready=true policy=wlancfg driver=mt7921 network=netstack3-provider netcfg=netcfg-service resolver={integrated_resolver}"
    );

    let mut shutdown_cause = None;
    let mut shutdown_deadline = None;
    let mut policy_done = false;
    let mut driver_done = false;
    let mut network_revoked = false;
    let result = loop {
        match driver_child.try_wait() {
            Ok(Some(status)) => {
                driver_done = true;
                if status.success() && shutdown_cause.is_some() {
                    // The runtime has synchronously revoked its Ethernet peer
                    // and certified hardware containment. Revoke the external
                    // network capability now, before any completion marker.
                    if let Err(error) = network.terminate() {
                        break Err(format!("revoke network after driver stop: {error}"));
                    }
                    network_revoked = true;
                    let _ = writeln!(std::io::stdout(), "wlan_stack_shutdown=network_revoked");
                    break Ok(());
                }
                let policy_detail = match policy_child.try_wait() {
                    Ok(Some(policy_status)) => format!("; wlancfg status: {policy_status}"),
                    Ok(None) => String::new(),
                    Err(error) => format!("; wlancfg status unavailable: {error}"),
                };
                break Err(format!(
                    "MT7921 service exited unexpectedly: {status}{policy_detail}"
                ));
            }
            Ok(None) => {}
            Err(error) => break Err(format!("poll MT7921 service: {error}")),
        }

        if !policy_done {
            match policy_child.try_wait() {
                Ok(Some(status)) => {
                    policy_done = true;
                    if shutdown_cause.is_none() {
                        break Err(format!("wlancfg service exited unexpectedly: {status}"));
                    }
                }
                Ok(None) => {}
                Err(error) => break Err(format!("poll wlancfg service: {error}")),
            }
        }

        if shutdown_timed_out(shutdown_deadline) {
            break Err("MT7921 orderly shutdown timed out".into());
        }

        if shutdown_cause.is_none()
            && let Some(cause) = requested_shutdown(Instant::now(), deadline)
        {
            shutdown_cause = Some(cause);
            if let Err(error) = stop_netcfg(&mut netcfg_child) {
                break Err(error);
            }
            shutdown_deadline = Some(Instant::now() + Duration::from_secs(10));
            if !policy_done {
                if let Err(error) = policy_child.kill() {
                    break Err(format!("stop wlancfg service: {error}"));
                }
                let _ = policy_child.wait();
                policy_done = true;
            }
            let _ = writeln!(
                std::io::stdout(),
                "wlan_stack_shutdown=policy_closed cause={}",
                match cause {
                    ShutdownCause::Ordinary => "ordinary",
                    ShutdownCause::SuspendPreparation => "suspend-preparation",
                }
            );
            // Keep the network peer alive until policy EOF makes the driver
            // revoke its own Ethernet endpoint. Closing the peer first is a
            // protocol error to the callback bridge and can race orderly stop.
            // The network child is still terminated before certification.
        }

        if shutdown_cause.is_none() {
            match netcfg_child.try_wait() {
                Ok(Some(status)) => break Err(format!("netcfg exited unexpectedly: {status}")),
                Ok(None) => {}
                Err(error) => break Err(format!("poll netcfg: {error}")),
            }
        }
        match network.poll_exit() {
            Ok(Some(exit)) if shutdown_cause.is_none() => {
                break Err(format!(
                    "network-service generation {} exited success={}",
                    exit.provider_generation, exit.success
                ));
            }
            Ok(_) => {}
            Err(error) => break Err(error),
        }
        std::thread::sleep(Duration::from_millis(1));
    };

    let result = match stop_netcfg(&mut netcfg_child) {
        Ok(()) => result,
        Err(error) => Err(match result {
            Ok(()) => error,
            Err(original) => format!("{original}; {error}"),
        }),
    };
    let result = finish_children(
        &mut policy_child,
        policy_done,
        &mut driver_child,
        driver_done,
        &mut network,
        result,
    );
    if let Some(marker) = completion_marker(shutdown_cause, result.is_ok(), network_revoked) {
        // All children have been reaped; failed certificate delivery is an
        // ordinary error now, not a panic that can bypass containment.
        writeln!(std::io::stdout(), "{marker}")
            .map_err(|error| format!("write shutdown completion: {error}"))?;
    }
    result
}

fn usage() -> String {
    "usage: wlan-stack-kvm MT7921_DRIVER WLANCFG_SERVICE NETSTACK3_PROVIDER NETCFG_SERVICE STATE_DIRECTORY".into()
}

fn wait_driver_after_policy_close(mut driver: Child, original: String) -> Result<(), String> {
    match close_and_reap_driver(&mut driver) {
        Ok((detail, _)) => Err(format!("{original}; {detail}")),
        Err(error) => Err(format!("{original}; {error}")),
    }
}

fn shutdown_timed_out(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

fn close_and_reap_driver(driver: &mut Child) -> Result<(String, bool), String> {
    close_and_reap_driver_until(driver, Instant::now() + Duration::from_secs(10))
}

fn close_and_reap_driver_until(
    driver: &mut Child,
    deadline: Instant,
) -> Result<(String, bool), String> {
    loop {
        match driver.try_wait() {
            Ok(Some(status)) => {
                return Ok((
                    format!("MT7921 cleanup exit success={}", status.success()),
                    false,
                ));
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(None) => {
                driver
                    .kill()
                    .map_err(|error| format!("kill unresponsive MT7921 service: {error}"))?;
                let status = driver
                    .wait()
                    .map_err(|error| format!("reap killed MT7921 service: {error}"))?;
                return Ok((
                    format!(
                        "MT7921 orderly cleanup timed out; killed and reaped success={}",
                        status.success()
                    ),
                    true,
                ));
            }
            Err(error) => return Err(format!("poll MT7921 cleanup: {error}")),
        }
    }
}

fn finish_children(
    policy: &mut Child,
    policy_done: bool,
    driver: &mut Child,
    driver_done: bool,
    network: &mut NetworkServiceSupervisor,
    result: Result<(), String>,
) -> Result<(), String> {
    let mut cleanup = Vec::new();
    if !policy_done {
        let _ = policy.kill();
        if let Err(error) = policy.wait() {
            cleanup.push(format!("reap wlancfg service: {error}"));
        }
    }
    if !driver_done {
        match close_and_reap_driver(driver) {
            Ok((detail, forced)) if result.is_err() || forced => cleanup.push(detail),
            Ok(_) => {}
            Err(error) => cleanup.push(error),
        }
    }
    if let Err(error) = network.terminate() {
        cleanup.push(format!("terminate network provider: {error}"));
    }
    match (result, cleanup.is_empty()) {
        (Ok(()), true) => Ok(()),
        (Ok(()), false) => Err(cleanup.join("; ")),
        (Err(error), true) => Err(error),
        (Err(error), false) => Err(format!("{error}; {}", cleanup.join("; "))),
    }
}

fn generation() -> Result<[u8; 16], String> {
    use std::io::Read as _;
    let mut generation = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut generation))
        .map_err(|error| format!("create Wi-Fi generation identity: {error}"))?;
    if generation == [0; 16] {
        return Err("generated zero Wi-Fi identity".into());
    }
    Ok(generation)
}

fn parse_mac(value: &str) -> Result<[u8; 6], String> {
    let bytes = value
        .split(':')
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "DRV_SAE_CLIENT_MAC is invalid")?;
    let mac: [u8; 6] = bytes
        .try_into()
        .map_err(|_| "DRV_SAE_CLIENT_MAC is invalid")?;
    if mac == [0; 6] || mac[0] & 1 != 0 {
        return Err("DRV_SAE_CLIENT_MAC must be nonzero unicast".into());
    }
    Ok(mac)
}

fn socket_pair() -> Result<(OwnedFd, OwnedFd), String> {
    let mut fds = [-1; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    } != 0
    {
        return Err(format!(
            "create service capability: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

#[derive(Clone, Copy)]
enum ListenerKind {
    Policy,
    Resolver,
    NetworkStatus,
}

fn bind_listener(path: &Path, kind: ListenerKind) -> Result<(File, OwnedFd), String> {
    let (socket_type, permissions) = match kind {
        ListenerKind::Policy => (libc::SOCK_SEQPACKET, 0o600),
        ListenerKind::Resolver => (libc::SOCK_STREAM, 0o666),
        ListenerKind::NetworkStatus => (libc::SOCK_SEQPACKET, 0o666),
    };
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path.with_extension("lock"))
        .map_err(|error| format!("open local ownership lock: {error}"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(format!(
            "local listener already owned: {}",
            std::io::Error::last_os_error()
        ));
    }
    let encoded_path = CString::new(
        path.to_str()
            .ok_or("local socket path is not UTF-8")?,
    )
    .map_err(|_| "local socket path contains NUL")?;
    if encoded_path.as_bytes_with_nul().len() > 108 {
        return Err("local socket path is too long".into());
    }
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            socket_type | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(format!(
            "create local listener: {}",
            std::io::Error::last_os_error()
        ));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as _;
    for (destination, source) in address
        .sun_path
        .iter_mut()
        .zip(encoded_path.as_bytes_with_nul())
    {
        *destination = *source as libc::c_char;
    }
    let length = (size_of::<libc::sa_family_t>() + encoded_path.as_bytes_with_nul().len())
        as libc::socklen_t;
    // Keep the lock inode across launches. Only its current owner may remove
    // a stale socket; never unlink a live launcher's endpoint or another file.
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            // A policy child may still own the listener after its launcher
            // was killed. The lock alone does not prove that endpoint dead.
            let connected = unsafe {
                libc::connect(
                    fd.as_raw_fd(),
                    (&address as *const libc::sockaddr_un).cast(),
                    length,
                )
            };
            if connected == 0 {
                return Err("local listener is still live".into());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ECONNREFUSED) {
                return Err(format!("probe existing local listener: {error}"));
            }
            std::fs::remove_file(path)
                .map_err(|error| format!("remove stale local socket: {error}"))?;
        }
        Ok(_) => return Err("local socket path is not a socket".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect local socket: {error}")),
    }
    if unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length,
        )
    } != 0
    {
        return Err(format!(
            "bind local listener: {}",
            std::io::Error::last_os_error()
        ));
    }
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(permissions))
        .map_err(|error| format!("protect local listener: {error}"))?;
    if unsafe { libc::listen(fd.as_raw_fd(), 16) } != 0 {
        return Err(format!(
            "listen on local socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok((lock, fd))
}

fn duplicate(fd: RawFd) -> Result<OwnedFd, String> {
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 20) };
    if duplicate < 0 {
        return Err(format!(
            "duplicate launcher capability: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn spawn_driver(
    binary: &Path,
    generation: &str,
    regulatory: RawFd,
    policy: &OwnedFd,
    supervisor: &OwnedFd,
) -> Result<Child, String> {
    let regulatory = duplicate(regulatory)?;
    let policy = duplicate(policy.as_raw_fd())?;
    let supervisor = duplicate(supervisor.as_raw_fd())?;
    let inherited = [
        (regulatory.as_raw_fd(), REGULATORY_FD),
        (policy.as_raw_fd(), DRIVER_POLICY_FD),
        (supervisor.as_raw_fd(), DRIVER_SUPERVISOR_FD),
    ];
    let mut command = Command::new(binary);
    command
        .arg("--run-wifi-service")
        .env("DRV_WIFI_POLICY_FD", DRIVER_POLICY_FD.to_string())
        .env("DRV_WIFI_SUPERVISOR_FD", DRIVER_SUPERVISOR_FD.to_string())
        .env("DRV_WIFI_GENERATION", generation)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    unsafe {
        command.pre_exec(move || {
            for (source, target) in inherited {
                if libc::dup2(source, target) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command
        .spawn()
        .map_err(|error| format!("spawn MT7921 service: {error}"))
}

fn spawn_netcfg(
    binary: &Path,
    device: &OwnedFd,
    network: &NetworkServiceSupervisor,
    mac: [u8; 6],
    listener: &OwnedFd,
) -> Result<Child, String> {
    use std::io::Read as _;
    let device = duplicate(device.as_raw_fd())?;
    let (admin, generation) = network.configuration_capability()?;
    let (mut ready, child_ready) = std::os::unix::net::UnixStream::pair()
        .map_err(|e| format!("netcfg readiness channel: {e}"))?;
    ready.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    let ready_pass = duplicate(child_ready.as_raw_fd())?;
    let status_pass = duplicate(listener.as_raw_fd())?;
    let inherited = [(device.as_raw_fd(), 3), (admin.as_raw_fd(), 4),
        (ready_pass.as_raw_fd(), 5), (status_pass.as_raw_fd(), 6), (ready_pass.as_raw_fd(), 7)];
    let mut command = Command::new(binary);
    command.env_clear()
        .arg(mac.map(|v| format!("{v:02x}")).join(":"))
        .arg(generation.to_string())
        .stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    unsafe {
        command.pre_exec(move || {
            for (source, target) in inherited {
                if libc::dup2(source, target) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| format!("spawn netcfg: {e}"))?;
    drop((ready_pass, child_ready));
    let mut reply = [0; 5];
    if let Err(error) = ready.read_exact(&mut reply).and_then(|()| {
        if &reply == b"READY" { Ok(()) }
        else { Err(std::io::Error::other("invalid netcfg readiness")) }
    }) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("netcfg startup: {error}"));
    }
    Ok(child)
}

fn stop_netcfg(child: &mut Child) -> Result<(), String> {
    if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
        return if status.success() { Ok(()) }
            else { Err(format!("netcfg failed: {status}")) };
    }
    // Unlike a hardware owner, netcfg can be killed safely if its bounded
    // control operation fails to drain. Its peer capabilities remain held by
    // the launcher until Wi-Fi has stopped, preserving shutdown ordering.
    if unsafe { libc::kill(child.id() as i32, libc::SIGTERM) } != 0 {
        return Err(format!("stop netcfg: {}", std::io::Error::last_os_error()));
    }
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return if status.success() { Ok(()) }
                else { Err(format!("netcfg stop failed: {status}")) };
        }
        if Instant::now() >= deadline {
            child.kill().map_err(|e| e.to_string())?;
            child.wait().map_err(|e| e.to_string())?;
            return Err("netcfg stop timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_policy(
    binary: &Path,
    generation: &str,
    policy: &OwnedFd,
    state: &File,
    application: &OwnedFd,
) -> Result<Child, String> {
    let policy = duplicate(policy.as_raw_fd())?;
    let state = duplicate(state.as_raw_fd())?;
    let application = duplicate(application.as_raw_fd())?;
    let inherited = [
        (policy.as_raw_fd(), POLICY_FD),
        (state.as_raw_fd(), REGULATORY_FD),
        (application.as_raw_fd(), APPLICATION_FD),
    ];
    let mut command = Command::new(binary);
    command
        .args([
            POLICY_FD.to_string(),
            REGULATORY_FD.to_string(),
            APPLICATION_FD.to_string(),
            generation.to_string(),
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    unsafe {
        command.pre_exec(move || {
            for (source, target) in inherited {
                if libc::dup2(source, target) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command
        .spawn()
        .map_err(|error| format!("spawn wlancfg service: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn local_listeners_reclaim_stale_socket_without_replacing_live_owner() {
        for kind in [ListenerKind::Policy, ListenerKind::Resolver, ListenerKind::NetworkStatus] {
            let directory = std::env::temp_dir().join(format!(
                "wlan-{}-{:x}",
                std::process::id(),
                u64::from_le_bytes(generation().unwrap()[..8].try_into().unwrap()),
            ));
            std::fs::create_dir(&directory).unwrap();
            let path = directory.join("application.sock");
            let first = bind_listener(&path, kind).unwrap();
            assert!(bind_listener(&path, kind).is_err());
            assert!(path.exists());
            // Simulate a listener retained by a child after parent-lock release.
            assert_eq!(
                unsafe { libc::flock(first.0.as_raw_fd(), libc::LOCK_UN) },
                0
            );
            assert!(bind_listener(&path, kind).is_err());
            assert!(path.exists());
            drop(first);
            let second = bind_listener(&path, kind).unwrap();
            drop(second);
            std::fs::remove_file(&path).unwrap();
            std::fs::write(&path, b"not a socket").unwrap();
            assert!(bind_listener(&path, kind).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"not a socket");
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn driver_ignoring_policy_close_is_forced_and_cannot_be_success() {
        let mut driver = Command::new("sh")
            .args(["-c", "trap '' TERM; while :; do sleep 1; done"])
            .spawn()
            .unwrap();
        let shutdown_deadline = Some(Instant::now() + Duration::from_millis(20));
        while !shutdown_timed_out(shutdown_deadline) {
            assert!(driver.try_wait().unwrap().is_none());
            std::thread::sleep(Duration::from_millis(1));
        }
        let (detail, forced) = close_and_reap_driver_until(&mut driver, Instant::now()).unwrap();
        assert!(forced);
        assert!(detail.contains("timed out; killed and reaped success=false"));
        assert!(!driver.try_wait().unwrap().unwrap().success());
    }

    #[test]
    fn suspend_request_has_priority_and_deadline_is_ordinary_shutdown() {
        let now = Instant::now();
        STOP_REQUESTED.store(false, Ordering::Release);
        SUSPEND_REQUESTED.store(false, Ordering::Release);
        assert_eq!(requested_shutdown(now, Some(now + Duration::from_secs(1))), None);
        assert_eq!(requested_shutdown(now, None), None);
        assert_eq!(requested_shutdown(now, Some(now)), Some(ShutdownCause::Ordinary));
        STOP_REQUESTED.store(true, Ordering::Release);
        assert_eq!(
            requested_shutdown(now, Some(now + Duration::from_secs(1))),
            Some(ShutdownCause::Ordinary)
        );
        SUSPEND_REQUESTED.store(true, Ordering::Release);
        assert_eq!(
            requested_shutdown(now, Some(now + Duration::from_secs(1))),
            Some(ShutdownCause::SuspendPreparation)
        );
        STOP_REQUESTED.store(false, Ordering::Release);
        SUSPEND_REQUESTED.store(false, Ordering::Release);
    }

    #[test]
    fn failed_containment_never_authorizes_suspend() {
        assert_eq!(
            completion_marker(Some(ShutdownCause::SuspendPreparation), false, true),
            None
        );
        assert_eq!(
            completion_marker(Some(ShutdownCause::SuspendPreparation), true, true),
            Some("wlan_stack_suspend_ready=true hardware_stopped=true network_revoked=true")
        );
        assert_eq!(completion_marker(None, true, true), None);
        assert_eq!(
            completion_marker(Some(ShutdownCause::SuspendPreparation), true, false),
            None,
            "hardware success cannot certify suspend before network revocation"
        );
    }

    #[test]
    fn policy_child_has_no_ambient_target_or_credentials() {
        let source = include_str!("wlan-stack-kvm.rs");
        let spawn = source
            .split("fn spawn_policy(")
            .nth(1)
            .unwrap()
            .split("fn ")
            .next()
            .unwrap();
        let clear = spawn.find(".env_clear()").unwrap();
        let start = spawn.find(".stdin(Stdio::null())").unwrap();
        assert!(clear < start);
        assert!(!spawn.contains("DRV_SAE_CHANNEL"));
        assert!(!spawn.contains("DRV_SAE_BSSID"));
        assert!(!spawn.contains("credential"));
    }
}
