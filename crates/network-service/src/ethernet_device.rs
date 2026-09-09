// SPDX-License-Identifier: GPL-2.0-only

use netstack3_port_spike::{
    EthernetDevice, EthernetDeviceEvent, EthernetEventSource, EthernetFrame,
};
use std::os::fd::{AsRawFd, OwnedFd};

const MSG_DONTWAIT: i32 = 0x40;
const MSG_TRUNC: i32 = 0x20;
const MSG_NOSIGNAL: i32 = 0x4000;
unsafe extern "C" {
    fn send(fd: i32, bytes: *const u8, len: usize, flags: i32) -> isize;
    fn recv(fd: i32, bytes: *mut u8, len: usize, flags: i32) -> isize;
}

pub(crate) struct ServiceEthernetDevice {
    fd: OwnedFd,
    mac_address: [u8; 6],
    initial_link_event: bool,
    link_closed: bool,
    link_close_notified: bool,
    receive_notified: bool,
    receive_ready: bool,
    transmit_ready: bool,
    transmit_blocked: bool,
}

impl ServiceEthernetDevice {
    /// Reconstructs the network side from its sole Ethernet frame capability.
    ///
    /// # Safety
    ///
    /// `fd` must be the nonblocking `SOCK_SEQPACKET` endpoint passed by the
    /// trusted launcher, with no other owner using it afterward.
    pub(crate) unsafe fn from_frame_fd(fd: OwnedFd, mac_address: [u8; 6]) -> Self {
        Self {
            fd,
            mac_address,
            initial_link_event: true,
            link_closed: false,
            link_close_notified: false,
            receive_notified: false,
            receive_ready: false,
            transmit_ready: false,
            transmit_blocked: false,
        }
    }

    pub(crate) fn mac_address(&self) -> [u8; 6] {
        self.mac_address
    }

    pub(crate) fn raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    pub(crate) fn wants_write(&self) -> bool {
        self.transmit_blocked
    }

    pub(crate) fn notify_epoll(&mut self, events: u32) {
        if events & (libc::EPOLLERR | libc::EPOLLHUP) as u32 != 0 {
            self.link_closed = true;
        }
        if events & libc::EPOLLIN as u32 != 0 {
            self.receive_ready = true;
        }
        if events & libc::EPOLLOUT as u32 != 0 {
            self.transmit_ready = true;
        }
    }
}

impl EthernetDevice for ServiceEthernetDevice {
    fn receive(&mut self) -> Option<EthernetFrame> {
        let mut bytes = [0u8; 1515];
        let received = loop {
            let received = unsafe {
                recv(
                    self.fd.as_raw_fd(),
                    bytes.as_mut_ptr(),
                    bytes.len(),
                    MSG_DONTWAIT | MSG_TRUNC,
                )
            };
            if received >= 0 {
                break received;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            self.receive_notified = false;
            if error.kind() != std::io::ErrorKind::WouldBlock {
                self.link_closed = true;
            }
            return None;
        };
        if received == 0 {
            self.link_closed = true;
            return None;
        }
        if received as usize > bytes.len() {
            return None;
        }
        EthernetFrame::copy_from_slice(&bytes[..received as usize]).ok()
    }

    fn transmit(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
        let bytes = frame.as_bytes();
        let sent = loop {
            let sent = unsafe {
                send(
                    self.fd.as_raw_fd(),
                    bytes.as_ptr(),
                    bytes.len(),
                    MSG_DONTWAIT | MSG_NOSIGNAL,
                )
            };
            if sent >= 0 {
                break sent;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            self.transmit_blocked = error.kind() == std::io::ErrorKind::WouldBlock;
            if !self.transmit_blocked {
                self.link_closed = true;
            }
            return Err(frame);
        };
        if sent == bytes.len() as isize {
            self.transmit_blocked = false;
            Ok(())
        } else {
            self.transmit_blocked = true;
            Err(frame)
        }
    }
}

impl EthernetEventSource for ServiceEthernetDevice {
    fn take_event(&mut self) -> Option<EthernetDeviceEvent> {
        if self.initial_link_event {
            self.initial_link_event = false;
            return Some(EthernetDeviceEvent::LinkStateChanged(true));
        }
        if self.link_closed {
            if self.link_close_notified {
                return None;
            }
            self.link_close_notified = true;
            return Some(EthernetDeviceEvent::LinkStateChanged(false));
        }
        if self.receive_ready && !self.receive_notified {
            self.receive_ready = false;
            self.receive_notified = true;
            return Some(EthernetDeviceEvent::ReceiveReady);
        }
        if self.transmit_ready && self.transmit_blocked {
            self.transmit_ready = false;
            self.transmit_blocked = false;
            return Some(EthernetDeviceEvent::TransmitReady);
        }
        None
    }
}
