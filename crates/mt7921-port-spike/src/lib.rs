#![no_std]

//! Hardware-independent pieces of the Linux mt76 Connac2 data formats.
//!
//! This is an exploratory port, not an MT7921/MT7922 device driver. The source
//! correspondence and the boundary deliberately left out are documented in
//! the crate README.

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareError {
    MissingTrailer,
    RegionTableTooLarge,
    PayloadLengthOverflow,
    PayloadOverlapsMetadata,
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

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;

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
