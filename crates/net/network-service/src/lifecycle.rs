// SPDX-License-Identifier: GPL-2.0-only

use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd as _, OwnedFd, RawFd};
use wifi_supervisor_wire::{LifecycleKind, LifecycleMessage, MESSAGE_LEN};

const SO_DOMAIN: i32 = 39;

fn socket_option(fd: RawFd, option: i32) -> Result<i32, String> {
    let mut value = 0i32;
    let mut len = size_of::<i32>() as u32;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            (&mut value as *mut i32).cast(),
            &mut len,
        )
    } != 0
        || len as usize != size_of::<i32>()
    {
        return Err(format!(
            "inspect Wi-Fi supervisor socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(value)
}

pub(crate) fn validate_seqpacket(fd: RawFd) -> Result<(), String> {
    if socket_option(fd, SO_DOMAIN)? != libc::AF_UNIX
        || socket_option(fd, libc::SO_TYPE)? != libc::SOCK_SEQPACKET
    {
        return Err("Wi-Fi supervisor capability must be AF_UNIX SOCK_SEQPACKET".into());
    }
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(format!(
            "inspect Wi-Fi supervisor capability flags: {}",
            std::io::Error::last_os_error()
        ));
    }
    if flags & libc::O_NONBLOCK == 0 {
        return Err("Wi-Fi supervisor capability must be nonblocking".into());
    }
    let mut peer: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut peer_len = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    if unsafe {
        libc::getpeername(
            fd,
            (&mut peer as *mut libc::sockaddr_storage).cast(),
            &mut peer_len,
        )
    } != 0
        || peer.ss_family as i32 != libc::AF_UNIX
    {
        return Err("Wi-Fi supervisor capability must be a connected Unix socket".into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WifiLifecycleUpdate {
    Installed {
        wifi_generation: [u8; 16],
        ethernet_generation: u64,
        provider_generation: u64,
    },
    Revoked {
        wifi_generation: [u8; 16],
        ethernet_generation: u64,
    },
    ChannelClosed,
}

struct ActiveGeneration {
    wifi: [u8; 16],
    ethernet: u64,
}

/// Device installation authority; deliberately excludes process and hardware
/// control. The legacy launcher and the private provider protocol implement
/// the same interface so configuration policy does not own either mechanism.
pub trait InterfaceInstaller {
    fn mac_address(&self) -> [u8; 6];
    fn install(&mut self, generation: u64, frame: OwnedFd) -> Result<u64, String>;
    fn revoke(&mut self, generation: u64) -> Result<(), String>;
}

impl InterfaceInstaller for crate::NetworkServiceSupervisor {
    fn mac_address(&self) -> [u8; 6] { self.mac_address() }
    fn install(&mut self, generation: u64, frame: OwnedFd) -> Result<u64, String> {
        self.attach_generation(generation, frame)
    }
    fn revoke(&mut self, generation: u64) -> Result<(), String> {
        self.revoke_generation(generation)
    }
}

/// Receiver for the dedicated, fd-bearing Wi-Fi lifecycle channel.
///
/// Policy IPC must never be passed here. Each seqpacket datagram is decoded by
/// `wifi-supervisor-wire`; only Install carries one Ethernet frame descriptor.
pub struct WifiLifecycleReceiver {
    channel: OwnedFd,
    active: Option<ActiveGeneration>,
    wifi_identity: Option<[u8; 16]>,
    last_ethernet: Option<u64>,
    closed: bool,
}

impl WifiLifecycleReceiver {
    pub fn new(channel: OwnedFd) -> Result<Self, String> {
        validate_seqpacket(channel.as_raw_fd())?;
        Ok(Self {
            channel,
            active: None,
            wifi_identity: None,
            last_ethernet: None,
            closed: false,
        })
    }

    pub fn receive(
        &mut self,
        supervisor: &mut impl InterfaceInstaller,
    ) -> Result<Option<WifiLifecycleUpdate>, String> {
        if self.closed {
            return Ok(None);
        }
        let mut bytes = [0u8; MESSAGE_LEN];
        let mut control = [0usize; 8];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = size_of_val(&control) as _;
        let received = unsafe {
            libc::recvmsg(
                self.channel.as_raw_fd(),
                &mut header,
                libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
            )
        };
        if received < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(format!("receive Wi-Fi lifecycle message: {error}"));
        }

        let mut descriptors = Vec::new();
        let mut ancillary_valid = true;
        let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&header) };
        while !cmsg.is_null() {
            let current = unsafe { &*cmsg };
            let header_len = unsafe { libc::CMSG_LEN(0) as usize };
            if (current.cmsg_len as usize) < header_len {
                ancillary_valid = false;
                break;
            }
            let data_len = current.cmsg_len as usize - header_len;
            if current.cmsg_level == libc::SOL_SOCKET
                && current.cmsg_type == libc::SCM_RIGHTS
                && data_len.is_multiple_of(size_of::<RawFd>())
            {
                let data = unsafe { libc::CMSG_DATA(cmsg).cast::<RawFd>() };
                for index in 0..data_len / size_of::<RawFd>() {
                    descriptors.push(unsafe { OwnedFd::from_raw_fd(*data.add(index)) });
                }
            } else {
                ancillary_valid = false;
            }
            cmsg = unsafe { libc::CMSG_NXTHDR(&header, cmsg) };
        }
        if header.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
            return Err("truncated Wi-Fi lifecycle message or ancillary data".into());
        }
        if !ancillary_valid {
            return Err("unknown or malformed Wi-Fi lifecycle ancillary data".into());
        }
        if received == 0 {
            if !descriptors.is_empty() {
                return Err("zero-length Wi-Fi lifecycle record carried descriptors".into());
            }
            let mut descriptor = libc::pollfd {
                fd: self.channel.as_raw_fd(),
                events: libc::POLLRDHUP,
                revents: 0,
            };
            if unsafe { libc::poll(&mut descriptor, 1, 0) } < 0 {
                return Err(format!(
                    "inspect Wi-Fi lifecycle channel closure: {}",
                    std::io::Error::last_os_error()
                ));
            }
            if descriptor.revents
                & (libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)
                == 0
            {
                return Err(wifi_supervisor_wire::WireError::InvalidLength.to_string());
            }
            if let Some(active) = self.active.take() {
                supervisor.revoke(active.ethernet)?;
            }
            self.closed = true;
            return Ok(Some(WifiLifecycleUpdate::ChannelClosed));
        }
        let message = LifecycleMessage::decode(&bytes[..received as usize])
            .map_err(|error| error.to_string())?;
        if message.mac_address != supervisor.mac_address() {
            return Err("Wi-Fi lifecycle MAC does not match network supervisor".into());
        }

        match message.kind {
            LifecycleKind::Install => {
                if descriptors.len() != 1 {
                    return Err("Wi-Fi lifecycle Install requires exactly one descriptor".into());
                }
                if message.wifi_generation == [0; 16]
                    || self
                        .wifi_identity
                        .is_some_and(|identity| message.wifi_generation != identity)
                    || message.ethernet_generation == 0
                    || self
                        .last_ethernet
                        .is_some_and(|last| message.ethernet_generation <= last)
                {
                    return Err("nonmonotonic Wi-Fi lifecycle generation".into());
                }
                let frame = descriptors.pop().unwrap();
                validate_seqpacket(frame.as_raw_fd())?;
                // Identity and replay protection describe the validated wire
                // record, not whether launching its sandboxed consumer works.
                // A failed launch must not permit replay or an identity swap.
                self.wifi_identity = Some(message.wifi_generation);
                self.last_ethernet = Some(message.ethernet_generation);
                // The persistent provider acknowledges revocation of any old
                // link and installation of this descriptor before ownership is
                // reported to Wi-Fi.
                let provider_generation = supervisor.install(
                    message.ethernet_generation,
                    frame,
                )?;
                self.active = Some(ActiveGeneration {
                    wifi: message.wifi_generation,
                    ethernet: message.ethernet_generation,
                });
                Ok(Some(WifiLifecycleUpdate::Installed {
                    wifi_generation: message.wifi_generation,
                    ethernet_generation: message.ethernet_generation,
                    provider_generation,
                }))
            }
            LifecycleKind::Revoke => {
                if !descriptors.is_empty() {
                    return Err("Wi-Fi lifecycle Revoke must not carry descriptors".into());
                }
                let Some(active) = self.active.as_ref() else {
                    return Err("Wi-Fi lifecycle Revoke has no active generation".into());
                };
                if active.wifi != message.wifi_generation
                    || active.ethernet != message.ethernet_generation
                {
                    return Err("Wi-Fi lifecycle Revoke does not match active generation".into());
                }
                supervisor.revoke(message.ethernet_generation)?;
                self.active = None;
                Ok(Some(WifiLifecycleUpdate::Revoked {
                    wifi_generation: message.wifi_generation,
                    ethernet_generation: message.ethernet_generation,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NetworkServiceSupervisor;

    #[test]
    fn receiver_rejects_unconnected_seqpacket_capability() {
        let fd = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        };
        assert!(fd >= 0);
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        assert!(matches!(
            WifiLifecycleReceiver::new(fd),
            Err(error) if error == "Wi-Fi supervisor capability must be a connected Unix socket"
        ));
    }
}
