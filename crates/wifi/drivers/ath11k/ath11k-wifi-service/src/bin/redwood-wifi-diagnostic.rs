// SPDX-License-Identifier: GPL-2.0-only

//! Operator-attended launcher for the explicitly unsandboxed Redwood association diagnostic.

use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::mem::{size_of, size_of_val};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use wifi_control_service::UnixSeqpacketEndpoint;
use wifi_supervisor_wire::{LifecycleKind, LifecycleMessage, MESSAGE_LEN};
use wlan_control_wire::{ConnectReply, Message, Packet, SessionValidator};

const DEADLINE: Duration = Duration::from_secs(90);

fn main() {
    if let Err(error) = run() {
        eprintln!("redwood_wifi_diagnostic=REFUSED detail={error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 7 || args.len() > 8 || args.get(7).is_some_and(|v| v != "--broker") {
        return Err("usage: redwood-wifi-diagnostic ATH11K_SERVICE VFIO_CDEV BOARD.BIN REGDB.BIN REGISTER_REGION MAC REMOTEPROC_DIRECTORY [--broker]".into());
    }
    let expected_mac = parse_mac(&args[5])?;
    let mut passphrase = Vec::with_capacity(63);
    std::io::stdin()
        .take(64)
        .read_to_end(&mut passphrase)
        .map_err(|e| format!("read transient passphrase: {e}"))?;
    if !(8..=63).contains(&passphrase.len()) {
        return Err("transient passphrase length is outside 8..=63 bytes".into());
    }
    let mut generation = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut generation))
        .map_err(|e| format!("create generation identity: {e}"))?;
    if generation == [0; 16] {
        return Err("invalid zero generation identity".into());
    }
    let generation_hex: String = generation
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let (wifi_policy, policy) = socket_pair()?;
    let (wifi_supervisor, supervisor) = socket_pair()?;

    let mut wifi_args = vec![generation_hex.clone()];
    wifi_args.extend(args[1..7].iter().cloned());
    if args.get(7).is_some() {
        wifi_args.push("--broker".into());
    }
    wifi_args.push("--diagnostic-unsandboxed".into());
    let mut wifi = spawn_with_fds(
        &args[0],
        &wifi_args,
        wifi_policy.as_raw_fd(),
        wifi_supervisor.as_raw_fd(),
    )?;
    drop(wifi_policy);
    drop(wifi_supervisor);
    let deadline = Instant::now() + DEADLINE;
    let policy = match UnixSeqpacketEndpoint::from_inherited_fd(policy) {
        Ok(policy) => policy,
        Err(error) => {
            return finish_wifi(
                wifi,
                format!("validate diagnostic policy endpoint: {error}"),
                false,
            );
        }
    };
    macro_rules! cleanup_try {
        ($result:expr) => {
            match $result {
                Ok(value) => value,
                Err(error) => {
                    drop(policy);
                    return finish_wifi(wifi, error, false);
                }
            }
        };
    }
    let mut validator = SessionValidator::new(generation);
    let ready = cleanup_try!(wait_policy(&policy, &mut validator, &mut wifi, deadline));
    if !matches!(ready, Message::Ready) {
        drop(policy);
        return finish_wifi(wifi, "first policy message was not Ready".into(), false);
    }
    cleanup_try!(send_policy(
        &policy,
        Packet {
            generation,
            request_id: 1,
            message: Message::Scan {
                deadline: cleanup_try!(
                    wlan_control_wire::MonotonicDeadline::after(Duration::from_secs(30))
                        .map_err(|e| format!("create operation deadline: {e}"))
                ),
                request: fidl_fuchsia_wlan_sme::ScanRequest::Passive(
                    fidl_fuchsia_wlan_sme::PassiveScanRequest {
                        channels: vec![149],
                    },
                )
            },
        },
        deadline,
    ));
    let scan = loop {
        match cleanup_try!(wait_policy(&policy, &mut validator, &mut wifi, deadline)) {
            Message::ScanReply(reply) if reply.in_reply_to == 1 => {
                match &reply.result {
                    Ok(results) => eprintln!(
                        "redwood_wifi_diagnostic=SCAN_REPLY request_id=1 success=true result_count={}",
                        results.len()
                    ),
                    Err(error) => eprintln!(
                        "redwood_wifi_diagnostic=SCAN_REPLY request_id=1 success=false error={error:?}"
                    ),
                }
                break cleanup_try!(
                    reply
                        .result
                        .map_err(|e| format!("channel-149 scan failed: {e:?}"))
                );
            }
            Message::GenerationEnd(reason) => {
                drop(policy);
                return finish_wifi(
                    wifi,
                    format!("Wi-Fi generation ended during scan: {reason:?}"),
                    false,
                );
            }
            _ => {}
        }
    };
    let bss = cleanup_try!(
        scan.into_iter()
            .filter(|result| {
                result.bss_description.primary.number == 149
                    && result.bss_description.primary.band
                        == fidl_fuchsia_wlan_ieee80211::WlanBand::FiveGhz
                    && ssid(&result.bss_description.ies) == Some(b"ajay")
                    && matches!(
                        &result.compatibility,
                        fidl_fuchsia_wlan_sme::Compatibility::Compatible(compatible)
                            if compatible.mutual_security_protocols.contains(
                                &fidl_fuchsia_wlan_internal::Protocol::Wpa3Personal
                            )
                    )
            })
            .max_by_key(|result| result.bss_description.rssi_dbm)
            .ok_or_else(|| "scan found no compatible WPA3 ajay BSS on channel 149".to_string())
    )
    .bss_description;
    cleanup_try!(send_policy(
        &policy,
        Packet {
            generation,
            request_id: 2,
            message: Message::Connect {
                deadline: cleanup_try!(
                    wlan_control_wire::MonotonicDeadline::after(Duration::from_secs(30))
                        .map_err(|e| format!("create operation deadline: {e}"))
                ),
                request: fidl_fuchsia_wlan_sme::ConnectRequest {
                    ssid: b"ajay".to_vec(),
                    bss_description: bss,
                    multiple_bss_candidates: false,
                    authentication: fidl_fuchsia_wlan_internal::Authentication {
                        protocol: fidl_fuchsia_wlan_internal::Protocol::Wpa3Personal,
                        credentials: Some(Box::new(fidl_fuchsia_wlan_internal::Credentials::Wpa(
                            fidl_fuchsia_wlan_internal::WpaCredentials::Passphrase(passphrase),
                        ))),
                    },
                    deprecated_scan_type: fidl_fuchsia_wlan_common::ScanType::Passive,
                }
            },
        },
        deadline,
    ));
    let mut connected = false;
    let mut ethernet = None;
    loop {
        if let Some(status) = cleanup_try!(
            wifi.try_wait()
                .map_err(|e| format!("inspect Wi-Fi service: {e}"))
        ) {
            return Err(format!("Wi-Fi service exited before association: {status}"));
        }
        if Instant::now() >= deadline {
            drop(policy);
            return finish_wifi(wifi, "association deadline expired".into(), false);
        }
        if let Some(received) = cleanup_try!(
            policy
                .try_receive_packet()
                .map_err(|e| format!("receive diagnostic policy response: {e}"))
        ) {
            cleanup_try!(
                validator
                    .validate(&received.packet)
                    .map_err(|e| format!("validate diagnostic policy response: {e}"))
            );
            match received.packet.message {
                Message::ConnectReply(reply) if reply.in_reply_to == 2 => {
                    let ConnectReply::Completed(result) = reply.result;
                    if result.code != fidl_fuchsia_wlan_ieee80211::StatusCode::Success {
                        drop(policy);
                        return finish_wifi(
                            wifi,
                            format!("WPA3 association failed with status {:?}", result.code),
                            false,
                        );
                    }
                    connected = true;
                }
                Message::GenerationEnd(reason) => {
                    drop(policy);
                    return finish_wifi(
                        wifi,
                        format!("Wi-Fi generation ended during association: {reason:?}"),
                        false,
                    );
                }
                _ => {}
            }
        }
        let update = match receive_lifecycle(supervisor.as_raw_fd()) {
            Ok(update) => update,
            Err(error) => {
                drop(policy);
                return finish_wifi(wifi, error, false);
            }
        };
        if let Some((message, descriptor)) = update {
            if message.wifi_generation != generation || message.mac_address != expected_mac {
                drop(policy);
                return finish_wifi(wifi, "lifecycle identity mismatch".into(), false);
            }
            match message.kind {
                LifecycleKind::Install => {
                    let Some(descriptor) = descriptor else {
                        drop(policy);
                        return finish_wifi(
                            wifi,
                            "Install omitted Ethernet descriptor".into(),
                            false,
                        );
                    };
                    if let Err(error) = validate_seqpacket(descriptor.as_raw_fd()) {
                        drop(policy);
                        return finish_wifi(wifi, error, false);
                    }
                    ethernet = Some(descriptor);
                }
                LifecycleKind::Revoke => {
                    drop(policy);
                    return finish_wifi(wifi, "lifecycle revoked before Install".into(), false);
                }
            }
        }
        if connected && ethernet.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let ethernet = ethernet.expect("association requires installed Ethernet generation");
    eprintln!(
        "redwood_wifi_diagnostic=ASSOCIATED protected_link=true ethernet_generation_retained=true"
    );
    // Association-only remains useful for ring diagnostics. The Internet
    // mode hands the existing capability to the production sandboxed Netstack3
    // supervisor; it does not create a Linux Wi-Fi interface or duplicate IP.
    let outcome = if let Some(binary) =
        std::env::var_os("REDWOOD_NETWORK_SERVICE").filter(|value| !value.is_empty())
    {
        let network_result = (|| -> Result<(), String> {
            let listener = std::net::TcpListener::bind("127.0.0.1:1080")
                .map_err(|error| format!("bind diagnostic SOCKS listener: {error}"))?;
            let mut network =
                drv_network_service::NetworkServiceSupervisor::new(binary, listener, expected_mac)?;
            network.install_generation(ethernet)?;
            eprintln!(
                "redwood_wifi_diagnostic=NETWORK_RUNNING socks5=127.0.0.1:1080 window_seconds=180"
            );
            let network_deadline = Instant::now() + Duration::from_secs(180);
            let result = loop {
                if let Some(exit) = network.poll_exit()? {
                    break Err(format!("network service exited: {exit:?}"));
                }
                if let Some(exit) = wifi.try_wait().map_err(|error| error.to_string())? {
                    break Err(format!("Wi-Fi exited during network proof: {exit}"));
                }
                if let Some(received) = policy
                    .try_receive_packet()
                    .map_err(|error| error.to_string())?
                {
                    validator
                        .validate(&received.packet)
                        .map_err(|error| error.to_string())?;
                    if let Message::GenerationEnd(reason) = received.packet.message {
                        break Err(format!(
                            "Wi-Fi generation ended during network proof: {reason:?}"
                        ));
                    }
                }
                if Instant::now() >= network_deadline {
                    break Ok(());
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            network.terminate()?;
            result
        })();
        network_result
    } else {
        drop(ethernet);
        Ok(())
    };
    drop(policy);
    match outcome {
        Ok(()) => finish_wifi(wifi, "diagnostic window completed".into(), true),
        Err(error) => finish_wifi(wifi, error, false),
    }
}

fn finish_wifi(wifi: Child, outcome: String, succeeded: bool) -> Result<(), String> {
    finish_wifi_with_warning(wifi, outcome, succeeded, Duration::from_secs(15))
}

fn finish_wifi_with_warning(
    mut wifi: Child,
    outcome: String,
    succeeded: bool,
    warning_after: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + warning_after;
    let mut warned = false;
    loop {
        if let Some(status) = wifi
            .try_wait()
            .map_err(|e| format!("inspect Wi-Fi cleanup: {e}"))?
        {
            if status.success() && succeeded {
                return Ok(());
            }
            if status.success() {
                return Err(outcome);
            }
            return Err(format!("{outcome}; Wi-Fi cleanup exited {status}"));
        }
        if !warned && Instant::now() >= deadline {
            eprintln!(
                "redwood_wifi_diagnostic=RETAINED detail={outcome}; orderly WPSS cleanup did not finish; launcher remains resident and manual recovery is required"
            );
            warned = true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_policy(
    policy: &UnixSeqpacketEndpoint,
    validator: &mut SessionValidator,
    wifi: &mut Child,
    deadline: Instant,
) -> Result<Message, String> {
    loop {
        if let Some(status) = wifi
            .try_wait()
            .map_err(|e| format!("inspect Wi-Fi service: {e}"))?
        {
            return Err(format!(
                "Wi-Fi service exited before policy reply: {status}"
            ));
        }
        if Instant::now() >= deadline {
            return Err("diagnostic policy response deadline expired".into());
        }
        if let Some(received) = policy
            .try_receive_packet()
            .map_err(|e| format!("receive diagnostic policy response: {e}"))?
        {
            validator
                .validate(&received.packet)
                .map_err(|e| format!("validate diagnostic policy response: {e}"))?;
            return Ok(received.packet.message);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn send_policy(
    policy: &UnixSeqpacketEndpoint,
    packet: Packet,
    deadline: Instant,
) -> Result<(), String> {
    while Instant::now() < deadline {
        if policy
            .try_send_packet(&packet, None)
            .map_err(|e| format!("send diagnostic policy request: {e}"))?
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("diagnostic policy send deadline expired".into())
}

fn ssid(mut ies: &[u8]) -> Option<&[u8]> {
    while ies.len() >= 2 {
        let length = usize::from(ies[1]);
        if ies.len() < length + 2 {
            return None;
        }
        if ies[0] == 0 {
            return Some(&ies[2..length + 2]);
        }
        ies = &ies[length + 2..];
    }
    None
}

fn socket_pair() -> Result<(OwnedFd, OwnedFd), String> {
    let mut fds = [-1; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    } != 0
    {
        return Err(format!(
            "create seqpacket capability: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn validate_seqpacket(fd: RawFd) -> Result<(), String> {
    let mut socket_type = 0i32;
    let mut len = size_of::<i32>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut socket_type as *mut i32).cast(),
            &mut len,
        )
    } != 0
        || socket_type != libc::SOCK_SEQPACKET
    {
        return Err("Ethernet capability is not SOCK_SEQPACKET".into());
    }
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || flags & libc::O_NONBLOCK == 0 {
        return Err("Ethernet capability is not nonblocking".into());
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
        return Err("Ethernet capability is not a connected Unix socket".into());
    }
    Ok(())
}

fn parse_mac(value: &str) -> Result<[u8; 6], String> {
    let parts: Vec<_> = value.split(':').collect();
    if parts.len() != 6 {
        return Err("MAC must contain six hexadecimal octets".into());
    }
    let mut mac = [0u8; 6];
    for (output, input) in mac.iter_mut().zip(parts) {
        *output = u8::from_str_radix(input, 16).map_err(|_| "invalid MAC")?;
    }
    Ok(mac)
}

fn spawn_with_fds(program: &str, args: &[String], fd3: RawFd, fd4: RawFd) -> Result<Child, String> {
    // The phone wrapper redirects stderr to its persistent diagnostic file.
    // Reopen it with synchronous data writes: inherited buffered file output
    // otherwise disappears when a device fault resets the phone.
    let log = OpenOptions::new()
        .append(true)
        .custom_flags(libc::O_DSYNC)
        .open("/proc/self/fd/2")
        .map_err(|e| format!("open synchronous diagnostic log: {e}"))?;
    let stdout = log
        .try_clone()
        .map_err(|e| format!("clone synchronous diagnostic log: {e}"))?;
    let mut command = Command::new(program);
    command.args(args).stdout(stdout).stderr(log);
    unsafe {
        command.pre_exec(move || {
            let source3 = libc::fcntl(fd3, libc::F_DUPFD_CLOEXEC, 5);
            let source4 = libc::fcntl(fd4, libc::F_DUPFD_CLOEXEC, 5);
            if source3 < 0
                || source4 < 0
                || libc::dup2(source3, 3) < 0
                || libc::dup2(source4, 4) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            libc::close(source3);
            libc::close(source4);
            Ok(())
        });
    }
    command.spawn().map_err(|e| format!("spawn {program}: {e}"))
}

// libc's msghdr::msg_controllen is usize on glibc but u32 on aarch64-musl.
#[allow(clippy::useless_conversion)]
fn receive_lifecycle(fd: RawFd) -> Result<Option<(LifecycleMessage, Option<OwnedFd>)>, String> {
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
    header.msg_controllen = size_of_val(&control)
        .try_into()
        .map_err(|_| "lifecycle ancillary buffer does not fit msghdr")?;
    let received =
        unsafe { libc::recvmsg(fd, &mut header, libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC) };
    if received < 0 {
        let error = std::io::Error::last_os_error();
        return if error.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(format!("receive lifecycle: {error}"))
        };
    }
    if received == 0 {
        return Err("Wi-Fi lifecycle channel closed".into());
    }
    if header.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err("truncated lifecycle record".into());
    }
    let message =
        LifecycleMessage::decode(&bytes[..received as usize]).map_err(|e| e.to_string())?;
    let mut descriptors = Vec::new();
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&header) };
    while !cmsg.is_null() {
        let current = unsafe { &*cmsg };
        let header_len = unsafe { libc::CMSG_LEN(0) as usize };
        let message_len = current.cmsg_len as usize;
        if current.cmsg_level != libc::SOL_SOCKET
            || current.cmsg_type != libc::SCM_RIGHTS
            || message_len < header_len
            || !(message_len - header_len).is_multiple_of(size_of::<RawFd>())
        {
            return Err("malformed lifecycle ancillary data".into());
        }
        let data = unsafe { libc::CMSG_DATA(cmsg).cast::<RawFd>() };
        for index in 0..(message_len - header_len) / size_of::<RawFd>() {
            descriptors.push(unsafe { OwnedFd::from_raw_fd(*data.add(index)) });
        }
        cmsg = unsafe { libc::CMSG_NXTHDR(&header, cmsg) };
    }
    let expected = usize::from(message.kind == LifecycleKind::Install);
    if descriptors.len() != expected {
        return Err("lifecycle descriptor cardinality mismatch".into());
    }
    Ok(Some((message, descriptors.pop())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_eof_allows_child_cleanup_without_forced_termination() {
        let (launcher_policy, child_policy) = socket_pair().unwrap();
        let (launcher_supervisor, child_supervisor) = socket_pair().unwrap();
        let marker =
            std::env::temp_dir().join(format!("redwood-wifi-cleanup-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let script = format!(
            "while IFS= read -r line <&3; do :; done; printf cleaned >{}",
            marker.display()
        );
        let mut command = Command::new("sh");
        command.args(["-c", &script]);
        let fd3 = child_policy.as_raw_fd();
        let fd4 = child_supervisor.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(fd3, 3) < 0 || libc::dup2(fd4, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let flags = libc::fcntl(3, libc::F_GETFL);
                if flags < 0 || libc::fcntl(3, libc::F_SETFL, flags & !libc::O_NONBLOCK) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        drop(child_policy);
        drop(child_supervisor);
        drop(launcher_policy);
        drop(launcher_supervisor);

        assert_eq!(
            finish_wifi_with_warning(
                child,
                "expected scan rejection".into(),
                false,
                Duration::from_millis(50),
            ),
            Err("expected scan rejection".into())
        );
        let mut contents = String::new();
        File::open(&marker)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "cleaned");
        std::fs::remove_file(marker).unwrap();
    }
}
