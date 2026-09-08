// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host value bindings generated from the pinned `fuchsia.wlan.driver` schema.

use fidl_fuchsia_wlan_ieee80211::{BssType, CipherSuiteType, KeyType, MacAddr};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JoinBssRequest {
    pub bssid: Option<MacAddr>,
    pub bss_type: Option<BssType>,
    pub remote: Option<bool>,
    pub beacon_period: Option<u16>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum WlanProtection {
    None = 0,
    Rx = 1,
    Tx = 2,
    RxTx = 3,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanKeyConfig {
    pub protection: Option<WlanProtection>,
    pub cipher_oui: Option<[u8; 3]>,
    pub cipher_type: Option<CipherSuiteType>,
    pub key_type: Option<KeyType>,
    pub peer_addr: Option<MacAddr>,
    pub key_idx: Option<u8>,
    pub key: Option<Vec<u8>>,
    pub rsc: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WlanWmmAccessCategoryParameters {
    pub ecw_min: u8,
    pub ecw_max: u8,
    pub aifsn: u8,
    pub txop_limit: u16,
    pub acm: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WlanWmmParameters {
    pub apsd: bool,
    pub ac_be_params: WlanWmmAccessCategoryParameters,
    pub ac_bk_params: WlanWmmAccessCategoryParameters,
    pub ac_vi_params: WlanWmmAccessCategoryParameters,
    pub ac_vo_params: WlanWmmAccessCategoryParameters,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum WlanSoftmacHardwareCapabilityBit {
    ShortPreamble = 0x0020,
    SpectrumMgmt = 0x0100,
    Qos = 0x0200,
    ShortSlotTime = 0x0400,
    RadioMsmt = 0x1000,
    SimultaneousClientAp = 0x10000,
}

pub type WlanSoftmacHardwareCapability = u32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_default_to_absent_fields() {
        assert_eq!(JoinBssRequest::default().bssid, None);
        assert_eq!(WlanKeyConfig::default().key, None);
    }

    #[test]
    fn protection_and_capability_discriminants_match() {
        assert_eq!(WlanProtection::RxTx as u8, 3);
        assert_eq!(
            WlanSoftmacHardwareCapabilityBit::ShortPreamble as u32,
            0x0020
        );
        assert_eq!(
            WlanSoftmacHardwareCapabilityBit::SimultaneousClientAp as u32,
            0x10000
        );
    }

    #[test]
    fn key_config_uses_ieee_value_types() {
        let config = WlanKeyConfig {
            protection: Some(WlanProtection::RxTx),
            cipher_oui: Some([0x00, 0x0f, 0xac]),
            cipher_type: Some(CipherSuiteType::Ccmp128),
            key_type: Some(KeyType::Pairwise),
            peer_addr: Some([1, 2, 3, 4, 5, 6]),
            key_idx: Some(0),
            key: Some(vec![7; 16]),
            rsc: Some(9),
        };
        assert_eq!(config.cipher_type.unwrap().into_primitive(), 4);
        assert_eq!(config.key.unwrap().len(), 16);
    }

    #[test]
    fn wmm_parameters_preserve_access_category_fields() {
        let access = WlanWmmAccessCategoryParameters {
            ecw_min: 3,
            ecw_max: 7,
            aifsn: 2,
            txop_limit: 94,
            acm: true,
        };
        let params = WlanWmmParameters {
            apsd: true,
            ac_be_params: access,
            ac_bk_params: access,
            ac_vi_params: access,
            ac_vo_params: access,
        };
        assert_eq!(params.ac_vi_params.txop_limit, 94);
        assert!(params.ac_vi_params.acm);
    }
}
