//! Little-endian WCN6750 data-path descriptor layouts.
//!
//! These layouts are transcribed from the pinned Linux ath11k `hal_desc.h`
//! and `hal_rx.h` oracle. They deliberately model bytes rather than native
//! Rust integer layout: descriptors have the same representation on every
//! host, including big-endian hosts, without packed references or unsafe code.

use crate::Descriptor;
use alloc::vec::Vec;
use ath11k_platform_backend::{Backend, DeviceAddress, Direction};

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

impl TclDataCommand {
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

// `struct hal_reo_entrance_ring` (RXDMA destination ring entry).
fixed_descriptor!(ReoEntranceRing, 32);

impl ReoEntranceRing {
    pub fn buffer_address(&self) -> RxdmaBufferRing {
        RxdmaBufferRing::from_bytes(&self.0[..8]).expect("embedded fixed layout")
    }
    pub fn mpdu(&self) -> RxMpduDescriptor {
        RxMpduDescriptor::from_bytes(&self.0[8..16]).expect("embedded fixed layout")
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
    pub fn mpdu(&self) -> RxMpduDescriptor {
        RxMpduDescriptor::from_bytes(&self.0[8..16]).expect("embedded fixed layout")
    }
    pub fn msdu(&self) -> RxMsduDescriptor {
        RxMsduDescriptor::from_bytes(&self.0[16..24]).expect("embedded fixed layout")
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
    field_accessors!(buffer_timestamp, set_buffer_timestamp, 4, 0xffff_e000, u32);
    field_accessors!(peer_id, set_peer_id, 7, 0xffff, u16);
    field_accessors!(tid, set_tid, 7, 0x000f_0000, u8);
    field_accessors!(ring_id, set_ring_id, 7, 0x0ff0_0000, u8);
    field_accessors!(looping_count, set_looping_count, 7, 0xf000_0000, u8);
}

// `struct hal_ce_srng_src_desc`.
fixed_descriptor!(CeSourceDescriptor, 16);
impl CeSourceDescriptor {
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

// WCN6750 uses the QCN9074 `struct hal_rx_mpdu_info_qcn9074` layout.
fixed_descriptor!(RxMpduInfoWcn6750, 92);
impl RxMpduInfoWcn6750 {
    field_accessors!(peer_id, set_peer_id, 10, 0xffff_0000, u16);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcl_data_command_is_byte_exact_and_checked() {
        let mut addr = RxdmaBufferRing::new();
        addr.set_address_bits(0xab_0123_4567).unwrap();
        addr.set_return_buffer_manager(5).unwrap();
        addr.set_software_cookie(0x1a_bcde).unwrap();

        let mut cmd = TclDataCommand::new();
        cmd.set_buffer_address(&addr);
        cmd.set_encapsulation_type(2).unwrap();
        cmd.set_search_type(1).unwrap();
        cmd.set_address_search_enable(3).unwrap();
        cmd.set_command_number(0x1234).unwrap();
        cmd.set_data_length(0x5678).unwrap();
        cmd.set_ipv4_checksum(true);
        cmd.set_to_firmware(true);
        cmd.set_packet_offset(0x101).unwrap();
        cmd.set_buffer_timestamp(0x45678).unwrap();
        cmd.set_buffer_timestamp_valid(true);
        cmd.set_tid_overwrite(true);
        cmd.set_tid(9).unwrap();
        cmd.set_lmac_id(2).unwrap();
        cmd.set_dscp_tid_table(0x2a).unwrap();
        cmd.set_search_index(0x8_7654).unwrap();
        cmd.set_cache_set(0xd).unwrap();
        cmd.set_ring_id(0x5a).unwrap();
        cmd.set_looping_count(0xc).unwrap();

        assert_eq!(
            cmd.as_bytes(),
            &[
                0x67, 0x45, 0x23, 0x01, 0xab, 0xed, 0xe6, 0xd5, 0x08, 0xd0, 0x34, 0x12, 0x78, 0x56,
                0xa1, 0x80, 0x78, 0x56, 0xac, 0x09, 0x2a, 0x95, 0x21, 0x36, 0x00, 0x00, 0xa0, 0xc5,
            ]
        );
        assert_eq!(
            cmd.set_encapsulation_type(4),
            Err(LayoutError::FieldValueOutOfRange)
        );
        assert_eq!(
            TclDataCommand::from_bytes(&[0; 27]),
            Err(LayoutError::WrongLength {
                expected: 28,
                actual: 27
            })
        );
    }

    #[test]
    fn reo_and_rxdma_masks_land_in_oracle_words() {
        let mut entrance = ReoEntranceRing::new();
        entrance.set_queue_address_bits(0x7e_89ab_cdef).unwrap();
        entrance.set_mpdu_byte_count(0x2345).unwrap();
        entrance.set_reo_destination(0x12).unwrap();
        entrance.set_frameless_bar(true);
        entrance.set_rxdma_push_reason(2).unwrap();
        entrance.set_rxdma_error_code(0x13).unwrap();
        entrance.set_ring_id(0xa5).unwrap();
        entrance.set_looping_count(0xb).unwrap();
        assert_eq!(
            &entrance.as_bytes()[16..],
            &[
                0xef, 0xcd, 0xab, 0x89, 0x7e, 0x45, 0xa3, 0x0c, 0x4e, 0, 0, 0, 0, 0, 0x50, 0xba
            ]
        );

        let mut dest = ReoDestinationRing::new();
        dest.set_queue_address_bits(0x55_1234_5678).unwrap();
        dest.set_buffer_type(1).unwrap();
        dest.set_push_reason(2).unwrap();
        dest.set_error_code(0x1d).unwrap();
        dest.set_rx_queue_number(0xbeef).unwrap();
        dest.set_reorder_info_valid(true);
        dest.set_reorder_opcode(0xa).unwrap();
        dest.set_reorder_slot(0x7f).unwrap();
        dest.set_ring_id(0x33).unwrap();
        dest.set_looping_count(0xe).unwrap();
        assert_eq!(
            &dest.as_bytes()[24..36],
            &[
                0x78, 0x56, 0x34, 0x12, 0x55, 0xed, 0xef, 0xbe, 0xd5, 0x0f, 0, 0
            ]
        );
        assert_eq!(&dest.as_bytes()[60..], &[0, 0, 0x30, 0xe3]);
    }

    #[test]
    fn ce_and_wbm_layouts_are_little_endian() {
        let mut source = CeSourceDescriptor::new();
        source.set_address_bits(0x12_dead_beef).unwrap();
        source.set_hash_enable(true);
        source.set_gather(true);
        source.set_length(0x3456).unwrap();
        source.set_metadata(0x789a).unwrap();
        source.set_ring_id(0xbc).unwrap();
        source.set_looping_count(0xd).unwrap();
        assert_eq!(
            source.as_bytes(),
            &[
                0xef, 0xbe, 0xad, 0xde, 0x12, 0x09, 0x56, 0x34, 0x9a, 0x78, 0, 0, 0, 0, 0xc0, 0xdb
            ]
        );

        let mut release = WbmReleaseRing::new();
        release.set_release_source(4).unwrap();
        release.set_descriptor_type(5).unwrap();
        release.set_first_msdu_index(0xa).unwrap();
        release.set_internal_error(true);
        release.set_tqm_status_number(0xabcdef).unwrap();
        release.set_transmit_count(0x55).unwrap();
        release.set_peer_id(0x1234).unwrap();
        release.set_tid(9).unwrap();
        release.set_ring_id(0x67).unwrap();
        release.set_looping_count(0xe).unwrap();
        assert_eq!(
            &release.as_bytes()[8..16],
            &[0x44, 0x15, 0, 0x80, 0xef, 0xcd, 0xab, 0x55]
        );
        assert_eq!(&release.as_bytes()[28..], &[0x34, 0x12, 0x79, 0xe6]);
    }

    #[test]
    fn rx_end_family_offsets_match_oracle() {
        let mut mpdu = RxMpduInfoWcn6750::new();
        mpdu.set_peer_id(0x1234).unwrap();
        mpdu.set_mpdu_length(0x2345).unwrap();
        assert_eq!(&mpdu.as_bytes()[40..44], &[0, 0, 0x34, 0x12]);
        assert_eq!(&mpdu.as_bytes()[52..56], &[0x45, 0x23, 0, 0]);

        let mut stats = RxPpduEndUserStats::new();
        stats.set_mpdu_fcs_error_count(0x155).unwrap();
        stats.set_mpdu_fcs_ok_count(0x101).unwrap();
        stats.set_frame_control_valid(true);
        stats.set_packet_type(0xa).unwrap();
        stats.set_frame_control(0x8899).unwrap();
        stats.set_ast_index(0x6677).unwrap();
        stats.set_udp_msdu_count(0x1122).unwrap();
        stats.set_tcp_msdu_count(0x3344).unwrap();
        stats.set_mpdu_ok_byte_count(0x123456).unwrap();
        assert_eq!(
            &stats.as_bytes()[8..20],
            &[
                0, 0, 0x55, 0x01, 0x01, 0x03, 0xa0, 0, 0x77, 0x66, 0x99, 0x88
            ]
        );
        assert_eq!(&stats.as_bytes()[36..40], &[0x22, 0x11, 0x44, 0x33]);
        assert_eq!(&stats.as_bytes()[68..72], &[0x56, 0x34, 0x12, 0]);

        let mut duration = RxPpduEndDuration::new();
        duration.set_duration(0xabcdef).unwrap();
        assert_eq!(&duration.as_bytes()[36..40], &[0xef, 0xcd, 0xab, 0]);
        assert_eq!(
            duration.set_duration(0x0100_0000),
            Err(LayoutError::FieldValueOutOfRange)
        );
    }

    #[test]
    fn monitor_tlv_header_uses_linux_bit_positions() {
        let mut header = RxMonitorTlvHeader::new();
        header.set_tag(0x155).unwrap();
        header.set_length(0x4567).unwrap();
        header.set_user_id(0x2a).unwrap();
        assert_eq!(header.as_bytes(), &[0xaa, 0x9d, 0x15, 0xa9]);
        assert_eq!(header.tag(), 0x155);
        assert_eq!(header.length(), 0x4567);
        assert_eq!(header.user_id(), 0x2a);
    }
}
