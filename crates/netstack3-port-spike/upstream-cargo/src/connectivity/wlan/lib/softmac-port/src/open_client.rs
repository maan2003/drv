// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host-portable open-network subset of Fuchsia's pinned client MLME state machine.

#[path = "../../mlme/rust/src/auth.rs"]
mod pinned_auth;

use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
use fidl_fuchsia_wlan_mlme as fidl_mlme;
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use ieee80211::{Bssid, MacAddr};
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use wlan_common::capabilities::{
    ApCapabilities, ClientCapabilities, StaCapabilities, intersect_with_ap_as_client,
};
use wlan_common::ie::rsn::rsne;
use wlan_common::ie::{self, Id};
use wlan_common::mac::{self, MgmtBody};
use wlan_common::mgmt_writer;
use wlan_common::sequence::SequenceManager;
use wlan_frame_writer::write_frame;

/// The pinned MLME uses one `TimedEvent::Connecting` deadline for both open
/// authentication and association. Its duration is the selected BSS beacon
/// period multiplied by SME's `connect_failure_timeout`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectTimer {
    pub id: u64,
    pub duration_nanos: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenClientState {
    Joined,
    Authenticating,
    Associating,
    Associated,
}

/// Source-exact device edge retained from client `BoundClient`/`DeviceOps`.
/// Implementations transport bytes and program firmware; they do not construct
/// or parse 802.11 frames and do not own connection policy.
pub trait ClientHardware {
    type Error: Error;

    fn send_mgmt_frame(&mut self, frame: Vec<u8>) -> Result<(), Self::Error>;
    fn notify_association_complete(
        &mut self,
        config: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), Self::Error>;
    fn set_ethernet_up(&mut self) -> Result<(), Self::Error>;
    fn clear_association(&mut self) -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub enum OpenConnectError<E> {
    Busy,
    NotOpenNetwork,
    NotProtectedNetwork,
    MissingSsid,
    FrameWrite,
    Hardware(E),
}

impl<E: fmt::Display> fmt::Display for OpenConnectError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("client MLME is not joined"),
            Self::NotOpenNetwork => f.write_str("request is not open-system authentication"),
            Self::NotProtectedNetwork => {
                f.write_str("request is not SAE authentication with an RSNE")
            }
            Self::MissingSsid => f.write_str("selected BSS has no valid SSID IE"),
            Self::FrameWrite => f.write_str("failed to construct management frame"),
            Self::Hardware(error) => write!(f, "client hardware operation failed: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for OpenConnectError<E> {}

/// Host-portable extraction of the pinned client MLME's open-network closure.
pub struct OpenClientMlme {
    iface_mac: MacAddr,
    request: fidl_mlme::ConnectRequest,
    capabilities: ClientCapabilities,
    state: OpenClientState,
    sequence_manager: SequenceManager,
    timer: Option<ConnectTimer>,
    events: VecDeque<fidl_mlme::MlmeEvent>,
}

impl OpenClientMlme {
    pub fn new(
        iface_mac: MacAddr,
        request: fidl_mlme::ConnectRequest,
        capabilities: ClientCapabilities,
    ) -> Self {
        Self {
            iface_mac,
            request,
            capabilities,
            state: OpenClientState::Joined,
            sequence_manager: SequenceManager::new(),
            timer: None,
            events: VecDeque::new(),
        }
    }

    pub fn state(&self) -> OpenClientState {
        self.state
    }

    pub fn connect_timer(&self) -> Option<ConnectTimer> {
        self.timer
    }

    pub fn next_mlme_event(&mut self) -> Option<fidl_mlme::MlmeEvent> {
        self.events.pop_front()
    }

    pub fn start<H: ClientHardware>(
        &mut self,
        hardware: &mut H,
    ) -> Result<(), OpenConnectError<H::Error>> {
        if self.state != OpenClientState::Joined {
            return Err(OpenConnectError::Busy);
        }
        if self.request.auth_type != fidl_mlme::AuthenticationTypes::OpenSystem
            || !self.request.security_ie.is_empty()
        {
            return Err(OpenConnectError::NotOpenNetwork);
        }
        if self.ssid().is_none() {
            return Err(OpenConnectError::MissingSsid);
        }
        self.timer = Some(ConnectTimer {
            id: 1,
            duration_nanos: i64::from(self.request.selected_bss.beacon_period)
                * i64::from(self.request.connect_failure_timeout)
                * 1_024_000,
        });
        let frame = match self.open_auth_frame() {
            Ok(frame) => frame,
            Err(()) => {
                self.finish_failure(
                    hardware,
                    fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                    true,
                )?;
                return Err(OpenConnectError::FrameWrite);
            }
        };
        if let Err(error) = hardware.send_mgmt_frame(frame) {
            self.finish_failure(
                hardware,
                fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                true,
            )?;
            return Err(OpenConnectError::Hardware(error));
        }
        self.state = OpenClientState::Authenticating;
        Ok(())
    }

    /// Continue the pinned client closure after an external SME-managed SAE
    /// exchange has authenticated the selected peer.
    ///
    /// This sends no second authentication frame and leaves the controlled
    /// port closed after association; EAPOL traffic-key installation owns the
    /// later transition to Ethernet-up.
    pub fn start_protected_association<H: ClientHardware>(
        &mut self,
        hardware: &mut H,
    ) -> Result<(), OpenConnectError<H::Error>> {
        if self.state != OpenClientState::Joined {
            return Err(OpenConnectError::Busy);
        }
        if self.request.auth_type != fidl_mlme::AuthenticationTypes::Sae
            || self.request.security_ie.is_empty()
            || self.request.security_ie[0] != Id::RSNE.0
        {
            return Err(OpenConnectError::NotProtectedNetwork);
        }
        if self.ssid().is_none() {
            return Err(OpenConnectError::MissingSsid);
        }
        self.timer = Some(ConnectTimer {
            id: 1,
            duration_nanos: i64::from(self.request.selected_bss.beacon_period)
                * i64::from(self.request.connect_failure_timeout)
                * 1_024_000,
        });
        let frame = self
            .association_request_frame()
            .map_err(|()| OpenConnectError::FrameWrite)?;
        hardware
            .send_mgmt_frame(frame)
            .map_err(OpenConnectError::Hardware)?;
        self.state = OpenClientState::Associating;
        Ok(())
    }

    /// Feed raw, unaligned 802.11 bytes from the device RX boundary.
    pub fn on_mac_frame<H: ClientHardware>(
        &mut self,
        hardware: &mut H,
        bytes: &[u8],
    ) -> Result<(), OpenConnectError<H::Error>> {
        let Some(frame) = mac::MgmtFrame::parse(bytes, false) else {
            return Ok(());
        };
        let bssid = Bssid::from(self.request.selected_bss.bssid);
        if frame.mgmt_hdr.addr3 != bssid.into()
            || (frame.mgmt_hdr.addr1.is_unicast() && frame.mgmt_hdr.addr1 != self.iface_mac)
        {
            return Ok(());
        }
        let (_, Some(body)) = frame.try_into_mgmt_body() else {
            return Ok(());
        };
        match (self.state, body) {
            (OpenClientState::Authenticating, MgmtBody::Authentication(frame)) => {
                if pinned_auth::validate_ap_resp(&frame.auth_hdr).is_err() {
                    self.finish_failure(
                        hardware,
                        fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                        true,
                    )?;
                    return Ok(());
                }
                let assoc = match self.association_request_frame() {
                    Ok(frame) => frame,
                    Err(()) => {
                        self.finish_failure(
                            hardware,
                            fidl_ieee80211::StatusCode::RefusedTemporarily,
                            false,
                        )?;
                        return Err(OpenConnectError::FrameWrite);
                    }
                };
                if let Err(error) = hardware.send_mgmt_frame(assoc) {
                    self.finish_failure(
                        hardware,
                        fidl_ieee80211::StatusCode::RefusedTemporarily,
                        false,
                    )?;
                    return Err(OpenConnectError::Hardware(error));
                }
                self.state = OpenClientState::Associating;
            }
            (OpenClientState::Associating, MgmtBody::AssociationResp(frame)) => {
                self.on_association_response(hardware, frame)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn on_timeout<H: ClientHardware>(
        &mut self,
        hardware: &mut H,
        timer_id: u64,
    ) -> Result<(), OpenConnectError<H::Error>> {
        if self.timer.is_some_and(|timer| timer.id == timer_id)
            && matches!(
                self.state,
                OpenClientState::Authenticating | OpenClientState::Associating
            )
        {
            self.finish_failure(
                hardware,
                fidl_ieee80211::StatusCode::RejectedSequenceTimeout,
                true,
            )?;
        }
        Ok(())
    }

    fn open_auth_frame(&mut self) -> Result<Vec<u8>, ()> {
        let bssid = Bssid::from(self.request.selected_bss.bssid);
        let auth = pinned_auth::make_open_client_req();
        write_frame!({
            headers: {
                mac::MgmtHdr: &mgmt_writer::mgmt_hdr_to_ap(
                    mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::MGMT)
                        .with_mgmt_subtype(mac::MgmtSubtype::AUTH),
                    bssid,
                    self.iface_mac,
                    mac::SequenceControl(0).with_seq_num(
                        self.sequence_manager.next_sns1(&bssid.into()) as u16
                    )
                ),
                mac::AuthHdr: &auth,
            },
        })
        .map_err(|_| ())
    }

    fn association_request_frame(&mut self) -> Result<Vec<u8>, ()> {
        let bssid = Bssid::from(self.request.selected_bss.bssid);
        let ssid = self.ssid().ok_or(())?.to_vec();
        let cap = &self.capabilities.0;
        let rates: Vec<u8> = cap.rates.iter().map(|rate| rate.rate()).collect();
        let ht_cap = cap.ht_cap;
        let vht_cap = cap.vht_cap;
        let rsne = (!self.request.security_ie.is_empty()
            && self.request.security_ie[0] == Id::RSNE.0)
            .then(|| rsne::from_bytes(&self.request.security_ie).map(|(_, rsne)| rsne))
            .transpose()
            .map_err(|_| ())?;
        let mut frame = write_frame!({
            headers: {
                mac::MgmtHdr: &mgmt_writer::mgmt_hdr_to_ap(
                    mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::MGMT)
                        .with_mgmt_subtype(mac::MgmtSubtype::ASSOC_REQ),
                    bssid,
                    self.iface_mac,
                    mac::SequenceControl(0).with_seq_num(
                        self.sequence_manager.next_sns1(&bssid.into()) as u16
                    )
                ),
                mac::AssocReqHdr: &mac::AssocReqHdr {
                    capabilities: cap.capability_info,
                    listen_interval: 0,
                },
            },
            ies: {
                ssid: ssid,
                supported_rates: rates,
                extended_supported_rates: {/* continue rates */},
                rsne?: rsne,
                ht_cap?: ht_cap,
                vht_cap?: vht_cap,
            },
        })
        .map_err(|_| ())?;
        if self.request.auth_type == fidl_mlme::AuthenticationTypes::Sae {
            if let Some(rsnxe) = self.selected_h2e_rsnxe()? {
                frame.extend_from_slice(rsnxe);
            }
        }
        Ok(frame)
    }

    fn selected_h2e_rsnxe(&self) -> Result<Option<&[u8]>, ()> {
        let ies = &self.request.selected_bss.ies;
        let mut cursor = 0;
        let mut rsnxe = None;
        while cursor < ies.len() {
            let header = ies.get(cursor..cursor + 2).ok_or(())?;
            let next = cursor.checked_add(2 + usize::from(header[1])).ok_or(())?;
            let raw = ies.get(cursor..next).ok_or(())?;
            if header[0] == Id::RSNXE.0 {
                if rsnxe.is_some() || header[1] != 1 {
                    return Err(());
                }
                rsnxe = Some(raw);
            }
            cursor = next;
        }
        Ok(rsnxe.filter(|raw| raw[2] & 0x20 != 0))
    }

    fn ssid(&self) -> Option<&[u8]> {
        ie::Reader::new(&self.request.selected_bss.ies[..])
            .find_map(|(id, body)| (id == Id::SSID).then_some(body))
            .filter(|ssid| ssid.len() <= usize::from(fidl_ieee80211::MAX_SSID_BYTE_LEN))
    }

    fn on_association_response<H: ClientHardware>(
        &mut self,
        hardware: &mut H,
        frame: mac::AssocRespFrame<&[u8]>,
    ) -> Result<(), OpenConnectError<H::Error>> {
        let status = Option::<fidl_ieee80211::StatusCode>::from(frame.assoc_resp_hdr.status_code)
            .unwrap_or(fidl_ieee80211::StatusCode::RefusedReasonUnspecified);
        if status != fidl_ieee80211::StatusCode::Success {
            self.finish_failure(hardware, status, false)?;
            return Ok(());
        }
        let mut ap = StaCapabilities {
            capability_info: frame.assoc_resp_hdr.capabilities,
            rates: vec![],
            ht_cap: None,
            vht_cap: None,
        };
        let mut ht_op = None;
        let mut vht_op = None;
        for (id, body) in frame.ies() {
            match id {
                Id::SUPPORTED_RATES => {
                    if let Ok(rates) = ie::parse_supported_rates(body) {
                        ap.rates.extend(rates.iter().copied());
                    }
                }
                Id::EXTENDED_SUPPORTED_RATES => {
                    if let Ok(rates) = ie::parse_extended_supported_rates(body) {
                        ap.rates.extend(rates.iter().copied());
                    }
                }
                Id::HT_CAPABILITIES => {
                    if let Ok(cap) = ie::parse_ht_capabilities(body) {
                        ap.ht_cap = Some(*cap);
                    }
                }
                Id::VHT_CAPABILITIES => {
                    if let Ok(cap) = ie::parse_vht_capabilities(body) {
                        ap.vht_cap = Some(*cap);
                    }
                }
                Id::HT_OPERATION => {
                    if let Ok(op) = ie::parse_ht_operation(body) {
                        ht_op = Some(*op);
                    }
                }
                Id::VHT_OPERATION => {
                    if let Ok(op) = ie::parse_vht_operation(body) {
                        vht_op = Some(*op);
                    }
                }
                _ => {}
            }
        }
        let Ok(negotiated) = intersect_with_ap_as_client(&self.capabilities, &ApCapabilities(ap))
        else {
            self.finish_failure(
                hardware,
                fidl_ieee80211::StatusCode::RefusedCapabilitiesMismatch,
                false,
            )?;
            return Ok(());
        };
        let aid = frame.assoc_resp_hdr.aid;
        let association_ies = frame.elements.to_vec();
        let config = fidl_softmac::WlanAssociationConfig {
            bssid: Some(self.request.selected_bss.bssid),
            aid: Some(aid),
            listen_interval: Some(0),
            primary: Some(self.request.selected_bss.primary),
            bandwidth: Some(self.request.selected_bss.bandwidth),
            vht_secondary_80_channel: Some(self.request.selected_bss.vht_secondary_80_channel),
            qos: Some(negotiated.ht_cap.is_some()),
            rates: Some(negotiated.rates.iter().map(|rate| rate.0).collect()),
            capability_info: Some(negotiated.capability_info.raw()),
            ht_cap: negotiated.ht_cap.map(Into::into),
            ht_op: ht_op.map(Into::into),
            vht_cap: negotiated.vht_cap.map(Into::into),
            vht_op: vht_op.map(Into::into),
            ..Default::default()
        };
        if hardware.notify_association_complete(config).is_err() {
            self.finish_failure(
                hardware,
                fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                false,
            )?;
            return Ok(());
        }
        if self.request.security_ie.is_empty() {
            // Open networks do not require EAPOL, so the pinned MLME opens the
            // controlled port immediately. Failure is intentionally non-fatal.
            let _ = hardware.set_ethernet_up();
        }
        self.timer = None;
        self.state = OpenClientState::Associated;
        self.events.push_back(fidl_mlme::MlmeEvent::ConnectConf {
            resp: fidl_mlme::ConnectConfirm {
                peer_sta_address: self.request.selected_bss.bssid,
                result_code: fidl_ieee80211::StatusCode::Success,
                association_id: aid,
                association_ies,
            },
        });
        Ok(())
    }

    fn finish_failure<H: ClientHardware>(
        &mut self,
        hardware: &mut H,
        status: fidl_ieee80211::StatusCode,
        clear_association: bool,
    ) -> Result<(), OpenConnectError<H::Error>> {
        self.timer = None;
        self.state = OpenClientState::Joined;
        self.events.push_back(fidl_mlme::MlmeEvent::ConnectConf {
            resp: fidl_mlme::ConnectConfirm {
                peer_sta_address: self.request.selected_bss.bssid,
                result_code: status,
                association_id: 0,
                association_ies: vec![],
            },
        });
        if clear_association {
            hardware
                .clear_association()
                .map_err(OpenConnectError::Hardware)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_wlan_ieee80211::{
        BssDescription, BssType, ChannelBandwidth, ChannelNumber, WlanBand,
    };
    use wlan_common::ie::SupportedRate;

    const CLIENT: [u8; 6] = [2, 2, 2, 2, 2, 2];
    const AP: [u8; 6] = [6, 6, 6, 6, 6, 6];
    const WPA3_SAE_RSNE: &[u8] = &[
        48, 20, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 8, 204, 0,
    ];

    #[derive(Debug)]
    struct FakeError;

    impl fmt::Display for FakeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("fixture failure")
        }
    }

    impl Error for FakeError {}

    #[derive(Default)]
    struct FakeHardware {
        frames: Vec<Vec<u8>>,
        associations: Vec<fidl_softmac::WlanAssociationConfig>,
        clears: usize,
        ethernet_up: usize,
        fail_send: bool,
        fail_association: bool,
    }

    impl ClientHardware for FakeHardware {
        type Error = FakeError;

        fn send_mgmt_frame(&mut self, frame: Vec<u8>) -> Result<(), Self::Error> {
            if self.fail_send {
                return Err(FakeError);
            }
            self.frames.push(frame);
            Ok(())
        }

        fn notify_association_complete(
            &mut self,
            config: fidl_softmac::WlanAssociationConfig,
        ) -> Result<(), Self::Error> {
            if self.fail_association {
                return Err(FakeError);
            }
            self.associations.push(config);
            Ok(())
        }

        fn set_ethernet_up(&mut self) -> Result<(), Self::Error> {
            self.ethernet_up += 1;
            Ok(())
        }

        fn clear_association(&mut self) -> Result<(), Self::Error> {
            self.clears += 1;
            Ok(())
        }
    }

    fn request() -> fidl_mlme::ConnectRequest {
        fidl_mlme::ConnectRequest {
            selected_bss: BssDescription {
                bssid: AP,
                bss_type: BssType::Infrastructure,
                beacon_period: 100,
                capability_info: 1,
                ies: vec![
                    Id::SSID.0,
                    4,
                    b't',
                    b'e',
                    b's',
                    b't',
                    Id::SUPPORTED_RATES.0,
                    2,
                    0x82,
                    0x84,
                ],
                primary: ChannelNumber {
                    band: WlanBand::TwoGhz,
                    number: 6,
                },
                bandwidth: ChannelBandwidth::Cbw20,
                vht_secondary_80_channel: ChannelNumber {
                    band: WlanBand::TwoGhz,
                    number: 0,
                },
                rssi_dbm: -30,
                snr_db: 20,
            },
            connect_failure_timeout: 20,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
            owe_public_key: None,
        }
    }

    fn capabilities(rates: Vec<u8>) -> ClientCapabilities {
        ClientCapabilities(StaCapabilities {
            capability_info: mac::CapabilityInfo(1),
            rates: rates.into_iter().map(SupportedRate).collect(),
            ht_cap: None,
            vht_cap: None,
        })
    }

    fn client() -> OpenClientMlme {
        OpenClientMlme::new(CLIENT.into(), request(), capabilities(vec![0x82, 0x84]))
    }

    fn auth_response(bssid: [u8; 6], status: fidl_ieee80211::StatusCode) -> Vec<u8> {
        write_frame!({
            headers: {
                mac::MgmtHdr: &mgmt_writer::mgmt_hdr_from_ap(
                    mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::MGMT)
                        .with_mgmt_subtype(mac::MgmtSubtype::AUTH),
                    CLIENT.into(),
                    bssid.into(),
                    mac::SequenceControl(0),
                ),
                mac::AuthHdr: &mac::AuthHdr {
                    auth_alg_num: mac::AuthAlgorithmNumber::OPEN,
                    auth_txn_seq_num: 2,
                    status_code: status.into(),
                },
            },
        })
        .unwrap()
    }

    fn association_response(status: fidl_ieee80211::StatusCode, rates: &[u8]) -> Vec<u8> {
        write_frame!({
            headers: {
                mac::MgmtHdr: &mgmt_writer::mgmt_hdr_from_ap(
                    mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::MGMT)
                        .with_mgmt_subtype(mac::MgmtSubtype::ASSOC_RESP),
                    CLIENT.into(),
                    AP.into(),
                    mac::SequenceControl(0),
                ),
                mac::AssocRespHdr: &mac::AssocRespHdr {
                    capabilities: mac::CapabilityInfo(1),
                    status_code: status.into(),
                    aid: 42,
                },
            },
            ies: {
                supported_rates: rates,
            },
        })
        .unwrap()
    }

    fn connect_status(client: &mut OpenClientMlme) -> fidl_ieee80211::StatusCode {
        match client.next_mlme_event().unwrap() {
            fidl_mlme::MlmeEvent::ConnectConf { resp } => resp.result_code,
            event => panic!("unexpected MLME event: {event:?}"),
        }
    }

    // Closure fixture derived from pinned client state/bound tests: auth
    // request, valid open response, association request, response, device
    // programming, controlled-port open, and CONNECT.confirm.
    #[test]
    fn open_network_connect_closes_over_fake_hardware() {
        let mut client = client();
        let mut hardware = FakeHardware::default();
        client.start(&mut hardware).unwrap();
        assert_eq!(client.state(), OpenClientState::Authenticating);
        assert_eq!(
            client.connect_timer(),
            Some(ConnectTimer {
                id: 1,
                duration_nanos: 2_048_000_000
            })
        );

        let (_, auth) = mac::MgmtFrame::parse(&hardware.frames[0][..], false)
            .unwrap()
            .try_into_mgmt_body();
        let MgmtBody::Authentication(auth) = auth.unwrap() else {
            panic!("not auth")
        };
        let got = *auth.auth_hdr;
        let expected = pinned_auth::make_open_client_req();
        let got_alg = got.auth_alg_num;
        let got_txn = got.auth_txn_seq_num;
        let got_status = got.status_code;
        let expected_alg = expected.auth_alg_num;
        let expected_txn = expected.auth_txn_seq_num;
        let expected_status = expected.status_code;
        assert_eq!(got_alg, expected_alg);
        assert_eq!(got_txn, expected_txn);
        assert_eq!(got_status, expected_status);

        client
            .on_mac_frame(
                &mut hardware,
                &auth_response(AP, fidl_ieee80211::StatusCode::Success),
            )
            .unwrap();
        assert_eq!(client.state(), OpenClientState::Associating);
        let (_, assoc) = mac::MgmtFrame::parse(&hardware.frames[1][..], false)
            .unwrap()
            .try_into_mgmt_body();
        let MgmtBody::AssociationReq(assoc) = assoc.unwrap() else {
            panic!("not assoc request")
        };
        let listen_interval = assoc.assoc_req_hdr.listen_interval;
        assert_eq!(listen_interval, 0);
        assert_eq!(assoc.ies().next(), Some((Id::SSID, &b"test"[..])));

        client
            .on_mac_frame(
                &mut hardware,
                &association_response(fidl_ieee80211::StatusCode::Success, &[0x82, 0x84]),
            )
            .unwrap();
        assert_eq!(client.state(), OpenClientState::Associated);
        assert_eq!(client.connect_timer(), None);
        assert_eq!(hardware.associations.len(), 1);
        assert_eq!(hardware.associations[0].aid, Some(42));
        assert_eq!(hardware.associations[0].rates, Some(vec![0x82, 0x84]));
        assert_eq!(hardware.ethernet_up, 1);
        assert_eq!(
            connect_status(&mut client),
            fidl_ieee80211::StatusCode::Success
        );
    }

    #[test]
    fn sae_authenticated_client_associates_with_rsne_and_keeps_port_closed() {
        let mut protected = request();
        protected.auth_type = fidl_mlme::AuthenticationTypes::Sae;
        protected.security_ie = WPA3_SAE_RSNE.to_vec();
        protected
            .selected_bss
            .ies
            .extend_from_slice(&[244, 1, 0x20]);
        let mut client =
            OpenClientMlme::new(CLIENT.into(), protected, capabilities(vec![0x82, 0x84]));
        let mut hardware = FakeHardware::default();

        client.start_protected_association(&mut hardware).unwrap();
        assert_eq!(client.state(), OpenClientState::Associating);
        assert_eq!(hardware.frames.len(), 1);
        let (_, Some(MgmtBody::AssociationReq(assoc))) =
            mac::MgmtFrame::parse(&hardware.frames[0][..], false)
                .unwrap()
                .try_into_mgmt_body()
        else {
            panic!("not association request")
        };
        assert!(
            assoc
                .ies()
                .any(|(id, body)| { id == Id::RSNE && body == &WPA3_SAE_RSNE[2..] })
        );
        assert_eq!(
            assoc.ies().collect::<Vec<_>>().last(),
            Some(&(Id::RSNXE, &[0x20][..]))
        );

        client
            .on_mac_frame(
                &mut hardware,
                &association_response(fidl_ieee80211::StatusCode::Success, &[0x82, 0x84]),
            )
            .unwrap();
        assert_eq!(client.state(), OpenClientState::Associated);
        assert_eq!(hardware.associations.len(), 1);
        assert_eq!(hardware.ethernet_up, 0);
    }

    #[test]
    fn sae_h2e_rejects_malformed_or_duplicate_selected_bss_rsnxe() {
        for suffix in [
            vec![244, 0],
            vec![244, 2, 0x20, 0],
            vec![244, 1, 0x20, 244, 1, 0x20],
            vec![244, 1],
        ] {
            let mut protected = request();
            protected.auth_type = fidl_mlme::AuthenticationTypes::Sae;
            protected.security_ie = WPA3_SAE_RSNE.to_vec();
            protected.selected_bss.ies.extend_from_slice(&suffix);
            let mut client =
                OpenClientMlme::new(CLIENT.into(), protected, capabilities(vec![0x82, 0x84]));
            assert!(matches!(
                client.start_protected_association(&mut FakeHardware::default()),
                Err(OpenConnectError::FrameWrite)
            ));
        }
    }

    #[test]
    fn wrong_bssid_is_ignored_and_invalid_auth_fails_with_cleanup() {
        let mut client = client();
        let mut hardware = FakeHardware::default();
        client.start(&mut hardware).unwrap();
        client
            .on_mac_frame(
                &mut hardware,
                &auth_response([8; 6], fidl_ieee80211::StatusCode::Success),
            )
            .unwrap();
        assert_eq!(client.state(), OpenClientState::Authenticating);
        client
            .on_mac_frame(
                &mut hardware,
                &auth_response(AP, fidl_ieee80211::StatusCode::RefusedReasonUnspecified),
            )
            .unwrap();
        assert_eq!(client.state(), OpenClientState::Joined);
        assert_eq!(hardware.clears, 1);
        assert_eq!(
            connect_status(&mut client),
            fidl_ieee80211::StatusCode::RefusedReasonUnspecified
        );
    }

    #[test]
    fn association_rejection_and_capability_mismatch_are_source_exact_failures() {
        let mut client = client();
        let mut hardware = FakeHardware::default();
        client.start(&mut hardware).unwrap();
        client
            .on_mac_frame(
                &mut hardware,
                &auth_response(AP, fidl_ieee80211::StatusCode::Success),
            )
            .unwrap();
        let rejected = fidl_ieee80211::StatusCode::from_primitive(17).unwrap();
        client
            .on_mac_frame(&mut hardware, &association_response(rejected, &[0x82]))
            .unwrap();
        assert_eq!(connect_status(&mut client), rejected);

        let mut mismatch = OpenClientMlme::new(CLIENT.into(), request(), capabilities(vec![0x82]));
        mismatch.start(&mut hardware).unwrap();
        mismatch
            .on_mac_frame(
                &mut hardware,
                &auth_response(AP, fidl_ieee80211::StatusCode::Success),
            )
            .unwrap();
        mismatch
            .on_mac_frame(
                &mut hardware,
                &association_response(fidl_ieee80211::StatusCode::Success, &[0x84]),
            )
            .unwrap();
        assert_eq!(connect_status(&mut mismatch).into_primitive(), 10);
        assert!(hardware.associations.is_empty());
    }

    #[test]
    fn one_connect_timer_covers_authentication_and_association() {
        let mut authenticating = client();
        let mut hardware = FakeHardware::default();
        authenticating.start(&mut hardware).unwrap();
        authenticating.on_timeout(&mut hardware, 99).unwrap();
        assert_eq!(authenticating.state(), OpenClientState::Authenticating);
        authenticating.on_timeout(&mut hardware, 1).unwrap();
        assert_eq!(authenticating.state(), OpenClientState::Joined);
        assert_eq!(
            connect_status(&mut authenticating),
            fidl_ieee80211::StatusCode::RejectedSequenceTimeout
        );

        let mut associating = client();
        associating.start(&mut hardware).unwrap();
        associating
            .on_mac_frame(
                &mut hardware,
                &auth_response(AP, fidl_ieee80211::StatusCode::Success),
            )
            .unwrap();
        associating.on_timeout(&mut hardware, 1).unwrap();
        assert_eq!(
            connect_status(&mut associating),
            fidl_ieee80211::StatusCode::RejectedSequenceTimeout
        );
    }

    #[test]
    fn transport_and_device_programming_fail_closed() {
        let mut hardware = FakeHardware {
            fail_send: true,
            ..Default::default()
        };
        let mut send_failure = client();
        assert!(matches!(
            send_failure.start(&mut hardware),
            Err(OpenConnectError::Hardware(_))
        ));
        assert_eq!(send_failure.state(), OpenClientState::Joined);
        assert_eq!(hardware.clears, 1);

        let mut hardware = FakeHardware::default();
        let mut assoc_send_failure = client();
        assoc_send_failure.start(&mut hardware).unwrap();
        hardware.fail_send = true;
        assert!(matches!(
            assoc_send_failure.on_mac_frame(
                &mut hardware,
                &auth_response(AP, fidl_ieee80211::StatusCode::Success)
            ),
            Err(OpenConnectError::Hardware(_))
        ));
        assert_eq!(assoc_send_failure.state(), OpenClientState::Joined);

        let mut hardware = FakeHardware {
            fail_association: true,
            ..Default::default()
        };
        let mut program_failure = client();
        program_failure.start(&mut hardware).unwrap();
        program_failure
            .on_mac_frame(
                &mut hardware,
                &auth_response(AP, fidl_ieee80211::StatusCode::Success),
            )
            .unwrap();
        program_failure
            .on_mac_frame(
                &mut hardware,
                &association_response(fidl_ieee80211::StatusCode::Success, &[0x82, 0x84]),
            )
            .unwrap();
        assert_eq!(program_failure.state(), OpenClientState::Joined);
        assert_eq!(
            connect_status(&mut program_failure),
            fidl_ieee80211::StatusCode::RefusedReasonUnspecified
        );
    }
}
