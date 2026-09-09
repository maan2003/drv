// SPDX-License-Identifier: GPL-2.0-only

use anyhow::{Context as _, bail};
use linux_self_sandbox::{Profile, Sandbox};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use wlancfg_service::{PreparedHostControlClient, policy::serve_one_generation};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let control_fd = parse_fd(args.next(), "CONTROL_SEQPACKET_FD")?;
    let state_fd = parse_fd(args.next(), "PERSISTENCE_DIRECTORY_FD")?;
    if control_fd == state_fd {
        bail!("control and persistence capabilities must be distinct");
    }
    let generation = parse_generation(args.next())?;
    if args.next().is_some() {
        bail!(
            "usage: wlancfg-service CONTROL_SEQPACKET_FD PERSISTENCE_DIRECTORY_FD GENERATION_HEX"
        );
    }

    // SAFETY: the launcher transfers unique ownership of each named inherited
    // descriptor. Validation below rejects the wrong control capability.
    let control_fd = unsafe { OwnedFd::from_raw_fd(control_fd) };
    // SAFETY: as above; SavedNetworksManager validates directory semantics
    // after lockdown, before using the capability.
    let state_fd = unsafe { OwnedFd::from_raw_fd(state_fd) };
    let control_raw = control_fd.as_raw_fd();
    let state_raw = state_fd.as_raw_fd();
    let prepared = PreparedHostControlClient::from_inherited_socket(control_fd, generation)
        .context("validate inherited WLAN control socket")?;

    // Production has no sandbox-bypass flag: inability to establish the jail
    // is a fatal startup error. Tests that cannot unshare use a separately
    // labelled integration-test process role.
    let setup = Sandbox::new()
        .setup(&[control_raw, state_raw], Some(state_raw))
        .context("establish wlancfg namespaces and capabilities")?;
    // Thread creation is setup-only. The owner blocks on a private start gate
    // and cannot poll or receive the policy socket before TSYNC lockdown.
    let parked = prepared
        .spawn_parked_after_setup()
        .context("park WLAN control owner before lockdown")?;
    let locked = setup
        .lockdown(Profile::Wlancfg {
            persistence_dir_fd: state_raw,
        })
        .context("install wlancfg seccomp policy")?;
    locked.run(|| serve_one_generation(parked, state_fd))
}

fn parse_fd(value: Option<String>, name: &str) -> anyhow::Result<RawFd> {
    let value = value.with_context(|| format!("missing {name}"))?;
    let fd: RawFd = value.parse().with_context(|| format!("invalid {name}"))?;
    if fd < 3 {
        bail!("{name} must not alias standard I/O");
    }
    Ok(fd)
}

fn parse_generation(value: Option<String>) -> anyhow::Result<[u8; 16]> {
    let value = value.context("missing GENERATION_HEX")?;
    if value.len() != 32 || !value.is_ascii() {
        bail!("GENERATION_HEX must contain exactly 32 hexadecimal digits");
    }
    let mut generation = [0; 16];
    for (index, byte) in generation.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .context("GENERATION_HEX contains a non-hexadecimal digit")?;
    }
    Ok(generation)
}
