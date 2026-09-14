// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Narrow host wrapper around the pinned Fuchsia SME-managed SAE supplicant.

use fidl_fuchsia_wlan_common::{MfpFeature, SaeFeature, SecuritySupport};
use fidl_fuchsia_wlan_ieee80211::StatusCode;
use fidl_fuchsia_wlan_mlme::{EapolResultCode, SaeFrame};
use ieee80211::{MacAddr, MacAddrBytes, Ssid};
use wlan_common::ie::parse_rsnxe;
use wlan_common::ie::rsn::rsne;
use wlan_common::mac;
use wlan_common::mgmt_writer;
use wlan_common::security::wpa::credential::Passphrase;
use wlan_frame_writer::write_frame;
use wlan_rsn::auth;
use wlan_rsn::key::Tk;
use wlan_rsn::key::exchange::Key;
use wlan_rsn::nonce::NonceReader;
use wlan_rsn::rsna::{AuthStatus, SecAssocUpdate, UpdateSink};
use wlan_rsn::{ProtectionInfo, PweMethod, Supplicant};

pub enum SaeHandshakeUpdate {
    TxFrame(SaeFrame),
    ScheduleTimeout {
        id: u64,
        duration_millis: u64,
    },
    Authenticated,
    Pmk(SaePmk),
    TxEapolKeyFrame {
        frame: Vec<u8>,
        expect_response: bool,
    },
    TrafficKey(SaeTrafficKey),
    EssSaEstablished,
    Rejected,
}

/// One secret-bearing PMK produced by the pinned SAE supplicant.
///
/// The key is deliberately neither clonable nor printable. Callers may expose
/// it only for the duration of a closure, and dropping it overwrites the
/// retained allocation before release.
pub struct SaePmk(Vec<u8>);

impl SaePmk {
    pub fn expose<T>(&self, use_key: impl FnOnce(&[u8]) -> T) -> T {
        use_key(&self.0)
    }
}

impl Drop for SaePmk {
    fn drop(&mut self) {
        self.0.fill(0);
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaeTrafficKeyKind {
    Pairwise,
    Group,
    IntegrityGroup,
}

/// A non-printable traffic-key installation request from the pinned
/// supplicant. Metadata is public; key bytes remain borrow-only and are
/// overwritten on drop.
pub struct SaeTrafficKey {
    bytes: Vec<u8>,
    pub kind: SaeTrafficKeyKind,
    pub cipher_oui: [u8; 3],
    pub cipher_type: u8,
    pub key_id: u16,
    pub rsc: u64,
}

impl SaeTrafficKey {
    pub fn expose<T>(&self, use_key: impl FnOnce(&[u8]) -> T) -> T {
        use_key(&self.bytes)
    }
}

impl Drop for SaeTrafficKey {
    fn drop(&mut self) {
        self.bytes.fill(0);
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// Owns secret-bearing pinned SAE state. This type deliberately implements no
/// `Debug` and exposes only management-frame and timer updates.
pub struct SaeHandshake {
    supplicant: Supplicant,
    client: MacAddr,
    peer: MacAddr,
}

pub const SAE_RETRANSMISSION_TIMEOUT_MILLIS: u64 = 1000;

impl SaeHandshake {
    /// Build the same SME-managed WPA3 supplicant selected by pinned
    /// `client/protection.rs`. The authenticator IEs include their id/length.
    pub fn new(
        ssid: Vec<u8>,
        password: Vec<u8>,
        client: MacAddr,
        peer: MacAddr,
        authenticator_rsne: &[u8],
        authenticator_rsnxe: Option<&[u8]>,
        hash_to_element_supported: bool,
    ) -> Result<Self, anyhow::Error> {
        let peer_hash_to_element_supported = match authenticator_rsnxe {
            None => false,
            Some(rsnxe)
                if rsnxe.len() >= 3 && rsnxe[0] == 244 && rsnxe[1] as usize == rsnxe.len() - 2 =>
            {
                parse_rsnxe(&rsnxe[2..])
                    .rsnxe_octet_1
                    .map(|octet| octet.sae_hash_to_element())
                    .unwrap_or(false)
            }
            Some(_) => anyhow::bail!("invalid authenticator RSNXE"),
        };
        let password: Vec<u8> = Passphrase::try_from(password.as_slice())?.into();
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
        let pwe_method = if peer_hash_to_element_supported && hash_to_element_supported {
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
        Ok(Self {
            supplicant,
            client,
            peer,
        })
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
        receiver: MacAddr,
        transmitter: MacAddr,
        bssid: MacAddr,
        algorithm: u16,
        seq_num: u16,
        status_code: StatusCode,
        sae_fields: Vec<u8>,
    ) -> Result<Vec<SaeHandshakeUpdate>, wlan_rsn::Error> {
        if receiver != self.client
            || transmitter != self.peer
            || bssid != self.peer
            || algorithm != mac::AuthAlgorithmNumber::SAE.0
        {
            return Ok(vec![]);
        }
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

    /// Process one EAPOL-Key PDU after association. SAE uses a 128-bit MIC in
    /// the controlled WPA3-Personal closure packaged here.
    pub fn on_eapol_frame(
        &mut self,
        bytes: &[u8],
    ) -> Result<Vec<SaeHandshakeUpdate>, anyhow::Error> {
        let frame = eapol::KeyFrameRx::parse(16, bytes)?;
        let mut sink = UpdateSink::default();
        self.supplicant
            .on_eapol_frame(&mut sink, eapol::Frame::Key(frame))?;
        Ok(convert_updates(sink))
    }

    pub fn on_eapol_tx_confirm(
        &mut self,
        success: bool,
    ) -> Result<Vec<SaeHandshakeUpdate>, wlan_rsn::Error> {
        let mut sink = UpdateSink::default();
        self.supplicant.on_eapol_conf(
            &mut sink,
            if success {
                EapolResultCode::Success
            } else {
                EapolResultCode::TransmissionFailure
            },
        )?;
        Ok(convert_updates(sink))
    }
}

fn convert_updates(updates: UpdateSink) -> Vec<SaeHandshakeUpdate> {
    updates
        .into_iter()
        .filter_map(|update| match update {
            SecAssocUpdate::TxSaeFrame(frame) => Some(SaeHandshakeUpdate::TxFrame(frame)),
            SecAssocUpdate::ScheduleSaeTimeout(id) => Some(SaeHandshakeUpdate::ScheduleTimeout {
                id,
                duration_millis: SAE_RETRANSMISSION_TIMEOUT_MILLIS,
            }),
            SecAssocUpdate::SaeAuthStatus(AuthStatus::Success) => {
                Some(SaeHandshakeUpdate::Authenticated)
            }
            SecAssocUpdate::SaeAuthStatus(_) => Some(SaeHandshakeUpdate::Rejected),
            SecAssocUpdate::Key(Key::Pmk(pmk)) => Some(SaeHandshakeUpdate::Pmk(SaePmk(pmk))),
            SecAssocUpdate::TxEapolKeyFrame {
                frame,
                expect_response,
            } => Some(SaeHandshakeUpdate::TxEapolKeyFrame {
                frame: frame.into(),
                expect_response,
            }),
            SecAssocUpdate::Key(Key::Ptk(mut ptk)) => {
                let bytes = ptk.tk().to_vec();
                let cipher_oui = ptk.cipher.oui.into();
                let cipher_type = ptk.cipher.suite_type;
                ptk.ptk.fill(0);
                Some(SaeHandshakeUpdate::TrafficKey(SaeTrafficKey {
                    bytes,
                    kind: SaeTrafficKeyKind::Pairwise,
                    cipher_oui,
                    cipher_type,
                    key_id: 0,
                    rsc: 0,
                }))
            }
            SecAssocUpdate::Key(Key::Gtk(mut gtk)) => {
                let bytes = gtk.tk().to_vec();
                let cipher_oui = gtk.cipher().oui.into();
                let cipher_type = gtk.cipher().suite_type;
                let key_id = u16::from(gtk.key_id());
                let rsc = gtk.key_rsc();
                gtk.bytes.fill(0);
                Some(SaeHandshakeUpdate::TrafficKey(SaeTrafficKey {
                    bytes,
                    kind: SaeTrafficKeyKind::Group,
                    cipher_oui,
                    cipher_type,
                    key_id,
                    rsc,
                }))
            }
            SecAssocUpdate::Key(Key::Igtk(mut igtk)) => {
                let bytes = igtk.tk().to_vec();
                let cipher_oui = igtk.cipher.oui.into();
                let cipher_type = igtk.cipher.suite_type;
                let key_id = igtk.key_id;
                let mut rsc_bytes = [0; 8];
                rsc_bytes[2..].copy_from_slice(&igtk.ipn);
                let rsc = u64::from_be_bytes(rsc_bytes);
                igtk.igtk.fill(0);
                Some(SaeHandshakeUpdate::TrafficKey(SaeTrafficKey {
                    bytes,
                    kind: SaeTrafficKeyKind::IntegrityGroup,
                    cipher_oui,
                    cipher_type,
                    key_id,
                    rsc,
                }))
            }
            SecAssocUpdate::Status(wlan_rsn::rsna::SecAssocStatus::EssSaEstablished) => {
                Some(SaeHandshakeUpdate::EssSaEstablished)
            }
            // Unsupported status and key families remain outside this narrow
            // WPA3-Personal client boundary.
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
    const SAE_H2E_RSNXE: &[u8] = &[244, 1, 0x20];

    #[test]
    fn pinned_supplicant_emits_sae_commit_and_timer_without_association_updates() {
        let peer = MacAddr::from([6; 6]);
        let mut handshake = SaeHandshake::new(
            b"fixture".to_vec(),
            b"fixture passphrase".to_vec(),
            MacAddr::from([2; 6]),
            peer,
            WPA3_SAE_RSNE,
            None,
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
        assert!(updates.iter().any(|update| matches!(
            update,
            SaeHandshakeUpdate::ScheduleTimeout {
                duration_millis: SAE_RETRANSMISSION_TIMEOUT_MILLIS,
                ..
            }
        )));
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
            None,
            false,
        )
        .unwrap();
        let _ = handshake.start().unwrap();
        assert!(handshake.on_timeout(u64::MAX).unwrap().is_empty());
    }

    #[test]
    fn pmk_update_is_secret_bearing_and_available_only_by_borrow() {
        let updates = convert_updates(vec![SecAssocUpdate::Key(Key::Pmk(vec![7; 32]))]);
        let Some(SaeHandshakeUpdate::Pmk(pmk)) = updates.into_iter().next() else {
            panic!("PMK update was discarded")
        };
        assert_eq!(pmk.expose(|bytes| bytes.len()), 32);
        assert!(pmk.expose(|bytes| bytes.iter().all(|byte| *byte == 7)));
    }

    #[test]
    fn traffic_key_handoff_separates_metadata_from_borrow_only_bytes() {
        let key = SaeTrafficKey {
            bytes: vec![9; 16],
            kind: SaeTrafficKeyKind::Pairwise,
            cipher_oui: [0, 15, 172],
            cipher_type: 4,
            key_id: 0,
            rsc: 0,
        };
        assert_eq!(key.kind, SaeTrafficKeyKind::Pairwise);
        assert_eq!(key.cipher_oui, [0, 15, 172]);
        assert_eq!(key.expose(|bytes| bytes.len()), 16);
        assert!(key.expose(|bytes| bytes.iter().all(|byte| *byte == 9)));
    }

    #[test]
    fn h2e_peer_and_local_support_emit_direct_group_20_commit() {
        let mut handshake = SaeHandshake::new(
            b"fixture".to_vec(),
            b"fixture passphrase".to_vec(),
            MacAddr::from([2; 6]),
            MacAddr::from([6; 6]),
            WPA3_SAE_RSNE,
            Some(SAE_H2E_RSNXE),
            true,
        )
        .unwrap();
        let updates = handshake.start().unwrap();
        assert!(updates.iter().any(|update| matches!(
            update,
            SaeHandshakeUpdate::TxFrame(SaeFrame {
                seq_num: 1,
                status_code: StatusCode::SaeHashToElement,
                sae_fields,
                ..
            }) if sae_fields.len() == 146 && sae_fields[..2] == [20, 0]
        )));
    }

    #[test]
    fn h2e_requires_both_peer_and_local_support() {
        for (rsnxe, local_h2e) in [(Some(SAE_H2E_RSNXE), false), (None, true)] {
            let mut handshake = SaeHandshake::new(
                b"fixture".to_vec(),
                b"fixture passphrase".to_vec(),
                MacAddr::from([2; 6]),
                MacAddr::from([6; 6]),
                WPA3_SAE_RSNE,
                rsnxe,
                local_h2e,
            )
            .unwrap();
            assert!(handshake.start().unwrap().iter().any(|update| matches!(
                update,
                SaeHandshakeUpdate::TxFrame(SaeFrame {
                    seq_num: 1,
                    status_code: StatusCode::Success,
                    sae_fields,
                    ..
                }) if sae_fields.len() == 98 && sae_fields[..2] == [19, 0]
            )));
        }
    }

    #[test]
    fn malformed_peer_rsnxe_is_rejected() {
        for rsnxe in [&[244, 2, 0x20][..], &[48, 1, 0x20][..], &[244, 0][..]] {
            assert!(
                SaeHandshake::new(
                    b"fixture".to_vec(),
                    b"fixture passphrase".to_vec(),
                    MacAddr::from([2; 6]),
                    MacAddr::from([6; 6]),
                    WPA3_SAE_RSNE,
                    Some(rsnxe),
                    true,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn foreign_or_non_sae_authentication_frames_never_reach_supplicant() {
        let client = MacAddr::from([2; 6]);
        let peer = MacAddr::from([6; 6]);
        let mut handshake = SaeHandshake::new(
            b"fixture".to_vec(),
            b"fixture passphrase".to_vec(),
            client,
            peer,
            WPA3_SAE_RSNE,
            None,
            false,
        )
        .unwrap();
        let _ = handshake.start().unwrap();
        for (receiver, transmitter, bssid, algorithm) in [
            (MacAddr::from([3; 6]), peer, peer, 3),
            (client, MacAddr::from([4; 6]), peer, 3),
            (client, peer, MacAddr::from([5; 6]), 3),
            (client, peer, peer, 0),
        ] {
            assert!(
                handshake
                    .on_frame_rx(
                        receiver,
                        transmitter,
                        bssid,
                        algorithm,
                        1,
                        StatusCode::Success,
                        vec![1, 2, 3],
                    )
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn sae_update_uses_pinned_bound_client_frame_shape() {
        let mut sae_fields = vec![19, 0];
        sae_fields.extend([9; 32 + 64]);
        let frame = SaeFrame {
            peer_sta_address: [6; 6],
            status_code: StatusCode::SaeHashToElement,
            seq_num: 1,
            sae_fields: sae_fields.clone(),
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
        let status = auth.auth_hdr.status_code;
        assert_eq!(algorithm, mac::AuthAlgorithmNumber::SAE);
        assert_eq!(sequence, 1);
        assert_eq!(status, StatusCode::SaeHashToElement.into());
        assert_eq!(auth.elements, sae_fields);
    }
}
