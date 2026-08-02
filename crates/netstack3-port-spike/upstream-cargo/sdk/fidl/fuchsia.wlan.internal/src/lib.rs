// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host subset generated from the pinned `fuchsia.wlan.internal` schema.

use fidl_fuchsia_wlan_ieee80211::{ChannelBandwidth, ChannelNumber};

pub const MAX_ASSOC_BASIC_RATES: u8 = 14;
pub const COUNTRY_CODE_LEN: u32 = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwePublicKey {
    pub group: u16,
    pub key: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalReportIndication {
    pub rssi_dbm: i8,
    pub snr_db: i8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelSwitchInfo {
    pub new_primary_channel: ChannelNumber,
    pub bandwidth: ChannelBandwidth,
    pub vht_secondary_80_channel: ChannelNumber,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WepCredentials {
    pub key: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WpaCredentials {
    Psk([u8; 32]),
    Passphrase(Vec<u8>),
    #[doc(hidden)]
    __Unknown {
        ordinal: u64,
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Credentials {
    Wep(WepCredentials),
    Wpa(WpaCredentials),
    #[doc(hidden)]
    __Unknown {
        ordinal: u64,
        bytes: Vec<u8>,
    },
}

impl Credentials {
    #[allow(clippy::boxed_local)]
    pub fn into_wep(self: Box<Self>) -> Option<WepCredentials> {
        match *self {
            Self::Wep(credentials) => Some(credentials),
            _ => None,
        }
    }

    #[allow(clippy::boxed_local)]
    pub fn into_wpa(self: Box<Self>) -> Option<WpaCredentials> {
        match *self {
            Self::Wpa(credentials) => Some(credentials),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct Protocol(u32);

#[allow(non_upper_case_globals)]
impl Protocol {
    pub const Open: Self = Self(1);
    pub const Wep: Self = Self(2);
    pub const Wpa1: Self = Self(3);
    pub const Wpa2Personal: Self = Self(4);
    pub const Wpa2Enterprise: Self = Self(5);
    pub const Wpa3Personal: Self = Self(6);
    pub const Wpa3Enterprise: Self = Self(7);
    pub const Owe: Self = Self(8);

    pub const fn from_primitive(value: u32) -> Option<Self> {
        if value >= 1 && value <= 8 {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn from_primitive_allow_unknown(value: u32) -> Self {
        Self(value)
    }

    pub const fn into_primitive(self) -> u32 {
        self.0
    }

    pub const fn unknown() -> Self {
        Self(u32::MAX)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Authentication {
    pub protocol: Protocol,
    pub credentials: Option<Box<Credentials>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_values_match_pinned_fidl() {
        assert_eq!(Protocol::Open.into_primitive(), 1);
        assert_eq!(Protocol::Wpa3Enterprise.into_primitive(), 7);
        assert_eq!(Protocol::Owe.into_primitive(), 8);
        assert_eq!(Protocol::from_primitive(8), Some(Protocol::Owe));
        assert_eq!(Protocol::from_primitive(44), None);
        assert_eq!(
            Protocol::from_primitive_allow_unknown(44).into_primitive(),
            44
        );
    }

    #[test]
    fn credentials_preserve_schema_shapes() {
        let psk = WpaCredentials::Psk([7; 32]);
        let credentials = Box::new(Credentials::Wpa(psk.clone()));
        assert_eq!(credentials.into_wpa(), Some(psk));

        let passphrase = WpaCredentials::Passphrase(vec![b'x'; 63]);
        assert_eq!(
            Credentials::Wpa(passphrase.clone()),
            Credentials::Wpa(passphrase)
        );
    }

    #[test]
    fn flexible_unions_retain_opaque_unknown_payloads() {
        let credentials = Credentials::__Unknown {
            ordinal: 9,
            bytes: vec![1, 2, 3],
        };
        assert_eq!(
            credentials,
            Credentials::__Unknown {
                ordinal: 9,
                bytes: vec![1, 2, 3]
            }
        );
    }
}
