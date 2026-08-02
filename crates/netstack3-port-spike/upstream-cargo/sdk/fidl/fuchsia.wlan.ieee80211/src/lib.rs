// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host subset generated from the pinned `fuchsia.wlan.ieee80211` schema.

pub const MAX_SSID_BYTE_LEN: u8 = 32;
pub const MAC_ADDR_LEN: u8 = 6;
pub const HT_CAP_LEN: u8 = 26;
pub const HT_OP_LEN: u8 = 22;
pub const VHT_CAP_LEN: u8 = 12;
pub const VHT_OP_LEN: u8 = 5;

pub type MacAddr = [u8; MAC_ADDR_LEN as usize];

macro_rules! flexible_enum {
    ($name:ident, $raw:ty, $unknown:ident, {$($variant:ident = $value:expr),+ $(,)?}) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        pub struct $name($raw);

        #[allow(non_upper_case_globals)]
        impl $name {
            $(pub const $variant: Self = Self($value);)+

            pub const fn from_primitive(value: $raw) -> Option<Self> {
                $(if value == $value {
                    return Some(Self::$variant);
                })+
                None
            }

            pub const fn from_primitive_allow_unknown(value: $raw) -> Self {
                Self(value)
            }

            pub const fn into_primitive(self) -> $raw {
                self.0
            }

            pub const fn unknown() -> Self {
                Self(<$raw>::MAX)
            }
        }

        #[macro_export]
        macro_rules! $unknown {
            () => { _ };
        }
    };
}

macro_rules! flexible_code {
    ($name:ident, $valid:expr) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        pub struct $name(u16);

        impl $name {
            pub const fn from_primitive(value: u16) -> Option<Self> {
                if ($valid)(value) {
                    Some(Self(value))
                } else {
                    None
                }
            }

            pub const fn from_primitive_allow_unknown(value: u16) -> Self {
                Self(value)
            }

            pub const fn into_primitive(self) -> u16 {
                self.0
            }

            pub const fn unknown() -> Self {
                Self(u16::MAX)
            }
        }
    };
}

const fn valid_reason_code(value: u16) -> bool {
    matches!(value, 1..=39 | 45..=66 | 128..=130)
}

const fn valid_status_code(value: u16) -> bool {
    matches!(
        value,
        0..=3
            | 5..=7
            | 10..=19
            | 22..=25
            | 27..=35
            | 37..=65
            | 67..=68
            | 72..=89
            | 92..=113
            | 116..=123
            | 125..=126
            | 128..=129
            | 256..=260
    )
}

flexible_code!(ReasonCode, valid_reason_code);
flexible_code!(StatusCode, valid_status_code);

#[allow(non_upper_case_globals)]
impl ReasonCode {
    pub const MicFailure: Self = Self(14);
}

#[allow(non_upper_case_globals)]
impl StatusCode {
    pub const Success: Self = Self(0);
    pub const RefusedReasonUnspecified: Self = Self(1);
    pub const AntiCloggingTokenRequired: Self = Self(76);
    pub const SaeHashToElement: Self = Self(126);
}

flexible_enum!(ChannelBandwidth, u32, ChannelBandwidthUnknown, {
    Cbw20 = 1,
    Cbw40 = 2,
    Cbw40Below = 3,
    Cbw80 = 4,
    Cbw160 = 5,
    Cbw80P80 = 6,
});

flexible_enum!(WlanBand, u8, WlanBandUnknown, {
    TwoGhz = 0,
    FiveGhz = 1,
});

flexible_enum!(BssType, u32, BssTypeUnknown, {
    Unknown = 0,
    Infrastructure = 1,
    Independent = 2,
    Mesh = 3,
    Personal = 4,
});

flexible_enum!(WlanPhyType, u32, WlanPhyTypeUnknown, {
    Dsss = 1,
    Hr = 2,
    Ofdm = 3,
    Erp = 4,
    Ht = 5,
    Dmg = 6,
    Vht = 7,
    Tvht = 8,
    S1G = 9,
    Cdmg = 10,
    Cmmg = 11,
    He = 12,
});

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelNumber {
    pub band: WlanBand,
    pub number: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HtCapabilities {
    pub bytes: [u8; HT_CAP_LEN as usize],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HtOperation {
    pub bytes: [u8; HT_OP_LEN as usize],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VhtCapabilities {
    pub bytes: [u8; VHT_CAP_LEN as usize],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VhtOperation {
    pub bytes: [u8; VHT_OP_LEN as usize],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BssDescription {
    pub bssid: MacAddr,
    pub bss_type: BssType,
    pub beacon_period: u16,
    pub capability_info: u16,
    pub ies: Vec<u8>,
    pub primary: ChannelNumber,
    pub bandwidth: ChannelBandwidth,
    pub vht_secondary_80_channel: ChannelNumber,
    pub rssi_dbm: i8,
    pub snr_db: i8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_values_match_pinned_fidl() {
        assert_eq!(ChannelBandwidth::Cbw80P80.into_primitive(), 6);
        assert_eq!(WlanBand::FiveGhz.into_primitive(), 1);
        assert_eq!(BssType::Personal.into_primitive(), 4);
        assert_eq!(WlanPhyType::He.into_primitive(), 12);
        assert_eq!(ReasonCode::MicFailure.into_primitive(), 14);
        assert_eq!(StatusCode::Success.into_primitive(), 0);
        assert_eq!(StatusCode::AntiCloggingTokenRequired.into_primitive(), 76);
        assert_eq!(StatusCode::SaeHashToElement.into_primitive(), 126);
        assert_eq!(std::mem::size_of::<MacAddr>(), MAC_ADDR_LEN as usize);
        assert_eq!(
            HtCapabilities { bytes: [0; 26] }.bytes.len(),
            HT_CAP_LEN as usize
        );
    }

    #[test]
    fn flexible_unknown_round_trips() {
        assert_eq!(WlanBand::unknown().into_primitive(), u8::MAX);
        assert_eq!(
            ChannelBandwidth::from_primitive(6),
            Some(ChannelBandwidth::Cbw80P80)
        );
        assert_eq!(ChannelBandwidth::from_primitive(77), None);
        assert_eq!(
            ChannelBandwidth::from_primitive_allow_unknown(77).into_primitive(),
            77
        );
        assert_eq!(ReasonCode::from_primitive(40), None);
        assert_eq!(
            ReasonCode::from_primitive(128).unwrap().into_primitive(),
            128
        );
        assert_eq!(
            StatusCode::from_primitive(260).unwrap().into_primitive(),
            260
        );
    }
}
