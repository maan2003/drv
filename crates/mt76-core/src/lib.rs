#![no_std]
#![forbid(unsafe_code)]

//! Linux v7.1.5 mt76-shared hardware primitives.
//!
//! Source map:
//! - `drivers/net/wireless/mediatek/mt76/dma.h`: `struct mt76_desc`.
//! - `drivers/net/wireless/mediatek/mt76/dma.c`: descriptor encoding,
//!   queue allocation, producer publication, completion, and teardown.
//!
//! Chip register maps, firmware policy, NIC/channel state, and device
//! sequencing deliberately remain in the consuming device crate.

extern crate alloc;

use alloc::{vec, vec::Vec};

/// Size of `struct mt76_desc` from Linux `mt76/dma.h`.
pub const DMA_DESCRIPTOR_LEN: usize = 16;
const DMA_MAX_SEGMENT_LEN: u16 = 0x3fff;
const DMA_CTL_LAST_SEC1: u32 = 1 << 14;
const DMA_CTL_SD_LEN0_SHIFT: u32 = 16;
const DMA_CTL_LAST_SEC0: u32 = 1 << 30;
const DMA_CTL_DMA_DONE: u32 = 1 << 31;

/// A buffer segment representable by the MT7921 PCI DMA setup.
///
/// Linux selects a 32-bit DMA mask in `mt7921_pci_probe`, so this spike rejects
/// IOVAs and lengths which the descriptor would otherwise silently truncate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DmaSegment {
    pub iova: u64,
    pub len: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorError {
    IovaAbove32Bits,
    SegmentTooLong,
    InvalidArena,
}

/// The four little-endian words of `struct mt76_desc`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DmaDescriptor {
    pub buf0: u32,
    pub ctrl: u32,
    pub buf1: u32,
    pub info: u32,
}

impl DmaDescriptor {
    /// Encode one or two TX segments as `mt76_dma_add_buf` does.
    ///
    /// `info` is the caller-owned TX metadata word. Token allocation, ownership
    /// publication, ring indices, and cache synchronization are intentionally
    /// outside this format-only function.
    pub fn tx(
        first: DmaSegment,
        second: Option<DmaSegment>,
        info: u32,
    ) -> Result<Self, DescriptorError> {
        validate_segment(first)?;
        if let Some(segment) = second {
            validate_segment(segment)?;
        }

        let mut ctrl = u32::from(first.len) << DMA_CTL_SD_LEN0_SHIFT;
        let buf1 = if let Some(segment) = second {
            ctrl |= u32::from(segment.len) | DMA_CTL_LAST_SEC1;
            segment.iova as u32
        } else {
            ctrl |= DMA_CTL_LAST_SEC0;
            0
        };

        Ok(Self {
            buf0: first.iova as u32,
            ctrl,
            buf1,
            info,
        })
    }

    /// Encode one device-owned RX buffer as `mt76_dma_add_rx_buf` does.
    pub fn rx(buffer: DmaSegment) -> Result<Self, DescriptorError> {
        validate_segment(buffer)?;
        Ok(Self {
            buf0: buffer.iova as u32,
            ctrl: u32::from(buffer.len) << DMA_CTL_SD_LEN0_SHIFT,
            buf1: 0,
            info: 0,
        })
    }

    /// Descriptor state used by `mt76_dma_queue_reset` before device ownership.
    pub const fn reset() -> Self {
        Self {
            buf0: 0,
            ctrl: DMA_CTL_DMA_DONE,
            buf1: 0,
            info: 0,
        }
    }

    pub const fn to_le_bytes(self) -> [u8; DMA_DESCRIPTOR_LEN] {
        let mut bytes = [0; DMA_DESCRIPTOR_LEN];
        let buf0 = self.buf0.to_le_bytes();
        let ctrl = self.ctrl.to_le_bytes();
        let buf1 = self.buf1.to_le_bytes();
        let info = self.info.to_le_bytes();
        bytes[0] = buf0[0];
        bytes[1] = buf0[1];
        bytes[2] = buf0[2];
        bytes[3] = buf0[3];
        bytes[4] = ctrl[0];
        bytes[5] = ctrl[1];
        bytes[6] = ctrl[2];
        bytes[7] = ctrl[3];
        bytes[8] = buf1[0];
        bytes[9] = buf1[1];
        bytes[10] = buf1[2];
        bytes[11] = buf1[3];
        bytes[12] = info[0];
        bytes[13] = info[1];
        bytes[14] = info[2];
        bytes[15] = info[3];
        bytes
    }

    pub const fn is_dma_done(self) -> bool {
        self.ctrl & DMA_CTL_DMA_DONE != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RingAllocation {
    pub id: u64,
    pub iova: u64,
    pub len: usize,
}

pub trait Low32RingMemory {
    type Error;
    fn allocate_low32(&mut self, size: usize, align: usize) -> Result<RingAllocation, Self::Error>;
    fn free(&mut self, allocation: RingAllocation);
}

pub trait RingPublisher {
    type Error;
    fn write_descriptor(
        &mut self,
        index: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error>;
    fn release_fence(&mut self);
    fn publish_producer(&mut self, index: u16) -> Result<(), Self::Error>;
    fn acquire_fence(&mut self);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RingError<E> {
    InvalidCount,
    Allocation(E),
    AllocationTooSmall,
    AllocationAbove32Bits,
    Full,
    Descriptor(DescriptorError),
    Publish(E),
}

/// Inactive MT7921 WFDMA TX ring model ported from pinned Linux mt76 `dma.c`.
///
/// Allocation is constrained to addresses representable by the MT7921 PCI
/// 32-bit DMA mask. Descriptors start CPU-owned (`DMA_DONE`), enqueue clears
/// that bit, and producer publication follows a release fence like
/// `mt76_dma_kick_queue`. Reclaim advances only from the consumer tail after a
/// device completion and acquire fence. This type has no MMIO enable method.
pub struct WfdmaRing {
    allocation: RingAllocation,
    descriptors: Vec<DmaDescriptor>,
    producer: u16,
    consumer: u16,
    queued: u16,
}

impl WfdmaRing {
    pub fn allocate<M: Low32RingMemory>(
        memory: &mut M,
        count: u16,
    ) -> Result<Self, RingError<M::Error>> {
        if count < 2 {
            return Err(RingError::InvalidCount);
        }
        let size = usize::from(count)
            .checked_mul(DMA_DESCRIPTOR_LEN)
            .ok_or(RingError::InvalidCount)?;
        let allocation = memory
            .allocate_low32(size, DMA_DESCRIPTOR_LEN)
            .map_err(RingError::Allocation)?;
        if allocation.len < size {
            memory.free(allocation);
            return Err(RingError::AllocationTooSmall);
        }
        let end = allocation
            .iova
            .checked_add(size as u64 - 1)
            .filter(|end| *end <= u64::from(u32::MAX));
        if allocation.iova % DMA_DESCRIPTOR_LEN as u64 != 0 || end.is_none() {
            memory.free(allocation);
            return Err(RingError::AllocationAbove32Bits);
        }
        Ok(Self {
            allocation,
            descriptors: vec![DmaDescriptor::reset(); usize::from(count)],
            producer: 0,
            consumer: 0,
            queued: 0,
        })
    }

    pub const fn allocation(&self) -> RingAllocation {
        self.allocation
    }
    pub const fn producer(&self) -> u16 {
        self.producer
    }
    pub const fn consumer(&self) -> u16 {
        self.consumer
    }
    pub const fn queued(&self) -> u16 {
        self.queued
    }

    pub fn enqueue<P: RingPublisher>(
        &mut self,
        publisher: &mut P,
        first: DmaSegment,
        second: Option<DmaSegment>,
        info: u32,
    ) -> Result<u16, RingError<P::Error>> {
        if usize::from(self.queued) == self.descriptors.len() {
            return Err(RingError::Full);
        }
        let index = self.producer;
        let descriptor = DmaDescriptor::tx(first, second, info).map_err(RingError::Descriptor)?;
        publisher
            .write_descriptor(index, descriptor)
            .map_err(RingError::Publish)?;
        publisher.release_fence();
        let next = (usize::from(index) + 1) % self.descriptors.len();
        publisher
            .publish_producer(next as u16)
            .map_err(RingError::Publish)?;
        self.descriptors[usize::from(index)] = descriptor;
        self.producer = next as u16;
        self.queued += 1;
        Ok(index)
    }

    /// Model the device's DMA_DONE write for deterministic tests/backends.
    pub fn complete(&mut self, index: u16) -> bool {
        let Some(descriptor) = self.descriptors.get_mut(usize::from(index)) else {
            return false;
        };
        descriptor.ctrl |= DMA_CTL_DMA_DONE;
        true
    }

    pub fn reclaim_one<P: RingPublisher>(&mut self, publisher: &mut P) -> Option<u16> {
        if self.queued == 0 {
            return None;
        }
        let index = self.consumer;
        if !self.descriptors[usize::from(index)].is_dma_done() {
            return None;
        }
        publisher.acquire_fence();
        self.descriptors[usize::from(index)] = DmaDescriptor::reset();
        self.consumer = ((usize::from(index) + 1) % self.descriptors.len()) as u16;
        self.queued -= 1;
        Some(index)
    }

    pub fn teardown<M: Low32RingMemory>(mut self, memory: &mut M) {
        self.descriptors.fill(DmaDescriptor::reset());
        self.queued = 0;
        memory.free(self.allocation);
    }
}

fn validate_segment(segment: DmaSegment) -> Result<(), DescriptorError> {
    if segment.iova > u64::from(u32::MAX) {
        return Err(DescriptorError::IovaAbove32Bits);
    }
    if segment.len > DMA_MAX_SEGMENT_LEN {
        return Err(DescriptorError::SegmentTooLong);
    }
    Ok(())
}
