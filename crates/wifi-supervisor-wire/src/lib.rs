// SPDX-License-Identifier: GPL-2.0-only

//! Transport-neutral codec for the dedicated Wi-Fi Ethernet lifecycle channel.

use std::fmt;

pub const MESSAGE_LEN: usize = 40;
const MAGIC: [u8; 4] = *b"WLSV";
const VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleKind {
    Install,
    Revoke,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LifecycleMessage {
    pub kind: LifecycleKind,
    pub wifi_generation: [u8; 16],
    pub ethernet_generation: u64,
    pub mac_address: [u8; 6],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireError {
    InvalidLength,
    InvalidMagic,
    UnsupportedVersion,
    UnknownKind,
    InvalidFlags,
    InvalidReserved,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid Wi-Fi supervisor lifecycle message: {self:?}")
    }
}

impl std::error::Error for WireError {}

impl LifecycleMessage {
    pub fn encode(self) -> [u8; MESSAGE_LEN] {
        let mut bytes = [0; MESSAGE_LEN];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4..6].copy_from_slice(&VERSION.to_le_bytes());
        bytes[6] = match self.kind {
            LifecycleKind::Install => 1,
            LifecycleKind::Revoke => 2,
        };
        bytes[8..24].copy_from_slice(&self.wifi_generation);
        bytes[24..32].copy_from_slice(&self.ethernet_generation.to_le_bytes());
        bytes[32..38].copy_from_slice(&self.mac_address);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() != MESSAGE_LEN {
            return Err(WireError::InvalidLength);
        }
        if bytes[..4] != MAGIC {
            return Err(WireError::InvalidMagic);
        }
        if u16::from_le_bytes(bytes[4..6].try_into().unwrap()) != VERSION {
            return Err(WireError::UnsupportedVersion);
        }
        let kind = match bytes[6] {
            1 => LifecycleKind::Install,
            2 => LifecycleKind::Revoke,
            _ => return Err(WireError::UnknownKind),
        };
        if bytes[7] != 0 {
            return Err(WireError::InvalidFlags);
        }
        if bytes[38..40] != [0, 0] {
            return Err(WireError::InvalidReserved);
        }
        Ok(Self {
            kind,
            wifi_generation: bytes[8..24].try_into().unwrap(),
            ethernet_generation: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            mac_address: bytes[32..38].try_into().unwrap(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_round_trip_and_reserved_rejection() {
        let message = LifecycleMessage {
            kind: LifecycleKind::Install,
            wifi_generation: [0x5a; 16],
            ethernet_generation: 9,
            mac_address: [2, 0, 0, 0, 0, 1],
        };
        assert_eq!(LifecycleMessage::decode(&message.encode()), Ok(message));
        let mut invalid = message.encode();
        invalid[39] = 1;
        assert_eq!(
            LifecycleMessage::decode(&invalid),
            Err(WireError::InvalidReserved)
        );
        assert_eq!(
            LifecycleMessage::decode(&invalid[..39]),
            Err(WireError::InvalidLength)
        );
    }
}
