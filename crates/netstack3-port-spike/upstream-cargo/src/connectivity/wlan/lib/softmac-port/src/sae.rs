// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Narrow host wrapper around the pinned Fuchsia SME-managed SAE supplicant.

use fidl_fuchsia_wlan_common::{MfpFeature, SaeFeature, SecuritySupport};
use fidl_fuchsia_wlan_ieee80211::StatusCode;
use fidl_fuchsia_wlan_mlme::SaeFrame;
use ieee80211::{MacAddr, MacAddrBytes, Ssid};
use wlan_common::ie::rsn::rsne;
use wlan_common::mac;
use wlan_common::mgmt_writer;
use wlan_frame_writer::write_frame;
use wlan_rsn::auth;
use wlan_rsn::nonce::NonceReader;
use wlan_rsn::rsna::{AuthStatus, SecAssocUpdate, UpdateSink};
use wlan_rsn::{ProtectionInfo, PweMethod, Supplicant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaeHandshakeUpdate {
    TxFrame(SaeFrame),
    ScheduleTimeout(u64),
    Authenticated,
    Rejected,
}

/// Owns secret-bearing pinned SAE state. This type deliberately implements no
/// `Debug` and exposes only management-frame and timer updates.
pub struct SaeHandshake {
    supplicant: Supplicant,
    peer: MacAddr,
}

impl SaeHandshake {
    /// Build the same SME-managed WPA3 supplicant selected by pinned
    /// `client/protection.rs`. `authenticator_rsne` includes the IE id/length.
    pub fn new(
        ssid: Vec<u8>,
        password: Vec<u8>,
        client: MacAddr,
        peer: MacAddr,
        authenticator_rsne: &[u8],
        hash_to_element_supported: bool,
    ) -> Result<Self, anyhow::Error> {
        let (_, authenticator) = rsne::from_bytes(authenticator_rsne)
            .map_err(|error| anyhow::format_err!("invalid authenticator RSNE: {error:?}"))?;
        let support = SecuritySupport {
            sae: Some(SaeFeature {
                driver_handler_supported: Some(false),
                sme_handler_supported: Some(true),
                hash_to_element_supported: Some(hash_to_element_supported),
            }),
            mfp: Some(MfpFeature {
                supported: Some(true),
            }),
            ..Default::default()
        };
        let supplicant_rsne = authenticator.derive_wpa3_s_rsne(&support)?;
        let pwe_method = if hash_to_element_supported {
            PweMethod::Direct
        } else {
            PweMethod::Loop
        };
        let supplicant = Supplicant::new_wpa_personal(
            NonceReader::new(&client)?,
            auth::Config::Sae {
                ssid: Ssid::try_from(ssid)?,
                password,
                mac: client,
                peer_mac: peer,
                pwe_method,
            },
            client,
            ProtectionInfo::Rsne(supplicant_rsne),
            peer,
            ProtectionInfo::Rsne(authenticator),
        )?;
        Ok(Self { supplicant, peer })
    }

    /// Mirrors SME startup followed by MLME's `OnSaeHandshakeInd` event.
    pub fn start(&mut self) -> Result<Vec<SaeHandshakeUpdate>, wlan_rsn::Error> {
        let mut sink = UpdateSink::default();
        self.supplicant.start(&mut sink)?;
        self.supplicant.on_sae_handshake_ind(&mut sink)?;
        Ok(convert_updates(sink))
    }

    pub fn on_frame_rx(
        &mut self,
        seq_num: u16,
        status_code: StatusCode,
        sae_fields: Vec<u8>,
    ) -> Result<Vec<SaeHandshakeUpdate>, wlan_rsn::Error> {
        let mut sink = UpdateSink::default();
        self.supplicant.on_sae_frame_rx(
            &mut sink,
            SaeFrame {
                peer_sta_address: self.peer.to_array(),
                status_code,
                seq_num,
                sae_fields,
            },
        )?;
        Ok(convert_updates(sink))
    }

    pub fn on_timeout(&mut self, id: u64) -> Result<Vec<SaeHandshakeUpdate>, wlan_rsn::Error> {
        let mut sink = UpdateSink::default();
        self.supplicant.on_sae_timeout(&mut sink, id)?;
        Ok(convert_updates(sink))
    }
}

fn convert_updates(updates: UpdateSink) -> Vec<SaeHandshakeUpdate> {
    updates
        .into_iter()
        .filter_map(|update| match update {
            SecAssocUpdate::TxSaeFrame(frame) => Some(SaeHandshakeUpdate::TxFrame(frame)),
            SecAssocUpdate::ScheduleSaeTimeout(id) => Some(SaeHandshakeUpdate::ScheduleTimeout(id)),
            SecAssocUpdate::SaeAuthStatus(AuthStatus::Success) => {
                Some(SaeHandshakeUpdate::Authenticated)
            }
            SecAssocUpdate::SaeAuthStatus(_) => Some(SaeHandshakeUpdate::Rejected),
            // Association/EAPOL/key updates are outside this boundary and are
            // intentionally unreachable before SAE authentication succeeds.
            _ => None,
        })
        .collect()
}

/// Exact pinned `BoundClient::send_auth_frame` shape for an SME-generated SAE
/// commit/confirm. The caller owns only transport and the sequence counter.
pub fn build_sae_auth_frame(
    client: MacAddr,
    peer: MacAddr,
    sequence_control: u16,
    frame: &SaeFrame,
) -> Result<Vec<u8>, anyhow::Error> {
    if frame.peer_sta_address != peer.to_array() {
        anyhow::bail!("SAE peer does not match selected BSSID");
    }
    Ok(write_frame!({
        headers: {
            mac::MgmtHdr: &mgmt_writer::mgmt_hdr_to_ap(
                mac::FrameControl(0)
                    .with_frame_type(mac::FrameType::MGMT)
                    .with_mgmt_subtype(mac::MgmtSubtype::AUTH),
                peer.into(),
                client,
                mac::SequenceControl(0).with_seq_num(sequence_control),
            ),
            mac::AuthHdr: &mac::AuthHdr {
                auth_alg_num: mac::AuthAlgorithmNumber::SAE,
                auth_txn_seq_num: frame.seq_num,
                status_code: frame.status_code.into(),
            },
        },
        body: &frame.sae_fields,
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured public RSN IE shape from the controlled target: CCMP/SAE with
    // management-frame protection required. No credential material.
    const WPA3_SAE_RSNE: &[u8] = &[
        48, 20, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 8, 204, 0,
    ];

    #[test]
    fn pinned_supplicant_emits_sae_commit_and_timer_without_association_updates() {
        let peer = MacAddr::from([6; 6]);
        let mut handshake = SaeHandshake::new(
            b"fixture".to_vec(),
            b"fixture passphrase".to_vec(),
            MacAddr::from([2; 6]),
            peer,
            WPA3_SAE_RSNE,
            false,
        )
        .unwrap();
        let updates = handshake.start().unwrap();
        assert!(updates.iter().any(|update| matches!(
            update,
            SaeHandshakeUpdate::TxFrame(SaeFrame {
                peer_sta_address,
                seq_num: 1,
                status_code: StatusCode::Success,
                sae_fields,
            }) if *peer_sta_address == peer.to_array() && !sae_fields.is_empty()
        )));
        assert!(
            updates
                .iter()
                .any(|update| matches!(update, SaeHandshakeUpdate::ScheduleTimeout(_)))
        );
        assert!(!updates.iter().any(|update| matches!(
            update,
            SaeHandshakeUpdate::Authenticated | SaeHandshakeUpdate::Rejected
        )));
    }

    #[test]
    fn stale_timeout_is_ignored_by_pinned_sae_counter() {
        let mut handshake = SaeHandshake::new(
            b"fixture".to_vec(),
            b"fixture passphrase".to_vec(),
            MacAddr::from([2; 6]),
            MacAddr::from([6; 6]),
            WPA3_SAE_RSNE,
            false,
        )
        .unwrap();
        let _ = handshake.start().unwrap();
        assert!(handshake.on_timeout(u64::MAX).unwrap().is_empty());
    }

    #[test]
    fn sae_update_uses_pinned_bound_client_frame_shape() {
        let frame = SaeFrame {
            peer_sta_address: [6; 6],
            status_code: StatusCode::Success,
            seq_num: 1,
            sae_fields: vec![9, 8, 7],
        };
        let bytes =
            build_sae_auth_frame(MacAddr::from([2; 6]), MacAddr::from([6; 6]), 1, &frame).unwrap();
        let parsed = mac::MgmtFrame::parse(&bytes[..], false).unwrap();
        assert_eq!(parsed.mgmt_hdr.addr1, MacAddr::from([6; 6]));
        assert_eq!(parsed.mgmt_hdr.addr2, MacAddr::from([2; 6]));
        let (_, Some(mac::MgmtBody::Authentication(auth))) = parsed.try_into_mgmt_body() else {
            panic!("not authentication")
        };
        let algorithm = auth.auth_hdr.auth_alg_num;
        let sequence = auth.auth_hdr.auth_txn_seq_num;
        assert_eq!(algorithm, mac::AuthAlgorithmNumber::SAE);
        assert_eq!(sequence, 1);
        assert_eq!(auth.elements, &[9, 8, 7]);
    }
}
