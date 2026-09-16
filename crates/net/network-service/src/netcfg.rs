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

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

struct ProviderInterfaces {
    control: OwnedFd,
    mac: [u8; 6],
    provider_generation: u64,
}

impl ProviderInterfaces {
    fn completed(&self, generation: u64) -> Result<(), String> {
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
                return match link_control::receive_ack(self.control.as_fd(), generation)? {
                    link_control::AckStatus::Applied => Ok(()),
                    link_control::AckStatus::Rejected => Err("netstack rejected interface operation".into()),
                };
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
        link_control::send_attach(self.control.as_fd(), generation, frame.as_fd())?;
        self.completed(generation)?;
        Ok(self.provider_generation)
    }
    fn revoke(&mut self, generation: u64) -> Result<(), String> {
        link_control::send_detach(self.control.as_fd(), generation)?;
        self.completed(generation)
    }
}

/// Inherited private capabilities: Wi-Fi device events FD3, Netstack admin FD4,
/// readiness FD5. No hardware descriptors, credential storage or filesystem.
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
        .setup(&[3, 4, 5], None).map_err(|e| format!("netcfg setup: {e:?}"))?
        .lockdown(linux_self_sandbox::Profile::Netcfg {
            device_fd: 3, provider_fd: 4, readiness_fd: 5,
        }).map_err(|e| format!("netcfg lockdown: {e:?}"))?;
    sandbox.run(|| serve(mac, provider_generation))
}

fn serve(mac: [u8; 6], provider_generation: u64) -> Result<(), String> {
    // Validate endpoint metadata before consuming any service message.
    crate::lifecycle::validate_seqpacket(4)?;
    let mut devices = WifiLifecycleReceiver::new(unsafe { OwnedFd::from_raw_fd(3) })?;
    let mut provider = ProviderInterfaces {
        control: unsafe { OwnedFd::from_raw_fd(4) }, mac, provider_generation,
    };
    // Keep this slot occupied for the entire sandbox lifetime.
    let _readiness = unsafe { OwnedFd::from_raw_fd(5) };
    if unsafe { libc::write(5, b"READY".as_ptr().cast(), 5) } != 5 {
        return Err("netcfg readiness delivery failed".into());
    }
    while !STOP.load(Ordering::Relaxed) {
        match devices.receive(&mut provider)? {
            Some(update) => {
                eprintln!("netcfg_interface={update:?}");
                if matches!(update, crate::WifiLifecycleUpdate::ChannelClosed) {
                    return Ok(());
                }
            }
            None => {
                let mut descriptors = [
                    libc::pollfd { fd: 3, events: libc::POLLIN, revents: 0 },
                    libc::pollfd { fd: 4, events: 0, revents: 0 },
                ];
                let result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, 100) };
                if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                if descriptors[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                    return Err("netcfg provider exited".into());
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
                linux_self_sandbox::Profile::Netcfg { device_fd: 3, provider_fd: 4, readiness_fd: 5 }
            ).unwrap();
            let result = serve(mac, 1);
            if let Err(error) = &result { eprintln!("netcfg fixture: {error}"); }
            unsafe { libc::_exit(if result.is_ok() { 0 } else { 88 }) }
        }
        let (wifi, cfg_wifi) = pair();
        let (stack, cfg_stack) = pair();
        let (mut ready, cfg_ready) = std::os::unix::net::UnixStream::pair().unwrap();
        ready.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        // Sources above all targets prevent pre_exec remapping collisions.
        let sources = [cfg_wifi.as_raw_fd(), cfg_stack.as_raw_fd(), cfg_ready.as_raw_fd()]
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
        let mut bytes = [0; 5];
        ready.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"READY");
        for generation in 1..=16 {
            let (_driver, frame) = pair();
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
        drop(wifi);
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
        assert!(child.wait().unwrap().success());
    }
}
