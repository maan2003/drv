// SPDX-License-Identifier: GPL-2.0-only

//! Diagnostic provisioning for one WPA2 or WPA3 saved network.
//!
//! The passphrase is accepted only on standard input. The non-secret command
//! line names an already-open wlancfg state-directory capability, the SSID,
//! and one of the two protected modes supported by this bounded diagnostic.

use anyhow::{Context as _, bail};
use futures::channel::mpsc;
use std::{
    fs::File,
    io::{self, Read},
    os::fd::{FromRawFd as _, OwnedFd, RawFd},
};
use wlancfg_selection::{
    client::types,
    config_management::{
        Credential, NetworkIdentifier, SavedNetworksManager, SavedNetworksManagerApi as _,
        SecurityType,
    },
    telemetry::{TelemetryEvent, TelemetrySender},
};

const MAX_SSID_BYTES: usize = 32;
const MIN_PASSPHRASE_BYTES: usize = 8;
const MAX_PASSPHRASE_BYTES: usize = 63;

fn main() -> anyhow::Result<()> {
    let (state_fd, network) = parse_args(std::env::args().skip(1))?;
    let credential = read_passphrase(io::stdin().lock())?;

    // SAFETY: this diagnostic takes ownership of the inherited descriptor
    // named by its caller. parse_args rejects standard-I/O descriptors.
    let state_directory = unsafe { OwnedFd::from_raw_fd(state_fd) };
    let (telemetry_tx, _telemetry_rx) = mpsc::channel::<TelemetryEvent>(8);
    let manager = futures::executor::block_on(SavedNetworksManager::new_with_directory(
        File::from(state_directory),
        TelemetrySender::new(telemetry_tx),
    ))
    .context("open wlancfg saved-network store")?;

    futures::executor::block_on(manager.store(network, Credential::Password(credential)))
        .map_err(|_| anyhow::anyhow!("store protected saved network"))?;
    Ok(())
}

fn parse_args(
    mut args: impl Iterator<Item = String>,
) -> anyhow::Result<(RawFd, NetworkIdentifier)> {
    let state_fd = args
        .next()
        .context("missing PERSISTENCE_DIRECTORY_FD")?
        .parse::<RawFd>()
        .context("invalid PERSISTENCE_DIRECTORY_FD")?;
    if state_fd < 3 {
        bail!("PERSISTENCE_DIRECTORY_FD must not alias standard I/O");
    }

    let ssid = args.next().context("missing SSID")?.into_bytes();
    if ssid.is_empty() || ssid.len() > MAX_SSID_BYTES {
        bail!("SSID length is outside 1..=32 bytes");
    }

    let security = match args.next().context("missing SECURITY")?.as_str() {
        "wpa2" => SecurityType::Wpa2,
        "wpa3" => SecurityType::Wpa3,
        _ => bail!("SECURITY must be wpa2 or wpa3"),
    };
    if args.next().is_some() {
        bail!("usage: wlancfg-provision-saved-network PERSISTENCE_DIRECTORY_FD SSID wpa2|wpa3");
    }

    Ok((
        state_fd,
        NetworkIdentifier::new(types::Ssid::from_bytes_unchecked(ssid), security),
    ))
}

fn read_passphrase(mut input: impl Read) -> anyhow::Result<Vec<u8>> {
    let mut passphrase = Vec::with_capacity(MAX_PASSPHRASE_BYTES);
    input
        .by_ref()
        .take((MAX_PASSPHRASE_BYTES + 1) as u64)
        .read_to_end(&mut passphrase)
        .context("read passphrase from standard input")?;
    if !(MIN_PASSPHRASE_BYTES..=MAX_PASSPHRASE_BYTES).contains(&passphrase.len()) {
        bail!("passphrase length is outside 8..=63 bytes");
    }
    Ok(passphrase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    struct FailingReader;

    impl io::Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("synthetic non-secret read failure"))
        }
    }

    #[test]
    fn arguments_accept_only_bounded_protected_networks() {
        for (mode, expected) in [("wpa2", SecurityType::Wpa2), ("wpa3", SecurityType::Wpa3)] {
            let (_, network) =
                parse_args(["7", "redwood-lab", mode].into_iter().map(str::to_owned)).unwrap();
            assert_eq!(network.security_type, expected);
            assert_eq!(network.ssid.to_vec(), b"redwood-lab");
        }

        for args in [
            vec!["0", "redwood-lab", "wpa3"],
            vec!["7", "", "wpa3"],
            vec!["7", "redwood-lab", "open"],
            vec!["7", "redwood-lab", "wpa3", "extra"],
        ] {
            assert!(parse_args(args.into_iter().map(str::to_owned)).is_err());
        }
        assert!(
            parse_args(
                ["7", &"x".repeat(33), "wpa3"]
                    .into_iter()
                    .map(str::to_owned)
            )
            .is_err()
        );
    }

    #[test]
    fn passphrase_reader_enforces_exact_wpa_bounds() {
        for length in [8, 63] {
            assert_eq!(
                read_passphrase(io::repeat(b'x').take(length))
                    .unwrap()
                    .len(),
                length as usize
            );
        }
        for length in [0, 7, 64, 65] {
            assert!(read_passphrase(io::repeat(b'x').take(length)).is_err());
        }
    }

    #[test]
    fn passphrase_never_appears_in_errors() {
        let secret =
            b"secret-too-long-because-this-input-is-deliberately-more-than-sixty-three-bytes";
        let error = read_passphrase(&secret[..]).unwrap_err().to_string();
        assert!(!error.contains(std::str::from_utf8(secret).unwrap()));

        let read_error = read_passphrase(FailingReader).unwrap_err().to_string();
        assert_eq!(read_error, "read passphrase from standard input");
    }
}
