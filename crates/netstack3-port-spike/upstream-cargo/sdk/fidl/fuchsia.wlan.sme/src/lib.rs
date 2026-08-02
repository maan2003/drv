// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host subset generated from the pinned `fuchsia.wlan.sme` schema.

use fidl_fuchsia_wlan_common::{ScanType, WlanMacRole};
use fidl_fuchsia_wlan_ieee80211::{
    BssDescription, ChannelBandwidth, ChannelNumber, MacAddr, ReasonCode, Ssid, StatusCode,
    WlanPhyType,
};
use fidl_fuchsia_wlan_internal::{
    Authentication, ChannelSwitchInfo, Protocol, SignalReportIndication, WmmStatusResponse,
};

pub type ClientSmeWmmStatusResult = Result<WmmStatusResponse, i32>;

/// Host boundary for the response token retained by core SME disconnect state.
/// Fuchsia endpoint construction remains in the feature-gated serving module.
#[derive(Debug, Default)]
pub struct ClientSmeDisconnectResponder;

impl ClientSmeDisconnectResponder {
    pub fn send(self) -> Result<(), fidl::Error> {
        Err(fidl::Error::TransportUnavailable)
    }
}

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
#[repr(u32)]
pub enum UserDisconnectReason {
    Unknown = 0,
    FailedToConnect = 1,
    FidlConnectRequest = 2,
    FidlStopClientConnectionsRequest = 3,
    ProactiveNetworkSwitch = 4,
    DisconnectDetectedFromSme = 5,
    RegulatoryRegionChange = 6,
    Startup = 7,
    NetworkUnsaved = 8,
    NetworkConfigUpdated = 9,
    Recovery = 10,
    WlanstackUnitTesting = 124,
    WlanSmeUnitTesting = 125,
    WlanServiceUtilTesting = 126,
    WlanDevTool = 127,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DisconnectMlmeEventName {
    DeauthenticateIndication = 1,
    DisassociateIndication = 2,
    RoamStartIndication = 3,
    RoamResultIndication = 4,
    SaeHandshakeResponse = 5,
    RoamRequest = 6,
    RoamConfirmation = 7,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisconnectCause {
    pub mlme_event_name: DisconnectMlmeEventName,
    pub reason_code: ReasonCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisconnectSource {
    Ap(DisconnectCause),
    User(UserDisconnectReason),
    Mlme(DisconnectCause),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ScanErrorCode {
    NotSupported = 1,
    InternalError = 2,
    InternalMlmeError = 3,
    ShouldWait = 4,
    CanceledByDriverOrFirmware = 5,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScanRequest {
    Active(ActiveScanRequest),
    Passive(PassiveScanRequest),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PassiveScanRequest {
    pub channels: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveScanRequest {
    pub ssids: Vec<Ssid>,
    pub channels: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectResult {
    pub code: StatusCode,
    pub is_credential_rejected: bool,
    pub is_reconnect: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisconnectInfo {
    pub is_sme_reconnecting: bool,
    pub disconnect_source: DisconnectSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoamResult {
    pub bssid: MacAddr,
    pub status_code: StatusCode,
    pub original_association_maintained: bool,
    pub bss_description: Option<Box<BssDescription>>,
    pub disconnect_info: Option<Box<DisconnectInfo>>,
    pub is_credential_rejected: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Empty {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectRequest {
    pub ssid: Ssid,
    pub bss_description: BssDescription,
    pub multiple_bss_candidates: bool,
    pub authentication: Authentication,
    pub deprecated_scan_type: ScanType,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoamRequest {
    pub bss_description: BssDescription,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServingApInfo {
    pub bssid: MacAddr,
    pub ssid: Ssid,
    pub rssi_dbm: i8,
    pub snr_db: i8,
    pub primary: ChannelNumber,
    pub protection: Protection,
    pub bandwidth: ChannelBandwidth,
    pub vht_secondary_80_channel: ChannelNumber,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientStatusResponse {
    Connected(ServingApInfo),
    Connecting(Ssid),
    Idle(Empty),
    Roaming(MacAddr),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectTransactionEvent {
    OnConnectResult { result: ConnectResult },
    OnDisconnect { info: DisconnectInfo },
    OnRoamResult { result: RoamResult },
    OnSignalReport { ind: SignalReportIndication },
    OnChannelSwitched { info: ChannelSwitchInfo },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApConfig {
    pub ssid: Ssid,
    pub password: Vec<u8>,
    pub radio_cfg: RadioConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum StartApResultCode {
    Success = 0,
    AlreadyStarted = 1,
    InternalError = 2,
    Canceled = 3,
    TimedOut = 4,
    PreviousStartInProgress = 5,
    InvalidArguments = 6,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum StopApResultCode {
    Success = 0,
    InternalError = 1,
    TimedOut = 2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ap {
    pub ssid: Ssid,
    pub channel: u8,
    pub num_clients: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApStatusResponse {
    pub running_ap: Option<Box<Ap>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegacyPrivacySupport {
    pub wep_supported: bool,
    pub wpa1_supported: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenericSmeQuery {
    pub role: WlanMacRole,
    pub sta_addr: MacAddr,
    pub factory_addr: MacAddr,
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
        assert_eq!(UserDisconnectReason::Recovery as u32, 10);
        assert_eq!(UserDisconnectReason::WlanDevTool as u32, 127);
        assert_eq!(DisconnectMlmeEventName::RoamConfirmation as u32, 7);
        assert_eq!(ScanErrorCode::CanceledByDriverOrFirmware as u32, 5);
        assert_eq!(StartApResultCode::InvalidArguments as u32, 6);
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

    #[test]
    fn disconnect_and_roam_preserve_strict_union_variants_and_nullable_boxes() {
        let source = DisconnectSource::Mlme(DisconnectCause {
            mlme_event_name: DisconnectMlmeEventName::RoamResultIndication,
            reason_code: fidl_fuchsia_wlan_ieee80211::ReasonCode::MicFailure,
        });
        let info = DisconnectInfo {
            is_sme_reconnecting: false,
            disconnect_source: source,
        };
        let result = RoamResult {
            bssid: [3; 6],
            status_code: StatusCode::RefusedReasonUnspecified,
            original_association_maintained: false,
            bss_description: None,
            disconnect_info: Some(Box::new(info)),
            is_credential_rejected: false,
        };
        assert!(matches!(
            result.disconnect_info.unwrap().disconnect_source,
            DisconnectSource::Mlme(_)
        ));
    }

    #[test]
    fn protocol_event_values_preserve_payloads_without_transport() {
        let event = ConnectTransactionEvent::OnSignalReport {
            ind: SignalReportIndication {
                rssi_dbm: -50,
                snr_db: 20,
            },
        };
        assert_eq!(
            event,
            ConnectTransactionEvent::OnSignalReport {
                ind: SignalReportIndication {
                    rssi_dbm: -50,
                    snr_db: 20
                }
            }
        );
        assert_eq!(ApStatusResponse { running_ap: None }.running_ap, None);
        assert_eq!(
            ClientStatusResponse::Idle(Empty {}),
            ClientStatusResponse::Idle(Empty {})
        );
    }
}
