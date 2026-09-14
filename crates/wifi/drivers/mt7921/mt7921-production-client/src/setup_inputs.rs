//! Owned inputs prepared before sandbox lockdown.

use mt7921_core::{Firmware, FirmwareError, Patch, PatchError};
use sha2::{Digest, Sha256};
use std::{fmt, fs::File, io::Read};
use zeroize::Zeroize;

const MIN_CREDENTIAL_BYTES: usize = 8;
const MAX_CREDENTIAL_BYTES: usize = 63;
const MAX_REGULATORY_DATABASE_BYTES: usize = 1024 * 1024;

/// This identity must come from trusted build metadata or the supervising
/// process. Accepting an expectation supplied by an untrusted client would
/// authenticate that client's replacement image rather than the built asset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareImageExpectation {
    pub length: usize,
    pub sha256: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareImageKind {
    Patch,
    Ram,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareVerificationError {
    Length {
        image: FirmwareImageKind,
        expected: usize,
        actual: usize,
    },
    Sha256 {
        image: FirmwareImageKind,
        expected: [u8; 32],
        actual: [u8; 32],
    },
    Patch(PatchError),
    Ram(FirmwareError),
}

/// Decompressed patch and RAM firmware verified before sandbox lockdown.
///
/// Verification checks both the caller- or build-provided exact identity and
/// the format consumed by `mt7921-core`. The immutable owned bytes can then be
/// opened after lockdown without a path lookup, subprocess, decompression, or
/// hashing operation.
pub struct VerifiedFirmwareImages {
    patch: Box<[u8]>,
    ram: Box<[u8]>,
}

impl VerifiedFirmwareImages {
    pub fn verify(
        patch: Vec<u8>,
        ram: Vec<u8>,
        expected_patch: FirmwareImageExpectation,
        expected_ram: FirmwareImageExpectation,
    ) -> Result<Self, FirmwareVerificationError> {
        verify_identity(FirmwareImageKind::Patch, &patch, expected_patch)?;
        verify_identity(FirmwareImageKind::Ram, &ram, expected_ram)?;
        Patch::parse(&patch).map_err(FirmwareVerificationError::Patch)?;
        Firmware::parse(&ram).map_err(FirmwareVerificationError::Ram)?;
        Ok(Self {
            patch: patch.into_boxed_slice(),
            ram: ram.into_boxed_slice(),
        })
    }

    /// Borrow parsed views of the already verified immutable images.
    pub fn open(&self) -> VerifiedFirmware<'_> {
        VerifiedFirmware {
            patch: Patch::parse(&self.patch).expect("verified patch bytes are immutable"),
            ram: Firmware::parse(&self.ram).expect("verified RAM firmware bytes are immutable"),
        }
    }
}

fn verify_identity(
    image: FirmwareImageKind,
    bytes: &[u8],
    expected: FirmwareImageExpectation,
) -> Result<(), FirmwareVerificationError> {
    if bytes.len() != expected.length {
        return Err(FirmwareVerificationError::Length {
            image,
            expected: expected.length,
            actual: bytes.len(),
        });
    }
    let actual = <[u8; 32]>::from(Sha256::digest(bytes));
    if actual != expected.sha256 {
        return Err(FirmwareVerificationError::Sha256 {
            image,
            expected: expected.sha256,
            actual,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub struct VerifiedFirmware<'a> {
    pub patch: Patch<'a>,
    pub ram: Firmware<'a>,
}

/// An inherited credential descriptor. Adoption is deliberately inert.
pub struct CredentialFile(File);

impl CredentialFile {
    pub fn adopt(file: File) -> Self {
        Self(file)
    }

    /// Consume the descriptor after lockdown, reading the declared WPA
    /// passphrase length and requiring immediate EOF.
    pub fn read_exact(self, length: usize) -> std::io::Result<CredentialBytes> {
        if !(MIN_CREDENTIAL_BYTES..=MAX_CREDENTIAL_BYTES).contains(&length) {
            return Err(invalid_data("credential length is outside 8..=63 bytes"));
        }
        read_exact_to_eof(self.0, length, "credential exceeds declared length").map(CredentialBytes)
    }
}

/// Credential material whose diagnostics never expose its contents.
pub struct CredentialBytes(Vec<u8>);

impl CredentialBytes {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

impl fmt::Debug for CredentialBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialBytes([REDACTED])")
    }
}

impl Drop for CredentialBytes {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// An inherited pinned wireless-regdb descriptor. Adoption is inert;
/// reading, authentication and parsing occur only after sandbox lockdown.
pub struct RegulatoryDatabaseFile(File);

/// Authenticated immutable bytes; the core parser alone does not verify hashes.
#[derive(Debug)]
pub struct VerifiedRegulatoryDatabase {
    bytes: Vec<u8>,
    sha256: [u8; 32],
}

impl RegulatoryDatabaseFile {
    pub fn adopt(file: File) -> Self {
        Self(file)
    }

    pub fn verify(
        self,
        length: usize,
        sha256: [u8; 32],
    ) -> std::io::Result<VerifiedRegulatoryDatabase> {
        if !(1..=MAX_REGULATORY_DATABASE_BYTES).contains(&length) {
            return Err(invalid_data(
                "regulatory database length is outside 1..=1048576 bytes",
            ));
        }
        let bytes = read_exact_to_eof(
            self.0,
            length,
            "regulatory database exceeds declared length",
        )?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != sha256 {
            return Err(invalid_data("regulatory database SHA-256 mismatch"));
        }
        Ok(VerifiedRegulatoryDatabase { bytes, sha256 })
    }
}

impl VerifiedRegulatoryDatabase {
    pub fn world_snapshot(
        &self,
        generation: u64,
        capability: mt7921_core::NicCapability,
    ) -> Result<mt7921_core::RegulatoryRatePowerSnapshot, mt7921_core::RateTxPowerError> {
        mt7921_core::regulatory_rate_power_snapshot_from_regdb_v20(
            &self.bytes,
            generation,
            *b"00",
            capability,
            self.sha256,
        )
    }
}

fn read_exact_to_eof(
    mut file: File,
    length: usize,
    trailing_data: &'static str,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = vec![0; length];
    if let Err(error) = file.read_exact(&mut bytes) {
        wipe(&mut bytes);
        return Err(error);
    }
    let mut extra = [0];
    match file.read(&mut extra) {
        Ok(0) => Ok(bytes),
        Ok(_) => {
            wipe(&mut bytes);
            wipe(&mut extra);
            Err(invalid_data(trailing_data))
        }
        Err(error) => {
            wipe(&mut bytes);
            wipe(&mut extra);
            Err(error)
        }
    }
}

fn wipe(bytes: &mut [u8]) {
    bytes.zeroize();
}

fn invalid_data(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::OpenOptions,
        io::{Seek, SeekFrom, Write},
        path::PathBuf,
    };

    fn expectation(bytes: &[u8]) -> FirmwareImageExpectation {
        FirmwareImageExpectation {
            length: bytes.len(),
            sha256: Sha256::digest(bytes).into(),
        }
    }

    fn valid_images() -> (Vec<u8>, Vec<u8>) {
        let patch = vec![0; mt7921_core::PATCH_HEADER_LEN];
        let mut ram = vec![0; mt7921_core::FW_TRAILER_LEN];
        ram[0] = 0x79;
        (patch, ram)
    }

    #[test]
    fn firmware_identity_and_format_fail_closed() {
        let (patch, ram) = valid_images();
        let patch_expected = expectation(&patch);
        let ram_expected = expectation(&ram);

        let verified = VerifiedFirmwareImages::verify(
            patch.clone(),
            ram.clone(),
            patch_expected,
            ram_expected,
        )
        .unwrap();
        assert_eq!(verified.open().patch.region_count(), 0);
        assert_eq!(verified.open().ram.region_count(), 0);

        let mut wrong_length = patch_expected;
        wrong_length.length += 1;
        assert!(matches!(
            VerifiedFirmwareImages::verify(patch.clone(), ram.clone(), wrong_length, ram_expected),
            Err(FirmwareVerificationError::Length {
                image: FirmwareImageKind::Patch,
                ..
            })
        ));

        let mut wrong_hash = patch_expected;
        wrong_hash.sha256[0] ^= 1;
        assert!(matches!(
            VerifiedFirmwareImages::verify(patch.clone(), ram.clone(), wrong_hash, ram_expected),
            Err(FirmwareVerificationError::Sha256 {
                image: FirmwareImageKind::Patch,
                ..
            })
        ));

        let malformed_patch = vec![0; mt7921_core::PATCH_HEADER_LEN - 1];
        assert!(matches!(
            VerifiedFirmwareImages::verify(
                malformed_patch.clone(),
                ram.clone(),
                expectation(&malformed_patch),
                ram_expected
            ),
            Err(FirmwareVerificationError::Patch(PatchError::MissingHeader))
        ));

        let malformed_ram = vec![0; mt7921_core::FW_TRAILER_LEN - 1];
        assert!(matches!(
            VerifiedFirmwareImages::verify(
                patch.clone(),
                malformed_ram.clone(),
                patch_expected,
                expectation(&malformed_ram)
            ),
            Err(FirmwareVerificationError::Ram(
                FirmwareError::MissingTrailer
            ))
        ));
    }

    fn input_file(label: &str, bytes: &[u8], offset: u64) -> (File, File, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "drv-mt7921-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(bytes).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        let observer = file.try_clone().unwrap();
        (file, observer, path)
    }

    #[test]
    fn credential_and_snapshot_adoption_are_inert() {
        let secret = b"do-not-log-this-credential";
        let (credential, mut credential_observer, credential_path) =
            input_file("credential", secret, 3);
        let (snapshot, mut snapshot_observer, snapshot_path) =
            input_file("snapshot", b"snapshot", 2);

        let _credential = CredentialFile::adopt(credential);
        let _snapshot = RegulatoryDatabaseFile::adopt(snapshot);
        assert_eq!(credential_observer.stream_position().unwrap(), 3);
        assert_eq!(snapshot_observer.stream_position().unwrap(), 2);

        std::fs::remove_file(credential_path).unwrap();
        std::fs::remove_file(snapshot_path).unwrap();
    }

    fn fresh_input(label: &str, bytes: &[u8]) -> (File, PathBuf) {
        let (file, observer, path) = input_file(label, bytes, 0);
        drop(observer);
        (file, path)
    }

    #[test]
    fn credential_read_is_exact_bounded_redacted_and_eof_terminated() {
        let secret = b"eight-by";
        let (file, path) = fresh_input("credential-exact", secret);
        let credential = CredentialFile::adopt(file)
            .read_exact(secret.len())
            .unwrap();
        assert_eq!(credential.as_bytes(), secret);
        assert!(!format!("{credential:?}").contains("eight-by"));
        std::fs::remove_file(path).unwrap();

        let (file, path) = fresh_input("credential-short", b"short!!");
        let error = CredentialFile::adopt(file).read_exact(8).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
        assert!(!format!("{error:?}").contains("short"));
        std::fs::remove_file(path).unwrap();

        let (file, path) = fresh_input("credential-extra", b"eight-by-extra-secret");
        let error = CredentialFile::adopt(file).read_exact(8).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!format!("{error:?}").contains("extra-secret"));
        std::fs::remove_file(path).unwrap();

        for length in [7, 64] {
            let (file, path) = fresh_input("credential-bounds", b"");
            assert_eq!(
                CredentialFile::adopt(file)
                    .read_exact(length)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidData
            );
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn regulatory_database_read_authenticates_exact_bounded_bytes() {
        let bytes = include_bytes!("../../mt7921-core/tests/fixtures/regulatory.db");
        let sha = <[u8; 32]>::from(Sha256::digest(bytes));
        let (file, path) = fresh_input("regdb-exact", bytes);
        let verified = RegulatoryDatabaseFile::adopt(file)
            .verify(bytes.len(), sha)
            .unwrap();
        assert_eq!(verified.bytes, bytes);
        assert_eq!(verified.sha256, sha);
        std::fs::remove_file(path).unwrap();

        for (length, hash, expected) in [
            (bytes.len(), [0; 32], std::io::ErrorKind::InvalidData),
            (bytes.len() + 1, sha, std::io::ErrorKind::UnexpectedEof),
            (bytes.len() - 1, sha, std::io::ErrorKind::InvalidData),
            (0, sha, std::io::ErrorKind::InvalidData),
            (
                MAX_REGULATORY_DATABASE_BYTES + 1,
                sha,
                std::io::ErrorKind::InvalidData,
            ),
        ] {
            let (file, path) = fresh_input("regdb-invalid", bytes);
            assert_eq!(
                RegulatoryDatabaseFile::adopt(file)
                    .verify(length, hash)
                    .unwrap_err()
                    .kind(),
                expected
            );
            std::fs::remove_file(path).unwrap();
        }
    }
}
