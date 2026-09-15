//! Station firmware state and bounded observations needed for PHY/RA setup.
//! Authentication and information-element semantics beyond rate translation
//! remain with MLME/SME; this owner never verifies SAE or authorizes Ethernet.

use crate::radio::{FirmwareCommands, MacPreparation, RadioResponse};
use drv_hardware::Backend;
use mt7921_core::{CandidateChannel, Connac2RxFrame, PhysicalBand};
use std::time::{Duration, Instant};

pub(super) const OFDM_RATES: &[u8] = &[12, 18, 24, 36, 48, 72, 96, 108];
pub(super) const OBSERVATION_CAPACITY: usize = 64;

#[derive(Clone)]
pub(super) struct ObservedBss {
    pub bssid: [u8; 6],
    pub channel: CandidateChannel,
    pub beacon_period: u16,
    dtim_period: Option<u8>,
    observed_at: Instant,
    basic_rates: u16,
    legacy_rates: u16,
}

impl ObservedBss {
    pub fn from_rx(frame: &Connac2RxFrame, now: Instant) -> Option<Self> {
        let bytes = &frame.bytes;
        let control = u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?);
        if !matches!(control & 0x00fc, 0x0080 | 0x0050) || bytes.len() < 36 {
            return None;
        }
        let bssid: [u8; 6] = bytes[16..22].try_into().ok()?;
        if bssid == [0; 6] || bssid[0] & 1 != 0 || bytes[10..16] != bssid {
            return None;
        }
        // Only infrastructure advertisements can establish a client peer.
        if u16::from_le_bytes([bytes[34], bytes[35]]) & 3 != 1 {
            return None;
        }
        let mut rates = Vec::new();
        let mut dtim_period = None;
        let mut ies = &bytes[36..];
        while !ies.is_empty() {
            let header = ies.get(..2)?;
            let body = ies.get(2..2 + usize::from(header[1]))?;
            if matches!(header[0], 1 | 50) {
                if rates.len() + body.len() > 32 {
                    return None;
                }
                rates.extend_from_slice(body);
            }
            if header[0] == 5 {
                // TIM is beacon provenance, never an invented probe-response
                // default. Count must be smaller than the advertised period.
                if control & 0x00fc != 0x0080
                    || body.len() < 4
                    || body[1] == 0
                    || body[0] >= body[1]
                    || dtim_period.replace(body[1]).is_some()
                {
                    return None;
                }
            }
            ies = &ies[2 + usize::from(header[1])..];
        }
        if rates
            .iter()
            .any(|rate| rate & 0x80 != 0 && !OFDM_RATES.contains(&(rate & 0x7f)))
        {
            return None;
        }
        let (band, frequency_mhz) = match frame.band {
            PhysicalBand::Ghz2 => (
                0,
                if frame.channel == 14 {
                    2484
                } else {
                    2407 + u16::from(frame.channel) * 5
                },
            ),
            PhysicalBand::Ghz5 => (1, 5000 + u16::from(frame.channel) * 5),
            PhysicalBand::Ghz6 => return None,
        };
        let (basic_rates, legacy_rates) =
            mt7921_core::linux_preauth_rate_context_reference(band, OFDM_RATES, &rates).ok()?;
        let beacon_period = u16::from_le_bytes([bytes[32], bytes[33]]);
        if beacon_period == 0 {
            return None;
        }
        Some(Self {
            bssid,
            beacon_period,
            dtim_period,
            observed_at: now,
            basic_rates,
            legacy_rates,
            channel: CandidateChannel {
                band: frame.band,
                number: u16::from(frame.channel),
                frequency_mhz,
            },
        })
    }

    pub fn management_rate(&self) -> u8 {
        let ofdm = if self.channel.band == PhysicalBand::Ghz2 {
            self.basic_rates >> 4
        } else {
            self.basic_rates
        };
        OFDM_RATES[ofdm.trailing_zeros() as usize]
    }

    pub fn fresh(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.observed_at) < Duration::from_secs(30)
    }
}

/// Linux DECLARE_EWMA(rssi, 10, 8), retained for the selected interface.
/// Only addressed authentication/association responses contribute, not scans.
#[derive(Default)]
pub(super) struct AssociationRssi(u32);

impl AssociationRssi {
    pub fn observe(&mut self, frame: &Connac2RxFrame, local: [u8; 6], peer: [u8; 6]) {
        let bytes = &frame.bytes;
        if bytes.len() < 24
            || frame.rssi_dbm > 0
            || !matches!(bytes[0] & 0xfc, 0xb0 | 0x10)
            || bytes[4..10] != local
            || bytes[10..16] != peer
            || bytes[16..22] != peer
        {
            return;
        }
        let sample = u32::from(-i16::from(frame.rssi_dbm) as u16) << 10;
        self.0 = if self.0 == 0 {
            sample
        } else {
            (7 * self.0 + sample) >> 3
        };
    }

    pub fn rcpi(&self) -> u8 {
        (220 - 2 * (self.0 >> 10) as i16).clamp(0, 220) as u8
    }
}

/// Linux mac_sta_add: clear peer WCID accounting, then publish one enabled
/// STATE_NONE station record. No legacy empty-WTBL pre-reset, preauth BSS/RLM
/// update or extra peer allocation is inserted into the pinned sequence.
pub(super) struct PeerJoin {
    pub context: wlan_softmac_class_support::OperationContext,
    pub bss: ObservedBss,
    clear: MacPreparation,
    commands: FirmwareCommands,
    pub reply: Option<futures_channel::oneshot::Sender<Result<(), zx::Status>>>,
}

impl PeerJoin {
    pub fn new(
        context: wlan_softmac_class_support::OperationContext,
        bss: ObservedBss,
        reply: futures_channel::oneshot::Sender<Result<(), zx::Status>>,
    ) -> Result<Self, zx::Status> {
        let band = if bss.channel.band == PhysicalBand::Ghz2 {
            0
        } else {
            1
        };
        // mt7921_add_interface initializes RSSI EWMA to zero. Only auth/assoc
        // responses update it (mt792x_mac_assoc_rssi), not scan beacons.
        let rcpi = 220;
        let command = mt7921_core::encode_preauth_peer_wcid_command(
            1,
            0,
            1,
            bss.bssid,
            band,
            rcpi,
            bss.basic_rates,
            bss.legacy_rates,
        )
        .map_err(|_| zx::Status::INVALID_ARGS)?;
        Ok(Self {
            context,
            bss,
            clear: MacPreparation::for_wcid(1),
            commands: FirmwareCommands::new([(command, RadioResponse::Unified(3))].into()),
            reply: Some(reply),
        })
    }

    pub fn complete(&self) -> bool {
        self.clear.complete() && self.commands.ready()
    }

    pub fn drive<B: Backend>(
        &mut self,
        resources: &mut crate::OwnedHardwareResources<B>,
        mechanics: &mut mt7921_core::LoaderMechanics,
        receive: &mut crate::receive::RxRouting,
        start: Instant,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        self.context.check(now)?;
        if !self.clear.complete() {
            return self.clear.drive(&resources.bar0, now);
        }
        self.commands.drive(
            resources,
            mechanics,
            receive,
            start,
            now,
            Some(&self.context),
        )
    }
}

/// Linux association activation: BSS/RLM, peer accounting reset, then
/// associated STA and BSS callbacks. Completing this does not open the port.
pub(super) struct PeerAssociation {
    pub context: wlan_softmac_class_support::OperationContext,
    pub qos: bool,
    bss: FirmwareCommands,
    clear: MacPreparation,
    commands: FirmwareCommands,
    pub reply: Option<futures_channel::oneshot::Sender<Result<(), zx::Status>>>,
}

impl PeerAssociation {
    pub fn new(
        context: wlan_softmac_class_support::OperationContext,
        bss: &ObservedBss,
        rcpi: u8,
        configuration: wlan_softmac_class_support::WlanAssociationConfig,
        reply: futures_channel::oneshot::Sender<Result<(), zx::Status>>,
    ) -> Result<Self, zx::Status> {
        use fidl_fuchsia_wlan_ieee80211::{ChannelBandwidth, WlanBand};
        use mt7921_core::*;
        let band = if bss.channel.band == PhysicalBand::Ghz2 {
            0
        } else {
            1
        };
        let primary = configuration.primary.ok_or(zx::Status::INVALID_ARGS)?;
        let expected_band = if band == 0 {
            WlanBand::TwoGhz
        } else {
            WlanBand::FiveGhz
        };
        if configuration.bssid != Some(bss.bssid)
            || primary.band != expected_band
            || u16::from(primary.number) != bss.channel.number
        {
            return Err(zx::Status::INVALID_ARGS);
        }
        if configuration.bandwidth != Some(ChannelBandwidth::Cbw20)
            || configuration.ht_cap.is_some()
            || configuration.vht_cap.is_some()
        {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        let secondary = configuration
            .vht_secondary_80_channel
            .ok_or(zx::Status::INVALID_ARGS)?;
        if secondary.band != primary.band || secondary.number != 0 {
            return Err(zx::Status::INVALID_ARGS);
        }
        let qos = configuration.qos.ok_or(zx::Status::INVALID_ARGS)?;
        if qos != configuration.wmm_params.is_some() {
            return Err(zx::Status::INVALID_ARGS);
        }
        let rates = configuration.rates.ok_or(zx::Status::INVALID_ARGS)?;
        if rates
            .iter()
            .any(|rate| !OFDM_RATES.contains(&(rate & 0x7f)))
        {
            return Err(zx::Status::INVALID_ARGS);
        }
        let (basic_rates, legacy_rates) = linux_legacy_rate_context_reference(band, &rates)
            .map_err(|_| zx::Status::INVALID_ARGS)?;
        let aid = configuration.aid.ok_or(zx::Status::INVALID_ARGS)?;
        let dtim = bss.dtim_period.ok_or(zx::Status::BAD_STATE)?;
        let encode = |result: Result<Vec<u8>, String>| result.map_err(|_| zx::Status::INVALID_ARGS);
        let bss_commands = [
            (
                encode(encode_client_bss_command(
                    1,
                    0,
                    bss.bssid,
                    bss.channel.number,
                    bss.beacon_period,
                    dtim,
                    qos,
                    true,
                ))?,
                RadioResponse::Unified(2),
            ),
            (
                encode(encode_client_post_assoc_rlm_command(
                    1,
                    0,
                    ClientPhysicalChannel {
                        band,
                        primary: bss.channel.number,
                        center: bss.channel.number,
                        center2: 0,
                        bandwidth: 0,
                    },
                ))?,
                RadioResponse::Unified(2),
            ),
        ];
        let mut commands = std::collections::VecDeque::from([(
            encode(encode_legacy_wme_add_wcid_command(
                1,
                0,
                1,
                aid,
                bss.bssid,
                rcpi,
                basic_rates,
                legacy_rates,
                None,
                None,
                0,
                band,
                qos,
            ))?,
            RadioResponse::Unified(3),
        )]);
        if let Some(wmm) = configuration.wmm_params {
            let mut ac = [ClientEdcaAc {
                cw_min: 0,
                cw_max: 0,
                txop: 0,
                aifs: 0,
                acm: false,
            }; 4];
            for (output, input) in ac.iter_mut().zip([
                wmm.ac_vo_params,
                wmm.ac_vi_params,
                wmm.ac_be_params,
                wmm.ac_bk_params,
            ]) {
                if input.ecw_min > 15 || input.ecw_max > 15 || input.ecw_min > input.ecw_max {
                    return Err(zx::Status::INVALID_ARGS);
                }
                *output = ClientEdcaAc {
                    cw_min: (1u16 << input.ecw_min) - 1,
                    cw_max: (1u16 << input.ecw_max) - 1,
                    txop: input.txop_limit,
                    aifs: u16::from(input.aifsn),
                    acm: input.acm,
                };
            }
            commands.push_back((
                encode(encode_client_edca_command(
                    1,
                    0,
                    ClientEdcaParameters { ac },
                ))?,
                RadioResponse::None,
            ));
        }
        commands.extend([
            (
                // Association starts awake. Fuchsia PHY power policy may
                // request acknowledged dynamic saving after SME connects.
                encode(encode_client_post_assoc_power_state_command(1, 0, 0))?,
                RadioResponse::Unified(2),
            ),
            (
                encode(encode_client_post_assoc_interface_wcid_command(
                    1, 0, bss.bssid,
                ))?,
                RadioResponse::Unified(3),
            ),
            (
                encode(encode_client_post_assoc_beacon_timing_command(
                    1,
                    0,
                    bss.beacon_period,
                    dtim,
                ))?,
                RadioResponse::Unified(2),
            ),
            (
                encode(encode_client_post_assoc_rx_filter_command(1))?,
                RadioResponse::None,
            ),
        ]);
        Ok(Self {
            context,
            qos,
            bss: FirmwareCommands::new(bss_commands.into()),
            clear: MacPreparation::for_wcid(1),
            commands: FirmwareCommands::new(commands),
            reply: Some(reply),
        })
    }

    pub fn complete(&self) -> bool {
        self.bss.ready() && self.clear.complete() && self.commands.ready()
    }

    /// Called only after the management owner relinquishes its MCU/ROC work.
    pub fn drive<B: Backend>(
        &mut self,
        resources: &mut crate::OwnedHardwareResources<B>,
        mechanics: &mut mt7921_core::LoaderMechanics,
        receive: &mut crate::receive::RxRouting,
        management: &crate::transmit::ClientTx,
        start: Instant,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        self.context.check(now)?;
        if !management.idle() {
            return Ok(false);
        }
        if !self.bss.ready() {
            return self.bss.drive(
                resources,
                mechanics,
                receive,
                start,
                now,
                Some(&self.context),
            );
        }
        if !self.clear.complete() {
            return self.clear.drive(&resources.bar0, now);
        }
        self.commands.drive(
            resources,
            mechanics,
            receive,
            start,
            now,
            Some(&self.context),
        )
    }
}

/// Host replay state and secret material are scoped to this association.
/// GTK bytes are also needed by the firmware's combined GTK/IGTK update.
pub(super) struct ClientKey {
    pub index: u8,
    pub bytes: zeroize::Zeroizing<Vec<u8>>,
    pub rx_pn: [u64; 16],
    pub management_rx_pn: u64,
}

pub(super) struct PowerSaveChange {
    pub context: wlan_softmac_class_support::OperationContext,
    pub enabled: bool,
    commands: FirmwareCommands,
    pub reply: Option<futures_channel::oneshot::Sender<Result<(), zx::Status>>>,
}

impl PowerSaveChange {
    pub fn new(
        context: wlan_softmac_class_support::OperationContext,
        enabled: bool,
        reply: futures_channel::oneshot::Sender<Result<(), zx::Status>>,
    ) -> Result<Self, zx::Status> {
        let command = mt7921_core::encode_client_post_assoc_power_state_command(
            1,
            0,
            if enabled { 2 } else { 0 },
        )
        .map_err(|_| zx::Status::INVALID_ARGS)?;
        Ok(Self {
            context,
            enabled,
            commands: FirmwareCommands::new([(command, RadioResponse::Unified(2))].into()),
            reply: Some(reply),
        })
    }

    pub fn complete(&self) -> bool {
        self.commands.ready()
    }

    pub fn drive<B: Backend>(
        &mut self,
        resources: &mut crate::OwnedHardwareResources<B>,
        mechanics: &mut mt7921_core::LoaderMechanics,
        receive: &mut crate::receive::RxRouting,
        start: Instant,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        self.context.check(now)?;
        self.commands.drive(
            resources,
            mechanics,
            receive,
            start,
            now,
            Some(&self.context),
        )
    }

    #[cfg(test)]
    fn command(&self) -> &[u8] {
        self.commands.queued_command(0).unwrap()
    }
}

pub(super) struct KeyInstallation {
    pub context: wlan_softmac_class_support::OperationContext,
    pub group: bool,
    pub management: bool,
    pub key: Option<ClientKey>,
    commands: FirmwareCommands,
    pub reply: Option<futures_channel::oneshot::Sender<Result<(), zx::Status>>>,
}

impl KeyInstallation {
    pub fn new(
        context: wlan_softmac_class_support::OperationContext,
        bssid: [u8; 6],
        configuration: wlan_softmac_class_support::WlanKeyConfiguration,
        bytes: zeroize::Zeroizing<Vec<u8>>,
        gtk: Option<&ClientKey>,
        reply: futures_channel::oneshot::Sender<Result<(), zx::Status>>,
    ) -> Result<Self, zx::Status> {
        use fidl_fuchsia_wlan_ieee80211::KeyType;
        use fidl_fuchsia_wlan_softmac::WlanProtection;
        context.check(Instant::now())?;
        let management = configuration.key_type == Some(KeyType::Igtk);
        if configuration.cipher_oui != Some([0, 0x0f, 0xac])
            || configuration.cipher_type != Some(if management { 6 } else { 4 })
            || configuration.protection != Some(WlanProtection::RxTx)
        {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        let index = configuration.key_idx.ok_or(zx::Status::INVALID_ARGS)?;
        let group = match configuration.key_type {
            Some(KeyType::Pairwise) if index == 0 && configuration.peer_addr == Some(bssid) => {
                false
            }
            Some(KeyType::Group) if index <= 3 && configuration.peer_addr == Some([0xff; 6]) => {
                true
            }
            Some(KeyType::Igtk)
                if matches!(index, 4 | 5) && configuration.peer_addr == Some([0xff; 6]) =>
            {
                true
            }
            _ => return Err(zx::Status::INVALID_ARGS),
        };
        let rsc = configuration.rsc.ok_or(zx::Status::INVALID_ARGS)?;
        // SME's GTK RSC preserves the EAPOL octets in a big-endian u64;
        // CCMP's packet number has little-endian wire order.
        let pn = if management {
            let wire = rsc.to_be_bytes();
            if wire[..2] != [0; 2] {
                return Err(zx::Status::INVALID_ARGS);
            }
            let mut ipn = [0; 8];
            ipn[..6].copy_from_slice(&wire[2..]);
            u64::from_le_bytes(ipn)
        } else if group {
            u64::from_le_bytes(rsc.to_be_bytes())
        } else {
            rsc
        };
        if bytes.len() != 16 || pn > 0x0000_ffff_ffff_ffff {
            return Err(zx::Status::INVALID_ARGS);
        }
        // Linux selects the peer WCID for PTK and the VIF WCID for GTK.
        let command = mt7921_core::encode_key_v2_command(
            1,
            0,
            if group { 19 } else { 1 },
            if group { 0x0e } else { 0 },
            index,
            &bytes,
            if management {
                let gtk = gtk.ok_or(zx::Status::BAD_STATE)?;
                Some((gtk.index, gtk.bytes.as_slice()))
            } else {
                None
            },
        )
        .map_err(|_| zx::Status::INVALID_ARGS)?;
        Ok(Self {
            context,
            group,
            management,
            key: Some(ClientKey {
                index,
                bytes,
                rx_pn: [pn; 16],
                management_rx_pn: pn,
            }),
            commands: FirmwareCommands::new(
                [(command.as_bytes().to_vec(), RadioResponse::Unified(3))].into(),
            ),
            reply: Some(reply),
        })
    }

    pub fn complete(&self) -> bool {
        self.commands.ready()
    }

    pub fn drive<B: Backend>(
        &mut self,
        resources: &mut crate::OwnedHardwareResources<B>,
        mechanics: &mut mt7921_core::LoaderMechanics,
        receive: &mut crate::receive::RxRouting,
        tx: &crate::transmit::ClientTx,
        start: Instant,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        self.context.check(now)?;
        // Finish the unprotected handshake TX before switching its key state.
        if !tx.idle() {
            return Ok(false);
        }
        self.commands.drive(
            resources,
            mechanics,
            receive,
            start,
            now,
            Some(&self.context),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OwnedHardwareResources;
    use drv_hardware_backends::{DeterministicBackend, Operation};
    use mt7921_core::{DMA_DESCRIPTOR_LEN, DmaDescriptor, LoaderMechanics};

    fn advertisement() -> Connac2RxFrame {
        let mut bytes = vec![0; 36];
        bytes[0] = 0x80;
        bytes[4..10].fill(0xff);
        bytes[10..16].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        bytes[16..22].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        bytes[32..34].copy_from_slice(&100u16.to_le_bytes());
        bytes[34] = 1;
        bytes.extend_from_slice(&[1, 8, 0x8c, 18, 0x98, 36, 0xb0, 72, 96, 108]);
        bytes.extend_from_slice(&[5, 4, 0, 2, 0, 0]);
        Connac2RxFrame {
            bytes,
            band: PhysicalBand::Ghz5,
            channel: 149,
            rssi_dbm: -60,
            pn: None,
        }
    }

    fn association_configuration() -> wlan_softmac_class_support::WlanAssociationConfig {
        use fidl_fuchsia_wlan_ieee80211::{ChannelBandwidth, ChannelNumber, WlanBand};
        wlan_softmac_class_support::WlanAssociationConfig {
            bssid: Some([2, 3, 4, 5, 6, 7]),
            aid: Some(42),
            qos: Some(false),
            primary: Some(ChannelNumber {
                band: WlanBand::FiveGhz,
                number: 149,
            }),
            bandwidth: Some(ChannelBandwidth::Cbw20),
            vht_secondary_80_channel: Some(ChannelNumber {
                band: WlanBand::FiveGhz,
                number: 0,
            }),
            rates: Some(vec![0x8c, 18, 0x98, 36, 0xb0, 72, 96, 108]),
            ..Default::default()
        }
    }

    #[test]
    fn association_rssi_uses_addressed_authentication_history_not_beacons() {
        let local = [2, 7, 6, 5, 4, 3];
        let peer = [2, 3, 4, 5, 6, 7];
        let mut rssi = AssociationRssi::default();
        let mut frame = advertisement();
        rssi.observe(&frame, local, peer);
        assert_eq!(rssi.rcpi(), 220);
        frame.bytes[0] = 0xb0;
        rssi.observe(&frame, local, peer); // broadcast destination
        assert_eq!(rssi.rcpi(), 220);
        frame.bytes[4..10].copy_from_slice(&local);
        rssi.observe(&frame, local, peer); // -60 dBm
        assert_eq!(rssi.rcpi(), 100);
        frame.bytes[0] = 0x10;
        frame.rssi_dbm = -68;
        rssi.observe(&frame, local, peer); // (7*60 + 68)/8 = 61
        assert_eq!(rssi.rcpi(), 98);
        frame.rssi_dbm = 1;
        rssi.observe(&frame, local, peer);
        assert_eq!(rssi.rcpi(), 98);
        frame.rssi_dbm = -100;
        frame.bytes[10] ^= 2;
        rssi.observe(&frame, local, peer);
        assert_eq!(rssi.rcpi(), 98);
    }

    #[test]
    fn association_requires_selected_bss_timing_and_supported_negotiation() {
        let now = Instant::now();
        for invalid in 0..5 {
            let mut bss = ObservedBss::from_rx(&advertisement(), now).unwrap();
            assert_eq!(bss.dtim_period, Some(2));
            let mut configuration = association_configuration();
            match invalid {
                0 => bss.dtim_period = None,
                1 => configuration.bssid = Some([2; 6]),
                2 => configuration.aid = Some(0),
                3 => configuration.qos = Some(true), // no WMM parameters
                _ => configuration.rates = Some(vec![0x82]), // not our advertised OFDM rates
            }
            let (reply, _) = futures_channel::oneshot::channel();
            let (context, _) = wlan_softmac_class_support::conformance::operation_context(
                now + Duration::from_secs(1),
            );
            assert!(PeerAssociation::new(context, &bss, 100, configuration, reply).is_err());
        }
        let mut frame = advertisement();
        let period = frame.bytes.len() - 3;
        frame.bytes[period] = 0;
        assert!(ObservedBss::from_rx(&frame, now).is_none());
    }

    #[test]
    fn association_orders_firmware_phases_and_requires_ack_plus_dma_consumption() {
        for (qos, reject_sta) in [(false, false), (true, false), (true, true)] {
            let (device, log, model) =
                DeterministicBackend::recording_mt7921_device_with_model(Default::default());
            let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
            resources.interrupt = Some(resources.device.open_interrupt(0).unwrap());
            let now = Instant::now();
            let (context, _) = wlan_softmac_class_support::conformance::operation_context(
                now + Duration::from_secs(10),
            );
            let bss = ObservedBss::from_rx(&advertisement(), now).unwrap();
            let mut configuration = association_configuration();
            if qos {
                configuration.qos = Some(true);
                let ac = fidl_fuchsia_wlan_driver::WlanWmmAccessCategoryParameters {
                    ecw_min: 3,
                    ecw_max: 4,
                    aifsn: 2,
                    txop_limit: 0,
                    acm: false,
                };
                configuration.wmm_params = Some(fidl_fuchsia_wlan_driver::WlanWmmParameters {
                    apsd: false,
                    ac_vo_params: ac,
                    ac_vi_params: ac,
                    ac_be_params: ac,
                    ac_bk_params: ac,
                });
            }
            let (reply, _receiver) = futures_channel::oneshot::channel();
            let mut association =
                PeerAssociation::new(context.clone(), &bss, 100, configuration, reply).unwrap();
            // Association explicitly starts awake; policy changes are a
            // separate acknowledged firmware operation after connection.
            let power_index = usize::from(qos) + 1;
            let power = association.commands.queued_command(power_index).unwrap();
            assert_eq!(&power[52..54], &[21, 0]);
            assert_eq!(power[56], 0);
            let mut mechanics = LoaderMechanics::default();
            let mut receive = crate::receive::RxRouting::default();
            let mut busy = crate::transmit::ClientTx::default();
            let mut auth = vec![0; 30];
            auth[0] = 0xb0;
            auth[4..10].copy_from_slice(&bss.bssid);
            busy.enqueue(context, &auth, 12, bss.channel).unwrap();
            assert!(
                !association
                    .drive(
                        &mut resources,
                        &mut mechanics,
                        &mut receive,
                        &busy,
                        now,
                        now
                    )
                    .unwrap()
            );
            assert!(!log.borrow().iter().any(|op| matches!(
                op,
                Operation::WriteU32 {
                    offset: 0xd4418,
                    ..
                }
            )));
            let idle = crate::transmit::ClientTx::default();
            let expected: &[Option<u8>] = if qos {
                &[
                    Some(2),
                    Some(2),
                    Some(3),
                    None,
                    Some(2),
                    Some(3),
                    Some(2),
                    None,
                ]
            } else {
                &[Some(2), Some(2), Some(3), Some(2), Some(3), Some(2), None]
            };
            let mut published = 0;
            let mut rx_slot = 0usize;
            let mut rejected = false;
            for _ in 0..100 {
                let result = association.drive(
                    &mut resources,
                    &mut mechanics,
                    &mut receive,
                    &idle,
                    now,
                    now,
                );
                if reject_sta && published == 3 {
                    assert_eq!(result, Err(zx::Status::IO_DATA_INTEGRITY));
                    rejected = true;
                    break;
                }
                result.unwrap();
                if association.complete() {
                    break;
                }
                let Some(slot) = mechanics.active_command_slot() else {
                    continue;
                };
                if usize::from(slot) != published {
                    continue;
                }
                let mut descriptor = [0; DMA_DESCRIPTOR_LEN];
                resources
                    .dma
                    .mcu_tx_ring
                    .read(usize::from(slot) * DMA_DESCRIPTOR_LEN, &mut descriptor)
                    .unwrap();
                if let Some(cid) = expected[published] {
                    let mut response = vec![0; 44];
                    response[24..26].copy_from_slice(&20u16.to_le_bytes());
                    response[28] = 1;
                    response[29] = mechanics.sequence();
                    response[36] = if reject_sta && published == 2 { 2 } else { cid };
                    let address = resources
                        .dma
                        .mcu_rx_buffers
                        .device_address(rx_slot * mt7921_core::MT7921_MCU_RX_BUFFER_BYTES)
                        .unwrap()
                        .bits();
                    let rx = DmaDescriptor {
                        buf0: address as u32,
                        ctrl: (1 << 31) | (1 << 30) | (44 << 16),
                        buf1: 0,
                        info: 0,
                    };
                    model.write_dma(address, response);
                    model.write_dma(
                        resources
                            .dma
                            .mcu_rx_ring
                            .device_address(rx_slot * DMA_DESCRIPTOR_LEN)
                            .unwrap()
                            .bits(),
                        rx.to_le_bytes().to_vec(),
                    );
                    rx_slot += 1;
                    association
                        .drive(
                            &mut resources,
                            &mut mechanics,
                            &mut receive,
                            &idle,
                            now,
                            now,
                        )
                        .unwrap();
                    assert_eq!(mechanics.active_command_slot(), Some(slot)); // ACK alone cannot reclaim
                    assert!(!association.complete());
                }
                let control = u32::from_le_bytes(descriptor[4..8].try_into().unwrap()) | (1 << 31);
                descriptor[4..8].copy_from_slice(&control.to_le_bytes());
                model.write_dma(
                    resources
                        .dma
                        .mcu_tx_ring
                        .device_address(usize::from(slot) * DMA_DESCRIPTOR_LEN)
                        .unwrap()
                        .bits(),
                    descriptor.to_vec(),
                );
                resources
                    .bar0
                    .write_u32(0xd441c, u32::from(slot) + 1)
                    .unwrap();
                published += 1;
            }
            if reject_sta {
                assert!(rejected);
                assert!(!association.complete());
            } else {
                assert!(association.complete());
                assert_eq!(published, expected.len());
                assert_eq!(association.qos, qos);
            }
        }
    }

    #[test]
    fn power_policy_maps_performance_and_balanced_to_acknowledged_firmware_states() {
        let now = Instant::now();
        for (enabled, expected) in [(false, 0), (true, 2)] {
            let (context, _) = wlan_softmac_class_support::conformance::operation_context(
                now + Duration::from_secs(1),
            );
            let (reply, _) = futures_channel::oneshot::channel();
            let change = PowerSaveChange::new(context, enabled, reply).unwrap();
            assert_eq!(&change.command()[52..54], &[21, 0]);
            assert_eq!(change.command()[56], expected);
            assert!(!change.complete());
        }
    }

    #[test]
    fn observations_bind_rate_intersection_to_bss_channel_and_age() {
        let now = Instant::now();
        let mut frame = advertisement();
        let bss = ObservedBss::from_rx(&frame, now).unwrap();
        assert_eq!(bss.basic_rates, 0x15);
        assert_eq!(bss.legacy_rates, 0x3fc0);
        assert_eq!(bss.channel.frequency_mhz, 5745);
        assert!(bss.fresh(now + Duration::from_secs(29)));
        assert!(!bss.fresh(now + Duration::from_secs(30)));
        frame.bytes.push(50); // incomplete IE, not partial rate authority
        assert!(ObservedBss::from_rx(&frame, now).is_none());
        frame = advertisement();
        frame.bytes[10] ^= 2; // transmitter differs from BSSID
        assert!(ObservedBss::from_rx(&frame, now).is_none());
        frame = advertisement();
        frame.bytes[38] = 0x82; // unsupported mandatory CCK rate
        assert!(ObservedBss::from_rx(&frame, now).is_none());
        frame = advertisement();
        frame.bytes[34] = 2; // IBSS is not a client peer
        assert!(ObservedBss::from_rx(&frame, now).is_none());
    }

    #[test]
    fn peer_join_clears_wcid_before_one_full_record_and_waits_for_ack_and_reclaim() {
        for valid_cid in [true, false] {
            let (device, log, model) =
                DeterministicBackend::recording_mt7921_device_with_model(Default::default());
            let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
            resources.interrupt = Some(resources.device.open_interrupt(0).unwrap());
            let now = Instant::now();
            let (context, _) = wlan_softmac_class_support::conformance::operation_context(
                now + Duration::from_secs(1),
            );
            let (reply, _receiver) = futures_channel::oneshot::channel();
            let mut join = PeerJoin::new(
                context,
                ObservedBss::from_rx(&advertisement(), now).unwrap(),
                reply,
            )
            .unwrap();
            let mut mechanics = LoaderMechanics::default();
            let mut receive = crate::receive::RxRouting::default();
            // WTBL publication and busy-clear observation are separate turns.
            join.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap();
            join.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap();
            assert!(!log.borrow().iter().any(|op| matches!(
                op,
                Operation::WriteU32 {
                    offset: 0xd4418,
                    ..
                }
            )));
            join.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap();
            let mut tx = [0; DMA_DESCRIPTOR_LEN];
            resources.dma.mcu_tx_ring.read(0, &mut tx).unwrap();
            let control = u32::from_le_bytes(tx[4..8].try_into().unwrap()) | (1 << 31);
            tx[4..8].copy_from_slice(&control.to_le_bytes());
            let mut response = vec![0; 44];
            response[24..26].copy_from_slice(&20u16.to_le_bytes());
            response[28] = 1;
            response[29] = mechanics.sequence();
            response[36] = if valid_cid { 3 } else { 2 };
            let rx = DmaDescriptor {
                buf0: resources
                    .dma
                    .mcu_rx_buffers
                    .device_address(0)
                    .unwrap()
                    .bits() as u32,
                ctrl: (1 << 31) | (1 << 30) | (44 << 16),
                buf1: 0,
                info: 0,
            };
            model.write_dma(
                resources
                    .dma
                    .mcu_rx_buffers
                    .device_address(0)
                    .unwrap()
                    .bits(),
                response,
            );
            model.write_dma(
                resources.dma.mcu_rx_ring.device_address(0).unwrap().bits(),
                rx.to_le_bytes().to_vec(),
            );
            join.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap();
            assert!(!join.complete()); // response cannot retire DMA
            model.write_dma(
                resources.dma.mcu_tx_ring.device_address(0).unwrap().bits(),
                tx.to_vec(),
            );
            resources.bar0.write_u32(0xd441c, 1).unwrap();
            let result = join.drive(&mut resources, &mut mechanics, &mut receive, now, now);
            if valid_cid {
                assert!(result.unwrap());
                assert!(join.complete());
            } else {
                assert_eq!(result, Err(zx::Status::IO_DATA_INTEGRITY));
                assert!(!join.complete());
            }
            assert_eq!(
                log.borrow()
                    .iter()
                    .filter(|op| matches!(
                        op,
                        Operation::WriteU32 {
                            offset: 0xd4418,
                            value: 1,
                            ..
                        }
                    ))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn revoked_join_does_not_publish_after_partial_wtbl_effect() {
        let (device, log) = DeterministicBackend::recording_mt7921_activation_device();
        let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        let now = Instant::now();
        let (context, revoke) = wlan_softmac_class_support::conformance::operation_context(
            now + Duration::from_secs(1),
        );
        let (reply, _) = futures_channel::oneshot::channel();
        let mut join = PeerJoin::new(
            context,
            ObservedBss::from_rx(&advertisement(), now).unwrap(),
            reply,
        )
        .unwrap();
        let mut mechanics = LoaderMechanics::default();
        let mut receive = crate::receive::RxRouting::default();
        join.drive(&mut resources, &mut mechanics, &mut receive, now, now)
            .unwrap();
        revoke();
        let before = log.borrow().len();
        assert_eq!(
            join.drive(&mut resources, &mut mechanics, &mut receive, now, now),
            Err(zx::Status::CANCELED)
        );
        assert_eq!(log.borrow().len(), before);
        assert!(!join.complete());
    }
}
