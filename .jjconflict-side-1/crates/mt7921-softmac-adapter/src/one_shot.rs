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

struct ConnectionSequence {
    generation: u64,
    completed: Option<OneShotStage>,
}

impl ConnectionSequence {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            completed: None,
        }
    }

    fn advance(&mut self, stage: OneShotStage) -> Result<(), OneShotError> {
        let expected = match self.completed {
            None => OneShotStage::Pmk,
            Some(OneShotStage::Pmk) => OneShotStage::Association,
            Some(OneShotStage::Association) => OneShotStage::FourWay,
            Some(OneShotStage::FourWay) => OneShotStage::Authorized,
            Some(OneShotStage::Authorized) => OneShotStage::Dhcp,
            Some(OneShotStage::Dhcp) => OneShotStage::Dns,
            Some(OneShotStage::Dns) => OneShotStage::Tcp,
            Some(OneShotStage::Tcp) => OneShotStage::Http,
            Some(OneShotStage::Http) => {
                return Err(fail(stage, "connection proof already complete"));
            }
        };
        if stage != expected {
            return Err(fail(stage, "connection stage out of order"));
        }
        self.completed = Some(stage);
        Ok(())
    }

    fn require_generation(&self, actual: u64, stage: OneShotStage) -> Result<(), OneShotError> {
        if actual == self.generation {
            Ok(())
        } else {
            Err(fail(stage, "generation changed during connection"))
        }
    }
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
    let mut sequence = ConnectionSequence::new(generation);
    sequence.advance(OneShotStage::Pmk)?;

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
        sequence.advance(OneShotStage::Association)?;

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
        sequence.advance(OneShotStage::FourWay)?;
        effects
            .set_controlled_port(true)
            .map_err(|_| fail(OneShotStage::Authorized, "controlled port refused"))?;
        sequence.require_generation(effects.generation(), OneShotStage::Authorized)?;
        sequence.advance(OneShotStage::Authorized)?;
        run_authorized_data_proof(effects, &mut sequence, deadline)
    })();
    drop(pmk);
    match result {
        Ok(()) => Ok(()),
        Err(error) => teardown(effects, error),
    }
}

fn run_authorized_data_proof<E: OneShotWpa3Effects>(
    effects: &mut E,
    sequence: &mut ConnectionSequence,
    deadline: Instant,
) -> Result<(), OneShotError> {
    for (proof, stage) in [
        (DataProofStage::Dhcp, OneShotStage::Dhcp),
        (DataProofStage::Dns, OneShotStage::Dns),
        (DataProofStage::Tcp, OneShotStage::Tcp),
        (DataProofStage::Http, OneShotStage::Http),
    ] {
        sequence.require_generation(effects.generation(), stage)?;
        sequence.advance(stage)?;
        effects
            .run_data_proof(proof, deadline)
            .map_err(|_| fail(stage, "data proof failed"))?;
        sequence.require_generation(effects.generation(), stage)?;
    }
    Ok(())
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
    use std::fmt;

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

    #[derive(Clone, Copy, Debug)]
    struct FakeError;

    impl fmt::Display for FakeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("injected connection effect failure")
        }
    }

    impl std::error::Error for FakeError {}

    struct SimulatedEffects {
        generation: u64,
        authorized: bool,
        fail: Option<OneShotStage>,
        reconnect_during: Option<DataProofStage>,
        calls: Vec<&'static str>,
    }

    impl SimulatedEffects {
        fn new() -> Self {
            Self {
                generation: 1,
                authorized: false,
                fail: None,
                reconnect_during: None,
                calls: vec![],
            }
        }

        fn hit(&mut self, stage: OneShotStage, call: &'static str) -> Result<(), FakeError> {
            self.calls.push(call);
            if self.fail == Some(stage) {
                Err(FakeError)
            } else {
                Ok(())
            }
        }
    }

    impl ClientHardware for SimulatedEffects {
        type Error = FakeError;

        fn send_mgmt_frame(&mut self, _: Vec<u8>) -> Result<(), Self::Error> {
            self.hit(OneShotStage::Association, "association-request")
        }

        fn notify_association_complete(
            &mut self,
            _: fidl_softmac::WlanAssociationConfig,
        ) -> Result<(), Self::Error> {
            self.hit(OneShotStage::Association, "association")
        }

        fn set_ethernet_up(&mut self) -> Result<(), Self::Error> {
            Err(FakeError)
        }

        fn clear_association(&mut self) -> Result<(), Self::Error> {
            self.authorized = false;
            self.calls.push("clear-association");
            Ok(())
        }
    }

    impl OneShotWpa3Effects for SimulatedEffects {
        fn generation(&self) -> u64 {
            self.generation
        }

        fn receive_management(&mut self, _: Instant) -> Result<ConnectionRx, Self::Error> {
            Err(FakeError)
        }

        fn send_eapol(&mut self, _: &[u8]) -> Result<(), Self::Error> {
            self.hit(OneShotStage::FourWay, "eapol")
        }

        fn receive_eapol(&mut self, _: Instant) -> Result<ConnectionRx, Self::Error> {
            Err(FakeError)
        }

        fn install_key(
            &mut self,
            _: &fidl_softmac::WlanKeyConfiguration,
        ) -> Result<(), Self::Error> {
            self.hit(OneShotStage::FourWay, "key")
        }

        fn set_controlled_port(&mut self, open: bool) -> Result<(), Self::Error> {
            if !open {
                self.authorized = false;
                self.calls.push("port-down");
                return Ok(());
            }
            self.hit(OneShotStage::Authorized, "authorized")?;
            self.authorized = true;
            Ok(())
        }

        fn run_data_proof(&mut self, stage: DataProofStage, _: Instant) -> Result<(), Self::Error> {
            if !self.authorized {
                self.calls.push("data-before-authorization");
                return Err(FakeError);
            }
            let (connection_stage, call) = match stage {
                DataProofStage::Dhcp => (OneShotStage::Dhcp, "dhcp"),
                DataProofStage::Dns => (OneShotStage::Dns, "dns"),
                DataProofStage::Tcp => (OneShotStage::Tcp, "tcp"),
                DataProofStage::Http => (OneShotStage::Http, "http"),
            };
            self.hit(connection_stage, call)?;
            if self.reconnect_during == Some(stage) {
                self.generation += 1;
                self.authorized = false;
            }
            Ok(())
        }
    }

    fn simulated_run(effects: &mut SimulatedEffects) -> Result<(), OneShotError> {
        let mut sequence = ConnectionSequence::new(effects.generation());
        let result = (|| {
            effects.calls.push("sae");
            sequence.advance(OneShotStage::Pmk)?;
            effects
                .notify_association_complete(fidl_softmac::WlanAssociationConfig::default())
                .map_err(|_| fail(OneShotStage::Association, "association failed"))?;
            sequence.advance(OneShotStage::Association)?;
            effects
                .install_key(&fidl_softmac::WlanKeyConfiguration::default())
                .map_err(|_| fail(OneShotStage::FourWay, "key failed"))?;
            sequence.advance(OneShotStage::FourWay)?;
            effects
                .set_controlled_port(true)
                .map_err(|_| fail(OneShotStage::Authorized, "authorization failed"))?;
            sequence.advance(OneShotStage::Authorized)?;
            run_authorized_data_proof(
                effects,
                &mut sequence,
                Instant::now() + std::time::Duration::from_secs(1),
            )
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => teardown(effects, error),
        }
    }

    #[test]
    fn simulated_end_to_end_orders_sae_association_four_way_authorization_and_data() {
        let mut effects = SimulatedEffects::new();
        simulated_run(&mut effects).unwrap();
        assert_eq!(
            effects.calls,
            [
                "sae",
                "association",
                "key",
                "authorized",
                "dhcp",
                "dns",
                "tcp",
                "http"
            ]
        );
    }

    #[test]
    fn data_cannot_run_before_controlled_port_authorization() {
        let mut effects = SimulatedEffects::new();
        let mut sequence = ConnectionSequence::new(1);
        sequence.advance(OneShotStage::Pmk).unwrap();
        sequence.advance(OneShotStage::Association).unwrap();
        sequence.advance(OneShotStage::FourWay).unwrap();
        assert_eq!(
            run_authorized_data_proof(
                &mut effects,
                &mut sequence,
                Instant::now() + std::time::Duration::from_secs(1)
            ),
            Err(fail(OneShotStage::Dhcp, "connection stage out of order"))
        );
        assert!(effects.calls.is_empty());
    }

    #[test]
    fn every_downstream_failure_closes_port_clears_association_and_stops() {
        for stage in [
            OneShotStage::Association,
            OneShotStage::FourWay,
            OneShotStage::Authorized,
            OneShotStage::Dhcp,
            OneShotStage::Dns,
            OneShotStage::Tcp,
            OneShotStage::Http,
        ] {
            let mut effects = SimulatedEffects::new();
            effects.fail = Some(stage);
            assert_eq!(simulated_run(&mut effects).unwrap_err().stage, stage);
            assert_eq!(
                &effects.calls[effects.calls.len() - 2..],
                ["port-down", "clear-association"]
            );
            assert!(!effects.authorized);
            let terminal = match stage {
                OneShotStage::Association => "association",
                OneShotStage::FourWay => "key",
                OneShotStage::Authorized => "authorized",
                OneShotStage::Dhcp => "dhcp",
                OneShotStage::Dns => "dns",
                OneShotStage::Tcp => "tcp",
                OneShotStage::Http => "http",
                _ => unreachable!(),
            };
            assert_eq!(
                effects
                    .calls
                    .iter()
                    .filter(|call| **call == terminal)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn disconnect_reconnect_invalidates_old_generation_and_new_run_can_authorize() {
        let mut effects = SimulatedEffects::new();
        effects.reconnect_during = Some(DataProofStage::Dns);
        let error = simulated_run(&mut effects).unwrap_err();
        assert_eq!(error.stage, OneShotStage::Dns);
        assert_eq!(effects.generation, 2);
        assert!(!effects.calls.contains(&"tcp"));
        assert!(!effects.authorized);

        effects.calls.clear();
        effects.reconnect_during = None;
        simulated_run(&mut effects).unwrap();
        assert_eq!(effects.calls.last(), Some(&"http"));
        assert!(effects.authorized);
    }
}
