// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

use futures::channel::mpsc;
use futures::executor::block_on;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use wlancfg_selection::client::types;
use wlancfg_selection::config_management::{
    Credential, NetworkConfigError, NetworkIdentifier, SavedNetworksManager,
    SavedNetworksManagerApi, SecurityType,
};
use wlancfg_selection::telemetry::{TelemetryEvent, TelemetrySender};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "drv-wlancfg-saved-networks-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn capability(&self) -> File {
        OpenOptions::new().read(true).open(&self.0).unwrap()
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn telemetry() -> TelemetrySender {
    let (sender, _receiver) = mpsc::channel::<TelemetryEvent>(8);
    TelemetrySender::new(sender)
}

fn id(ssid: &[u8], security_type: SecurityType) -> NetworkIdentifier {
    NetworkIdentifier::new(
        types::Ssid::from_bytes_unchecked(ssid.to_vec()),
        security_type,
    )
}

fn manager(directory: &TestDirectory) -> SavedNetworksManager {
    block_on(SavedNetworksManager::new_with_directory(
        directory.capability(),
        telemetry(),
    ))
    .unwrap()
}

#[test]
fn restart_preserves_store_replace_and_remove() {
    let directory = TestDirectory::new();
    let network = id(b"home", SecurityType::Wpa2);
    let first = Credential::Password(b"first-password".to_vec());
    let second = Credential::Password(b"second-password".to_vec());

    let saved = manager(&directory);
    assert!(
        block_on(saved.store(network.clone(), first.clone()))
            .unwrap()
            .is_none()
    );
    drop(saved);

    let saved = manager(&directory);
    let loaded = block_on(saved.lookup(&network)).unwrap();
    assert!(loaded.credential == first);
    let replaced = block_on(saved.store(network.clone(), second.clone()))
        .unwrap()
        .unwrap();
    assert!(replaced.credential == first);
    drop(saved);

    let saved = manager(&directory);
    assert!(block_on(saved.lookup(&network)).unwrap().credential == second);
    assert!(block_on(saved.remove(network.clone())).unwrap());
    drop(saved);

    let saved = manager(&directory);
    assert!(block_on(saved.lookup(&network)).is_none());
    assert_eq!(block_on(saved.known_network_count()), 0);
}

#[test]
fn successful_connect_mutation_is_durable() {
    let directory = TestDirectory::new();
    let network = id(b"durable", SecurityType::Wpa2);
    let credential = Credential::Password(b"durable-password".to_vec());
    let saved = manager(&directory);
    block_on(saved.store(network.clone(), credential.clone())).unwrap();
    block_on(saved.record_connect_result(
        network.clone(),
        &credential,
        types::Bssid::from([1, 2, 3, 4, 5, 6]),
        fidl_fuchsia_wlan_sme::ConnectResult {
            code: fidl_fuchsia_wlan_ieee80211::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        },
        types::ScanObservation::Passive,
    ));
    drop(saved);

    let loaded = block_on(manager(&directory).lookup(&network)).unwrap();
    assert!(loaded.has_ever_connected);
    assert_eq!(loaded.hidden_probability, 0.0);
}

#[test]
fn corrupt_truncated_and_oversized_files_are_rejected() {
    let directory = TestDirectory::new();
    let data = directory.path().join("saved-networks.v1");

    for bytes in [&b"not a saved network"[..], &b"DRVWIFI\0\x01"[..]] {
        std::fs::write(&data, bytes).unwrap();
        assert!(
            block_on(SavedNetworksManager::new_with_directory(
                directory.capability(),
                telemetry(),
            ))
            .is_err()
        );
    }

    let oversized = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data)
        .unwrap();
    oversized.set_len(128 * 1024 + 1).unwrap();
    assert!(
        block_on(SavedNetworksManager::new_with_directory(
            directory.capability(),
            telemetry(),
        ))
        .is_err()
    );
}

#[test]
fn pinned_manager_validation_and_compatible_lookup_are_preserved() {
    let directory = TestDirectory::new();
    let saved = manager(&directory);
    let ssid = types::Ssid::from_bytes_unchecked(b"policy".to_vec());
    let network = NetworkIdentifier::new(ssid.clone(), SecurityType::Wpa2);

    let invalid = block_on(saved.store(network.clone(), Credential::Password(b"short".to_vec())));
    assert!(matches!(invalid, Err(NetworkConfigError::PasswordLen)));
    block_on(saved.store(
        network.clone(),
        Credential::Password(b"valid-password".to_vec()),
    ))
    .unwrap();
    assert!(
        block_on(saved.store(
            network.clone(),
            Credential::Password(b"valid-password".to_vec()),
        ))
        .unwrap()
        .is_none()
    );

    let compatible =
        block_on(saved.lookup_compatible(&ssid, types::SecurityTypeDetailed::Wpa2Wpa3Personal));
    assert_eq!(compatible.len(), 1);
    assert!(compatible[0].credential == Credential::Password(b"valid-password".to_vec()));
}

#[test]
fn debug_output_never_contains_credentials() {
    let secret = "never-print-this-secret";
    let credential = Credential::Password(secret.as_bytes().to_vec());
    let credential_debug = format!("{credential:?}");
    assert!(!credential_debug.contains(secret));
    assert!(credential_debug.contains("redacted"));
    let facade = wlancfg_selection::fidl_fuchsia_wlan_policy::Credential::Password(
        secret.as_bytes().to_vec(),
    );
    assert!(!format!("{facade:?}").contains(secret));

    let directory = TestDirectory::new();
    let saved = manager(&directory);
    let private = id(b"private", SecurityType::Wpa2);
    block_on(saved.store(private.clone(), credential)).unwrap();
    assert!(!format!("{saved:?}").contains(secret));
    assert!(!format!("{:?}", block_on(saved.lookup(&private)).unwrap()).contains(secret));

    // Persistence errors expose only fixed category messages.
    let mut file = File::create(directory.path().join("saved-networks.v1")).unwrap();
    file.write_all(secret.as_bytes()).unwrap();
    drop(file);
    let error = block_on(SavedNetworksManager::new_with_directory(
        directory.capability(),
        telemetry(),
    ))
    .unwrap_err();
    assert!(!format!("{error:?} {error}").contains(secret));
}

#[test]
fn rejects_non_syncable_directory_capability() {
    let directory = TestDirectory::new();
    let path = std::ffi::CString::new(directory.path().as_os_str().as_bytes()).unwrap();
    // SAFETY: path is NUL-terminated and a successful descriptor is transferred
    // exactly once to File.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_DIRECTORY) };
    assert!(fd >= 0);
    // SAFETY: libc::open returned a new owned descriptor.
    let capability = unsafe { File::from_raw_fd(fd) };
    assert!(
        block_on(SavedNetworksManager::new_with_directory(
            capability,
            telemetry(),
        ))
        .is_err()
    );
}

#[test]
fn rejects_special_saved_network_file_without_blocking() {
    let directory = TestDirectory::new();
    let path = directory.path().join("saved-networks.v1");
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: path is a valid NUL-terminated pathname used only by this test.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(
        block_on(SavedNetworksManager::new_with_directory(
            directory.capability(),
            telemetry(),
        ))
        .is_err()
    );
}

#[test]
fn failed_mutations_are_not_published_in_memory() {
    let directory = TestDirectory::new();
    let saved = manager(&directory);
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let network = id(b"read-only", SecurityType::Wpa2);
    let result = block_on(saved.store(
        network.clone(),
        Credential::Password(b"valid-password".to_vec()),
    ));
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(matches!(result, Err(NetworkConfigError::FileWriteError)));
    assert!(block_on(saved.lookup(&network)).is_none());

    let retained = id(b"retained", SecurityType::Wpa2);
    block_on(saved.store(
        retained.clone(),
        Credential::Password(b"valid-password".to_vec()),
    ))
    .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = block_on(saved.remove(retained.clone()));
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(NetworkConfigError::FileWriteError)));
    assert!(block_on(saved.lookup(&retained)).is_some());

    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let credential = Credential::Password(b"valid-password".to_vec());
    block_on(saved.record_connect_result(
        retained.clone(),
        &credential,
        types::Bssid::from([1, 2, 3, 4, 5, 6]),
        fidl_fuchsia_wlan_sme::ConnectResult {
            code: fidl_fuchsia_wlan_ieee80211::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        },
        types::ScanObservation::Passive,
    ));
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let retained = block_on(saved.lookup(&retained)).unwrap();
    assert!(!retained.has_ever_connected);
    assert_eq!(retained.hidden_probability, 0.9);
}
