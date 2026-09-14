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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OwnedHardwareResources;
    use drv_hardware_backends::{DeterministicBackend, Operation};

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
