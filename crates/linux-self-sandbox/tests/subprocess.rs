// SPDX-License-Identifier: GPL-2.0-only

use std::os::fd::RawFd;
use std::process::{Command, Output};

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
