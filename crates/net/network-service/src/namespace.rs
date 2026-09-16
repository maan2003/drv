// SPDX-License-Identifier: GPL-2.0-only
//! Operations on the inherited, namespace-bound provider capability.
use std::os::fd::{AsRawFd, OwnedFd};
use netstack3_port_integration::service::DhcpService;
use rustix::io::Errno;

pub(crate) const CONTROL_FD: i32 = 11;
pub(crate) const CLAIM: libc::c_ulong = 0x8008B501;
pub(crate) const STATE: libc::c_ulong = 0x8010B502;
pub(crate) const ACK: libc::c_ulong = 0x4008B503;
pub(crate) const SET_UP: libc::c_ulong = 0x4004B504;
pub(crate) const REVOKE: libc::c_ulong = 0xB505;

pub(crate) struct Control { fd: OwnedFd, applied: Option<u64> }
impl Control {
    pub(crate) fn new(fd: OwnedFd) -> Self { Self { fd, applied: None } }
    pub(crate) fn fd(&self) -> &OwnedFd { &self.fd }
    /// Returns false if a concurrent interface change requires another pass.
    pub(crate) fn synchronize(&mut self, network: &mut DhcpService) -> Result<bool, Errno> {
        let mut state = [0u64; 2];
        // SAFETY: STATE writes its fixed 16-byte record into initialized storage.
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), STATE, state.as_mut_ptr()) } < 0 {
            return Err(Errno::from_raw_os_error(std::io::Error::last_os_error().raw_os_error().unwrap()));
        }
        let revision = state[0];
        if self.applied == Some(revision) { return Ok(true); }
        let flags = state[1] as u32;
        network.set_loopback_up(flags & libc::IFF_UP as u32 != 0);
        // ACK commits readiness only after the actual core operation above.
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), ACK, &revision) } < 0 {
            let error = Errno::from_raw_os_error(std::io::Error::last_os_error().raw_os_error().unwrap());
            if error == Errno::AGAIN { return Ok(false); }
            return Err(error);
        }
        self.applied = Some(revision);
        Ok(true)
    }
    pub(crate) fn set_up(&mut self, network: &mut DhcpService, up: bool) -> Result<(), Errno> {
        let value = u32::from(up);
        // SAFETY: SET_UP reads exactly one u32; authority comes from this object.
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), SET_UP, &value) } < 0 {
            return Err(Errno::from_raw_os_error(std::io::Error::last_os_error().raw_os_error().unwrap()));
        }
        for _ in 0..32 {
            if self.synchronize(network)? { return Ok(()); }
        }
        Err(Errno::AGAIN)
    }
}
