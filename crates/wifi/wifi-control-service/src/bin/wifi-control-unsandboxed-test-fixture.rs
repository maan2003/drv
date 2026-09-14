// SPDX-License-Identifier: GPL-2.0-only

//! UNSANDBOXED TEST FIXTURE ONLY.
//!
//! This subprocess creates its own test Ethernet socketpair and deliberately
//! bypasses `linux-self-sandbox` so deterministic protocol tests can run under
//! outer kernels that deny namespace creation. It is not a service entrypoint
//! and holds only the policy, supervisor, and test-created Ethernet fds.
use std::os::fd::{FromRawFd, OwnedFd};
use wifi_control_service::{PreparedServer, SimulatedWifiRuntime};

fn main() {
    let fd: i32 = std::env::args()
        .nth(1)
        .expect("fd")
        .parse()
        .expect("numeric fd");
    let supervisor_fd: i32 = std::env::args()
        .nth(2)
        .expect("supervisor fd")
        .parse()
        .expect("numeric supervisor fd");
    let generation_byte: u8 = std::env::args()
        .nth(3)
        .expect("generation byte")
        .parse()
        .expect("generation byte");
    let control = unsafe { OwnedFd::from_raw_fd(fd) };
    let supervisor = unsafe { OwnedFd::from_raw_fd(supervisor_fd) };
    let mut ethernet = [-1; 2];
    assert_eq!(
        unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET,
                0,
                ethernet.as_mut_ptr(),
            )
        },
        0
    );
    let ethernet_service = unsafe { OwnedFd::from_raw_fd(ethernet[0]) };
    let _ethernet_driver = unsafe { OwnedFd::from_raw_fd(ethernet[1]) };
    let mut runtime = SimulatedWifiRuntime::new([2, 4, 6, 8, 10, 12]);
    runtime.publish_ethernet_after_connect(ethernet_service);
    futures::executor::block_on(
        PreparedServer::new(control, supervisor, [generation_byte; 16], runtime)
            .expect("validated control fd")
            .post_lockdown_open_complete()
            .expect("post-lockdown open")
            .run(),
    )
    .expect("simulated service loop");
}
