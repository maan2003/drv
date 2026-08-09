#![no_std]

//! Hardware-independent pieces of the Linux mt76 Connac2 data formats.
//!
//! This is an exploratory port, not an MT7921/MT7922 device driver. The source
//! correspondence and the boundary deliberately left out are documented in
//! the crate README.

extern crate alloc;

use alloc::{boxed::Box, vec, vec::Vec};

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClcDiscovery {
    pub segment_count: u16,
    pub selected_power_segments: u16,
    pub selected_power_rules: u16,
    pub channel_segments: u16,
    pub channel_rules: u16,
    pub unique_country_codes: u16,
    pub world_domain_available: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClcDiscoveryError {
    TruncatedSegmentHeader,
    InvalidSegmentLength(u32),
    UnsupportedSegmentIndex(u8),
    TruncatedRule { segment: u16 },
    CountOverflow,
}

/// Inventory the local CLC region exactly up to (but not including) Linux's
/// mutating `SET_CLC` call. Power segment selection uses the EEPROM hardware
/// encapsulation bit; channel rules remain opaque firmware input.
pub fn discover_clc(
    firmware: Firmware<'_>,
    hardware: EepromHardwareInfo,
) -> Result<ClcDiscovery, ClcDiscoveryError> {
    let Some(region) = firmware.regions().find(FirmwareRegion::is_clc) else {
        return Ok(ClcDiscovery::default());
    };
    let mut discovery = ClcDiscovery::default();
    let mut countries = Vec::<[u8; 2]>::new();
    let mut accepted = [false; 2];
    let mut offset = 0usize;
    while offset < region.payload.len() {
        let header = region
            .payload
            .get(offset..offset + 16)
            .ok_or(ClcDiscoveryError::TruncatedSegmentHeader)?;
        let length = u32::from_le_bytes(header[0..4].try_into().expect("fixed field"));
        let length_usize = length as usize;
        if length_usize < 16
            || offset
                .checked_add(length_usize)
                .is_none_or(|end| end > region.payload.len())
        {
            return Err(ClcDiscoveryError::InvalidSegmentLength(length));
        }
        let index = header[4];
        if index > 1 {
            return Err(ClcDiscoveryError::UnsupportedSegmentIndex(index));
        }
        discovery.segment_count = discovery
            .segment_count
            .checked_add(1)
            .ok_or(ClcDiscoveryError::CountOverflow)?;
        let selected = !accepted[index as usize]
            && (index == 1 || ((header[7] & 1 != 0) == hardware.encapsulated_calibration));
        if selected {
            accepted[index as usize] = true;
        }
        if index == 0 && selected {
            discovery.selected_power_segments = discovery
                .selected_power_segments
                .checked_add(1)
                .ok_or(ClcDiscoveryError::CountOverflow)?;
        } else if index == 1 {
            discovery.channel_segments = discovery
                .channel_segments
                .checked_add(1)
                .ok_or(ClcDiscoveryError::CountOverflow)?;
        }
        let end = offset + length_usize;
        let mut rule_offset = offset + 16;
        // Pinned Linux stops when no more than 16 bytes remain in a segment.
        while end - rule_offset > 16 {
            let rule = region.payload.get(rule_offset..rule_offset + 6).ok_or(
                ClcDiscoveryError::TruncatedRule {
                    segment: discovery.segment_count - 1,
                },
            )?;
            let data_length = u16::from_le_bytes([rule[4], rule[5]]) as usize;
            let rule_length = 6usize
                .checked_add(data_length)
                .ok_or(ClcDiscoveryError::CountOverflow)?;
            if rule_offset
                .checked_add(rule_length)
                .is_none_or(|rule_end| rule_end > end)
            {
                return Err(ClcDiscoveryError::TruncatedRule {
                    segment: discovery.segment_count - 1,
                });
            }
            if selected {
                if index == 0 {
                    discovery.selected_power_rules = discovery
                        .selected_power_rules
                        .checked_add(1)
                        .ok_or(ClcDiscoveryError::CountOverflow)?;
                } else {
                    discovery.channel_rules = discovery
                        .channel_rules
                        .checked_add(1)
                        .ok_or(ClcDiscoveryError::CountOverflow)?;
                }
                let alpha2 = [rule[0], rule[1]];
                if alpha2 == *b"00" {
                    discovery.world_domain_available = true;
                }
                if !countries.contains(&alpha2) {
                    countries.push(alpha2);
                }
            }
            rule_offset += rule_length;
        }
        offset = end;
    }
    discovery.unique_country_codes =
        u16::try_from(countries.len()).map_err(|_| ClcDiscoveryError::CountOverflow)?;
    Ok(discovery)
}

/// A bounds-checked view of the Connac2 RAM firmware layout consumed by
/// `mt76_connac_mcu_send_ram_firmware` and `mt7921_load_clc`.
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

/// Portable scan result at the Fuchsia MLME/SME boundary.
///
/// This deliberately contains no FIDL types. `from_beacon` follows pinned
/// Fuchsia `construct_bss_description`: it walks beacon/probe-response IEs,
/// prefers the advertised DSSS channel, and retains capability information.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessPoint {
    pub bssid: [u8; 6],
    pub ssid: Vec<u8>,
    pub frequency_mhz: u16,
    pub channel: u8,
    pub signal_dbm: i16,
    pub capability_info: u16,
    pub security: Security,
    pub ht: bool,
    pub vht: bool,
    pub he: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Security {
    Open,
    Wep,
    Wpa1,
    Wpa2Personal,
    Wpa2Enterprise,
    Wpa3Personal,
    Wpa3Enterprise,
    Owe,
    UnknownProtected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdvertisementError {
    TruncatedElement,
    SsidTooLong,
    InvalidRsn,
}

impl AccessPoint {
    pub fn from_beacon(
        bssid: [u8; 6],
        capability_info: u16,
        ies: &[u8],
        frequency_mhz: u16,
        signal_dbm: i16,
    ) -> Result<Self, AdvertisementError> {
        let mut ssid = Vec::new();
        let mut dsss_channel = None;
        let mut security = None;
        let mut ht = false;
        let mut vht = false;
        let mut he = false;
        let mut offset = 0;
        while offset < ies.len() {
            if ies.len() - offset < 2 {
                return Err(AdvertisementError::TruncatedElement);
            }
            let id = ies[offset];
            let len = usize::from(ies[offset + 1]);
            offset += 2;
            let end = offset
                .checked_add(len)
                .ok_or(AdvertisementError::TruncatedElement)?;
            let body = ies
                .get(offset..end)
                .ok_or(AdvertisementError::TruncatedElement)?;
            offset = end;
            match id {
                0 if body.len() <= 32 => ssid.extend_from_slice(body),
                0 => return Err(AdvertisementError::SsidTooLong),
                3 if body.len() == 1 => dsss_channel = Some(body[0]),
                45 | 61 => ht = true,
                191 | 192 => vht = true,
                48 => security = Some(parse_rsn(body)?),
                221 if body.starts_with(&[0x00, 0x50, 0xf2, 0x01]) && security.is_none() => {
                    security = Some(Security::Wpa1)
                }
                255 if body.first() == Some(&35) || body.first() == Some(&36) => he = true,
                _ => {}
            }
        }
        let privacy = capability_info & 0x0010 != 0;
        Ok(Self {
            bssid,
            ssid,
            frequency_mhz,
            channel: dsss_channel.unwrap_or_else(|| frequency_to_channel(frequency_mhz)),
            signal_dbm,
            capability_info,
            security: security.unwrap_or(if privacy {
                Security::Wep
            } else {
                Security::Open
            }),
            ht,
            vht,
            he,
        })
    }
}

fn parse_rsn(body: &[u8]) -> Result<Security, AdvertisementError> {
    // version(2), group suite(4), pairwise count/list, AKM count/list
    if body.len() < 8 || u16::from_le_bytes([body[0], body[1]]) != 1 {
        return Err(AdvertisementError::InvalidRsn);
    }
    let pairwise_count = u16::from_le_bytes([body[6], body[7]]) as usize;
    let akm_count_at = 8usize
        .checked_add(
            pairwise_count
                .checked_mul(4)
                .ok_or(AdvertisementError::InvalidRsn)?,
        )
        .ok_or(AdvertisementError::InvalidRsn)?;
    let akm_count_bytes = body
        .get(akm_count_at..akm_count_at + 2)
        .ok_or(AdvertisementError::InvalidRsn)?;
    let akm_count = u16::from_le_bytes([akm_count_bytes[0], akm_count_bytes[1]]) as usize;
    let mut enterprise = false;
    let mut personal = false;
    let mut sae = false;
    let mut owe = false;
    let mut suite_at = akm_count_at + 2;
    for _ in 0..akm_count {
        let suite = body
            .get(suite_at..suite_at + 4)
            .ok_or(AdvertisementError::InvalidRsn)?;
        suite_at += 4;
        if suite[..3] != [0x00, 0x0f, 0xac] {
            continue;
        }
        match suite[3] {
            1 | 3 | 5 => enterprise = true,
            2 | 4 | 6 => personal = true,
            8 | 9 => sae = true,
            12 | 13 => return Ok(Security::Wpa3Enterprise),
            18 => owe = true,
            _ => {}
        }
    }
    Ok(if owe {
        Security::Owe
    } else if sae {
        Security::Wpa3Personal
    } else if personal {
        Security::Wpa2Personal
    } else if enterprise {
        Security::Wpa2Enterprise
    } else {
        Security::UnknownProtected
    })
}

pub const fn frequency_to_channel(frequency_mhz: u16) -> u8 {
    match frequency_mhz {
        2484 => 14,
        2412..=2472 => ((frequency_mhz - 2407) / 5) as u8,
        5000..=5895 => ((frequency_mhz - 5000) / 5) as u8,
        5955..=7115 => ((frequency_mhz - 5950) / 5) as u8,
        _ => 0,
    }
}

/// Read-only MT7921 registers admitted by the first physical VFIO slice.
///
/// These BAR offsets are either direct PCIe addresses below 1 MiB or pinned
/// Linux mt7921 fixed-map translations. There is intentionally no generic
/// physical-address translator or caller-selected BAR offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadRegister {
    McuCommand,
    HostInterruptStatus,
    WfdmaGlobalConfig,
    ConnOnLowPowerControl,
    ConnOnMisc,
}

impl ReadRegister {
    pub const ALL: [Self; 5] = [
        Self::McuCommand,
        Self::HostInterruptStatus,
        Self::WfdmaGlobalConfig,
        Self::ConnOnLowPowerControl,
        Self::ConnOnMisc,
    ];

    pub const fn bar_offset(self) -> usize {
        match self {
            // MT_WFDMA0_BASE (0xd4000) plus register offset.
            Self::McuCommand => 0xd41f0,
            Self::HostInterruptStatus => 0xd4200,
            Self::WfdmaGlobalConfig => 0xd4208,
            // Linux fixed-map 0x7c060000 -> BAR 0xe0000.
            Self::ConnOnLowPowerControl => 0xe0010,
            Self::ConnOnMisc => 0xe00f0,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::McuCommand => "mcu_command",
            Self::HostInterruptStatus => "host_interrupt_status",
            Self::WfdmaGlobalConfig => "wfdma_global_config",
            Self::ConnOnLowPowerControl => "conn_on_low_power_control",
            Self::ConnOnMisc => "conn_on_misc",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadOnlyStatus {
    pub firmware_powered: bool,
    pub firmware_n9_ready: bool,
    pub firmware_owns_device: bool,
    pub tx_dma_enabled: bool,
    pub tx_dma_busy: bool,
    pub rx_dma_enabled: bool,
    pub rx_dma_busy: bool,
}

// Pinned Linux mt792x_regs.h names these bits PCIE_LPCR_HOST_{SET,CLR}_OWN and
// PCIE_LPCR_HOST_OWN_SYNC. __mt792xe_mcu_drv_pmctrl writes CLR_OWN and polls
// OWN_SYNC clear up to ten times, with a 50 ms poll per attempt at 1 ms ticks.
pub const PCIE_LPCR_HOST_SET_OWN: u32 = 1 << 0;
pub const PCIE_LPCR_HOST_CLR_OWN: u32 = 1 << 1;
pub const PCIE_LPCR_HOST_OWN_SYNC: u32 = 1 << 2;
pub const DRIVER_OWN_ATTEMPTS: u8 = 10;
pub const DRIVER_OWN_ATTEMPT_MS: u64 = 50;
pub const DRIVER_OWN_POLL_MS: u64 = 1;
pub const DRIVER_OWN_HARD_DEADLINE_MS: u64 = DRIVER_OWN_ATTEMPTS as u64 * DRIVER_OWN_ATTEMPT_MS;

pub trait OwnershipTransport {
    type Error;

    fn now_ms(&self) -> u64;
    fn write_clear_own(&mut self) -> Result<(), Self::Error>;
    fn read_low_power_control(&mut self) -> Result<u32, Self::Error>;
    fn sleep_ms(&mut self, milliseconds: u64);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnershipEvent {
    ClearOwnWritten { attempt: u8, at_ms: u64 },
    StatusRead { attempt: u8, at_ms: u64, raw: u32 },
    AttemptExpired { attempt: u8, at_ms: u64 },
    Acquired { attempt: u8, at_ms: u64 },
    UnexpectedState { attempt: u8, at_ms: u64, raw: u32 },
    TimedOut { at_ms: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnershipError<E> {
    Transport(E),
    ClockOverflow,
    UnexpectedState(u32),
    Timeout,
}

/// Acquire PCIe driver ownership using the exact bounded Linux retry shape.
///
/// The transport contract exposes only the one CLR_OWN write, the matching
/// status read, a monotonic clock, and bounded sleep. The event sink makes each
/// state transition durable at the host adapter without coupling this logic to
/// a logger or component runtime.
pub fn acquire_driver_ownership<T, F>(
    transport: &mut T,
    mut event: F,
) -> Result<(), OwnershipError<T::Error>>
where
    T: OwnershipTransport,
    F: FnMut(OwnershipEvent),
{
    let start = transport.now_ms();
    let hard_deadline = start
        .checked_add(DRIVER_OWN_HARD_DEADLINE_MS)
        .ok_or(OwnershipError::ClockOverflow)?;
    for attempt in 1..=DRIVER_OWN_ATTEMPTS {
        let now = transport.now_ms();
        if now >= hard_deadline {
            event(OwnershipEvent::TimedOut { at_ms: now - start });
            return Err(OwnershipError::Timeout);
        }
        transport
            .write_clear_own()
            .map_err(OwnershipError::Transport)?;
        event(OwnershipEvent::ClearOwnWritten {
            attempt,
            at_ms: transport.now_ms().saturating_sub(start),
        });
        let attempt_deadline = transport
            .now_ms()
            .checked_add(DRIVER_OWN_ATTEMPT_MS)
            .ok_or(OwnershipError::ClockOverflow)?
            .min(hard_deadline);
        loop {
            let raw = transport
                .read_low_power_control()
                .map_err(OwnershipError::Transport)?;
            let now = transport.now_ms();
            event(OwnershipEvent::StatusRead {
                attempt,
                at_ms: now.saturating_sub(start),
                raw,
            });
            // SET_OWN and CLR_OWN are write commands. Seeing either asserted
            // on readback is not a state Linux relies on and is rejected.
            if raw & (PCIE_LPCR_HOST_SET_OWN | PCIE_LPCR_HOST_CLR_OWN) != 0 {
                event(OwnershipEvent::UnexpectedState {
                    attempt,
                    at_ms: now.saturating_sub(start),
                    raw,
                });
                return Err(OwnershipError::UnexpectedState(raw));
            }
            if raw & PCIE_LPCR_HOST_OWN_SYNC == 0 {
                event(OwnershipEvent::Acquired {
                    attempt,
                    at_ms: now.saturating_sub(start),
                });
                return Ok(());
            }
            if now >= attempt_deadline {
                event(OwnershipEvent::AttemptExpired {
                    attempt,
                    at_ms: now.saturating_sub(start),
                });
                break;
            }
            transport.sleep_ms(DRIVER_OWN_POLL_MS.min(attempt_deadline - now));
        }
    }
    let at_ms = transport.now_ms().saturating_sub(start);
    event(OwnershipEvent::TimedOut { at_ms });
    Err(OwnershipError::Timeout)
}

pub const MT_HIF_REMAP_L1_BAR_OFFSET: usize = 0xfe24c;
pub const MT_HIF_REMAP_WINDOW_BAR_OFFSET: usize = 0x40000;
const MT_HIF_REMAP_L1_MASK: u32 = 0xffff;

pub trait DynamicL1Transport {
    type Error;
    fn read_selector(&mut self) -> Result<u32, Self::Error>;
    fn write_selector(&mut self, value: u32) -> Result<(), Self::Error>;
    fn read_window(&mut self, offset: u16) -> Result<u32, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicL1Event {
    SelectorSaved {
        raw: u32,
    },
    SelectorWritten {
        base: u16,
        raw: u32,
    },
    SelectorVerified {
        base: u16,
        raw: u32,
    },
    RegisterRead {
        name: &'static str,
        physical: u32,
        value: u32,
    },
    SelectorRestored {
        raw: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DynamicL1Error<E> {
    Transport(E),
    SelectorMismatch { expected_base: u16, raw: u32 },
    Restore(E),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicIdentityStatus {
    pub chip_id: u32,
    pub revision: u32,
    pub hardware_bound: u32,
    pub top_low_power_control: u32,
}

/// Execute the first bounded dynamic-L1 transaction used by pinned mt7921.
///
/// The physical targets are closed over here rather than accepted from the
/// caller. The selector is restored on both success and read/select failure.
pub fn read_dynamic_identity_status<T, F>(
    transport: &mut T,
    mut event: F,
) -> Result<DynamicIdentityStatus, DynamicL1Error<T::Error>>
where
    T: DynamicL1Transport,
    F: FnMut(DynamicL1Event),
{
    let saved = transport
        .read_selector()
        .map_err(DynamicL1Error::Transport)?;
    event(DynamicL1Event::SelectorSaved { raw: saved });
    let operation = (|| {
        select_l1(transport, saved, 0x7001, &mut event)?;
        let chip_id = read_l1(transport, "chip_id", 0x7001_0200, &mut event)?;
        let revision = read_l1(transport, "revision", 0x7001_0204, &mut event)?;
        let hardware_bound = read_l1(transport, "hardware_bound", 0x7001_0020, &mut event)?;
        select_l1(transport, saved, 0x1806, &mut event)?;
        let top_low_power_control =
            read_l1(transport, "top_low_power_control", 0x1806_0010, &mut event)?;
        Ok(DynamicIdentityStatus {
            chip_id,
            revision,
            hardware_bound,
            top_low_power_control,
        })
    })();
    if let Err(error) = transport.write_selector(saved) {
        return Err(DynamicL1Error::Restore(error));
    }
    event(DynamicL1Event::SelectorRestored { raw: saved });
    operation
}

fn select_l1<T, F>(
    transport: &mut T,
    saved: u32,
    base: u16,
    event: &mut F,
) -> Result<(), DynamicL1Error<T::Error>>
where
    T: DynamicL1Transport,
    F: FnMut(DynamicL1Event),
{
    let selected = (saved & !MT_HIF_REMAP_L1_MASK) | u32::from(base);
    transport
        .write_selector(selected)
        .map_err(DynamicL1Error::Transport)?;
    event(DynamicL1Event::SelectorWritten {
        base,
        raw: selected,
    });
    // Linux reads MT_HIF_REMAP_L1 to push the selector write.
    let verified = transport
        .read_selector()
        .map_err(DynamicL1Error::Transport)?;
    event(DynamicL1Event::SelectorVerified {
        base,
        raw: verified,
    });
    if verified & MT_HIF_REMAP_L1_MASK != u32::from(base) {
        return Err(DynamicL1Error::SelectorMismatch {
            expected_base: base,
            raw: verified,
        });
    }
    Ok(())
}

fn read_l1<T, F>(
    transport: &mut T,
    name: &'static str,
    physical: u32,
    event: &mut F,
) -> Result<u32, DynamicL1Error<T::Error>>
where
    T: DynamicL1Transport,
    F: FnMut(DynamicL1Event),
{
    let value = transport
        .read_window(physical as u16)
        .map_err(DynamicL1Error::Transport)?;
    event(DynamicL1Event::RegisterRead {
        name,
        physical,
        value,
    });
    Ok(value)
}

pub const MT_TOP_LPCR_HOST_FW_OWN: u32 = 1 << 0;
pub const MT_TOP_LPCR_HOST_DRV_OWN: u32 = 1 << 1;
pub const TOP_DRIVER_OWN_DEADLINE_MS: u64 = 500;

pub trait TopOwnershipTransport {
    type Error;
    fn now_ms(&self) -> u64;
    fn read_selector(&mut self) -> Result<u32, Self::Error>;
    fn write_selector(&mut self, value: u32) -> Result<(), Self::Error>;
    fn write_top_driver_own(&mut self) -> Result<(), Self::Error>;
    fn read_top_low_power_control(&mut self) -> Result<u32, Self::Error>;
    fn sleep_ms(&mut self, milliseconds: u64);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TopOwnershipEvent {
    SelectorSaved { raw: u32 },
    SelectorWritten { raw: u32 },
    SelectorVerified { raw: u32 },
    DriverOwnWritten { at_ms: u64 },
    StatusRead { at_ms: u64, raw: u32 },
    Acquired { at_ms: u64 },
    UnexpectedState { at_ms: u64, raw: u32 },
    TimedOut { at_ms: u64 },
    SelectorRestored { raw: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TopOwnershipError<E> {
    Transport(E),
    ClockOverflow,
    SelectorMismatch(u32),
    UnexpectedState(u32),
    Timeout,
    Restore(E),
}

/// Port pinned Linux `mt7921e_driver_own` with mandatory remap restoration.
pub fn acquire_top_driver_ownership<T, F>(
    transport: &mut T,
    mut event: F,
) -> Result<(), TopOwnershipError<T::Error>>
where
    T: TopOwnershipTransport,
    F: FnMut(TopOwnershipEvent),
{
    let saved = transport
        .read_selector()
        .map_err(TopOwnershipError::Transport)?;
    event(TopOwnershipEvent::SelectorSaved { raw: saved });
    let start = transport.now_ms();
    let deadline = start
        .checked_add(TOP_DRIVER_OWN_DEADLINE_MS)
        .ok_or(TopOwnershipError::ClockOverflow)?;
    let operation = (|| {
        let selected = (saved & !MT_HIF_REMAP_L1_MASK) | 0x1806;
        transport
            .write_selector(selected)
            .map_err(TopOwnershipError::Transport)?;
        event(TopOwnershipEvent::SelectorWritten { raw: selected });
        let verified = transport
            .read_selector()
            .map_err(TopOwnershipError::Transport)?;
        event(TopOwnershipEvent::SelectorVerified { raw: verified });
        if verified & MT_HIF_REMAP_L1_MASK != 0x1806 {
            return Err(TopOwnershipError::SelectorMismatch(verified));
        }
        transport
            .write_top_driver_own()
            .map_err(TopOwnershipError::Transport)?;
        event(TopOwnershipEvent::DriverOwnWritten {
            at_ms: transport.now_ms().saturating_sub(start),
        });
        loop {
            let raw = transport
                .read_top_low_power_control()
                .map_err(TopOwnershipError::Transport)?;
            let now = transport.now_ms();
            event(TopOwnershipEvent::StatusRead {
                at_ms: now.saturating_sub(start),
                raw,
            });
            if raw & MT_TOP_LPCR_HOST_DRV_OWN != 0 {
                event(TopOwnershipEvent::UnexpectedState {
                    at_ms: now.saturating_sub(start),
                    raw,
                });
                return Err(TopOwnershipError::UnexpectedState(raw));
            }
            if raw & MT_TOP_LPCR_HOST_FW_OWN == 0 {
                event(TopOwnershipEvent::Acquired {
                    at_ms: now.saturating_sub(start),
                });
                return Ok(());
            }
            if now >= deadline {
                event(TopOwnershipEvent::TimedOut {
                    at_ms: now.saturating_sub(start),
                });
                return Err(TopOwnershipError::Timeout);
            }
            transport.sleep_ms(DRIVER_OWN_POLL_MS.min(deadline - now));
        }
    })();
    if let Err(error) = transport.write_selector(saved) {
        return Err(TopOwnershipError::Restore(error));
    }
    event(TopOwnershipEvent::SelectorRestored { raw: saved });
    operation
}

pub const MT7921_FWDL_RING_COUNT: u32 = 128;
pub const MT7921_FWDL_RING_BYTES: usize = MT7921_FWDL_RING_COUNT as usize * DMA_DESCRIPTOR_LEN;
pub const MT7921_INT_TX_DONE_FWDL: u32 = 1 << 26;
pub const MT7921_FWDL_CHUNK_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledFwdlRegister {
    HostInterruptEnable,
    WfdmaGlobalConfig,
    DescriptorBase,
    DescriptorCount,
    CpuIndex,
    DmaIndex,
}
impl DisabledFwdlRegister {
    pub const fn bar_offset(self) -> usize {
        match self {
            Self::HostInterruptEnable => 0xd4204,
            Self::WfdmaGlobalConfig => 0xd4208,
            // MT_TX_RING_BASE 0xd4300 + MT7921_TXQ_FWDL(16) * 0x10.
            Self::DescriptorBase => 0xd4400,
            Self::DescriptorCount => 0xd4404,
            Self::CpuIndex => 0xd4408,
            Self::DmaIndex => 0xd440c,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledFwdlWrite {
    DescriptorBase,
    DescriptorCount,
    CpuIndex,
}

pub trait DisabledFwdlRingTransport {
    type Error;
    fn read(&mut self, register: DisabledFwdlRegister) -> Result<u32, Self::Error>;
    fn write(&mut self, register: DisabledFwdlWrite, value: u32) -> Result<(), Self::Error>;
    fn release_fence(&mut self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FwdlRingRegisters {
    pub descriptor_base: u32,
    pub descriptor_count: u32,
    pub cpu_index: u32,
    pub dma_index: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledFwdlEvent {
    DisabledVerified {
        global_config: u32,
        interrupt_enable: u32,
    },
    Snapshot(FwdlRingRegisters),
    DescriptorFence,
    RegisterWritten {
        register: DisabledFwdlWrite,
        value: u32,
    },
    Programmed(FwdlRingRegisters),
    Restored(FwdlRingRegisters),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisabledFwdlError<E> {
    InvalidArena,
    DmaOrInterruptActive {
        global_config: u32,
        interrupt_enable: u32,
    },
    Transport(E),
    Readback {
        expected: FwdlRingRegisters,
        actual: FwdlRingRegisters,
    },
    Restore(E),
}

/// Temporarily program the inactive MT7921 firmware-download TX ring.
///
/// The transaction refuses to run unless TX/RX DMA-enable bits and every host
/// interrupt-enable bit are clear. Descriptor initialization must precede the
/// release fence supplied here. Only base/count/CPU index are written; the
/// device DMA index, WFDMA configuration, and interrupt registers are never
/// written. Original values are restored before return.
pub fn program_disabled_fwdl_ring<T, F>(
    transport: &mut T,
    arena_iova: u64,
    mut event: F,
) -> Result<FwdlRingRegisters, DisabledFwdlError<T::Error>>
where
    T: DisabledFwdlRingTransport,
    F: FnMut(DisabledFwdlEvent),
{
    if !arena_iova.is_multiple_of(4096)
        || arena_iova
            .checked_add(MT7921_FWDL_RING_BYTES as u64 - 1)
            .is_none_or(|end| end > u64::from(u32::MAX))
    {
        return Err(DisabledFwdlError::InvalidArena);
    }
    let interrupt_enable = transport
        .read(DisabledFwdlRegister::HostInterruptEnable)
        .map_err(DisabledFwdlError::Transport)?;
    let global_config = transport
        .read(DisabledFwdlRegister::WfdmaGlobalConfig)
        .map_err(DisabledFwdlError::Transport)?;
    if interrupt_enable != 0 || global_config & 0x5 != 0 {
        return Err(DisabledFwdlError::DmaOrInterruptActive {
            global_config,
            interrupt_enable,
        });
    }
    event(DisabledFwdlEvent::DisabledVerified {
        global_config,
        interrupt_enable,
    });
    let snapshot = read_fwdl_registers(transport).map_err(DisabledFwdlError::Transport)?;
    event(DisabledFwdlEvent::Snapshot(snapshot));
    transport.release_fence();
    event(DisabledFwdlEvent::DescriptorFence);
    let expected = FwdlRingRegisters {
        descriptor_base: arena_iova as u32,
        descriptor_count: MT7921_FWDL_RING_COUNT,
        cpu_index: 0,
        dma_index: snapshot.dma_index,
    };
    let operation = (|| {
        for (register, value) in [
            (DisabledFwdlWrite::DescriptorBase, expected.descriptor_base),
            (
                DisabledFwdlWrite::DescriptorCount,
                expected.descriptor_count,
            ),
            (DisabledFwdlWrite::CpuIndex, expected.cpu_index),
        ] {
            transport
                .write(register, value)
                .map_err(DisabledFwdlError::Transport)?;
            event(DisabledFwdlEvent::RegisterWritten { register, value });
        }
        let actual = read_fwdl_registers(transport).map_err(DisabledFwdlError::Transport)?;
        if actual != expected {
            return Err(DisabledFwdlError::Readback { expected, actual });
        }
        event(DisabledFwdlEvent::Programmed(actual));
        Ok(actual)
    })();
    for (register, value) in [
        (DisabledFwdlWrite::CpuIndex, snapshot.cpu_index),
        (
            DisabledFwdlWrite::DescriptorCount,
            snapshot.descriptor_count,
        ),
        (DisabledFwdlWrite::DescriptorBase, snapshot.descriptor_base),
    ] {
        if let Err(error) = transport.write(register, value) {
            return Err(DisabledFwdlError::Restore(error));
        }
    }
    event(DisabledFwdlEvent::Restored(snapshot));
    operation
}

fn read_fwdl_registers<T: DisabledFwdlRingTransport>(
    transport: &mut T,
) -> Result<FwdlRingRegisters, T::Error> {
    Ok(FwdlRingRegisters {
        descriptor_base: transport.read(DisabledFwdlRegister::DescriptorBase)?,
        descriptor_count: transport.read(DisabledFwdlRegister::DescriptorCount)?,
        cpu_index: transport.read(DisabledFwdlRegister::CpuIndex)?,
        dma_index: transport.read(DisabledFwdlRegister::DmaIndex)?,
    })
}

pub trait DisabledFwdlInterruptTransport {
    type Error;
    fn read_global_config(&mut self) -> Result<u32, Self::Error>;
    fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error>;
    fn write_interrupt_enable(&mut self, value: u32) -> Result<(), Self::Error>;
    fn read_interrupt_status(&mut self) -> Result<u32, Self::Error>;
    fn acknowledge_interrupt_status(&mut self, value: u32) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledInterruptEvent {
    Snapshot {
        global_config: u32,
        enable: u32,
        status: u32,
    },
    Masked,
    Acknowledged {
        value: u32,
    },
    Readback {
        status: u32,
    },
    MaskRestored {
        value: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisabledInterruptError<E> {
    DmaActive(u32),
    InterruptsEnabled(u32),
    Transport(E),
    MaskReadback(u32),
    AckDidNotClear(u32),
    Restore(E),
}

/// Port the firmware-download subset of pinned Linux `mt792x_irq_tasklet`.
///
/// The physical slice starts with a zero host mask, writes that same zero mask,
/// and acknowledges only TX ring 16's W1C status bit. No unrelated pending bit
/// is acknowledged and no interrupt source is enabled or armed.
pub fn mask_ack_disabled_fwdl_interrupt<T, F>(
    transport: &mut T,
    mut event: F,
) -> Result<u32, DisabledInterruptError<T::Error>>
where
    T: DisabledFwdlInterruptTransport,
    F: FnMut(DisabledInterruptEvent),
{
    let global_config = transport
        .read_global_config()
        .map_err(DisabledInterruptError::Transport)?;
    if global_config & 0x5 != 0 {
        return Err(DisabledInterruptError::DmaActive(global_config));
    }
    let enable = transport
        .read_interrupt_enable()
        .map_err(DisabledInterruptError::Transport)?;
    if enable != 0 {
        return Err(DisabledInterruptError::InterruptsEnabled(enable));
    }
    let status = transport
        .read_interrupt_status()
        .map_err(DisabledInterruptError::Transport)?;
    event(DisabledInterruptEvent::Snapshot {
        global_config,
        enable,
        status,
    });
    transport
        .write_interrupt_enable(0)
        .map_err(DisabledInterruptError::Transport)?;
    event(DisabledInterruptEvent::Masked);
    let operation = (|| {
        let mask_readback = transport
            .read_interrupt_enable()
            .map_err(DisabledInterruptError::Transport)?;
        if mask_readback != 0 {
            return Err(DisabledInterruptError::MaskReadback(mask_readback));
        }
        let acknowledged = status & MT7921_INT_TX_DONE_FWDL;
        transport
            .acknowledge_interrupt_status(acknowledged)
            .map_err(DisabledInterruptError::Transport)?;
        event(DisabledInterruptEvent::Acknowledged {
            value: acknowledged,
        });
        let readback = transport
            .read_interrupt_status()
            .map_err(DisabledInterruptError::Transport)?;
        event(DisabledInterruptEvent::Readback { status: readback });
        if readback & acknowledged != 0 {
            return Err(DisabledInterruptError::AckDidNotClear(readback));
        }
        Ok(readback)
    })();
    if let Err(error) = transport.write_interrupt_enable(enable) {
        return Err(DisabledInterruptError::Restore(error));
    }
    event(DisabledInterruptEvent::MaskRestored { value: enable });
    operation
}

pub trait DisabledFirmwareStageTransport {
    type Error;
    fn write_payload(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    fn write_descriptor(&mut self, descriptor: DmaDescriptor) -> Result<(), Self::Error>;
    fn release_fence(&mut self);
    fn read_descriptor(&mut self) -> Result<DmaDescriptor, Self::Error>;
    fn reset_descriptor(&mut self) -> Result<(), Self::Error>;
    fn zero_payload(&mut self, length: usize) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledFirmwareStageEvent {
    PayloadWritten { bytes: usize, iova: u64 },
    DescriptorWritten(DmaDescriptor),
    DescriptorFence,
    DescriptorVerified(DmaDescriptor),
    DescriptorReset,
    PayloadZeroed { bytes: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisabledFirmwareStageError<E> {
    InvalidPayload,
    InvalidIova,
    Descriptor(DescriptorError),
    Transport(E),
    DescriptorReadback {
        expected: DmaDescriptor,
        actual: DmaDescriptor,
    },
    Reset(E),
}

/// Stage one raw `FW_SCATTER` chunk without publishing a producer index.
///
/// Pinned Linux limits PCI firmware chunks to 4096 bytes. `FW_SCATTER` skips
/// the normal MCU TX header, so the ring descriptor directly names the bounded
/// artifact payload. Cleanup is mandatory on success and readback failure.
pub fn stage_disabled_firmware_chunk<T, F>(
    transport: &mut T,
    payload_iova: u64,
    payload: &[u8],
    mut event: F,
) -> Result<DmaDescriptor, DisabledFirmwareStageError<T::Error>>
where
    T: DisabledFirmwareStageTransport,
    F: FnMut(DisabledFirmwareStageEvent),
{
    if payload.is_empty() || payload.len() > MT7921_FWDL_CHUNK_BYTES {
        return Err(DisabledFirmwareStageError::InvalidPayload);
    }
    if payload_iova
        .checked_add(payload.len() as u64 - 1)
        .is_none_or(|end| end > u64::from(u32::MAX))
    {
        return Err(DisabledFirmwareStageError::InvalidIova);
    }
    let descriptor = DmaDescriptor::tx(
        DmaSegment {
            iova: payload_iova,
            len: payload.len() as u16,
        },
        None,
        0,
    )
    .map_err(DisabledFirmwareStageError::Descriptor)?;
    let operation = (|| {
        transport
            .write_payload(payload)
            .map_err(DisabledFirmwareStageError::Transport)?;
        event(DisabledFirmwareStageEvent::PayloadWritten {
            bytes: payload.len(),
            iova: payload_iova,
        });
        transport
            .write_descriptor(descriptor)
            .map_err(DisabledFirmwareStageError::Transport)?;
        event(DisabledFirmwareStageEvent::DescriptorWritten(descriptor));
        transport.release_fence();
        event(DisabledFirmwareStageEvent::DescriptorFence);
        match transport.read_descriptor() {
            Ok(actual) if actual == descriptor => {
                event(DisabledFirmwareStageEvent::DescriptorVerified(actual));
                Ok(descriptor)
            }
            Ok(actual) => Err(DisabledFirmwareStageError::DescriptorReadback {
                expected: descriptor,
                actual,
            }),
            Err(error) => Err(DisabledFirmwareStageError::Transport(error)),
        }
    })();
    let descriptor_reset = transport.reset_descriptor();
    if descriptor_reset.is_ok() {
        event(DisabledFirmwareStageEvent::DescriptorReset);
    }
    let payload_zeroed = transport.zero_payload(payload.len());
    if payload_zeroed.is_ok() {
        event(DisabledFirmwareStageEvent::PayloadZeroed {
            bytes: payload.len(),
        });
    }
    descriptor_reset.map_err(DisabledFirmwareStageError::Reset)?;
    payload_zeroed.map_err(DisabledFirmwareStageError::Reset)?;
    operation
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareCompletion {
    Partial { completed: u16, total: u16 },
    Complete,
    TimedOut { completed: u16, total: u16 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareCompletionTracker {
    total: u16,
    completed: u16,
    deadline_ms: u64,
}
impl FirmwareCompletionTracker {
    pub fn new(total: u16, start_ms: u64, timeout_ms: u64) -> Option<Self> {
        Some(Self {
            total: (total != 0).then_some(total)?,
            completed: 0,
            deadline_ms: start_ms.checked_add(timeout_ms)?,
        })
    }
    pub fn observe(&mut self, completed: u16, now_ms: u64) -> Option<FirmwareCompletion> {
        if completed < self.completed || completed > self.total {
            return None;
        }
        self.completed = completed;
        if completed == self.total {
            Some(FirmwareCompletion::Complete)
        } else if now_ms >= self.deadline_ms {
            Some(FirmwareCompletion::TimedOut {
                completed,
                total: self.total,
            })
        } else {
            Some(FirmwareCompletion::Partial {
                completed,
                total: self.total,
            })
        }
    }
}

pub const MT7921_TX_RING_SLOTS: usize = 18;
pub const MT7921_FWDL_RING_INDEX: usize = 16;
pub const MT7921_MCU_TX_RING_INDEX: usize = 17;
pub const MT7921_MCU_TX_RING_COUNT: u32 = 256;
pub const MT7921_MCU_RX_RING_COUNT: usize = 8;
pub const MT7921_MCU_RX_BUFFER_BYTES: usize = 2048;
pub const MT7921_RX_RING_SLOTS: usize = 8;
pub const MT7921_RESET_ALL_TX_INDICES: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McuRxRing {
    pub descriptors: [DmaDescriptor; MT7921_MCU_RX_RING_COUNT],
    pub producer_index: u32,
}

/// Build Linux's eight-entry pre-firmware MCU response ring while retaining
/// one empty descriptor so producer and consumer indices cannot alias full.
pub fn prepare_mcu_rx_ring(
    ring_iova: u64,
    buffers_iova: u64,
) -> Result<McuRxRing, DescriptorError> {
    let buffers_bytes = MT7921_MCU_RX_RING_COUNT * MT7921_MCU_RX_BUFFER_BYTES;
    if !ring_iova.is_multiple_of(4096)
        || !buffers_iova.is_multiple_of(4096)
        || ring_iova
            .checked_add(4095)
            .is_none_or(|end| end > u64::from(u32::MAX))
        || buffers_iova
            .checked_add(buffers_bytes as u64 - 1)
            .is_none_or(|end| end > u64::from(u32::MAX))
        || (ring_iova < buffers_iova + buffers_bytes as u64 && buffers_iova <= ring_iova + 4095)
    {
        return Err(DescriptorError::InvalidArena);
    }
    let mut descriptors = [DmaDescriptor::reset(); MT7921_MCU_RX_RING_COUNT];
    for (index, descriptor) in descriptors
        .iter_mut()
        .enumerate()
        .take(MT7921_MCU_RX_RING_COUNT - 1)
    {
        *descriptor = DmaDescriptor::rx(DmaSegment {
            iova: buffers_iova + (index * MT7921_MCU_RX_BUFFER_BYTES) as u64,
            len: MT7921_MCU_RX_BUFFER_BYTES as u16,
        })?;
    }
    Ok(McuRxRing {
        descriptors,
        producer_index: (MT7921_MCU_RX_RING_COUNT - 1) as u32,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McuRxRegisters {
    pub descriptor_base: u32,
    pub descriptor_count: u32,
    pub cpu_index: u32,
    pub dma_index: u32,
}

pub trait DisabledMcuRxTransport {
    type Error;
    fn read_global_config(&mut self) -> Result<u32, Self::Error>;
    fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error>;
    fn read_registers(&mut self) -> Result<McuRxRegisters, Self::Error>;
    fn read_registers_at(&mut self, index: usize) -> Result<McuRxRegisters, Self::Error>;
    fn write_initial(
        &mut self,
        descriptor_base: u32,
        descriptor_count: u32,
    ) -> Result<(), Self::Error>;
    fn publish_cpu_index(&mut self, cpu_index: u32) -> Result<(), Self::Error>;
    fn write_ring_initial(
        &mut self,
        index: usize,
        descriptor_base: u32,
        descriptor_count: u32,
    ) -> Result<(), Self::Error>;
    fn publish_ring_cpu_index(&mut self, index: usize, cpu_index: u32) -> Result<(), Self::Error>;
    fn release_fence(&mut self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledMcuRxEvent {
    Snapshot(McuRxRegisters),
    SnapshotAt { index: usize, state: McuRxRegisters },
    DescriptorFence,
    Programmed(McuRxRegisters),
    ProgrammedAt { index: usize, state: McuRxRegisters },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisabledMcuRxError<E> {
    InvalidArena,
    InvalidMmio,
    ActiveState {
        global_config: u32,
        interrupt_enable: u32,
    },
    DirtyRing(McuRxRegisters),
    Transport(E),
    Readback(McuRxRegisters),
}

/// Program the pre-firmware MCU response ring without enabling RX DMA.
pub fn program_disabled_mcu_rx_ring<T, F>(
    transport: &mut T,
    ring_iova: u64,
    mut event: F,
) -> Result<McuRxRegisters, DisabledMcuRxError<T::Error>>
where
    T: DisabledMcuRxTransport,
    F: FnMut(DisabledMcuRxEvent),
{
    if !ring_iova.is_multiple_of(4096)
        || ring_iova
            .checked_add(4095)
            .is_none_or(|end| end > u64::from(u32::MAX))
    {
        return Err(DisabledMcuRxError::InvalidArena);
    }
    let global_config = transport
        .read_global_config()
        .map_err(DisabledMcuRxError::Transport)?;
    let interrupt_enable = transport
        .read_interrupt_enable()
        .map_err(DisabledMcuRxError::Transport)?;
    if global_config & 0xf != 0 || interrupt_enable != 0 {
        return Err(DisabledMcuRxError::ActiveState {
            global_config,
            interrupt_enable,
        });
    }
    let snapshot = transport
        .read_registers()
        .map_err(DisabledMcuRxError::Transport)?;
    event(DisabledMcuRxEvent::Snapshot(snapshot));
    if snapshot.cpu_index != snapshot.dma_index {
        return Err(DisabledMcuRxError::DirtyRing(snapshot));
    }
    let initial = McuRxRegisters {
        descriptor_base: ring_iova as u32,
        descriptor_count: MT7921_MCU_RX_RING_COUNT as u32,
        cpu_index: 0,
        dma_index: 0,
    };
    transport
        .write_initial(initial.descriptor_base, initial.descriptor_count)
        .map_err(DisabledMcuRxError::Transport)?;
    if transport
        .read_registers()
        .map_err(DisabledMcuRxError::Transport)?
        != initial
    {
        return Err(DisabledMcuRxError::Readback(
            transport
                .read_registers()
                .map_err(DisabledMcuRxError::Transport)?,
        ));
    }
    transport.release_fence();
    event(DisabledMcuRxEvent::DescriptorFence);
    transport
        .publish_cpu_index((MT7921_MCU_RX_RING_COUNT - 1) as u32)
        .map_err(DisabledMcuRxError::Transport)?;
    let expected = McuRxRegisters {
        cpu_index: (MT7921_MCU_RX_RING_COUNT - 1) as u32,
        ..initial
    };
    let actual = transport
        .read_registers()
        .map_err(DisabledMcuRxError::Transport)?;
    if actual != expected {
        return Err(DisabledMcuRxError::Readback(actual));
    }
    event(DisabledMcuRxEvent::Programmed(actual));
    Ok(actual)
}

/// Replace every MT7921 RX ring slot with owned backing while RX DMA is off.
///
/// Ring zero receives the seven-buffer MCU response queue. All other slots
/// point at a CPU-owned guard page with equal producer and consumer indices,
/// so globally enabling RX DMA cannot follow stale kernel mappings.
pub fn prepare_global_rx_rings<T, F>(
    transport: &mut T,
    guard_iova: u64,
    mcu_ring_iova: u64,
    mut event: F,
) -> Result<[McuRxRegisters; MT7921_RX_RING_SLOTS], DisabledMcuRxError<T::Error>>
where
    T: DisabledMcuRxTransport,
    F: FnMut(DisabledMcuRxEvent),
{
    let valid_page = |iova: u64| {
        iova.is_multiple_of(4096)
            && iova
                .checked_add(4095)
                .is_some_and(|end| end <= u64::from(u32::MAX))
    };
    if !valid_page(guard_iova)
        || !valid_page(mcu_ring_iova)
        || guard_iova.abs_diff(mcu_ring_iova) < 4096
    {
        return Err(DisabledMcuRxError::InvalidArena);
    }
    let global_config = transport
        .read_global_config()
        .map_err(DisabledMcuRxError::Transport)?;
    let interrupt_enable = transport
        .read_interrupt_enable()
        .map_err(DisabledMcuRxError::Transport)?;
    if global_config & 0xf != 0 || interrupt_enable != 0 {
        return Err(DisabledMcuRxError::ActiveState {
            global_config,
            interrupt_enable,
        });
    }
    let mut owned = [McuRxRegisters {
        descriptor_base: 0,
        descriptor_count: 0,
        cpu_index: 0,
        dma_index: 0,
    }; MT7921_RX_RING_SLOTS];
    for index in 0..MT7921_RX_RING_SLOTS {
        let snapshot = transport
            .read_registers_at(index)
            .map_err(DisabledMcuRxError::Transport)?;
        event(DisabledMcuRxEvent::SnapshotAt {
            index,
            state: snapshot,
        });
        if [
            snapshot.descriptor_base,
            snapshot.descriptor_count,
            snapshot.cpu_index,
            snapshot.dma_index,
        ]
        .contains(&u32::MAX)
        {
            return Err(DisabledMcuRxError::InvalidMmio);
        }
        if snapshot.cpu_index != snapshot.dma_index {
            return Err(DisabledMcuRxError::DirtyRing(snapshot));
        }
    }
    for (index, expected) in owned.iter_mut().enumerate() {
        *expected = McuRxRegisters {
            descriptor_base: if index == 0 {
                mcu_ring_iova as u32
            } else {
                guard_iova as u32
            },
            descriptor_count: MT7921_MCU_RX_RING_COUNT as u32,
            cpu_index: if index == 0 {
                (MT7921_MCU_RX_RING_COUNT - 1) as u32
            } else {
                0
            },
            dma_index: 0,
        };
        transport
            .write_ring_initial(index, expected.descriptor_base, expected.descriptor_count)
            .map_err(DisabledMcuRxError::Transport)?;
    }
    transport.release_fence();
    event(DisabledMcuRxEvent::DescriptorFence);
    for (index, expected) in owned.iter().enumerate() {
        transport
            .publish_ring_cpu_index(index, expected.cpu_index)
            .map_err(DisabledMcuRxError::Transport)?;
        let actual = transport
            .read_registers_at(index)
            .map_err(DisabledMcuRxError::Transport)?;
        if actual != *expected {
            return Err(DisabledMcuRxError::Readback(actual));
        }
        event(DisabledMcuRxEvent::ProgrammedAt {
            index,
            state: actual,
        });
    }
    Ok(owned)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxRingState {
    pub descriptor_base: u32,
    pub descriptor_count: u32,
    pub cpu_index: u32,
    pub dma_index: u32,
}

pub trait GlobalTxRingTransport {
    type Error;
    fn read_global_config(&mut self) -> Result<u32, Self::Error>;
    fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error>;
    fn read_tx_ring(&mut self, index: usize) -> Result<TxRingState, Self::Error>;
    fn write_tx_ring(
        &mut self,
        index: usize,
        descriptor_base: u32,
        descriptor_count: u32,
        cpu_index: u32,
    ) -> Result<(), Self::Error>;
    fn reset_tx_indices(&mut self, value: u32) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlobalTxRingEvent {
    Snapshot { index: usize, state: TxRingState },
    RingOwned { index: usize, descriptor_base: u32 },
    IndicesReset,
    RingVerified { index: usize, state: TxRingState },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlobalTxRingError<E> {
    InvalidArena,
    ActiveState {
        global_config: u32,
        interrupt_enable: u32,
    },
    InvalidMmio,
    DirtyRing {
        index: usize,
        state: TxRingState,
    },
    Transport(E),
    Readback {
        index: usize,
        state: TxRingState,
    },
}

/// Replace every globally enabled TX ring with owned, pinned backing.
///
/// TX DMA remains disabled throughout this transition. All eighteen hardware
/// ring slots are inspected, must be idle, and are then pointed either at the
/// target firmware ring or a page-sized guard ring. Linux's all-ring DTX reset
/// is issued only after every base/count/CPU index is safe, then every DIDX is
/// required to read zero. Old kernel DMA bases are deliberately not restored.
pub fn prepare_global_tx_rings<T, F>(
    transport: &mut T,
    guard_iova: u64,
    fwdl_iova: u64,
    mcu_iova: u64,
    mut event: F,
) -> Result<[TxRingState; MT7921_TX_RING_SLOTS], GlobalTxRingError<T::Error>>
where
    T: GlobalTxRingTransport,
    F: FnMut(GlobalTxRingEvent),
{
    let page_end = |iova: u64| {
        iova.is_multiple_of(4096)
            .then(|| iova.checked_add(4095))
            .flatten()
            .filter(|end| *end <= u64::from(u32::MAX))
    };
    let Some(guard_end) = page_end(guard_iova) else {
        return Err(GlobalTxRingError::InvalidArena);
    };
    let Some(fwdl_end) = page_end(fwdl_iova) else {
        return Err(GlobalTxRingError::InvalidArena);
    };
    let Some(mcu_end) = page_end(mcu_iova) else {
        return Err(GlobalTxRingError::InvalidArena);
    };
    if (guard_iova <= fwdl_end && fwdl_iova <= guard_end)
        || (guard_iova <= mcu_end && mcu_iova <= guard_end)
        || (fwdl_iova <= mcu_end && mcu_iova <= fwdl_end)
    {
        return Err(GlobalTxRingError::InvalidArena);
    }
    let global_config = transport
        .read_global_config()
        .map_err(GlobalTxRingError::Transport)?;
    let interrupt_enable = transport
        .read_interrupt_enable()
        .map_err(GlobalTxRingError::Transport)?;
    if global_config == u32::MAX || interrupt_enable == u32::MAX {
        return Err(GlobalTxRingError::InvalidMmio);
    }
    if global_config & 0xf != 0 || interrupt_enable != 0 {
        return Err(GlobalTxRingError::ActiveState {
            global_config,
            interrupt_enable,
        });
    }
    for index in 0..MT7921_TX_RING_SLOTS {
        let state = transport
            .read_tx_ring(index)
            .map_err(GlobalTxRingError::Transport)?;
        if [
            state.descriptor_base,
            state.descriptor_count,
            state.cpu_index,
            state.dma_index,
        ]
        .contains(&u32::MAX)
        {
            return Err(GlobalTxRingError::InvalidMmio);
        }
        event(GlobalTxRingEvent::Snapshot { index, state });
        if state.cpu_index != 0 || state.dma_index != 0 {
            return Err(GlobalTxRingError::DirtyRing { index, state });
        }
    }
    for index in 0..MT7921_TX_RING_SLOTS {
        let descriptor_base = if index == MT7921_FWDL_RING_INDEX {
            fwdl_iova as u32
        } else if index == MT7921_MCU_TX_RING_INDEX {
            mcu_iova as u32
        } else {
            guard_iova as u32
        };
        let descriptor_count = if index == MT7921_MCU_TX_RING_INDEX {
            MT7921_MCU_TX_RING_COUNT
        } else {
            MT7921_FWDL_RING_COUNT
        };
        transport
            .write_tx_ring(index, descriptor_base, descriptor_count, 0)
            .map_err(GlobalTxRingError::Transport)?;
        event(GlobalTxRingEvent::RingOwned {
            index,
            descriptor_base,
        });
    }
    transport
        .reset_tx_indices(MT7921_RESET_ALL_TX_INDICES)
        .map_err(GlobalTxRingError::Transport)?;
    event(GlobalTxRingEvent::IndicesReset);
    let mut owned = [TxRingState {
        descriptor_base: 0,
        descriptor_count: 0,
        cpu_index: 0,
        dma_index: 0,
    }; MT7921_TX_RING_SLOTS];
    for (index, state) in owned.iter_mut().enumerate() {
        *state = transport
            .read_tx_ring(index)
            .map_err(GlobalTxRingError::Transport)?;
        let expected_base = if index == MT7921_FWDL_RING_INDEX {
            fwdl_iova as u32
        } else if index == MT7921_MCU_TX_RING_INDEX {
            mcu_iova as u32
        } else {
            guard_iova as u32
        };
        let expected_count = if index == MT7921_MCU_TX_RING_INDEX {
            MT7921_MCU_TX_RING_COUNT
        } else {
            MT7921_FWDL_RING_COUNT
        };
        if *state
            != (TxRingState {
                descriptor_base: expected_base,
                descriptor_count: expected_count,
                cpu_index: 0,
                dma_index: 0,
            })
        {
            return Err(GlobalTxRingError::Readback {
                index,
                state: *state,
            });
        }
        event(GlobalTxRingEvent::RingVerified {
            index,
            state: *state,
        });
    }
    Ok(owned)
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadCommand {
    NicPowerControl,
    GetNicCapability,
    ReadEepromBlock {
        address: u32,
    },
    PatchSemaphoreGet,
    PatchSemaphoreRelease,
    PatchFinish,
    FirmwareStart {
        address: u32,
        option: u32,
    },
    PatchStart {
        address: u32,
        length: u32,
        mode: u32,
    },
    TargetAddressLength {
        address: u32,
        length: u32,
        mode: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadCommandError {
    InvalidSequence,
    InvalidLength,
    InvalidFirmwareStart,
    InvalidEepromAddress,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DownloadResponse {
    pub length: u16,
    pub packet_type: u16,
    pub event_id: u8,
    pub sequence: u8,
    pub option: u8,
    pub extended_event_id: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadResponseError {
    Truncated,
    InvalidLength,
    SequenceMismatch { expected: u8, actual: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NicPhyCapability {
    pub ht: bool,
    pub vht: bool,
    pub has_5ghz: bool,
    pub max_bandwidth: u8,
    pub spatial_streams: u8,
    pub hardware_path: u8,
    pub he: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NicCapability {
    pub element_count: u16,
    pub mac_address: Option<[u8; 6]>,
    pub phy: Option<NicPhyCapability>,
    pub has_6ghz: Option<bool>,
    pub chip_capability: Option<u64>,
    pub unknown_elements: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NicCapabilityError {
    TruncatedHeader,
    TruncatedElementHeader { index: u16 },
    TruncatedElement { index: u16, length: u32 },
    InvalidKnownElement { index: u16, kind: u32 },
}

pub const MT7921_EEPROM_BLOCK_SIZE: usize = 16;
pub const MT7921_EEPROM_HW_TYPE: u32 = 0x55b;
pub const MT7921_EEPROM_HW_TYPE_BLOCK: u32 = 0x550;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EepromBlock {
    pub address: u32,
    pub valid: u32,
    pub data: [u8; MT7921_EEPROM_BLOCK_SIZE],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EepromBlockError {
    Truncated,
    AddressMismatch { expected: u32, actual: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EepromHardwareInfo {
    pub raw_type: u8,
    pub encapsulated_calibration: bool,
}

impl EepromBlock {
    pub fn hardware_info(&self) -> Result<EepromHardwareInfo, EepromBlockError> {
        if self.address != MT7921_EEPROM_HW_TYPE_BLOCK {
            return Err(EepromBlockError::AddressMismatch {
                expected: MT7921_EEPROM_HW_TYPE_BLOCK,
                actual: self.address,
            });
        }
        let raw_type = self.data[(MT7921_EEPROM_HW_TYPE - MT7921_EEPROM_HW_TYPE_BLOCK) as usize];
        Ok(EepromHardwareInfo {
            raw_type,
            encapsulated_calibration: raw_type & 1 != 0,
        })
    }
}

/// Parse pinned Linux `struct mt7921_mcu_eeprom_info` after the MCU RXD.
pub fn parse_eeprom_block(
    bytes: &[u8],
    expected_address: u32,
) -> Result<EepromBlock, EepromBlockError> {
    let response = bytes.get(..24).ok_or(EepromBlockError::Truncated)?;
    let address = u32::from_le_bytes(response[0..4].try_into().expect("fixed field"));
    if address != expected_address {
        return Err(EepromBlockError::AddressMismatch {
            expected: expected_address,
            actual: address,
        });
    }
    Ok(EepromBlock {
        address,
        valid: u32::from_le_bytes(response[4..8].try_into().expect("fixed field")),
        data: response[8..24].try_into().expect("fixed EEPROM block"),
    })
}

/// Parse the TLV body returned by pinned Linux GET_NIC_CAPAB.
pub fn parse_nic_capability(bytes: &[u8]) -> Result<NicCapability, NicCapabilityError> {
    let header = bytes.get(..4).ok_or(NicCapabilityError::TruncatedHeader)?;
    let element_count = u16::from_le_bytes([header[0], header[1]]);
    let mut offset = 4usize;
    let mut capability = NicCapability {
        element_count,
        mac_address: None,
        phy: None,
        has_6ghz: None,
        chip_capability: None,
        unknown_elements: 0,
    };
    for index in 0..element_count {
        let tlv = bytes
            .get(offset..offset + 8)
            .ok_or(NicCapabilityError::TruncatedElementHeader { index })?;
        let kind = u32::from_le_bytes(tlv[0..4].try_into().expect("fixed field"));
        let length = u32::from_le_bytes(tlv[4..8].try_into().expect("fixed field"));
        offset += 8;
        let end = offset
            .checked_add(length as usize)
            .filter(|end| *end <= bytes.len())
            .ok_or(NicCapabilityError::TruncatedElement { index, length })?;
        let value = &bytes[offset..end];
        offset = end;
        match kind {
            7 if value.len() >= 6 => {
                capability.mac_address = Some(value[..6].try_into().expect("checked MAC length"));
            }
            8 if value.len() >= 12 => {
                capability.phy = Some(NicPhyCapability {
                    ht: value[0] != 0,
                    vht: value[1] != 0,
                    has_5ghz: value[2] != 0,
                    max_bandwidth: value[3],
                    spatial_streams: value[4],
                    hardware_path: value[10],
                    he: value[11] != 0,
                });
            }
            0x18 if !value.is_empty() => capability.has_6ghz = Some(value[0] != 0),
            0x20 if value.len() >= 8 => {
                capability.chip_capability = Some(u64::from_le_bytes(
                    value[..8].try_into().expect("checked u64"),
                ));
            }
            7 | 8 | 0x18 | 0x20 => {
                return Err(NicCapabilityError::InvalidKnownElement { index, kind });
            }
            _ => capability.unknown_elements += 1,
        }
    }
    Ok(capability)
}

/// Parse the fixed 36-byte Connac2 MCU RX header before command-specific data.
pub fn parse_download_response(
    bytes: &[u8],
    expected_sequence: u8,
) -> Result<DownloadResponse, DownloadResponseError> {
    let header = bytes.get(..36).ok_or(DownloadResponseError::Truncated)?;
    let length = u16::from_le_bytes(header[24..26].try_into().expect("fixed field"));
    if 24usize
        .checked_add(usize::from(length))
        .is_none_or(|end| end > bytes.len())
        || length < 12
    {
        return Err(DownloadResponseError::InvalidLength);
    }
    let sequence = header[29];
    if sequence != expected_sequence {
        return Err(DownloadResponseError::SequenceMismatch {
            expected: expected_sequence,
            actual: sequence,
        });
    }
    Ok(DownloadResponse {
        length,
        packet_type: u16::from_le_bytes(header[26..28].try_into().expect("fixed field")),
        event_id: header[28],
        sequence,
        option: header[30],
        extended_event_id: header[32],
    })
}

/// Encode the non-scatter MCU command which must precede firmware DMA.
///
/// This is the exact legacy Connac2 long command header produced by pinned
/// Linux `mt76_connac2_mcu_fill_message`, followed by the little-endian request
/// payload used by patch semaphore or download initialization.
pub fn encode_download_command(
    command: DownloadCommand,
    sequence: u8,
) -> Result<Vec<u8>, DownloadCommandError> {
    if sequence == 0 || sequence > 15 {
        return Err(DownloadCommandError::InvalidSequence);
    }
    let (cid, set_query, ext_cid, ext_cid_ack, payload): (u8, u8, u8, u8, Vec<u8>) = match command {
        DownloadCommand::NicPowerControl => (0x04, 3, 0, 0, vec![1, 0, 0, 0]),
        DownloadCommand::GetNicCapability => (0x8a, 1, 0, 0, vec![]),
        DownloadCommand::ReadEepromBlock { address } => {
            if address & 0xf != 0 || address > 0x9f0 {
                return Err(DownloadCommandError::InvalidEepromAddress);
            }
            let mut payload = vec![0; 24];
            payload[..4].copy_from_slice(&address.to_le_bytes());
            (0xed, 0, 0x01, 1, payload)
        }
        DownloadCommand::PatchSemaphoreGet => (0x10, 3, 0, 0, 1u32.to_le_bytes().to_vec()),
        DownloadCommand::PatchSemaphoreRelease => (0x10, 3, 0, 0, 0u32.to_le_bytes().to_vec()),
        DownloadCommand::PatchFinish => (0x07, 3, 0, 0, vec![0; 4]),
        DownloadCommand::FirmwareStart { address, option } => {
            if address != 0x0091_5000 || option != 1 {
                return Err(DownloadCommandError::InvalidFirmwareStart);
            }
            let mut payload = Vec::with_capacity(8);
            payload.extend_from_slice(&option.to_le_bytes());
            payload.extend_from_slice(&address.to_le_bytes());
            (0x02, 3, 0, 0, payload)
        }
        DownloadCommand::PatchStart {
            address,
            length,
            mode,
        } => {
            if address != 0x0090_0000 || length == 0 {
                return Err(DownloadCommandError::InvalidLength);
            }
            let mut payload = Vec::with_capacity(12);
            payload.extend_from_slice(&address.to_le_bytes());
            payload.extend_from_slice(&length.to_le_bytes());
            payload.extend_from_slice(&mode.to_le_bytes());
            (0x05, 3, 0, 0, payload)
        }
        DownloadCommand::TargetAddressLength {
            address,
            length,
            mode,
        } => {
            if length == 0 {
                return Err(DownloadCommandError::InvalidLength);
            }
            let mut payload = Vec::with_capacity(12);
            payload.extend_from_slice(&address.to_le_bytes());
            payload.extend_from_slice(&length.to_le_bytes());
            payload.extend_from_slice(&mode.to_le_bytes());
            (0x01, 3, 0, 0, payload)
        }
    };
    let total = CONNAC2_MCU_TXD_BYTES + payload.len();
    let mut bytes = vec![0; total];
    let txd0 = (total as u32) | (2 << 23) | (0x20 << 25);
    let txd1 = (1u32 << 31) | (1 << 16);
    bytes[0..4].copy_from_slice(&txd0.to_le_bytes());
    bytes[4..8].copy_from_slice(&txd1.to_le_bytes());
    bytes[32..34].copy_from_slice(&((total - 32) as u16).to_le_bytes());
    bytes[34..36].copy_from_slice(&0x8000u16.to_le_bytes());
    bytes[36] = cid;
    bytes[37] = 0xa0;
    bytes[38] = set_query;
    bytes[39] = sequence;
    bytes[41] = ext_cid;
    bytes[43] = ext_cid_ack;
    bytes[CONNAC2_MCU_TXD_BYTES..].copy_from_slice(&payload);
    Ok(bytes)
}

pub const FIRMWARE_POLL_INTERVAL_MS: u64 = 10;
pub const DOWNLOAD_READY_TIMEOUT_MS: u64 = 1000;
pub const N9_READY_TIMEOUT_MS: u64 = 1500;
/// A fail-closed per-chunk safety deadline. Pinned PCI Linux uses a 3-second
/// MCU timeout and requests no FW_SCATTER response; this offline model is
/// deliberately stricter by requiring synchronous TX completion per chunk.
pub const SCATTER_COMPLETION_TIMEOUT_MS: u64 = 3000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareLoaderState {
    Powering,
    DownloadReady,
    PatchSemaphoreHeld,
    PatchSemaphoreReleased,
    PatchComplete,
    RamDownloading,
    FirmwareStarted,
    N9Ready,
    CapabilityDiscovered,
    EepromDiscovered,
    Ready,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareImagePart {
    Patch,
    Ram,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareLoaderOperation {
    Command(DownloadCommand),
    PublishScatter(FirmwareImagePart),
    WaitScatterCompletion(FirmwareImagePart),
    PollDownloadReady,
    PollN9Ready,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareCommandCompletion {
    NoResponse,
    Ack,
    PatchSemaphore(PatchSemaphoreStatus),
    PatchFinish(u8),
    NicCapability(NicCapability),
    EepromBlock(EepromBlock),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatchSemaphoreStatus {
    NotDownloadedFailed,
    AlreadyDownloaded,
    Acquired,
    Released,
    Other(u8),
}

impl From<u8> for PatchSemaphoreStatus {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::NotDownloadedFailed,
            1 => Self::AlreadyDownloaded,
            2 => Self::Acquired,
            3 => Self::Released,
            value => Self::Other(value),
        }
    }
}

/// Transport boundary for the complete loader transaction. Implementations
/// own command/RX matching and one completion for every scatter chunk.
pub trait FirmwareLoaderTransport {
    type Error;

    /// Allocate the next persistent nonzero four-bit MCU sequence. Linux keeps
    /// this counter on the device and consumes a value for scatter messages.
    fn next_sequence(&mut self) -> u8;
    fn command(
        &mut self,
        command: DownloadCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<FirmwareCommandCompletion, Self::Error>;
    fn publish_scatter(
        &mut self,
        part: FirmwareImagePart,
        sequence: u8,
        chunk: &[u8],
    ) -> Result<(), Self::Error>;
    fn wait_scatter_completion(
        &mut self,
        part: FirmwareImagePart,
        sequence: u8,
        deadline_ms: u64,
    ) -> Result<(), Self::Error>;
    fn firmware_download_state(&mut self) -> Result<u8, Self::Error>;
    fn firmware_n9_ready(&mut self) -> Result<bool, Self::Error>;
    fn now_ms(&self) -> u64;
    fn sleep_ms(&mut self, duration_ms: u64);
    /// Quiesce DMA/IRQ activity and revoke all loader resources. `state` is
    /// the last successfully entered protocol state, not cleanup permission.
    fn fail_closed_cleanup(&mut self, state: FirmwareLoaderState) -> Result<(), Self::Error>;
}

#[derive(Debug, Eq, PartialEq)]
pub enum FirmwareLoaderFailure<E> {
    Command(DownloadCommandError),
    PatchSecurity(PatchSecurityError),
    Transport {
        operation: FirmwareLoaderOperation,
        source: E,
    },
    UnexpectedCommandCompletion {
        command: DownloadCommand,
        completion: FirmwareCommandCompletion,
    },
    UnexpectedPatchSemaphore(PatchSemaphoreStatus),
    UnexpectedPatchRelease(PatchSemaphoreStatus),
    UnexpectedPatchFinish(u8),
    PatchRelease {
        primary: Option<Box<FirmwareLoaderFailure<E>>>,
        release: Box<FirmwareLoaderFailure<E>>,
    },
    MissingFirmwareOverride,
    N9ReadyTimeout,
    Clc(ClcDiscoveryError),
}

#[derive(Debug, Eq, PartialEq)]
pub enum FirmwareLoaderError<E> {
    Failed(FirmwareLoaderFailure<E>),
    Cleanup {
        failure: Option<FirmwareLoaderFailure<E>>,
        source: E,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatchDisposition {
    AlreadyDownloaded,
    Downloaded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareLoaderReport {
    pub download_ready_observed: bool,
    pub patch: PatchDisposition,
    pub patch_sections: usize,
    pub ram_regions: usize,
    pub scatter_chunks: usize,
    pub scatter_bytes: usize,
    pub nic_capability: NicCapability,
    pub eeprom_hardware: EepromBlock,
    pub clc: ClcDiscovery,
}

fn loader_command<T: FirmwareLoaderTransport>(
    transport: &mut T,
    command: DownloadCommand,
) -> Result<FirmwareCommandCompletion, FirmwareLoaderFailure<T::Error>> {
    let sequence = next_loader_sequence(transport)?;
    let encoded =
        encode_download_command(command, sequence).map_err(FirmwareLoaderFailure::Command)?;
    transport
        .command(command, sequence, &encoded)
        .map_err(|source| FirmwareLoaderFailure::Transport {
            operation: FirmwareLoaderOperation::Command(command),
            source,
        })
}

fn next_loader_sequence<T: FirmwareLoaderTransport>(
    transport: &mut T,
) -> Result<u8, FirmwareLoaderFailure<T::Error>> {
    let sequence = transport.next_sequence();
    if sequence == 0 || sequence > 15 {
        Err(FirmwareLoaderFailure::Command(
            DownloadCommandError::InvalidSequence,
        ))
    } else {
        Ok(sequence)
    }
}

fn loader_scatter<T: FirmwareLoaderTransport>(
    transport: &mut T,
    part: FirmwareImagePart,
    payload: &[u8],
    report: &mut FirmwareLoaderReport,
) -> Result<(), FirmwareLoaderFailure<T::Error>> {
    for chunk in payload.chunks(MT7921_FWDL_CHUNK_BYTES) {
        let sequence = next_loader_sequence(transport)?;
        transport
            .publish_scatter(part, sequence, chunk)
            .map_err(|source| FirmwareLoaderFailure::Transport {
                operation: FirmwareLoaderOperation::PublishScatter(part),
                source,
            })?;
        let deadline_ms = transport
            .now_ms()
            .saturating_add(SCATTER_COMPLETION_TIMEOUT_MS);
        transport
            .wait_scatter_completion(part, sequence, deadline_ms)
            .map_err(|source| FirmwareLoaderFailure::Transport {
                operation: FirmwareLoaderOperation::WaitScatterCompletion(part),
                source,
            })?;
        report.scatter_chunks += 1;
        report.scatter_bytes += chunk.len();
    }
    Ok(())
}

fn expect_loader_completion<E>(
    command: DownloadCommand,
    completion: FirmwareCommandCompletion,
    expected: FirmwareCommandCompletion,
) -> Result<(), FirmwareLoaderFailure<E>> {
    if completion == expected {
        Ok(())
    } else {
        Err(FirmwareLoaderFailure::UnexpectedCommandCompletion {
            command,
            completion,
        })
    }
}

fn run_firmware_loader<T: FirmwareLoaderTransport>(
    transport: &mut T,
    patch: Patch<'_>,
    firmware: Firmware<'_>,
    state: &mut FirmwareLoaderState,
) -> Result<FirmwareLoaderReport, FirmwareLoaderFailure<T::Error>> {
    let mut report = FirmwareLoaderReport {
        download_ready_observed: false,
        patch: PatchDisposition::Downloaded,
        patch_sections: 0,
        ram_regions: 0,
        scatter_chunks: 0,
        scatter_bytes: 0,
        nic_capability: NicCapability {
            element_count: 0,
            mac_address: None,
            phy: None,
            has_6ghz: None,
            chip_capability: None,
            unknown_elements: 0,
        },
        eeprom_hardware: EepromBlock {
            address: MT7921_EEPROM_HW_TYPE_BLOCK,
            valid: 0,
            data: [0; MT7921_EEPROM_BLOCK_SIZE],
        },
        clc: ClcDiscovery::default(),
    };

    let power = DownloadCommand::NicPowerControl;
    let completion = loader_command(transport, power)?;
    expect_loader_completion(power, completion, FirmwareCommandCompletion::NoResponse)?;
    let download_deadline = transport.now_ms().saturating_add(DOWNLOAD_READY_TIMEOUT_MS);
    loop {
        let firmware_state = transport.firmware_download_state().map_err(|source| {
            FirmwareLoaderFailure::Transport {
                operation: FirmwareLoaderOperation::PollDownloadReady,
                source,
            }
        })?;
        if firmware_state == 1 {
            *state = FirmwareLoaderState::DownloadReady;
            report.download_ready_observed = true;
            break;
        }
        if transport.now_ms() >= download_deadline {
            // Pinned Linux warns and continues into patch semaphore handling.
            break;
        }
        transport.sleep_ms(FIRMWARE_POLL_INTERVAL_MS);
    }

    let get = DownloadCommand::PatchSemaphoreGet;
    match loader_command(transport, get)? {
        FirmwareCommandCompletion::PatchSemaphore(PatchSemaphoreStatus::AlreadyDownloaded) => {
            report.patch = PatchDisposition::AlreadyDownloaded
        }
        FirmwareCommandCompletion::PatchSemaphore(PatchSemaphoreStatus::Acquired) => {
            *state = FirmwareLoaderState::PatchSemaphoreHeld;
            let patch_result = (|| {
                for section in patch.sections() {
                    let mode = patch_download_mode(section.security_info)
                        .map_err(FirmwareLoaderFailure::PatchSecurity)?;
                    let command = DownloadCommand::PatchStart {
                        address: section.address,
                        length: section.payload.len() as u32,
                        mode,
                    };
                    let completion = loader_command(transport, command)?;
                    expect_loader_completion(command, completion, FirmwareCommandCompletion::Ack)?;
                    loader_scatter(
                        transport,
                        FirmwareImagePart::Patch,
                        section.payload,
                        &mut report,
                    )?;
                    report.patch_sections += 1;
                }
                let finish = DownloadCommand::PatchFinish;
                match loader_command(transport, finish)? {
                    FirmwareCommandCompletion::PatchFinish(0) => {}
                    FirmwareCommandCompletion::PatchFinish(status) => {
                        return Err(FirmwareLoaderFailure::UnexpectedPatchFinish(status));
                    }
                    completion => {
                        return Err(FirmwareLoaderFailure::UnexpectedCommandCompletion {
                            command: finish,
                            completion,
                        });
                    }
                }
                Ok(())
            })();
            let primary = patch_result.err();
            let release = loader_command(transport, DownloadCommand::PatchSemaphoreRelease);
            let release_failure = match release {
                Ok(FirmwareCommandCompletion::PatchSemaphore(PatchSemaphoreStatus::Released)) => {
                    None
                }
                Ok(FirmwareCommandCompletion::PatchSemaphore(result)) => {
                    Some(FirmwareLoaderFailure::UnexpectedPatchRelease(result))
                }
                Ok(completion) => Some(FirmwareLoaderFailure::UnexpectedCommandCompletion {
                    command: DownloadCommand::PatchSemaphoreRelease,
                    completion,
                }),
                Err(error) => Some(error),
            };
            if let Some(release) = release_failure {
                return Err(FirmwareLoaderFailure::PatchRelease {
                    primary: primary.map(Box::new),
                    release: Box::new(release),
                });
            }
            *state = FirmwareLoaderState::PatchSemaphoreReleased;
            if let Some(primary) = primary {
                return Err(primary);
            }
        }
        FirmwareCommandCompletion::PatchSemaphore(result) => {
            return Err(FirmwareLoaderFailure::UnexpectedPatchSemaphore(result));
        }
        completion => {
            return Err(FirmwareLoaderFailure::UnexpectedCommandCompletion {
                command: get,
                completion,
            });
        }
    }
    *state = FirmwareLoaderState::PatchComplete;

    let mut override_address = 0;
    *state = FirmwareLoaderState::RamDownloading;
    for region in firmware.regions() {
        if !region.is_downloadable() {
            continue;
        }
        if region.feature_set & (1 << 5) != 0 {
            override_address = region.address;
        }
        let command = DownloadCommand::TargetAddressLength {
            address: region.address,
            length: region.payload.len() as u32,
            mode: firmware_download_mode(region.feature_set, false),
        };
        let completion = loader_command(transport, command)?;
        expect_loader_completion(command, completion, FirmwareCommandCompletion::Ack)?;
        loader_scatter(
            transport,
            FirmwareImagePart::Ram,
            region.payload,
            &mut report,
        )?;
        report.ram_regions += 1;
    }
    if override_address == 0 {
        return Err(FirmwareLoaderFailure::MissingFirmwareOverride);
    }
    let start = DownloadCommand::FirmwareStart {
        address: override_address,
        option: 1,
    };
    let completion = loader_command(transport, start)?;
    expect_loader_completion(start, completion, FirmwareCommandCompletion::Ack)?;
    *state = FirmwareLoaderState::FirmwareStarted;
    let n9_deadline = transport.now_ms().saturating_add(N9_READY_TIMEOUT_MS);
    loop {
        if transport
            .firmware_n9_ready()
            .map_err(|source| FirmwareLoaderFailure::Transport {
                operation: FirmwareLoaderOperation::PollN9Ready,
                source,
            })?
        {
            *state = FirmwareLoaderState::N9Ready;
            break;
        }
        if transport.now_ms() >= n9_deadline {
            return Err(FirmwareLoaderFailure::N9ReadyTimeout);
        }
        transport.sleep_ms(FIRMWARE_POLL_INTERVAL_MS);
    }
    let capability_command = DownloadCommand::GetNicCapability;
    match loader_command(transport, capability_command)? {
        FirmwareCommandCompletion::NicCapability(capability) => {
            report.nic_capability = capability;
            *state = FirmwareLoaderState::CapabilityDiscovered;
        }
        completion => {
            return Err(FirmwareLoaderFailure::UnexpectedCommandCompletion {
                command: capability_command,
                completion,
            });
        }
    }
    let eeprom_command = DownloadCommand::ReadEepromBlock {
        address: MT7921_EEPROM_HW_TYPE_BLOCK,
    };
    match loader_command(transport, eeprom_command)? {
        FirmwareCommandCompletion::EepromBlock(block) => {
            report.eeprom_hardware = block;
            *state = FirmwareLoaderState::EepromDiscovered;
            report.clc = discover_clc(
                firmware,
                block
                    .hardware_info()
                    .expect("the fixed EEPROM hardware block was validated"),
            )
            .map_err(FirmwareLoaderFailure::Clc)?;
            *state = FirmwareLoaderState::Ready;
            Ok(report)
        }
        completion => Err(FirmwareLoaderFailure::UnexpectedCommandCompletion {
            command: eeprom_command,
            completion,
        }),
    }
}

/// Execute the bounded Linux MT7921 patch + RAM loading sequence. Cleanup is
/// mandatory after both success and failure; release of an acquired patch
/// semaphore is always attempted before fail-closed cleanup.
pub fn load_mt7921_firmware<T: FirmwareLoaderTransport>(
    transport: &mut T,
    patch: Patch<'_>,
    firmware: Firmware<'_>,
) -> Result<FirmwareLoaderReport, FirmwareLoaderError<T::Error>> {
    let mut state = FirmwareLoaderState::Powering;
    let result = run_firmware_loader(transport, patch, firmware, &mut state);
    match (result, transport.fail_closed_cleanup(state)) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(failure), Ok(())) => Err(FirmwareLoaderError::Failed(failure)),
        (Ok(_), Err(source)) => Err(FirmwareLoaderError::Cleanup {
            failure: None,
            source,
        }),
        (Err(failure), Err(source)) => Err(FirmwareLoaderError::Cleanup {
            failure: Some(failure),
            source,
        }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PciIrqKind {
    Intx,
    Msi,
    Msix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciIrqCapability {
    pub kind: PciIrqKind,
    pub count: u32,
    pub eventfd: bool,
}

/// Select the same interrupt preference used by PCI drivers without admitting
/// an interrupt source which cannot be drained through an installed eventfd.
pub fn select_vfio_irq(capabilities: &[PciIrqCapability]) -> Option<PciIrqCapability> {
    [PciIrqKind::Msix, PciIrqKind::Msi, PciIrqKind::Intx]
        .into_iter()
        .find_map(|kind| {
            capabilities.iter().copied().find(|capability| {
                capability.kind == kind && capability.count != 0 && capability.eventfd
            })
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqLifecycle {
    Uninstalled,
    EventfdInstalled(PciIrqCapability),
    DeviceSourceEnabled(PciIrqCapability),
    EventObserved(PciIrqCapability),
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqLifecycleError {
    InvalidTransition,
    EmptyEvent,
}

impl IrqLifecycle {
    pub fn install(self, capability: PciIrqCapability) -> Result<Self, IrqLifecycleError> {
        if self != Self::Uninstalled || capability.count == 0 || !capability.eventfd {
            return Err(IrqLifecycleError::InvalidTransition);
        }
        Ok(Self::EventfdInstalled(capability))
    }

    pub fn enable_device_source(self) -> Result<Self, IrqLifecycleError> {
        match self {
            Self::EventfdInstalled(capability) => Ok(Self::DeviceSourceEnabled(capability)),
            _ => Err(IrqLifecycleError::InvalidTransition),
        }
    }

    pub fn observe_event(self, counter: u64) -> Result<Self, IrqLifecycleError> {
        if counter == 0 {
            return Err(IrqLifecycleError::EmptyEvent);
        }
        match self {
            Self::DeviceSourceEnabled(capability) => Ok(Self::EventObserved(capability)),
            _ => Err(IrqLifecycleError::InvalidTransition),
        }
    }

    pub fn disable(self) -> Result<Self, IrqLifecycleError> {
        match self {
            Self::EventfdInstalled(_) | Self::DeviceSourceEnabled(_) | Self::EventObserved(_) => {
                Ok(Self::Disabled)
            }
            _ => Err(IrqLifecycleError::InvalidTransition),
        }
    }

    pub const fn may_unmask_device(self) -> bool {
        matches!(self, Self::EventfdInstalled(_))
    }
}

pub const PINNED_DMA_QUIESCE_MS: u64 = 100;

pub trait PinnedDmaTeardownTransport {
    type Error;
    fn now_ms(&self) -> u64;
    fn mask_device_interrupts(&mut self) -> Result<(), Self::Error>;
    fn disable_dma(&mut self) -> Result<(), Self::Error>;
    fn read_global_config(&mut self) -> Result<u32, Self::Error>;
    fn sleep_ms(&mut self, milliseconds: u64);
    fn reset_vfio_device(&mut self) -> Result<(), Self::Error>;
    fn unmap_all(&mut self) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinnedDmaTeardownEvent {
    InterruptsMasked,
    DmaDisabled,
    DmaQuiesced,
    DmaBusyTimedOut { raw: u32 },
    DeviceReset,
    MappingsReleased,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinnedDmaTeardownError<E> {
    Cleanup(E),
    Reset(E),
    Unmap(E),
    BusyTimedOut(u32),
}

/// Revoke active DMA without ever exposing an unmapped IOVA to the device.
///
/// A function reset is mandatory while every mapping remains pinned, even if
/// TX busy clears normally. If busy never clears, reset is the only transition
/// which permits unmapping. A failed reset returns without calling `unmap_all`,
/// leaving the process and external reboot watchdog as the final containment.
pub fn teardown_pinned_dma<T, F>(
    transport: &mut T,
    mut event: F,
) -> Result<(), PinnedDmaTeardownError<T::Error>>
where
    T: PinnedDmaTeardownTransport,
    F: FnMut(PinnedDmaTeardownEvent),
{
    let mut cleanup_error = None;
    match transport.mask_device_interrupts() {
        Ok(()) => event(PinnedDmaTeardownEvent::InterruptsMasked),
        Err(error) => cleanup_error = Some(error),
    }
    match transport.disable_dma() {
        Ok(()) => event(PinnedDmaTeardownEvent::DmaDisabled),
        Err(error) if cleanup_error.is_none() => cleanup_error = Some(error),
        Err(_) => {}
    }
    let deadline = transport.now_ms().saturating_add(PINNED_DMA_QUIESCE_MS);
    let mut busy_timeout = None;
    loop {
        match transport.read_global_config() {
            Ok(raw) if raw & ((1 << 1) | (1 << 3)) == 0 => {
                event(PinnedDmaTeardownEvent::DmaQuiesced);
                break;
            }
            Ok(raw) if transport.now_ms() >= deadline => {
                busy_timeout = Some(raw);
                event(PinnedDmaTeardownEvent::DmaBusyTimedOut { raw });
                break;
            }
            Ok(_) => transport.sleep_ms(1),
            Err(error) => {
                if cleanup_error.is_none() {
                    cleanup_error = Some(error);
                }
                break;
            }
        }
    }
    transport
        .reset_vfio_device()
        .map_err(PinnedDmaTeardownError::Reset)?;
    event(PinnedDmaTeardownEvent::DeviceReset);
    transport
        .unmap_all()
        .map_err(PinnedDmaTeardownError::Unmap)?;
    event(PinnedDmaTeardownEvent::MappingsReleased);
    if let Some(error) = cleanup_error {
        return Err(PinnedDmaTeardownError::Cleanup(error));
    }
    if let Some(raw) = busy_timeout {
        return Err(PinnedDmaTeardownError::BusyTimedOut(raw));
    }
    Ok(())
}

pub const WFSYS_SW_RST_B: u32 = 1 << 0;
pub const WFSYS_SW_INIT_DONE: u32 = 1 << 4;
pub const WFSYS_ASSERT_MS: u64 = 50;
pub const WFSYS_READY_DEADLINE_MS: u64 = 500;

pub trait WfsysResetTransport {
    type Error;
    fn now_ms(&self) -> u64;
    fn read_reset_control(&mut self) -> Result<u32, Self::Error>;
    fn write_reset_control(&mut self, value: u32) -> Result<(), Self::Error>;
    fn sleep_ms(&mut self, milliseconds: u64);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WfsysResetEvent {
    Snapshot { raw: u32 },
    Asserted { raw: u32, at_ms: u64 },
    Released { raw: u32, at_ms: u64 },
    StatusRead { raw: u32, at_ms: u64 },
    Ready { raw: u32, at_ms: u64 },
    TimedOut { raw: u32, at_ms: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WfsysResetError<E> {
    Transport(E),
    ClockOverflow,
    Timeout,
}

/// Port pinned Linux `mt792x_wfsys_reset` without its kernel runtime.
///
/// The concrete MT7921 target is physical address 0x18000140 and therefore
/// requires the same bounded dynamic-L1 mechanism before a physical adapter is
/// admitted. This portable state machine only owns the exact bit sequence and
/// deadlines; it cannot select or access any register itself.
pub fn reset_wfsys<T, F>(transport: &mut T, mut event: F) -> Result<(), WfsysResetError<T::Error>>
where
    T: WfsysResetTransport,
    F: FnMut(WfsysResetEvent),
{
    let start = transport.now_ms();
    let initial = transport
        .read_reset_control()
        .map_err(WfsysResetError::Transport)?;
    event(WfsysResetEvent::Snapshot { raw: initial });
    let asserted = initial & !WFSYS_SW_RST_B;
    transport
        .write_reset_control(asserted)
        .map_err(WfsysResetError::Transport)?;
    event(WfsysResetEvent::Asserted {
        raw: asserted,
        at_ms: 0,
    });
    transport.sleep_ms(WFSYS_ASSERT_MS);
    let released = asserted | WFSYS_SW_RST_B;
    transport
        .write_reset_control(released)
        .map_err(WfsysResetError::Transport)?;
    event(WfsysResetEvent::Released {
        raw: released,
        at_ms: transport.now_ms().saturating_sub(start),
    });
    let deadline = transport
        .now_ms()
        .checked_add(WFSYS_READY_DEADLINE_MS)
        .ok_or(WfsysResetError::ClockOverflow)?;
    loop {
        let raw = transport
            .read_reset_control()
            .map_err(WfsysResetError::Transport)?;
        let now = transport.now_ms();
        event(WfsysResetEvent::StatusRead {
            raw,
            at_ms: now.saturating_sub(start),
        });
        if raw & WFSYS_SW_INIT_DONE != 0 {
            event(WfsysResetEvent::Ready {
                raw,
                at_ms: now.saturating_sub(start),
            });
            return Ok(());
        }
        if now >= deadline {
            event(WfsysResetEvent::TimedOut {
                raw,
                at_ms: now.saturating_sub(start),
            });
            return Err(WfsysResetError::Timeout);
        }
        transport.sleep_ms(DRIVER_OWN_POLL_MS.min(deadline - now));
    }
}

impl ReadOnlyStatus {
    pub const fn decode(conn_misc: u32, low_power: u32, wfdma_config: u32) -> Self {
        Self {
            firmware_powered: conn_misc & 1 != 0,
            firmware_n9_ready: conn_misc & 3 == 3,
            firmware_owns_device: low_power & 4 != 0,
            tx_dma_enabled: wfdma_config & 1 != 0,
            tx_dma_busy: wfdma_config & 2 != 0,
            rx_dma_enabled: wfdma_config & 4 != 0,
            rx_dma_busy: wfdma_config & 8 != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;
    use std::vec::Vec;

    #[test]
    fn ports_fuchsia_beacon_conversion_fixture() {
        // SSID, rates, DSSS channel and RSNE copied from pinned Fuchsia
        // mlme/rust/src/client/convert_beacon.rs::beacon_frame_ies.
        let ies = [
            0x00, 0x08, b'f', b'o', b'o', b'-', b's', b's', b'i', b'd', 0x01, 0x04, 0xb0, 0x48,
            0x60, 0x6c, 0x03, 0x01, 140, 0x30, 0x14, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x04, 0x01,
            0x00, 0x00, 0x0f, 0xac, 0x04, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x01, 0x28, 0x00, 0x2d,
            0x01, 0x00, 0xbf, 0x01, 0x00,
        ];
        let ap = AccessPoint::from_beacon([0x33; 6], 0x1111, &ies, 5700, -40).unwrap();
        assert_eq!(ap.ssid.as_slice(), b"foo-ssid");
        assert_eq!(ap.channel, 140);
        assert_eq!(ap.security, Security::Wpa2Enterprise);
        assert!(ap.ht && ap.vht);
    }

    #[test]
    fn rejects_truncated_ies_and_classifies_modern_akms() {
        assert_eq!(
            AccessPoint::from_beacon([0; 6], 0, &[0, 3, b'a'], 2412, -1),
            Err(AdvertisementError::TruncatedElement)
        );
        let sae = [
            0x30, 0x12, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 8,
        ];
        assert_eq!(
            AccessPoint::from_beacon([0; 6], 0x10, &sae, 5955, -30)
                .unwrap()
                .security,
            Security::Wpa3Personal
        );
        assert_eq!(frequency_to_channel(2412), 1);
        assert_eq!(frequency_to_channel(2484), 14);
        assert_eq!(frequency_to_channel(5955), 1);
    }

    #[test]
    fn read_only_register_policy_matches_pinned_linux_mt7921_map() {
        assert_eq!(ReadRegister::McuCommand.bar_offset(), 0xd4000 + 0x1f0);
        assert_eq!(
            ReadRegister::HostInterruptStatus.bar_offset(),
            0xd4000 + 0x200
        );
        assert_eq!(
            ReadRegister::WfdmaGlobalConfig.bar_offset(),
            0xd4000 + 0x208
        );
        assert_eq!(
            ReadRegister::ConnOnLowPowerControl.bar_offset(),
            0xe0000 + 0x10
        );
        assert_eq!(ReadRegister::ConnOnMisc.bar_offset(), 0xe0000 + 0xf0);
        assert!(ReadRegister::ALL.windows(2).all(|pair| pair[0] != pair[1]));

        assert_eq!(
            ReadOnlyStatus::decode(3, 4, 0b1111),
            ReadOnlyStatus {
                firmware_powered: true,
                firmware_n9_ready: true,
                firmware_owns_device: true,
                tx_dma_enabled: true,
                tx_dma_busy: true,
                rx_dma_enabled: true,
                rx_dma_busy: true,
            }
        );
    }

    struct FakeOwnership {
        now: u64,
        status: u32,
        clear_after_writes: Option<u8>,
        writes: u8,
    }
    impl OwnershipTransport for FakeOwnership {
        type Error = ();
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn write_clear_own(&mut self) -> Result<(), Self::Error> {
            self.writes += 1;
            if self.clear_after_writes == Some(self.writes) {
                self.status &= !PCIE_LPCR_HOST_OWN_SYNC;
            }
            Ok(())
        }
        fn read_low_power_control(&mut self) -> Result<u32, Self::Error> {
            Ok(self.status)
        }
        fn sleep_ms(&mut self, milliseconds: u64) {
            self.now += milliseconds;
        }
    }

    #[test]
    fn driver_ownership_succeeds_and_logs_each_transition() {
        let mut transport = FakeOwnership {
            now: 10,
            status: PCIE_LPCR_HOST_OWN_SYNC,
            clear_after_writes: Some(2),
            writes: 0,
        };
        let mut events = Vec::new();
        acquire_driver_ownership(&mut transport, |event| events.push(event)).unwrap();
        assert_eq!(transport.writes, 2);
        assert!(events.contains(&OwnershipEvent::AttemptExpired {
            attempt: 1,
            at_ms: 50
        }));
        assert_eq!(
            events.last(),
            Some(&OwnershipEvent::Acquired {
                attempt: 2,
                at_ms: 50
            })
        );
    }

    #[test]
    fn driver_ownership_times_out_at_hard_deadline() {
        let mut transport = FakeOwnership {
            now: 0,
            status: PCIE_LPCR_HOST_OWN_SYNC,
            clear_after_writes: None,
            writes: 0,
        };
        let mut events = Vec::new();
        assert_eq!(
            acquire_driver_ownership(&mut transport, |event| events.push(event)),
            Err(OwnershipError::Timeout)
        );
        assert_eq!(transport.writes, DRIVER_OWN_ATTEMPTS);
        assert_eq!(transport.now, DRIVER_OWN_HARD_DEADLINE_MS);
        assert_eq!(
            events.last(),
            Some(&OwnershipEvent::TimedOut {
                at_ms: DRIVER_OWN_HARD_DEADLINE_MS
            })
        );
    }

    #[test]
    fn driver_ownership_rejects_command_bits_on_readback() {
        let mut transport = FakeOwnership {
            now: 0,
            status: PCIE_LPCR_HOST_CLR_OWN,
            clear_after_writes: None,
            writes: 0,
        };
        let mut events = Vec::new();
        assert_eq!(
            acquire_driver_ownership(&mut transport, |event| events.push(event)),
            Err(OwnershipError::UnexpectedState(PCIE_LPCR_HOST_CLR_OWN))
        );
        assert_eq!(transport.writes, 1);
        assert_eq!(
            events.last(),
            Some(&OwnershipEvent::UnexpectedState {
                attempt: 1,
                at_ms: 0,
                raw: PCIE_LPCR_HOST_CLR_OWN
            })
        );
    }

    #[derive(Default)]
    struct FakeMemory {
        allocation: Option<RingAllocation>,
        freed: Vec<RingAllocation>,
    }
    impl Low32RingMemory for FakeMemory {
        type Error = ();
        fn allocate_low32(&mut self, size: usize, _: usize) -> Result<RingAllocation, Self::Error> {
            Ok(self.allocation.unwrap_or(RingAllocation {
                id: 1,
                iova: 0x1000_0000,
                len: size,
            }))
        }
        fn free(&mut self, allocation: RingAllocation) {
            self.freed.push(allocation);
        }
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum PublishEvent {
        Descriptor(u16),
        Release,
        Producer(u16),
        Acquire,
    }
    #[derive(Default)]
    struct FakePublisher(Vec<PublishEvent>);
    impl RingPublisher for FakePublisher {
        type Error = ();
        fn write_descriptor(&mut self, index: u16, _: DmaDescriptor) -> Result<(), Self::Error> {
            self.0.push(PublishEvent::Descriptor(index));
            Ok(())
        }
        fn release_fence(&mut self) {
            self.0.push(PublishEvent::Release)
        }
        fn publish_producer(&mut self, index: u16) -> Result<(), Self::Error> {
            self.0.push(PublishEvent::Producer(index));
            Ok(())
        }
        fn acquire_fence(&mut self) {
            self.0.push(PublishEvent::Acquire)
        }
    }

    #[test]
    fn inactive_wfdma_ring_orders_publish_reclaim_and_teardown() {
        let mut memory = FakeMemory::default();
        let mut ring = WfdmaRing::allocate(&mut memory, 2).unwrap();
        assert_eq!(ring.allocation().iova, 0x1000_0000);
        let mut publisher = FakePublisher::default();
        assert_eq!(
            ring.enqueue(
                &mut publisher,
                DmaSegment {
                    iova: 0x2000_0000,
                    len: 64
                },
                None,
                7,
            ),
            Ok(0)
        );
        assert_eq!(
            publisher.0,
            [
                PublishEvent::Descriptor(0),
                PublishEvent::Release,
                PublishEvent::Producer(1)
            ]
        );
        assert_eq!(ring.reclaim_one(&mut publisher), None);
        assert!(ring.complete(0));
        assert_eq!(ring.reclaim_one(&mut publisher), Some(0));
        assert_eq!(publisher.0.last(), Some(&PublishEvent::Acquire));
        ring.teardown(&mut memory);
        assert_eq!(memory.freed.len(), 1);
    }

    #[test]
    fn inactive_wfdma_ring_rejects_high_or_wrapping_allocations() {
        let mut memory = FakeMemory {
            allocation: Some(RingAllocation {
                id: 2,
                iova: 0xffff_fff0,
                len: 32,
            }),
            ..Default::default()
        };
        assert!(matches!(
            WfdmaRing::allocate(&mut memory, 2),
            Err(RingError::AllocationAbove32Bits)
        ));
        assert_eq!(memory.freed.len(), 1);
    }

    fn patch_image(section_type: u32, offset: u32, length: u32) -> Vec<u8> {
        let mut bytes = vec![0; PATCH_HEADER_LEN + PATCH_SECTION_LEN + length as usize];
        bytes[0..16].copy_from_slice(b"20260101-120000\0");
        bytes[16..20].copy_from_slice(b"ALPS");
        bytes[20..24].copy_from_slice(&0x8a10_8a10u32.to_be_bytes());
        bytes[44..48].copy_from_slice(&1u32.to_be_bytes());
        let section = PATCH_HEADER_LEN;
        bytes[section..section + 4].copy_from_slice(&section_type.to_be_bytes());
        bytes[section + 4..section + 8].copy_from_slice(&offset.to_be_bytes());
        bytes[section + 12..section + 16].copy_from_slice(&0x0090_0000u32.to_be_bytes());
        bytes[section + 16..section + 20].copy_from_slice(&length.to_be_bytes());
        bytes
    }

    #[test]
    fn parses_connac2_patch_header_and_bounded_sections() {
        let bytes = patch_image(0x0004_0002, 160, 4);
        let patch = Patch::parse(&bytes).unwrap();
        assert_eq!(patch.header.platform, b"ALPS");
        assert_eq!(patch.header.hardware_software_version, 0x8a10_8a10);
        assert_eq!(patch.region_count(), 1);
        let section = patch.sections().next().unwrap();
        assert_eq!(section.address, 0x0090_0000);
        assert_eq!(section.payload.len(), 4);

        assert_eq!(
            Patch::parse(&patch_image(1, 160, 4)),
            Err(PatchError::UnsupportedSectionType)
        );
        assert_eq!(
            Patch::parse(&patch_image(2, 200, 4)),
            Err(PatchError::PayloadOutOfBounds)
        );
    }

    #[derive(Default)]
    struct FakeL1 {
        selector: u32,
        writes: Vec<u32>,
        fail_window: Option<u16>,
    }
    impl DynamicL1Transport for FakeL1 {
        type Error = u16;
        fn read_selector(&mut self) -> Result<u32, Self::Error> {
            Ok(self.selector)
        }
        fn write_selector(&mut self, value: u32) -> Result<(), Self::Error> {
            self.selector = value;
            self.writes.push(value);
            Ok(())
        }
        fn read_window(&mut self, offset: u16) -> Result<u32, Self::Error> {
            if self.fail_window == Some(offset) {
                return Err(offset);
            }
            Ok((self.selector & 0xffff) << 16 | u32::from(offset))
        }
    }

    #[test]
    fn dynamic_l1_reads_only_fixed_targets_and_restores_selector() {
        let mut transport = FakeL1 {
            selector: 0xabcd_1234,
            ..Default::default()
        };
        let mut events = Vec::new();
        let status =
            read_dynamic_identity_status(&mut transport, |event| events.push(event)).unwrap();
        assert_eq!(status.chip_id, 0x7001_0200);
        assert_eq!(status.revision, 0x7001_0204);
        assert_eq!(status.hardware_bound, 0x7001_0020);
        assert_eq!(status.top_low_power_control, 0x1806_0010);
        assert_eq!(transport.selector, 0xabcd_1234);
        assert_eq!(transport.writes, [0xabcd_7001, 0xabcd_1806, 0xabcd_1234]);
        assert_eq!(
            events.last(),
            Some(&DynamicL1Event::SelectorRestored { raw: 0xabcd_1234 })
        );
    }

    #[test]
    fn dynamic_l1_restores_selector_after_window_failure() {
        let mut transport = FakeL1 {
            selector: 0x55aa_4321,
            fail_window: Some(0x0204),
            ..Default::default()
        };
        let mut events = Vec::new();
        assert_eq!(
            read_dynamic_identity_status(&mut transport, |event| events.push(event)),
            Err(DynamicL1Error::Transport(0x0204))
        );
        assert_eq!(transport.selector, 0x55aa_4321);
        assert_eq!(transport.writes.last(), Some(&0x55aa_4321));
        assert_eq!(
            events.last(),
            Some(&DynamicL1Event::SelectorRestored { raw: 0x55aa_4321 })
        );
    }

    struct FakeTopOwn {
        now: u64,
        selector: u32,
        status: u32,
        clear_after_ms: Option<u64>,
        writes: Vec<u32>,
    }
    impl TopOwnershipTransport for FakeTopOwn {
        type Error = ();
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn read_selector(&mut self) -> Result<u32, Self::Error> {
            Ok(self.selector)
        }
        fn write_selector(&mut self, value: u32) -> Result<(), Self::Error> {
            self.selector = value;
            self.writes.push(value);
            Ok(())
        }
        fn write_top_driver_own(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn read_top_low_power_control(&mut self) -> Result<u32, Self::Error> {
            if self
                .clear_after_ms
                .is_some_and(|deadline| self.now >= deadline)
            {
                self.status &= !MT_TOP_LPCR_HOST_FW_OWN;
            }
            Ok(self.status)
        }
        fn sleep_ms(&mut self, milliseconds: u64) {
            self.now += milliseconds;
        }
    }

    #[test]
    fn top_ownership_succeeds_and_restores_selector() {
        let mut transport = FakeTopOwn {
            now: 0,
            selector: 0xaaaa_5555,
            status: MT_TOP_LPCR_HOST_FW_OWN,
            clear_after_ms: Some(2),
            writes: Vec::new(),
        };
        let mut events = Vec::new();
        acquire_top_driver_ownership(&mut transport, |event| events.push(event)).unwrap();
        assert_eq!(transport.now, 2);
        assert_eq!(transport.selector, 0xaaaa_5555);
        assert_eq!(transport.writes, [0xaaaa_1806, 0xaaaa_5555]);
        assert_eq!(
            events.last(),
            Some(&TopOwnershipEvent::SelectorRestored { raw: 0xaaaa_5555 })
        );
    }

    #[test]
    fn top_ownership_times_out_and_restores_selector() {
        let mut transport = FakeTopOwn {
            now: 0,
            selector: 0x1234_5678,
            status: MT_TOP_LPCR_HOST_FW_OWN,
            clear_after_ms: None,
            writes: Vec::new(),
        };
        assert_eq!(
            acquire_top_driver_ownership(&mut transport, |_| {}),
            Err(TopOwnershipError::Timeout)
        );
        assert_eq!(transport.now, TOP_DRIVER_OWN_DEADLINE_MS);
        assert_eq!(transport.selector, 0x1234_5678);
    }

    #[test]
    fn top_ownership_rejects_driver_command_readback_and_restores() {
        let mut transport = FakeTopOwn {
            now: 0,
            selector: 0,
            status: MT_TOP_LPCR_HOST_DRV_OWN,
            clear_after_ms: None,
            writes: Vec::new(),
        };
        assert_eq!(
            acquire_top_driver_ownership(&mut transport, |_| {}),
            Err(TopOwnershipError::UnexpectedState(MT_TOP_LPCR_HOST_DRV_OWN))
        );
        assert_eq!(transport.selector, 0);
    }

    struct FakeFwdl {
        interrupt_enable: u32,
        global_config: u32,
        ring: FwdlRingRegisters,
        corrupt_count: bool,
        events: Vec<PublishEvent>,
        writes: Vec<(DisabledFwdlWrite, u32)>,
    }
    impl DisabledFwdlRingTransport for FakeFwdl {
        type Error = ();
        fn read(&mut self, register: DisabledFwdlRegister) -> Result<u32, Self::Error> {
            Ok(match register {
                DisabledFwdlRegister::HostInterruptEnable => self.interrupt_enable,
                DisabledFwdlRegister::WfdmaGlobalConfig => self.global_config,
                DisabledFwdlRegister::DescriptorBase => self.ring.descriptor_base,
                DisabledFwdlRegister::DescriptorCount => {
                    self.ring.descriptor_count
                        + u32::from(self.corrupt_count && self.ring.descriptor_count == 128)
                }
                DisabledFwdlRegister::CpuIndex => self.ring.cpu_index,
                DisabledFwdlRegister::DmaIndex => self.ring.dma_index,
            })
        }
        fn write(&mut self, register: DisabledFwdlWrite, value: u32) -> Result<(), Self::Error> {
            self.writes.push((register, value));
            match register {
                DisabledFwdlWrite::DescriptorBase => self.ring.descriptor_base = value,
                DisabledFwdlWrite::DescriptorCount => self.ring.descriptor_count = value,
                DisabledFwdlWrite::CpuIndex => self.ring.cpu_index = value,
            }
            Ok(())
        }
        fn release_fence(&mut self) {
            self.events.push(PublishEvent::Release)
        }
    }

    fn fake_fwdl() -> FakeFwdl {
        FakeFwdl {
            interrupt_enable: 0,
            global_config: 0x1010_b870,
            ring: FwdlRingRegisters {
                descriptor_base: 0xaaaa_0000,
                descriptor_count: 7,
                cpu_index: 3,
                dma_index: 3,
            },
            corrupt_count: false,
            events: Vec::new(),
            writes: Vec::new(),
        }
    }

    #[test]
    fn disabled_fwdl_ring_programs_after_fence_and_restores() {
        let mut transport = fake_fwdl();
        let snapshot = transport.ring;
        let mut events = Vec::new();
        let programmed =
            program_disabled_fwdl_ring(&mut transport, 0x0100_0000, |event| events.push(event))
                .unwrap();
        assert_eq!(programmed.descriptor_base, 0x0100_0000);
        assert_eq!(programmed.descriptor_count, 128);
        assert_eq!(transport.ring, snapshot);
        assert_eq!(transport.events, [PublishEvent::Release]);
        assert_eq!(events[2], DisabledFwdlEvent::DescriptorFence);
        assert_eq!(events.last(), Some(&DisabledFwdlEvent::Restored(snapshot)));
    }

    #[test]
    fn disabled_fwdl_ring_rejects_active_dma_or_interrupts_without_writes() {
        let mut transport = fake_fwdl();
        transport.interrupt_enable = 1;
        assert!(matches!(
            program_disabled_fwdl_ring(&mut transport, 0x0100_0000, |_| {}),
            Err(DisabledFwdlError::DmaOrInterruptActive { .. })
        ));
        assert!(transport.writes.is_empty());
    }

    #[test]
    fn disabled_fwdl_ring_restores_after_readback_mismatch() {
        let mut transport = fake_fwdl();
        let snapshot = transport.ring;
        transport.corrupt_count = true;
        assert!(matches!(
            program_disabled_fwdl_ring(&mut transport, 0x0100_0000, |_| {}),
            Err(DisabledFwdlError::Readback { .. })
        ));
        // The fake corrupts reads only; stored register values are restored.
        assert_eq!(transport.ring, snapshot);
    }

    struct FakeWfsys {
        now: u64,
        raw: u32,
        ready_at: Option<u64>,
        writes: Vec<u32>,
    }
    impl WfsysResetTransport for FakeWfsys {
        type Error = ();
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn read_reset_control(&mut self) -> Result<u32, Self::Error> {
            if self.ready_at.is_some_and(|ready| self.now >= ready) {
                self.raw |= WFSYS_SW_INIT_DONE;
            }
            Ok(self.raw)
        }
        fn write_reset_control(&mut self, value: u32) -> Result<(), Self::Error> {
            self.raw = value;
            self.writes.push(value);
            Ok(())
        }
        fn sleep_ms(&mut self, milliseconds: u64) {
            self.now += milliseconds;
        }
    }

    #[test]
    fn wfsys_reset_matches_linux_assert_release_and_ready_poll() {
        let mut transport = FakeWfsys {
            now: 10,
            raw: 0x101,
            ready_at: Some(62),
            writes: Vec::new(),
        };
        let mut events = Vec::new();
        reset_wfsys(&mut transport, |event| events.push(event)).unwrap();
        assert_eq!(transport.writes, [0x100, 0x101]);
        assert_eq!(transport.now, 62);
        assert_eq!(
            events.last(),
            Some(&WfsysResetEvent::Ready {
                raw: 0x111,
                at_ms: 52
            })
        );
    }

    #[test]
    fn wfsys_reset_has_bounded_ready_timeout() {
        let mut transport = FakeWfsys {
            now: 0,
            raw: WFSYS_SW_RST_B,
            ready_at: None,
            writes: Vec::new(),
        };
        let mut events = Vec::new();
        assert_eq!(
            reset_wfsys(&mut transport, |event| events.push(event)),
            Err(WfsysResetError::Timeout)
        );
        assert_eq!(transport.now, WFSYS_ASSERT_MS + WFSYS_READY_DEADLINE_MS);
        assert_eq!(
            events.last(),
            Some(&WfsysResetEvent::TimedOut {
                raw: WFSYS_SW_RST_B,
                at_ms: WFSYS_ASSERT_MS + WFSYS_READY_DEADLINE_MS
            })
        );
    }

    struct FakeInterrupt {
        global: u32,
        enable: u32,
        status: u32,
        stuck: bool,
        writes: Vec<(&'static str, u32)>,
    }
    impl DisabledFwdlInterruptTransport for FakeInterrupt {
        type Error = ();
        fn read_global_config(&mut self) -> Result<u32, Self::Error> {
            Ok(self.global)
        }
        fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
            Ok(self.enable)
        }
        fn write_interrupt_enable(&mut self, value: u32) -> Result<(), Self::Error> {
            self.enable = value;
            self.writes.push(("enable", value));
            Ok(())
        }
        fn read_interrupt_status(&mut self) -> Result<u32, Self::Error> {
            Ok(self.status)
        }
        fn acknowledge_interrupt_status(&mut self, value: u32) -> Result<(), Self::Error> {
            self.writes.push(("ack", value));
            if !self.stuck {
                self.status &= !value;
            }
            Ok(())
        }
    }

    #[test]
    fn disabled_interrupt_acknowledges_only_fwdl_and_restores_mask() {
        let mut transport = FakeInterrupt {
            global: 0x1010_b870,
            enable: 0,
            status: MT7921_INT_TX_DONE_FWDL | 0x20,
            stuck: false,
            writes: Vec::new(),
        };
        let mut events = Vec::new();
        assert_eq!(
            mask_ack_disabled_fwdl_interrupt(&mut transport, |event| events.push(event)),
            Ok(0x20)
        );
        assert_eq!(
            transport.writes,
            [
                ("enable", 0),
                ("ack", MT7921_INT_TX_DONE_FWDL),
                ("enable", 0)
            ]
        );
        assert_eq!(transport.status, 0x20);
        assert_eq!(
            events.last(),
            Some(&DisabledInterruptEvent::MaskRestored { value: 0 })
        );
    }

    #[test]
    fn disabled_interrupt_rejects_active_state_without_writes() {
        let mut transport = FakeInterrupt {
            global: 1,
            enable: 0,
            status: 0,
            stuck: false,
            writes: Vec::new(),
        };
        assert_eq!(
            mask_ack_disabled_fwdl_interrupt(&mut transport, |_| {}),
            Err(DisabledInterruptError::DmaActive(1))
        );
        assert!(transport.writes.is_empty());
    }

    #[test]
    fn disabled_interrupt_reports_stuck_w1c_and_restores_mask() {
        let mut transport = FakeInterrupt {
            global: 0,
            enable: 0,
            status: MT7921_INT_TX_DONE_FWDL,
            stuck: true,
            writes: Vec::new(),
        };
        assert_eq!(
            mask_ack_disabled_fwdl_interrupt(&mut transport, |_| {}),
            Err(DisabledInterruptError::AckDidNotClear(
                MT7921_INT_TX_DONE_FWDL
            ))
        );
        assert_eq!(transport.writes.last(), Some(&("enable", 0)));
    }

    struct FakeFirmwareStage {
        calls: Vec<&'static str>,
        descriptor: DmaDescriptor,
        corrupt_readback: bool,
        fail_reset: bool,
    }
    impl Default for FakeFirmwareStage {
        fn default() -> Self {
            Self {
                calls: Vec::new(),
                descriptor: DmaDescriptor::reset(),
                corrupt_readback: false,
                fail_reset: false,
            }
        }
    }
    impl DisabledFirmwareStageTransport for FakeFirmwareStage {
        type Error = &'static str;
        fn write_payload(&mut self, _: &[u8]) -> Result<(), Self::Error> {
            self.calls.push("payload");
            Ok(())
        }
        fn write_descriptor(&mut self, descriptor: DmaDescriptor) -> Result<(), Self::Error> {
            self.calls.push("descriptor");
            self.descriptor = descriptor;
            Ok(())
        }
        fn release_fence(&mut self) {
            self.calls.push("fence");
        }
        fn read_descriptor(&mut self) -> Result<DmaDescriptor, Self::Error> {
            self.calls.push("readback");
            Ok(if self.corrupt_readback {
                DmaDescriptor::reset()
            } else {
                self.descriptor
            })
        }
        fn reset_descriptor(&mut self) -> Result<(), Self::Error> {
            self.calls.push("reset");
            if self.fail_reset {
                return Err("reset failed");
            }
            self.descriptor = DmaDescriptor::reset();
            Ok(())
        }
        fn zero_payload(&mut self, _: usize) -> Result<(), Self::Error> {
            self.calls.push("zero");
            Ok(())
        }
    }

    #[test]
    fn stages_one_disabled_firmware_descriptor_then_cleans_it() {
        let mut transport = FakeFirmwareStage::default();
        let mut events = Vec::new();
        let descriptor = stage_disabled_firmware_chunk(
            &mut transport,
            0x0100_1000,
            &[0x5a; MT7921_FWDL_CHUNK_BYTES],
            |event| events.push(event),
        )
        .unwrap();
        assert_eq!(descriptor.buf0, 0x0100_1000);
        assert_eq!(descriptor.ctrl, (4096 << 16) | (1 << 30));
        assert_eq!(
            transport.calls,
            [
                "payload",
                "descriptor",
                "fence",
                "readback",
                "reset",
                "zero"
            ]
        );
        assert_eq!(transport.descriptor, DmaDescriptor::reset());
        assert_eq!(
            events.last(),
            Some(&DisabledFirmwareStageEvent::PayloadZeroed { bytes: 4096 })
        );
    }

    #[test]
    fn rejects_malformed_firmware_staging_without_writes() {
        for (iova, payload) in [
            (0x1000, &[][..]),
            (0x1000, &[0u8; MT7921_FWDL_CHUNK_BYTES + 1][..]),
            (u64::from(u32::MAX), &[0u8; 2][..]),
        ] {
            let mut transport = FakeFirmwareStage::default();
            assert!(stage_disabled_firmware_chunk(&mut transport, iova, payload, |_| {}).is_err());
            assert!(transport.calls.is_empty());
        }
    }

    #[test]
    fn readback_mismatch_still_resets_and_zeros_staged_memory() {
        let mut transport = FakeFirmwareStage {
            corrupt_readback: true,
            ..Default::default()
        };
        assert!(matches!(
            stage_disabled_firmware_chunk(&mut transport, 0x1000, b"firmware", |_| {}),
            Err(DisabledFirmwareStageError::DescriptorReadback { .. })
        ));
        assert_eq!(transport.calls.last_chunk::<2>(), Some(&["reset", "zero"]));
        assert_eq!(transport.descriptor, DmaDescriptor::reset());
    }

    #[test]
    fn descriptor_reset_failure_still_zeros_payload() {
        let mut transport = FakeFirmwareStage {
            fail_reset: true,
            ..Default::default()
        };
        assert_eq!(
            stage_disabled_firmware_chunk(&mut transport, 0x1000, b"firmware", |_| {}),
            Err(DisabledFirmwareStageError::Reset("reset failed"))
        );
        assert_eq!(transport.calls.last_chunk::<2>(), Some(&["reset", "zero"]));
    }

    #[test]
    fn firmware_completion_tracks_partial_timeout_complete_and_malformed() {
        assert_eq!(FirmwareCompletionTracker::new(0, 0, 1), None);
        let mut tracker = FirmwareCompletionTracker::new(4, 100, 20).unwrap();
        assert_eq!(
            tracker.observe(2, 110),
            Some(FirmwareCompletion::Partial {
                completed: 2,
                total: 4
            })
        );
        assert_eq!(tracker.observe(1, 111), None);
        assert_eq!(tracker.observe(5, 111), None);
        assert_eq!(
            tracker.observe(2, 120),
            Some(FirmwareCompletion::TimedOut {
                completed: 2,
                total: 4
            })
        );
        assert_eq!(tracker.observe(4, 121), Some(FirmwareCompletion::Complete));
    }

    struct FakeGlobalTx {
        global: u32,
        interrupts: u32,
        rings: [TxRingState; MT7921_TX_RING_SLOTS],
        writes: Vec<(usize, u32)>,
        resets: Vec<u32>,
    }
    impl FakeGlobalTx {
        fn new() -> Self {
            let mut rings = [TxRingState {
                descriptor_base: 0,
                descriptor_count: 128,
                cpu_index: 0,
                dma_index: 0,
            }; MT7921_TX_RING_SLOTS];
            for (index, ring) in rings.iter_mut().enumerate() {
                ring.descriptor_base = 0x8000_0000 + index as u32 * 0x1000;
            }
            Self {
                global: 0x1010_b870,
                interrupts: 0,
                rings,
                writes: Vec::new(),
                resets: Vec::new(),
            }
        }
    }
    impl GlobalTxRingTransport for FakeGlobalTx {
        type Error = ();
        fn read_global_config(&mut self) -> Result<u32, Self::Error> {
            Ok(self.global)
        }
        fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
            Ok(self.interrupts)
        }
        fn read_tx_ring(&mut self, index: usize) -> Result<TxRingState, Self::Error> {
            Ok(self.rings[index])
        }
        fn write_tx_ring(
            &mut self,
            index: usize,
            descriptor_base: u32,
            descriptor_count: u32,
            cpu_index: u32,
        ) -> Result<(), Self::Error> {
            self.writes.push((index, descriptor_base));
            self.rings[index].descriptor_base = descriptor_base;
            self.rings[index].descriptor_count = descriptor_count;
            self.rings[index].cpu_index = cpu_index;
            Ok(())
        }
        fn reset_tx_indices(&mut self, value: u32) -> Result<(), Self::Error> {
            self.resets.push(value);
            for ring in &mut self.rings {
                ring.dma_index = 0;
            }
            Ok(())
        }
    }

    struct FakeMcuRx {
        global: u32,
        interrupts: u32,
        registers: McuRxRegisters,
        writes: Vec<&'static str>,
    }
    impl DisabledMcuRxTransport for FakeMcuRx {
        type Error = ();
        fn read_global_config(&mut self) -> Result<u32, Self::Error> {
            Ok(self.global)
        }
        fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
            Ok(self.interrupts)
        }
        fn read_registers(&mut self) -> Result<McuRxRegisters, Self::Error> {
            Ok(self.registers)
        }
        fn read_registers_at(&mut self, index: usize) -> Result<McuRxRegisters, Self::Error> {
            if index != 0 {
                return Err(());
            }
            Ok(self.registers)
        }
        fn write_initial(
            &mut self,
            descriptor_base: u32,
            descriptor_count: u32,
        ) -> Result<(), Self::Error> {
            self.writes.push("initial");
            self.registers = McuRxRegisters {
                descriptor_base,
                descriptor_count,
                cpu_index: 0,
                dma_index: 0,
            };
            Ok(())
        }
        fn publish_cpu_index(&mut self, cpu_index: u32) -> Result<(), Self::Error> {
            self.writes.push("publish");
            self.registers.cpu_index = cpu_index;
            Ok(())
        }
        fn write_ring_initial(
            &mut self,
            index: usize,
            descriptor_base: u32,
            descriptor_count: u32,
        ) -> Result<(), Self::Error> {
            if index != 0 {
                return Err(());
            }
            self.write_initial(descriptor_base, descriptor_count)
        }
        fn publish_ring_cpu_index(
            &mut self,
            index: usize,
            cpu_index: u32,
        ) -> Result<(), Self::Error> {
            if index != 0 {
                return Err(());
            }
            self.publish_cpu_index(cpu_index)
        }
        fn release_fence(&mut self) {
            self.writes.push("fence");
        }
    }

    #[test]
    fn disabled_mcu_rx_resets_both_indices_before_publish() {
        let mut transport = FakeMcuRx {
            global: 0x1010_b870,
            interrupts: 0,
            registers: McuRxRegisters {
                descriptor_base: 0x8000_0000,
                descriptor_count: 8,
                cpu_index: 3,
                dma_index: 3,
            },
            writes: Vec::new(),
        };
        assert_eq!(
            program_disabled_mcu_rx_ring(&mut transport, 0x0100_3000, |_| {}),
            Ok(McuRxRegisters {
                descriptor_base: 0x0100_3000,
                descriptor_count: 8,
                cpu_index: 7,
                dma_index: 0,
            })
        );
        assert_eq!(transport.writes, ["initial", "fence", "publish"]);
    }

    #[test]
    fn disabled_mcu_rx_rejects_dirty_ring_without_writes() {
        let mut transport = FakeMcuRx {
            global: 0x1010_b870,
            interrupts: 0,
            registers: McuRxRegisters {
                descriptor_base: 0,
                descriptor_count: 8,
                cpu_index: 1,
                dma_index: 0,
            },
            writes: Vec::new(),
        };
        assert!(matches!(
            program_disabled_mcu_rx_ring(&mut transport, 0x0100_3000, |_| {}),
            Err(DisabledMcuRxError::DirtyRing(_))
        ));
        assert!(transport.writes.is_empty());
    }

    struct FakeGlobalRx {
        rings: [McuRxRegisters; MT7921_RX_RING_SLOTS],
        writes: Vec<(usize, u32)>,
    }
    impl DisabledMcuRxTransport for FakeGlobalRx {
        type Error = ();
        fn read_global_config(&mut self) -> Result<u32, Self::Error> {
            Ok(0x5020_b870)
        }
        fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
            Ok(0)
        }
        fn read_registers(&mut self) -> Result<McuRxRegisters, Self::Error> {
            Ok(self.rings[0])
        }
        fn read_registers_at(&mut self, index: usize) -> Result<McuRxRegisters, Self::Error> {
            Ok(self.rings[index])
        }
        fn write_initial(
            &mut self,
            descriptor_base: u32,
            descriptor_count: u32,
        ) -> Result<(), Self::Error> {
            self.write_ring_initial(0, descriptor_base, descriptor_count)
        }
        fn publish_cpu_index(&mut self, cpu_index: u32) -> Result<(), Self::Error> {
            self.publish_ring_cpu_index(0, cpu_index)
        }
        fn write_ring_initial(
            &mut self,
            index: usize,
            descriptor_base: u32,
            descriptor_count: u32,
        ) -> Result<(), Self::Error> {
            self.rings[index] = McuRxRegisters {
                descriptor_base,
                descriptor_count,
                cpu_index: 0,
                dma_index: 0,
            };
            self.writes.push((index, 0));
            Ok(())
        }
        fn publish_ring_cpu_index(
            &mut self,
            index: usize,
            cpu_index: u32,
        ) -> Result<(), Self::Error> {
            self.rings[index].cpu_index = cpu_index;
            self.writes.push((index, cpu_index));
            Ok(())
        }
        fn release_fence(&mut self) {}
    }

    #[test]
    fn global_rx_preparation_owns_all_eight_slots_before_publish() {
        let empty = McuRxRegisters {
            descriptor_base: 0,
            descriptor_count: 0,
            cpu_index: 0,
            dma_index: 0,
        };
        let mut transport = FakeGlobalRx {
            rings: [empty; MT7921_RX_RING_SLOTS],
            writes: Vec::new(),
        };
        let owned =
            prepare_global_rx_rings(&mut transport, 0x0100_3000, 0x0100_4000, |_| {}).unwrap();
        assert_eq!(owned[0].descriptor_base, 0x0100_4000);
        assert_eq!(owned[0].cpu_index, 7);
        assert!(
            owned[1..]
                .iter()
                .all(|ring| ring.descriptor_base == 0x0100_3000)
        );
        assert!(owned[1..].iter().all(|ring| ring.cpu_index == 0));
        assert_eq!(transport.writes.len(), MT7921_RX_RING_SLOTS * 2);
        assert!(
            transport.writes[..MT7921_RX_RING_SLOTS]
                .iter()
                .all(|(_, cpu_index)| *cpu_index == 0)
        );
    }

    #[test]
    fn global_tx_preparation_owns_all_eighteen_rings_before_index_reset() {
        let mut transport = FakeGlobalTx::new();
        let mut events = Vec::new();
        let owned = prepare_global_tx_rings(
            &mut transport,
            0x0100_0000,
            0x0100_1000,
            0x0100_2000,
            |event| events.push(event),
        )
        .unwrap();
        assert_eq!(transport.writes.len(), MT7921_TX_RING_SLOTS);
        assert_eq!(transport.resets, [MT7921_RESET_ALL_TX_INDICES]);
        for (index, state) in owned.into_iter().enumerate() {
            assert_eq!(
                state.descriptor_base,
                if index == MT7921_FWDL_RING_INDEX {
                    0x0100_1000
                } else if index == MT7921_MCU_TX_RING_INDEX {
                    0x0100_2000
                } else {
                    0x0100_0000
                }
            );
            assert_eq!(
                state.descriptor_count,
                if index == MT7921_MCU_TX_RING_INDEX {
                    MT7921_MCU_TX_RING_COUNT
                } else {
                    MT7921_FWDL_RING_COUNT
                }
            );
            assert_eq!(state.cpu_index, 0);
            assert_eq!(state.dma_index, 0);
        }
        assert!(matches!(
            events.last(),
            Some(GlobalTxRingEvent::RingVerified { index: 17, .. })
        ));
    }

    #[test]
    fn global_tx_preparation_rejects_dirty_mmio_and_arenas_before_writes() {
        let mut dirty = FakeGlobalTx::new();
        dirty.rings[7].cpu_index += 1;
        assert!(matches!(
            prepare_global_tx_rings(&mut dirty, 0x0100_0000, 0x0100_1000, 0x0100_2000, |_| {}),
            Err(GlobalTxRingError::DirtyRing { index: 7, .. })
        ));
        assert!(dirty.writes.is_empty());

        let mut overlap = FakeGlobalTx::new();
        assert_eq!(
            prepare_global_tx_rings(&mut overlap, 0x0100_0000, 0x0100_0000, 0x0100_2000, |_| {}),
            Err(GlobalTxRingError::InvalidArena)
        );
        assert!(overlap.writes.is_empty());
    }

    #[test]
    fn encodes_connac2_patch_protocol_before_scatter_dma() {
        let semaphore = encode_download_command(DownloadCommand::PatchSemaphoreGet, 1).unwrap();
        assert_eq!(semaphore.len(), PATCH_SEMAPHORE_REQUEST_BYTES);
        assert_eq!(
            u32::from_le_bytes(semaphore[0..4].try_into().unwrap()),
            0x4100_0044
        );
        assert_eq!(
            u32::from_le_bytes(semaphore[4..8].try_into().unwrap()),
            0x8001_0000
        );
        assert_eq!(&semaphore[32..34], &36u16.to_le_bytes());
        assert_eq!(&semaphore[34..36], &0x8000u16.to_le_bytes());
        assert_eq!(&semaphore[36..40], &[0x10, 0xa0, 3, 1]);
        assert_eq!(&semaphore[64..68], &1u32.to_le_bytes());
        let release = encode_download_command(DownloadCommand::PatchSemaphoreRelease, 2).unwrap();
        assert_eq!(&release[36..40], &[0x10, 0xa0, 3, 2]);
        assert_eq!(&release[64..68], &0u32.to_le_bytes());
        let power = encode_download_command(DownloadCommand::NicPowerControl, 3).unwrap();
        assert_eq!(&power[36..40], &[0x04, 0xa0, 3, 3]);
        assert_eq!(&power[64..68], &[1, 0, 0, 0]);
        let capability = encode_download_command(DownloadCommand::GetNicCapability, 4).unwrap();
        assert_eq!(capability.len(), CONNAC2_MCU_TXD_BYTES);
        assert_eq!(&capability[34..36], &0x8000u16.to_le_bytes());
        assert_eq!(&capability[36..40], &[0x8a, 0xa0, 1, 4]);
        let eeprom = encode_download_command(
            DownloadCommand::ReadEepromBlock {
                address: MT7921_EEPROM_HW_TYPE_BLOCK,
            },
            5,
        )
        .unwrap();
        assert_eq!(eeprom.len(), CONNAC2_MCU_TXD_BYTES + 24);
        assert_eq!(&eeprom[36..44], &[0xed, 0xa0, 0, 5, 0, 1, 0, 1]);
        assert_eq!(&eeprom[64..68], &MT7921_EEPROM_HW_TYPE_BLOCK.to_le_bytes());
        assert_eq!(&eeprom[68..], &[0; 20]);
        assert_eq!(
            encode_download_command(DownloadCommand::ReadEepromBlock { address: 0x551 }, 5),
            Err(DownloadCommandError::InvalidEepromAddress)
        );

        let patch = encode_download_command(
            DownloadCommand::PatchStart {
                address: 0x0090_0000,
                length: 0x0001_6780,
                mode: 1 << 31,
            },
            2,
        )
        .unwrap();
        assert_eq!(patch.len(), PATCH_START_REQUEST_BYTES);
        assert_eq!(
            u32::from_le_bytes(patch[0..4].try_into().unwrap()),
            0x4100_004c
        );
        assert_eq!(&patch[36..40], &[0x05, 0xa0, 3, 2]);
        assert_eq!(&patch[64..68], &0x0090_0000u32.to_le_bytes());
        assert_eq!(&patch[68..72], &0x0001_6780u32.to_le_bytes());
        assert_eq!(&patch[72..76], &(1u32 << 31).to_le_bytes());
        let finish = encode_download_command(DownloadCommand::PatchFinish, 3).unwrap();
        assert_eq!(finish.len(), PATCH_FINISH_REQUEST_BYTES);
        assert_eq!(&finish[36..40], &[0x07, 0xa0, 3, 3]);
        assert_eq!(&finish[64..68], &[0; 4]);
        let start = encode_download_command(
            DownloadCommand::FirmwareStart {
                address: 0x0091_5000,
                option: 1,
            },
            4,
        )
        .unwrap();
        assert_eq!(start.len(), FIRMWARE_START_REQUEST_BYTES);
        assert_eq!(
            u32::from_le_bytes(start[0..4].try_into().unwrap()),
            0x4100_0048
        );
        assert_eq!(&start[34..36], &0x8000u16.to_le_bytes());
        assert_eq!(&start[36..40], &[0x02, 0xa0, 3, 4]);
        assert_eq!(&start[64..68], &1u32.to_le_bytes());
        assert_eq!(&start[68..72], &0x0091_5000u32.to_le_bytes());
        assert_eq!(
            encode_download_command(
                DownloadCommand::FirmwareStart {
                    address: 0,
                    option: 0,
                },
                4,
            ),
            Err(DownloadCommandError::InvalidFirmwareStart)
        );
        assert_eq!(
            encode_download_command(DownloadCommand::PatchSemaphoreGet, 0),
            Err(DownloadCommandError::InvalidSequence)
        );
    }

    #[test]
    fn derives_connac2_patch_download_security_mode_fail_closed() {
        assert_eq!(patch_download_mode(u32::MAX), Ok(DL_MODE_NEED_RESPONSE));
        assert_eq!(patch_download_mode(0), Ok(DL_MODE_NEED_RESPONSE));
        assert_eq!(
            patch_download_mode(0x0100_0002),
            Ok(DL_MODE_NEED_RESPONSE | DL_MODE_ENCRYPT | DL_MODE_RESET_SECURITY_IV | (2 << 1))
        );
        assert_eq!(
            patch_download_mode(0x0200_0000),
            Ok(DL_MODE_NEED_RESPONSE
                | DL_MODE_ENCRYPT
                | DL_MODE_RESET_SECURITY_IV
                | DL_MODE_ENCRYPTION_MODE_SELECT)
        );
        assert_eq!(
            patch_download_mode(0x0300_0000),
            Err(PatchSecurityError::UnsupportedEncryptionType(3))
        );
    }

    #[test]
    fn derives_connac2_ram_region_download_mode() {
        assert_eq!(firmware_download_mode(0, false), DL_MODE_NEED_RESPONSE);
        assert_eq!(
            firmware_download_mode(0b0001_0111, false),
            DL_MODE_NEED_RESPONSE
                | DL_MODE_ENCRYPT
                | DL_MODE_KEY_INDEX
                | DL_MODE_RESET_SECURITY_IV
                | DL_MODE_ENCRYPTION_MODE_SELECT
        );
        assert_eq!(firmware_download_mode(1 << 5, false), DL_MODE_NEED_RESPONSE);
        assert_eq!(
            firmware_download_mode(0, true),
            DL_MODE_NEED_RESPONSE | DL_MODE_WORKING_PDA_CR4
        );
    }

    #[test]
    fn parses_bounded_connac2_download_responses_by_sequence() {
        let mut bytes = [0u8; 40];
        bytes[24..26].copy_from_slice(&12u16.to_le_bytes());
        bytes[26..28].copy_from_slice(&0xa0u16.to_le_bytes());
        bytes[28] = 4;
        bytes[29] = 7;
        bytes[30] = 1;
        bytes[32] = 2;
        assert_eq!(
            parse_download_response(&bytes, 7),
            Ok(DownloadResponse {
                length: 12,
                packet_type: 0xa0,
                event_id: 4,
                sequence: 7,
                option: 1,
                extended_event_id: 2,
            })
        );
        assert_eq!(
            parse_download_response(&bytes, 6),
            Err(DownloadResponseError::SequenceMismatch {
                expected: 6,
                actual: 7
            })
        );
        assert_eq!(
            parse_download_response(&bytes[..35], 7),
            Err(DownloadResponseError::Truncated)
        );
        bytes[24..26].copy_from_slice(&41u16.to_le_bytes());
        assert_eq!(
            parse_download_response(&bytes, 7),
            Err(DownloadResponseError::InvalidLength)
        );
    }

    #[test]
    fn vfio_irq_selection_requires_eventfd_and_prefers_msix() {
        let capabilities = [
            PciIrqCapability {
                kind: PciIrqKind::Intx,
                count: 1,
                eventfd: true,
            },
            PciIrqCapability {
                kind: PciIrqKind::Msi,
                count: 1,
                eventfd: false,
            },
            PciIrqCapability {
                kind: PciIrqKind::Msix,
                count: 8,
                eventfd: true,
            },
        ];
        assert_eq!(select_vfio_irq(&capabilities), Some(capabilities[2]));
        assert_eq!(
            select_vfio_irq(&[PciIrqCapability {
                kind: PciIrqKind::Msi,
                count: 1,
                eventfd: false,
            }]),
            None
        );
    }

    #[test]
    fn irq_lifecycle_cannot_unmask_before_eventfd_install() {
        let capability = PciIrqCapability {
            kind: PciIrqKind::Msi,
            count: 1,
            eventfd: true,
        };
        assert!(!IrqLifecycle::Uninstalled.may_unmask_device());
        assert_eq!(
            IrqLifecycle::Uninstalled.enable_device_source(),
            Err(IrqLifecycleError::InvalidTransition)
        );
        let installed = IrqLifecycle::Uninstalled.install(capability).unwrap();
        assert!(installed.may_unmask_device());
        let enabled = installed.enable_device_source().unwrap();
        assert_eq!(enabled.observe_event(0), Err(IrqLifecycleError::EmptyEvent));
        let observed = enabled.observe_event(1).unwrap();
        assert_eq!(observed.disable(), Ok(IrqLifecycle::Disabled));
    }

    struct FakePinnedTeardown {
        now: u64,
        busy_until: Option<u64>,
        reset_fails: bool,
        calls: Vec<&'static str>,
    }
    impl PinnedDmaTeardownTransport for FakePinnedTeardown {
        type Error = &'static str;
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn mask_device_interrupts(&mut self) -> Result<(), Self::Error> {
            self.calls.push("mask");
            Ok(())
        }
        fn disable_dma(&mut self) -> Result<(), Self::Error> {
            self.calls.push("disable");
            Ok(())
        }
        fn read_global_config(&mut self) -> Result<u32, Self::Error> {
            self.calls.push("read");
            Ok(if self.busy_until.is_none_or(|until| self.now < until) {
                (1 << 1) | (1 << 3)
            } else {
                0
            })
        }
        fn sleep_ms(&mut self, milliseconds: u64) {
            self.now += milliseconds;
        }
        fn reset_vfio_device(&mut self) -> Result<(), Self::Error> {
            self.calls.push("reset");
            if self.reset_fails {
                Err("reset failed")
            } else {
                Ok(())
            }
        }
        fn unmap_all(&mut self) -> Result<(), Self::Error> {
            self.calls.push("unmap");
            Ok(())
        }
    }

    #[test]
    fn pinned_dma_teardown_resets_before_unmapping_after_quiescence() {
        let mut transport = FakePinnedTeardown {
            now: 0,
            busy_until: Some(2),
            reset_fails: false,
            calls: Vec::new(),
        };
        teardown_pinned_dma(&mut transport, |_| {}).unwrap();
        let reset = transport
            .calls
            .iter()
            .position(|call| *call == "reset")
            .unwrap();
        let unmap = transport
            .calls
            .iter()
            .position(|call| *call == "unmap")
            .unwrap();
        assert!(reset < unmap);
    }

    #[test]
    fn pinned_dma_busy_timeout_resets_before_release() {
        let mut transport = FakePinnedTeardown {
            now: 0,
            busy_until: None,
            reset_fails: false,
            calls: Vec::new(),
        };
        assert_eq!(
            teardown_pinned_dma(&mut transport, |_| {}),
            Err(PinnedDmaTeardownError::BusyTimedOut((1 << 1) | (1 << 3)))
        );
        assert_eq!(transport.calls.last_chunk::<2>(), Some(&["reset", "unmap"]));
    }

    #[test]
    fn pinned_dma_reset_failure_never_unmaps() {
        let mut transport = FakePinnedTeardown {
            now: 0,
            busy_until: None,
            reset_fails: true,
            calls: Vec::new(),
        };
        assert_eq!(
            teardown_pinned_dma(&mut transport, |_| {}),
            Err(PinnedDmaTeardownError::Reset("reset failed"))
        );
        assert_eq!(transport.calls.last(), Some(&"reset"));
        assert!(!transport.calls.contains(&"unmap"));
    }

    #[test]
    fn encodes_single_and_paired_dma_segments_like_mt76() {
        let one = DmaDescriptor::tx(
            DmaSegment {
                iova: 0x1234_5000,
                len: 0x345,
            },
            None,
            0xaabb_ccdd,
        )
        .unwrap();
        assert_eq!(one.ctrl, 0x4345_0000);
        assert_eq!(one.buf1, 0);
        assert_eq!(one.to_le_bytes()[..4], [0x00, 0x50, 0x34, 0x12]);

        let two = DmaDescriptor::tx(
            DmaSegment {
                iova: 0x1000,
                len: 64,
            },
            Some(DmaSegment {
                iova: 0x2000,
                len: 1500,
            }),
            7,
        )
        .unwrap();
        assert_eq!(two.ctrl, (64 << 16) | 1500 | (1 << 14));
        assert_eq!(two.buf1, 0x2000);
        assert_eq!(DmaDescriptor::reset().ctrl, 1 << 31);
    }

    #[test]
    fn builds_owned_mcu_rx_ring_with_one_empty_slot() {
        let ring = prepare_mcu_rx_ring(0x0100_3000, 0x0100_4000).unwrap();
        assert_eq!(ring.producer_index, 7);
        for (index, descriptor) in ring.descriptors[..7].iter().enumerate() {
            assert_eq!(
                *descriptor,
                DmaDescriptor {
                    buf0: 0x0100_4000 + index as u32 * 2048,
                    ctrl: 2048 << 16,
                    buf1: 0,
                    info: 0,
                }
            );
        }
        assert_eq!(ring.descriptors[7], DmaDescriptor::reset());
        assert_eq!(
            prepare_mcu_rx_ring(0x0100_3000, 0x0100_3000),
            Err(DescriptorError::InvalidArena)
        );
    }

    #[test]
    fn rejects_values_the_mt7921_pci_descriptor_cannot_represent() {
        assert_eq!(
            DmaDescriptor::tx(
                DmaSegment {
                    iova: 1u64 << 32,
                    len: 1
                },
                None,
                0
            ),
            Err(DescriptorError::IovaAbove32Bits)
        );
        assert_eq!(
            DmaDescriptor::tx(
                DmaSegment {
                    iova: 0,
                    len: 0x4000
                },
                None,
                0
            ),
            Err(DescriptorError::SegmentTooLong)
        );
    }

    fn firmware_image(regions: &[(u32, u8, u8, &[u8])]) -> std::vec::Vec<u8> {
        let mut image = vec![];
        for (_, _, _, payload) in regions {
            image.extend_from_slice(payload);
        }
        for (address, feature, kind, payload) in regions {
            let mut record = [0u8; FW_REGION_LEN];
            record[16..20].copy_from_slice(&address.to_le_bytes());
            record[20..24].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            record[24] = *feature;
            record[25] = *kind;
            image.extend_from_slice(&record);
        }
        let mut trailer = [0u8; FW_TRAILER_LEN];
        trailer[0] = 0x79;
        trailer[1] = 2;
        trailer[2] = regions.len() as u8;
        trailer[3] = 3;
        trailer[4] = 4;
        trailer[7..17].copy_from_slice(b"FW-TEST-01");
        trailer[17..32].copy_from_slice(b"20260101-120000");
        trailer[32..36].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        image.extend_from_slice(&trailer);
        image
    }

    fn nic_capability_fixture() -> (Vec<u8>, NicCapability) {
        let mut bytes = vec![0; 4];
        bytes[0..2].copy_from_slice(&4u16.to_le_bytes());
        for (kind, value) in [
            (7u32, vec![0x00, 0x11, 0x22, 0x33, 0x44, 0x55]),
            (8, vec![1, 1, 1, 2, 2, 0, 1, 1, 1, 1, 3, 1]),
            (0x18, vec![1]),
            (0x20, 0x1122_3344_5566_7788u64.to_le_bytes().to_vec()),
        ] {
            bytes.extend_from_slice(&kind.to_le_bytes());
            bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&value);
        }
        (
            bytes,
            NicCapability {
                element_count: 4,
                mac_address: Some([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]),
                phy: Some(NicPhyCapability {
                    ht: true,
                    vht: true,
                    has_5ghz: true,
                    max_bandwidth: 2,
                    spatial_streams: 2,
                    hardware_path: 3,
                    he: true,
                }),
                has_6ghz: Some(true),
                chip_capability: Some(0x1122_3344_5566_7788),
                unknown_elements: 0,
            },
        )
    }

    fn eeprom_hardware_fixture() -> (Vec<u8>, EepromBlock) {
        let mut bytes = vec![0; 24];
        bytes[0..4].copy_from_slice(&MT7921_EEPROM_HW_TYPE_BLOCK.to_le_bytes());
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        bytes[8 + 11] = 1;
        (
            bytes,
            EepromBlock {
                address: MT7921_EEPROM_HW_TYPE_BLOCK,
                valid: 1,
                data: {
                    let mut data = [0; MT7921_EEPROM_BLOCK_SIZE];
                    data[11] = 1;
                    data
                },
            },
        )
    }

    fn clc_fixture() -> Vec<u8> {
        let mut bytes = vec![0; 16];
        bytes[0..4].copy_from_slice(&33u32.to_le_bytes());
        bytes[4] = 0;
        bytes[5] = 1;
        bytes[6] = 1;
        bytes[7] = 1;
        bytes.extend_from_slice(b"00");
        bytes.extend_from_slice(b"-0");
        bytes.extend_from_slice(&11u16.to_le_bytes());
        bytes.extend_from_slice(&[0x5a; 11]);
        bytes
    }

    #[test]
    fn parses_bounded_nic_capability_tlvs() {
        let (bytes, expected) = nic_capability_fixture();
        assert_eq!(parse_nic_capability(&bytes), Ok(expected));
        assert_eq!(
            parse_nic_capability(&bytes[..bytes.len() - 1]),
            Err(NicCapabilityError::TruncatedElement {
                index: 3,
                length: 8,
            })
        );
        assert_eq!(
            parse_nic_capability(&[1, 0, 0, 0]),
            Err(NicCapabilityError::TruncatedElementHeader { index: 0 })
        );
    }

    #[test]
    fn parses_bounded_eeprom_block_and_hardware_type() {
        let (bytes, expected) = eeprom_hardware_fixture();
        assert_eq!(
            parse_eeprom_block(&bytes, MT7921_EEPROM_HW_TYPE_BLOCK),
            Ok(expected)
        );
        assert_eq!(
            expected.hardware_info(),
            Ok(EepromHardwareInfo {
                raw_type: 1,
                encapsulated_calibration: true,
            })
        );
        assert_eq!(
            parse_eeprom_block(&bytes[..23], MT7921_EEPROM_HW_TYPE_BLOCK),
            Err(EepromBlockError::Truncated)
        );
        assert_eq!(
            parse_eeprom_block(&bytes, 0),
            Err(EepromBlockError::AddressMismatch {
                expected: 0,
                actual: MT7921_EEPROM_HW_TYPE_BLOCK,
            })
        );
    }

    #[test]
    fn discovers_selected_clc_rules_without_sending_configuration() {
        let clc = clc_fixture();
        let image = firmware_image(&[(0, FW_FEATURE_NON_DL, FW_TYPE_CLC, &clc)]);
        assert_eq!(
            discover_clc(
                Firmware::parse(&image).unwrap(),
                EepromHardwareInfo {
                    raw_type: 1,
                    encapsulated_calibration: true,
                }
            ),
            Ok(ClcDiscovery {
                segment_count: 1,
                selected_power_segments: 1,
                selected_power_rules: 1,
                channel_segments: 0,
                channel_rules: 0,
                unique_country_codes: 1,
                world_domain_available: true,
            })
        );
        let mut malformed = clc;
        malformed[0..4].copy_from_slice(&34u32.to_le_bytes());
        let image = firmware_image(&[(0, FW_FEATURE_NON_DL, FW_TYPE_CLC, &malformed)]);
        assert_eq!(
            discover_clc(
                Firmware::parse(&image).unwrap(),
                EepromHardwareInfo {
                    raw_type: 1,
                    encapsulated_calibration: true,
                }
            ),
            Err(ClcDiscoveryError::InvalidSegmentLength(34))
        );
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum LoaderTrace {
        Command(DownloadCommand, u8),
        PublishScatter(FirmwareImagePart, u8, usize),
        ScatterCompletion(FirmwareImagePart, u8, u64),
        DownloadState,
        N9Ready,
        Sleep(u64),
        Cleanup(FirmwareLoaderState),
    }

    struct FakeFirmwareLoader {
        trace: Vec<LoaderTrace>,
        calls: usize,
        sequence: u8,
        now_ms: u64,
        fail_at: Option<usize>,
        download_states: Vec<u8>,
        download_poll: usize,
        n9_states: Vec<bool>,
        n9_poll: usize,
        semaphore_result: u8,
        completion_override: Option<(DownloadCommand, FirmwareCommandCompletion)>,
        fail_release: bool,
        fail_patch_publish: bool,
        fail_patch_completion: bool,
    }

    impl Default for FakeFirmwareLoader {
        fn default() -> Self {
            Self {
                trace: vec![],
                calls: 0,
                sequence: 0,
                now_ms: 0,
                fail_at: None,
                download_states: vec![0, 1],
                download_poll: 0,
                n9_states: vec![false, true],
                n9_poll: 0,
                semaphore_result: 2,
                completion_override: None,
                fail_release: false,
                fail_patch_publish: false,
                fail_patch_completion: false,
            }
        }
    }

    impl FakeFirmwareLoader {
        fn step(&mut self) -> Result<(), &'static str> {
            self.calls += 1;
            if self.fail_at == Some(self.calls) {
                Err("injected transport failure")
            } else {
                Ok(())
            }
        }

        fn next_download_state(&mut self) -> u8 {
            let state = self
                .download_states
                .get(self.download_poll)
                .copied()
                .unwrap_or_else(|| *self.download_states.last().unwrap_or(&0));
            self.download_poll += 1;
            state
        }

        fn next_n9_state(&mut self) -> bool {
            let ready = self
                .n9_states
                .get(self.n9_poll)
                .copied()
                .unwrap_or_else(|| *self.n9_states.last().unwrap_or(&false));
            self.n9_poll += 1;
            ready
        }
    }

    impl FirmwareLoaderTransport for FakeFirmwareLoader {
        type Error = &'static str;

        fn next_sequence(&mut self) -> u8 {
            self.sequence = (self.sequence + 1) & 0x0f;
            if self.sequence == 0 {
                self.sequence = 1;
            }
            self.sequence
        }

        fn command(
            &mut self,
            command: DownloadCommand,
            sequence: u8,
            encoded: &[u8],
        ) -> Result<FirmwareCommandCompletion, Self::Error> {
            assert_eq!(encoded[39], sequence);
            assert_eq!(&encoded[34..36], &0x8000u16.to_le_bytes());
            self.trace.push(LoaderTrace::Command(command, sequence));
            self.step()?;
            if self.fail_release && command == DownloadCommand::PatchSemaphoreRelease {
                return Err("injected release failure");
            }
            if let Some((overridden, completion)) = self.completion_override
                && overridden == command
            {
                return Ok(completion);
            }
            Ok(match command {
                DownloadCommand::NicPowerControl => FirmwareCommandCompletion::NoResponse,
                DownloadCommand::GetNicCapability => {
                    FirmwareCommandCompletion::NicCapability(nic_capability_fixture().1)
                }
                DownloadCommand::ReadEepromBlock { .. } => {
                    FirmwareCommandCompletion::EepromBlock(eeprom_hardware_fixture().1)
                }
                DownloadCommand::PatchSemaphoreGet => {
                    FirmwareCommandCompletion::PatchSemaphore(self.semaphore_result.into())
                }
                DownloadCommand::PatchSemaphoreRelease => {
                    FirmwareCommandCompletion::PatchSemaphore(PatchSemaphoreStatus::Released)
                }
                DownloadCommand::PatchFinish => FirmwareCommandCompletion::PatchFinish(0),
                DownloadCommand::PatchStart { .. }
                | DownloadCommand::TargetAddressLength { .. }
                | DownloadCommand::FirmwareStart { .. } => FirmwareCommandCompletion::Ack,
            })
        }

        fn publish_scatter(
            &mut self,
            part: FirmwareImagePart,
            sequence: u8,
            chunk: &[u8],
        ) -> Result<(), Self::Error> {
            assert!(!chunk.is_empty());
            assert!(chunk.len() <= MT7921_FWDL_CHUNK_BYTES);
            self.trace
                .push(LoaderTrace::PublishScatter(part, sequence, chunk.len()));
            self.step()?;
            if self.fail_patch_publish && part == FirmwareImagePart::Patch {
                Err("injected patch publish failure")
            } else {
                Ok(())
            }
        }

        fn wait_scatter_completion(
            &mut self,
            part: FirmwareImagePart,
            sequence: u8,
            deadline_ms: u64,
        ) -> Result<(), Self::Error> {
            assert_eq!(
                deadline_ms,
                self.now_ms.saturating_add(SCATTER_COMPLETION_TIMEOUT_MS)
            );
            self.trace
                .push(LoaderTrace::ScatterCompletion(part, sequence, deadline_ms));
            self.step()?;
            if self.fail_patch_completion && part == FirmwareImagePart::Patch {
                Err("injected patch completion timeout")
            } else {
                Ok(())
            }
        }

        fn firmware_download_state(&mut self) -> Result<u8, Self::Error> {
            self.trace.push(LoaderTrace::DownloadState);
            self.step()?;
            Ok(self.next_download_state())
        }

        fn firmware_n9_ready(&mut self) -> Result<bool, Self::Error> {
            self.trace.push(LoaderTrace::N9Ready);
            self.step()?;
            Ok(self.next_n9_state())
        }

        fn now_ms(&self) -> u64 {
            self.now_ms
        }

        fn sleep_ms(&mut self, duration_ms: u64) {
            self.trace.push(LoaderTrace::Sleep(duration_ms));
            self.now_ms = self.now_ms.saturating_add(duration_ms);
        }

        fn fail_closed_cleanup(&mut self, state: FirmwareLoaderState) -> Result<(), Self::Error> {
            self.trace.push(LoaderTrace::Cleanup(state));
            self.step()
        }
    }

    fn loader_images() -> (Vec<u8>, Vec<u8>) {
        let patch = patch_image(0x0004_0002, 160, 4097);
        let ram_large = vec![0x5a; 4097];
        let clc = clc_fixture();
        let ram = firmware_image(&[
            (0x0091_5000, 1 << 5, 0, &ram_large),
            (0x0201_5c00, 0, 0, b"ram"),
            (0, FW_FEATURE_NON_DL, FW_TYPE_CLC, &clc),
        ]);
        (patch, ram)
    }

    #[test]
    fn firmware_loader_matches_linux_transaction_golden_trace() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut transport = FakeFirmwareLoader::default();
        let report = load_mt7921_firmware(
            &mut transport,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();
        assert_eq!(
            report,
            FirmwareLoaderReport {
                download_ready_observed: true,
                patch: PatchDisposition::Downloaded,
                patch_sections: 1,
                ram_regions: 2,
                scatter_chunks: 5,
                scatter_bytes: 8197,
                nic_capability: nic_capability_fixture().1,
                eeprom_hardware: eeprom_hardware_fixture().1,
                clc: ClcDiscovery {
                    segment_count: 1,
                    selected_power_segments: 1,
                    selected_power_rules: 1,
                    channel_segments: 0,
                    channel_rules: 0,
                    unique_country_codes: 1,
                    world_domain_available: true,
                },
            }
        );
        assert_eq!(
            transport.trace,
            [
                LoaderTrace::Command(DownloadCommand::NicPowerControl, 1),
                LoaderTrace::DownloadState,
                LoaderTrace::Sleep(10),
                LoaderTrace::DownloadState,
                LoaderTrace::Command(DownloadCommand::PatchSemaphoreGet, 2),
                LoaderTrace::Command(
                    DownloadCommand::PatchStart {
                        address: 0x0090_0000,
                        length: 4097,
                        mode: DL_MODE_NEED_RESPONSE,
                    },
                    3,
                ),
                LoaderTrace::PublishScatter(FirmwareImagePart::Patch, 4, 4096),
                LoaderTrace::ScatterCompletion(FirmwareImagePart::Patch, 4, 3010),
                LoaderTrace::PublishScatter(FirmwareImagePart::Patch, 5, 1),
                LoaderTrace::ScatterCompletion(FirmwareImagePart::Patch, 5, 3010),
                LoaderTrace::Command(DownloadCommand::PatchFinish, 6),
                LoaderTrace::Command(DownloadCommand::PatchSemaphoreRelease, 7),
                LoaderTrace::Command(
                    DownloadCommand::TargetAddressLength {
                        address: 0x0091_5000,
                        length: 4097,
                        mode: DL_MODE_NEED_RESPONSE,
                    },
                    8,
                ),
                LoaderTrace::PublishScatter(FirmwareImagePart::Ram, 9, 4096),
                LoaderTrace::ScatterCompletion(FirmwareImagePart::Ram, 9, 3010),
                LoaderTrace::PublishScatter(FirmwareImagePart::Ram, 10, 1),
                LoaderTrace::ScatterCompletion(FirmwareImagePart::Ram, 10, 3010),
                LoaderTrace::Command(
                    DownloadCommand::TargetAddressLength {
                        address: 0x0201_5c00,
                        length: 3,
                        mode: DL_MODE_NEED_RESPONSE,
                    },
                    11,
                ),
                LoaderTrace::PublishScatter(FirmwareImagePart::Ram, 12, 3),
                LoaderTrace::ScatterCompletion(FirmwareImagePart::Ram, 12, 3010),
                LoaderTrace::Command(
                    DownloadCommand::FirmwareStart {
                        address: 0x0091_5000,
                        option: 1,
                    },
                    13,
                ),
                LoaderTrace::N9Ready,
                LoaderTrace::Sleep(10),
                LoaderTrace::N9Ready,
                LoaderTrace::Command(DownloadCommand::GetNicCapability, 14),
                LoaderTrace::Command(
                    DownloadCommand::ReadEepromBlock {
                        address: MT7921_EEPROM_HW_TYPE_BLOCK,
                    },
                    15,
                ),
                LoaderTrace::Cleanup(FirmwareLoaderState::Ready),
            ]
        );
    }

    #[test]
    fn firmware_loader_injected_transport_failures_always_cleanup_and_release() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut baseline = FakeFirmwareLoader::default();
        load_mt7921_firmware(
            &mut baseline,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();
        for fail_at in 1..=baseline.calls {
            let mut transport = FakeFirmwareLoader {
                fail_at: Some(fail_at),
                ..Default::default()
            };
            assert!(
                load_mt7921_firmware(
                    &mut transport,
                    Patch::parse(&patch_bytes).unwrap(),
                    Firmware::parse(&ram_bytes).unwrap(),
                )
                .is_err()
            );
            assert!(matches!(
                transport.trace.last(),
                Some(LoaderTrace::Cleanup(_))
            ));
            let acquired = transport.trace.iter().any(|event| {
                matches!(
                    event,
                    LoaderTrace::Command(DownloadCommand::PatchSemaphoreGet, _)
                )
            }) && fail_at > 4;
            if acquired {
                assert!(transport.trace.iter().any(|event| {
                    matches!(
                        event,
                        LoaderTrace::Command(DownloadCommand::PatchSemaphoreRelease, _)
                    )
                }));
            }
        }
    }

    #[test]
    fn firmware_loader_preserves_patch_release_and_completion_failures() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut combined = FakeFirmwareLoader {
            fail_patch_publish: true,
            fail_release: true,
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut combined,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::PatchRelease {
                    primary: Some(_),
                    release: _,
                }
            ))
        ));
        assert_eq!(
            combined.trace.last(),
            Some(&LoaderTrace::Cleanup(
                FirmwareLoaderState::PatchSemaphoreHeld
            ))
        );

        let mut completion_timeout = FakeFirmwareLoader {
            fail_patch_completion: true,
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut completion_timeout,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::Transport {
                    operation: FirmwareLoaderOperation::WaitScatterCompletion(
                        FirmwareImagePart::Patch
                    ),
                    ..
                }
            ))
        ));
        assert!(completion_timeout.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::Command(DownloadCommand::PatchSemaphoreRelease, _)
        )));
    }

    #[test]
    fn firmware_loader_rejects_wrong_completions_and_wraps_sequence() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut wrong = FakeFirmwareLoader {
            completion_override: Some((
                DownloadCommand::NicPowerControl,
                FirmwareCommandCompletion::Ack,
            )),
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut wrong,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::UnexpectedCommandCompletion {
                    command: DownloadCommand::NicPowerControl,
                    completion: FirmwareCommandCompletion::Ack,
                }
            ))
        ));

        let mut wrong_and_cleanup = FakeFirmwareLoader {
            fail_at: Some(2),
            completion_override: Some((
                DownloadCommand::NicPowerControl,
                FirmwareCommandCompletion::Ack,
            )),
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut wrong_and_cleanup,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Cleanup {
                failure: Some(FirmwareLoaderFailure::UnexpectedCommandCompletion { .. }),
                source: "injected transport failure",
            })
        ));

        let mut finish_status = FakeFirmwareLoader {
            completion_override: Some((
                DownloadCommand::PatchFinish,
                FirmwareCommandCompletion::PatchFinish(9),
            )),
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut finish_status,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::UnexpectedPatchFinish(9)
            ))
        ));

        let mut wrapped = FakeFirmwareLoader {
            sequence: 14,
            ..Default::default()
        };
        load_mt7921_firmware(
            &mut wrapped,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();
        assert_eq!(
            wrapped.trace[0],
            LoaderTrace::Command(DownloadCommand::NicPowerControl, 15)
        );
        assert!(wrapped.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::Command(DownloadCommand::PatchSemaphoreGet, 1)
        )));
        assert!(!wrapped.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::Command(_, 0)
                | LoaderTrace::PublishScatter(_, 0, _)
                | LoaderTrace::ScatterCompletion(_, 0, _)
        )));
    }

    #[test]
    fn firmware_loader_bounds_readiness_warning_and_skips_an_existing_patch() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut timeout = FakeFirmwareLoader {
            download_states: vec![0],
            ..Default::default()
        };
        let timed_out_report = load_mt7921_firmware(
            &mut timeout,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();
        assert!(!timed_out_report.download_ready_observed);
        assert_eq!(timeout.now_ms, DOWNLOAD_READY_TIMEOUT_MS + 10);
        assert_eq!(
            timeout
                .trace
                .iter()
                .filter(|event| matches!(event, LoaderTrace::DownloadState))
                .count(),
            101
        );
        assert_eq!(
            timeout.trace.last(),
            Some(&LoaderTrace::Cleanup(FirmwareLoaderState::Ready))
        );

        let mut existing = FakeFirmwareLoader {
            semaphore_result: 1,
            ..Default::default()
        };
        let report = load_mt7921_firmware(
            &mut existing,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();
        assert_eq!(report.patch, PatchDisposition::AlreadyDownloaded);
        assert_eq!(report.patch_sections, 0);
        assert!(!existing.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::PublishScatter(FirmwareImagePart::Patch, ..)
        )));
        assert_eq!(
            existing
                .trace
                .iter()
                .filter(|event| matches!(
                    event,
                    LoaderTrace::Command(DownloadCommand::TargetAddressLength { .. }, _)
                ))
                .count(),
            2
        );
        assert!(!existing.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::Command(DownloadCommand::PatchSemaphoreRelease, _)
        )));

        let mut n9_timeout = FakeFirmwareLoader {
            n9_states: vec![false],
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut n9_timeout,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::N9ReadyTimeout
            ))
        ));
        assert_eq!(
            n9_timeout.trace.last(),
            Some(&LoaderTrace::Cleanup(FirmwareLoaderState::FirmwareStarted))
        );
        assert_eq!(
            n9_timeout
                .trace
                .iter()
                .filter(|event| matches!(event, LoaderTrace::N9Ready))
                .count(),
            151
        );
    }

    #[test]
    fn parses_download_and_non_download_clc_regions() {
        let image = firmware_image(&[
            (0x0040_0000, 0, 0, b"ram"),
            (0, FW_FEATURE_NON_DL, FW_TYPE_CLC, b"clc-data"),
        ]);
        let firmware = Firmware::parse(&image).unwrap();
        assert_eq!(firmware.region_count(), 2);
        assert_eq!(firmware.trailer.chip_id, 0x79);
        assert_eq!(firmware.trailer.firmware_version, b"FW-TEST-01");
        assert_eq!(firmware.trailer.crc, 0xdead_beef);

        let mut regions = firmware.regions();
        let ram = regions.next().unwrap();
        assert!(ram.is_downloadable());
        assert_eq!(ram.address, 0x0040_0000);
        assert_eq!(ram.payload, b"ram");
        let clc = regions.next().unwrap();
        assert!(!clc.is_downloadable());
        assert!(clc.is_clc());
        assert_eq!(clc.payload, b"clc-data");
        assert_eq!(regions.next(), None);
    }

    #[test]
    fn rejects_truncated_tables_and_payload_overlapping_metadata() {
        assert_eq!(
            Firmware::parse(&[0; FW_TRAILER_LEN - 1]),
            Err(FirmwareError::MissingTrailer)
        );

        let mut table_too_large = [0u8; FW_TRAILER_LEN];
        table_too_large[2] = 1;
        assert_eq!(
            Firmware::parse(&table_too_large),
            Err(FirmwareError::RegionTableTooLarge)
        );

        let mut image = firmware_image(&[(0, 0, 0, b"x")]);
        let record = image.len() - FW_TRAILER_LEN - FW_REGION_LEN;
        image[record + 20..record + 24].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            Firmware::parse(&image),
            Err(FirmwareError::PayloadOverlapsMetadata)
        );
    }
}
