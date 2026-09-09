// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable saved-network storage using only an inherited directory capability.
//!
//! Atomic replacement cannot be implemented safely from one already-open regular
//! file: after a crash there is no name at which to install a fully synced
//! replacement.  The explicit capability is therefore an owned `File` opened on
//! a dedicated, sole-writer directory.  All names below are fixed, relative names
//! accepted by `directory-capability`; no ambient path lookup is performed.
//!
//! Version 1 is an exact-EOF, little-endian format: eight-byte magic, `u16`
//! version, `u16` record count, then bounded records containing length-prefixed
//! SSID and credential bytes, one-byte security/credential/boolean tags, and an
//! `f32` hidden probability. Files are limited to 128 KiB and 1000 records.

use crate::config_management::{Credential, NetworkConfig, SecurityType};
use std::collections::HashMap;
use directory_capability::Directory;
use std::fs::File;
use std::io::{self, Read, Write};

const DATA_NAME: &str = "saved-networks.v1";
const TEMP_NAME: &str = ".saved-networks.v1.tmp";
const MAGIC: &[u8; 8] = b"DRVWIFI\0";
const VERSION: u16 = 1;
const MAX_NETWORKS: usize = 1000;
const MAX_FILE_BYTES: u64 = 128 * 1024;

/// A parsed persistent record. Credential bytes are moved directly into the
/// pinned wlancfg credential type during manager construction.
pub(crate) struct PersistedNetwork {
    pub(crate) ssid: Vec<u8>,
    pub(crate) security_type: SecurityType,
    pub(crate) credential: Credential,
    pub(crate) has_ever_connected: bool,
    pub(crate) hidden_probability: Option<f32>,
}

/// Credential-safe storage failure. Messages contain no SSIDs or credentials.
pub struct PersistenceError {
    kind: io::ErrorKind,
    message: &'static str,
    installed: bool,
}

impl PersistenceError {
    fn io(message: &'static str, error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            message,
            installed: false,
        }
    }

    fn invalid(message: &'static str) -> Self {
        Self {
            kind: io::ErrorKind::InvalidData,
            message,
            installed: false,
        }
    }

    pub(crate) fn invalid_config() -> Self {
        Self::invalid("invalid saved-network configuration")
    }

    fn after_install(message: &'static str, error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            message,
            installed: true,
        }
    }

    pub(crate) fn was_installed(&self) -> bool {
        self.installed
    }
}

impl std::fmt::Debug for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistenceError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .field("installed", &self.installed)
            .finish()
    }
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for PersistenceError {}

/// Persistent storage owned by the pinned `SavedNetworksManager`.
pub struct HostPolicyStorage {
    directory: Directory,
}

impl HostPolicyStorage {
    pub(crate) fn new(directory: File) -> Result<Self, PersistenceError> {
        let directory = Directory::new(directory).map_err(|error| {
            PersistenceError::io("invalid saved-network directory capability", error)
        })?;
        let storage = Self { directory };
        if remove_stale_temp(&storage.directory)? {
            storage.directory.sync().map_err(|error| {
                PersistenceError::io("cannot sync saved-network directory", error)
            })?;
        }
        Ok(storage)
    }

    pub(crate) fn load(&mut self) -> Result<Vec<PersistedNetwork>, PersistenceError> {
        let mut file = match self.directory.open_existing_regular(DATA_NAME) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(PersistenceError::io(
                    "cannot open saved-network data",
                    error,
                ));
            }
        };
        let metadata = file
            .metadata()
            .map_err(|error| PersistenceError::io("cannot inspect saved-network data", error))?;
        let length = metadata.len();
        if length > MAX_FILE_BYTES {
            return Err(PersistenceError::invalid("saved-network data is oversized"));
        }
        decode(&mut file)
    }

    pub(crate) fn write(
        &mut self,
        networks: &HashMap<crate::config_management::NetworkIdentifier, NetworkConfig>,
    ) -> Result<(), PersistenceError> {
        if remove_stale_temp(&self.directory)? {
            self.directory.sync().map_err(|error| {
                PersistenceError::io("cannot sync saved-network directory", error)
            })?;
        }
        let mut temporary = self.directory.create_new_regular(TEMP_NAME, 0o600)
        .map_err(|error| PersistenceError::io("cannot create saved-network replacement", error))?;

        let result = (|| {
            encode(&mut temporary, networks)?;
            temporary.sync_all().map_err(|error| {
                PersistenceError::io("cannot sync saved-network replacement", error)
            })?;
            self.directory.replace(TEMP_NAME, DATA_NAME).map_err(|error| {
                PersistenceError::io("cannot install saved-network replacement", error)
            })?;
            self.directory.sync().map_err(|error| {
                PersistenceError::after_install("cannot sync saved-network directory", error)
            })
        })();
        if result.is_err() {
            let _ = self.directory.unlink(TEMP_NAME);
        }
        result
    }
}

fn encode(
    output: &mut File,
    networks: &HashMap<crate::config_management::NetworkIdentifier, NetworkConfig>,
) -> Result<(), PersistenceError> {
    if networks.len() > MAX_NETWORKS {
        return Err(PersistenceError::invalid("too many saved networks"));
    }
    output
        .write_all(MAGIC)
        .and_then(|()| output.write_all(&VERSION.to_le_bytes()))
        .and_then(|()| output.write_all(&(networks.len() as u16).to_le_bytes()))
        .map_err(|error| PersistenceError::io("cannot write saved-network replacement", error))?;

    // Stable ordering makes the format deterministic without copying credentials.
    let mut configs: Vec<&NetworkConfig> = networks.values().collect();
    configs.sort_by(|left, right| {
        left.ssid.as_ref().cmp(right.ssid.as_ref()).then_with(|| {
            security_byte(left.security_type).cmp(&security_byte(right.security_type))
        })
    });
    for config in configs {
        let ssid = config.ssid.as_ref();
        let (credential_kind, credential): (u8, &[u8]) = match &config.credential {
            Credential::None => (0, &[]),
            Credential::Password(bytes) => (1, bytes),
            Credential::Psk(bytes) => (2, bytes),
        };
        let ssid_len = u8::try_from(ssid.len())
            .map_err(|_| PersistenceError::invalid("saved-network SSID is too long"))?;
        let credential_len = u8::try_from(credential.len())
            .map_err(|_| PersistenceError::invalid("saved-network credential is too long"))?;
        output
            .write_all(&[ssid_len])
            .and_then(|()| output.write_all(ssid))
            .and_then(|()| output.write_all(&[security_byte(config.security_type)]))
            .and_then(|()| output.write_all(&[credential_kind, credential_len]))
            .and_then(|()| output.write_all(credential))
            .and_then(|()| output.write_all(&[u8::from(config.has_ever_connected)]))
            .and_then(|()| output.write_all(&config.hidden_probability.to_bits().to_le_bytes()))
            .map_err(|error| {
                PersistenceError::io("cannot write saved-network replacement", error)
            })?;
    }
    Ok(())
}

fn decode(input: &mut File) -> Result<Vec<PersistedNetwork>, PersistenceError> {
    let mut magic = [0; 8];
    read_exact(input, &mut magic)?;
    if &magic != MAGIC {
        return Err(PersistenceError::invalid("invalid saved-network format"));
    }
    let version = read_u16(input)?;
    if version != VERSION {
        return Err(PersistenceError::invalid(
            "unsupported saved-network version",
        ));
    }
    let count = usize::from(read_u16(input)?);
    if count > MAX_NETWORKS {
        return Err(PersistenceError::invalid("too many saved networks"));
    }
    let mut networks = Vec::with_capacity(count);
    for _ in 0..count {
        let ssid_len = usize::from(read_u8(input)?);
        if !(1..=32).contains(&ssid_len) {
            return Err(PersistenceError::invalid(
                "invalid saved-network SSID length",
            ));
        }
        let mut ssid = vec![0; ssid_len];
        read_exact(input, &mut ssid)?;
        let security_type = security_from_byte(read_u8(input)?)?;
        let credential_kind = read_u8(input)?;
        let credential_len = usize::from(read_u8(input)?);
        let mut bytes = vec![0; credential_len];
        read_exact(input, &mut bytes)?;
        let credential = match credential_kind {
            0 if bytes.is_empty() => Credential::None,
            1 => Credential::Password(bytes),
            2 => Credential::Psk(bytes),
            _ => {
                return Err(PersistenceError::invalid(
                    "invalid saved-network credential",
                ));
            }
        };
        let has_ever_connected = match read_u8(input)? {
            0 => false,
            1 => true,
            _ => return Err(PersistenceError::invalid("invalid saved-network boolean")),
        };
        let hidden_probability = f32::from_bits(read_u32(input)?);
        if !hidden_probability.is_finite() || !(0.0..=1.0).contains(&hidden_probability) {
            return Err(PersistenceError::invalid(
                "invalid saved-network hidden probability",
            ));
        }
        networks.push(PersistedNetwork {
            ssid,
            security_type,
            credential,
            has_ever_connected,
            hidden_probability: Some(hidden_probability),
        });
    }
    let mut trailing = [0];
    match input.read(&mut trailing) {
        Ok(0) => Ok(networks),
        Ok(_) => Err(PersistenceError::invalid("trailing saved-network data")),
        Err(error) => Err(PersistenceError::io(
            "cannot read saved-network data",
            error,
        )),
    }
}

fn read_exact(input: &mut File, bytes: &mut [u8]) -> Result<(), PersistenceError> {
    input
        .read_exact(bytes)
        .map_err(|error| PersistenceError::io("truncated saved-network data", error))
}

fn read_u8(input: &mut File) -> Result<u8, PersistenceError> {
    let mut bytes = [0];
    read_exact(input, &mut bytes)?;
    Ok(bytes[0])
}

fn read_u16(input: &mut File) -> Result<u16, PersistenceError> {
    let mut bytes = [0; 2];
    read_exact(input, &mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(input: &mut File) -> Result<u32, PersistenceError> {
    let mut bytes = [0; 4];
    read_exact(input, &mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn security_byte(security: SecurityType) -> u8 {
    match security {
        SecurityType::None => 0,
        SecurityType::Wep => 1,
        SecurityType::Wpa => 2,
        SecurityType::Wpa2 => 3,
        SecurityType::Wpa3 => 4,
    }
}

fn security_from_byte(value: u8) -> Result<SecurityType, PersistenceError> {
    match value {
        0 => Ok(SecurityType::None),
        1 => Ok(SecurityType::Wep),
        2 => Ok(SecurityType::Wpa),
        3 => Ok(SecurityType::Wpa2),
        4 => Ok(SecurityType::Wpa3),
        _ => Err(PersistenceError::invalid(
            "invalid saved-network security type",
        )),
    }
}

fn remove_stale_temp(directory: &Directory) -> Result<bool, PersistenceError> {
    match directory.unlink(TEMP_NAME) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(PersistenceError::io(
            "cannot remove stale saved-network replacement",
            error,
        )),
    }
}
