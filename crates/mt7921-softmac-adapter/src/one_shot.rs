// SPDX-License-Identifier: GPL-2.0-only

//! Bounded orchestration for the single WPA3 connection proof.
//!
//! Frame construction/parsing and cryptography stay in the pinned Fuchsia
//! client modules.  This module only orders their effects and rejects stale
//! physical responses.

use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use fuchsia_softmac_port::{
    ClientHardware, OpenClientMlme, OpenClientState, SaeHandshake, SaeHandshakeUpdate,
    SaeTrafficKeyKind,
};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataProofStage {
    Dhcp,
    Dns,
    Tcp,
    Http,
}

pub struct ConnectionRx {
    pub generation: u64,
    pub peer: [u8; 6],
    pub channel: fidl_ieee80211::ChannelNumber,
    pub bytes: Vec<u8>,
}

/// Physical and Netstack effects required by orchestration. Implementations
/// must complete or fail each operation synchronously; they own no RSN policy.
pub trait OneShotWpa3Effects: ClientHardware {
    fn generation(&self) -> u64;
    fn receive_management(&mut self, deadline: Instant) -> Result<ConnectionRx, Self::Error>;
    fn send_eapol(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    fn receive_eapol(&mut self, deadline: Instant) -> Result<ConnectionRx, Self::Error>;
    fn install_key(&mut self, key: &fidl_softmac::WlanKeyConfiguration) -> Result<(), Self::Error>;
    fn set_controlled_port(&mut self, open: bool) -> Result<(), Self::Error>;
    fn run_data_proof(
        &mut self,
        stage: DataProofStage,
        deadline: Instant,
    ) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OneShotStage {
    Pmk,
    Association,
    FourWay,
    Authorized,
    Dhcp,
    Dns,
    Tcp,
    Http,
}

#[derive(Debug, Eq, PartialEq)]
pub struct OneShotError {
    pub stage: OneShotStage,
    pub reason: &'static str,
}

fn fail(stage: OneShotStage, reason: &'static str) -> OneShotError {
    OneShotError { stage, reason }
}

/// Continue an already-authenticated SAE supplicant through association,
/// EAPOL, authorization, and the bounded Netstack proof. The PMK update is
/// retained (and therefore zeroized only after the run) while the same pinned
/// supplicant processes the four-way handshake.
pub fn run_one_shot_wpa3<E: OneShotWpa3Effects>(
    handshake: &mut SaeHandshake,
    authenticated_updates: Vec<SaeHandshakeUpdate>,
    association: &mut OpenClientMlme,
    effects: &mut E,
    peer: [u8; 6],
    channel: fidl_ieee80211::ChannelNumber,
    deadline: Instant,
) -> Result<(), OneShotError> {
    let generation = effects.generation();
    let mut authenticated = false;
    let mut pmk = None;
    for update in authenticated_updates {
        match update {
            SaeHandshakeUpdate::Authenticated => authenticated = true,
            SaeHandshakeUpdate::Pmk(value) => pmk = Some(value),
            SaeHandshakeUpdate::Rejected => {
                return teardown(effects, fail(OneShotStage::Pmk, "SAE rejected"));
            }
            _ => {}
        }
    }
    if !authenticated || pmk.is_none() {
        return teardown(
            effects,
            fail(OneShotStage::Pmk, "authenticated SAE did not yield PMK"),
        );
    }

    let result = (|| {
        association
            .start_protected_association(effects)
            .map_err(|_| fail(OneShotStage::Association, "association request failed"))?;
        while association.state() != OpenClientState::Associated {
            if Instant::now() >= deadline {
                return Err(fail(OneShotStage::Association, "association deadline"));
            }
            let rx = effects
                .receive_management(deadline)
                .map_err(|_| fail(OneShotStage::Association, "association receive failed"))?;
            validate_rx(&rx, generation, peer, channel, OneShotStage::Association)?;
            association
                .on_mac_frame(effects, &rx.bytes)
                .map_err(|_| fail(OneShotStage::Association, "association response failed"))?;
        }

        let mut established = false;
        for _ in 0..64 {
            if established {
                break;
            }
            if Instant::now() >= deadline {
                return Err(fail(OneShotStage::FourWay, "four-way deadline"));
            }
            let rx = effects
                .receive_eapol(deadline)
                .map_err(|_| fail(OneShotStage::FourWay, "EAPOL receive failed"))?;
            validate_rx(&rx, generation, peer, channel, OneShotStage::FourWay)?;
            let updates = handshake
                .on_eapol_frame(&rx.bytes)
                .map_err(|_| fail(OneShotStage::FourWay, "pinned supplicant rejected EAPOL"))?;
            established = apply_updates(handshake, effects, peer, updates)?;
        }
        if !established {
            return Err(fail(OneShotStage::FourWay, "four-way did not establish"));
        }
        effects
            .set_controlled_port(true)
            .map_err(|_| fail(OneShotStage::Authorized, "controlled port refused"))?;
        if effects.generation() != generation {
            return Err(fail(
                OneShotStage::Authorized,
                "generation changed before authorization",
            ));
        }
        for (proof, stage) in [
            (DataProofStage::Dhcp, OneShotStage::Dhcp),
            (DataProofStage::Dns, OneShotStage::Dns),
            (DataProofStage::Tcp, OneShotStage::Tcp),
            (DataProofStage::Http, OneShotStage::Http),
        ] {
            if effects.generation() != generation {
                return Err(fail(stage, "generation changed during data proof"));
            }
            effects
                .run_data_proof(proof, deadline)
                .map_err(|_| fail(stage, "data proof failed"))?;
            if effects.generation() != generation {
                return Err(fail(stage, "generation changed during data proof"));
            }
        }
        Ok(())
    })();
    drop(pmk);
    match result {
        Ok(()) => Ok(()),
        Err(error) => teardown(effects, error),
    }
}

fn validate_rx(
    rx: &ConnectionRx,
    generation: u64,
    peer: [u8; 6],
    channel: fidl_ieee80211::ChannelNumber,
    stage: OneShotStage,
) -> Result<(), OneShotError> {
    if rx.generation != generation || rx.peer != peer || rx.channel != channel {
        Err(fail(stage, "stale or foreign response"))
    } else {
        Ok(())
    }
}

fn apply_updates<E: OneShotWpa3Effects>(
    handshake: &mut SaeHandshake,
    effects: &mut E,
    peer: [u8; 6],
    mut updates: Vec<SaeHandshakeUpdate>,
) -> Result<bool, OneShotError> {
    let mut established = false;
    while let Some(update) = updates.pop() {
        match update {
            SaeHandshakeUpdate::TxEapolKeyFrame { frame, .. } => {
                effects
                    .send_eapol(&frame)
                    .map_err(|_| fail(OneShotStage::FourWay, "EAPOL TX failed"))?;
                updates.extend(
                    handshake
                        .on_eapol_tx_confirm(true)
                        .map_err(|_| fail(OneShotStage::FourWay, "EAPOL confirmation failed"))?,
                );
            }
            SaeHandshakeUpdate::TrafficKey(key) => {
                let (kind, address) = match key.kind {
                    SaeTrafficKeyKind::Pairwise => (fidl_ieee80211::KeyType::Pairwise, peer),
                    SaeTrafficKeyKind::Group => (fidl_ieee80211::KeyType::Group, [0xff; 6]),
                    SaeTrafficKeyKind::IntegrityGroup => (fidl_ieee80211::KeyType::Igtk, [0xff; 6]),
                };
                let config = key.expose(|bytes| fidl_softmac::WlanKeyConfiguration {
                    protection: Some(fidl_softmac::WlanProtection::RxTx),
                    cipher_oui: Some(key.cipher_oui),
                    cipher_type: Some(key.cipher_type),
                    key_type: Some(kind),
                    peer_addr: Some(address),
                    key_idx: Some(key.key_id as u8),
                    key: Some(bytes.to_vec()),
                    rsc: Some(key.rsc),
                });
                effects
                    .install_key(&config)
                    .map_err(|_| fail(OneShotStage::FourWay, "key install failed"))?;
            }
            SaeHandshakeUpdate::EssSaEstablished => established = true,
            SaeHandshakeUpdate::Rejected => {
                return Err(fail(OneShotStage::FourWay, "supplicant rejected four-way"));
            }
            _ => {}
        }
    }
    Ok(established)
}

fn teardown<E: OneShotWpa3Effects>(
    effects: &mut E,
    error: OneShotError,
) -> Result<(), OneShotError> {
    let _ = effects.set_controlled_port(false);
    let _ = effects.clear_association();
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(number: u8) -> fidl_ieee80211::ChannelNumber {
        fidl_ieee80211::ChannelNumber {
            band: fidl_ieee80211::WlanBand::TwoGhz,
            number,
        }
    }

    #[test]
    fn response_identity_is_exact_and_generation_scoped() {
        let expected = ConnectionRx {
            generation: 7,
            peer: [6; 6],
            channel: channel(6),
            bytes: vec![],
        };
        assert_eq!(
            validate_rx(&expected, 7, [6; 6], channel(6), OneShotStage::Association),
            Ok(())
        );
        for stale in [
            ConnectionRx {
                generation: 8,
                ..ConnectionRx {
                    generation: 7,
                    peer: [6; 6],
                    channel: channel(6),
                    bytes: vec![],
                }
            },
            ConnectionRx {
                peer: [8; 6],
                ..ConnectionRx {
                    generation: 7,
                    peer: [6; 6],
                    channel: channel(6),
                    bytes: vec![],
                }
            },
            ConnectionRx {
                channel: channel(11),
                ..ConnectionRx {
                    generation: 7,
                    peer: [6; 6],
                    channel: channel(6),
                    bytes: vec![],
                }
            },
        ] {
            assert_eq!(
                validate_rx(&stale, 7, [6; 6], channel(6), OneShotStage::Association),
                Err(fail(OneShotStage::Association, "stale or foreign response"))
            );
        }
    }
}
