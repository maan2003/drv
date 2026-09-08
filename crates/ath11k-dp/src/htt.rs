// PORT-MAP: reusable
//! Host-target transport (HTT) wire messages from `dp.h`.

use alloc::vec::Vec;

use crate::{DpError, HttControl, HttHostMessage, HttTargetMessage, PeerId};

const VERSION_REQ: u8 = 0;
const SRING_SETUP: u8 = 0x0b;
const RX_RING_SELECTION_CFG: u8 = 0x0c;
const EXT_STATS_CFG: u8 = 0x10;
const PPDU_STATS_CFG: u8 = 0x11;

pub const TARGET_VERSION_MAJOR: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SrngRingType {
    HardwareToSoftware = 0,
    SoftwareToHardware = 1,
    SoftwareToSoftware = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SrngRingId {
    RxdmaHostBuffer = 0,
    RxdmaMonitorStatus = 1,
    RxdmaMonitorBuffer = 2,
    RxdmaMonitorDescriptor = 3,
    RxdmaMonitorDestination = 4,
    Host1ToFirmwareRxBuffer = 5,
    Host2ToFirmwareRxBuffer = 6,
    RxdmaNonMonitorDestination = 7,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SrngFlags {
    pub msi_swap: bool,
    pub host_firmware_swap: bool,
    pub tlv_swap: bool,
    pub low_threshold_interrupt: bool,
}

/// Inputs to `ath11k_dp_tx_htt_srng_setup`, already expressed in HTT units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SrngSetup {
    pub pdev_id: u8,
    pub ring_id: SrngRingId,
    pub ring_type: SrngRingType,
    pub ring_base_address: u64,
    /// Number of four-byte words in the complete ring.
    pub ring_size_words: u16,
    /// Number of four-byte words in one entry.
    pub ring_entry_size_words: u8,
    pub head_address: u64,
    pub tail_address: u64,
    pub msi_address: u64,
    pub msi_data: u32,
    /// Batch threshold in four-byte words.
    pub interrupt_batch_threshold_words: u16,
    /// Timer threshold in 8 microsecond units.
    pub interrupt_timer_threshold: u16,
    pub interrupt_low_threshold: u16,
    pub flags: SrngFlags,
}

impl SrngSetup {
    pub fn encode(self) -> HttHostMessage {
        let mut words = [0_u32; 13];
        words[0] = u32::from(SRING_SETUP)
            | (u32::from(self.pdev_id) << 8)
            | (u32::from(self.ring_id as u8) << 16)
            | (u32::from(self.ring_type as u8) << 24);
        words[1] = self.ring_base_address as u32;
        words[2] = (self.ring_base_address >> 32) as u32;
        words[3] = u32::from(self.ring_size_words) | (u32::from(self.ring_entry_size_words) << 16);
        if self.ring_type == SrngRingType::SoftwareToHardware {
            words[3] |= 1 << 25;
        }
        words[3] |= bool_bit(self.flags.msi_swap, 27)
            | bool_bit(self.flags.host_firmware_swap, 28)
            | bool_bit(self.flags.tlv_swap, 29);
        words[4] = self.head_address as u32;
        words[5] = (self.head_address >> 32) as u32;
        words[6] = self.tail_address as u32;
        words[7] = (self.tail_address >> 32) as u32;
        words[8] = self.msi_address as u32;
        words[9] = (self.msi_address >> 32) as u32;
        words[10] = self.msi_data;
        words[11] = u32::from(self.interrupt_batch_threshold_words & 0x7fff)
            | (u32::from(self.interrupt_timer_threshold) << 16);
        if self.flags.low_threshold_interrupt {
            words[12] = u32::from(self.interrupt_low_threshold);
        }
        HttHostMessage(words_to_bytes(&words))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RxRingFilter {
    pub tlvs: u32,
    pub management_0: u32,
    pub management_1: u32,
    pub control: u32,
    pub data: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RxRingSelection {
    pub pdev_id: u8,
    pub ring_id: SrngRingId,
    pub status_swap: bool,
    pub packet_swap: bool,
    pub buffer_size: u16,
    pub filter: RxRingFilter,
}

impl RxRingSelection {
    pub fn encode(self) -> HttHostMessage {
        let words = [
            u32::from(RX_RING_SELECTION_CFG)
                | (u32::from(self.pdev_id) << 8)
                | (u32::from(self.ring_id as u8) << 16)
                | bool_bit(self.status_swap, 24)
                | bool_bit(self.packet_swap, 25),
            u32::from(self.buffer_size),
            self.filter.management_0,
            self.filter.management_1,
            self.filter.control,
            self.filter.data,
            self.filter.tlvs,
        ];
        HttHostMessage(words_to_bytes(&words))
    }
}

/// One `htt_ppdu_stats_cfg_cmd` for a selected set of physical devices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PpduStatsConfig {
    pub pdev_mask: u8,
    pub tlv_mask: u16,
}

impl PpduStatsConfig {
    pub fn encode(self) -> HttHostMessage {
        let word = u32::from(PPDU_STATS_CFG)
            | (u32::from(self.pdev_mask & 0x7f) << 9)
            | (u32::from(self.tlv_mask) << 16);
        HttHostMessage(word.to_le_bytes().to_vec())
    }
}

/// `htt_ext_stats_cfg_cmd`, including the opaque response-correlation cookie.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtStatsConfig {
    pub pdev_mask: u8,
    pub stats_type: u8,
    pub parameters: [u32; 4],
    pub cookie: u64,
}

impl ExtStatsConfig {
    pub fn encode(self) -> HttHostMessage {
        let words = [
            u32::from(EXT_STATS_CFG)
                | (u32::from(self.pdev_mask) << 8)
                | (u32::from(self.stats_type) << 16),
            self.parameters[0],
            self.parameters[1],
            self.parameters[2],
            self.parameters[3],
            0,
            self.cookie as u32,
            (self.cookie >> 32) as u32,
        ];
        HttHostMessage(words_to_bytes(&words))
    }
}

pub fn version_request() -> HttHostMessage {
    HttHostMessage(u32::from(VERSION_REQ).to_le_bytes().to_vec())
}

/// `ath11k_dp_tx_htt_h2t_ver_req_msg`: request and validate the target's
/// incompatible-major-version boundary.
pub fn request_target_version<C: HttControl>(
    control: &mut C,
    deadline_ns: u64,
) -> Result<(u8, u8), DpError> {
    control.send(version_request())?;
    loop {
        let message = control.receive(deadline_ns)?.ok_or(DpError::Timeout)?;
        if let HttEvent::VersionConfirm { major, minor } = message.decode()? {
            if major != TARGET_VERSION_MAJOR {
                return Err(DpError::UnsupportedVersion);
            }
            return Ok((major, minor));
        }
    }
}

pub fn send_srng_setup<C: HttControl>(control: &mut C, setup: SrngSetup) -> Result<(), DpError> {
    control.send(setup.encode())
}

pub fn send_rx_ring_selection<C: HttControl>(
    control: &mut C,
    selection: RxRingSelection,
) -> Result<(), DpError> {
    control.send(selection.encode())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerMap {
    pub vdev_id: u8,
    pub peer_id: PeerId,
    pub address: [u8; 6],
    pub ast_hash: u16,
    pub hardware_peer_id: u16,
    pub v2: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttEvent {
    VersionConfirm {
        major: u8,
        minor: u8,
    },
    PeerMap(PeerMap),
    PeerUnmap {
        peer_id: PeerId,
        v2: bool,
    },
    /// A source-recognized event deferred from the client data path.
    Deferred {
        message_type: u8,
    },
    Unknown {
        message_type: u8,
    },
}

impl HttTargetMessage {
    /// Decode the events dispatched by `ath11k_dp_htt_htc_t2h_msg_handler`.
    pub fn decode(&self) -> Result<HttEvent, DpError> {
        let first = word(&self.0, 0)?;
        let message_type = first as u8;
        match message_type {
            0 => Ok(HttEvent::VersionConfirm {
                major: ((first >> 16) & 0xff) as u8,
                minor: ((first >> 8) & 0xff) as u8,
            }),
            0x03 | 0x1e => {
                let mac_low = word(&self.0, 1)?;
                let info1 = word(&self.0, 2)?;
                let wire_info2 = word(&self.0, 3)?;
                let info2 = if message_type == 0x1e { wire_info2 } else { 0 };
                let low = mac_low.to_le_bytes();
                let high = (info1 as u16).to_le_bytes();
                Ok(HttEvent::PeerMap(PeerMap {
                    vdev_id: ((first >> 8) & 0xff) as u8,
                    peer_id: PeerId((first >> 16) as u16),
                    address: [low[0], low[1], low[2], low[3], high[0], high[1]],
                    ast_hash: info2 as u16,
                    hardware_peer_id: (info1 >> 16) as u16,
                    v2: message_type == 0x1e,
                }))
            }
            0x04 | 0x1f => {
                word(&self.0, 1)?;
                word(&self.0, 2)?;
                Ok(HttEvent::PeerUnmap {
                    peer_id: PeerId((first >> 16) as u16),
                    v2: message_type == 0x1f,
                })
            }
            0x08 | 0x1c | 0x1d | 0x24 => Ok(HttEvent::Deferred { message_type }),
            _ => Ok(HttEvent::Unknown { message_type }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxCompletion {
    pub status: u8,
    pub reinject_reason: u8,
    pub ack_rssi: i8,
    pub peer: Option<PeerId>,
}

impl TxCompletion {
    /// Parse the HTT completion overlay at offset 8 in a WBM release entry.
    pub fn decode_wbm_release(bytes: &[u8]) -> Result<Self, DpError> {
        let overlay = bytes.get(8..24).ok_or(DpError::MalformedHtt)?;
        let info0 = word(overlay, 0)?;
        let info1 = word(overlay, 1)?;
        let info2 = word(overlay, 2)?;
        Ok(Self {
            status: ((info0 >> 9) & 0x0f) as u8,
            reinject_reason: ((info0 >> 13) & 0x0f) as u8,
            ack_rssi: (info1 >> 24) as u8 as i8,
            peer: if info2 & (1 << 21) != 0 {
                Some(PeerId(info2 as u16))
            } else {
                None
            },
        })
    }
}

fn bool_bit(value: bool, bit: u32) -> u32 {
    u32::from(value) << bit
}

fn word(bytes: &[u8], index: usize) -> Result<u32, DpError> {
    let start = index.checked_mul(4).ok_or(DpError::MalformedHtt)?;
    let value = bytes.get(start..start + 4).ok_or(DpError::MalformedHtt)?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for value in words {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec;

    struct Control {
        sent: Vec<HttHostMessage>,
        received: VecDeque<HttTargetMessage>,
    }

    impl HttControl for Control {
        fn send(&mut self, message: HttHostMessage) -> Result<(), DpError> {
            self.sent.push(message);
            Ok(())
        }

        fn receive(&mut self, _: u64) -> Result<Option<HttTargetMessage>, DpError> {
            Ok(self.received.pop_front())
        }
    }

    #[test]
    fn version_request_matches_htt_ver_req_cmd() {
        assert_eq!(version_request().0, [0, 0, 0, 0]);
    }

    #[test]
    fn version_handshake_ignores_other_events_and_checks_major() {
        let mut control = Control {
            sent: Vec::new(),
            received: VecDeque::from([
                HttTargetMessage(vec![0x08, 0, 0, 0]),
                HttTargetMessage(vec![0, 9, TARGET_VERSION_MAJOR, 0]),
            ]),
        };
        assert_eq!(request_target_version(&mut control, 3), Ok((3, 9)));
        assert_eq!(control.sent, [version_request()]);

        control
            .received
            .push_back(HttTargetMessage(vec![0, 0, 4, 0]));
        assert_eq!(
            request_target_version(&mut control, 3),
            Err(DpError::UnsupportedVersion)
        );
    }

    #[test]
    fn srng_setup_matches_dp_h_layout() {
        let message = SrngSetup {
            pdev_id: 2,
            ring_id: SrngRingId::RxdmaNonMonitorDestination,
            ring_type: SrngRingType::HardwareToSoftware,
            ring_base_address: 0x1122_3344_5566_7788,
            ring_size_words: 0x1234,
            ring_entry_size_words: 8,
            head_address: 0x0123_4567_89ab_cdef,
            tail_address: 0xfedc_ba98_7654_3210,
            msi_address: 0x8877_6655_4433_2211,
            msi_data: 0xaabb_ccdd,
            interrupt_batch_threshold_words: 0x3456,
            interrupt_timer_threshold: 0x789a,
            interrupt_low_threshold: 0xbcde,
            flags: SrngFlags {
                msi_swap: true,
                host_firmware_swap: true,
                tlv_swap: true,
                low_threshold_interrupt: true,
            },
        }
        .encode();
        assert_eq!(message.0.len(), 52);
        assert_eq!(&message.0[0..4], &0x0007_020b_u32.to_le_bytes());
        assert_eq!(&message.0[12..16], &0x3808_1234_u32.to_le_bytes());
        assert_eq!(&message.0[44..48], &0x789a_3456_u32.to_le_bytes());
        assert_eq!(&message.0[48..52], &0x0000_bcde_u32.to_le_bytes());
    }

    #[test]
    fn software_to_hardware_disables_loop_count() {
        let mut setup = SrngSetup {
            pdev_id: 1,
            ring_id: SrngRingId::RxdmaHostBuffer,
            ring_type: SrngRingType::SoftwareToHardware,
            ring_base_address: 0,
            ring_size_words: 0,
            ring_entry_size_words: 0,
            head_address: 0,
            tail_address: 0,
            msi_address: 0,
            msi_data: 0,
            interrupt_batch_threshold_words: 0,
            interrupt_timer_threshold: 0,
            interrupt_low_threshold: 0,
            flags: SrngFlags::default(),
        };
        assert_eq!(word(&setup.encode().0, 3).unwrap(), 1 << 25);
        setup.ring_type = SrngRingType::SoftwareToSoftware;
        assert_eq!(word(&setup.encode().0, 3).unwrap(), 0);
    }

    #[test]
    fn rx_selection_matches_dp_h_layout() {
        let message = RxRingSelection {
            pdev_id: 1,
            ring_id: SrngRingId::RxdmaHostBuffer,
            status_swap: true,
            packet_swap: false,
            buffer_size: 2048,
            filter: RxRingFilter {
                tlvs: 0x1f,
                management_0: 1,
                management_1: 2,
                control: 3,
                data: 4,
            },
        }
        .encode();
        assert_eq!(message.0.len(), 28);
        assert_eq!(
            message.0,
            vec![
                12, 1, 0, 1, 0, 8, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 31, 0, 0,
                0
            ]
        );
    }

    #[test]
    fn decodes_version_and_peer_events_byte_exactly() {
        let version = HttTargetMessage(vec![0, 7, 3, 0]);
        assert_eq!(
            version.decode(),
            Ok(HttEvent::VersionConfirm { major: 3, minor: 7 })
        );

        let words = [0x1234_5603, 0x4433_2211, 0x7788_6655, 0x0000_abcd];
        let map = HttTargetMessage(words_to_bytes(&words));
        assert_eq!(
            map.decode(),
            Ok(HttEvent::PeerMap(PeerMap {
                vdev_id: 0x56,
                peer_id: PeerId(0x1234),
                address: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
                ast_hash: 0,
                hardware_peer_id: 0x7788,
                v2: false,
            }))
        );

        let mut v2 = words;
        v2[0] = (v2[0] & !0xff) | 0x1e;
        let map = HttTargetMessage(words_to_bytes(&v2));
        assert_eq!(
            map.decode(),
            Ok(HttEvent::PeerMap(PeerMap {
                vdev_id: 0x56,
                peer_id: PeerId(0x1234),
                address: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
                ast_hash: 0xabcd,
                hardware_peer_id: 0x7788,
                v2: true,
            }))
        );
    }

    #[test]
    fn malformed_events_never_panic() {
        for len in 0..16 {
            let bytes = vec![0x1e; len];
            let result = HttTargetMessage(bytes).decode();
            if len < 16 {
                assert_eq!(result, Err(DpError::MalformedHtt));
            }
        }
    }

    #[test]
    fn tx_completion_overlay_is_checked() {
        let mut bytes = vec![0; 24];
        bytes[8..12].copy_from_slice(&((5_u32 << 9) | (7 << 13)).to_le_bytes());
        bytes[12..16].copy_from_slice(&0x9a00_0000_u32.to_le_bytes());
        bytes[16..20].copy_from_slice(&((1 << 21) | 0x1234_u32).to_le_bytes());
        assert_eq!(
            TxCompletion::decode_wbm_release(&bytes),
            Ok(TxCompletion {
                status: 5,
                reinject_reason: 7,
                ack_rssi: -102,
                peer: Some(PeerId(0x1234)),
            })
        );
        assert_eq!(
            TxCompletion::decode_wbm_release(&bytes[..23]),
            Err(DpError::MalformedHtt)
        );
    }
}
