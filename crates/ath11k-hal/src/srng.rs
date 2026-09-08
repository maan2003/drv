// PORT-MAP: reusable
//! WCN6750 scatter/gather ring (SRNG) programming and index arithmetic.
//!
//! Offsets and write order follow Linux `hal.c` and the `wcn6750_regs` table
//! at commit 509ce3d952d550f93b544c8d94c99e798f09a9b4.

use crate::{HalError, RingId, RingMemory};
use ath11k_platform_backend::{Backend, Bidirectional, CoherentDma, MmioRegion};
use core::sync::atomic::{Ordering, fence};

const UMAC_REO: usize = 0x00a3_8000;
const UMAC_TCL: usize = 0x00a4_4000;
const UMAC_WBM: usize = 0x00a3_4000;
const CE0_SRC: usize = 0x01b8_0000;
const CE0_DST: usize = 0x01b8_1000;
const CE_STRIDE: usize = 0x2000;
const RING_SIZE_SHIFT: u32 = 8;
const RING_ENABLE: u32 = 1 << 6;
const SRC_LOOP_COUNT_DISABLE: u32 = 1 << 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RingDirection {
    Source,
    Destination,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RingType {
    ReoDestination,
    ReoException,
    ReoReinject,
    ReoCommand,
    ReoStatus,
    TclData,
    TclCommand,
    TclStatus,
    CeSource,
    CeDestination,
    CeDestinationStatus,
    WbmIdleLink,
    SwToWbmRelease,
    WbmToSwRelease,
    RxdmaBuffer,
    RxdmaDestination,
    RxdmaMonitorBuffer,
    RxdmaMonitorStatus,
    RxdmaMonitorDestination,
    RxdmaMonitorDescriptor,
    RxdmaDirectBuffer,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RingFlags(u32);
impl RingFlags {
    pub const MSI_SWAP: Self = Self(0x0000_0008);
    pub const POINTER_SWAP: Self = Self(0x0000_0010);
    pub const DATA_TLV_SWAP: Self = Self(0x0000_0020);
    pub const LOW_THRESHOLD_INTERRUPT: Self = Self(0x0001_0000);
    pub const MSI_INTERRUPT: Self = Self(0x0002_0000);
    pub const CACHED: Self = Self(0x2000_0000);
    pub const LMAC_RING: Self = Self(0x8000_0000);
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }
    pub const fn bits(self) -> u32 {
        self.0
    }
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SrngParams {
    pub interrupt_batch_entries: u32,
    pub interrupt_timer_us: u32,
    pub flags: RingFlags,
    pub max_buffer_len: u32,
    pub low_threshold: u32,
    pub msi_address: u64,
    pub msi_data: u32,
}

#[derive(Clone, Copy)]
struct Config {
    start_id: u16,
    max_rings: u8,
    entry_words: u16,
    direction: RingDirection,
    max_size_words: u32,
    lmac: bool,
    r0: usize,
    r2: usize,
    r0_stride: usize,
    r2_stride: usize,
}

/// The register layout selected by `ath11k_hw_params` for WCN6750.
// PORT-MAP: wcn6750-specific
#[derive(Clone, Copy, Debug, Default)]
pub struct Wcn6750Registers;
impl Wcn6750Registers {
    pub const fn entry_size(ring_type: RingType) -> usize {
        config(ring_type).entry_words as usize * 4
    }
    pub const fn max_entries(ring_type: RingType) -> u32 {
        let c = config(ring_type);
        c.max_size_words / c.entry_words as u32
    }
    pub const fn ring_id(ring_type: RingType, ring_number: u8, mac_id: u8) -> Option<RingId> {
        let c = config(ring_type);
        if ring_number >= c.max_rings {
            return None;
        }
        let lmac = if c.lmac { mac_id as u16 * 15 } else { 0 };
        let id = c.start_id + ring_number as u16 + lmac;
        if id >= 172 { None } else { Some(RingId(id)) }
    }
}

const fn config(t: RingType) -> Config {
    match t {
        RingType::ReoDestination => c(
            0,
            4,
            16,
            RingDirection::Destination,
            0xfffff,
            false,
            UMAC_REO + 0x1ec,
            UMAC_REO + 0x3028,
            0x58,
            8,
        ),
        RingType::ReoException => c(
            4,
            1,
            16,
            RingDirection::Destination,
            0xfffff,
            false,
            UMAC_REO + 0x3fc,
            UMAC_REO + 0x3058,
            0,
            0,
        ),
        RingType::ReoReinject => c(
            5,
            1,
            8,
            RingDirection::Source,
            0xffff,
            false,
            UMAC_REO + 0x13c,
            UMAC_REO + 0x3018,
            0,
            0,
        ),
        RingType::ReoCommand => c(
            8,
            1,
            10,
            RingDirection::Source,
            0xffff,
            false,
            UMAC_REO + 0xe4,
            UMAC_REO + 0x3010,
            0,
            0,
        ),
        RingType::ReoStatus => c(
            9,
            1,
            26,
            RingDirection::Destination,
            0xffff,
            false,
            UMAC_REO + 0x504,
            UMAC_REO + 0x3070,
            0,
            0,
        ),
        RingType::TclData => c(
            16,
            3,
            7,
            RingDirection::Source,
            0xfffff,
            false,
            UMAC_TCL + 0x694,
            UMAC_TCL + 0x2000,
            0x58,
            8,
        ),
        RingType::TclCommand => c(
            24,
            1,
            8,
            RingDirection::Source,
            0xfffff,
            false,
            UMAC_TCL + 0x79c,
            UMAC_TCL + 0x2018,
            0,
            0,
        ),
        RingType::TclStatus => c(
            25,
            1,
            9,
            RingDirection::Destination,
            0xffff,
            false,
            UMAC_TCL + 0x8a4,
            UMAC_TCL + 0x2030,
            0,
            0,
        ),
        RingType::CeSource => c(
            32,
            12,
            4,
            RingDirection::Source,
            0xffff,
            false,
            CE0_SRC,
            CE0_SRC + 0x400,
            CE_STRIDE,
            CE_STRIDE,
        ),
        RingType::CeDestination => c(
            56,
            12,
            2,
            RingDirection::Source,
            0xffff,
            false,
            CE0_DST,
            CE0_DST + 0x400,
            CE_STRIDE,
            CE_STRIDE,
        ),
        RingType::CeDestinationStatus => c(
            80,
            12,
            4,
            RingDirection::Destination,
            0xffff,
            false,
            CE0_DST + 0x58,
            CE0_DST + 0x408,
            CE_STRIDE,
            CE_STRIDE,
        ),
        RingType::WbmIdleLink => c(
            104,
            1,
            2,
            RingDirection::Source,
            0xffff,
            false,
            UMAC_WBM + 0x874,
            UMAC_WBM + 0x30b0,
            0,
            0,
        ),
        RingType::SwToWbmRelease => c(
            105,
            1,
            8,
            RingDirection::Source,
            0xffff,
            false,
            UMAC_WBM + 0x1ec,
            UMAC_WBM + 0x3018,
            0,
            0,
        ),
        RingType::WbmToSwRelease => c(
            106,
            5,
            8,
            RingDirection::Destination,
            0xfffff,
            false,
            UMAC_WBM + 0x924,
            UMAC_WBM + 0x30c0,
            0x58,
            8,
        ),
        RingType::RxdmaBuffer => c(128, 2, 2, RingDirection::Source, 0xffff, true, 0, 0, 0, 0),
        RingType::RxdmaDestination => c(
            133,
            1,
            8,
            RingDirection::Destination,
            0xffff,
            true,
            0,
            0,
            0,
            0,
        ),
        RingType::RxdmaMonitorBuffer => {
            c(130, 1, 2, RingDirection::Source, 0xffff, true, 0, 0, 0, 0)
        }
        RingType::RxdmaMonitorStatus => {
            c(132, 1, 2, RingDirection::Source, 0xffff, true, 0, 0, 0, 0)
        }
        RingType::RxdmaMonitorDestination => c(
            134,
            1,
            8,
            RingDirection::Destination,
            0xffff,
            true,
            0,
            0,
            0,
            0,
        ),
        RingType::RxdmaMonitorDescriptor => {
            c(135, 1, 2, RingDirection::Source, 0xffff, true, 0, 0, 0, 0)
        }
        RingType::RxdmaDirectBuffer => {
            c(136, 2, 2, RingDirection::Source, 0xffff, true, 0, 0, 0, 0)
        }
    }
}

#[allow(clippy::too_many_arguments)]
const fn c(
    start_id: u16,
    max_rings: u8,
    entry_words: u16,
    direction: RingDirection,
    max_size_words: u32,
    lmac: bool,
    r0: usize,
    r2: usize,
    r0_stride: usize,
    r2_stride: usize,
) -> Config {
    Config {
        start_id,
        max_rings,
        entry_words,
        direction,
        max_size_words,
        lmac,
        r0,
        r2,
        r0_stride,
        r2_stride,
    }
}

/// Host state for one SRNG. Indices are in dwords, exactly as in Linux HAL.
// PORT-MAP: reusable
pub struct Srng<B: Backend> {
    pub id: RingId,
    pub ring_type: RingType,
    pub direction: RingDirection,
    pub memory: RingMemory<B>,
    entry_words: u32,
    ring_words: u32,
    r0: usize,
    r2: usize,
    pointer_offset: usize,
    firmware_pointer_offset: usize,
    publication_offset: usize,
    head: u32,
    tail: u32,
    cached_hardware_pointer: u32,
    reap_head: u32,
    loop_count: u16,
    flags: RingFlags,
}

#[derive(Clone, Copy)]
pub struct SrngCursor {
    head: u32,
    tail: u32,
    reap_head: u32,
    loop_count: u16,
}

impl<B: Backend> Srng<B> {
    pub fn checkpoint(&self) -> SrngCursor {
        SrngCursor {
            head: self.head,
            tail: self.tail,
            reap_head: self.reap_head,
            loop_count: self.loop_count,
        }
    }

    pub fn restore(&mut self, cursor: SrngCursor) {
        self.head = cursor.head;
        self.tail = cursor.tail;
        self.reap_head = cursor.reap_head;
        self.loop_count = cursor.loop_count;
    }
    /// Equivalent to `ath11k_hal_srng_setup`. `remote_read_pointers` is the
    /// coherent RDP array indexed by hardware ring id.
    pub fn setup(
        mmio: &MmioRegion<B>,
        ring_type: RingType,
        ring_number: u8,
        mac_id: u8,
        mut memory: RingMemory<B>,
        remote_read_pointers: &CoherentDma<B, Bidirectional>,
        params: SrngParams,
    ) -> Result<Self, HalError> {
        let c = config(ring_type);
        let id = Wcn6750Registers::ring_id(ring_type, ring_number, mac_id)
            .ok_or(HalError::NoResources)?;
        let entry_words = c.entry_words as u32;
        let ring_words = entry_words * u32::from(memory.entries);
        memory
            .dma
            .write(0, &alloc::vec![0; ring_words as usize * 4])
            .map_err(|_| HalError::DeviceFault)?;
        let r0 = c.r0 + usize::from(ring_number) * c.r0_stride;
        let r2 = c.r2 + usize::from(ring_number) * c.r2_stride;
        let pointer_offset = usize::from(id.0) * 4;
        let firmware_pointer_offset = if c.lmac {
            usize::from(id.0 - 128) * 4
        } else {
            0
        };
        let publication_offset = if c.direction == RingDirection::Source {
            r2
        } else {
            r2 + 4
        };
        let flags = if c.lmac {
            params.flags.union(RingFlags::LMAC_RING)
        } else {
            params.flags
        };
        let ring = Self {
            id,
            ring_type,
            direction: c.direction,
            memory,
            entry_words,
            ring_words,
            r0,
            r2,
            pointer_offset,
            firmware_pointer_offset,
            publication_offset,
            head: 0,
            tail: 0,
            cached_hardware_pointer: 0,
            reap_head: ring_words - entry_words,
            loop_count: 1,
            flags,
        };
        if !c.lmac {
            ring.program(mmio, remote_read_pointers, params)?;
            if ring_type == RingType::CeDestination {
                let control = mmio
                    .read_u32(r0 + 0xb0)
                    .map_err(|_| HalError::DeviceFault)?;
                w(
                    mmio,
                    r0 + 0xb0,
                    (control & !0xffff) | (params.max_buffer_len & 0xffff),
                )?;
            }
        }
        Ok(ring)
    }

    fn program(
        &self,
        mmio: &MmioRegion<B>,
        rdp: &CoherentDma<B, Bidirectional>,
        p: SrngParams,
    ) -> Result<(), HalError> {
        let src = self.direction == RingDirection::Source;
        let (
            base_msb,
            id_off,
            misc,
            pointer_lsb,
            pointer_msb,
            intr0,
            intr1,
            msi_lsb,
            msi_msb,
            msi_data,
        ) = if src {
            (4, 8, 0x10, 0x1c, 0x20, 0x30, 0x34, 0x48, 0x4c, 0x50)
        } else {
            (4, 8, 0x10, 0x14, 0x18, 0x24, 0, 0x48, 0x4c, 0x50)
        };
        if self.flags.contains(RingFlags::MSI_INTERRUPT) {
            w(mmio, self.r0 + msi_lsb, p.msi_address as u32)?;
            w(
                mmio,
                self.r0 + msi_msb,
                ((p.msi_address >> 32) as u32 & 0xff) | (1 << 8),
            )?;
            w(mmio, self.r0 + msi_data, p.msi_data)?;
        }
        mmio.write_device_address(
            self.r0,
            Some(self.r0 + base_msb),
            self.memory
                .dma
                .device_address(0)
                .map_err(|_| HalError::DeviceFault)?,
        )
        .map_err(|_| HalError::DeviceFault)?;
        // write_device_address supplies address bits; Linux ORs ring size into MSB.
        let size = self.ring_words << RING_SIZE_SHIFT;
        let msb = mmio
            .read_u32(self.r0 + base_msb)
            .map_err(|_| HalError::DeviceFault)?
            | size;
        w(mmio, self.r0 + base_msb, msb)?;
        w(
            mmio,
            self.r0 + id_off,
            if src {
                self.entry_words
            } else {
                (u32::from(self.id.0) << 8) | self.entry_words
            },
        )?;
        if src && self.id.0 == 104 {
            mmio.write_device_address(
                self.r0,
                Some(self.r0 + base_msb),
                self.memory
                    .dma
                    .device_address(0)
                    .map_err(|_| HalError::DeviceFault)?,
            )
            .map_err(|_| HalError::DeviceFault)?;
            let msb = mmio
                .read_u32(self.r0 + base_msb)
                .map_err(|_| HalError::DeviceFault)?
                | size;
            w(mmio, self.r0 + base_msb, msb)?;
        }
        let timer = if src {
            p.interrupt_timer_us
        } else {
            p.interrupt_timer_us >> 3
        };
        w(
            mmio,
            self.r0 + intr0,
            (timer << 16) | (p.interrupt_batch_entries * self.entry_words),
        )?;
        if src {
            w(
                mmio,
                self.r0 + intr1,
                if self.flags.contains(RingFlags::LOW_THRESHOLD_INTERRUPT) {
                    p.low_threshold * self.entry_words
                } else {
                    0
                },
            )?;
        }
        if !(src && self.id.0 == 104) {
            mmio.write_device_address(
                self.r0 + pointer_lsb,
                Some(self.r0 + pointer_msb),
                rdp.device_address(self.pointer_offset)
                    .map_err(|_| HalError::DeviceFault)?,
            )
            .map_err(|_| HalError::DeviceFault)?;
        }
        w(mmio, self.r2, 0)?;
        w(mmio, self.r2 + 4, 0)?;
        let mut value = RING_ENABLE;
        if src {
            value |= SRC_LOOP_COUNT_DISABLE;
        }
        if self.flags.contains(RingFlags::MSI_SWAP) {
            value |= 1 << 3;
        }
        if self.flags.contains(RingFlags::POINTER_SWAP) {
            value |= 1 << 4;
        }
        if self.flags.contains(RingFlags::DATA_TLV_SWAP) {
            value |= 1 << 5;
        }
        w(mmio, self.r0 + misc, value)
    }

    pub const fn entry_size(&self) -> usize {
        self.entry_words as usize * 4
    }
    pub const fn peek(&self) -> Option<usize> {
        match self.direction {
            RingDirection::Source
                if (self.head + self.entry_words) % self.ring_words
                    != self.cached_hardware_pointer =>
            {
                Some(self.head as usize * 4)
            }
            RingDirection::Destination if self.tail != self.cached_hardware_pointer => {
                Some(self.tail as usize * 4)
            }
            _ => None,
        }
    }
    pub fn source_next(&mut self) -> Option<usize> {
        if self.direction != RingDirection::Source {
            return None;
        }
        let next = (self.head + self.entry_words) % self.ring_words;
        if next == self.cached_hardware_pointer {
            return None;
        }
        let offset = self.head as usize * 4;
        self.head = next;
        self.reap_head = next;
        Some(offset)
    }
    /// Port of `ath11k_hal_srng_src_reap_next`.
    pub fn source_reap_next(&mut self) -> Option<usize> {
        if self.direction != RingDirection::Source {
            return None;
        }
        let next = (self.reap_head + self.entry_words) % self.ring_words;
        if next == self.cached_hardware_pointer {
            return None;
        }
        self.reap_head = next;
        Some(next as usize * 4)
    }
    /// Port of `ath11k_hal_srng_src_get_next_reaped`.
    pub fn source_next_reaped(&mut self) -> Option<usize> {
        if self.direction != RingDirection::Source || self.head == self.reap_head {
            return None;
        }
        let offset = self.head as usize * 4;
        self.head = (self.head + self.entry_words) % self.ring_words;
        Some(offset)
    }
    /// Port of `ath11k_hal_srng_src_next_peek`.
    pub fn source_next_peek(&self) -> Option<usize> {
        if self.direction != RingDirection::Source {
            return None;
        }
        let next = (self.head + self.entry_words) % self.ring_words;
        (next != self.cached_hardware_pointer).then_some(next as usize * 4)
    }
    pub fn destination_next(&mut self) -> Option<usize> {
        if self.direction != RingDirection::Destination || self.tail == self.cached_hardware_pointer
        {
            return None;
        }
        let offset = self.tail as usize * 4;
        self.tail += self.entry_words;
        if self.tail == self.ring_words {
            self.tail = 0;
            self.loop_count = self.loop_count.wrapping_add(1);
        }
        Some(offset)
    }
    pub fn number_free(&self) -> u32 {
        let hw = self.cached_hardware_pointer;
        match self.direction {
            RingDirection::Source if hw > self.head => (hw - self.head) / self.entry_words - 1,
            RingDirection::Source => (self.ring_words - self.head + hw) / self.entry_words - 1,
            RingDirection::Destination if hw >= self.tail => (hw - self.tail) / self.entry_words,
            RingDirection::Destination => (self.ring_words - self.tail + hw) / self.entry_words,
        }
    }
    /// Refreshes the hardware-owned pointer. `read_u32` has acquire ordering,
    /// matching Linux's READ_ONCE followed by dma_rmb.
    pub fn access_begin(&mut self, mmio: &MmioRegion<B>) -> Result<(), HalError> {
        self.cached_hardware_pointer = mmio
            .read_u32(if self.direction == RingDirection::Source {
                self.r2 + 4
            } else {
                self.r2
            })
            .map_err(|_| HalError::DeviceFault)?;
        Ok(())
    }
    /// Source-faithful pointer refresh from HAL's coherent remote-pointer
    /// array. The acquire fence maps the destination-ring `dma_rmb()`.
    pub fn access_begin_remote(
        &mut self,
        remote_read_pointers: &mut CoherentDma<B, Bidirectional>,
    ) -> Result<(), HalError> {
        let mut bytes = [0; 4];
        remote_read_pointers
            .read(self.pointer_offset, &mut bytes)
            .map_err(|_| HalError::DeviceFault)?;
        self.cached_hardware_pointer = u32::from_le_bytes(bytes);
        if self.direction == RingDirection::Destination {
            fence(Ordering::Acquire);
        }
        Ok(())
    }
    /// Publishes the software-owned pointer. The ordered MMIO write is release,
    /// matching Linux's dma_wmb/mb before its head/tail write.
    pub fn access_end(&self, mmio: &MmioRegion<B>) -> Result<(), HalError> {
        let value = if self.direction == RingDirection::Source {
            self.head
        } else {
            self.tail
        };
        w(mmio, self.publication_offset, value)
    }
    /// Select a firmware-programmed shadow register for subsequent pointer
    /// publications (`ath11k_hal_srng_update_hp_tp_addr`).
    pub fn set_shadow_publication_register(&mut self, offset: usize) {
        self.publication_offset = offset;
    }
    /// LMAC rings publish through the coherent WRP array instead of MMIO.
    /// The fence preserves Linux's dma_wmb/dma_mb before the shared write.
    pub fn access_end_lmac(
        &self,
        remote_write_pointers: &mut CoherentDma<B, Bidirectional>,
    ) -> Result<(), HalError> {
        if self.direction == RingDirection::Source {
            fence(Ordering::Release);
        } else {
            fence(Ordering::SeqCst);
        }
        let value = if self.direction == RingDirection::Source {
            self.head
        } else {
            self.tail
        };
        remote_write_pointers
            .write(self.firmware_pointer_offset, &value.to_le_bytes())
            .map_err(|_| HalError::DeviceFault)
    }

    /// Quiesce host-owned SRNG state before its coherent ring memory is
    /// released. The caller must already have stopped firmware/interrupt
    /// dispatch, matching `ath11k_dp_srng_cleanup`'s lifecycle ordering.
    pub fn teardown(
        &mut self,
        mmio: &MmioRegion<B>,
        remote_read_pointers: &mut CoherentDma<B, Bidirectional>,
        remote_write_pointers: &mut CoherentDma<B, Bidirectional>,
    ) -> Result<(), HalError> {
        if config(self.ring_type).lmac {
            remote_read_pointers
                .write(self.pointer_offset, &0_u32.to_le_bytes())
                .map_err(|_| HalError::DeviceFault)?;
            remote_write_pointers
                .write(self.firmware_pointer_offset, &0_u32.to_le_bytes())
                .map_err(|_| HalError::DeviceFault)
        } else {
            w(mmio, self.r0 + 0x10, 0)
        }
    }
}

fn w<B: Backend>(m: &MmioRegion<B>, o: usize, v: u32) -> Result<(), HalError> {
    m.write_u32(o, v).map_err(|_| HalError::DeviceFault)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{rc::Rc, vec::Vec};
    use ath11k_platform_backend::{Backend, Device, DmaDirection, Error, IrqEvent, Result};
    use core::{cell::RefCell, ops::Range};
    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Op {
        Write(usize, u32),
        Address(usize, Option<usize>, u64),
    }
    #[derive(Default)]
    struct Fake {
        ops: Rc<RefCell<Vec<Op>>>,
        mem: alloc::collections::BTreeMap<usize, u32>,
        next: u64,
    }
    impl Backend for Fake {
        type Region = ();
        type Dma = u64;
        type Interrupt = ();
        fn generation(&self) -> u64 {
            1
        }
        fn open_region(&mut self, i: u8) -> Result<()> {
            if i == 0 { Ok(()) } else { Err(Error::Invalid) }
        }
        fn region_len(&self, _: &()) -> usize {
            0x0200_0000
        }
        fn read_u32(&mut self, _: &(), o: usize) -> Result<u32> {
            Ok(*self.mem.get(&o).unwrap_or(&0))
        }
        fn write_u32(&mut self, _: &(), o: usize, v: u32) -> Result<()> {
            self.mem.insert(o, v);
            self.ops.borrow_mut().push(Op::Write(o, v));
            Ok(())
        }
        fn write_dma_address(
            &mut self,
            _: &(),
            l: usize,
            h: Option<usize>,
            d: &u64,
            o: usize,
        ) -> Result<()> {
            let a = *d + o as u64;
            self.mem.insert(l, a as u32);
            if let Some(x) = h {
                self.mem.insert(x, (a >> 32) as u32);
            }
            self.ops.borrow_mut().push(Op::Address(l, h, a));
            Ok(())
        }
        fn dma_device_address(&self, dma: &u64, offset: usize) -> Result<u64> {
            dma.checked_add(offset as u64).ok_or(Error::OutOfBounds)
        }
        fn alloc_dma(&mut self, s: usize, _: usize, _: DmaDirection, _: bool) -> Result<u64> {
            self.next = (self.next.max(0x1000_0000) + 0xfff) & !0xfff;
            let x = self.next;
            self.next += s as u64;
            Ok(x)
        }
        fn dma_read(&mut self, _: &u64, _: Range<usize>, o: &mut [u8]) -> Result<()> {
            o.fill(0);
            Ok(())
        }
        fn dma_write(&mut self, _: &u64, _: Range<usize>, _: &[u8]) -> Result<()> {
            Ok(())
        }
        fn sync_for_cpu(&mut self, _: &u64, _: Range<usize>) -> Result<()> {
            Ok(())
        }
        fn sync_for_device(&mut self, _: &u64, _: Range<usize>) -> Result<()> {
            Ok(())
        }
        fn open_interrupt(&mut self, _: u32) -> Result<()> {
            Ok(())
        }
        fn wait_interrupt(&mut self, _: &(), _: u64) -> Result<Option<IrqEvent>> {
            Ok(None)
        }
        fn wait_any(&mut self, interrupts: &[&()], _: u64) -> Result<Vec<IrqEvent>> {
            if interrupts.is_empty() {
                Err(Error::Invalid)
            } else {
                Ok(Vec::new())
            }
        }
        fn reset(&mut self) -> Result<u64> {
            Ok(1)
        }
        fn release_region(&mut self, _: ()) {}
        fn release_dma(&mut self, _: u64) {}
        fn release_interrupt(&mut self, _: ()) {}
    }
    #[test]
    fn wcn6750_table_matches_source() {
        assert_eq!(Wcn6750Registers::entry_size(RingType::TclData), 28);
        assert_eq!(
            Wcn6750Registers::ring_id(RingType::CeDestinationStatus, 11, 0),
            Some(RingId(91))
        );
        assert_eq!(
            Wcn6750Registers::ring_id(RingType::CeDestinationStatus, 12, 0),
            None
        );
        assert_eq!(
            Wcn6750Registers::ring_id(RingType::RxdmaBuffer, 0, 1),
            Some(RingId(143))
        );
        assert_eq!(
            Wcn6750Registers::max_entries(RingType::CeSource),
            0xffff / 4
        );
    }
    #[test]
    fn source_setup_write_order_matches_hal_c() {
        let ops = Rc::new(RefCell::new(Vec::new()));
        let d = Device::from_backend(Fake {
            ops: ops.clone(),
            ..Fake::default()
        });
        let mmio = d.open_region(0).unwrap();
        let mem = RingMemory {
            dma: d.alloc_coherent(28 * 8, 8).unwrap(),
            entries: 8,
            entry_bytes: 28,
        };
        let rdp = d.alloc_coherent(176 * 4, 4).unwrap();
        let _ = Srng::setup(
            &mmio,
            RingType::TclData,
            0,
            0,
            mem,
            &rdp,
            SrngParams {
                interrupt_batch_entries: 2,
                interrupt_timer_us: 16,
                ..SrngParams::default()
            },
        )
        .unwrap();
        let o = ops.borrow();
        assert_eq!(
            o[0],
            Op::Address(UMAC_TCL + 0x694, Some(UMAC_TCL + 0x698), 0x1000_0000)
        );
        assert_eq!(o[1], Op::Write(UMAC_TCL + 0x698, 56 << 8));
        assert_eq!(o[2], Op::Write(UMAC_TCL + 0x69c, 7));
        assert_eq!(
            o[o.len() - 1],
            Op::Write(UMAC_TCL + 0x6a4, RING_ENABLE | SRC_LOOP_COUNT_DISABLE)
        );
    }
    #[test]
    fn destination_setup_write_order_matches_hal_c() {
        let ops = Rc::new(RefCell::new(Vec::new()));
        let d = Device::from_backend(Fake {
            ops: ops.clone(),
            ..Fake::default()
        });
        let mmio = d.open_region(0).unwrap();
        let mem = RingMemory {
            dma: d.alloc_coherent(64 * 4, 8).unwrap(),
            entries: 4,
            entry_bytes: 64,
        };
        let rdp = d.alloc_coherent(176 * 4, 4).unwrap();
        let _ = Srng::setup(
            &mmio,
            RingType::ReoDestination,
            0,
            0,
            mem,
            &rdp,
            SrngParams {
                interrupt_batch_entries: 2,
                interrupt_timer_us: 16,
                ..SrngParams::default()
            },
        )
        .unwrap();
        let o = ops.borrow();
        assert_eq!(
            o[0],
            Op::Address(UMAC_REO + 0x1ec, Some(UMAC_REO + 0x1f0), 0x1000_0000)
        );
        assert_eq!(o[1], Op::Write(UMAC_REO + 0x1f0, 64 << 8));
        assert_eq!(o[2], Op::Write(UMAC_REO + 0x1f4, 16));
        assert_eq!(o[3], Op::Write(UMAC_REO + 0x210, (2 << 16) | 32));
        assert_eq!(o[o.len() - 1], Op::Write(UMAC_REO + 0x1fc, RING_ENABLE));
    }
    #[test]
    fn ring_arithmetic_reserves_one_source_entry() {
        let d = Device::from_backend(Fake::default());
        let m = d.open_region(0).unwrap();
        let mem = RingMemory {
            dma: d.alloc_coherent(16 * 4, 8).unwrap(),
            entries: 4,
            entry_bytes: 16,
        };
        let r = d.alloc_coherent(176 * 4, 4).unwrap();
        let mut s =
            Srng::setup(&m, RingType::CeSource, 0, 0, mem, &r, SrngParams::default()).unwrap();
        assert_eq!(s.number_free(), 3);
        assert_eq!(s.source_next(), Some(0));
        assert_eq!(s.source_next(), Some(16));
        assert_eq!(s.source_next(), Some(32));
        assert_eq!(s.source_next(), None);
        s.cached_hardware_pointer = s.entry_words;
        s.reap_head = s.ring_words - s.entry_words;
        s.head = s.entry_words * 2;
        assert_eq!(s.source_reap_next(), Some(0));
        assert_eq!(s.source_next_reaped(), Some(32));
    }
}
