// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host subset generated from the pinned `fuchsia.wlan.mlme` schema.

use fidl_fuchsia_wlan_common::WlanMacRole;
use fidl_fuchsia_wlan_ieee80211::{
    BssDescription, BssType, CapabilityInfo, ChannelBandwidth, ChannelNumber, CipherSuiteType,
    HtCapabilities, MacAddr, ReasonCode, Ssid, StatusCode, VhtCapabilities, WlanBand, WlanPhyType,
};
use fidl_fuchsia_wlan_internal::OwePublicKey;
use fidl_fuchsia_wlan_minstrel::{Peer, Peers};
use fidl_fuchsia_wlan_stats::{IfaceHistogramStats, IfaceStats};

pub const MAX_SSIDS_PER_SCAN_REQUEST: u32 = 32;
pub const WMM_PARAM_LEN: u8 = 18;
pub const COUNTRY_ENVIRON_ALL: u8 = 32;
pub const COUNTRY_ENVIRON_OUTDOOR: u8 = 79;
pub const COUNTRY_ENVIRON_INDOOR: u8 = 73;
pub const COUNTRY_ENVIRON_NON_COUNTRY: u8 = 88;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum KeyType {
    Group = 1,
    Pairwise = 2,
    PeerKey = 3,
    Igtk = 4,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetKeyDescriptor {
    pub key: Vec<u8>,
    pub key_id: u16,
    pub key_type: KeyType,
    pub address: MacAddr,
    pub rsc: u64,
    pub cipher_suite_oui: [u8; 3],
    pub cipher_suite_type: CipherSuiteType,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetKeysRequest {
    pub keylist: Vec<SetKeyDescriptor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ScanTypes {
    Active = 1,
    Passive = 2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanRequest {
    pub txn_id: u64,
    pub scan_type: ScanTypes,
    pub channel_list: Vec<ChannelNumber>,
    pub ssid_list: Vec<Ssid>,
    pub probe_delay: u32,
    pub min_channel_time: u32,
    pub max_channel_time: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WmmParameter {
    pub bytes: [u8; WMM_PARAM_LEN as usize],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ScanResultCode {
    Success = 0,
    NotSupported = 1,
    InvalidArgs = 2,
    InternalError = 3,
    ShouldWait = 4,
    CanceledByDriverOrFirmware = 5,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanResult {
    pub txn_id: u64,
    pub timestamp_nanos: i64,
    pub bss: BssDescription,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanEnd {
    pub txn_id: u64,
    pub code: ScanResultCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AuthenticationTypes {
    OpenSystem = 1,
    SharedKey = 2,
    FastBssTransition = 3,
    Sae = 4,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectRequest {
    pub selected_bss: BssDescription,
    pub connect_failure_timeout: u32,
    pub auth_type: AuthenticationTypes,
    pub sae_password: Vec<u8>,
    pub wep_key: Option<Box<SetKeyDescriptor>>,
    pub security_ie: Vec<u8>,
    pub owe_public_key: Option<Box<OwePublicKey>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectConfirm {
    pub peer_sta_address: MacAddr,
    pub result_code: StatusCode,
    pub association_id: u16,
    pub association_ies: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectRequest {
    pub peer_sta_address: MacAddr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticateIndication {
    pub peer_sta_address: MacAddr,
    pub auth_type: AuthenticationTypes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AuthenticateResultCode {
    Success = 0,
    Refused = 1,
    AntiCloggingTokenRequired = 2,
    FiniteCyclicGroupNotSupported = 3,
    AuthenticationRejected = 4,
    AuthFailureTimeout = 5,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticateResponse {
    pub peer_sta_address: MacAddr,
    pub result_code: AuthenticateResultCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeauthenticateRequest {
    pub peer_sta_address: MacAddr,
    pub reason_code: ReasonCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeauthenticateConfirm {
    pub peer_sta_address: MacAddr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeauthenticateIndication {
    pub peer_sta_address: MacAddr,
    pub reason_code: ReasonCode,
    pub locally_initiated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssociateIndication {
    pub peer_sta_address: MacAddr,
    pub capability_info: CapabilityInfo,
    pub listen_interval: u16,
    pub ssid: Option<Ssid>,
    pub rates: Vec<u8>,
    pub rsne: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AssociateResultCode {
    Success = 0,
    RefusedReasonUnspecified = 1,
    RefusedNotAuthenticated = 2,
    RefusedCapabilitiesMismatch = 3,
    RefusedExternalReason = 4,
    RefusedApOutOfMemory = 5,
    RefusedBasicRatesMismatch = 6,
    RejectedEmergencyServicesNotSupported = 7,
    RefusedTemporarily = 8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssociateResponse {
    pub peer_sta_address: MacAddr,
    pub result_code: AssociateResultCode,
    pub association_id: u16,
    pub capability_info: CapabilityInfo,
    pub rates: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisassociateRequest {
    pub peer_sta_address: MacAddr,
    pub reason_code: ReasonCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisassociateConfirm {
    pub status: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisassociateIndication {
    pub peer_sta_address: MacAddr,
    pub reason_code: ReasonCode,
    pub locally_initiated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Country {
    pub alpha2: [u8; 2],
    pub suffix: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartRequest {
    pub ssid: Ssid,
    pub bss_type: BssType,
    pub beacon_period: u16,
    pub dtim_period: u8,
    pub primary: ChannelNumber,
    pub capability_info: CapabilityInfo,
    pub rates: Vec<u8>,
    pub country: Country,
    pub mesh_id: Vec<u8>,
    pub rsne: Option<Vec<u8>>,
    pub phy: WlanPhyType,
    pub bandwidth: ChannelBandwidth,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum StartResultCode {
    Success = 0,
    BssAlreadyStartedOrJoined = 1,
    ResetRequiredBeforeStart = 2,
    NotSupported = 3,
    InternalError = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StartConfirm {
    pub result_code: StartResultCode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StopRequest {
    pub ssid: Ssid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum StopResultCode {
    Success = 0,
    BssAlreadyStopped = 1,
    InternalError = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StopConfirm {
    pub result_code: StopResultCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeleteKeyDescriptor {
    pub key_id: u16,
    pub key_type: KeyType,
    pub address: MacAddr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteKeysRequest {
    pub keylist: Vec<DeleteKeyDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EapolRequest {
    pub src_addr: MacAddr,
    pub dst_addr: MacAddr,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EapolConfirm {
    pub result_code: EapolResultCode,
    pub dst_addr: MacAddr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EapolIndication {
    pub src_addr: MacAddr,
    pub dst_addr: MacAddr,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum EapolResultCode {
    Success = 0,
    TransmissionFailure = 1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaeFrame {
    pub peer_sta_address: MacAddr,
    pub status_code: StatusCode,
    pub seq_num: u16,
    pub sae_fields: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BandCapability {
    pub band: WlanBand,
    pub basic_rates: Vec<u8>,
    pub ht_cap: Option<Box<HtCapabilities>>,
    pub vht_cap: Option<Box<VhtCapabilities>>,
    pub primary_channels: Vec<ChannelNumber>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInfo {
    pub sta_addr: MacAddr,
    pub factory_addr: MacAddr,
    pub role: WlanMacRole,
    pub bands: Vec<BandCapability>,
    pub softmac_hardware_capability: u32,
    pub qos_capable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SetKeyResult {
    pub key_id: u16,
    pub status: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetKeysConfirm {
    pub results: Vec<SetKeyResult>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GetIfaceStatsResponse {
    Stats(IfaceStats),
    ErrorStatus(i32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GetIfaceHistogramStatsResponse {
    Stats(IfaceHistogramStats),
    ErrorStatus(i32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinstrelListResponse {
    pub peers: Peers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MinstrelStatsRequest {
    pub peer_addr: MacAddr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MinstrelStatsResponse {
    pub peer: Option<Box<Peer>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ControlledPortState {
    Closed = 0,
    Open = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SetControlledPortRequest {
    pub peer_sta_address: MacAddr,
    pub state: ControlledPortState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiatedCapabilities {
    pub primary: ChannelNumber,
    pub capability_info: CapabilityInfo,
    pub rates: Vec<u8>,
    pub wmm_param: Option<Box<WmmParameter>>,
    pub ht_cap: Option<Box<HtCapabilities>>,
    pub vht_cap: Option<Box<VhtCapabilities>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PmkInfo {
    pub pmk: Vec<u8>,
    pub pmkid: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SaeHandshakeIndication {
    pub peer_sta_address: MacAddr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SaeHandshakeResponse {
    pub peer_sta_address: MacAddr,
    pub status_code: StatusCode,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_wlan_ieee80211::HT_CAP_LEN;

    #[test]
    fn band_capability_preserves_schema_shapes() {
        let capability = BandCapability {
            band: WlanBand::FiveGhz,
            basic_rates: vec![0x0c, 0x12],
            ht_cap: Some(Box::new(HtCapabilities {
                bytes: [0; HT_CAP_LEN as usize],
            })),
            vht_cap: None,
            primary_channels: vec![ChannelNumber {
                band: WlanBand::FiveGhz,
                number: 36,
            }],
        };
        assert_eq!(capability.ht_cap.unwrap().bytes.len(), HT_CAP_LEN as usize);
        assert_eq!(capability.primary_channels[0].number, 36);
    }

    #[test]
    fn rsn_value_types_match_schema() {
        assert_eq!(EapolResultCode::Success as u32, 0);
        assert_eq!(EapolResultCode::TransmissionFailure as u32, 1);
        let frame = SaeFrame {
            peer_sta_address: [1, 2, 3, 4, 5, 6],
            status_code: StatusCode::SaeHashToElement,
            seq_num: 2,
            sae_fields: vec![7, 8],
        };
        assert_eq!(frame.status_code.into_primitive(), 126);
    }

    #[test]
    fn device_info_uses_common_and_ieee80211_schema_types() {
        let info = DeviceInfo {
            sta_addr: [1; 6],
            factory_addr: [2; 6],
            role: WlanMacRole::Client,
            bands: vec![],
            softmac_hardware_capability: 0x1020,
            qos_capable: true,
        };
        assert_eq!(info.role, WlanMacRole::Client);
        assert_eq!(info.factory_addr.len(), 6);
    }

    #[test]
    fn management_discriminants_and_bounds_match_schema() {
        assert_eq!(KeyType::PeerKey as u32, 3);
        assert_eq!(ScanTypes::Passive as u32, 2);
        assert_eq!(ScanResultCode::CanceledByDriverOrFirmware as u32, 5);
        assert_eq!(AuthenticationTypes::Sae as u32, 4);
        assert_eq!(AssociateResultCode::RefusedTemporarily as u32, 8);
        assert_eq!(StartResultCode::InternalError as u32, 4);
        assert_eq!(COUNTRY_ENVIRON_ALL, b' ');
        assert_eq!(WMM_PARAM_LEN, 18);
    }

    #[test]
    fn connect_and_negotiated_capabilities_preserve_nullable_boxes() {
        let request = ConnectRequest {
            selected_bss: BssDescription {
                bssid: [1; 6],
                bss_type: BssType::Infrastructure,
                beacon_period: 100,
                capability_info: 0,
                ies: vec![],
                primary: ChannelNumber {
                    band: WlanBand::FiveGhz,
                    number: 36,
                },
                bandwidth: ChannelBandwidth::Cbw80,
                vht_secondary_80_channel: ChannelNumber {
                    band: WlanBand::FiveGhz,
                    number: 0,
                },
                rssi_dbm: -40,
                snr_db: 30,
            },
            connect_failure_timeout: 20,
            auth_type: AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
            owe_public_key: Some(Box::new(OwePublicKey {
                group: 19,
                key: vec![1, 2],
            })),
        };
        assert_eq!(request.owe_public_key.unwrap().group, 19);

        let negotiated = NegotiatedCapabilities {
            primary: ChannelNumber {
                band: WlanBand::FiveGhz,
                number: 36,
            },
            capability_info: 0,
            rates: vec![12, 24],
            wmm_param: None,
            ht_cap: Some(Box::new(HtCapabilities { bytes: [0; 26] })),
            vht_cap: None,
        };
        assert_eq!(negotiated.ht_cap.unwrap().bytes.len(), 26);
    }

    #[test]
    fn extension_unions_and_minstrel_values_preserve_variants() {
        assert_eq!(
            GetIfaceStatsResponse::ErrorStatus(-2),
            GetIfaceStatsResponse::ErrorStatus(-2)
        );
        assert_eq!(ControlledPortState::Open as u32, 1);
        let response = MinstrelListResponse {
            peers: Peers {
                addrs: vec![[2; 6]],
            },
        };
        assert_eq!(response.peers.addrs[0], [2; 6]);
    }
}
