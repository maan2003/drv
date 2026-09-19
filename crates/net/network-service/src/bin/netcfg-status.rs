// SPDX-License-Identifier: GPL-2.0-only
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
fn main() -> Result<(), String> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as _;
    for (out, byte) in address
        .sun_path
        .iter_mut()
        .zip(drv_network_service::netcfg::STATUS_PATH.bytes())
    {
        *out = byte as _;
    }
    if unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&address) as _,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut poll, 1, 5000) } != 1 {
        return Err("netcfg status timed out".into());
    }
    let mut bytes = vec![0u8; 65536];
    let n = unsafe {
        libc::recv(
            fd.as_raw_fd(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            libc::MSG_TRUNC,
        )
    };
    if n <= 0 || n as usize > bytes.len() {
        return Err("invalid netcfg status packet".into());
    }
    let status: serde_json::Value =
        serde_json::from_slice(&bytes[..n as usize]).map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&status).map_err(|e| e.to_string())?
    );
    Ok(())
}
