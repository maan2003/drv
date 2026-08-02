// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host subset generated from the pinned `fuchsia.wlan.sme` schema.

use fidl_fuchsia_wlan_common::WlanMacRole;
use fidl_fuchsia_wlan_ieee80211::{BssDescription, ChannelBandwidth, ChannelNumber, WlanPhyType};
use fidl_fuchsia_wlan_internal::Protocol;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u32)]
pub enum Protection {
    Unknown = 0,
    Open = 1,
    Wep = 2,
    Wpa1 = 3,
    Wpa1Wpa2PersonalTkipOnly = 4,
    Wpa2PersonalTkipOnly = 5,
    Wpa1Wpa2Personal = 6,
    Wpa2Personal = 7,
    Wpa2Wpa3Personal = 8,
    Wpa3Personal = 9,
    Wpa2Enterprise = 10,
    Wpa3Enterprise = 11,
    Owe = 12,
    OpenOweTransition = 13,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisjointSecurityProtocol {
    pub protocol: Protocol,
    pub role: WlanMacRole,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Compatible {
    pub mutual_security_protocols: Vec<Protocol>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Incompatible {
    pub description: String,
    pub disjoint_security_protocols: Option<Vec<DisjointSecurityProtocol>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Compatibility {
    Compatible(Compatible),
    Incompatible(Incompatible),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioConfig {
    pub phy: WlanPhyType,
    pub primary: ChannelNumber,
    pub bandwidth: ChannelBandwidth,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanResult {
    pub compatibility: Compatibility,
    pub timestamp_nanos: i64,
    pub bss_description: BssDescription,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanResultVector {
    pub results: Vec<ScanResult>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_wlan_ieee80211::{BssType, MacAddr, WlanBand};

    fn channel(number: u8) -> ChannelNumber {
        ChannelNumber {
            band: WlanBand::FiveGhz,
            number,
        }
    }

    #[test]
    fn protection_discriminants_match_pinned_fidl() {
        assert_eq!(Protection::Unknown as u32, 0);
        assert_eq!(Protection::Wpa3Enterprise as u32, 11);
        assert_eq!(Protection::Owe as u32, 12);
        assert_eq!(Protection::OpenOweTransition as u32, 13);
    }

    #[test]
    fn compatibility_preserves_common_and_internal_schema_types() {
        let compatibility = Compatibility::Compatible(Compatible {
            mutual_security_protocols: vec![Protocol::Wpa3Personal],
        });
        assert!(matches!(compatibility, Compatibility::Compatible(_)));

        let disjoint = DisjointSecurityProtocol {
            protocol: Protocol::Wep,
            role: WlanMacRole::Client,
        };
        assert_eq!(disjoint.role, WlanMacRole::Client);
    }

    #[test]
    fn scan_result_preserves_ieee80211_value_shape() {
        let result = ScanResult {
            compatibility: Compatibility::Incompatible(Incompatible {
                description: "unsupported security".into(),
                disjoint_security_protocols: None,
            }),
            timestamp_nanos: 42,
            bss_description: BssDescription {
                bssid: MacAddr::default(),
                bss_type: BssType::Infrastructure,
                beacon_period: 100,
                capability_info: 0,
                ies: vec![],
                primary: channel(36),
                bandwidth: ChannelBandwidth::Cbw80,
                vht_secondary_80_channel: channel(0),
                rssi_dbm: -45,
                snr_db: 30,
            },
        };
        assert_eq!(result.bss_description.primary.number, 36);
    }
}
