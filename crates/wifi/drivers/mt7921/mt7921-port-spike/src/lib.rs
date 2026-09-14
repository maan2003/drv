#![no_std]

//! Compatibility facade for the extracted Linux-derived MT7921 hardware core.
//!
//! CLI and lab orchestration remain in this package's binaries. New
//! hardware-facing code belongs in `mt7921_core`; this facade preserves the
//! existing public API without duplicating state or behavior.

extern crate alloc;
use alloc::vec::Vec;

pub use mt7921_core::*;

pub struct AccessPoint {
    pub bssid: [u8; 6],
    pub ssid: Vec<u8>,
    pub frequency_mhz: u16,
    pub channel: u8,
    pub signal_dbm: i16,
    pub capability_info: u16,
    pub security: Security,
    pub ht: bool,
    pub vht: bool,
    pub he: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Security {
    Open,
    Wep,
    Wpa1,
    Wpa2Personal,
    Wpa2Enterprise,
    Wpa3Personal,
    Wpa3Enterprise,
    Owe,
    UnknownProtected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdvertisementError {
    TruncatedElement,
    SsidTooLong,
    InvalidRsn,
}

impl AccessPoint {
    pub fn from_beacon(
        bssid: [u8; 6],
        capability_info: u16,
        ies: &[u8],
        frequency_mhz: u16,
        signal_dbm: i16,
    ) -> Result<Self, AdvertisementError> {
        let mut ssid = Vec::new();
        let mut dsss_channel = None;
        let mut security = None;
        let mut ht = false;
        let mut vht = false;
        let mut he = false;
        let mut offset = 0;
        while offset < ies.len() {
            if ies.len() - offset < 2 {
                return Err(AdvertisementError::TruncatedElement);
            }
            let id = ies[offset];
            let len = usize::from(ies[offset + 1]);
            offset += 2;
            let end = offset
                .checked_add(len)
                .ok_or(AdvertisementError::TruncatedElement)?;
            let body = ies
                .get(offset..end)
                .ok_or(AdvertisementError::TruncatedElement)?;
            offset = end;
            match id {
                0 if body.len() <= 32 => ssid.extend_from_slice(body),
                0 => return Err(AdvertisementError::SsidTooLong),
                3 if body.len() == 1 => dsss_channel = Some(body[0]),
                45 | 61 => ht = true,
                191 | 192 => vht = true,
                48 => security = Some(parse_rsn(body)?),
                221 if body.starts_with(&[0x00, 0x50, 0xf2, 0x01]) && security.is_none() => {
                    security = Some(Security::Wpa1)
                }
                255 if body.first() == Some(&35) || body.first() == Some(&36) => he = true,
                _ => {}
            }
        }
        let privacy = capability_info & 0x0010 != 0;
        Ok(Self {
            bssid,
            ssid,
            frequency_mhz,
            channel: dsss_channel.unwrap_or_else(|| frequency_to_channel(frequency_mhz)),
            signal_dbm,
            capability_info,
            security: security.unwrap_or(if privacy {
                Security::Wep
            } else {
                Security::Open
            }),
            ht,
            vht,
            he,
        })
    }
}

fn parse_rsn(body: &[u8]) -> Result<Security, AdvertisementError> {
    // version(2), group suite(4), pairwise count/list, AKM count/list
    if body.len() < 8 || u16::from_le_bytes([body[0], body[1]]) != 1 {
        return Err(AdvertisementError::InvalidRsn);
    }
    let pairwise_count = u16::from_le_bytes([body[6], body[7]]) as usize;
    let akm_count_at = 8usize
        .checked_add(
            pairwise_count
                .checked_mul(4)
                .ok_or(AdvertisementError::InvalidRsn)?,
        )
        .ok_or(AdvertisementError::InvalidRsn)?;
    let akm_count_bytes = body
        .get(akm_count_at..akm_count_at + 2)
        .ok_or(AdvertisementError::InvalidRsn)?;
    let akm_count = u16::from_le_bytes([akm_count_bytes[0], akm_count_bytes[1]]) as usize;
    let mut enterprise = false;
    let mut personal = false;
    let mut sae = false;
    let mut owe = false;
    let mut suite_at = akm_count_at + 2;
    for _ in 0..akm_count {
        let suite = body
            .get(suite_at..suite_at + 4)
            .ok_or(AdvertisementError::InvalidRsn)?;
        suite_at += 4;
        if suite[..3] != [0x00, 0x0f, 0xac] {
            continue;
        }
        match suite[3] {
            1 | 3 | 5 => enterprise = true,
            2 | 4 | 6 => personal = true,
            8 | 9 => sae = true,
            12 | 13 => return Ok(Security::Wpa3Enterprise),
            18 => owe = true,
            _ => {}
        }
    }
    Ok(if owe {
        Security::Owe
    } else if sae {
        Security::Wpa3Personal
    } else if personal {
        Security::Wpa2Personal
    } else if enterprise {
        Security::Wpa2Enterprise
    } else {
        Security::UnknownProtected
    })
}

pub const fn frequency_to_channel(frequency_mhz: u16) -> u8 {
    match frequency_mhz {
        2484 => 14,
        2412..=2472 => ((frequency_mhz - 2407) / 5) as u8,
        5000..=5895 => ((frequency_mhz - 5000) / 5) as u8,
        5955..=7115 => ((frequency_mhz - 5950) / 5) as u8,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_beacon_conversion_remains_outside_hardware_core() {
        let ies = [
            0x00, 0x08, b'f', b'o', b'o', b'-', b's', b's', b'i', b'd', 0x03, 0x01, 140, 0x30,
            0x12, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 8,
        ];
        let ap = AccessPoint::from_beacon([0x33; 6], 0x11, &ies, 5700, -40).unwrap();
        assert_eq!(ap.ssid.as_slice(), b"foo-ssid");
        assert_eq!(ap.channel, 140);
        assert_eq!(ap.security, Security::Wpa3Personal);
    }
}
