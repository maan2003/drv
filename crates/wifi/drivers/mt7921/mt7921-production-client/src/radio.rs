//! Device-lifetime MAC preparation. Operation-specific radio commands are
//! separate: initialization does not authorize channel changes or scanning.

use drv_hardware::{Backend, MmioRegion};
use mt7921_core::{
    PassiveMacMmioOperation, passive_mac_bar_offset, passive_mac_mmio_plan,
    passive_mac_source_rmw_value, validate_passive_mac_bar_read,
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub(super) struct MacPreparation {
    remaining: VecDeque<PassiveMacMmioOperation>,
    waiting: Option<(u32, u32, Instant)>,
    failed: bool,
}

impl MacPreparation {
    pub fn new() -> Self {
        Self {
            remaining: passive_mac_mmio_plan().into(),
            waiting: None,
            failed: false,
        }
    }

    /// One source RMW, WTBL publication, or busy observation per owner turn.
    /// A failed/partially executed plan cannot be restarted without containment.
    pub fn drive<B: Backend>(
        &mut self,
        bar: &MmioRegion<B>,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        if self.failed {
            return Err(zx::Status::BAD_STATE);
        }
        let result = self.step(bar, now);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn step<B: Backend>(&mut self, bar: &MmioRegion<B>, now: Instant) -> Result<bool, zx::Status> {
        if let Some((address, busy_mask, deadline)) = self.waiting {
            if now >= deadline {
                return Err(zx::Status::TIMED_OUT);
            }
            let offset = passive_mac_bar_offset(address).map_err(|_| zx::Status::INTERNAL)?;
            let value = bar.read_u32(offset).map_err(|_| zx::Status::IO)?;
            validate_passive_mac_bar_read(address, value)
                .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
            if value & busy_mask != 0 {
                return Ok(false);
            }
            self.waiting = None;
            return Ok(true);
        }
        let Some(operation) = self.remaining.pop_front() else {
            return Ok(false);
        };
        match operation {
            PassiveMacMmioOperation::Rmw {
                address,
                mask,
                value,
            } => {
                let offset = passive_mac_bar_offset(address).map_err(|_| zx::Status::INTERNAL)?;
                let initial = bar.read_u32(offset).map_err(|_| zx::Status::IO)?;
                validate_passive_mac_bar_read(address, initial)
                    .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
                // Source mt76_mmio_rmw has no immediate equality readback.
                bar.write_u32(
                    offset,
                    passive_mac_source_rmw_value(initial, mask, value & mask),
                )
                .map_err(|_| zx::Status::IO)?;
            }
            PassiveMacMmioOperation::WtblClear {
                address,
                index_mask,
                value,
                busy_mask,
                timeout_us,
                ..
            } => {
                let offset = passive_mac_bar_offset(address).map_err(|_| zx::Status::INTERNAL)?;
                self.waiting = Some((
                    address,
                    busy_mask,
                    now + Duration::from_micros(u64::from(timeout_us)),
                ));
                let initial = bar.read_u32(offset).map_err(|_| zx::Status::IO)?;
                validate_passive_mac_bar_read(address, initial)
                    .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
                // Linux mt7921_mac_wtbl_update uses mt76_rmw, not writel.
                bar.write_u32(
                    offset,
                    passive_mac_source_rmw_value(initial, index_mask, value),
                )
                .map_err(|_| zx::Status::IO)?;
            }
        }
        Ok(true)
    }

    pub fn complete(&self) -> bool {
        !self.failed && self.remaining.is_empty() && self.waiting.is_none()
    }
}

/// Linux mt7921/init.c::mt7921_mac_init orders MAC MMIO before the single
/// mt76_connac_mcu_set_rts_thresh command (pin in nix/mt76-reference-source.nix).
/// Radio/scan capabilities remain gated after this device initialization.
pub(super) enum MacInitialization {
    Mac(MacPreparation),
    Protect { deadline: Instant },
    Ready,
    Failed,
}

impl MacInitialization {
    pub fn new() -> Self {
        Self::Mac(MacPreparation::new())
    }

    pub fn drive<B: Backend>(
        &mut self,
        resources: &mut crate::OwnedHardwareResources<B>,
        mechanics: &mut mt7921_core::LoaderMechanics,
        receive: &mut crate::receive::RxRouting,
        start: Instant,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        let result = self.step(resources, mechanics, receive, start, now);
        if result.is_err() {
            *self = Self::Failed;
        }
        result
    }

    fn step<B: Backend>(
        &mut self,
        resources: &mut crate::OwnedHardwareResources<B>,
        mechanics: &mut mt7921_core::LoaderMechanics,
        receive: &mut crate::receive::RxRouting,
        start: Instant,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        use mt7921_core::{
            DownloadCommand, FirmwareCommandCompletion, LoaderCommandCompletion,
            LoaderCommandProgress, LoaderCompletion, McuResponse, classify_mcu_completion,
            encode_download_command,
        };
        match self {
            Self::Mac(prep) => {
                if !prep.complete() {
                    return prep.drive(&resources.bar0, now);
                }
                // mt7921/pci_mcu.c gives MCU commands a 3-second timeout.
                let deadline = now + Duration::from_secs(3);
                let command = encode_download_command(DownloadCommand::ProtectControl, 1)
                    .map_err(|_| zx::Status::INTERNAL)?;
                let mut views = resources
                    .active_mcu_views(receive, start)
                    .map_err(|_| zx::Status::IO)?;
                mechanics
                    .begin_command(
                        &mut views,
                        &mut (),
                        &command,
                        LoaderCommandCompletion::Response,
                    )
                    .map_err(|_| zx::Status::IO)?;
                *self = Self::Protect { deadline };
                Ok(true)
            }
            Self::Protect { deadline } => {
                if now >= *deadline {
                    return Err(zx::Status::TIMED_OUT);
                }
                let mut views = resources
                    .active_mcu_views(receive, start)
                    .map_err(|_| zx::Status::IO)?;
                match mechanics
                    .poll_command(&mut views, &mut ())
                    .map_err(|_| zx::Status::IO)?
                {
                    LoaderCommandProgress::Pending { progressed } => Ok(progressed),
                    LoaderCommandProgress::Complete(LoaderCompletion::Response(response)) => {
                        let result = classify_mcu_completion(
                            DownloadCommand::ProtectControl,
                            McuResponse {
                                event_id: response.response.event_id,
                                option: response.response.option,
                                bytes: &response.bytes,
                            },
                        )
                        .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
                        if result != FirmwareCommandCompletion::Ack {
                            return Err(zx::Status::IO_DATA_INTEGRITY);
                        }
                        *self = Self::Ready;
                        Ok(true)
                    }
                    LoaderCommandProgress::Complete(LoaderCompletion::NoResponse) => {
                        Err(zx::Status::IO_DATA_INTEGRITY)
                    }
                }
            }
            Self::Ready => Ok(false),
            Self::Failed => Err(zx::Status::BAD_STATE),
        }
    }
}

/// Device-owned, bounded firmware progression for passive radio startup.
/// Firmware tables and policy are encoded once; no descriptor authority or
/// protocol futures escape into the plan.
pub(super) struct RadioPreparation {
    commands: VecDeque<(Vec<u8>, RadioResponse)>,
    pending: Option<(RadioResponse, Instant)>,
    station: MacPreparation,
    failed: bool,
}

#[derive(Clone, Copy)]
enum RadioResponse {
    None,
    Ack,
    Clc,
    Unified(u8),
}

impl RadioPreparation {
    pub fn new(
        firmware: mt7921_core::Firmware<'_>,
        report: &mt7921_core::FirmwareLoaderReport,
        regulatory: &mt7921_core::RegulatoryRatePowerSnapshot,
    ) -> Result<Self, zx::Status> {
        use mt7921_core::*;
        let mut domain = conservative_channel_domain(
            report.nic_capability,
            regulatory.alpha2(),
            true,
            report.special_unii_mask,
        )
        .map_err(|_| zx::Status::NOT_SUPPORTED)?;
        domain.channels.retain(|channel| {
            regulatory.channels().iter().any(|rule| {
                rule.band == channel.band
                    && rule.channel == channel.number
                    && rule.present
                    && !rule.disabled
                    && rule.max_reg_power_dbm.is_some()
            })
        });
        let first = candidate_channels(report.nic_capability)
            .into_iter()
            .find(|channel| {
                domain
                    .channels
                    .iter()
                    .any(|allowed| allowed.band == channel.band && allowed.number == channel.number)
            })
            .ok_or(zx::Status::NOT_SUPPORTED)?;
        let phy = report.nic_capability.phy.ok_or(zx::Status::NOT_SUPPORTED)?;
        if phy.spatial_streams != 2 {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        let clc = world_clc_commands(
            firmware,
            report
                .eeprom_hardware
                .hardware_info()
                .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?,
            report.nic_capability.chip_capability.unwrap_or(0),
            1,
        )
        .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
        let mut commands = VecDeque::new();
        for command in clc {
            commands.push_back((
                encode_clc_set_command(&command, 1).map_err(|_| zx::Status::INTERNAL)?,
                if command.expects_response() {
                    RadioResponse::Clc
                } else {
                    RadioResponse::None
                },
            ));
        }
        // Linux mt7921_regd_update: CLC -> domain -> rate power.
        let encoded_domain =
            encode_channel_domain_command(&domain, 1).map_err(|_| zx::Status::NOT_SUPPORTED)?;
        let powers = encode_regulatory_rate_tx_power_commands(
            report.nic_capability,
            regulatory,
            regulatory.generation(),
            1,
        )
        .map_err(|_| zx::Status::NOT_SUPPORTED)?;
        commands.push_back((encoded_domain.clone(), RadioResponse::None));
        commands.extend(
            powers
                .iter()
                .cloned()
                .map(|bytes| (bytes, RadioResponse::None)),
        );
        // Linux __mt7921_start: MAC -> domain -> RX path -> rate power.
        commands.push_back((
            encode_passive_mcu_command(&PassiveMcuCommand::MacEnable, 1)
                .map_err(|_| zx::Status::INTERNAL)?,
            RadioResponse::Ack,
        ));
        commands.push_back((encoded_domain, RadioResponse::None));
        commands.push_back((
            encode_passive_mcu_command(
                &PassiveMcuCommand::SetRxPath {
                    channel: first,
                    antenna_mask: 3,
                },
                1,
            )
            .map_err(|_| zx::Status::INTERNAL)?,
            RadioResponse::Ack,
        ));
        commands.extend(powers.into_iter().map(|bytes| (bytes, RadioResponse::None)));
        for value in [1, 2] {
            commands.push_back((
                encode_passive_mcu_command(&PassiveMcuCommand::RadioLedCtrl { value }, 1)
                    .map_err(|_| zx::Status::INTERNAL)?,
                RadioResponse::None,
            ));
        }
        let mac = report
            .nic_capability
            .mac_address
            .ok_or(zx::Status::IO_DATA_INTEGRITY)?;
        // Linux add_interface: unified device, unified BSS, then WCID19 clear.
        for (command, cid) in [
            (PassiveMcuCommand::AddDevice { mac }, 1),
            (PassiveMcuCommand::AddBss, 2),
        ] {
            commands.push_back((
                encode_passive_mcu_command(&command, 1).map_err(|_| zx::Status::INTERNAL)?,
                RadioResponse::Unified(cid),
            ));
        }
        let station = MacPreparation {
            remaining: passive_mac_mmio_plan().into_iter().filter(|operation|
                matches!(operation, PassiveMacMmioOperation::WtblClear { value, .. } if value & 0x3ff == 19)
            ).collect(),
            waiting: None, failed: false,
        };
        Ok(Self {
            commands,
            pending: None,
            station,
            failed: false,
        })
    }

    pub fn ready(&self) -> bool {
        !self.failed
            && self.commands.is_empty()
            && self.pending.is_none()
            && self.station.complete()
    }

    pub fn drive<B: Backend>(
        &mut self,
        resources: &mut crate::OwnedHardwareResources<B>,
        mechanics: &mut mt7921_core::LoaderMechanics,
        receive: &mut crate::receive::RxRouting,
        start: Instant,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        let result = self.step(resources, mechanics, receive, start, now);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn step<B: Backend>(
        &mut self,
        resources: &mut crate::OwnedHardwareResources<B>,
        mechanics: &mut mt7921_core::LoaderMechanics,
        receive: &mut crate::receive::RxRouting,
        start: Instant,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        use mt7921_core::{LoaderCommandCompletion, LoaderCommandProgress, LoaderCompletion};
        if self.failed {
            return Err(zx::Status::BAD_STATE);
        }
        if let Some((expected, deadline)) = self.pending {
            if now >= deadline {
                return Err(zx::Status::TIMED_OUT);
            }
            let mut views = resources
                .active_mcu_views(receive, start)
                .map_err(|_| zx::Status::IO)?;
            let completion = match mechanics
                .poll_command(&mut views, &mut ())
                .map_err(|_| zx::Status::IO)?
            {
                LoaderCommandProgress::Pending { progressed } => return Ok(progressed),
                LoaderCommandProgress::Complete(completion) => completion,
            };
            match (expected, completion) {
                (RadioResponse::None, LoaderCompletion::NoResponse) => {}
                (RadioResponse::Ack, LoaderCompletion::Response(_)) => {}
                (RadioResponse::Unified(cid), LoaderCompletion::Response(response)) => {
                    let body = response
                        .bytes
                        .get(36..44)
                        .ok_or(zx::Status::IO_DATA_INTEGRITY)?;
                    if body[0] != cid || body[4..8] != [0; 4] {
                        return Err(zx::Status::IO_DATA_INTEGRITY);
                    }
                }
                (RadioResponse::Clc, LoaderCompletion::Response(response)) => {
                    let result = mt7921_core::classify_clc_response(mt7921_core::McuResponse {
                        event_id: response.response.event_id,
                        option: response.response.option,
                        bytes: &response.bytes,
                    })
                    .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
                    if result.special_unii_mask != 0 {
                        return Err(zx::Status::NOT_SUPPORTED);
                    }
                }
                _ => return Err(zx::Status::IO_DATA_INTEGRITY),
            }
            self.pending = None;
            return Ok(true);
        }
        if let Some((bytes, expected)) = self.commands.pop_front() {
            let mut views = resources
                .active_mcu_views(receive, start)
                .map_err(|_| zx::Status::IO)?;
            mechanics
                .begin_command(
                    &mut views,
                    &mut (),
                    &bytes,
                    if matches!(expected, RadioResponse::None) {
                        LoaderCommandCompletion::NoResponse
                    } else {
                        LoaderCommandCompletion::Response
                    },
                )
                .map_err(|_| zx::Status::IO)?;
            self.pending = Some((expected, now + Duration::from_secs(3)));
            return Ok(true);
        }
        self.station.drive(&resources.bar0, now)
    }
}

pub(super) struct PassiveScan {
    pub context: wlan_softmac_host::OperationContext,
    pub id: u64,
    pub sequence: u8,
    pub channels: Vec<mt7921_core::CandidateChannel>,
    pub reply: Option<
        futures_channel::oneshot::Sender<
            Result<wlan_softmac_host::WlanSoftmacBaseStartPassiveScanResponse, zx::Status>,
        >,
    >,
    pub published: bool,
    pub reclaimed: bool,
    pub done: Option<mt7921_core::PassiveScanDone>,
}

impl PassiveScan {
    pub fn drive<B: Backend>(
        &mut self,
        resources: &mut crate::OwnedHardwareResources<B>,
        mechanics: &mut mt7921_core::LoaderMechanics,
        receive: &mut crate::receive::RxRouting,
        start: Instant,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        use mt7921_core::*;
        // Expiry after publication is a containment event, not permission to
        // release DMA or reuse a firmware scan sequence.
        if let Err(status) = self.context.check(now) {
            if let Some(reply) = self.reply.take() {
                let _ = reply.send(Err(status));
            }
            return Err(status);
        }
        if !self.published {
            let bytes = encode_passive_mcu_command(
                &PassiveMcuCommand::StartScan {
                    scan_sequence: self.sequence,
                    channels: self.channels.clone(),
                },
                1,
            )
            .map_err(|_| zx::Status::INVALID_ARGS)?;
            let mut views = resources
                .active_mcu_views(receive, start)
                .map_err(|_| zx::Status::IO)?;
            // Original authority is checked immediately before the publication.
            self.context.check(Instant::now())?;
            mechanics
                .begin_command(
                    &mut views,
                    &mut (),
                    &bytes,
                    LoaderCommandCompletion::NoResponse,
                )
                .map_err(|_| zx::Status::IO)?;
            self.published = true;
            return Ok(true);
        }
        if !self.reclaimed {
            let mut views = resources
                .active_mcu_views(receive, start)
                .map_err(|_| zx::Status::IO)?;
            return match mechanics
                .poll_command(&mut views, &mut ())
                .map_err(|_| zx::Status::IO)?
            {
                LoaderCommandProgress::Pending { progressed } => Ok(progressed),
                LoaderCommandProgress::Complete(LoaderCompletion::NoResponse) => {
                    self.reclaimed = true;
                    if let Some(reply) = self.reply.take() {
                        let _ = reply.send(Ok(
                            wlan_softmac_host::WlanSoftmacBaseStartPassiveScanResponse {
                                scan_id: Some(self.id),
                                ..Default::default()
                            },
                        ));
                    }
                    Ok(true)
                }
                _ => Err(zx::Status::IO_DATA_INTEGRITY),
            };
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OwnedHardwareResources;
    use drv_hardware_backends::{DeterministicBackend, Operation};

    #[test]
    fn scan_reply_waits_for_tx_reclamation_and_revocation_retains_published_dma() {
        use mt7921_core::*;
        for revoke_after_publish in [false, true] {
            let (device, _, model) =
                DeterministicBackend::recording_mt7921_device_with_model(Default::default());
            let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
            resources.interrupt = Some(resources.device.open_interrupt(0).unwrap());
            let mut mechanics = LoaderMechanics::default();
            let mut receive = crate::receive::RxRouting::default();
            let now = Instant::now();
            let (context, revoke) =
                wlan_softmac_host::conformance::operation_context(now + Duration::from_secs(1));
            let (reply, mut receiver) = futures_channel::oneshot::channel();
            let mut scan = PassiveScan {
                context,
                id: 257,
                sequence: 1,
                channels: vec![CandidateChannel {
                    band: PhysicalBand::Ghz2,
                    number: 1,
                    frequency_mhz: 2412,
                }],
                reply: Some(reply),
                published: false,
                reclaimed: false,
                done: None,
            };
            assert!(
                scan.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                    .unwrap()
            );
            assert_eq!(receiver.try_recv().unwrap(), None);
            let mut tx = [0; DMA_DESCRIPTOR_LEN];
            resources.dma.mcu_tx_ring.read(0, &mut tx).unwrap();
            if revoke_after_publish {
                revoke();
                assert_eq!(
                    scan.drive(&mut resources, &mut mechanics, &mut receive, now, now),
                    Err(zx::Status::CANCELED)
                );
                assert_eq!(
                    receiver.try_recv().unwrap(),
                    Some(Err(zx::Status::CANCELED))
                );
                let mut retained = [0; DMA_DESCRIPTOR_LEN];
                resources.dma.mcu_tx_ring.read(0, &mut retained).unwrap();
                assert_eq!(retained, tx);
                assert!(!scan.reclaimed);
            } else {
                let control = u32::from_le_bytes(tx[4..8].try_into().unwrap()) | (1 << 31);
                tx[4..8].copy_from_slice(&control.to_le_bytes());
                model.write_dma(
                    resources.dma.mcu_tx_ring.device_address(0).unwrap().bits(),
                    tx.to_vec(),
                );
                resources.bar0.write_u32(0xd441c, 1).unwrap();
                assert!(
                    scan.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                        .unwrap()
                );
                assert_eq!(
                    receiver.try_recv().unwrap().unwrap().unwrap().scan_id,
                    Some(257)
                );
                assert!(scan.reclaimed);
                assert!(
                    !scan
                        .drive(&mut resources, &mut mechanics, &mut receive, now, now)
                        .unwrap()
                );
            }
        }
    }

    #[test]
    fn radio_preparation_retains_command_until_reclaim_and_poisoned_timeout_never_restarts() {
        let (device, log) = DeterministicBackend::recording_mt7921_activation_device();
        let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        resources.interrupt = Some(resources.device.open_interrupt(0).unwrap());
        let command = mt7921_core::encode_passive_mcu_command(
            &mt7921_core::PassiveMcuCommand::RadioLedCtrl { value: 1 },
            1,
        )
        .unwrap();
        let mut preparation = RadioPreparation {
            commands: [
                (command.clone(), RadioResponse::None),
                (command, RadioResponse::None),
            ]
            .into(),
            pending: None,
            station: MacPreparation {
                remaining: VecDeque::new(),
                waiting: None,
                failed: false,
            },
            failed: false,
        };
        let mut mechanics = mt7921_core::LoaderMechanics::default();
        let mut receive = crate::receive::RxRouting::default();
        let now = Instant::now();
        assert!(
            preparation
                .drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap()
        );
        assert_eq!(preparation.commands.len(), 1);
        assert!(
            !preparation
                .drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap()
        );
        assert_eq!(preparation.commands.len(), 1);
        assert!(!preparation.ready());
        let before = log.borrow().len();
        assert_eq!(
            preparation.drive(
                &mut resources,
                &mut mechanics,
                &mut receive,
                now,
                now + Duration::from_secs(3)
            ),
            Err(zx::Status::TIMED_OUT)
        );
        assert_eq!(
            preparation.drive(&mut resources, &mut mechanics, &mut receive, now, now),
            Err(zx::Status::BAD_STATE)
        );
        assert_eq!(log.borrow().len(), before);
        assert_eq!(preparation.commands.len(), 1);
    }

    #[test]
    fn expired_scan_never_publishes_a_descriptor() {
        let (device, log) = DeterministicBackend::recording_mt7921_activation_device();
        let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        let now = Instant::now();
        let (context, _) = wlan_softmac_host::conformance::operation_context(now);
        let (reply, mut receiver) = futures_channel::oneshot::channel();
        let mut scan = PassiveScan {
            context,
            id: 1,
            sequence: 1,
            channels: Vec::new(),
            reply: Some(reply),
            published: false,
            reclaimed: false,
            done: None,
        };
        let before = log.borrow().len();
        assert_eq!(
            scan.drive(
                &mut resources,
                &mut Default::default(),
                &mut Default::default(),
                now,
                now
            ),
            Err(zx::Status::TIMED_OUT)
        );
        assert_eq!(log.borrow().len(), before);
        assert_eq!(
            receiver.try_recv().unwrap(),
            Some(Err(zx::Status::TIMED_OUT))
        );
    }

    #[test]
    fn plan_advances_in_bounded_turns_without_repeating_completed_writes() {
        let (device, log) = DeterministicBackend::recording_mt7921_activation_device();
        let (resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        let mut prep = MacPreparation::new();
        let now = Instant::now();
        let writes = passive_mac_mmio_plan().len();
        for _ in 0..writes + 20 {
            let before = log.borrow().len();
            assert!(prep.drive(&resources.bar0, now).unwrap());
            assert!(log.borrow().len() - before <= 2);
        }
        assert!(prep.complete());
        let before = log.borrow().len();
        assert!(!prep.drive(&resources.bar0, now).unwrap());
        assert_eq!(log.borrow().len(), before);
        assert_eq!(
            log.borrow()
                .iter()
                .filter(|op| matches!(op, Operation::WriteU32 { .. }))
                .count(),
            writes
        );
    }

    #[test]
    fn rts_ready_requires_both_response_and_tx_reclaim_in_either_order() {
        use mt7921_core::{DMA_DESCRIPTOR_LEN, DmaDescriptor};
        for response_first in [true, false] {
            let (device, log, model) =
                DeterministicBackend::recording_mt7921_device_with_model(Default::default());
            let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
            resources.interrupt = Some(resources.device.open_interrupt(0).unwrap());
            let mut init = MacInitialization::new();
            let mut mechanics = mt7921_core::LoaderMechanics::default();
            let mut receive = crate::receive::RxRouting::default();
            let now = Instant::now();
            for _ in 0..passive_mac_mmio_plan().len() + 21 {
                init.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                    .unwrap();
            }
            let mut tx = [0; DMA_DESCRIPTOR_LEN];
            resources.dma.mcu_tx_ring.read(0, &mut tx).unwrap();
            let control = u32::from_le_bytes(tx[4..8].try_into().unwrap()) | (1 << 31);
            tx[4..8].copy_from_slice(&control.to_le_bytes());
            let mut response = vec![0; 36];
            response[24..26].copy_from_slice(&12u16.to_le_bytes());
            response[28] = 1;
            response[29] = mechanics.sequence();
            let rx = DmaDescriptor {
                buf0: resources
                    .dma
                    .mcu_rx_buffers
                    .device_address(0)
                    .unwrap()
                    .bits() as u32,
                ctrl: (1 << 31) | (1 << 30) | (36 << 16),
                buf1: 0,
                info: 0,
            };
            for deliver_response in [response_first, !response_first] {
                if deliver_response {
                    model.write_dma(
                        resources
                            .dma
                            .mcu_rx_buffers
                            .device_address(0)
                            .unwrap()
                            .bits(),
                        response.clone(),
                    );
                    model.write_dma(
                        resources.dma.mcu_rx_ring.device_address(0).unwrap().bits(),
                        rx.to_le_bytes().to_vec(),
                    );
                } else {
                    model.write_dma(
                        resources.dma.mcu_tx_ring.device_address(0).unwrap().bits(),
                        tx.to_vec(),
                    );
                    resources.bar0.write_u32(0xd441c, 1).unwrap();
                }
                init.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                    .unwrap();
                if deliver_response == response_first {
                    assert!(matches!(init, MacInitialization::Protect { .. }));
                }
            }
            assert!(matches!(init, MacInitialization::Ready));
            let before = log.borrow().len();
            assert!(
                !init
                    .drive(&mut resources, &mut mechanics, &mut receive, now, now)
                    .unwrap()
            );
            assert_eq!(log.borrow().len(), before);
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
            let mut reclaimed = [0; DMA_DESCRIPTOR_LEN];
            resources.dma.mcu_tx_ring.read(0, &mut reclaimed).unwrap();
            assert_eq!(reclaimed, DmaDescriptor::reset().to_le_bytes());
        }
    }

    #[test]
    fn rts_is_published_once_after_mac_and_timeout_retains_the_mcu_operation() {
        let (device, log) = DeterministicBackend::recording_mt7921_activation_device();
        let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        resources.interrupt = Some(resources.device.open_interrupt(0).unwrap());
        let mut init = MacInitialization::new();
        let mut mechanics = mt7921_core::LoaderMechanics::default();
        let mut receive = crate::receive::RxRouting::default();
        let now = Instant::now();
        for _ in 0..passive_mac_mmio_plan().len() + 20 {
            assert!(
                init.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                    .unwrap()
            );
        }
        assert!(matches!(init, MacInitialization::Mac(_)));
        assert!(!log.borrow().iter().any(|op| matches!(
            op,
            Operation::WriteU32 {
                offset: 0xd4418,
                ..
            }
        )));
        assert!(
            init.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap()
        );
        assert!(matches!(init, MacInitialization::Protect { .. }));
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
        let mut descriptor = [0; mt7921_core::DMA_DESCRIPTOR_LEN];
        resources.dma.mcu_tx_ring.read(0, &mut descriptor).unwrap();
        assert_eq!(
            init.drive(
                &mut resources,
                &mut mechanics,
                &mut receive,
                now,
                now + Duration::from_secs(3)
            ),
            Err(zx::Status::TIMED_OUT)
        );
        assert!(matches!(init, MacInitialization::Failed));
        let mut after = [0; mt7921_core::DMA_DESCRIPTOR_LEN];
        resources.dma.mcu_tx_ring.read(0, &mut after).unwrap();
        assert_eq!(after, descriptor);
        assert_ne!(after, mt7921_core::DmaDescriptor::reset().to_le_bytes());
        let encoded =
            mt7921_core::encode_download_command(mt7921_core::DownloadCommand::ProtectControl, 1)
                .unwrap();
        let mut views = resources.active_mcu_views(&mut receive, now).unwrap();
        assert!(matches!(
            mechanics.begin_command(
                &mut views,
                &mut (),
                &encoded,
                mt7921_core::LoaderCommandCompletion::Response
            ),
            Err(mt7921_core::LoaderMechanicsError::CommandPending { .. })
        ));
        let before = log.borrow().len();
        assert_eq!(
            init.drive(&mut resources, &mut mechanics, &mut receive, now, now),
            Err(zx::Status::BAD_STATE)
        );
        assert_eq!(log.borrow().len(), before);
    }

    #[test]
    fn wtbl_clear_preserves_fields_outside_linux_station_index_mask() {
        let (device, _) = DeterministicBackend::recording_mt7921_activation_device();
        let (resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        let mut prep = MacPreparation::new();
        let now = Instant::now();
        let offset = passive_mac_bar_offset(0x820d_4230).unwrap();
        resources.bar0.write_u32(offset, 0x0040_03ff).unwrap();
        for _ in 0..4 {
            prep.drive(&resources.bar0, now).unwrap();
        }
        assert_eq!(resources.bar0.read_u32(offset).unwrap(), 0x0040_1000);
    }

    #[test]
    fn busy_wait_keeps_original_deadline_and_fault_is_terminal() {
        let (device, log) = DeterministicBackend::recording_mt7921_activation_device();
        let (resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        let mut prep = MacPreparation::new();
        let now = Instant::now();
        for _ in 0..4 {
            prep.drive(&resources.bar0, now).unwrap();
        }
        let offset = passive_mac_bar_offset(0x820d_4230).unwrap();
        resources.bar0.write_u32(offset, 1 << 31).unwrap();
        assert!(
            !prep
                .drive(&resources.bar0, now + Duration::from_millis(4))
                .unwrap()
        );
        assert_eq!(
            prep.drive(&resources.bar0, now + Duration::from_millis(5)),
            Err(zx::Status::TIMED_OUT)
        );
        let before = log.borrow().len();
        assert_eq!(prep.drive(&resources.bar0, now), Err(zx::Status::BAD_STATE));
        assert_eq!(log.borrow().len(), before);
        assert!(!prep.complete());
    }
}
