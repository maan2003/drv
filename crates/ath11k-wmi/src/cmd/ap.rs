//! Deferred AP-side WMI command builders.
//!
//! Wire layouts follow the named helpers in the pinned Linux `ath11k/wmi.c`.

use alloc::vec::Vec;

use crate::tags::*;
use crate::{Command, WmiError};

use super::{EncodeCommand, TlvWriter};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BeaconOffloadControl {
    pub vdev_id: u32,
    pub operation: u32,
}
impl EncodeCommand for BeaconOffloadControl {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_BCN_OFFLOAD_CTRL_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(self.operation);
        })?;
        w.finish(WMI_BCN_OFFLOAD_CTRL_CMDID)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct P2pGoBeaconIe {
    pub vdev_id: u32,
    pub information_element: Vec<u8>,
}
impl EncodeCommand for P2pGoBeaconIe {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let declared = self
            .information_element
            .get(1)
            .map(|length| usize::from(*length) + 2)
            .ok_or(WmiError::Malformed)?;
        let ie = self
            .information_element
            .get(..declared)
            .ok_or(WmiError::Malformed)?;
        let aligned = declared.div_ceil(4) * 4;
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_P2P_GO_SET_BEACON_IE, |w| {
            w.u32(self.vdev_id);
            w.u32(declared as u32);
        })?;
        let len = u16::try_from(aligned).map_err(|_| WmiError::Malformed)?;
        w.header(WMI_TAG_ARRAY_BYTE, len);
        w.bytes(ie);
        w.zeros(aligned - declared);
        w.finish(WMI_P2P_GO_SET_BEACON_IE)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BeaconOffsets {
    pub tim: u32,
    pub csa_switch_count: u32,
    pub extended_csa_switch_count: u32,
    pub mbssid: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BeaconTemplate {
    pub vdev_id: u32,
    pub offsets: BeaconOffsets,
    pub csa_active: bool,
    pub ema_params: u32,
    pub frame: Vec<u8>,
}
impl EncodeCommand for BeaconTemplate {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_BCN_TMPL_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(self.offsets.tim);
            w.u32(self.frame.len() as u32);
            w.u32(if self.csa_active {
                self.offsets.csa_switch_count
            } else {
                0
            });
            w.u32(if self.csa_active {
                self.offsets.extended_csa_switch_count
            } else {
                0
            });
            w.u32(0); // csa_event_bitmap
            w.u32(self.offsets.mbssid);
            w.u32(0); // esp_ie_offset
            w.u32(0); // csc_switch_count_offset
            w.u32(0); // csc_event_bitmap
            w.u32(0); // mu_edca_ie_offset
            w.u32(0); // feature_enable_bitmap
            w.u32(self.ema_params);
        })?;
        beacon_probe_info(&mut w)?;
        w.byte_array(&self.frame)?;
        w.finish(WMI_BCN_TMPL_CMDID)
    }
}

fn beacon_probe_info(w: &mut TlvWriter) -> Result<(), WmiError> {
    w.tlv(WMI_TAG_BCN_PRB_INFO, |w| {
        w.u32(0);
        w.u32(0);
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WmmAccessCategory {
    pub cw_min: u32,
    pub cw_max: u32,
    pub aifs: u32,
    pub txop_limit: u32,
    pub admission_control_mandatory: u32,
    pub no_ack: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WmmUpdate {
    pub vdev_id: u32,
    pub parameter_type: u32,
    /// Firmware order: best-effort, background, video, voice.
    pub access_categories: [WmmAccessCategory; 4],
}
impl EncodeCommand for WmmUpdate {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_VDEV_SET_WMM_PARAMS_CMD, |w| {
            w.u32(self.vdev_id);
            for ac in self.access_categories {
                // These four source structs are inline nested TLVs.
                w.header(WMI_TAG_VDEV_SET_WMM_PARAMS_CMD, 24);
                w.u32(ac.cw_min);
                w.u32(ac.cw_max);
                w.u32(ac.aifs);
                w.u32(ac.txop_limit);
                w.u32(ac.admission_control_mandatory);
                w.u32(ac.no_ack);
            }
            w.u32(self.parameter_type);
        })?;
        w.finish(WMI_VDEV_SET_WMM_PARAMS_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DelbaSend {
    pub vdev_id: u32,
    pub peer_mac: [u8; 6],
    pub tid: u32,
    pub initiator: u32,
    pub reason_code: u32,
}
impl EncodeCommand for DelbaSend {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_DELBA_SEND_CMD, |w| {
            w.u32(self.vdev_id);
            w.mac(&self.peer_mac);
            w.u32(self.tid);
            w.u32(self.initiator);
            w.u32(self.reason_code);
        })?;
        w.finish(WMI_DELBA_SEND_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AddbaSetResponse {
    pub vdev_id: u32,
    pub peer_mac: [u8; 6],
    pub tid: u32,
    pub status_code: u32,
}
impl EncodeCommand for AddbaSetResponse {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_ADDBA_SETRESPONSE_CMD, |w| {
            w.u32(self.vdev_id);
            w.mac(&self.peer_mac);
            w.u32(self.tid);
            w.u32(self.status_code);
        })?;
        w.finish(WMI_ADDBA_SET_RESP_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AddbaSend {
    pub vdev_id: u32,
    pub peer_mac: [u8; 6],
    pub tid: u32,
    pub buffer_size: u32,
}
impl EncodeCommand for AddbaSend {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_ADDBA_SEND_CMD, |w| {
            w.u32(self.vdev_id);
            w.mac(&self.peer_mac);
            w.u32(self.tid);
            w.u32(self.buffer_size);
        })?;
        w.finish(WMI_ADDBA_SEND_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AddbaClearResponse {
    pub vdev_id: u32,
    pub peer_mac: [u8; 6],
}
impl EncodeCommand for AddbaClearResponse {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_ADDBA_CLEAR_RESP_CMD, |w| {
            w.u32(self.vdev_id);
            w.mac(&self.peer_mac);
        })?;
        w.finish(WMI_ADDBA_CLEAR_RESP_CMDID)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilsDiscoveryTemplate {
    pub vdev_id: u32,
    pub frame: Vec<u8>,
}
impl EncodeCommand for FilsDiscoveryTemplate {
    fn encode_command(&self) -> Result<Command, WmiError> {
        template_with_frame(
            WMI_FILS_DISCOVERY_TMPL_CMDID,
            WMI_TAG_FILS_DISCOVERY_TMPL_CMD,
            self.vdev_id,
            &self.frame,
            false,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeResponseTemplate {
    pub vdev_id: u32,
    pub frame: Vec<u8>,
}
impl EncodeCommand for ProbeResponseTemplate {
    fn encode_command(&self) -> Result<Command, WmiError> {
        template_with_frame(
            WMI_PRB_TMPL_CMDID,
            WMI_TAG_PRB_TMPL_CMD,
            self.vdev_id,
            &self.frame,
            true,
        )
    }
}

fn template_with_frame(
    id: crate::CommandId,
    tag: TlvTag,
    vdev_id: u32,
    frame: &[u8],
    probe_info: bool,
) -> Result<Command, WmiError> {
    let mut w = TlvWriter::default();
    w.tlv(tag, |w| {
        w.u32(vdev_id);
        w.u32(frame.len() as u32);
    })?;
    if probe_info {
        beacon_probe_info(&mut w)?;
    }
    w.byte_array(frame)?;
    w.finish(id)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilsDiscovery {
    pub vdev_id: u32,
    pub interval: u32,
    pub unsolicited_broadcast_probe_response: bool,
}
impl EncodeCommand for FilsDiscovery {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_ENABLE_FILS_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(self.interval);
            w.u32(u32::from(self.unsolicited_broadcast_probe_response));
        })?;
        w.finish(WMI_ENABLE_FILS_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerCfrCapture {
    pub vdev_id: u32,
    pub peer_mac: [u8; 6],
    pub request: u32,
    pub periodicity: u32,
    pub bandwidth: u32,
    pub capture_method: u32,
}
impl EncodeCommand for PeerCfrCapture {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_PEER_CFR_CAPTURE_CMD, |w| {
            w.u32(self.request);
            w.mac(&self.peer_mac);
            w.u32(self.vdev_id);
            w.u32(self.periodicity);
            w.u32(self.bandwidth);
            w.u32(self.capture_method);
        })?;
        w.finish(WMI_PEER_CFR_CAPTURE_CMDID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn beacon_template_matches_source_layout() {
        let command = BeaconTemplate {
            vdev_id: 7,
            offsets: BeaconOffsets {
                tim: 9,
                csa_switch_count: 11,
                extended_csa_switch_count: 13,
                mbssid: 15,
            },
            csa_active: true,
            ema_params: 17,
            frame: alloc::vec![1, 2, 3],
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.id, WMI_BCN_TMPL_CMDID);
        assert_eq!(
            word(command.tlvs(), 0),
            (u32::from(WMI_TAG_BCN_TMPL_CMD.0) << 16) | 52
        );
        assert_eq!(word(command.tlvs(), 4), 7);
        assert_eq!(word(command.tlvs(), 8), 9);
        assert_eq!(word(command.tlvs(), 52), 17);
        assert_eq!(
            word(command.tlvs(), 56),
            (u32::from(WMI_TAG_BCN_PRB_INFO.0) << 16) | 8
        );
        assert_eq!(&command.tlvs()[72..75], &[1, 2, 3]);
    }

    #[test]
    fn wmm_contains_four_inline_parameter_tlvs() {
        let command = WmmUpdate {
            vdev_id: 1,
            parameter_type: 2,
            access_categories: [WmmAccessCategory::default(); 4],
        }
        .encode_command()
        .unwrap();
        assert_eq!(word(command.tlvs(), 0) & 0xffff, 120);
        for offset in [8usize, 36, 64, 92] {
            assert_eq!(word(command.tlvs(), offset) & 0xffff, 24);
        }
    }

    #[test]
    fn p2p_ie_uses_declared_ie_length_and_aligned_array() {
        let command = P2pGoBeaconIe {
            vdev_id: 3,
            information_element: alloc::vec![221, 3, 1, 2, 3, 99],
        }
        .encode_command()
        .unwrap();
        assert_eq!(word(command.tlvs(), 8), 5);
        assert_eq!(word(command.tlvs(), 12) & 0xffff, 8);
        assert_eq!(&command.tlvs()[16..21], &[221, 3, 1, 2, 3]);
        assert_eq!(&command.tlvs()[21..24], &[0; 3]);
    }

    #[test]
    fn short_p2p_ie_fails_without_panic() {
        assert_eq!(
            P2pGoBeaconIe {
                vdev_id: 0,
                information_element: alloc::vec![221]
            }
            .encode_command(),
            Err(WmiError::Malformed)
        );
    }
}
