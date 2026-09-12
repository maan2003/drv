//! HAL-backed WCN6750 datapath ring owner.

use alloc::vec::Vec;
use ath11k_hal::{
    Descriptor, HalError, RingFlags, RingId, RingKind, RingMemory, RingType, Rings, Srng,
    SrngParams,
};
use ath11k_platform_backend::{Backend, Bidirectional, CoherentDma, Device, MmioRegion};

use crate::{
    DpError, DpRingOps, DpRingSpec,
    htt::{SrngFlags, SrngRingId, SrngRingType, SrngSetup},
};

const HAL_RING_COUNT: usize = 172;
const LMAC_RING_START: u16 = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DpRingMsi {
    pub ring_type: RingType,
    pub ring_number: u8,
    pub mac_id: u8,
    pub address: u64,
    pub data: u32,
}

struct Ring<B: Backend> {
    spec: DpRingSpec,
    srng: Srng<B>,
    params: SrngParams,
}

/// Production DP ring owner. Its MMIO view and remote read/write pointer
/// arrays remain alive independently of CE's packet-I/O owners.
pub struct HalDpRings<B: Backend> {
    mmio: MmioRegion<B>,
    remote_read_pointers: CoherentDma<B, Bidirectional>,
    remote_write_pointers: CoherentDma<B, Bidirectional>,
    msi: Vec<DpRingMsi>,
    rings: Vec<Ring<B>>,
    shadow_registers: Option<Vec<u32>>,
}

impl<B: Backend> HalDpRings<B> {
    /// An empty MSI table selects the polling-first mode required by
    /// `specs/ARCH-dma-broker.md`; a nonempty table selects MSI mode and must
    /// cover every interrupt-bearing ring.
    pub fn new(
        device: &Device<B>,
        mmio: MmioRegion<B>,
        msi: &[DpRingMsi],
    ) -> Result<Self, DpError> {
        for (index, entry) in msi.iter().enumerate() {
            if entry.address == 0 {
                return Err(DpError::WrongState);
            }
            if msi[..index].iter().any(|previous| {
                (previous.ring_type, previous.ring_number, previous.mac_id)
                    == (entry.ring_type, entry.ring_number, entry.mac_id)
            }) {
                return Err(DpError::WrongState);
            }
        }
        let remote_read_pointers = device
            .alloc_coherent(HAL_RING_COUNT * 4, 8)
            .map_err(|_| DpError::NoResources)?;
        let write_pointer_bytes = (HAL_RING_COUNT - usize::from(LMAC_RING_START)) * 4;
        let remote_write_pointers = device
            .alloc_coherent(write_pointer_bytes, 8)
            .map_err(|_| DpError::NoResources)?;
        Ok(Self {
            mmio,
            remote_read_pointers,
            remote_write_pointers,
            msi: msi.to_vec(),
            rings: Vec::new(),
            shadow_registers: None,
        })
    }

    /// Use the target table already accepted by the running firmware.
    pub fn with_shadow_registers(mut self, targets: Vec<u32>) -> Self {
        self.shadow_registers = Some(targets);
        self
    }

    fn parameters(&self, spec: DpRingSpec) -> SrngParams {
        let mut params = SrngParams::default();
        match spec.ring_type {
            RingType::ReoDestination => {
                params.interrupt_batch_entries = 128;
                params.interrupt_timer_us = 500;
            }
            RingType::RxdmaBuffer | RingType::RxdmaMonitorBuffer | RingType::RxdmaMonitorStatus => {
                params.interrupt_timer_us = 500;
                params.low_threshold = u32::from(spec.entries) >> 3;
                params.flags = params.flags.union(RingFlags::LOW_THRESHOLD_INTERRUPT);
            }
            RingType::WbmToSwRelease if spec.ring_number < 3 => {
                params.interrupt_batch_entries = 256;
                params.interrupt_timer_us = 1_000;
            }
            _ => {
                params.interrupt_batch_entries = 1;
                params.interrupt_timer_us = 256;
            }
        }
        if let Some(msi) = self.msi.iter().find(|msi| {
            (msi.ring_type, msi.ring_number, msi.mac_id)
                == (spec.ring_type, spec.ring_number, spec.mac_id)
        }) {
            params.msi_address = msi.address;
            params.msi_data = msi.data;
            params.flags = params.flags.union(RingFlags::MSI_INTERRUPT);
        }
        params
    }

    fn pointer_address(&self, ring: &Ring<B>, head: bool) -> Result<u64, DpError> {
        let id = ring.srng.id.0;
        let source = ring.srng.direction == ath11k_hal::RingDirection::Source;
        let use_write_pointer = source == head;
        let address = if use_write_pointer {
            self.remote_write_pointers.device_address(
                usize::from(
                    id.checked_sub(LMAC_RING_START)
                        .ok_or(DpError::DeviceFault)?,
                ) * 4,
            )
        } else {
            self.remote_read_pointers
                .device_address(usize::from(id) * 4)
        }
        .map_err(|_| DpError::DeviceFault)?;
        Ok(address.bits())
    }

    fn htt_setup(&self, ring: &Ring<B>) -> Result<Option<SrngSetup>, DpError> {
        let (ring_id, ring_type, pdev_id) = match ring.spec.ring_type {
            RingType::RxdmaBuffer if ring.srng.id.0 == LMAC_RING_START => (
                SrngRingId::Host1ToFirmwareRxBuffer,
                SrngRingType::SoftwareToSoftware,
                ring.spec.mac_id,
            ),
            RingType::RxdmaBuffer => (
                SrngRingId::RxdmaHostBuffer,
                SrngRingType::SoftwareToHardware,
                ring.spec.mac_id + 1,
            ),
            RingType::RxdmaDestination => (
                SrngRingId::RxdmaNonMonitorDestination,
                SrngRingType::HardwareToSoftware,
                ring.spec.mac_id + 1,
            ),
            RingType::RxdmaMonitorStatus => (
                SrngRingId::RxdmaMonitorStatus,
                SrngRingType::SoftwareToHardware,
                ring.spec.mac_id + 1,
            ),
            _ => return Ok(None),
        };
        let entry_words = ring.srng.entry_size() / 4;
        let ring_words = usize::from(ring.srng.memory.entries) * entry_words;
        Ok(Some(SrngSetup {
            pdev_id,
            ring_id,
            ring_type,
            ring_base_address: ring
                .srng
                .memory
                .dma
                .device_address(0)
                .map_err(|_| DpError::DeviceFault)?
                .bits(),
            ring_size_words: u16::try_from(ring_words).map_err(|_| DpError::NoResources)?,
            ring_entry_size_words: u8::try_from(entry_words).map_err(|_| DpError::NoResources)?,
            head_address: self.pointer_address(ring, true)?,
            tail_address: self.pointer_address(ring, false)?,
            msi_address: ring.params.msi_address,
            msi_data: ring.params.msi_data,
            interrupt_batch_threshold_words: u16::try_from(
                ring.params.interrupt_batch_entries * entry_words as u32,
            )
            .map_err(|_| DpError::NoResources)?,
            interrupt_timer_threshold: u16::try_from(ring.params.interrupt_timer_us >> 3)
                .map_err(|_| DpError::NoResources)?,
            interrupt_low_threshold: u16::try_from(ring.params.low_threshold)
                .map_err(|_| DpError::NoResources)?,
            flags: SrngFlags {
                msi_swap: ring.params.flags.contains(RingFlags::MSI_SWAP),
                host_firmware_swap: ring.params.flags.contains(RingFlags::POINTER_SWAP),
                tlv_swap: ring.params.flags.contains(RingFlags::DATA_TLV_SWAP),
                low_threshold_interrupt: ring
                    .params
                    .flags
                    .contains(RingFlags::LOW_THRESHOLD_INTERRUPT),
            },
        }))
    }
}

impl<B: Backend> Rings<B> for HalDpRings<B> {
    fn create(&mut self, _: RingKind, _: RingMemory<B>) -> Result<RingId, HalError> {
        Err(HalError::Unsupported)
    }

    fn publish(&mut self, id: RingId, descriptor: Descriptor) -> Result<(), HalError> {
        let Self {
            mmio,
            remote_read_pointers,
            remote_write_pointers,
            rings,
            ..
        } = self;
        let ring = rings
            .iter_mut()
            .find(|ring| ring.srng.id == id)
            .ok_or(HalError::NoResources)?;
        if ring.srng.direction != ath11k_hal::RingDirection::Source {
            return Err(HalError::Unsupported);
        }
        if descriptor.bytes().len() != ring.srng.entry_size() {
            return Err(HalError::WrongDescriptorLength);
        }
        ring.srng.access_begin_remote(remote_read_pointers)?;
        let offset = ring.srng.peek().ok_or(HalError::NoResources)?;
        ring.srng
            .memory
            .dma
            .write(offset, descriptor.bytes())
            .map_err(|_| HalError::DeviceFault)?;
        let cursor = ring.srng.checkpoint();
        if ring.srng.source_next() != Some(offset) {
            ring.srng.restore(cursor);
            return Err(HalError::DeviceFault);
        }
        let result = if is_lmac(ring.spec.ring_type) {
            ring.srng.access_end_lmac(remote_write_pointers)
        } else {
            ring.srng.access_end(mmio)
        };
        if result.is_err() {
            ring.srng.restore(cursor);
        }
        result
    }

    fn consume(&mut self, id: RingId) -> Result<Option<Descriptor>, HalError> {
        let Self {
            mmio,
            remote_read_pointers,
            remote_write_pointers,
            rings,
            ..
        } = self;
        let ring = rings
            .iter_mut()
            .find(|ring| ring.srng.id == id)
            .ok_or(HalError::NoResources)?;
        if ring.srng.direction != ath11k_hal::RingDirection::Destination {
            return Err(HalError::Unsupported);
        }
        ring.srng.access_begin_remote(remote_read_pointers)?;
        let Some(offset) = ring.srng.peek() else {
            return Ok(None);
        };
        let mut bytes = alloc::vec![0; ring.srng.entry_size()];
        ring.srng
            .memory
            .dma
            .read(offset, &mut bytes)
            .map_err(|_| HalError::DeviceFault)?;
        let cursor = ring.srng.checkpoint();
        if ring.srng.destination_next() != Some(offset) {
            ring.srng.restore(cursor);
            return Err(HalError::DeviceFault);
        }
        let result = if is_lmac(ring.spec.ring_type) {
            ring.srng.access_end_lmac(remote_write_pointers)
        } else {
            ring.srng.access_end(mmio)
        };
        if let Err(error) = result {
            ring.srng.restore(cursor);
            return Err(error);
        }
        Descriptor::new(bytes, ring.srng.entry_size()).map(Some)
    }
}

impl<B: Backend> DpRingOps<B> for HalDpRings<B> {
    fn create_dp_ring(
        &mut self,
        spec: DpRingSpec,
        memory: RingMemory<B>,
    ) -> Result<RingId, HalError> {
        let expected =
            ath11k_hal::Wcn6750Registers::ring_id(spec.ring_type, spec.ring_number, spec.mac_id)
                .ok_or(HalError::NoResources)?;
        if self.rings.iter().any(|ring| ring.srng.id == expected) {
            return Err(HalError::NoResources);
        }
        if !self.msi.is_empty()
            && requires_msi(spec)
            && !self.msi.iter().any(|msi| {
                (msi.ring_type, msi.ring_number, msi.mac_id)
                    == (spec.ring_type, spec.ring_number, spec.mac_id)
            })
        {
            return Err(HalError::NoResources);
        }
        // Linux clears the complete RDP allocation before ring setup in
        // `hal.c:ath11k_hal_alloc`, and `ath11k_hal_srng_setup` then selects
        // `hal->rdp.vaddr + ring_id`. We permit owner reuse, so repeat the
        // per-ID part here before every setup instead of inheriting a stale
        // hardware pointer from an earlier ring with the same ID.
        self.remote_read_pointers
            .write(usize::from(expected.0) * 4, &0_u32.to_le_bytes())
            .map_err(|_| HalError::DeviceFault)?;
        if is_lmac(spec.ring_type) {
            self.remote_write_pointers
                .write(
                    usize::from(expected.0 - LMAC_RING_START) * 4,
                    &0_u32.to_le_bytes(),
                )
                .map_err(|_| HalError::DeviceFault)?;
        }
        let params = self.parameters(spec);
        let mut srng = Srng::setup(
            &self.mmio,
            spec.ring_type,
            spec.ring_number,
            spec.mac_id,
            memory,
            &self.remote_read_pointers,
            params,
        )?;
        if let Some(targets) = &self.shadow_registers {
            srng.use_shadow_registers(targets)?;
        }
        let id = srng.id;
        debug_assert_eq!(id, expected);
        self.rings.push(Ring { spec, srng, params });
        Ok(id)
    }

    fn destroy(&mut self, id: RingId) -> Result<(), HalError> {
        let index = self
            .rings
            .iter()
            .position(|ring| ring.srng.id == id)
            .ok_or(HalError::NoResources)?;
        self.rings[index].srng.teardown(
            &mut self.remote_read_pointers,
            &mut self.remote_write_pointers,
        )?;
        self.rings.remove(index);
        Ok(())
    }

    fn send_htt_ring_setup<C: crate::HttControl>(
        &self,
        index: usize,
        control: &mut C,
    ) -> Result<bool, DpError> {
        let setup = self
            .rings
            .iter()
            .filter_map(|ring| self.htt_setup(ring).transpose())
            .nth(index)
            .transpose()?;
        let Some(setup) = setup else { return Ok(false) };
        crate::htt::send_srng_setup(control, setup)?;
        Ok(true)
    }

    fn setup_reo_controller(
        &self,
        command_ring: RingId,
        status_ring: RingId,
    ) -> Result<crate::reo::ReoController, DpError> {
        crate::reo::ReoController::ath11k_dp_pdev_reo_setup(&self.mmio, command_ring, status_ring)
    }
}

const fn is_lmac(ring_type: RingType) -> bool {
    matches!(
        ring_type,
        RingType::RxdmaBuffer
            | RingType::RxdmaDestination
            | RingType::RxdmaMonitorBuffer
            | RingType::RxdmaMonitorStatus
            | RingType::RxdmaMonitorDestination
            | RingType::RxdmaMonitorDescriptor
            | RingType::RxdmaDirectBuffer
    )
}

const fn requires_msi(spec: DpRingSpec) -> bool {
    matches!(
        spec.ring_type,
        RingType::WbmToSwRelease
            | RingType::ReoDestination
            | RingType::ReoException
            | RingType::ReoStatus
            | RingType::RxdmaDestination
            | RingType::RxdmaMonitorStatus
    )
}
