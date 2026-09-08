// SPDX-License-Identifier: GPL-2.0-only

use netstack3_port_spike::{
    EthernetDevice, EthernetDeviceEvent, EthernetEventSource, EthernetFrame,
};
use std::os::fd::{AsRawFd, OwnedFd};

const MSG_DONTWAIT: i32 = 0x40;
const MSG_TRUNC: i32 = 0x20;
const MSG_NOSIGNAL: i32 = 0x4000;
const POLLIN: i16 = 0x001;
const POLLOUT: i16 = 0x004;
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
    fn send(fd: i32, bytes: *const u8, len: usize, flags: i32) -> isize;
    fn recv(fd: i32, bytes: *mut u8, len: usize, flags: i32) -> isize;
    fn poll(fds: *mut PollFd, count: usize, timeout_ms: i32) -> i32;
}

pub(crate) struct ServiceEthernetDevice {
    fd: OwnedFd,
    mac_address: [u8; 6],
    initial_link_event: bool,
    link_closed: bool,
    link_close_notified: bool,
    receive_notified: bool,
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
            transmit_blocked: false,
        }
    }

    pub(crate) fn mac_address(&self) -> [u8; 6] {
        self.mac_address
    }
}

impl EthernetDevice for ServiceEthernetDevice {
    fn receive(&mut self) -> Option<EthernetFrame> {
        let mut bytes = [0u8; 1515];
        let received = unsafe {
            recv(
                self.fd.as_raw_fd(),
                bytes.as_mut_ptr(),
                bytes.len(),
                MSG_DONTWAIT | MSG_TRUNC,
            )
        };
        if received < 0 {
            self.receive_notified = false;
            return None;
        }
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
        let sent = unsafe {
            send(
                self.fd.as_raw_fd(),
                bytes.as_ptr(),
                bytes.len(),
                MSG_DONTWAIT | MSG_NOSIGNAL,
            )
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
        let mut descriptor = PollFd {
            fd: self.fd.as_raw_fd(),
            events: POLLIN | if self.transmit_blocked { POLLOUT } else { 0 },
            revents: 0,
        };
        if unsafe { poll(&mut descriptor, 1, 0) } <= 0 {
            return None;
        }
        if descriptor.revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
            self.link_closed = true;
            self.link_close_notified = true;
            return Some(EthernetDeviceEvent::LinkStateChanged(false));
        }
        if descriptor.revents & POLLIN != 0 && !self.receive_notified {
            self.receive_notified = true;
            return Some(EthernetDeviceEvent::ReceiveReady);
        }
        if descriptor.revents & POLLOUT != 0 {
            self.transmit_blocked = false;
            return Some(EthernetDeviceEvent::TransmitReady);
        }
        None
    }
}
