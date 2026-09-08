// PORT-MAP: reusable
//! REO command/status and per-peer receive-TID lifecycle.

use alloc::vec::Vec;
use ath11k_hal::{
    PacketNumberType, ReoCommand, ReoCommandKind, ReoCommandParams, ReoQueueDescriptor,
    ReoResources, ReoStatus, RingId, Rings, setup_wcn6750,
};
use ath11k_platform_backend::{Backend, Bidirectional, Device, MmioRegion, StreamingDma};

use crate::DpError;

/// Non-coherent REO queue descriptor owned by one peer/TID.
pub struct ReoTid<B: Backend> {
    pub tid: u8,
    pub ba_window_size: u32,
    dma: StreamingDma<B, Bidirectional>,
}

impl<B: Backend> ReoTid<B> {
    /// `ath11k_peer_rx_tid_setup`'s allocation, descriptor setup, and required
    /// post-write sync. The caller performs the WMI reorder-queue command.
    pub fn setup(
        device: &Device<B>,
        tid: u8,
        ba_window_size: u32,
        start_sequence: u16,
        pn: PacketNumberType,
    ) -> Result<Self, DpError> {
        let descriptor = ReoQueueDescriptor::new(
            tid,
            if tid == 16 { ba_window_size } else { 256 },
            u32::from(start_sequence),
            pn,
        );
        let mut dma = device
            .alloc_streaming::<Bidirectional>(descriptor.bytes().len(), 128)
            .map_err(|_| DpError::NoResources)?;
        dma.write(0, descriptor.bytes())
            .map_err(|_| DpError::DeviceFault)?;
        dma.sync_for_device(0, descriptor.bytes().len())
            .map_err(|_| DpError::DeviceFault)?;
        Ok(Self {
            tid,
            ba_window_size,
            dma,
        })
    }

    pub fn device_address(
        &self,
    ) -> Result<ath11k_platform_backend::DeviceAddress<'_, B, Bidirectional>, DpError> {
        self.dma.device_address(0).map_err(|_| DpError::DeviceFault)
    }
}

pub struct ReoController {
    command_ring: RingId,
    status_ring: RingId,
    next_command_number: u16,
    resources: ReoResources,
}

impl ReoController {
    /// `ath11k_dp_pdev_reo_setup`, after the coherent command ring has been
    /// initialized by HAL's `initialize_command_ring` during ring allocation.
    pub fn ath11k_dp_pdev_reo_setup<B: Backend>(
        mmio: &MmioRegion<B>,
        command_ring: RingId,
        status_ring: RingId,
    ) -> Result<Self, DpError> {
        setup_wcn6750(mmio).map_err(map_hal)?;
        Ok(Self {
            command_ring,
            status_ring,
            next_command_number: 1,
            resources: ReoResources::default(),
        })
    }

    pub fn ath11k_dp_pdev_reo_cleanup(self) {}

    pub fn ath11k_dp_tx_send_reo_cmd<B: Backend, R: Rings<B>>(
        &mut self,
        rings: &mut R,
        kind: ReoCommandKind,
        tid: &ReoTid<B>,
        params: ReoCommandParams,
    ) -> Result<u16, DpError> {
        let command_number = self.next_command_number;
        self.next_command_number = self.next_command_number.wrapping_add(1).max(1);
        let command = ReoCommand::encode(
            command_number,
            kind,
            &tid.device_address()?,
            params,
            &mut self.resources,
        )
        .map_err(map_hal)?;
        rings
            .publish(self.command_ring, command.into_descriptor())
            .map_err(map_hal)?;
        Ok(command_number)
    }

    pub fn ath11k_dp_process_reo_status<B: Backend, R: Rings<B>>(
        &mut self,
        rings: &mut R,
    ) -> Result<Vec<ReoStatus>, DpError> {
        let mut statuses = Vec::new();
        while let Some(descriptor) = rings.consume(self.status_ring).map_err(map_hal)? {
            statuses.push(ReoStatus::decode(&descriptor).map_err(map_hal)?);
        }
        Ok(statuses)
    }
}

fn map_hal(error: ath11k_hal::HalError) -> DpError {
    match error {
        ath11k_hal::HalError::WrongDescriptorLength => DpError::MalformedDescriptor,
        ath11k_hal::HalError::NoResources => DpError::NoResources,
        ath11k_hal::HalError::DeviceFault => DpError::DeviceFault,
        ath11k_hal::HalError::Unsupported => DpError::UnsupportedDescriptor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use ath11k_hal::{Descriptor, HalError, RingKind, RingMemory};
    use drv_hardware_backends::{DeterministicBackend, Operation};

    #[derive(Default)]
    struct ModelRings {
        published: Vec<(RingId, Descriptor)>,
        status: VecDeque<Descriptor>,
    }

    impl Rings<DeterministicBackend> for ModelRings {
        fn create(
            &mut self,
            _: RingKind,
            _: RingMemory<DeterministicBackend>,
        ) -> Result<RingId, HalError> {
            Ok(RingId(0))
        }
        fn publish(&mut self, ring: RingId, descriptor: Descriptor) -> Result<(), HalError> {
            self.published.push((ring, descriptor));
            Ok(())
        }
        fn consume(&mut self, _: RingId) -> Result<Option<Descriptor>, HalError> {
            Ok(self.status.pop_front())
        }
    }

    #[test]
    fn tid_descriptor_is_synced_before_reo_command_publication() {
        let (device, operations) = DeterministicBackend::recording_device();
        let tid = ReoTid::setup(&device, 3, 64, 0x123, PacketNumberType::Wpa).unwrap();
        assert!(matches!(
            operations.borrow().last(),
            Some(Operation::SyncForDevice { range, .. }) if range == &(0..512)
        ));
        let mut controller = ReoController {
            command_ring: RingId(8),
            status_ring: RingId(9),
            next_command_number: 1,
            resources: ReoResources::default(),
        };
        let mut rings = ModelRings::default();
        let number = controller
            .ath11k_dp_tx_send_reo_cmd(
                &mut rings,
                ReoCommandKind::QueueStats,
                &tid,
                ReoCommandParams::default().need_status(),
            )
            .unwrap();
        assert_eq!(number, 1);
        assert_eq!(rings.published[0].0, RingId(8));
        assert_eq!(rings.published[0].1.bytes().len(), 40);
    }
}
