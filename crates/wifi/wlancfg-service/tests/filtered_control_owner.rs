// SPDX-License-Identifier: GPL-2.0-only

use futures::StreamExt as _;
use linux_self_sandbox::{Profile, install_runtime_filter_for_integration_test};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use wlan_control_wire::{Message, Packet, Reply, decode, encode};
use wlancfg_selection::mode_management::ClientSmeTransport as _;
use wlancfg_service::PreparedHostControlClient;

const GENERATION: [u8; 16] = [0x5a; 16];
const CHILD_CONTROL_FD: RawFd = 3;
const CHILD_STATE_FD: RawFd = 4;

#[test]
fn parked_owner_exchanges_control_under_fatal_filter_and_returns() {
    if std::env::var_os("DRV_FILTERED_CONTROL_OWNER_CHILD").is_some() {
        child().unwrap();
        return;
    }

    let (parent, child_socket) = socket_pair();
    let state_path =
        std::env::temp_dir().join(format!("drv-filtered-control-owner-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state_path);
    std::fs::create_dir(&state_path).unwrap();
    let state = File::open(&state_path).unwrap();
    let marker_path = state_path.join("ancillary-marker");
    let marker = File::create(&marker_path).unwrap();
    let marker_path = std::fs::canonicalize(marker_path).unwrap();
    let child_socket = duplicate_high(child_socket.as_raw_fd());
    let state = duplicate_high(state.as_raw_fd());
    let control_source = child_socket.as_raw_fd();
    let state_source = state.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg("parked_owner_exchanges_control_under_fatal_filter_and_returns")
        .arg("--nocapture")
        .env("DRV_FILTERED_CONTROL_OWNER_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(control_source, CHILD_CONTROL_FD) < 0
                || libc::dup2(state_source, CHILD_STATE_FD) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    let child_pid = child.id();
    drop((child_socket, state));

    // Queue liveness before the child confirms its parked owner and installs
    // seccomp. It must remain unread until the child activates after lockdown.
    send_with_fd(&parent, 1, Message::Ready, marker.as_raw_fd());
    let scan = receive(&parent);
    assert!(matches!(scan.message, Message::Scan { .. }));
    for entry in std::fs::read_dir(format!("/proc/{child_pid}/fd")).unwrap() {
        let target = std::fs::read_link(entry.unwrap().path()).unwrap();
        assert_ne!(target, marker_path, "SCM_RIGHTS descriptor was installed");
    }
    drop(marker);
    send(
        &parent,
        2,
        Message::ScanReply(Reply {
            in_reply_to: scan.request_id,
            result: Ok(Vec::new()),
        }),
    );
    drop(parent);
    let output = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&state_path);
    assert!(
        output.status.success(),
        "filtered wlancfg child failed: status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn child() -> anyhow::Result<()> {
    let control = unsafe { OwnedFd::from_raw_fd(CHILD_CONTROL_FD) };
    let state = unsafe { OwnedFd::from_raw_fd(CHILD_STATE_FD) };
    let state_raw = state.as_raw_fd();
    let parked = PreparedHostControlClient::from_inherited_socket(control, GENERATION)?
        .spawn_parked_after_setup()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    install_runtime_filter_for_integration_test(Profile::Wlancfg {
        control_fd: CHILD_CONTROL_FD,
        persistence_dir_fd: state_raw,
        application_listener_fd: None,
    })?;
    let client = parked.activate_after_persistence()?;
    let mut liveness = client.take_event_stream();
    runtime.block_on(async {
        liveness
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("liveness ended"))??;
        let result = client
            .scan(&fidl_fuchsia_wlan_sme::ScanRequest::Passive(
                fidl_fuchsia_wlan_sme::PassiveScanRequest { channels: vec![] },
            ))
            .await?;
        let result = result.map_err(|error| anyhow::anyhow!("scan failed: {error:?}"))?;
        if result.results.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("unexpected scan results"))
        }
    })
}

fn socket_pair() -> (OwnedFd, OwnedFd) {
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

fn duplicate_high(fd: RawFd) -> OwnedFd {
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 16) };
    assert!(copy >= 0);
    unsafe { OwnedFd::from_raw_fd(copy) }
}

fn send(fd: &OwnedFd, sequence: u64, message: Message) {
    let bytes = encode(&Packet {
        generation: GENERATION,
        request_id: sequence,
        message,
    })
    .unwrap();
    assert_eq!(
        unsafe { libc::send(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len(), 0) },
        bytes.len() as isize
    );
}

fn send_with_fd(fd: &OwnedFd, sequence: u64, message: Message, passed: RawFd) {
    let bytes = encode(&Packet { generation: GENERATION, request_id: sequence, message }).unwrap();
    let mut iov = libc::iovec { iov_base: bytes.as_ptr().cast_mut().cast(), iov_len: bytes.len() };
    let mut control = [0usize; 4];
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = &mut iov;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as _) as usize };
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&header);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as _) as usize;
        *libc::CMSG_DATA(cmsg).cast::<RawFd>() = passed;
    }
    assert_eq!(unsafe { libc::sendmsg(fd.as_raw_fd(), &header, libc::MSG_NOSIGNAL) }, bytes.len() as isize);
}

fn receive(fd: &OwnedFd) -> Packet {
    let mut bytes = [0; wlan_control_wire::MAX_PACKET];
    let received = unsafe { libc::recv(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    assert!(received > 0, "recv: {}", io::Error::last_os_error());
    decode(&bytes[..received as usize]).unwrap()
}
