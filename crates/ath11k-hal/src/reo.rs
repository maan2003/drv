// PORT-MAP: wcn6750-specific
//! REO command/status ring codecs from `hal_rx.c`.

use crate::{Descriptor, HalError, RingMemory};
use alloc::vec::Vec;
use ath11k_platform_backend::{Backend, DeviceAddress, Direction, MmioRegion};

const UMAC_REO: usize = 0x00a3_8000;

/// WCN6750's `ath11k_hw_wcn6855_reo_setup` register sequence.
pub fn setup_wcn6750<B: Backend>(mmio: &MmioRegion<B>) -> Result<(), HalError> {
    let general = mmio.read_u32(UMAC_REO).map_err(|_| HalError::DeviceFault)?;
    mmio.write_u32(UMAC_REO, general | 1 << 2 | 1 << 3)
        .map_err(|_| HalError::DeviceFault)?;
    let misc = mmio
        .read_u32(UMAC_REO + 0x5d8)
        .map_err(|_| HalError::DeviceFault)?;
    mmio.write_u32(UMAC_REO + 0x5d8, misc & !(0xf << 17))
        .map_err(|_| HalError::DeviceFault)?;
    for offset in [0x564, 0x568, 0x56c, 0x570] {
        mmio.write_u32(UMAC_REO + offset, 40_000)
            .map_err(|_| HalError::DeviceFault)?;
    }
    let hash = 0x4321_4321;
    mmio.write_u32(UMAC_REO + 0x0c, hash)
        .map_err(|_| HalError::DeviceFault)?;
    mmio.write_u32(UMAC_REO + 0x10, hash)
        .map_err(|_| HalError::DeviceFault)
}

// PORT-MAP: reusable

const COMMAND_BYTES: usize = 40;
const STATUS_BYTES: usize = 104;
const NEED_STATUS: u32 = 1 << 0;
const STATS_CLEAR: u32 = 1 << 1;
const FLUSH_BLOCK_LATER: u32 = 1 << 2;
const FLUSH_NO_INVALIDATE: u32 = 1 << 4;
const FLUSH_FORWARD_ALL: u32 = 1 << 5;
const FLUSH_ALL: u32 = 1 << 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReoCommandKind {
    QueueStats,
    FlushQueue,
    FlushCache,
    UnblockCache,
    FlushTimeoutList,
    UpdateRxQueue,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReoCommandParams {
    pub flags: u32,
    pub update0: u32,
    pub update1: u32,
    pub update2: u32,
    pub pn: [u32; 4],
    pub rx_queue_number: u16,
    pub ba_window_size: u16,
    pub pn_size: u8,
}
impl ReoCommandParams {
    pub const fn need_status(mut self) -> Self {
        self.flags |= NEED_STATUS;
        self
    }
    pub const fn clear_stats(mut self) -> Self {
        self.flags |= STATS_CLEAR;
        self
    }
    pub const fn block_after_flush(mut self) -> Self {
        self.flags |= FLUSH_BLOCK_LATER;
        self
    }
    pub const fn flush_without_invalidate(mut self) -> Self {
        self.flags |= FLUSH_NO_INVALIDATE;
        self
    }
    pub const fn forward_all_mpdus(mut self) -> Self {
        self.flags |= FLUSH_FORWARD_ALL;
        self
    }
    pub const fn flush_all(mut self) -> Self {
        self.flags |= FLUSH_ALL;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReoResources {
    pub available_block_resources: u8,
    pub current_block_index: u8,
}

/// Port of `ath11k_hal_reo_init_cmd_ring`.
pub fn initialize_command_ring<B: Backend>(memory: &mut RingMemory<B>) -> Result<(), HalError> {
    for entry in 0..memory.entries {
        let offset = usize::from(entry) * usize::from(memory.entry_bytes) + 4;
        memory
            .dma
            .write(offset, &u32::from(entry + 1).to_le_bytes())
            .map_err(|_| HalError::DeviceFault)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketNumberType {
    None,
    Wpa,
    WapiEven,
    WapiUneven,
}

/// A REO queue descriptor plus the three queue-extension descriptors Linux
/// initializes for QoS TIDs. Non-QoS uses the first 128 bytes only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReoQueueDescriptor {
    bytes: [u8; 512],
    length: usize,
}
impl ReoQueueDescriptor {
    /// Port of `ath11k_hal_reo_qdesc_setup`.
    pub fn new(
        tid: u8,
        mut ba_window_size: u32,
        start_sequence: u32,
        pn: PacketNumberType,
    ) -> Self {
        let mut bytes = [0; 512];
        put(&mut bytes, 0, 0xddbeef00 | 0x84);
        put(&mut bytes, 1, u32::from(tid));
        let ac = if tid == 0 || tid == 3 {
            0
        } else if tid == 1 || tid == 2 {
            1
        } else if tid == 4 || tid == 5 {
            2
        } else {
            3
        };
        let mut info0 = 1 | 1 << 1 | ac << 5;
        if ba_window_size < 1 {
            ba_window_size = 1;
        }
        if ba_window_size == 1 && tid != 16 {
            ba_window_size += 1;
        }
        if ba_window_size == 1 {
            info0 |= 1 << 8;
        }
        info0 |= ((ba_window_size - 1) & 0xff) << 11;
        if pn == PacketNumberType::Wpa {
            info0 |= 1 << 19 | 1 << 23;
        }
        info0 |= 1 << 25;
        put(&mut bytes, 2, info0);
        if start_sequence <= 0xfff {
            put(&mut bytes, 3, (start_sequence & 0xfff) << 1);
        }
        let length = if tid == 16 { 128 } else { 512 };
        if tid != 16 {
            for (offset, magic) in [(128, 0xadbeef_u32), (256, 0xbdbeef), (384, 0xcdbeef)] {
                put(&mut bytes, offset / 4, magic << 8 | 0x94);
            }
        }
        Self { bytes, length }
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReoCommand([u8; COMMAND_BYTES]);
impl ReoCommand {
    pub fn encode<B: Backend, D: Direction>(
        command_number: u16,
        kind: ReoCommandKind,
        queue: &DeviceAddress<'_, B, D>,
        mut params: ReoCommandParams,
        resources: &mut ReoResources,
    ) -> Result<Self, HalError> {
        let mut bytes = [0; COMMAND_BYTES];
        let address = queue.bits();
        let status = if params.flags & NEED_STATUS != 0 {
            1 << 16
        } else {
            0
        };
        put(&mut bytes, 1, u32::from(command_number) | status);
        match kind {
            ReoCommandKind::QueueStats => {
                put(&mut bytes, 0, tlv(306, 36));
                put(&mut bytes, 2, address as u32);
                put(
                    &mut bytes,
                    3,
                    ((address >> 32) as u32 & 0xff)
                        | if params.flags & STATS_CLEAR != 0 {
                            1 << 8
                        } else {
                            0
                        },
                );
            }
            ReoCommandKind::FlushCache => {
                let mut info = (address >> 32) as u32 & 0xff;
                if params.flags & FLUSH_FORWARD_ALL != 0 {
                    info |= 1 << 8;
                }
                if params.flags & FLUSH_BLOCK_LATER != 0 {
                    let slot = (!resources.available_block_resources).trailing_zeros() as u8;
                    if slot >= 3 {
                        return Err(HalError::NoResources);
                    }
                    resources.current_block_index = slot;
                    info |= 1 << 13 | u32::from(slot) << 10;
                }
                if params.flags & FLUSH_NO_INVALIDATE != 0 {
                    info |= 1 << 12;
                }
                if params.flags & FLUSH_ALL != 0 {
                    info |= 1 << 14;
                }
                put(&mut bytes, 0, tlv(308, 36));
                put(&mut bytes, 2, address as u32);
                put(&mut bytes, 3, info);
            }
            ReoCommandKind::UpdateRxQueue => {
                put(&mut bytes, 0, tlv(419, 36));
                put(&mut bytes, 2, address as u32);
                // The update masks intentionally have the same bit positions
                // as the hardware descriptor. Linux omits update0 PN_ERR.
                put(
                    &mut bytes,
                    3,
                    ((address >> 32) as u32 & 0xff) | params.update0 & 0x6fff_ff00,
                );
                put(
                    &mut bytes,
                    4,
                    u32::from(params.rx_queue_number) | params.update1 & 0xffff_0000,
                );
                params.pn_size = match params.pn_size {
                    24 => 0,
                    48 => 1,
                    128 => 2,
                    value => value,
                };
                if params.ba_window_size < 1 {
                    params.ba_window_size = 1;
                }
                if params.ba_window_size == 1 {
                    params.ba_window_size += 1;
                }
                put(
                    &mut bytes,
                    5,
                    (u32::from(params.ba_window_size - 1) & 0xff)
                        | (u32::from(params.pn_size) & 3) << 8
                        | params.update2 & 0x01ff_fc00,
                );
                for (index, pn) in params.pn.into_iter().enumerate() {
                    put(&mut bytes, 6 + index, pn);
                }
            }
            ReoCommandKind::FlushQueue
            | ReoCommandKind::UnblockCache
            | ReoCommandKind::FlushTimeoutList => {
                return Err(HalError::Unsupported);
            }
        }
        Ok(Self(bytes))
    }
    pub const fn command_number(&self) -> u16 {
        word(&self.0, 1) as u16
    }
    pub fn into_descriptor(self) -> Descriptor {
        Descriptor::new(Vec::from(self.0), COMMAND_BYTES).expect("fixed REO command layout")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReoStatusHeader {
    pub command_number: u16,
    pub execution_time_us: u16,
    pub execution_status: u8,
    pub timestamp: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReoStatusKind {
    UpdateRxQueue,
    QueueStats,
    FlushQueue,
    FlushCache,
    UnblockCache,
    FlushTimeoutList,
    DescriptorThreshold,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReoStatus {
    pub kind: ReoStatusKind,
    pub header: ReoStatusHeader,
}
impl ReoStatus {
    /// Classifies the status TLV and decodes its uniform status header.
    pub fn decode(descriptor: &Descriptor) -> Result<Self, HalError> {
        let header = ReoStatusHeader::decode(descriptor)?;
        let tag = (word(descriptor.bytes(), 0) >> 1) & 0x1ff;
        let kind = match tag {
            153 => ReoStatusKind::UpdateRxQueue,
            311 => ReoStatusKind::QueueStats,
            312 => ReoStatusKind::FlushQueue,
            313 => ReoStatusKind::FlushCache,
            314 => ReoStatusKind::UnblockCache,
            342 => ReoStatusKind::FlushTimeoutList,
            345 => ReoStatusKind::DescriptorThreshold,
            _ => return Err(HalError::Unsupported),
        };
        Ok(Self { kind, header })
    }
}
impl ReoStatusHeader {
    /// Port of `ath11k_hal_reo_process_status` and the uniform status header.
    pub fn decode(descriptor: &Descriptor) -> Result<Self, HalError> {
        if descriptor.bytes().len() != STATUS_BYTES {
            return Err(HalError::WrongDescriptorLength);
        }
        let info = word(descriptor.bytes(), 1);
        Ok(Self {
            command_number: info as u16,
            execution_time_us: ((info >> 16) & 0x3ff) as u16,
            execution_status: ((info >> 26) & 3) as u8,
            timestamp: word(descriptor.bytes(), 2),
        })
    }
}

const fn tlv(tag: u32, length: u32) -> u32 {
    (tag & 0x1ff) << 1 | (length & 0xffff) << 10
}
fn put(bytes: &mut [u8], index: usize, value: u32) {
    bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
}
const fn word(bytes: &[u8], index: usize) -> u32 {
    let o = index * 4;
    u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_header_is_checked() {
        let mut bytes = alloc::vec![0; STATUS_BYTES];
        put(&mut bytes, 0, tlv(313, 100));
        put(&mut bytes, 1, 0x0a55_1234);
        put(&mut bytes, 2, 0x89ab_cdef);
        let d = Descriptor::new(bytes, STATUS_BYTES).unwrap();
        let h = ReoStatusHeader::decode(&d).unwrap();
        assert_eq!(h.command_number, 0x1234);
        assert_eq!(h.execution_time_us, 0x255);
        assert_eq!(h.execution_status, 2);
        assert_eq!(h.timestamp, 0x89ab_cdef);
        assert_eq!(
            ReoStatus::decode(&d).unwrap().kind,
            ReoStatusKind::FlushCache
        );
    }

    #[test]
    fn queue_descriptor_matches_linux_layout() {
        let qos = ReoQueueDescriptor::new(3, 1, 0x456, PacketNumberType::Wpa);
        assert_eq!(qos.bytes().len(), 512);
        assert_eq!(word(qos.bytes(), 0), 0xddbeef84);
        assert_eq!(word(qos.bytes(), 1), 3);
        assert_eq!(word(qos.bytes(), 3), 0x456 << 1);
        assert_eq!(word(qos.bytes(), 32), 0xadbeef94);

        let non_qos = ReoQueueDescriptor::new(16, 0, 0x1000, PacketNumberType::None);
        assert_eq!(non_qos.bytes().len(), 128);
        assert_eq!(word(non_qos.bytes(), 3), 0);
    }
}
