// SPDX-License-Identifier: GPL-2.0-only
//! Namespace control transactions on the same capability as data and metadata.
use std::os::fd::{FromRawFd, OwnedFd};
use rustix::io::Errno;
pub(crate) const CLAIM: libc::c_ulong = 0x8008B501;
pub(crate) const READY: libc::c_ulong = 0xB502;
pub(crate) const CLAIM_CONTROL: libc::c_ulong = 0xB503;
pub(crate) const REVOKE: libc::c_ulong = 0xB505;

pub(crate) fn serve(
    view: &mut crate::rtnetlink::View,
    set_up: &mut impl FnMut(bool) -> Result<crate::rtnetlink::View, Errno>,
) -> Result<bool, Errno> {
    let mut progress = false;
    for _ in 0..32 {
        // The inherited serving capability owns this namespace, irrespective of
        // the worker's current namespace. Each returned FD owns one transaction.
        let fd = unsafe { libc::ioctl(3, CLAIM_CONTROL) };
        if fd < 0 {
            let error = Errno::from_raw_os_error(std::io::Error::last_os_error().raw_os_error().unwrap());
            if error == Errno::AGAIN { return Ok(progress); }
            return Err(error);
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut request = [0u8; 48];
        match rustix::io::read(&fd, &mut request) {
            Ok(48) => {}
            Err(Errno::NOENT) => continue, // Caller canceled while being claimed.
            Err(error) => return Err(error),
            _ => return Err(Errno::PROTO),
        }
        let reply = match view.ioctl(&request, set_up) {
            Ok(data) => { let mut reply = 0i32.to_le_bytes().to_vec(); reply.extend(data); reply }
            Err(error) => (-error.raw_os_error()).to_le_bytes().to_vec(),
        };
        match rustix::io::write(&fd, &reply) {
            Ok(count) if count == reply.len() => {}
            Err(Errno::NOENT) => {} // A completed operation need not outlive its caller.
            Err(error) => return Err(error),
            _ => return Err(Errno::PROTO),
        }
        progress = true;
    }
    Ok(progress)
}
