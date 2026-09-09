//! Narrow owner for the PCI configuration operations surrounding VFIO use.

use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

const CONFIG_SNAPSHOT_LEN: usize = 256;
const PCI_COMMAND: u64 = 0x04;
const PCI_STATUS: usize = 0x06;
const PCI_CAPABILITY_LIST: usize = 0x34;
const PCI_STATUS_CAP_LIST: u16 = 1 << 4;
const PCI_CAP_ID_PM: u8 = 0x01;
const PCI_PM_CTRL: usize = 0x04;
const PCI_COMMAND_MEMORY: u16 = 1 << 1;
const PCI_COMMAND_MASTER: u16 = 1 << 2;
const PCI_COMMAND_INTX_DISABLE: u16 = 1 << 10;

#[derive(Debug)]
pub enum PciControlError {
    Io {
        operation: &'static str,
        source: std::io::Error,
    },
    InvalidCapabilityList,
    PowerManagementCapabilityAbsent,
    UnsafeDmaState {
        command: u16,
        power_state: u8,
    },
    CommandDidNotLatch {
        requested: u16,
        observed: u16,
    },
    CommandRollbackFailed {
        saved: u16,
        observed: Option<u16>,
    },
}

impl fmt::Display for PciControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::InvalidCapabilityList => write!(f, "invalid PCI capability list"),
            Self::PowerManagementCapabilityAbsent => {
                write!(f, "PCI power-management capability is absent")
            }
            Self::UnsafeDmaState {
                command,
                power_state,
            } => write!(
                f,
                "PCI DMA gate requires D0, MSE=1, BME=0; command={command:#06x}, power_state={power_state}"
            ),
            Self::CommandDidNotLatch {
                requested,
                observed,
            } => write!(
                f,
                "PCI Command did not latch: requested {requested:#06x}, observed {observed:#06x}"
            ),
            Self::CommandRollbackFailed { saved, observed } => write!(
                f,
                "PCI Command rollback to {saved:#06x} failed; observed {observed:?}"
            ),
        }
    }
}

impl std::error::Error for PciControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PciConfigSnapshot {
    bytes: [u8; CONFIG_SNAPSHOT_LEN],
    command: u16,
    power_state: u8,
}

impl PciConfigSnapshot {
    pub fn bytes(&self) -> &[u8; CONFIG_SNAPSHOT_LEN] {
        &self.bytes
    }
    pub fn command(&self) -> u16 {
        self.command
    }
    pub fn power_state(&self) -> u8 {
        self.power_state
    }
    pub fn vendor_id(&self) -> u16 {
        u16::from_le_bytes([self.bytes[0], self.bytes[1]])
    }
    pub fn device_id(&self) -> u16 {
        u16::from_le_bytes([self.bytes[2], self.bytes[3]])
    }
}

/// Owns one endpoint's sysfs config file.
///
/// Call verify_dma_disabled before constructing a VFIO device and again
/// immediately after attach, before BAR or DMA access.
pub struct PciControl {
    config: File,
}

impl PciControl {
    pub fn open(config_path: impl AsRef<Path>) -> Result<Self, PciControlError> {
        let config = OpenOptions::new()
            .read(true)
            .write(true)
            .open(config_path)
            .map_err(|source| PciControlError::Io {
                operation: "open PCI config",
                source,
            })?;
        Ok(Self { config })
    }

    pub(crate) fn from_file(config: File) -> Self {
        Self { config }
    }

    pub(crate) fn into_file(self) -> File {
        self.config
    }

    pub(crate) fn raw_fd(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(&self.config)
    }

    pub fn verify_dma_disabled(&mut self) -> Result<PciConfigSnapshot, PciControlError> {
        verify_dma_disabled(&mut self.config)
    }

    /// Set INTx Disable while preserving every other Command bit. A failed
    /// readback is rolled back to the saved Command value.
    pub fn disable_intx(&mut self) -> Result<u16, PciControlError> {
        update_command(&mut self.config, PCI_COMMAND_INTX_DISABLE, true, true)
    }

    /// Toggle Bus Master Enable while preserving every other Command bit.
    ///
    /// A failed enable is rolled back. A failed disable is deliberately not
    /// rolled back: retain the VFIO device and this owner for containment
    /// rather than intentionally reasserting BME.
    pub fn enable_bus_master(&mut self) -> Result<u16, PciControlError> {
        self.verify_dma_disabled()?;
        update_command(&mut self.config, PCI_COMMAND_MASTER, true, true)
    }

    /// Independently read back PCI Command and require Bus Master Enable.
    pub fn verify_bus_master_enabled(&mut self) -> Result<u16, PciControlError> {
        let command = read_command(&mut self.config)?;
        if command & PCI_COMMAND_MASTER != 0 {
            Ok(command)
        } else {
            Err(PciControlError::CommandDidNotLatch {
                requested: command | PCI_COMMAND_MASTER,
                observed: command,
            })
        }
    }

    /// Clear Bus Master Enable. Failure never reasserts BME; callers retain
    /// this owner together with the VFIO device for containment.
    pub fn disable_bus_master(&mut self) -> Result<u16, PciControlError> {
        update_command(&mut self.config, PCI_COMMAND_MASTER, false, false)
    }
}

fn io(operation: &'static str, source: std::io::Error) -> PciControlError {
    PciControlError::Io { operation, source }
}

fn read_exact_at(
    config: &mut (impl Read + Seek),
    offset: u64,
    bytes: &mut [u8],
) -> Result<(), PciControlError> {
    config
        .seek(SeekFrom::Start(offset))
        .and_then(|_| config.read_exact(bytes))
        .map_err(|source| io("read PCI config", source))
}

fn read_command(config: &mut (impl Read + Seek)) -> Result<u16, PciControlError> {
    let mut bytes = [0; 2];
    read_exact_at(config, PCI_COMMAND, &mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn write_command(config: &mut (impl Write + Seek), command: u16) -> Result<(), PciControlError> {
    config
        .seek(SeekFrom::Start(PCI_COMMAND))
        .and_then(|_| config.write_all(&command.to_le_bytes()))
        .and_then(|_| config.flush())
        .map_err(|source| io("write PCI Command", source))
}

fn snapshot(config: &mut (impl Read + Seek)) -> Result<PciConfigSnapshot, PciControlError> {
    let mut bytes = [0; CONFIG_SNAPSHOT_LEN];
    read_exact_at(config, 0, &mut bytes)?;
    let command =
        u16::from_le_bytes([bytes[PCI_COMMAND as usize], bytes[PCI_COMMAND as usize + 1]]);
    let status = u16::from_le_bytes([bytes[PCI_STATUS], bytes[PCI_STATUS + 1]]);
    if status & PCI_STATUS_CAP_LIST == 0 {
        return Err(PciControlError::PowerManagementCapabilityAbsent);
    }
    let mut offset = bytes[PCI_CAPABILITY_LIST] & !3;
    let mut visited = [false; 64];
    for _ in 0..48 {
        let index = usize::from(offset);
        if index < 0x40 || index + PCI_PM_CTRL + 2 > bytes.len() || visited[index / 4] {
            return Err(PciControlError::InvalidCapabilityList);
        }
        visited[index / 4] = true;
        if bytes[index] == PCI_CAP_ID_PM {
            let pmcsr =
                u16::from_le_bytes([bytes[index + PCI_PM_CTRL], bytes[index + PCI_PM_CTRL + 1]]);
            return Ok(PciConfigSnapshot {
                bytes,
                command,
                power_state: (pmcsr & 3) as u8,
            });
        }
        offset = bytes[index + 1] & !3;
        if offset == 0 {
            return Err(PciControlError::PowerManagementCapabilityAbsent);
        }
    }
    Err(PciControlError::InvalidCapabilityList)
}

fn verify_dma_disabled(
    config: &mut (impl Read + Seek),
) -> Result<PciConfigSnapshot, PciControlError> {
    let snapshot = snapshot(config)?;
    if snapshot.command & PCI_COMMAND_MEMORY == 0
        || snapshot.command & PCI_COMMAND_MASTER != 0
        || snapshot.power_state != 0
    {
        return Err(PciControlError::UnsafeDmaState {
            command: snapshot.command,
            power_state: snapshot.power_state,
        });
    }
    Ok(snapshot)
}

fn update_command(
    config: &mut (impl Read + Write + Seek),
    bit: u16,
    enabled: bool,
    rollback_on_mismatch: bool,
) -> Result<u16, PciControlError> {
    let saved = read_command(config)?;
    let requested = if enabled { saved | bit } else { saved & !bit };
    write_command(config, requested)?;
    let observed = read_command(config)?;
    if observed == requested {
        return Ok(observed);
    }
    if rollback_on_mismatch {
        if write_command(config, saved).is_err() {
            return Err(PciControlError::CommandRollbackFailed {
                saved,
                observed: None,
            });
        }
        let rollback = read_command(config).ok();
        if rollback != Some(saved) {
            return Err(PciControlError::CommandRollbackFailed {
                saved,
                observed: rollback,
            });
        }
    }
    Err(PciControlError::CommandDidNotLatch {
        requested,
        observed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn config(command: u16, power_state: u8) -> Vec<u8> {
        let mut bytes = vec![0; CONFIG_SNAPSHOT_LEN];
        bytes[0..2].copy_from_slice(&0x14c3u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&0x7961u16.to_le_bytes());
        bytes[4..6].copy_from_slice(&command.to_le_bytes());
        bytes[PCI_STATUS..PCI_STATUS + 2].copy_from_slice(&PCI_STATUS_CAP_LIST.to_le_bytes());
        bytes[PCI_CAPABILITY_LIST] = 0x40;
        bytes[0x40] = PCI_CAP_ID_PM;
        bytes[0x44..0x46].copy_from_slice(&u16::from(power_state).to_le_bytes());
        bytes
    }

    #[test]
    fn dma_gate_requires_d0_mse_and_bme_off() {
        let safe = verify_dma_disabled(&mut Cursor::new(config(PCI_COMMAND_MEMORY, 0))).unwrap();
        assert_eq!(safe.vendor_id(), 0x14c3);
        assert_eq!(safe.device_id(), 0x7961);
        for (command, power) in [
            (0, 0),
            (PCI_COMMAND_MEMORY | PCI_COMMAND_MASTER, 0),
            (PCI_COMMAND_MEMORY, 3),
        ] {
            assert!(matches!(
                verify_dma_disabled(&mut Cursor::new(config(command, power))),
                Err(PciControlError::UnsafeDmaState { .. })
            ));
        }
    }

    #[test]
    fn command_transitions_preserve_unrelated_bits_and_read_back() {
        let initial = PCI_COMMAND_MEMORY | (1 << 8);
        let mut fake = Cursor::new(config(initial, 0));
        assert_eq!(
            update_command(&mut fake, PCI_COMMAND_INTX_DISABLE, true, true).unwrap(),
            initial | PCI_COMMAND_INTX_DISABLE
        );
        assert_eq!(
            update_command(&mut fake, PCI_COMMAND_MASTER, true, true).unwrap(),
            initial | PCI_COMMAND_INTX_DISABLE | PCI_COMMAND_MASTER
        );
        assert_eq!(
            update_command(&mut fake, PCI_COMMAND_MASTER, false, false).unwrap(),
            initial | PCI_COMMAND_INTX_DISABLE
        );
    }

    struct FakeWrites {
        inner: Cursor<Vec<u8>>,
        writes: usize,
        accept_after: usize,
    }
    impl Read for FakeWrites {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(out)
        }
    }
    impl Seek for FakeWrites {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }
    impl Write for FakeWrites {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            if self.writes >= self.accept_after {
                self.inner.write(bytes)
            } else {
                Ok(bytes.len())
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failed_intx_write_restores_and_verifies_saved_command() {
        let initial = PCI_COMMAND_MEMORY | (1 << 8);
        let mut fake = FakeWrites {
            inner: Cursor::new(config(initial, 0)),
            writes: 0,
            accept_after: 2,
        };
        assert!(matches!(
            update_command(&mut fake, PCI_COMMAND_INTX_DISABLE, true, true),
            Err(PciControlError::CommandDidNotLatch { .. })
        ));
        assert_eq!(fake.writes, 2);
        assert_eq!(read_command(&mut fake).unwrap(), initial);
    }

    #[test]
    fn failed_bme_clear_is_not_rolled_back() {
        let initial = PCI_COMMAND_MEMORY | PCI_COMMAND_MASTER;
        let mut fake = FakeWrites {
            inner: Cursor::new(config(initial, 0)),
            writes: 0,
            accept_after: usize::MAX,
        };
        assert!(matches!(
            update_command(&mut fake, PCI_COMMAND_MASTER, false, false),
            Err(PciControlError::CommandDidNotLatch { .. })
        ));
        assert_eq!(fake.writes, 1);
        assert_eq!(read_command(&mut fake).unwrap(), initial);
    }
}
