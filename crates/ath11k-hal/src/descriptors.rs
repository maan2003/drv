//! Little-endian WCN6750 data-path descriptor layouts.
//!
//! These layouts are transcribed from the pinned Linux ath11k `hal_desc.h`
//! and `hal_rx.h` oracle. They deliberately model bytes rather than native
//! Rust integer layout: descriptors have the same representation on every
//! host, including big-endian hosts, without packed references or unsafe code.

use crate::Descriptor;
use alloc::vec::Vec;
use ath11k_platform_backend::{
    Backend, DeviceAddress, Direction, FromDevice, MmioRegion, ToDevice,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutError {
    WrongLength { expected: usize, actual: usize },
    FieldValueOutOfRange,
}

fn read_word(bytes: &[u8], word: usize) -> u32 {
    let offset = word * 4;
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn write_word(bytes: &mut [u8], word: usize, value: u32) {
    let offset = word * 4;
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn field(bytes: &[u8], word: usize, mask: u32) -> u32 {
    (read_word(bytes, word) & mask) >> mask.trailing_zeros()
}

fn set_field(bytes: &mut [u8], word: usize, mask: u32, value: u32) -> Result<(), LayoutError> {
    let shift = mask.trailing_zeros();
    if value > (mask >> shift) {
        return Err(LayoutError::FieldValueOutOfRange);
    }
    let old = read_word(bytes, word);
    write_word(bytes, word, (old & !mask) | (value << shift));
    Ok(())
}

fn flag(bytes: &[u8], word: usize, mask: u32) -> bool {
    read_word(bytes, word) & mask != 0
}

fn set_flag(bytes: &mut [u8], word: usize, mask: u32, value: bool) {
    let old = read_word(bytes, word);
    write_word(bytes, word, if value { old | mask } else { old & !mask });
}

macro_rules! fixed_descriptor {
    ($name:ident, $size:expr) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub struct $name([u8; $size]);

        impl $name {
            pub const LEN: usize = $size;

            pub const fn new() -> Self {
                Self([0; $size])
            }

            pub fn from_bytes(bytes: &[u8]) -> Result<Self, LayoutError> {
                let actual = bytes.len();
                let bytes: [u8; $size] =
                    bytes.try_into().map_err(|_| LayoutError::WrongLength {
                        expected: $size,
                        actual,
                    })?;
                Ok(Self(bytes))
            }

            pub const fn as_bytes(&self) -> &[u8; $size] {
                &self.0
            }

            pub fn into_descriptor(self) -> Descriptor {
                // The fixed-size type proves this length before allocation.
                Descriptor::new(Vec::from(self.0), $size).expect("fixed descriptor length")
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl TryFrom<&[u8]> for $name {
            type Error = LayoutError;

            fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
                Self::from_bytes(value)
            }
        }
    };
}

macro_rules! field_accessors {
    ($get:ident, $set:ident, $word:expr, $mask:expr, $ty:ty) => {
        pub fn $get(&self) -> $ty {
            field(&self.0, $word, $mask) as $ty
        }

        pub fn $set(&mut self, value: $ty) -> Result<(), LayoutError> {
            set_field(&mut self.0, $word, $mask, value as u32)
        }
    };
}

macro_rules! flag_accessors {
    ($get:ident, $set:ident, $word:expr, $mask:expr) => {
        pub fn $get(&self) -> bool {
            flag(&self.0, $word, $mask)
        }

        pub fn $set(&mut self, value: bool) {
            set_flag(&mut self.0, $word, $mask, value);
        }
    };
}

// `struct ath11k_buffer_addr` / `struct hal_wbm_buffer_ring`.
fixed_descriptor!(RxdmaBufferRing, 8);

impl RxdmaBufferRing {
    pub fn address(&self) -> u64 {
        u64::from(read_word(&self.0, 0)) | (u64::from(field(&self.0, 1, 0xff)) << 32)
    }

    pub fn set_address<B: Backend, D: Direction>(
        &mut self,
        address: &DeviceAddress<'_, B, D>,
    ) -> Result<(), LayoutError> {
        self.set_address_bits(address.bits())
    }

    fn set_address_bits(&mut self, address: u64) -> Result<(), LayoutError> {
        if address >= (1_u64 << 40) {
            return Err(LayoutError::FieldValueOutOfRange);
        }
        write_word(&mut self.0, 0, address as u32);
        set_field(&mut self.0, 1, 0xff, (address >> 32) as u32)
    }

    field_accessors!(
        return_buffer_manager,
        set_return_buffer_manager,
        1,
        0x0000_0700,
        u8
    );
    field_accessors!(software_cookie, set_software_cookie, 1, 0xffff_f800, u32);
}

// `struct rx_mpdu_desc`, embedded in REO descriptors.
fixed_descriptor!(RxMpduDescriptor, 8);

impl RxMpduDescriptor {
    field_accessors!(msdu_count, set_msdu_count, 0, 0x0000_00ff, u8);
    field_accessors!(sequence_number, set_sequence_number, 0, 0x000f_ff00, u16);
    flag_accessors!(fragment, set_fragment, 0, 1 << 20);
    flag_accessors!(retry, set_retry, 0, 1 << 21);
    flag_accessors!(ampdu, set_ampdu, 0, 1 << 22);
    flag_accessors!(bar_frame, set_bar_frame, 0, 1 << 23);
    flag_accessors!(pn_valid, set_pn_valid, 0, 1 << 24);
    flag_accessors!(source_address_valid, set_source_address_valid, 0, 1 << 25);
    flag_accessors!(
        destination_address_valid,
        set_destination_address_valid,
        0,
        1 << 27
    );
    flag_accessors!(raw_mpdu, set_raw_mpdu, 0, 1 << 30);
    field_accessors!(peer_id, set_peer_id, 1, 0x0000_ffff, u16);
}

// `struct rx_msdu_desc`, embedded in a REO destination descriptor.
fixed_descriptor!(RxMsduDescriptor, 8);

impl RxMsduDescriptor {
    flag_accessors!(first_in_mpdu, set_first_in_mpdu, 0, 1 << 0);
    flag_accessors!(last_in_mpdu, set_last_in_mpdu, 0, 1 << 1);
    flag_accessors!(continuation, set_continuation, 0, 1 << 2);
    field_accessors!(length, set_length, 0, 0x0001_fff8, u16);
    field_accessors!(reo_destination, set_reo_destination, 0, 0x003e_0000, u8);
    flag_accessors!(drop, set_drop, 0, 1 << 22);
    flag_accessors!(source_address_valid, set_source_address_valid, 0, 1 << 23);
    flag_accessors!(
        destination_address_valid,
        set_destination_address_valid,
        0,
        1 << 25
    );
}

// `struct hal_tcl_data_cmd` (seven little-endian dwords).
fixed_descriptor!(TclDataCommand, 28);

/// Source-shaped arguments to `ath11k_hal_tx_cmd_desc_setup`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TxCommandInfo {
    pub metadata_flags: u16,
    pub descriptor_id: u32,
    pub descriptor_type: u8,
    pub encapsulation_type: u8,
    pub data_length: u32,
    pub packet_offset: u32,
    pub encryption_type: u8,
    pub flags0: u32,
    pub flags1: u32,
    pub address_search_flags: u16,
    pub bss_ast_hash: u16,
    pub bss_ast_index: u16,
    pub tid: u8,
    pub search_type: u8,
    pub lmac_id: u8,
    pub dscp_tid_table: u8,
    pub mesh_enable: bool,
    pub return_buffer_manager: u8,
}

impl TclDataCommand {
    /// Port of `ath11k_hal_tx_cmd_desc_setup`, including WCN6750's QCN9074
    /// mesh-enable bit in info3[31:30].
    pub fn for_transmit<B: Backend>(
        address: &DeviceAddress<'_, B, ToDevice>,
        info: TxCommandInfo,
    ) -> Self {
        let mut command = Self::new();
        let mut buffer = RxdmaBufferRing::new();
        buffer
            .set_address_bits(address.bits())
            .expect("40-bit TCL address");
        // FIELD_PREP masks its inputs in the C implementation.
        let high_address = read_word(&buffer.0, 1);
        write_word(
            &mut buffer.0,
            1,
            high_address
                | (u32::from(info.return_buffer_manager) & 7) << 8
                | (info.descriptor_id & 0x1f_ffff) << 11,
        );
        command.set_buffer_address(&buffer);
        write_word(
            &mut command.0,
            2,
            u32::from(info.descriptor_type) & 1
                | (u32::from(info.encapsulation_type) & 3) << 2
                | (u32::from(info.encryption_type) & 15) << 4
                | (u32::from(info.search_type) & 3) << 12
                | (u32::from(info.address_search_flags) & 3) << 14
                | u32::from(info.metadata_flags) << 16,
        );
        write_word(
            &mut command.0,
            3,
            info.flags0 | (info.data_length & 0xffff) | (info.packet_offset & 0x1ff) << 23,
        );
        write_word(
            &mut command.0,
            4,
            info.flags1 | (u32::from(info.tid) & 15) << 22 | (u32::from(info.lmac_id) & 3) << 26,
        );
        let mesh = if info.mesh_enable { 1 << 30 } else { 0 };
        write_word(
            &mut command.0,
            5,
            (u32::from(info.dscp_tid_table) & 0x3f)
                | (u32::from(info.bss_ast_index) & 0xfffff) << 6
                | (u32::from(info.bss_ast_hash) & 15) << 26
                | mesh,
        );
        write_word(&mut command.0, 6, 0);
        command
    }
    pub fn buffer_address(&self) -> RxdmaBufferRing {
        RxdmaBufferRing::from_bytes(&self.0[..8]).expect("embedded fixed layout")
    }

    pub fn set_buffer_address(&mut self, address: &RxdmaBufferRing) {
        self.0[..8].copy_from_slice(address.as_bytes());
    }

    flag_accessors!(extension_descriptor, set_extension_descriptor, 2, 1 << 0);
    flag_accessors!(epd, set_epd, 2, 1 << 1);
    field_accessors!(
        encapsulation_type,
        set_encapsulation_type,
        2,
        0x0000_000c,
        u8
    );
    field_accessors!(encryption_type, set_encryption_type, 2, 0x0000_00f0, u8);
    field_accessors!(search_type, set_search_type, 2, 0x0000_3000, u8);
    field_accessors!(
        address_search_enable,
        set_address_search_enable,
        2,
        0x0000_c000,
        u8
    );
    field_accessors!(command_number, set_command_number, 2, 0xffff_0000, u16);
    field_accessors!(data_length, set_data_length, 3, 0x0000_ffff, u16);
    flag_accessors!(ipv4_checksum, set_ipv4_checksum, 3, 1 << 16);
    flag_accessors!(udp_ipv4_checksum, set_udp_ipv4_checksum, 3, 1 << 17);
    flag_accessors!(udp_ipv6_checksum, set_udp_ipv6_checksum, 3, 1 << 18);
    flag_accessors!(tcp_ipv4_checksum, set_tcp_ipv4_checksum, 3, 1 << 19);
    flag_accessors!(tcp_ipv6_checksum, set_tcp_ipv6_checksum, 3, 1 << 20);
    flag_accessors!(to_firmware, set_to_firmware, 3, 1 << 21);
    field_accessors!(packet_offset, set_packet_offset, 3, 0xff80_0000, u16);
    field_accessors!(buffer_timestamp, set_buffer_timestamp, 4, 0x0007_ffff, u32);
    flag_accessors!(
        buffer_timestamp_valid,
        set_buffer_timestamp_valid,
        4,
        1 << 19
    );
    flag_accessors!(tid_overwrite, set_tid_overwrite, 4, 1 << 21);
    field_accessors!(tid, set_tid, 4, 0x03c0_0000, u8);
    field_accessors!(lmac_id, set_lmac_id, 4, 0x0c00_0000, u8);
    field_accessors!(dscp_tid_table, set_dscp_tid_table, 5, 0x0000_003f, u8);
    field_accessors!(search_index, set_search_index, 5, 0x03ff_ffc0, u32);
    field_accessors!(cache_set, set_cache_set, 5, 0x3c00_0000, u8);
    field_accessors!(ring_id, set_ring_id, 6, 0x0ff0_0000, u8);
    field_accessors!(looping_count, set_looping_count, 6, 0xf000_0000, u8);
}

/// Port of `ath11k_hal_tx_set_dscp_tid_map`. The exact C read/write sequence
/// is retained so firmware never observes a partially enabled table update.
pub fn program_dscp_tid_map<B: Backend>(
    mmio: &MmioRegion<B>,
    table_id: usize,
) -> Result<(), crate::HalError> {
    const CONTROL: usize = 0x00a4_4014;
    const MAP: usize = 0x00a4_402c;
    let control = mmio
        .read_u32(CONTROL)
        .map_err(|_| crate::HalError::DeviceFault)?;
    mmio.write_u32(CONTROL, control | 1 << 17)
        .map_err(|_| crate::HalError::DeviceFault)?;
    let base = MAP + 24 * table_id;
    // Eight equal three-bit values pack into each three-byte group; two
    // groups form each little-endian register word in the Linux byte array.
    for word in 0..6 {
        // Direct construction below is clearer and exactly packs DSCP/8.
        let mut value = 0_u32;
        for byte in 0..4 {
            let packed_bit = word * 32 + byte * 8;
            for bit in 0..8 {
                let stream_bit = packed_bit + bit;
                let dscp_index = stream_bit / 3;
                if dscp_index < 64 {
                    value |=
                        ((((dscp_index / 8) >> (stream_bit % 3)) & 1) as u32) << (byte * 8 + bit);
                }
            }
        }
        mmio.write_u32(base + word * 4, value)
            .map_err(|_| crate::HalError::DeviceFault)?;
    }
    let control = mmio
        .read_u32(CONTROL)
        .map_err(|_| crate::HalError::DeviceFault)?;
    mmio.write_u32(CONTROL, control & !(1 << 17))
        .map_err(|_| crate::HalError::DeviceFault)
}

// `struct hal_reo_entrance_ring` (RXDMA destination ring entry).
fixed_descriptor!(ReoEntranceRing, 32);

impl ReoEntranceRing {
    pub fn buffer_address(&self) -> RxdmaBufferRing {
        RxdmaBufferRing::from_bytes(&self.0[..8]).expect("embedded fixed layout")
    }
    pub fn set_buffer_address(&mut self, value: &RxdmaBufferRing) {
        self.0[..8].copy_from_slice(value.as_bytes());
    }
    pub fn mpdu(&self) -> RxMpduDescriptor {
        RxMpduDescriptor::from_bytes(&self.0[8..16]).expect("embedded fixed layout")
    }
    pub fn set_mpdu(&mut self, value: &RxMpduDescriptor) {
        self.0[8..16].copy_from_slice(value.as_bytes());
    }
    pub fn queue_address(&self) -> u64 {
        u64::from(read_word(&self.0, 4)) | (u64::from(field(&self.0, 5, 0xff)) << 32)
    }
    pub fn set_queue_address<B: Backend, D: Direction>(
        &mut self,
        value: &DeviceAddress<'_, B, D>,
    ) -> Result<(), LayoutError> {
        self.set_queue_address_bits(value.bits())
    }
    fn set_queue_address_bits(&mut self, value: u64) -> Result<(), LayoutError> {
        if value >= (1_u64 << 40) {
            return Err(LayoutError::FieldValueOutOfRange);
        }
        write_word(&mut self.0, 4, value as u32);
        set_field(&mut self.0, 5, 0xff, (value >> 32) as u32)
    }
    field_accessors!(mpdu_byte_count, set_mpdu_byte_count, 5, 0x003f_ff00, u16);
    field_accessors!(reo_destination, set_reo_destination, 5, 0x07c0_0000, u8);
    flag_accessors!(frameless_bar, set_frameless_bar, 5, 1 << 27);
    field_accessors!(rxdma_push_reason, set_rxdma_push_reason, 6, 0x3, u8);
    field_accessors!(rxdma_error_code, set_rxdma_error_code, 6, 0x7c, u8);
    field_accessors!(ring_id, set_ring_id, 7, 0x0ff0_0000, u8);
    field_accessors!(looping_count, set_looping_count, 7, 0xf000_0000, u8);
}

// `struct hal_reo_dest_ring`.
fixed_descriptor!(ReoDestinationRing, 64);

impl ReoDestinationRing {
    pub fn buffer_address(&self) -> RxdmaBufferRing {
        RxdmaBufferRing::from_bytes(&self.0[..8]).expect("embedded fixed layout")
    }
    pub fn set_buffer_address(&mut self, value: &RxdmaBufferRing) {
        self.0[..8].copy_from_slice(value.as_bytes());
    }
    pub fn mpdu(&self) -> RxMpduDescriptor {
        RxMpduDescriptor::from_bytes(&self.0[8..16]).expect("embedded fixed layout")
    }
    pub fn set_mpdu(&mut self, value: &RxMpduDescriptor) {
        self.0[8..16].copy_from_slice(value.as_bytes());
    }
    pub fn msdu(&self) -> RxMsduDescriptor {
        RxMsduDescriptor::from_bytes(&self.0[16..24]).expect("embedded fixed layout")
    }
    pub fn set_msdu(&mut self, value: &RxMsduDescriptor) {
        self.0[16..24].copy_from_slice(value.as_bytes());
    }
    pub fn queue_address(&self) -> u64 {
        u64::from(read_word(&self.0, 6)) | (u64::from(field(&self.0, 7, 0xff)) << 32)
    }
    pub fn set_queue_address<B: Backend, D: Direction>(
        &mut self,
        value: &DeviceAddress<'_, B, D>,
    ) -> Result<(), LayoutError> {
        self.set_queue_address_bits(value.bits())
    }
    fn set_queue_address_bits(&mut self, value: u64) -> Result<(), LayoutError> {
        if value >= (1_u64 << 40) {
            return Err(LayoutError::FieldValueOutOfRange);
        }
        write_word(&mut self.0, 6, value as u32);
        set_field(&mut self.0, 7, 0xff, (value >> 32) as u32)
    }
    field_accessors!(buffer_type, set_buffer_type, 7, 1 << 8, u8);
    field_accessors!(push_reason, set_push_reason, 7, 0x0000_0600, u8);
    field_accessors!(error_code, set_error_code, 7, 0x0000_f800, u8);
    field_accessors!(rx_queue_number, set_rx_queue_number, 7, 0xffff_0000, u16);
    flag_accessors!(reorder_info_valid, set_reorder_info_valid, 8, 1);
    field_accessors!(reorder_opcode, set_reorder_opcode, 8, 0x1e, u8);
    field_accessors!(reorder_slot, set_reorder_slot, 8, 0x1fe0, u8);
    field_accessors!(ring_id, set_ring_id, 15, 0x0ff0_0000, u8);
    field_accessors!(looping_count, set_looping_count, 15, 0xf000_0000, u8);
}

// `struct hal_wbm_release_ring`.
fixed_descriptor!(WbmReleaseRing, 32);

impl WbmReleaseRing {
    pub fn buffer_address(&self) -> RxdmaBufferRing {
        RxdmaBufferRing::from_bytes(&self.0[..8]).expect("embedded fixed layout")
    }
    pub fn set_buffer_address(&mut self, value: &RxdmaBufferRing) {
        self.0[..8].copy_from_slice(value.as_bytes());
    }
    field_accessors!(release_source, set_release_source, 2, 0x7, u8);
    field_accessors!(
        buffer_manager_action,
        set_buffer_manager_action,
        2,
        0x38,
        u8
    );
    field_accessors!(descriptor_type, set_descriptor_type, 2, 0x1c0, u8);
    field_accessors!(first_msdu_index, set_first_msdu_index, 2, 0x1e00, u8);
    field_accessors!(tqm_release_reason, set_tqm_release_reason, 2, 0x1e000, u8);
    field_accessors!(rxdma_push_reason, set_rxdma_push_reason, 2, 0x60000, u8);
    field_accessors!(rxdma_error_code, set_rxdma_error_code, 2, 0xf80000, u8);
    field_accessors!(reo_push_reason, set_reo_push_reason, 2, 0x0300_0000, u8);
    field_accessors!(reo_error_code, set_reo_error_code, 2, 0x7c00_0000, u8);
    flag_accessors!(internal_error, set_internal_error, 2, 1 << 31);
    field_accessors!(
        tqm_status_number,
        set_tqm_status_number,
        3,
        0x00ff_ffff,
        u32
    );
    field_accessors!(transmit_count, set_transmit_count, 3, 0x7f00_0000, u8);
    field_accessors!(ack_rssi, set_ack_rssi, 4, 0xff, u8);
    flag_accessors!(
        software_release_details_valid,
        set_software_release_details_valid,
        4,
        1 << 8
    );
    flag_accessors!(first_msdu, set_first_msdu, 4, 1 << 9);
    flag_accessors!(last_msdu, set_last_msdu, 4, 1 << 10);
    flag_accessors!(msdu_in_amsdu, set_msdu_in_amsdu, 4, 1 << 11);
    flag_accessors!(
        firmware_tx_notification,
        set_firmware_tx_notification,
        4,
        1 << 12
    );
    field_accessors!(buffer_timestamp, set_buffer_timestamp, 4, 0xffff_e000, u32);
    flag_accessors!(rate_valid, set_rate_valid, 5, 1);
    field_accessors!(rate_bandwidth, set_rate_bandwidth, 5, 0x6, u8);
    field_accessors!(rate_packet_type, set_rate_packet_type, 5, 0x78, u8);
    flag_accessors!(rate_stbc, set_rate_stbc, 5, 1 << 7);
    flag_accessors!(rate_ldpc, set_rate_ldpc, 5, 1 << 8);
    field_accessors!(rate_guard_interval, set_rate_guard_interval, 5, 0x600, u8);
    field_accessors!(rate_mcs, set_rate_mcs, 5, 0x7800, u8);
    flag_accessors!(rate_ofdma, set_rate_ofdma, 5, 1 << 15);
    field_accessors!(rate_ru_tones, set_rate_ru_tones, 5, 0x0fff_0000, u16);
    pub fn rate_tsf(&self) -> u32 {
        read_word(&self.0, 6)
    }
    pub fn set_rate_tsf(&mut self, value: u32) {
        write_word(&mut self.0, 6, value);
    }
    field_accessors!(peer_id, set_peer_id, 7, 0xffff, u16);
    field_accessors!(tid, set_tid, 7, 0x000f_0000, u8);
    field_accessors!(ring_id, set_ring_id, 7, 0x0ff0_0000, u8);
    field_accessors!(looping_count, set_looping_count, 7, 0xf000_0000, u8);
}

// `struct hal_ce_srng_src_desc`.
fixed_descriptor!(CeSourceDescriptor, 16);
impl CeSourceDescriptor {
    /// Port of `ath11k_hal_ce_src_set_desc`. Linux accepts `u32` values and
    /// `FIELD_PREP` retains the low 16 bits of length and transfer id.
    pub fn for_transfer<B: Backend>(
        address: &DeviceAddress<'_, B, ToDevice>,
        length: u32,
        transfer_id: u32,
        byte_swap_data: bool,
    ) -> Self {
        let mut descriptor = Self::new();
        descriptor
            .set_address_bits(address.bits())
            .expect("40-bit CE address");
        let address_high = field(&descriptor.0, 1, 0xff);
        write_word(
            &mut descriptor.0,
            1,
            address_high | u32::from(byte_swap_data) << 9 | (length & 0xffff) << 16,
        );
        write_word(&mut descriptor.0, 2, transfer_id & 0xffff);
        descriptor
    }
    pub fn address(&self) -> u64 {
        u64::from(read_word(&self.0, 0)) | (u64::from(field(&self.0, 1, 0xff)) << 32)
    }
    pub fn set_address<B: Backend, D: Direction>(
        &mut self,
        value: &DeviceAddress<'_, B, D>,
    ) -> Result<(), LayoutError> {
        self.set_address_bits(value.bits())
    }
    fn set_address_bits(&mut self, value: u64) -> Result<(), LayoutError> {
        if value >= (1_u64 << 40) {
            return Err(LayoutError::FieldValueOutOfRange);
        }
        write_word(&mut self.0, 0, value as u32);
        set_field(&mut self.0, 1, 0xff, (value >> 32) as u32)
    }
    flag_accessors!(hash_enable, set_hash_enable, 1, 1 << 8);
    flag_accessors!(byte_swap, set_byte_swap, 1, 1 << 9);
    flag_accessors!(destination_swap, set_destination_swap, 1, 1 << 10);
    flag_accessors!(gather, set_gather, 1, 1 << 11);
    field_accessors!(length, set_length, 1, 0xffff_0000, u16);
    field_accessors!(metadata, set_metadata, 2, 0xffff, u16);
    field_accessors!(ring_id, set_ring_id, 3, 0x0ff0_0000, u8);
    field_accessors!(looping_count, set_looping_count, 3, 0xf000_0000, u8);
}

// `struct hal_ce_srng_dest_desc`.
fixed_descriptor!(CeDestinationDescriptor, 8);
impl CeDestinationDescriptor {
    /// Port of `ath11k_hal_ce_dst_set_desc`.
    pub fn from_address<B: Backend>(address: &DeviceAddress<'_, B, FromDevice>) -> Self {
        let mut descriptor = Self::new();
        descriptor
            .set_address_bits(address.bits())
            .expect("40-bit CE address");
        descriptor
    }
    pub fn address(&self) -> u64 {
        u64::from(read_word(&self.0, 0)) | (u64::from(field(&self.0, 1, 0xff)) << 32)
    }
    pub fn set_address<B: Backend, D: Direction>(
        &mut self,
        value: &DeviceAddress<'_, B, D>,
    ) -> Result<(), LayoutError> {
        self.set_address_bits(value.bits())
    }
    fn set_address_bits(&mut self, value: u64) -> Result<(), LayoutError> {
        if value >= (1_u64 << 40) {
            return Err(LayoutError::FieldValueOutOfRange);
        }
        write_word(&mut self.0, 0, value as u32);
        set_field(&mut self.0, 1, 0xff, (value >> 32) as u32)
    }
    field_accessors!(ring_id, set_ring_id, 1, 0x0ff0_0000, u8);
    field_accessors!(looping_count, set_looping_count, 1, 0xf000_0000, u8);
}

// `struct hal_ce_srng_dst_status_desc`.
fixed_descriptor!(CeDestinationStatusDescriptor, 16);
impl CeDestinationStatusDescriptor {
    flag_accessors!(hash_enable, set_hash_enable, 0, 1 << 8);
    flag_accessors!(byte_swap, set_byte_swap, 0, 1 << 9);
    flag_accessors!(destination_swap, set_destination_swap, 0, 1 << 10);
    flag_accessors!(gather, set_gather, 0, 1 << 11);
    field_accessors!(length, set_length, 0, 0xffff_0000, u16);
    pub fn toeplitz_hash(&self) -> u64 {
        u64::from(read_word(&self.0, 1)) | (u64::from(read_word(&self.0, 2)) << 32)
    }
    pub fn set_toeplitz_hash(&mut self, value: u64) {
        write_word(&mut self.0, 1, value as u32);
        write_word(&mut self.0, 2, (value >> 32) as u32);
    }
    field_accessors!(metadata, set_metadata, 3, 0xffff, u16);
    field_accessors!(ring_id, set_ring_id, 3, 0x0ff0_0000, u8);
    field_accessors!(looping_count, set_looping_count, 3, 0xf000_0000, u8);

    /// Port of `ath11k_hal_ce_dst_status_get_length`: return and clear LEN.
    pub fn take_length(&mut self) -> u16 {
        let length = self.length();
        let flags = read_word(&self.0, 0) & !0xffff_0000;
        write_word(&mut self.0, 0, flags);
        length
    }
}

// `struct hal_rx_ppdu_start`.
fixed_descriptor!(RxPpduStart, 12);
impl RxPpduStart {
    field_accessors!(ppdu_id, set_ppdu_id, 0, 0xffff, u16);
    pub fn channel_number_word(&self) -> u32 {
        read_word(&self.0, 1)
    }
    pub fn set_channel_number_word(&mut self, value: u32) {
        write_word(&mut self.0, 1, value);
    }
    pub fn timestamp(&self) -> u32 {
        read_word(&self.0, 2)
    }
    pub fn set_timestamp(&mut self, value: u32) {
        write_word(&mut self.0, 2, value);
    }
}

// WCN6750 selects `struct hal_rx_mpdu_info_ipq8074` for monitor MPDU info.
fixed_descriptor!(RxMpduInfoWcn6750, 92);
impl RxMpduInfoWcn6750 {
    field_accessors!(peer_id, set_peer_id, 1, 0xffff_0000, u16);
    field_accessors!(mpdu_length, set_mpdu_length, 13, 0x3fff, u16);
}

// `struct hal_rx_ppdu_end_duration`.
fixed_descriptor!(RxPpduEndDuration, 56);
impl RxPpduEndDuration {
    field_accessors!(duration, set_duration, 9, 0x00ff_ffff, u32);
}

// `struct hal_rx_ppdu_end_user_stats`.
fixed_descriptor!(RxPpduEndUserStats, 92);
impl RxPpduEndUserStats {
    field_accessors!(
        mpdu_fcs_error_count,
        set_mpdu_fcs_error_count,
        2,
        0x03ff_0000,
        u16
    );
    field_accessors!(mpdu_fcs_ok_count, set_mpdu_fcs_ok_count, 3, 0x1ff, u16);
    flag_accessors!(frame_control_valid, set_frame_control_valid, 3, 1 << 9);
    flag_accessors!(qos_control_valid, set_qos_control_valid, 3, 1 << 10);
    flag_accessors!(ht_control_valid, set_ht_control_valid, 3, 1 << 11);
    field_accessors!(packet_type, set_packet_type, 3, 0x00f0_0000, u8);
    field_accessors!(ast_index, set_ast_index, 4, 0xffff, u16);
    field_accessors!(frame_control, set_frame_control, 4, 0xffff_0000, u16);
    field_accessors!(qos_control, set_qos_control, 5, 0xffff_0000, u16);
    pub fn ht_control(&self) -> u32 {
        read_word(&self.0, 6)
    }
    pub fn set_ht_control(&mut self, value: u32) {
        write_word(&mut self.0, 6, value);
    }
    field_accessors!(udp_msdu_count, set_udp_msdu_count, 9, 0xffff, u16);
    field_accessors!(tcp_msdu_count, set_tcp_msdu_count, 9, 0xffff_0000, u16);
    field_accessors!(other_msdu_count, set_other_msdu_count, 10, 0xffff, u16);
    field_accessors!(
        tcp_ack_msdu_count,
        set_tcp_ack_msdu_count,
        10,
        0xffff_0000,
        u16
    );
    field_accessors!(tid_bitmap, set_tid_bitmap, 12, 0xffff, u16);
    field_accessors!(tid_eosp_bitmap, set_tid_eosp_bitmap, 12, 0xffff_0000, u16);
    field_accessors!(
        mpdu_ok_byte_count,
        set_mpdu_ok_byte_count,
        17,
        0x01ff_ffff,
        u32
    );
    field_accessors!(
        mpdu_error_byte_count,
        set_mpdu_error_byte_count,
        19,
        0x01ff_ffff,
        u32
    );
}

// `struct hal_rx_ppdu_end_user_stats_ext`; the oracle defines no masks for
// its seven words, so this type intentionally exposes only exact bytes.
fixed_descriptor!(RxPpduEndUserStatsExt, 28);

// Four-byte monitor TLV header (`struct hal_tlv_hdr`).
fixed_descriptor!(RxMonitorTlvHeader, 4);
impl RxMonitorTlvHeader {
    field_accessors!(tag, set_tag, 0, 0x0000_03fe, u16);
    field_accessors!(length, set_length, 0, 0x03ff_fc00, u16);
    field_accessors!(user_id, set_user_id, 0, 0xfc00_0000, u8);
}
