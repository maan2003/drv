//! Source-shaped ownership of the WCN6750 datapath rings.

use alloc::vec::Vec;
use ath11k_hal::{HalError, RingId, RingMemory, RingType, Rings, Wcn6750Registers};
use ath11k_platform_backend::{Backend, Bidirectional, Device};

use crate::{DataRings, DpError};

const RING_BASE_ALIGN: usize = 8;

/// The information `ath11k_dp_srng_setup` supplies to HAL for one SRNG.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DpRingSpec {
    pub ring_type: RingType,
    pub ring_number: u8,
    pub mac_id: u8,
    pub entries: u16,
}

impl DpRingSpec {
    const fn new(ring_type: RingType, ring_number: u8, mac_id: u8, entries: u16) -> Self {
        Self {
            ring_type,
            ring_number,
            mac_id,
            entries,
        }
    }
}

/// HAL ring operations needed by the DP owner to unwind partial setup.
///
/// `Rings` owns descriptor mechanics and the `RingMemory` passed to `create`;
/// this additive operation lets DP mirror `ath11k_dp_srng_cleanup` without
/// exposing HAL's backing allocation.
pub trait DpRingOps<B: Backend>: Rings<B> {
    fn create_dp_ring(
        &mut self,
        spec: DpRingSpec,
        memory: RingMemory<B>,
    ) -> Result<RingId, HalError>;
    fn destroy(&mut self, ring: RingId) -> Result<(), HalError>;
    fn send_htt_ring_setup<C: crate::HttControl>(
        &self,
        index: usize,
        control: &mut C,
    ) -> Result<bool, DpError>;
    fn setup_reo_controller(
        &self,
        command_ring: RingId,
        status_ring: RingId,
    ) -> Result<crate::reo::ReoController, DpError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocatedDpRing {
    pub id: RingId,
    pub spec: DpRingSpec,
}

/// All SoC and pdev rings owned by one WCN6750 datapath instance.
///
/// The three vectors preserve Linux's allocation order. Teardown walks the
/// same source-defined order while retaining the first failed ring and the
/// unvisited suffix for retry.
#[derive(Default)]
pub struct Wcn6750DpRings {
    common: Vec<AllocatedDpRing>,
    reo_destination: Vec<AllocatedDpRing>,
    pdev_rx: Vec<AllocatedDpRing>,
}

/// Failed initial DP allocation with every owner retained for cleanup/retry.
pub struct DpAllocationError<B: Backend, R: DpRingOps<B>> {
    cause: DpError,
    cleanup_error: Option<DpError>,
    device: Device<B>,
    rings: R,
    resources: Wcn6750DpRings,
}

impl<B: Backend, R: DpRingOps<B>> DpAllocationError<B, R> {
    pub(crate) fn new(
        cause: DpError,
        device: Device<B>,
        mut rings: R,
        mut resources: Wcn6750DpRings,
    ) -> Self {
        let cleanup_error = resources.free_common(&mut rings).err();
        Self {
            cause,
            cleanup_error,
            device,
            rings,
            resources,
        }
    }

    pub fn cause(&self) -> DpError {
        self.cause
    }

    pub fn cleanup_error(&self) -> Option<DpError> {
        self.cleanup_error
    }

    pub fn retry_cleanup(&mut self) -> Result<(), DpError> {
        self.resources.free_common(&mut self.rings)?;
        self.cleanup_error = None;
        Ok(())
    }

    pub fn into_parts(mut self) -> Result<(Device<B>, R), Self> {
        if self.retry_cleanup().is_err() {
            return Err(self);
        }
        Ok((self.device, self.rings))
    }
}

impl Wcn6750DpRings {
    pub fn common(&self) -> &[AllocatedDpRing] {
        &self.common
    }

    pub fn reo_destination(&self) -> &[AllocatedDpRing] {
        &self.reo_destination
    }

    pub fn pdev_rx(&self) -> &[AllocatedDpRing] {
        &self.pdev_rx
    }

    pub(crate) fn allocate_common<B: Backend, R: DpRingOps<B>>(
        &mut self,
        device: &Device<B>,
        rings: &mut R,
    ) -> Result<(), DpError> {
        if !self.common.is_empty() {
            return Err(DpError::WrongState);
        }
        allocate_group(device, rings, COMMON_RINGS, &mut self.common)
    }

    pub(crate) fn allocate_reo_destination<B: Backend, R: DpRingOps<B>>(
        &mut self,
        device: &Device<B>,
        rings: &mut R,
    ) -> Result<DataRings, DpError> {
        if self.common.is_empty() || !self.reo_destination.is_empty() {
            return Err(DpError::WrongState);
        }
        allocate_group(
            device,
            rings,
            REO_DESTINATION_RINGS,
            &mut self.reo_destination,
        )?;
        Ok(DataRings {
            tcl: find(&self.common, RingType::TclData, 0)?,
            reo: find(&self.reo_destination, RingType::ReoDestination, 0)?,
            wbm: find(&self.common, RingType::WbmToSwRelease, 0)?,
        })
    }

    pub(crate) fn allocate_pdev_rx<B: Backend, R: DpRingOps<B>>(
        &mut self,
        device: &Device<B>,
        rings: &mut R,
    ) -> Result<RingId, DpError> {
        if self.reo_destination.is_empty() || !self.pdev_rx.is_empty() {
            return Err(DpError::WrongState);
        }
        allocate_group(device, rings, PDEV_RX_RINGS, &mut self.pdev_rx)?;
        find(&self.pdev_rx, RingType::RxdmaBuffer, 0)
    }

    pub(crate) fn free_pdev_rx<B: Backend, R: DpRingOps<B>>(
        &mut self,
        rings: &mut R,
    ) -> Result<(), DpError> {
        free_group(rings, &mut self.pdev_rx)
    }

    pub(crate) fn free_reo_destination<B: Backend, R: DpRingOps<B>>(
        &mut self,
        rings: &mut R,
    ) -> Result<(), DpError> {
        if !self.pdev_rx.is_empty() {
            return Err(DpError::WrongState);
        }
        free_group(rings, &mut self.reo_destination)
    }

    pub(crate) fn free_common<B: Backend, R: DpRingOps<B>>(
        &mut self,
        rings: &mut R,
    ) -> Result<(), DpError> {
        if !self.pdev_rx.is_empty() || !self.reo_destination.is_empty() {
            return Err(DpError::WrongState);
        }
        free_group(rings, &mut self.common)
    }

    pub(crate) fn reo_controller_rings(&self) -> Result<(RingId, RingId), DpError> {
        Ok((
            find(&self.common, RingType::ReoCommand, 0)?,
            find(&self.common, RingType::ReoStatus, 0)?,
        ))
    }

    pub(crate) fn pdev_ring(
        &self,
        ring_type: RingType,
        ring_number: u8,
    ) -> Result<RingId, DpError> {
        find(&self.pdev_rx, ring_type, ring_number)
    }
}

fn allocate_group<B: Backend, R: DpRingOps<B>>(
    device: &Device<B>,
    rings: &mut R,
    specs: &[DpRingSpec],
    allocated: &mut Vec<AllocatedDpRing>,
) -> Result<(), DpError> {
    for &spec in specs {
        let id = allocate_ring(device, rings, spec)?;
        allocated.push(AllocatedDpRing { id, spec });
    }
    Ok(())
}

fn allocate_ring<B: Backend, R: DpRingOps<B>>(
    device: &Device<B>,
    rings: &mut R,
    spec: DpRingSpec,
) -> Result<RingId, DpError> {
    let entries = u32::from(spec.entries).min(Wcn6750Registers::max_entries(spec.ring_type));
    let entries = u16::try_from(entries).map_err(|_| DpError::NoResources)?;
    let entry_bytes = Wcn6750Registers::entry_size(spec.ring_type);
    let bytes = usize::from(entries)
        .checked_mul(entry_bytes)
        .and_then(|size| size.checked_add(RING_BASE_ALIGN - 1))
        .ok_or(DpError::NoResources)?;
    let dma = device
        .alloc_coherent::<Bidirectional>(bytes, RING_BASE_ALIGN)
        .map_err(|_| DpError::NoResources)?;
    rings
        .create_dp_ring(
            spec,
            RingMemory {
                dma,
                entries,
                entry_bytes: u16::try_from(entry_bytes).map_err(|_| DpError::NoResources)?,
            },
        )
        .map_err(map_hal)
}

fn free_group<B: Backend, R: DpRingOps<B>>(
    rings: &mut R,
    allocated: &mut Vec<AllocatedDpRing>,
) -> Result<(), DpError> {
    while let Some(ring) = allocated.first() {
        rings.destroy(ring.id).map_err(map_hal)?;
        allocated.remove(0);
    }
    Ok(())
}

fn find(
    rings: &[AllocatedDpRing],
    ring_type: RingType,
    ring_number: u8,
) -> Result<RingId, DpError> {
    rings
        .iter()
        .find(|ring| ring.spec.ring_type == ring_type && ring.spec.ring_number == ring_number)
        .map(|ring| ring.id)
        .ok_or(DpError::NoResources)
}

#[cfg(test)]
const fn kind(ring_type: RingType) -> ath11k_hal::RingKind {
    use ath11k_hal::RingKind;
    match ring_type {
        RingType::TclData | RingType::TclCommand | RingType::TclStatus => RingKind::Tcl,
        RingType::ReoDestination
        | RingType::ReoException
        | RingType::ReoReinject
        | RingType::ReoCommand
        | RingType::ReoStatus => RingKind::Reo,
        RingType::WbmIdleLink | RingType::SwToWbmRelease | RingType::WbmToSwRelease => {
            RingKind::Wbm
        }
        RingType::RxdmaBuffer
        | RingType::RxdmaDestination
        | RingType::RxdmaMonitorBuffer
        | RingType::RxdmaMonitorStatus
        | RingType::RxdmaMonitorDestination
        | RingType::RxdmaMonitorDescriptor
        | RingType::RxdmaDirectBuffer => RingKind::Rxdma,
        RingType::CeSource | RingType::CeDestination | RingType::CeDestinationStatus => {
            RingKind::Ce
        }
    }
}

fn map_hal(error: HalError) -> DpError {
    match error {
        HalError::WrongDescriptorLength => DpError::MalformedDescriptor,
        HalError::NoResources => DpError::NoResources,
        HalError::DeviceFault => DpError::DeviceFault,
        HalError::Unsupported => DpError::UnsupportedDescriptor,
    }
}

// dp.c:ath11k_dp_alloc / ath11k_dp_srng_common_setup, using WCN6750's
// three-entry tcl2wbm map {0->0, 1->4, 2->2} and 2048-entry TCL rings.
const COMMON_RINGS: &[DpRingSpec] = &[
    DpRingSpec::new(RingType::WbmIdleLink, 0, 0, 32_767),
    DpRingSpec::new(RingType::SwToWbmRelease, 0, 0, 64),
    DpRingSpec::new(RingType::TclCommand, 0, 0, 32),
    DpRingSpec::new(RingType::TclStatus, 0, 0, 32),
    DpRingSpec::new(RingType::TclData, 0, 0, 2_048),
    DpRingSpec::new(RingType::WbmToSwRelease, 0, 0, 32_768),
    DpRingSpec::new(RingType::TclData, 1, 0, 2_048),
    DpRingSpec::new(RingType::WbmToSwRelease, 4, 0, 32_768),
    DpRingSpec::new(RingType::TclData, 2, 0, 2_048),
    DpRingSpec::new(RingType::WbmToSwRelease, 2, 0, 32_768),
    DpRingSpec::new(RingType::ReoReinject, 0, 0, 32),
    DpRingSpec::new(RingType::WbmToSwRelease, 3, 0, 1_024),
    DpRingSpec::new(RingType::ReoException, 0, 0, 128),
    DpRingSpec::new(RingType::ReoCommand, 0, 0, 256),
    DpRingSpec::new(RingType::ReoStatus, 0, 0, 2_048),
];

const REO_DESTINATION_RINGS: &[DpRingSpec] = &[
    DpRingSpec::new(RingType::ReoDestination, 0, 0, 2_048),
    DpRingSpec::new(RingType::ReoDestination, 1, 0, 2_048),
    DpRingSpec::new(RingType::ReoDestination, 2, 0, 2_048),
    DpRingSpec::new(RingType::ReoDestination, 3, 0, 2_048),
];

// WCN6750 has one pdev, one RXDMA, rx_mac_buf_ring=true and rxdma1=false.
const PDEV_RX_RINGS: &[DpRingSpec] = &[
    DpRingSpec::new(RingType::RxdmaBuffer, 0, 0, 4_096),
    DpRingSpec::new(RingType::RxdmaBuffer, 1, 0, 1_024),
    DpRingSpec::new(RingType::RxdmaDestination, 0, 0, 1_024),
    DpRingSpec::new(RingType::RxdmaMonitorStatus, 0, 0, 1_024),
];

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use ath11k_hal::{Descriptor, RingKind};
    use drv_hardware_backends::DeterministicBackend;

    #[derive(Default)]
    struct ModelRings {
        next: u16,
        fail_at: Option<u16>,
        fail_destroy: Option<RingId>,
        created: Vec<(RingKind, u16, u16, usize)>,
        destroyed: Vec<RingId>,
    }

    impl Rings<DeterministicBackend> for ModelRings {
        fn create(
            &mut self,
            kind: RingKind,
            memory: RingMemory<DeterministicBackend>,
        ) -> Result<RingId, HalError> {
            if self.fail_at == Some(self.next) {
                return Err(HalError::NoResources);
            }
            let id = RingId(self.next);
            self.next += 1;
            self.created
                .push((kind, memory.entries, memory.entry_bytes, memory.dma.len()));
            Ok(id)
        }

        fn publish(&mut self, _: RingId, _: Descriptor) -> Result<(), HalError> {
            Ok(())
        }

        fn consume(&mut self, _: RingId) -> Result<Option<Descriptor>, HalError> {
            Ok(None)
        }
    }

    impl DpRingOps<DeterministicBackend> for ModelRings {
        fn create_dp_ring(
            &mut self,
            spec: DpRingSpec,
            memory: RingMemory<DeterministicBackend>,
        ) -> Result<RingId, HalError> {
            self.create(kind(spec.ring_type), memory)
        }

        fn destroy(&mut self, ring: RingId) -> Result<(), HalError> {
            if self.fail_destroy == Some(ring) {
                return Err(HalError::DeviceFault);
            }
            self.destroyed.push(ring);
            Ok(())
        }

        fn send_htt_ring_setup<C: crate::HttControl>(
            &self,
            _: usize,
            _: &mut C,
        ) -> Result<bool, DpError> {
            Ok(false)
        }

        fn setup_reo_controller(
            &self,
            _: RingId,
            _: RingId,
        ) -> Result<crate::reo::ReoController, DpError> {
            Err(DpError::UnsupportedDescriptor)
        }
    }

    #[test]
    fn wcn6750_plan_matches_pinned_c_order_and_sizes() {
        assert_eq!(COMMON_RINGS.len(), 15);
        assert_eq!(REO_DESTINATION_RINGS.len(), 4);
        assert_eq!(PDEV_RX_RINGS.len(), 4);
        assert!(
            REO_DESTINATION_RINGS
                .iter()
                .enumerate()
                .all(|(number, spec)| {
                    spec.ring_type == RingType::ReoDestination
                        && usize::from(spec.ring_number) == number
                        && spec.entries == 2_048
                })
        );
        assert_eq!(
            PDEV_RX_RINGS
                .iter()
                .map(|spec| (spec.ring_type, spec.ring_number, spec.entries))
                .collect::<Vec<_>>(),
            vec![
                (RingType::RxdmaBuffer, 0, 4_096),
                (RingType::RxdmaBuffer, 1, 1_024),
                (RingType::RxdmaDestination, 0, 1_024),
                (RingType::RxdmaMonitorStatus, 0, 1_024),
            ]
        );
        assert_eq!(
            COMMON_RINGS
                .iter()
                .map(|spec| (spec.ring_type, spec.ring_number, spec.entries))
                .collect::<Vec<_>>(),
            vec![
                (RingType::WbmIdleLink, 0, 32_767),
                (RingType::SwToWbmRelease, 0, 64),
                (RingType::TclCommand, 0, 32),
                (RingType::TclStatus, 0, 32),
                (RingType::TclData, 0, 2_048),
                (RingType::WbmToSwRelease, 0, 32_768),
                (RingType::TclData, 1, 2_048),
                (RingType::WbmToSwRelease, 4, 32_768),
                (RingType::TclData, 2, 2_048),
                (RingType::WbmToSwRelease, 2, 32_768),
                (RingType::ReoReinject, 0, 32),
                (RingType::WbmToSwRelease, 3, 1_024),
                (RingType::ReoException, 0, 128),
                (RingType::ReoCommand, 0, 256),
                (RingType::ReoStatus, 0, 2_048),
            ]
        );
    }

    #[test]
    fn allocation_failure_preserves_created_prefix_for_owner_cleanup() {
        let device = DeterministicBackend::device();
        let specs = [
            DpRingSpec::new(RingType::TclCommand, 0, 0, 2),
            DpRingSpec::new(RingType::TclStatus, 0, 0, 2),
            DpRingSpec::new(RingType::ReoCommand, 0, 0, 2),
        ];
        let mut rings = ModelRings {
            fail_at: Some(2),
            ..Default::default()
        };
        let mut allocated = Vec::new();
        assert_eq!(
            allocate_group(&device, &mut rings, &specs, &mut allocated),
            Err(DpError::NoResources)
        );
        assert_eq!(allocated.len(), 2);
        assert!(rings.destroyed.is_empty());
        assert_eq!(rings.created[0], (RingKind::Tcl, 2, 32, 71));
    }

    #[test]
    fn failed_teardown_keeps_failed_ring_owned_for_retry() {
        let mut rings = ModelRings {
            fail_destroy: Some(RingId(1)),
            ..Default::default()
        };
        let mut allocated = vec![
            AllocatedDpRing {
                id: RingId(0),
                spec: DpRingSpec::new(RingType::TclCommand, 0, 0, 2),
            },
            AllocatedDpRing {
                id: RingId(1),
                spec: DpRingSpec::new(RingType::TclStatus, 0, 0, 2),
            },
        ];
        assert_eq!(
            free_group::<DeterministicBackend, _>(&mut rings, &mut allocated),
            Err(DpError::DeviceFault)
        );
        assert_eq!(allocated.len(), 1);
        rings.fail_destroy = None;
        free_group::<DeterministicBackend, _>(&mut rings, &mut allocated).unwrap();
        assert!(allocated.is_empty());
        assert_eq!(rings.destroyed, [RingId(0), RingId(1)]);
    }
}
