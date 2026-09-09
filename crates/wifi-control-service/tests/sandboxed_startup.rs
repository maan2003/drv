// SPDX-License-Identifier: GPL-2.0-only

use fidl_fuchsia_wlan_sme as sme;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};
use wifi_control_service::UnixSeqpacketEndpoint;
use wlan_control_wire::{Message, Packet, decode};

const GENERATION: [u8; 16] = [0x5a; 16];

fn pair() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1; 2];
    assert_eq!(
        unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        },
        0
    );
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

fn send(endpoint: &UnixSeqpacketEndpoint, packet: &Packet) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !endpoint.try_send_packet(packet, None).unwrap() {
        assert!(
            Instant::now() < deadline,
            "timed out queueing policy packet"
        );
        sleep(Duration::from_millis(1));
    }
}

fn read_queued(fd: &OwnedFd) -> Packet {
    let mut bytes = vec![0; wlan_control_wire::MAX_PACKET];
    let length = unsafe {
        libc::recv(
            fd.as_raw_fd(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            libc::MSG_DONTWAIT,
        )
    };
    assert!(
        length > 0,
        "prequeued policy packet was consumed before lockdown"
    );
    bytes.truncate(length as usize);
    decode(&bytes).unwrap()
}

#[test]
fn prequeued_policy_is_read_only_after_lockdown_and_only_three_capabilities_survive() {
    let (policy_client, policy_child) = pair();
    let (_supervisor_client, supervisor_child) = pair();
    let (_ethernet_client, ethernet_child) = pair();
    let policy = UnixSeqpacketEndpoint::from_inherited_fd(policy_client).unwrap();
    let request = Packet {
        generation: GENERATION,
        request_id: 41,
        message: Message::Scan(sme::ScanRequest::Passive(sme::PassiveScanRequest {
            channels: vec![],
        })),
    };
    // Queue input before exec: an EPERM exit can therefore prove setup did not read it.
    send(&policy, &request);

    let mut pipe_fds = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
        0
    );
    let pipe_reader = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
    let pipe_writer = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };

    let sources = [
        policy_child.as_raw_fd(),
        supervisor_child.as_raw_fd(),
        ethernet_child.as_raw_fd(),
    ];
    let unwanted = pipe_writer.as_raw_fd();
    let mut command = Command::new(env!("CARGO_BIN_EXE_wifi-control-simulated-service"));
    command
        .arg("5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            // Duplicate every source out of the 3..=5 target range first, so
            // installing one fixed fd cannot overwrite a later source.
            let mut temporary = [-1; 3];
            for (slot, source) in temporary.iter_mut().zip(sources) {
                *slot = libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 16);
                if *slot < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            for (source, target) in temporary.into_iter().zip(3..=5) {
                if libc::dup2(source, target) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(source);
            }
            // This descriptor deliberately survives exec. Only the sandbox's
            // exact close pass may remove it.
            let flags = libc::fcntl(unwanted, libc::F_GETFD);
            if flags < 0 || libc::fcntl(unwanted, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn sandboxed simulated service");
    drop(pipe_writer);

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut stdout = String::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut stdout)
                .unwrap();
            if status.code() == Some(77) {
                assert!(stdout.contains(
                    "wifi_simulated_service=SKIP reason=kernel_namespace_permission_denied"
                ));
                assert_eq!(read_queued(&policy_child), request);
                eprintln!(
                    "SKIP: kernel denied mount/network namespaces; pre-lockdown no-read invariant verified"
                );
                return;
            }
            let mut stderr = String::new();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("sandboxed service exited before Ready: {status}; {stderr}");
        }

        if let Some(received) = policy.try_receive_packet().unwrap() {
            assert!(matches!(received.packet.message, Message::Ready));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for post-lockdown Ready"
        );
        sleep(Duration::from_millis(1));
    }

    let mut byte = 0u8;
    assert_eq!(
        unsafe { libc::read(pipe_reader.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) },
        0,
        "an unretained descriptor remained open after sandbox setup"
    );
    let reply_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(received) = policy.try_receive_packet().unwrap()
            && matches!(received.packet.message, Message::ScanReply(ref reply) if reply.in_reply_to == 41)
        {
            break;
        }
        assert!(
            Instant::now() < reply_deadline,
            "prequeued request was not served after lockdown"
        );
        sleep(Duration::from_millis(1));
    }

    child.kill().unwrap();
    child.wait().unwrap();
}
