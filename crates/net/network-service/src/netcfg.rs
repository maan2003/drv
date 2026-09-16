// SPDX-License-Identifier: GPL-2.0-only

//! Linux capability binding of Fuchsia netcfg's device introduction role.
//! See policy/netcfg/src/devices.rs: install a device through Netstack's
//! administration capability, separately from starting/containing drivers.
//! Wi-Fi credentials and protocol state remain in wlancfg and MLME.

use crate::lifecycle::{InterfaceInstaller, WifiLifecycleReceiver};
use crate::link_control;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const STATUS_PATH: &str = "/run/drv/netcfg.sock";
const MAX_CLIENTS: usize = 16;

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

struct ProviderInterfaces {
    control: OwnedFd,
    mac: [u8; 6],
    provider_generation: u64,
    active_link: Option<u64>,
    monitor: OwnedFd,
    last_link: Option<u64>,
    snapshot: Option<netstack3_port_integration::interfaces::InterfaceSnapshot>,
}

impl ProviderInterfaces {
    fn completed(&mut self, generation: u64) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("netcfg interface operation did not complete".into());
            }
            let mut descriptor = libc::pollfd {
                fd: self.control.as_raw_fd(), events: libc::POLLIN, revents: 0,
            };
            let result = unsafe {
                libc::poll(&mut descriptor, 1, remaining.as_millis().min(100) as i32)
            };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted { continue; }
                return Err(format!("netcfg interface wait: {error}"));
            }
            if descriptor.revents & libc::POLLIN != 0 {
                match link_control::receive_reply(self.control.as_fd())? {
                    Some(link_control::Reply::Interface(snapshot)) => self.snapshot = Some(snapshot),
                    Some(link_control::Reply::Ack { generation: received, status }) if received == generation => {
                        return match status {
                            link_control::AckStatus::Applied => Ok(()),
                            link_control::AckStatus::Rejected => Err("netstack rejected interface operation".into()),
                        };
                    }
                    None => {},
                    _ => return Err("mismatched interface acknowledgement".into()),
                }
            }
            if descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err("netcfg lost provider administration capability".into());
            }
        }
    }
}

impl InterfaceInstaller for ProviderInterfaces {
    fn mac_address(&self) -> [u8; 6] { self.mac }
    fn install(&mut self, generation: u64, frame: OwnedFd) -> Result<u64, String> {
        // Keep frame authority out of the dynamically accepted client range.
        // The fixed monitor slot permits only HUP observation and transfer.
        if unsafe { libc::dup3(frame.as_raw_fd(), self.monitor.as_raw_fd(), libc::O_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        drop(frame);
        link_control::send_attach(self.control.as_fd(), generation, self.monitor.as_fd())?;
        self.completed(generation)?;
        self.active_link = Some(generation);
        self.last_link = Some(generation);
        Ok(self.provider_generation)
    }
    fn revoke(&mut self, generation: u64) -> Result<(), String> {
        if self.active_link.is_none() && self.last_link == Some(generation) {
            return Ok(()); // Already acknowledged offline, not device removal.
        }
        if self.active_link != Some(generation) {
            return Err("netcfg revocation does not match installed link".into());
        }
        link_control::send_detach(self.control.as_fd(), generation)?;
        self.completed(generation)?;
        if unsafe { libc::dup3(5, self.monitor.as_raw_fd(), libc::O_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        self.active_link = None;
        Ok(())
    }
}

/// Inherited private capabilities: Wi-Fi device events FD3, Netstack admin FD4,
/// readiness FD5, read-only status listener FD6, reserved frame monitor FD7.
/// No hardware descriptors, credential storage or filesystem.
pub fn run(mac: [u8; 6], provider_generation: u64) -> Result<(), String> {
    if mac == [0; 6] || mac[0] & 1 != 0 || provider_generation == 0 {
        return Err("invalid netcfg interface identity".into());
    }
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = stop as *const () as usize;
    for signal in [libc::SIGTERM, libc::SIGINT] {
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    let sandbox = linux_self_sandbox::Sandbox::new()
        .setup(&[3, 4, 5, 6, 7], None).map_err(|e| format!("netcfg setup: {e:?}"))?
        .lockdown(linux_self_sandbox::Profile::Netcfg {
            device_fd: 3, provider_fd: 4, readiness_fd: 5, status_listener_fd: 6, monitor_fd: 7,
        }).map_err(|e| format!("netcfg lockdown: {e:?}"))?;
    sandbox.run(|| serve(mac, provider_generation))
}

fn serve(mac: [u8; 6], provider_generation: u64) -> Result<(), String> {
    // Validate endpoint metadata before consuming any service message.
    crate::lifecycle::validate_seqpacket(4)?;
    let mut devices = WifiLifecycleReceiver::new(unsafe { OwnedFd::from_raw_fd(3) })?;
    let mut provider = ProviderInterfaces {
        control: unsafe { OwnedFd::from_raw_fd(4) }, mac, provider_generation,
        active_link: None, last_link: None, snapshot: None,
        monitor: unsafe { OwnedFd::from_raw_fd(7) },
    };
    link_control::watch(provider.control.as_fd(), provider_generation)?;
    provider.completed(provider_generation)?;
    // Keep this slot occupied for the entire sandbox lifetime.
    let _readiness = unsafe { OwnedFd::from_raw_fd(5) };
    if unsafe { libc::write(5, b"READY".as_ptr().cast(), 5) } != 5 {
        return Err("netcfg readiness delivery failed".into());
    }
    let _listener = unsafe { OwnedFd::from_raw_fd(6) };
    let mut clients: Vec<(OwnedFd, Instant)> = Vec::new();
    let mut device_present = false;
    while !STOP.load(Ordering::Relaxed) {
        // Bound both accepted descriptors and time held by a client that does
        // not read. Every connection requests one current read-only snapshot.
        if clients.len() < MAX_CLIENTS {
            let fd = unsafe { libc::accept4(6, std::ptr::null_mut(), std::ptr::null_mut(),
                libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC) };
            if fd >= 0 {
                clients.push((unsafe { OwnedFd::from_raw_fd(fd) }, Instant::now() + Duration::from_secs(1)));
            } else {
                let error = std::io::Error::last_os_error();
                if !matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::EINTR | libc::EMFILE | libc::ENFILE)) {
                    return Err(format!("netcfg status accept: {error}"));
                }
            }
        }
        if !clients.is_empty() {
            let status = serde_json::to_vec(&serde_json::json!({
                "version": 1, "provider_generation": provider.provider_generation,
                "device_present": device_present, "link_online": provider.active_link.is_some(),
                "interface": provider.snapshot,
            })).map_err(|error| error.to_string())?;
            if status.len() > link_control::MAX_REPORT { return Err("netcfg status exceeds wire budget".into()); }
            clients.retain(|(fd, deadline)| {
                if Instant::now() >= *deadline { return false; }
                let result = unsafe { libc::sendto(fd.as_raw_fd(), status.as_ptr().cast(), status.len(),
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL, std::ptr::null(), 0) };
                result < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock
            });
        }
        match devices.receive(&mut provider)? {
            Some(update) => {
                device_present = matches!(update, crate::WifiLifecycleUpdate::Installed { .. });
                eprintln!("netcfg_interface={update:?}");
                if matches!(update, crate::WifiLifecycleUpdate::ChannelClosed) {
                    return Ok(());
                }
            }
            None => {
                let mut descriptors = vec![
                    libc::pollfd { fd: 3, events: libc::POLLIN, revents: 0 },
                    libc::pollfd { fd: 4, events: libc::POLLIN, revents: 0 },
                    libc::pollfd {
                        fd: provider.active_link.map_or(-1, |_| provider.monitor.as_raw_fd()),
                        events: 0, revents: 0,
                    },
                ];
                descriptors.push(libc::pollfd {
                    fd: if clients.len() < MAX_CLIENTS { 6 } else { -1 },
                    events: libc::POLLIN, revents: 0,
                });
                descriptors.extend(clients.iter().map(|(fd, _)| libc::pollfd {
                    fd: fd.as_raw_fd(), events: libc::POLLOUT, revents: 0,
                }));
                let result = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, 100) };
                if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                if descriptors[1].revents & libc::POLLIN != 0 {
                    match link_control::receive_reply(provider.control.as_fd())? {
                        Some(link_control::Reply::Interface(snapshot)) => provider.snapshot = Some(snapshot),
                        None => {},
                        _ => return Err("unsolicited interface acknowledgement".into()),
                    }
                }
                if descriptors[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                    return Err("netcfg provider exited".into());
                }
                if descriptors[2].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                    let generation = provider.active_link.unwrap();
                    provider.revoke(generation)?;
                    eprintln!("netcfg_link_offline={generation} device_present=true");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags};
    use std::io::{IoSlice, Read};
    use std::mem::MaybeUninit;
    use std::os::unix::process::CommandExt;

    fn pair() -> (OwnedFd, OwnedFd) {
        rustix::net::socketpair(rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::NONBLOCK | rustix::net::SocketFlags::CLOEXEC,
            None).unwrap()
    }

    #[test]
    fn filtered_netcfg_introduces_and_revokes_device_without_process_authority() {
        const TEST: &str = "netcfg::tests::filtered_netcfg_introduces_and_revokes_device_without_process_authority";
        let mac = [2, 0, 0, 0, 0, 1];
        if std::env::var_os("DRV_NETCFG_FIXTURE").is_some() {
            linux_self_sandbox::install_runtime_filter_for_integration_test(
                linux_self_sandbox::Profile::Netcfg { device_fd: 3, provider_fd: 4, readiness_fd: 5, status_listener_fd: 6, monitor_fd: 7 }
            ).unwrap();
            let result = serve(mac, 1);
            if let Err(error) = &result { eprintln!("netcfg fixture: {error}"); }
            unsafe { libc::_exit(if result.is_ok() { 0 } else { 88 }) }
        }
        let (wifi, cfg_wifi) = pair();
        let (stack, cfg_stack) = pair();
        let (mut ready, cfg_ready) = std::os::unix::net::UnixStream::pair().unwrap();
        ready.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let listener = rustix::net::socket_with(
            rustix::net::AddressFamily::UNIX, rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::NONBLOCK | rustix::net::SocketFlags::CLOEXEC, None).unwrap();
        let name = format!("drv-netcfg-test-{}", std::process::id());
        let address = rustix::net::SocketAddrUnix::new_abstract_name(name.as_bytes()).unwrap();
        rustix::net::bind(&listener, &address).unwrap();
        rustix::net::listen(&listener, 8).unwrap();
        // Sources above all targets prevent pre_exec remapping collisions.
        let sources = [cfg_wifi.as_raw_fd(), cfg_stack.as_raw_fd(), cfg_ready.as_raw_fd(), listener.as_raw_fd(), cfg_ready.as_raw_fd()]
            .map(|fd| unsafe { OwnedFd::from_raw_fd(libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10)) });
        let inherited = sources.iter().enumerate().map(|(i, fd)| (fd.as_raw_fd(), 3+i as i32))
            .collect::<Vec<_>>();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", TEST]).env("DRV_NETCFG_FIXTURE", "1");
        unsafe {
            command.pre_exec(move || {
                for &(source, target) in &inherited {
                    if libc::dup2(source, target) < 0 { return Err(std::io::Error::last_os_error()); }
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        drop((sources, cfg_ready, cfg_wifi, cfg_stack));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(request) = link_control::receive_request(stack.as_fd()).unwrap() {
                assert!(matches!(request, link_control::Request::Watch { generation: 1 }));
                link_control::send_ack(stack.as_fd(), 1, link_control::AckStatus::Applied).unwrap();
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut bytes = [0; 5];
        ready.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"READY");
        let client = rustix::net::socket_with(rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET, rustix::net::SocketFlags::CLOEXEC, None).unwrap();
        rustix::net::connect(&client, &address).unwrap();
        let mut poll = libc::pollfd { fd: client.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 5000) }, 1);
        let mut status = vec![0u8; link_control::MAX_REPORT];
        let n = unsafe { libc::recv(client.as_raw_fd(), status.as_mut_ptr().cast(), status.len(), 0) };
        assert!(n > 0);
        let status: serde_json::Value = serde_json::from_slice(&status[..n as usize]).unwrap();
        assert_eq!(status["link_online"], false);
        assert_eq!(status["device_present"], false);

        let mut drivers = Vec::new();
        for generation in 1..=16 {
            let (driver, frame) = pair();
            drivers.push(driver);
            let bytes = wifi_supervisor_wire::LifecycleMessage {
                kind: wifi_supervisor_wire::LifecycleKind::Install,
                wifi_generation: [1; 16], ethernet_generation: generation, mac_address: mac,
            }.encode();
            let fds = [frame.as_fd()];
            let mut storage = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut ancillary = SendAncillaryBuffer::new(&mut storage);
            assert!(ancillary.push(SendAncillaryMessage::ScmRights(&fds)));
            rustix::net::sendmsg(wifi.as_fd(), &[IoSlice::new(&bytes)], &mut ancillary,
                SendFlags::DONTWAIT | SendFlags::NOSIGNAL).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let request = loop {
                if let Some(request) = link_control::receive_request(stack.as_fd()).unwrap() { break request; }
                assert!(Instant::now() < deadline, "netcfg transfer timed out");
                std::thread::sleep(Duration::from_millis(1));
            };
            let link_control::Request::Attach { generation: received, frame: endpoint } = request else {
                panic!("expected interface attachment");
            };
            assert_eq!(received, generation);
            crate::lifecycle::validate_seqpacket(endpoint.as_raw_fd()).unwrap();
            link_control::send_ack(stack.as_fd(), generation, link_control::AckStatus::Applied).unwrap();
        }
        // Link loss is not device disappearance. Netcfg explicitly takes
        // the port offline while retaining the device identity/control seam.
        drop(drivers);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(request) = link_control::receive_request(stack.as_fd()).unwrap() {
                assert!(matches!(request, link_control::Request::Detach { generation: 16 }));
                link_control::send_ack(stack.as_fd(), 16, link_control::AckStatus::Applied).unwrap();
                break;
            }
            assert!(Instant::now() < deadline, "netcfg revocation timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(wifi); // Removing the already-offline device is idempotent.
        assert!(child.wait().unwrap().success());
    }
}
