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
pub const MAX_KEY_LEN: u8 = 32;
pub const SSID_LIST_MAX: u8 = 84;
pub const MAX_UNIQUE_CHANNEL_NUMBERS: u16 = 256;
pub const MAX_MGMT_FRAME_MAC_HEADER_BYTE_LEN: u8 = 28;
pub const MAX_VHT_MPDU_BYTE_LEN_2: u16 = 11_454;
pub const MAX_SUPPORTED_BASIC_RATES: u8 = 12;

pub type MacAddr = [u8; MAC_ADDR_LEN as usize];
pub type Ssid = Vec<u8>;
pub type CapabilityInfo = u16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CSsid {
    pub len: u8,
    pub data: [u8; MAX_SSID_BYTE_LEN as usize],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum WlanAccessCategory {
    Background = 1,
    BestEffort = 2,
    Video = 3,
    Voice = 4,
}

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

            pub const fn is_unknown(self) -> bool {
                self.0 == <$raw>::MAX
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
    pub const UnspecifiedReason: Self = Self(1);
    pub const InvalidAuthentication: Self = Self(2);
    pub const LeavingNetworkDeauth: Self = Self(3);
    pub const ReasonInvalidElement: Self = Self(13);
    pub const MicFailure: Self = Self(14);
    pub const FourwayHandshakeTimeout: Self = Self(15);
    pub const Ieee8021XAuthFailed: Self = Self(23);
    pub const StaLeaving: Self = Self(36);
    pub const Timeout: Self = Self(39);
}

#[allow(non_upper_case_globals)]
impl StatusCode {
    pub const Success: Self = Self(0);
    pub const RefusedReasonUnspecified: Self = Self(1);
    pub const NotInSameBss: Self = Self(7);
    pub const RefusedCapabilitiesMismatch: Self = Self(10);
    pub const DeniedNoAssociationExists: Self = Self(11);
    pub const UnsupportedAuthAlgorithm: Self = Self(13);
    pub const RejectedSequenceTimeout: Self = Self(16);
    pub const RefusedTemporarily: Self = Self(30);
    pub const RefusedUnauthenticatedAccessNotSupported: Self = Self(68);
    pub const AntiCloggingTokenRequired: Self = Self(76);
    pub const SaeHashToElement: Self = Self(126);
    pub const JoinFailure: Self = Self(256);
    pub const SpuriousDeauthOrDisassoc: Self = Self(257);
    pub const Canceled: Self = Self(258);
    pub const EstablishRsnaFailure: Self = Self(259);
    pub const OweHandshakeFailure: Self = Self(260);
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

flexible_enum!(CipherSuiteType, u32, CipherSuiteTypeUnknown, {
    UseGroup = 0,
    Wep40 = 1,
    Tkip = 2,
    Reserved3 = 3,
    Ccmp128 = 4,
    Wep104 = 5,
    BipCmac128 = 6,
    GroupAddressedNotAllowed = 7,
    Gcmp128 = 8,
    Gcmp256 = 9,
    Ccmp256 = 10,
    BipGmac128 = 11,
    BipGmac256 = 12,
    BipCmac256 = 13,
    Reserved14To255 = 14,
});

flexible_enum!(KeyType, u8, KeyTypeUnknown, {
    Pairwise = 1,
    Group = 2,
    Igtk = 3,
    Peer = 4,
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
        assert_eq!(CipherSuiteType::Ccmp128.into_primitive(), 4);
        assert_eq!(StatusCode::RefusedCapabilitiesMismatch.into_primitive(), 10);
        assert_eq!(StatusCode::RefusedTemporarily.into_primitive(), 30);
        assert_eq!(KeyType::Peer.into_primitive(), 4);
        assert_eq!(MAX_KEY_LEN, 32);
        assert_eq!(MAX_UNIQUE_CHANNEL_NUMBERS, 256);
        assert_eq!(MAX_VHT_MPDU_BYTE_LEN_2, 11_454);
        assert_eq!(WlanAccessCategory::Voice as u32, 4);
        assert_eq!(
            CSsid {
                len: 0,
                data: [0; 32]
            }
            .data
            .len(),
            32
        );
        assert_eq!(ReasonCode::MicFailure.into_primitive(), 14);
        assert_eq!(ReasonCode::Ieee8021XAuthFailed.into_primitive(), 23);
        assert_eq!(ReasonCode::Timeout.into_primitive(), 39);
        assert_eq!(StatusCode::Success.into_primitive(), 0);
        assert_eq!(StatusCode::AntiCloggingTokenRequired.into_primitive(), 76);
        assert_eq!(StatusCode::SaeHashToElement.into_primitive(), 126);
        assert_eq!(StatusCode::OweHandshakeFailure.into_primitive(), 260);
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
        assert_eq!(CipherSuiteType::from_primitive(15), None);
        assert_eq!(KeyType::from_primitive(8), None);
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
