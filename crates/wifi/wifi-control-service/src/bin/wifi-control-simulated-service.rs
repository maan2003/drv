// SPDX-License-Identifier: GPL-2.0-only

//! Fail-closed, self-sandboxed simulated Wi-Fi service entrypoint.
//!
//! The trusted launcher donates exactly three connected `SOCK_SEQPACKET`
//! capabilities as fds 3 (policy), 4 (supervisor lifecycle), and 5 (Ethernet),
//! plus one opaque 16-byte generation as 32 hexadecimal characters in
//! argv[1]. There is no persistence,
//! device, physical-constructor, or ambient network lifecycle authority.

use linux_self_sandbox::{Error as SandboxError, Profile, Sandbox};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use wifi_control_service::{
    EndpointError, PreparedServer, ServiceError, SimulatedWifiRuntime, UnixSeqpacketEndpoint,
};

const POLICY_FD: i32 = 3;
const SUPERVISOR_FD: i32 = 4;
const ETHERNET_FD: i32 = 5;

#[derive(Debug)]
enum StartError {
    Generation,
    Endpoint(EndpointError),
    Sandbox(SandboxError),
    Service(ServiceError),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Generation => {
                f.write_str("expected one opaque generation as 32 hexadecimal characters")
            }
            Self::Endpoint(error) => write!(f, "inherited endpoint: {error}"),
            Self::Sandbox(error) => write!(f, "sandbox: {error}"),
            Self::Service(error) => write!(f, "service: {error}"),
        }
    }
}

fn main() {
    match start() {
        Ok(()) => {}
        Err(StartError::Sandbox(error)) if error.is_namespace_permission_denied() => {
            // Still fail closed. Exit 77 lets enforcement tests report an
            // explicit outer-kernel skip rather than a false sandbox claim.
            println!(
                "wifi_simulated_service=SKIP reason=kernel_namespace_permission_denied detail={error}"
            );
            std::process::exit(77);
        }
        Err(error) => {
            eprintln!("wifi_simulated_service=REFUSED detail={error}");
            std::process::exit(1);
        }
    }
}

fn start() -> Result<(), StartError> {
    // Decode the trusted launcher's opaque label; do not interpret its value.
    let generation = opaque_generation()?;
    let policy = unsafe { OwnedFd::from_raw_fd(POLICY_FD) };
    let supervisor = unsafe { OwnedFd::from_raw_fd(SUPERVISOR_FD) };
    let ethernet = unsafe { OwnedFd::from_raw_fd(ETHERNET_FD) };
    let retained = [
        policy.as_raw_fd(),
        supervisor.as_raw_fd(),
        ethernet.as_raw_fd(),
    ];

    // These checks use only getsockopt/getpeername/fcntl. PreparedServer has no
    // receive-capable method, and the donated Ethernet endpoint is not touched.
    let ethernet = UnixSeqpacketEndpoint::from_inherited_fd(ethernet)
        .map_err(StartError::Endpoint)?
        .into_owned_fd();
    let mut runtime = SimulatedWifiRuntime::new([2, 4, 6, 8, 10, 12]);
    runtime.publish_ethernet_after_connect(ethernet);
    let prepared = PreparedServer::new(policy, supervisor, generation, runtime)
        .map_err(StartError::Endpoint)?;

    let setup = Sandbox::new()
        .setup(&retained, None)
        .map_err(StartError::Sandbox)?;
    let locked = setup
        .lockdown(Profile::WifiSimulated)
        .map_err(StartError::Sandbox)?;
    locked.run(move || {
        futures::executor::block_on(
            prepared
                .post_lockdown_open_complete()
                .map_err(StartError::Service)?
                .run(),
        )
        .map_err(StartError::Service)
    })
}

fn opaque_generation() -> Result<[u8; 16], StartError> {
    let mut arguments = std::env::args();
    let _program = arguments.next();
    let encoded = arguments.next().ok_or(StartError::Generation)?;
    if arguments.next().is_some() {
        return Err(StartError::Generation);
    }
    if encoded.len() != 32 || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StartError::Generation);
    }
    let mut generation = [0; 16];
    for (output, pair) in generation
        .iter_mut()
        .zip(encoded.as_bytes().chunks_exact(2))
    {
        let pair = std::str::from_utf8(pair).map_err(|_| StartError::Generation)?;
        *output = u8::from_str_radix(pair, 16).map_err(|_| StartError::Generation)?;
    }
    Ok(generation)
}
