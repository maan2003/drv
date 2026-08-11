#![no_std]
#![forbid(unsafe_code)]

//! Linux v7.1.5 mt76-shared hardware primitives.
//!
//! Source map:
//! - `drivers/net/wireless/mediatek/mt76/dma.h`: `struct mt76_desc`.
//! - `drivers/net/wireless/mediatek/mt76/dma.c`: descriptor encoding,
//!   queue allocation, producer publication, completion, and teardown.
//! - `drivers/net/wireless/mediatek/mt76/pci.c`: PCIe ASPM capability policy.
//! - `drivers/net/wireless/mediatek/mt76/mt76_connac_mcu.c` and
//!   `mt76_connac_mcu.h`: Connac firmware/patch image formats and common
//!   download-mode bit derivation.
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

pub const FW_TRAILER_LEN: usize = 36;
pub const FW_REGION_LEN: usize = 40;
pub const FW_FEATURE_NON_DL: u8 = 1 << 6;
pub const FW_TYPE_CLC: u8 = 2;
pub const PATCH_HEADER_LEN: usize = 96;
pub const PATCH_SECTION_LEN: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareError {
    MissingTrailer,
    RegionTableTooLarge,
    PayloadLengthOverflow,
    PayloadOverlapsMetadata,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatchError {
    MissingHeader,
    RegionTableTooLarge,
    UnsupportedSectionType,
    PayloadOutOfBounds,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PatchHeader<'a> {
    pub build_date: &'a [u8; 16],
    pub platform: &'a [u8; 4],
    pub hardware_software_version: u32,
    pub patch_version: u32,
    pub checksum: u16,
    pub descriptor_patch_version: u32,
    pub subsystem: u32,
    pub feature: u32,
    pub crc: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PatchSection<'a> {
    pub address: u32,
    pub security_info: u32,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Patch<'a> {
    bytes: &'a [u8],
    region_count: u32,
    pub header: PatchHeader<'a>,
}

impl<'a> Patch<'a> {
    /// Parse `mt76_connac2_patch_hdr` and `mt76_connac2_patch_sec` exactly as
    /// pinned Linux `mt76_connac2_load_patch` consumes their big-endian fields.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, PatchError> {
        let header = bytes
            .get(..PATCH_HEADER_LEN)
            .ok_or(PatchError::MissingHeader)?;
        let region_count = be_u32(&header[44..48]);
        let table_len = (region_count as usize)
            .checked_mul(PATCH_SECTION_LEN)
            .and_then(|length| PATCH_HEADER_LEN.checked_add(length))
            .ok_or(PatchError::RegionTableTooLarge)?;
        if table_len > bytes.len() {
            return Err(PatchError::RegionTableTooLarge);
        }
        for index in 0..region_count as usize {
            let start = PATCH_HEADER_LEN + index * PATCH_SECTION_LEN;
            let section = &bytes[start..start + PATCH_SECTION_LEN];
            if be_u32(&section[0..4]) & 0xffff != 2 {
                return Err(PatchError::UnsupportedSectionType);
            }
            let offset = be_u32(&section[4..8]) as usize;
            let length = be_u32(&section[16..20]) as usize;
            let end = offset
                .checked_add(length)
                .ok_or(PatchError::PayloadOutOfBounds)?;
            if offset < table_len || end > bytes.len() {
                return Err(PatchError::PayloadOutOfBounds);
            }
        }
        Ok(Self {
            bytes,
            region_count,
            header: PatchHeader {
                build_date: header[0..16].try_into().expect("fixed field"),
                platform: header[16..20].try_into().expect("fixed field"),
                hardware_software_version: be_u32(&header[20..24]),
                patch_version: be_u32(&header[24..28]),
                checksum: u16::from_be_bytes(header[28..30].try_into().expect("fixed field")),
                descriptor_patch_version: be_u32(&header[32..36]),
                subsystem: be_u32(&header[36..40]),
                feature: be_u32(&header[40..44]),
                crc: be_u32(&header[48..52]),
            },
        })
    }

    pub const fn region_count(&self) -> u32 {
        self.region_count
    }

    pub fn sections(&self) -> PatchSections<'a> {
        PatchSections {
            patch: *self,
            index: 0,
        }
    }
}

pub struct PatchSections<'a> {
    patch: Patch<'a>,
    index: usize,
}
impl<'a> Iterator for PatchSections<'a> {
    type Item = PatchSection<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.index == self.patch.region_count as usize {
            return None;
        }
        let start = PATCH_HEADER_LEN + self.index * PATCH_SECTION_LEN;
        let section = &self.patch.bytes[start..start + PATCH_SECTION_LEN];
        self.index += 1;
        let offset = be_u32(&section[4..8]) as usize;
        let length = be_u32(&section[16..20]) as usize;
        Some(PatchSection {
            address: be_u32(&section[12..16]),
            security_info: be_u32(&section[20..24]),
            payload: &self.patch.bytes[offset..offset + length],
        })
    }
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().expect("four-byte field"))
}

/// Parsed `struct mt76_connac2_fw_trailer` fields used by the Linux loader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareTrailer<'a> {
    pub chip_id: u8,
    pub eco_code: u8,
    pub format_version: u8,
    pub format_flag: u8,
    pub firmware_version: &'a [u8; 10],
    pub build_date: &'a [u8; 15],
    pub crc: u32,
}

/// One Connac2 firmware payload and its corresponding region metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareRegion<'a> {
    pub address: u32,
    pub feature_set: u8,
    pub region_type: u8,
    pub payload: &'a [u8],
}

impl FirmwareRegion<'_> {
    pub const fn is_downloadable(&self) -> bool {
        self.feature_set & FW_FEATURE_NON_DL == 0
    }

    pub const fn is_clc(&self) -> bool {
        self.feature_set & FW_FEATURE_NON_DL != 0 && self.region_type == FW_TYPE_CLC
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Firmware<'a> {
    bytes: &'a [u8],
    metadata_start: usize,
    region_count: u8,
    pub trailer: FirmwareTrailer<'a>,
}

impl<'a> Firmware<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FirmwareError> {
        let trailer_start = bytes
            .len()
            .checked_sub(FW_TRAILER_LEN)
            .ok_or(FirmwareError::MissingTrailer)?;
        let trailer = &bytes[trailer_start..];
        let region_count = trailer[2];
        let table_len = usize::from(region_count)
            .checked_mul(FW_REGION_LEN)
            .ok_or(FirmwareError::RegionTableTooLarge)?;
        let metadata_start = trailer_start
            .checked_sub(table_len)
            .ok_or(FirmwareError::RegionTableTooLarge)?;

        let firmware_version = trailer[7..17].try_into().expect("fixed slice length");
        let build_date = trailer[17..32].try_into().expect("fixed slice length");
        let parsed = Self {
            bytes,
            metadata_start,
            region_count,
            trailer: FirmwareTrailer {
                chip_id: trailer[0],
                eco_code: trailer[1],
                format_version: trailer[3],
                format_flag: trailer[4],
                firmware_version,
                build_date,
                crc: le_u32(&trailer[32..36]),
            },
        };

        // Validate all lengths up front so iteration cannot partially accept a
        // malformed image before discovering that payload overlaps metadata.
        let mut payload_end = 0usize;
        for index in 0..usize::from(region_count) {
            let record = parsed.region_record(index);
            payload_end = payload_end
                .checked_add(le_u32(&record[20..24]) as usize)
                .ok_or(FirmwareError::PayloadLengthOverflow)?;
            if payload_end > metadata_start {
                return Err(FirmwareError::PayloadOverlapsMetadata);
            }
        }

        Ok(parsed)
    }

    pub const fn region_count(&self) -> u8 {
        self.region_count
    }

    pub fn regions(&self) -> FirmwareRegions<'a> {
        FirmwareRegions {
            firmware: *self,
            index: 0,
            payload_offset: 0,
        }
    }

    fn region_record(&self, index: usize) -> &'a [u8] {
        let start = self.metadata_start + index * FW_REGION_LEN;
        &self.bytes[start..start + FW_REGION_LEN]
    }
}

pub struct FirmwareRegions<'a> {
    firmware: Firmware<'a>,
    index: usize,
    payload_offset: usize,
}

impl<'a> Iterator for FirmwareRegions<'a> {
    type Item = FirmwareRegion<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index == usize::from(self.firmware.region_count) {
            return None;
        }
        let record = self.firmware.region_record(self.index);
        let len = le_u32(&record[20..24]) as usize;
        let payload_start = self.payload_offset;
        self.payload_offset += len;
        self.index += 1;
        Some(FirmwareRegion {
            address: le_u32(&record[16..20]),
            feature_set: record[24],
            region_type: record[25],
            payload: &self.firmware.bytes[payload_start..self.payload_offset],
        })
    }
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("four-byte field"))
}

#[doc(hidden)]
pub const PCI_CAPABILITY_LIST: usize = 0x34;
#[doc(hidden)]
pub const PCI_STATUS: usize = 0x06;
#[doc(hidden)]
pub const PCI_STATUS_CAP_LIST: u16 = 1 << 4;
#[doc(hidden)]
pub const PCI_CAP_ID_EXP: u8 = 0x10;
#[doc(hidden)]
pub const PCI_EXP_LNKCTL: usize = 0x10;
const PCI_EXP_LNKCTL_ASPMC: u16 = 0x3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PcieLinkControlError {
    ConfigTooShort,
    InvalidCapabilityOffset(u8),
    CapabilityLoop(u8),
    TruncatedPcieCapability(u8),
    PcieCapabilityAbsent,
}

/// Parse the standard PCIe capability's Link Control word.
///
/// Pinned Linux `pcie_capability_read_word(..., PCI_EXP_LNKCTL, ...)` first
/// locates conventional capability ID `PCI_CAP_ID_EXP`, then reads the
/// little-endian word at capability offset `PCI_EXP_LNKCTL`.
pub fn pcie_link_control(config: &[u8]) -> Result<u16, PcieLinkControlError> {
    if config.len() <= PCI_CAPABILITY_LIST {
        return Err(PcieLinkControlError::ConfigTooShort);
    }
    let status = u16::from_le_bytes([config[PCI_STATUS], config[PCI_STATUS + 1]]);
    if status & PCI_STATUS_CAP_LIST == 0 {
        return Err(PcieLinkControlError::PcieCapabilityAbsent);
    }
    let mut offset = config[PCI_CAPABILITY_LIST] & !3;
    if offset == 0 {
        return Err(PcieLinkControlError::PcieCapabilityAbsent);
    }
    let mut visited = [false; 64];
    for _ in 0..48 {
        let index = usize::from(offset);
        if index < 0x40 || index + 2 > config.len() {
            return Err(PcieLinkControlError::InvalidCapabilityOffset(offset));
        }
        let slot = index / 4;
        if visited[slot] {
            return Err(PcieLinkControlError::CapabilityLoop(offset));
        }
        visited[slot] = true;
        if config[index] == PCI_CAP_ID_EXP {
            let link_control = index + PCI_EXP_LNKCTL;
            if link_control + 2 > config.len() {
                return Err(PcieLinkControlError::TruncatedPcieCapability(offset));
            }
            return Ok(u16::from_le_bytes([
                config[link_control],
                config[link_control + 1],
            ]));
        }
        offset = config[index + 1] & !3;
        if offset == 0 {
            return Err(PcieLinkControlError::PcieCapabilityAbsent);
        }
    }
    Err(PcieLinkControlError::CapabilityLoop(offset))
}

/// Reproduce pinned Linux `mt76_pci_aspm_supported`: ASPM is supported when
/// either the endpoint or its optional parent bridge has L0s/L1 enabled in
/// Link Control. Parsing errors are retained rather than guessed as false.
pub fn mt76_pci_aspm_supported(
    endpoint_config: &[u8],
    parent_config: Option<&[u8]>,
) -> Result<bool, PcieLinkControlError> {
    let endpoint = pcie_link_control(endpoint_config)? & PCI_EXP_LNKCTL_ASPMC;
    let parent = match parent_config {
        Some(config) => pcie_link_control(config)? & PCI_EXP_LNKCTL_ASPMC,
        None => 0,
    };
    Ok(endpoint != 0 || parent != 0)
}

pub const CONNAC2_MCU_TXD_BYTES: usize = 64;
pub const PATCH_START_REQUEST_BYTES: usize = CONNAC2_MCU_TXD_BYTES + 12;
pub const PATCH_SEMAPHORE_REQUEST_BYTES: usize = CONNAC2_MCU_TXD_BYTES + 4;
pub const PATCH_FINISH_REQUEST_BYTES: usize = CONNAC2_MCU_TXD_BYTES + 4;
pub const FIRMWARE_START_REQUEST_BYTES: usize = CONNAC2_MCU_TXD_BYTES + 8;
pub const DL_MODE_ENCRYPT: u32 = 1 << 0;
pub const DL_MODE_KEY_INDEX: u32 = 0b11 << 1;
pub const DL_MODE_RESET_SECURITY_IV: u32 = 1 << 3;
pub const DL_MODE_WORKING_PDA_CR4: u32 = 1 << 4;
pub const DL_MODE_ENCRYPTION_MODE_SELECT: u32 = 1 << 6;
pub const DL_MODE_NEED_RESPONSE: u32 = 1 << 31;

/// Translate a Connac2 RAM region feature byte into Linux's download mode.
/// Address override and non-download are caller-side region controls and do
/// not contribute mode bits.
pub const fn firmware_download_mode(feature_set: u8, working_pda_cr4: bool) -> u32 {
    let mut mode = DL_MODE_NEED_RESPONSE | ((feature_set as u32) & DL_MODE_KEY_INDEX);
    if feature_set & (1 << 0) != 0 {
        mode |= DL_MODE_ENCRYPT | DL_MODE_RESET_SECURITY_IV;
    }
    if feature_set & (1 << 4) != 0 {
        mode |= DL_MODE_ENCRYPTION_MODE_SELECT;
    }
    if working_pda_cr4 {
        mode |= DL_MODE_WORKING_PDA_CR4;
    }
    mode
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatchSecurityError {
    UnsupportedEncryptionType(u8),
}

/// Translate a Connac2 patch section's security word into Linux's download
/// mode. Unknown encryption types fail closed instead of merely being logged.
pub fn patch_download_mode(security_info: u32) -> Result<u32, PatchSecurityError> {
    let mut mode = DL_MODE_NEED_RESPONSE;
    if security_info == u32::MAX {
        return Ok(mode);
    }
    match (security_info >> 24) as u8 {
        0 => {}
        1 => {
            mode |= DL_MODE_ENCRYPT
                | ((security_info << 1) & DL_MODE_KEY_INDEX)
                | DL_MODE_RESET_SECURITY_IV;
        }
        2 => {
            mode |= DL_MODE_ENCRYPT | DL_MODE_ENCRYPTION_MODE_SELECT | DL_MODE_RESET_SECURITY_IV;
        }
        encryption_type => {
            return Err(PatchSecurityError::UnsupportedEncryptionType(
                encryption_type,
            ));
        }
    }
    Ok(mode)
}
