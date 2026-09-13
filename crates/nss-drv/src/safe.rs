//! Resolution policy and caller-buffer layout. No foreign pointer dereferences.
#![forbid(unsafe_code)]
use drv_dns_wire as wire;
use std::{
    mem::{MaybeUninit, align_of, size_of},
    net::IpAddr,
};
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Failure {
    pub status: i32,
    pub errno: i32,
    pub herrno: i32,
}
pub(crate) const INVALID: Failure = Failure {
    status: -1,
    errno: libc::EINVAL,
    herrno: 3,
};
pub(crate) const INTERNAL: Failure = Failure {
    status: -1,
    errno: libc::EIO,
    herrno: 3,
};
pub(crate) const RANGE: Failure = Failure {
    status: -2,
    errno: libc::ERANGE,
    herrno: 2,
};
pub(crate) struct Host {
    pub name: String,
    pub addresses: Vec<IpAddr>,
    pub family: i32,
}
pub(crate) fn resolve(name: &str, family: i32) -> Result<Host, Failure> {
    if family != libc::AF_INET && family != libc::AF_INET6 {
        return Err(Failure {
            status: -1,
            errno: libc::EAFNOSUPPORT,
            herrno: 4,
        });
    }
    if wire::request(name).is_none() {
        return Err(INVALID);
    }
    let addresses = match lookup(name) {
        Ok(Ok(addresses)) => addresses,
        Ok(Err(wire::NOT_FOUND)) => {
            return Err(Failure {
                status: 0,
                errno: 0,
                herrno: 1,
            });
        }
        Ok(Err(wire::TEMPORARY)) => {
            return Err(Failure {
                status: -2,
                errno: libc::EAGAIN,
                herrno: 2,
            });
        }
        Ok(Err(_)) => return Err(INTERNAL),
        Err(error) if matches!(error.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {
            return Err(Failure { status: -2, errno: libc::EAGAIN, herrno: 2 });
        }
        Err(error) => {
            return Err(Failure {
                errno: error.raw_os_error().unwrap_or(libc::EIO),
                ..INTERNAL
            });
        }
    };
    let addresses: Vec<_> = addresses
        .into_iter()
        .filter(|a| a.is_ipv4() == (family == libc::AF_INET))
        .collect();
    if addresses.is_empty() {
        return Err(Failure {
            status: 0,
            errno: 0,
            herrno: 4,
        });
    }
    Ok(Host {
        name: name.to_owned(),
        addresses,
        family,
    })
}
/// Offsets only. The FFI boundary creates pointers after all capacity checks succeed.
pub(crate) struct Layout {
    pub name: usize,
    pub aliases: usize,
    pub list: usize,
    pub addresses: Vec<usize>,
    pub family: i32,
    pub address_length: i32,
}
struct Buffer<'a> {
    bytes: &'a mut [MaybeUninit<u8>],
    used: usize,
}
impl Buffer<'_> {
    fn reserve(&mut self, len: usize, alignment: usize) -> Option<usize> {
        let base = self.bytes.as_ptr() as usize;
        let offset = (base.checked_add(self.used)?.checked_add(alignment - 1)? & !(alignment - 1))
            .checked_sub(base)?;
        let end = offset.checked_add(len)?;
        self.bytes.get_mut(offset..end)?;
        self.used = end;
        Some(offset)
    }
    fn copy(&mut self, bytes: &[u8]) -> Option<usize> {
        let offset = self.reserve(bytes.len(), 1)?;
        for (slot, byte) in self.bytes[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            slot.write(*byte);
        }
        Some(offset)
    }
}
pub(crate) fn pack(host: Host, buffer: &mut [MaybeUninit<u8>]) -> Result<Layout, Failure> {
    let mut b = Buffer {
        bytes: buffer,
        used: 0,
    };
    let mut name = host.name.into_bytes();
    name.push(0);
    let name = b.copy(&name).ok_or(RANGE)?;
    let aliases = b
        .reserve(size_of::<*mut u8>(), align_of::<*mut u8>())
        .ok_or(RANGE)?;
    let list = b
        .reserve(
            (host.addresses.len() + 1)
                .checked_mul(size_of::<*mut u8>())
                .ok_or(RANGE)?,
            align_of::<*mut u8>(),
        )
        .ok_or(RANGE)?;
    let mut addresses = Vec::with_capacity(host.addresses.len());
    for address in host.addresses {
        addresses.push(
            match address {
                IpAddr::V4(ip) => b.copy(&ip.octets()),
                IpAddr::V6(ip) => b.copy(&ip.octets()),
            }
            .ok_or(RANGE)?,
        );
    }
    Ok(Layout {
        name,
        aliases,
        list,
        addresses,
        family: host.family,
        address_length: if host.family == libc::AF_INET { 4 } else { 16 },
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn host() -> Host {
        Host {
            name: "example.test".into(),
            family: libc::AF_INET,
            addresses: vec!["192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap()],
        }
    }
    #[test]
    fn packing_checks_every_capacity_and_alignment() {
        assert_eq!(pack(host(), &mut []).err().unwrap(), RANGE);
        let mut full = [MaybeUninit::uninit(); 2048];
        assert!(pack(host(), &mut full).is_ok());
        for offset in 0..align_of::<*mut u8>() {
            for length in 0..100 {
                let mut bytes = [MaybeUninit::<u8>::uninit(); 128];
                let base = bytes[offset..].as_ptr() as usize;
                if let Ok(layout) = pack(host(), &mut bytes[offset..offset + length]) {
                    assert_eq!((base + layout.list) % align_of::<*mut u8>(), 0);
                    assert_eq!((base + layout.aliases) % align_of::<*mut u8>(), 0);
                    assert_eq!(layout.addresses.len(), 2);
                    assert!(layout.addresses.iter().all(|address| address + 4 <= length));
                }
            }
        }
    }
    #[test]
    fn unsupported_family_does_not_contact_service() {
        assert_eq!(
            resolve("example.test", libc::AF_UNIX).err().unwrap().errno,
            libc::EAFNOSUPPORT
        );
    }
}

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with},
};
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};
const TIMEOUT: Duration = Duration::from_secs(5);

fn lookup(name: &str) -> io::Result<Result<Vec<IpAddr>, u8>> {
    let mut request =
        wire::request(name).ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    let deadline = Instant::now() + TIMEOUT;
    // A full local backlog fails promptly with EAGAIN rather than blocking.
    let fd = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    connect(&fd, &SocketAddrUnix::new(wire::PATH)?)?;
    let mut stream = UnixStream::from(fd);
    let mut response = [0; wire::RESPONSE_LEN];
    for (buffer, writing) in [(&mut request[..], true), (&mut response[..], false)] {
        let mut offset = 0;
        while offset < buffer.len() {
            let result = if writing {
                stream.write(&buffer[offset..])
            } else {
                stream.read(&mut buffer[offset..])
            };
            match result {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "resolver disconnected",
                    ));
                }
                Ok(n) => offset += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    let timeout =
                        Timespec::try_from(left).map_err(|_| io::ErrorKind::InvalidInput)?;
                    let mut fds = [PollFd::new(
                        &stream,
                        if writing {
                            PollFlags::OUT
                        } else {
                            PollFlags::IN
                        },
                    )];
                    match poll(&mut fds, Some(&timeout)) {
                        Ok(0) => return Err(io::ErrorKind::TimedOut.into()),
                        Ok(_) | Err(rustix::io::Errno::INTR) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
        }
    }
    wire::addresses(&response).ok_or_else(|| io::ErrorKind::InvalidData.into())
}
