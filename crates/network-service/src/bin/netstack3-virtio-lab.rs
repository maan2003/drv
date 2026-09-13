// SPDX-License-Identifier: GPL-2.0-only
//! Test-only QEMU stream-netdev Ethernet adapter; not a production driver.
//! The virtio port transports four-byte big-endian lengths and Ethernet frames.
//! Shell redirection installs fixed child capabilities without a Rust pre_exec hook.
#![forbid(unsafe_code)]
use std::{fs::OpenOptions, io::{self, Read, Write}, process::{Command, Stdio}, sync::{Arc, mpsc}, time::Duration};
use rustix::net::{socketpair, AddressFamily, SocketType, SocketFlags, SendFlags, RecvFlags};

fn run() -> io::Result<()> {
    let port = OpenOptions::new().read(true).write(true).open("/dev/vport0p1")?;
    let mut input = port.try_clone()?;
    let mut output = port;
    let (driver, service) = socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, SocketFlags::CLOEXEC, None)?;
    let driver = Arc::new(driver);
    let receiver = driver.clone();
    let mut child = Command::new("/bin/sh").args(["-c",
        "exec /bin/netstack3-provider --ethernet-mac 02:00:00:00:00:01 --resolver 4<&0 0</dev/null 3<>/dev/netstack3"])
        .stdin(Stdio::from(service)).spawn()?;
    let (failed, failures) = mpsc::channel();
    let incoming_failed = failed.clone();
    std::thread::spawn(move || {
        let result = (|| -> io::Result<()> {
        loop {
            let mut header = [0; 4];
            input.read_exact(&mut header)?;
            let length = u32::from_be_bytes(header) as usize;
            if !(14..=1514).contains(&length) { return Err(io::Error::other(format!("incoming frame length {length}"))); }
            let mut frame = [0; 1514];
            input.read_exact(&mut frame[..length])?;
            let sent = rustix::net::send(&*receiver, &frame[..length], SendFlags::NOSIGNAL)?;
            if sent != length { return Err(io::ErrorKind::WriteZero.into()); }
        }
        })();
        let _ = incoming_failed.send(result);
    });
    std::thread::spawn(move || {
        let result = (|| -> io::Result<()> {
        loop {
            let mut frame = [0; 1515];
            let (n, total) = rustix::net::recv(&*driver, &mut frame, RecvFlags::TRUNC)?;
            if n != total || !(14..=1514).contains(&n) { return Err(io::Error::other(format!("provider frame length {n}/{total}"))); }
            output.write_all(&(n as u32).to_be_bytes())?;
            output.write_all(&frame[..n])?;
        }
        })();
        let _ = failed.send(result);
    });
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() { Ok(()) } else { Err(io::Error::other(format!("provider exited: {status}"))) };
        }
        if let Ok(result) = failures.recv_timeout(Duration::from_millis(100)) {
            let exited = child.try_wait()?;
            if exited.is_none() { let _ = child.kill(); }
            let status = child.wait()?;
            return result.map_err(|error| io::Error::other(format!("{error}; provider status {status}")));
        }
    }
}
fn main() {
    if let Err(error) = run() { eprintln!("virtio Ethernet lab: {error}"); std::process::exit(1); }
}
