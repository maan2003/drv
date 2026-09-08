// PORT-MAP: reusable
//! REO command/status and per-peer receive-TID lifecycle.

use alloc::vec::Vec;
use ath11k_hal::{
    PacketNumberType, ReoCommand, ReoCommandKind, ReoCommandParams, ReoQueueDescriptor,
    ReoResources, ReoStatus, RingId, Rings, setup_wcn6750,
};
use ath11k_platform_backend::{Backend, Bidirectional, Device, MmioRegion};
use ath11k_wmi::Transport;
use ath11k_wmi::cmd::{EncodeCommand, PeerReorderQueueSetup};
use dma_pool::{DmaPool, DmaSegment};

use crate::DpError;

/// Non-coherent REO queue descriptor owned by one peer/TID.
pub struct ReoTid<B: Backend> {
    pub tid: u8,
    pub ba_window_size: u32,
    size: usize,
    dma: DmaSegment<B, Bidirectional>,
}

impl<B: Backend> ReoTid<B> {
    /// `ath11k_peer_rx_tid_setup`'s allocation, descriptor setup, and required
    /// post-write sync. The caller performs the WMI reorder-queue command.
    pub fn setup(
        pool: &DmaPool<B, Bidirectional>,
        tid: u8,
        ba_window_size: u32,
        start_sequence: u16,
        pn: PacketNumberType,
    ) -> Result<Self, DpError> {
        let descriptor =
            ReoQueueDescriptor::new(tid, ba_window_size, u32::from(start_sequence), pn);
        let mut dma = pool.allocate().map_err(|_| DpError::NoResources)?;
        dma.write(0, descriptor.bytes())
            .map_err(|_| DpError::DeviceFault)?;
        dma.sync_for_device(0, descriptor.bytes().len())
            .map_err(|_| DpError::DeviceFault)?;
        Ok(Self {
            tid,
            ba_window_size,
            size: descriptor.bytes().len(),
            dma,
        })
    }

    pub fn device_address(
        &self,
    ) -> Result<ath11k_platform_backend::DeviceAddress<'_, B, Bidirectional>, DpError> {
        self.dma.device_address(0).map_err(|_| DpError::DeviceFault)
    }

    fn device_address_at(
        &self,
        offset: usize,
    ) -> Result<ath11k_platform_backend::DeviceAddress<'_, B, Bidirectional>, DpError> {
        self.dma
            .device_address(offset)
            .map_err(|_| DpError::DeviceFault)
    }

    fn descriptor_size(&self) -> usize {
        self.size
    }
}

const REO_QUEUE_DESCRIPTOR_BYTES: usize = 512;
const REO_QUEUE_DESCRIPTOR_ALIGNMENT: usize = 128;
const REO_DESCRIPTOR_FREE_THRESHOLD: usize = 64;
const REO_DESCRIPTOR_FREE_TIMEOUT_MS: u64 = 1_000;
const UPDATE_VALID: u32 = 1 << 9;
const UPDATE_BA_WINDOW_SIZE: u32 = 1 << 18;
const UPDATE_START_SEQUENCE: u32 = 1 << 26;
const START_SEQUENCE_SHIFT: u32 = 11;

struct PendingTid<B: Backend> {
    command_number: u16,
    vdev_id: u32,
    peer_addr: [u8; 6],
    tid: ReoTid<B>,
}

struct CachedTid<B: Backend> {
    queued_at_ms: u64,
    vdev_id: u32,
    peer_addr: [u8; 6],
    tid: ReoTid<B>,
}

struct ActiveTid<B: Backend> {
    vdev_id: u32,
    peer_addr: [u8; 6],
    tid: ReoTid<B>,
}

/// Global peer receive-reorder queue coordinator. Every peer's pending
/// command is kept here because they share one REO status ring.
pub struct PeerRxTids<B: Backend> {
    pool: DmaPool<B, Bidirectional>,
    tids: Vec<ActiveTid<B>>,
    pending_delete: Vec<PendingTid<B>>,
    cached_delete: Vec<CachedTid<B>>,
    pending_flush: Vec<PendingTid<B>>,
    uncertain_setup: Vec<ActiveTid<B>>,
    failed_delete: Vec<ActiveTid<B>>,
}

impl<B: Backend> PeerRxTids<B> {
    pub fn new(device: Device<B>) -> Result<Self, DpError> {
        Ok(Self {
            pool: DmaPool::new(
                device,
                REO_QUEUE_DESCRIPTOR_BYTES,
                4096,
                REO_QUEUE_DESCRIPTOR_ALIGNMENT,
                REO_DESCRIPTOR_FREE_THRESHOLD,
            )
            .map_err(|_| DpError::NoResources)?,
            tids: Vec::new(),
            pending_delete: Vec::new(),
            cached_delete: Vec::new(),
            pending_flush: Vec::new(),
            uncertain_setup: Vec::new(),
            failed_delete: Vec::new(),
        })
    }

    /// Ports `ath11k_peer_rx_tid_setup`, including the already-active update
    /// path and the reorder-queue WMI publication boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn ath11k_peer_rx_tid_setup<R: Rings<B>, T: Transport>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        wmi: &mut T,
        vdev_id: u32,
        peer_addr: [u8; 6],
        tid: u8,
        ba_window_size: u32,
        start_sequence: u16,
        pn: PacketNumberType,
    ) -> Result<(), DpError> {
        if tid > 16 {
            return Err(DpError::WrongState);
        }
        if self
            .uncertain_setup
            .iter()
            .chain(&self.failed_delete)
            .any(|entry| {
                entry.vdev_id == vdev_id && entry.peer_addr == peer_addr && entry.tid.tid == tid
            })
        {
            return Err(DpError::WrongState);
        }
        if let Some(active) = self.tids.iter_mut().find(|active| {
            active.vdev_id == vdev_id && active.peer_addr == peer_addr && active.tid.tid == tid
        }) {
            let params = ReoCommandParams {
                update0: UPDATE_BA_WINDOW_SIZE | UPDATE_START_SEQUENCE,
                update2: u32::from(start_sequence) << START_SEQUENCE_SHIFT,
                ba_window_size: ba_window_size.try_into().map_err(|_| DpError::WrongState)?,
                ..ReoCommandParams::default().need_status()
            };
            controller.ath11k_dp_tx_send_reo_cmd(
                rings,
                ReoCommandKind::UpdateRxQueue,
                &active.tid,
                params,
            )?;
            active.tid.ba_window_size = ba_window_size;
            return send_reorder_setup(wmi, vdev_id, peer_addr, &active.tid, ba_window_size);
        }

        let new_tid = ReoTid::setup(&self.pool, tid, ba_window_size, start_sequence, pn)?;
        if let Err(error) = send_reorder_setup(wmi, vdev_id, peer_addr, &new_tid, ba_window_size) {
            if !T::SEND_ERROR_IS_NON_VISIBLE {
                // The failed command may contain this IOVA. Quarantine the
                // owner until peer teardown rather than permit pool reuse.
                self.uncertain_setup.push(ActiveTid {
                    vdev_id,
                    peer_addr,
                    tid: new_tid,
                });
            }
            return Err(error);
        }
        self.tids.push(ActiveTid {
            vdev_id,
            peer_addr,
            tid: new_tid,
        });
        Ok(())
    }

    /// Ports `ath11k_peer_rx_tid_delete`: remove active publication first,
    /// invalidate the hardware queue, and retain DMA ownership until status.
    pub fn ath11k_peer_rx_tid_delete<R: Rings<B>>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        vdev_id: u32,
        peer_addr: [u8; 6],
        tid: u8,
    ) -> Result<(), DpError> {
        let Some(index) = self.tids.iter().position(|active| {
            active.vdev_id == vdev_id && active.peer_addr == peer_addr && active.tid.tid == tid
        }) else {
            return Ok(());
        };
        let owned = self.tids.swap_remove(index).tid;
        let result = controller.ath11k_dp_tx_send_reo_cmd(
            rings,
            ReoCommandKind::UpdateRxQueue,
            &owned,
            ReoCommandParams {
                update0: UPDATE_VALID,
                ..ReoCommandParams::default().need_status()
            },
        );
        match result {
            Ok(command_number) => {
                self.pending_delete.push(PendingTid {
                    command_number,
                    vdev_id,
                    peer_addr,
                    tid: owned,
                });
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Ports `ath11k_dp_rx_tid_del_func` and its aged REO cache invalidation.
    pub fn ath11k_dp_rx_tid_del_func<R: Rings<B>>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        now_ms: u64,
    ) -> Result<Vec<ReoStatus>, DpError> {
        let mut statuses = Vec::new();
        while let Some(descriptor) = rings.consume(controller.status_ring).map_err(map_hal)? {
            let status = ReoStatus::decode(&descriptor).map_err(map_hal)?;
            self.apply_status(status, now_ms);
            statuses.push(status);
        }
        self.flush_aged(controller, rings, now_ms)?;
        Ok(statuses)
    }

    fn apply_status(&mut self, status: ReoStatus, now_ms: u64) {
        if let Some(index) = self
            .pending_delete
            .iter()
            .position(|pending| pending.command_number == status.header.command_number)
        {
            let pending = self.pending_delete.swap_remove(index);
            if status.header.execution_status == 0 {
                self.cached_delete.push(CachedTid {
                    queued_at_ms: now_ms,
                    vdev_id: pending.vdev_id,
                    peer_addr: pending.peer_addr,
                    tid: pending.tid,
                });
            } else {
                self.failed_delete.push(ActiveTid {
                    vdev_id: pending.vdev_id,
                    peer_addr: pending.peer_addr,
                    tid: pending.tid,
                });
            }
        } else if let Some(index) = self
            .pending_flush
            .iter()
            .position(|pending| pending.command_number == status.header.command_number)
        {
            // Success and failure both release the host owner, matching
            // ath11k_dp_reo_cmd_free's terminal callback behavior.
            self.pending_flush.swap_remove(index);
        }
    }

    pub fn flush_aged<R: Rings<B>>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        now_ms: u64,
    ) -> Result<(), DpError> {
        let mut index = 0;
        while index < self.cached_delete.len() {
            let aged = now_ms.saturating_sub(self.cached_delete[index].queued_at_ms)
                > REO_DESCRIPTOR_FREE_TIMEOUT_MS;
            if self.cached_delete.len() > REO_DESCRIPTOR_FREE_THRESHOLD || aged {
                let cached = self.cached_delete.swap_remove(index);
                self.flush_one(controller, rings, cached)?;
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    fn flush_one<R: Rings<B>>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        cached: CachedTid<B>,
    ) -> Result<(), DpError> {
        let tid = cached.tid;
        let mut offset = tid.descriptor_size();
        while offset > 128 {
            offset -= 128;
            let _ = controller.send_at(
                rings,
                ReoCommandKind::FlushCache,
                &tid,
                offset,
                ReoCommandParams::default(),
            );
        }
        let command_number = controller.send_at(
            rings,
            ReoCommandKind::FlushCache,
            &tid,
            0,
            ReoCommandParams::default().need_status(),
        )?;
        self.pending_flush.push(PendingTid {
            command_number,
            vdev_id: cached.vdev_id,
            peer_addr: cached.peer_addr,
            tid,
        });
        Ok(())
    }

    pub fn is_active(&self, vdev_id: u32, peer_addr: [u8; 6], tid: u8) -> bool {
        self.tids.iter().any(|active| {
            active.vdev_id == vdev_id && active.peer_addr == peer_addr && active.tid.tid == tid
        })
    }
}

fn send_reorder_setup<B: Backend, T: Transport>(
    wmi: &mut T,
    vdev_id: u32,
    peer_addr: [u8; 6],
    tid: &ReoTid<B>,
    ba_window_size: u32,
) -> Result<(), DpError> {
    let request = PeerReorderQueueSetup {
        vdev_id,
        peer_addr,
        tid: tid.tid,
        queue_address: tid.device_address()?.bits(),
        ba_window_size_valid: 1,
        ba_window_size,
    };
    let command = request.encode_command().map_err(|_| DpError::DeviceFault)?;
    wmi.send(command).map_err(|_| DpError::DeviceFault)
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
        self.send_at(rings, kind, tid, 0, params)
    }

    fn send_at<B: Backend, R: Rings<B>>(
        &mut self,
        rings: &mut R,
        kind: ReoCommandKind,
        tid: &ReoTid<B>,
        offset: usize,
        params: ReoCommandParams,
    ) -> Result<u16, DpError> {
        let command_number = self.next_command_number;
        self.next_command_number = self.next_command_number.wrapping_add(1).max(1);
        let command = ReoCommand::encode(
            command_number,
            kind,
            &tid.device_address_at(offset)?,
            params,
            &mut self.resources,
        )
        .map_err(map_hal)?;
        rings
            .publish(self.command_ring, command.into_descriptor())
            .map_err(map_hal)?;
        Ok(command_number)
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
    use alloc::vec;
    use ath11k_hal::{Descriptor, HalError, RingKind, RingMemory};
    use drv_hardware_backends::{DeterministicBackend, Operation};

    #[derive(Default)]
    struct ModelRings {
        published: Vec<(RingId, Descriptor)>,
        status: VecDeque<Descriptor>,
        fail_publish: bool,
        fail_publish_attempt: Option<usize>,
        publish_attempts: usize,
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
            self.publish_attempts += 1;
            if self.fail_publish || self.fail_publish_attempt == Some(self.publish_attempts) {
                return Err(HalError::NoResources);
            }
            self.published.push((ring, descriptor));
            Ok(())
        }
        fn consume(&mut self, _: RingId) -> Result<Option<Descriptor>, HalError> {
            Ok(self.status.pop_front())
        }
    }

    #[test]
    fn tid_descriptor_is_synced_before_reo_command_publication() {
        let (device, operations) = DeterministicBackend::recording_noncoherent_device();
        let pool = DmaPool::new(device, 512, 4096, 128, 64).unwrap();
        let tid = ReoTid::setup(&pool, 3, 64, 0x123, PacketNumberType::Wpa).unwrap();
        assert!(matches!(
            operations.borrow().last(),
            Some(Operation::SyncForDevice { range, .. }) if range.end - range.start == 512
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

    #[derive(Default)]
    struct ModelWmi {
        commands: Vec<ath11k_wmi::Command>,
        fail: bool,
    }

    impl Transport for ModelWmi {
        const SEND_ERROR_IS_NON_VISIBLE: bool = true;

        fn send(&mut self, command: ath11k_wmi::Command) -> Result<(), ath11k_wmi::WmiError> {
            if self.fail {
                return Err(ath11k_wmi::WmiError::Transport);
            }
            self.commands.push(command);
            Ok(())
        }

        fn receive(&mut self, _: u64) -> Result<Option<ath11k_wmi::Event>, ath11k_wmi::WmiError> {
            Ok(None)
        }
    }

    #[derive(Default)]
    struct UncertainWmi;

    impl Transport for UncertainWmi {
        fn send(&mut self, _: ath11k_wmi::Command) -> Result<(), ath11k_wmi::WmiError> {
            Err(ath11k_wmi::WmiError::Transport)
        }

        fn receive(&mut self, _: u64) -> Result<Option<ath11k_wmi::Event>, ath11k_wmi::WmiError> {
            Ok(None)
        }
    }

    fn controller() -> ReoController {
        ReoController {
            command_ring: RingId(8),
            status_ring: RingId(9),
            next_command_number: 1,
            resources: ReoResources::default(),
        }
    }

    fn status(command_number: u16, kind: u32, execution_status: u8) -> Descriptor {
        let mut bytes = vec![0; 104];
        let header = (kind & 0x1ff) << 1 | 100 << 10;
        bytes[0..4].copy_from_slice(&header.to_le_bytes());
        let info = u32::from(command_number) | u32::from(execution_status) << 26;
        bytes[4..8].copy_from_slice(&info.to_le_bytes());
        Descriptor::new(bytes, 104).unwrap()
    }

    #[test]
    fn peer_tid_setup_syncs_then_publishes_typed_wmi_and_rolls_back_on_error() {
        let (device, operations) = DeterministicBackend::recording_noncoherent_device();
        let mut peer = PeerRxTids::new(device).unwrap();
        let mut rings = ModelRings::default();
        let mut reo = controller();
        let mut wmi = ModelWmi::default();
        peer.ath11k_peer_rx_tid_setup(
            &mut reo,
            &mut rings,
            &mut wmi,
            4,
            [1, 2, 3, 4, 5, 6],
            3,
            64,
            0x123,
            PacketNumberType::Wpa,
        )
        .unwrap();
        assert!(peer.is_active(4, [1, 2, 3, 4, 5, 6], 3));
        assert_eq!(wmi.commands.len(), 1);
        assert_eq!(
            wmi.commands[0].id,
            ath11k_wmi::tags::WMI_PEER_REORDER_QUEUE_SETUP_CMDID
        );
        assert!(
            matches!(operations.borrow().last(), Some(Operation::SyncForDevice { range, .. }) if range.end - range.start == 512)
        );

        let mut failed_wmi = ModelWmi {
            fail: true,
            ..ModelWmi::default()
        };
        assert_eq!(
            peer.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut failed_wmi,
                4,
                [1, 2, 3, 4, 5, 6],
                5,
                32,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::DeviceFault)
        );
        assert!(!peer.is_active(4, [1, 2, 3, 4, 5, 6], 5));
        assert_eq!(peer.pool.free_segments(), 7);
    }

    #[test]
    fn delete_keeps_owner_until_update_and_flush_statuses() {
        let device = DeterministicBackend::device();
        let mut peer = PeerRxTids::new(device).unwrap();
        let mut rings = ModelRings::default();
        let mut reo = controller();
        let mut wmi = ModelWmi::default();
        peer.ath11k_peer_rx_tid_setup(
            &mut reo,
            &mut rings,
            &mut wmi,
            1,
            [2; 6],
            3,
            64,
            0,
            PacketNumberType::None,
        )
        .unwrap();
        peer.ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [2; 6], 3)
            .unwrap();
        assert!(!peer.is_active(1, [2; 6], 3));
        assert_eq!(peer.pending_delete.len(), 1);
        assert_eq!(peer.pool.free_segments(), 7);

        rings.status.push_back(status(1, 153, 0));
        peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 0)
            .unwrap();
        assert_eq!(peer.pending_delete.len(), 0);
        assert_eq!(peer.cached_delete.len(), 1);
        peer.flush_aged(&mut reo, &mut rings, 1_001).unwrap();
        assert_eq!(peer.cached_delete.len(), 0);
        assert_eq!(peer.pending_flush.len(), 1);
        // Three extension flushes plus the base descriptor flush.
        assert_eq!(rings.published.len(), 5);

        rings.status.push_back(status(5, 313, 0));
        peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 1_001)
            .unwrap();
        assert_eq!(peer.pending_flush.len(), 0);
        assert_eq!(peer.pool.free_segments(), 8);
    }

    #[test]
    fn setup_sync_and_delete_publication_failures_leak_no_owner() {
        let (device, failures) = DeterministicBackend::noncoherent_device_with_failures();
        let mut peer = PeerRxTids::new(device).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        failures.fail_next_sync_for_device();
        assert_eq!(
            peer.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                1,
                [3; 6],
                2,
                64,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::NoResources)
        );
        assert!(!peer.is_active(1, [3; 6], 2));
        assert!(wmi.commands.is_empty());

        peer.ath11k_peer_rx_tid_setup(
            &mut reo,
            &mut rings,
            &mut wmi,
            1,
            [3; 6],
            2,
            64,
            0,
            PacketNumberType::None,
        )
        .unwrap();
        rings.fail_publish = true;
        assert_eq!(
            peer.ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [3; 6], 2),
            Err(DpError::NoResources)
        );
        assert!(!peer.is_active(1, [3; 6], 2));
        assert!(peer.pending_delete.is_empty());
    }

    #[test]
    fn uncertain_wmi_failure_quarantines_descriptor_without_active_publication() {
        let mut peer = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        assert_eq!(
            peer.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut UncertainWmi,
                1,
                [9; 6],
                4,
                64,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::DeviceFault)
        );
        assert!(!peer.is_active(1, [9; 6], 4));
        assert_eq!(peer.uncertain_setup.len(), 1);
        assert_eq!(peer.pool.free_segments(), 7);
        let mut retry = ModelWmi::default();
        assert_eq!(
            peer.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut retry,
                1,
                [9; 6],
                4,
                64,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::WrongState)
        );
        assert!(retry.commands.is_empty());
    }

    #[test]
    fn every_extension_flush_failure_still_gates_owner_on_base_status() {
        for failed_extension in 0..3 {
            let mut peer = PeerRxTids::new(DeterministicBackend::device()).unwrap();
            let mut reo = controller();
            let mut rings = ModelRings::default();
            let mut wmi = ModelWmi::default();
            peer.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                1,
                [4; 6],
                3,
                64,
                0,
                PacketNumberType::None,
            )
            .unwrap();
            peer.ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [4; 6], 3)
                .unwrap();
            rings.status.push_back(status(1, 153, 0));
            peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 0)
                .unwrap();

            rings.fail_publish_attempt = Some(2 + failed_extension);
            peer.flush_aged(&mut reo, &mut rings, 1_001).unwrap();
            assert_eq!(peer.pending_flush.len(), 1);
            assert_eq!(peer.pool.free_segments(), 7);
            rings.status.push_back(status(5, 313, 0));
            peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 1_001)
                .unwrap();
            assert!(peer.pending_flush.is_empty());
            assert_eq!(peer.pool.free_segments(), 8);
        }
    }

    #[test]
    fn malformed_status_does_not_discard_applied_completion_prefix() {
        let mut peer = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        peer.ath11k_peer_rx_tid_setup(
            &mut reo,
            &mut rings,
            &mut wmi,
            1,
            [5; 6],
            3,
            64,
            0,
            PacketNumberType::None,
        )
        .unwrap();
        peer.ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [5; 6], 3)
            .unwrap();
        rings.status.push_back(status(1, 153, 0));
        rings
            .status
            .push_back(Descriptor::new(vec![0; 40], 40).unwrap());
        assert_eq!(
            peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 0),
            Err(DpError::MalformedDescriptor)
        );
        assert!(peer.pending_delete.is_empty());
        assert_eq!(peer.cached_delete.len(), 1);

        peer.flush_aged(&mut reo, &mut rings, 1_001).unwrap();
        rings.status.push_back(status(5, 313, 0));
        rings
            .status
            .push_back(Descriptor::new(vec![0; 40], 40).unwrap());
        assert_eq!(
            peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 1_001),
            Err(DpError::MalformedDescriptor)
        );
        assert!(peer.pending_flush.is_empty());
        assert_eq!(peer.pool.free_segments(), 8);
    }

    #[test]
    fn global_status_dispatch_handles_two_peers_sharing_the_same_tid() {
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        for addr in [[6; 6], [7; 6]] {
            peers
                .ath11k_peer_rx_tid_setup(
                    &mut reo,
                    &mut rings,
                    &mut wmi,
                    1,
                    addr,
                    3,
                    64,
                    0,
                    PacketNumberType::None,
                )
                .unwrap();
            peers
                .ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, addr, 3)
                .unwrap();
        }
        rings.status.push_back(status(2, 153, 0));
        rings.status.push_back(status(1, 153, 0));
        peers
            .ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 0)
            .unwrap();
        assert!(peers.pending_delete.is_empty());
        assert_eq!(peers.cached_delete.len(), 2);
    }

    #[test]
    fn failed_invalidate_execution_quarantines_owner_until_reset() {
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        peers
            .ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                1,
                [8; 6],
                3,
                64,
                0,
                PacketNumberType::None,
            )
            .unwrap();
        peers
            .ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [8; 6], 3)
            .unwrap();
        rings.status.push_back(status(1, 153, 2));
        peers
            .ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 0)
            .unwrap();
        assert!(peers.pending_delete.is_empty());
        assert_eq!(peers.failed_delete.len(), 1);
        assert_eq!(peers.pool.free_segments(), 7);
        assert_eq!(
            peers.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                1,
                [8; 6],
                3,
                64,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::WrongState)
        );
    }
}
