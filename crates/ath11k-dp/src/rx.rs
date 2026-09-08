// PORT-MAP: wcn6750-specific
//! WCN6750 RX descriptor operations selected by `wcn6750_ops`.
//!
//! WCN6750 deliberately uses the QCN9074 RX TLV layout in the pinned source.

use crate::{DpError, PeerId};

pub const WCN6750_RX_DESCRIPTOR_BYTES: usize = 388;
pub const RX_HEADER_STATUS_BYTES: usize = 120;

const MSDU_END_INFO4: usize = 46;
const ATTENTION_INFO1: usize = 80;
const ATTENTION_INFO2: usize = 84;
const MSDU_START_INFO1: usize = 96;
const MSDU_START_INFO2: usize = 100;
const MSDU_START_INFO3: usize = 112;
const MSDU_START_PHY_METADATA: usize = 120;
const MPDU_START_TAG: usize = 136;
const MPDU_START_INFO9: usize = 168;
const MPDU_START_PHY_PPDU_ID: usize = 178;
const MPDU_START_SW_PEER_ID: usize = 182;
const MPDU_START_INFO11: usize = 184;
const MPDU_START_ADDR2: usize = 206;
const HEADER_STATUS: usize = 268;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Wcn6750RxDescriptor<'a> {
    bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RxDescriptorStatus {
    pub first_msdu: bool,
    pub last_msdu: bool,
    pub l3_padding: u8,
    pub msdu_done: bool,
    pub msdu_length_error: bool,
    pub fcs_error: bool,
    pub decrypt_error: bool,
    pub tkip_mic_error: bool,
    /// Pinned `ath11k_dp_rx_h_attn_mpdu_err` seven-class error map.
    pub mpdu_errors: u8,
    pub ip_checksum_failed: bool,
    pub l4_checksum_failed: bool,
    pub multicast_broadcast: bool,
    pub decrypted: bool,
    pub msdu_length: u16,
    pub decap_type: u8,
    pub mesh_control_present: bool,
    pub ldpc: bool,
    pub short_guard_interval: u8,
    pub mcs: u8,
    pub bandwidth: u8,
    pub packet_type: u8,
    pub spatial_stream_bitmap: u8,
    pub nss: u8,
    pub frequency: u32,
    pub tid: u8,
    pub peer: PeerId,
    pub sequence_control_valid: bool,
    pub frame_control_valid: bool,
    pub sequence_number: u16,
    pub encryption_info_valid: bool,
    pub encryption_type: u8,
    pub phy_ppdu_id: u16,
}

impl<'a> Wcn6750RxDescriptor<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, DpError> {
        if bytes.len() < WCN6750_RX_DESCRIPTOR_BYTES {
            return Err(DpError::MalformedDescriptor);
        }
        Ok(Self { bytes })
    }

    pub fn mpdu_start_valid(&self) -> bool {
        (self.u32(MPDU_START_TAG) >> 1) & 0x1ff == 207
    }

    pub fn address2(&self) -> Option<[u8; 6]> {
        let info11 = self.u32(MPDU_START_INFO11);
        if info11 & (1 << 3) == 0 {
            return None;
        }
        let a = &self.bytes[MPDU_START_ADDR2..MPDU_START_ADDR2 + 6];
        Some([a[0], a[1], a[2], a[3], a[4], a[5]])
    }

    pub fn header_status(&self) -> &'a [u8] {
        &self.bytes[HEADER_STATUS..HEADER_STATUS + RX_HEADER_STATUS_BYTES]
    }

    pub fn payload(&self) -> &'a [u8] {
        &self.bytes[WCN6750_RX_DESCRIPTOR_BYTES..]
    }

    pub fn status(&self) -> RxDescriptorStatus {
        let end4 = self.u16(MSDU_END_INFO4);
        let attention1 = self.u32(ATTENTION_INFO1);
        let attention2 = self.u32(ATTENTION_INFO2);
        let msdu1 = self.u32(MSDU_START_INFO1);
        let msdu2 = self.u32(MSDU_START_INFO2);
        let msdu3 = self.u32(MSDU_START_INFO3);
        let mpdu9 = self.u32(MPDU_START_INFO9);
        let mpdu11 = self.u32(MPDU_START_INFO11);
        let encryption_info_valid = mpdu11 & (1 << 9) != 0;
        let mpdu_errors = u8::from(attention1 & (1 << 31) != 0)
            | (u8::from(attention1 & (1 << 29) != 0) << 1)
            | (u8::from(attention1 & (1 << 28) != 0) << 2)
            | (u8::from(attention1 & (1 << 12) != 0) << 3)
            | (u8::from(attention1 & (1 << 16) != 0) << 4)
            | (u8::from(attention1 & (1 << 17) != 0) << 5)
            | (u8::from(attention1 & (1 << 27) != 0) << 6);
        RxDescriptorStatus {
            first_msdu: end4 & (1 << 12) != 0,
            last_msdu: end4 & (1 << 13) != 0,
            l3_padding: ((end4 >> 10) & 3) as u8,
            msdu_done: attention2 & (1 << 31) != 0,
            msdu_length_error: attention1 & (1 << 17) != 0,
            fcs_error: attention1 & (1 << 31) != 0,
            decrypt_error: attention1 & (1 << 29) != 0,
            tkip_mic_error: attention1 & (1 << 28) != 0,
            mpdu_errors,
            ip_checksum_failed: attention1 & (1 << 19) != 0,
            l4_checksum_failed: attention1 & (1 << 18) != 0,
            multicast_broadcast: attention1 & (1 << 2) != 0,
            decrypted: ((attention2 >> 10) & 7) == 0,
            msdu_length: (msdu1 & 0x3fff) as u16,
            decap_type: ((msdu2 >> 8) & 3) as u8,
            mesh_control_present: msdu2 & (1 << 22) != 0,
            ldpc: msdu2 & (1 << 23) != 0,
            short_guard_interval: ((msdu3 >> 13) & 3) as u8,
            mcs: ((msdu3 >> 15) & 0xf) as u8,
            bandwidth: ((msdu3 >> 19) & 3) as u8,
            packet_type: ((msdu3 >> 8) & 0xf) as u8,
            spatial_stream_bitmap: (msdu3 >> 24) as u8,
            nss: ((msdu3 >> 24) as u8).count_ones() as u8,
            frequency: self.u32(MSDU_START_PHY_METADATA),
            tid: ((mpdu9 >> 15) & 0xf) as u8,
            peer: PeerId(self.u16(MPDU_START_SW_PEER_ID)),
            sequence_control_valid: mpdu11 & (1 << 6) != 0,
            frame_control_valid: mpdu11 & 1 != 0,
            sequence_number: ((mpdu11 >> 20) & 0xfff) as u16,
            encryption_info_valid,
            // `ath11k_dp_rx_h_mpdu_start_enctype` maps invalid encryption
            // metadata to OPEN rather than consuming the stale type bits.
            encryption_type: if encryption_info_valid {
                ((mpdu9 >> 2) & 0xf) as u8
            } else {
                7
            },
            phy_ppdu_id: self.u16(MPDU_START_PHY_PPDU_ID),
        }
    }

    fn u16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.bytes[offset], self.bytes[offset + 1]])
    }

    fn u32(&self, offset: usize) -> u32 {
        u32::from_le_bytes([
            self.bytes[offset],
            self.bytes[offset + 1],
            self.bytes[offset + 2],
            self.bytes[offset + 3],
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn put16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn qcn9074_layout_fixture_matches_wcn6750_ops() {
        let mut bytes = vec![0; WCN6750_RX_DESCRIPTOR_BYTES + 4];
        put16(
            &mut bytes,
            MSDU_END_INFO4,
            (1 << 12) | (1 << 13) | (2 << 10),
        );
        put32(&mut bytes, ATTENTION_INFO1, (1 << 31) | (1 << 29));
        put32(&mut bytes, ATTENTION_INFO2, 1 << 31);
        put32(&mut bytes, MSDU_START_INFO1, 1500);
        put32(&mut bytes, MSDU_START_INFO2, (2 << 8) | (1 << 23));
        put32(
            &mut bytes,
            MSDU_START_INFO3,
            (4 << 8) | (2 << 13) | (9 << 15) | (1 << 19) | (3 << 24),
        );
        put32(&mut bytes, MSDU_START_PHY_METADATA, 5180);
        put32(&mut bytes, MPDU_START_TAG, 207 << 1);
        put32(&mut bytes, MPDU_START_INFO9, (7 << 15) | (6 << 2));
        put16(&mut bytes, MPDU_START_PHY_PPDU_ID, 0x1234);
        put16(&mut bytes, MPDU_START_SW_PEER_ID, 0x5678);
        put32(
            &mut bytes,
            MPDU_START_INFO11,
            1 | (1 << 3) | (1 << 6) | (1 << 9) | (0xabc << 20),
        );
        bytes[MPDU_START_ADDR2..MPDU_START_ADDR2 + 6].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        bytes[WCN6750_RX_DESCRIPTOR_BYTES..].copy_from_slice(&[9, 8, 7, 6]);

        let desc = Wcn6750RxDescriptor::parse(&bytes).unwrap();
        assert!(desc.mpdu_start_valid());
        assert_eq!(desc.address2(), Some([1, 2, 3, 4, 5, 6]));
        assert_eq!(desc.payload(), [9, 8, 7, 6]);
        let status = desc.status();
        assert!(status.first_msdu && status.last_msdu && status.msdu_done);
        assert_eq!(status.l3_padding, 2);
        assert_eq!(status.msdu_length, 1500);
        assert_eq!(status.peer, PeerId(0x5678));
        assert_eq!(status.sequence_number, 0xabc);
        assert_eq!(status.phy_ppdu_id, 0x1234);
        assert_eq!(
            (status.mcs, status.bandwidth, status.spatial_stream_bitmap),
            (9, 1, 3)
        );
        assert_eq!(status.nss, 2);
    }

    #[test]
    fn every_truncation_is_rejected_without_panic() {
        for length in 0..WCN6750_RX_DESCRIPTOR_BYTES {
            assert_eq!(
                Wcn6750RxDescriptor::parse(&vec![0; length]),
                Err(DpError::MalformedDescriptor)
            );
        }
    }
}
