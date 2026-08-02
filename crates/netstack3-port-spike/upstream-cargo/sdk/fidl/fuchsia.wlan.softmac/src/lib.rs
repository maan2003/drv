// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host subset generated from the pinned `fuchsia.wlan.softmac` schema.

use fidl_fuchsia_wlan_common::WlanMacRole;
use fidl_fuchsia_wlan_driver::{WlanSoftmacHardwareCapability, WlanWmmParameters};
use fidl_fuchsia_wlan_ieee80211::{
    CSsid, ChannelBandwidth, ChannelNumber, HtCapabilities, HtOperation, KeyType, MacAddr,
    VhtCapabilities, VhtOperation, WlanAccessCategory, WlanBand, WlanPhyType,
};

pub const WLAN_TX_VECTOR_IDX_INVALID: u16 = 0;
pub const WLAN_TX_RESULT_MAX_ENTRY: u32 = 8;
pub const WLAN_MAC_MAX_RATES: u32 = 263;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanOffloadExtension {
    pub supported: Option<bool>,
    pub scan_cancel_supported: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProbeResponseOffloadExtension {
    pub supported: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoverySupport {
    pub scan_offload: Option<ScanOffloadExtension>,
    pub probe_response_offload: Option<ProbeResponseOffloadExtension>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanSoftmacBandCapability {
    pub band: Option<WlanBand>,
    pub ht_caps: Option<HtCapabilities>,
    pub vht_caps: Option<VhtCapabilities>,
    pub basic_rates: Option<Vec<u8>>,
    pub primary_channels: Option<Vec<ChannelNumber>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanSoftmacQueryResponse {
    pub sta_addr: Option<MacAddr>,
    pub mac_role: Option<WlanMacRole>,
    pub supported_phys: Option<Vec<WlanPhyType>>,
    pub hardware_capability: Option<WlanSoftmacHardwareCapability>,
    pub band_caps: Option<Vec<WlanSoftmacBandCapability>>,
    pub factory_addr: Option<MacAddr>,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub struct WlanRxInfoValid: u32 {
        const PHY = 0x1;
        const DATA_RATE = 0x2;
        const CHAN_WIDTH = 0x4;
        const MCS = 0x8;
        const RSSI = 0x10;
        const SNR = 0x20;
    }

    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub struct WlanRxInfoFlags: u32 {
        const FCS_INVALID = 0x1;
        const FRAME_BODY_PADDING_4 = 0x2;
    }

    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub struct WlanTxInfoFlags: u32 {
        const PROTECTED = 0x1;
        const FAVOR_RELIABILITY = 0x2;
        const QOS = 0x4;
    }

    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub struct WlanTxInfoValid: u32 {
        const DATA_RATE = 0x1;
        const TX_VECTOR_IDX = 0x2;
        const PHY = 0x4;
        const CHANNEL_BANDWIDTH = 0x8;
        const MCS = 0x10;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WlanRxInfo {
    pub rx_flags: WlanRxInfoFlags,
    pub valid_fields: WlanRxInfoValid,
    pub phy: WlanPhyType,
    pub data_rate: u32,
    pub primary: ChannelNumber,
    pub bandwidth: ChannelBandwidth,
    pub vht_secondary_80_channel: ChannelNumber,
    pub mcs: u8,
    pub rssi_dbm: i8,
    pub snr_dbh: i16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WlanTxInfo {
    pub tx_flags: u32,
    pub valid_fields: u32,
    pub tx_vector_idx: u16,
    pub phy: WlanPhyType,
    pub bandwidth: ChannelBandwidth,
    pub mcs: u8,
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
pub struct WlanKeyConfiguration {
    pub protection: Option<WlanProtection>,
    pub cipher_oui: Option<[u8; 3]>,
    pub cipher_type: Option<u8>,
    pub key_type: Option<KeyType>,
    pub peer_addr: Option<MacAddr>,
    pub key_idx: Option<u8>,
    pub key: Option<Vec<u8>>,
    pub rsc: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WlanRxPacket {
    pub mac_frame: Vec<u8>,
    pub info: WlanRxInfo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WlanTxPacket {
    pub mac_frame: Vec<u8>,
    pub info: WlanTxInfo,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EthernetRxTransferRequest {
    pub packet_address: Option<u64>,
    pub packet_size: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EthernetTxTransferRequest {
    pub packet_address: Option<u64>,
    pub packet_size: Option<u64>,
    pub async_id: Option<u64>,
    pub borrowed_operation: Option<u64>,
    pub complete_borrowed_operation: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanRxTransferRequest {
    pub packet_address: Option<u64>,
    pub packet_size: Option<u64>,
    pub packet_info: Option<WlanRxInfo>,
    pub async_id: Option<u64>,
    pub arena: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanTxTransferRequest {
    pub packet_address: Option<u64>,
    pub packet_size: Option<u64>,
    pub packet_info: Option<WlanTxInfo>,
    pub async_id: Option<u64>,
    pub arena: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanSoftmacStartActiveScanRequest {
    pub channels: Option<Vec<ChannelNumber>>,
    pub ssids: Option<Vec<CSsid>>,
    pub mac_header: Option<Vec<u8>>,
    pub ies: Option<Vec<u8>>,
    pub min_channel_time: Option<i64>,
    pub max_channel_time: Option<i64>,
    pub min_home_time: Option<i64>,
    pub min_probes_per_channel: Option<u8>,
    pub max_probes_per_channel: Option<u8>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanAssociationConfig {
    pub bssid: Option<MacAddr>,
    pub aid: Option<u16>,
    pub listen_interval: Option<u16>,
    pub primary: Option<ChannelNumber>,
    pub qos: Option<bool>,
    pub wmm_params: Option<WlanWmmParameters>,
    pub rates: Option<Vec<u8>>,
    pub capability_info: Option<u16>,
    pub ht_cap: Option<HtCapabilities>,
    pub ht_op: Option<HtOperation>,
    pub vht_cap: Option<VhtCapabilities>,
    pub vht_op: Option<VhtOperation>,
    pub bandwidth: Option<ChannelBandwidth>,
    pub vht_secondary_80_channel: Option<ChannelNumber>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanSoftmacBaseStartPassiveScanRequest {
    pub channels: Option<Vec<ChannelNumber>>,
    pub min_channel_time: Option<i64>,
    pub max_channel_time: Option<i64>,
    pub min_home_time: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanSoftmacBaseStartPassiveScanResponse {
    pub scan_id: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanSoftmacBaseStartActiveScanResponse {
    pub scan_id: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanSoftmacBaseCancelScanRequest {
    pub scan_id: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanSoftmacBaseSetChannelRequest {
    pub primary: Option<ChannelNumber>,
    pub bandwidth: Option<ChannelBandwidth>,
    pub vht_secondary_80_channel: Option<ChannelNumber>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanSoftmacBaseClearAssociationRequest {
    pub peer_addr: Option<MacAddr>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanSoftmacBaseEnableBeaconingRequest {
    pub packet_template: Option<WlanTxPacket>,
    pub tim_ele_offset: Option<u64>,
    pub beacon_interval: Option<u16>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanSoftmacBaseUpdateWmmParametersRequest {
    pub ac: Option<WlanAccessCategory>,
    pub params: Option<WlanWmmParameters>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WlanTxResultEntry {
    pub tx_vector_idx: u16,
    pub attempts: u8,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct WlanTxResultCode(u8);

#[allow(non_upper_case_globals)]
impl WlanTxResultCode {
    pub const Failed: Self = Self(0);
    pub const Success: Self = Self(1);

    pub const fn from_primitive(value: u8) -> Option<Self> {
        if value <= 1 { Some(Self(value)) } else { None }
    }

    pub const fn from_primitive_allow_unknown(value: u8) -> Self {
        Self(value)
    }
    pub const fn into_primitive(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WlanTxResult {
    pub tx_result_entry: [WlanTxResultEntry; WLAN_TX_RESULT_MAX_ENTRY as usize],
    pub peer_addr: MacAddr,
    pub result_code: WlanTxResultCode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_bits_match_pinned_fidl() {
        assert_eq!(WlanRxInfoValid::all().bits(), 0x3f);
        assert_eq!(WlanRxInfoFlags::all().bits(), 0x3);
        assert_eq!(WlanTxInfoFlags::all().bits(), 0x7);
        assert_eq!(WlanTxInfoValid::all().bits(), 0x1f);
        assert_eq!(
            (WlanTxInfoValid::TX_VECTOR_IDX
                | WlanTxInfoValid::PHY
                | WlanTxInfoValid::CHANNEL_BANDWIDTH
                | WlanTxInfoValid::MCS)
                .bits(),
            0x1e
        );
    }

    #[test]
    fn flexible_bits_retain_unknown_values() {
        assert_eq!(WlanTxInfoFlags::from_bits_retain(0x80).bits(), 0x80);
    }

    #[test]
    fn tx_info_uses_ieee80211_schema_types() {
        let info = WlanTxInfo {
            tx_flags: WlanTxInfoFlags::FAVOR_RELIABILITY.bits(),
            valid_fields: WlanTxInfoValid::PHY.bits(),
            tx_vector_idx: 7,
            phy: WlanPhyType::Ht,
            bandwidth: ChannelBandwidth::Cbw40,
            mcs: 3,
        };
        assert_eq!(info.phy, WlanPhyType::Ht);
        assert_eq!(info.bandwidth, ChannelBandwidth::Cbw40);
    }

    #[test]
    fn mlme_tables_default_to_absent_fields() {
        assert_eq!(WlanSoftmacQueryResponse::default().sta_addr, None);
        assert_eq!(WlanAssociationConfig::default().bssid, None);
        assert_eq!(WlanSoftmacStartActiveScanRequest::default().channels, None);
        assert_eq!(WlanTxTransferRequest::default().arena, None);
    }

    #[test]
    fn tx_result_preserves_bounded_attempt_history_and_unknown_code() {
        let result = WlanTxResult {
            tx_result_entry: [WlanTxResultEntry {
                tx_vector_idx: 17,
                attempts: 2,
            }; 8],
            peer_addr: [1, 2, 3, 4, 5, 6],
            result_code: WlanTxResultCode::from_primitive_allow_unknown(9),
        };
        assert_eq!(
            result.tx_result_entry.len(),
            WLAN_TX_RESULT_MAX_ENTRY as usize
        );
        assert_eq!(result.tx_result_entry[0].attempts, 2);
        assert_eq!(result.result_code.into_primitive(), 9);
    }
}
