// SPDX-License-Identifier: GPL-2.0-only

use std::os::fd::RawFd;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output, Stdio};

#[test]
fn setup_preserves_only_capabilities_without_consuming_them_and_lockdown_denies_ambient_syscalls() {
    let mut pair = [0; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, pair.as_mut_ptr()) },
        0
    );
    let mut unwanted = [0; 2];
    assert_eq!(unsafe { libc::pipe(unwanted.as_mut_ptr()) }, 0);
    clear_cloexec(pair[1]);
    clear_cloexec(unwanted[0]);
    assert_eq!(
        unsafe { libc::send(pair[0], b"X".as_ptr().cast(), 1, 0) },
        1
    );
    let output = helper("simulated", pair[1], Some(unwanted[0]));
    unsafe {
        libc::close(pair[0]);
        libc::close(pair[1]);
        libc::close(unwanted[0]);
        libc::close(unwanted[1]);
    }
    assert_helper(output, "profile=wifi-simulated");
}

#[test]
fn wlancfg_allows_only_directory_relative_atomic_persistence() {
    let path = std::env::temp_dir().join(format!("drv-sandbox-{}", std::process::id()));
    let sibling = path.with_file_name(format!("drv-sandbox-{}-sibling", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_dir_all(&sibling);
    std::fs::create_dir(&path).unwrap();
    std::fs::create_dir(&sibling).unwrap();
    std::fs::write(sibling.join("secret"), b"outside").unwrap();
    std::os::unix::fs::symlink(
        "../drv-sandbox-".to_owned() + &std::process::id().to_string() + "-sibling",
        path.join("escape"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
    let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    assert!(fd >= 0);
    clear_cloexec(fd);
    let output = helper("wlancfg", fd, None);
    unsafe {
        libc::close(fd);
    }
    std::fs::remove_file(path.join("escape")).unwrap();
    std::fs::remove_dir(&path).unwrap();
    std::fs::remove_dir_all(&sibling).unwrap();
    assert_helper(output, "profile=wlancfg");
}

#[test]
fn mt7921_vfio_subprocess_has_only_its_runtime_authority() {
    let parent_mount_namespace = std::fs::read_link("/proc/self/ns/mnt").unwrap();
    let parent_network_namespace = std::fs::read_link("/proc/self/ns/net").unwrap();
    let retained = [
        open_dev_null(),
        open_dev_null(),
        open_dev_null(),
        open_irq_eventfd(),
    ];
    for fd in retained {
        clear_cloexec(fd);
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_self-test-helper"))
        .env("SANDBOX_HELPER_MODE", "mt7921-vfio")
        .env(
            "SANDBOX_RETAINED_FDS",
            retained.map(|fd| fd.to_string()).join(","),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    for fd in retained {
        assert_eq!(unsafe { libc::close(fd) }, 0);
    }

    use std::io::{BufRead, Read, Write};
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut first_line = String::new();
    stdout.read_line(&mut first_line).unwrap();
    if first_line.contains("sandbox_self_test=READY") {
        let pid = child.id();
        let child_mount_namespace = std::fs::read_link(format!("/proc/{pid}/ns/mnt")).unwrap();
        let child_network_namespace = std::fs::read_link(format!("/proc/{pid}/ns/net")).unwrap();
        assert_ne!(child_mount_namespace, parent_mount_namespace);
        assert_ne!(child_network_namespace, parent_network_namespace);

        let root = format!("/proc/{pid}/root");
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o555
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        child.stdin.take().unwrap().write_all(b"X").unwrap();
    }

    let mut output = Output {
        status: child.wait().unwrap(),
        stdout: first_line.into_bytes(),
        stderr: Vec::new(),
    };
    stdout.read_to_end(&mut output.stdout).unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut output.stderr)
        .unwrap();
    assert_helper(
        output,
        "profile=mt7921-vfio namespaces_distinct=true sealed_empty_root=true uid=65534 gid=65534 effective_caps_empty=true permitted_caps_empty=true inheritable_caps_empty=true ambient_caps_empty=true bounding_caps_empty=true fds=stdio+4 no_new_privs=true seccomp=true allocator=true monotonic_sleep=true inherited_irq_eventfd=true",
    );
}

fn helper(mode: &str, retained: RawFd, unwanted: Option<RawFd>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_self-test-helper"));
    command
        .env("SANDBOX_HELPER_MODE", mode)
        .env("SANDBOX_RETAINED_FD", retained.to_string());
    if let Some(fd) = unwanted {
        command.env("SANDBOX_UNWANTED_FD", fd.to_string());
    }
    command.output().unwrap()
}

fn clear_cloexec(fd: RawFd) {
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
}

fn open_dev_null() -> RawFd {
    let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
    assert!(fd >= 0);
    fd
}

fn open_irq_eventfd() -> RawFd {
    let fd = unsafe { libc::eventfd(1, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(fd >= 0);
    fd
}

fn assert_helper(output: Output, expected: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(77) {
        assert!(
            stdout.contains("sandbox_self_test=SKIP reason=kernel_namespace_permission_denied"),
            "non-explicit skip: {stdout}"
        );
        eprintln!("{stdout}");
        return;
    }
    assert!(
        output.status.success(),
        "helper failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("sandbox_self_test=PASS"));
    assert!(stdout.contains(expected));
}
