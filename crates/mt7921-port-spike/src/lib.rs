#![no_std]

//! Hardware-independent pieces of the Linux mt76 Connac2 data formats.
//!
//! This is an exploratory port, not an MT7921/MT7922 device driver. The source
//! correspondence and the boundary deliberately left out are documented in
//! the crate README.

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
    if arena_iova % 4096 != 0
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
