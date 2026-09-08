//! WMI host-to-firmware command encoders.
use crate::tags::*;
use crate::{Command, WmiError};
use alloc::vec::Vec;

mod control;
mod device;
mod peer_assoc;
mod scan;
mod transport;
mod wow;
pub use control::*;
pub use device::*;
pub use peer_assoc::*;
pub use scan::*;
pub use transport::*;
pub use wow::*;

pub trait CommandEncoder {
    type Request;
    fn encode(&self, request: &Self::Request) -> Result<Command, WmiError>;
}
pub trait EncodeCommand {
    fn encode_command(&self) -> Result<Command, WmiError>;
}
impl<T: EncodeCommand> CommandEncoder for T {
    type Request = T;
    fn encode(&self, request: &T) -> Result<Command, WmiError> {
        request.encode_command()
    }
}

#[derive(Default)]
pub(crate) struct TlvWriter(pub(crate) Vec<u8>);
impl TlvWriter {
    pub(crate) fn header(&mut self, tag: TlvTag, len: u16) {
        self.u32((u32::from(tag.0) << 16) | u32::from(len));
    }
    pub(crate) fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes())
    }
    pub(crate) fn mac(&mut self, v: &[u8; 6]) {
        self.0.extend_from_slice(v);
        self.0.extend_from_slice(&[0; 2])
    }
    pub(crate) fn bytes(&mut self, v: &[u8]) {
        self.0.extend_from_slice(v)
    }
    pub(crate) fn zeros(&mut self, n: usize) {
        self.0.resize(self.0.len() + n, 0)
    }
    pub(crate) fn tlv(&mut self, tag: TlvTag, f: impl FnOnce(&mut Self)) -> Result<(), WmiError> {
        let mut b = Self::default();
        f(&mut b);
        let n = u16::try_from(b.0.len()).map_err(|_| WmiError::Malformed)?;
        self.header(tag, n);
        self.bytes(&b.0);
        Ok(())
    }
    pub(crate) fn byte_array(&mut self, b: &[u8]) -> Result<(), WmiError> {
        let n = b.len().div_ceil(4) * 4;
        self.tlv(WMI_TAG_ARRAY_BYTE, |w| {
            w.bytes(b);
            w.zeros(n - b.len())
        })
    }
    pub(crate) fn finish(self, id: crate::CommandId) -> Result<Command, WmiError> {
        Command::from_tlvs(id, self.0)
    }
}
pub(crate) fn one(
    id: crate::CommandId,
    tag: TlvTag,
    f: impl FnOnce(&mut TlvWriter),
) -> Result<Command, WmiError> {
    let mut w = TlvWriter::default();
    w.tlv(tag, f)?;
    w.finish(id)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxRxStreams {
    pub tx: u32,
    pub rx: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VdevCreate {
    pub vdev_id: u32,
    pub vdev_type: u32,
    pub vdev_subtype: u32,
    pub mac_addr: [u8; 6],
    pub pdev_id: u32,
    pub mbssid_flags: u32,
    pub mbssid_tx_vdev_id: u32,
    pub band_2ghz: TxRxStreams,
    pub band_5ghz: TxRxStreams,
}
impl EncodeCommand for VdevCreate {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_VDEV_CREATE_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(self.vdev_type);
            w.u32(self.vdev_subtype);
            w.mac(&self.mac_addr);
            w.u32(2);
            w.u32(self.pdev_id);
            w.u32(self.mbssid_flags);
            w.u32(self.mbssid_tx_vdev_id)
        })?;
        w.tlv(WMI_TAG_ARRAY_STRUCT, |w| {
            for (band, s) in [(0, self.band_2ghz), (1, self.band_5ghz)] {
                let _ = w.tlv(WMI_TAG_VDEV_TXRX_STREAMS, |w| {
                    w.u32(band);
                    w.u32(s.tx);
                    w.u32(s.rx);
                });
            }
        })?;
        w.finish(WMI_VDEV_CREATE_CMDID)
    }
}
macro_rules! vdev_id_cmd {
    ($n:ident,$tag:ident,$id:ident) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct $n {
            pub vdev_id: u32,
        }
        impl EncodeCommand for $n {
            fn encode_command(&self) -> Result<Command, WmiError> {
                one($id, $tag, |w| w.u32(self.vdev_id))
            }
        }
    };
}
vdev_id_cmd!(VdevDelete, WMI_TAG_VDEV_DELETE_CMD, WMI_VDEV_DELETE_CMDID);
vdev_id_cmd!(VdevStop, WMI_TAG_VDEV_STOP_CMD, WMI_VDEV_STOP_CMDID);
vdev_id_cmd!(VdevDown, WMI_TAG_VDEV_DOWN_CMD, WMI_VDEV_DOWN_CMDID);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Channel {
    pub mhz: u32,
    pub band_center_freq1: u32,
    pub band_center_freq2: u32,
    pub info: u32,
    pub reg_info_1: u32,
    pub reg_info_2: u32,
}
impl Channel {
    pub(crate) fn encode(&self, w: &mut TlvWriter) {
        for v in [
            self.mhz,
            self.band_center_freq1,
            self.band_center_freq2,
            self.info,
            self.reg_info_1,
            self.reg_info_2,
        ] {
            w.u32(v)
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VdevStart {
    pub restart: bool,
    pub vdev_id: u32,
    pub beacon_interval: u32,
    pub dtim_period: u32,
    pub hidden_ssid: bool,
    pub pmf_enabled: bool,
    pub hw_crypto_disabled: bool,
    pub ssid: Option<Vec<u8>>,
    pub bcn_tx_rate: u32,
    pub num_noa_descriptors: u32,
    pub preferred_tx_streams: u32,
    pub preferred_rx_streams: u32,
    pub he_ops: u32,
    pub cac_duration_ms: u32,
    pub regdomain: u32,
    pub mbssid_flags: u32,
    pub mbssid_tx_vdev_id: u32,
    pub channel: Channel,
}
impl EncodeCommand for VdevStart {
    fn encode_command(&self) -> Result<Command, WmiError> {
        if self.ssid.as_ref().is_some_and(|s| s.len() > 32) {
            return Err(WmiError::Malformed);
        }
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_VDEV_START_REQUEST_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(0);
            w.u32(self.beacon_interval);
            w.u32(self.dtim_period);
            let mut f = 1 << 3;
            if !self.restart && self.hidden_ssid {
                f |= 1
            }
            if !self.restart && self.pmf_enabled {
                f |= 2
            }
            if self.hw_crypto_disabled {
                f |= 1 << 4
            }
            w.u32(f);
            let s = if self.restart {
                None
            } else {
                self.ssid.as_deref()
            };
            w.u32(s.map_or(0, |x| x.len() as u32));
            if let Some(s) = s {
                w.bytes(s);
                w.zeros(32 - s.len())
            } else {
                w.zeros(32)
            }
            for v in [
                self.bcn_tx_rate,
                0,
                self.num_noa_descriptors,
                0,
                self.preferred_tx_streams,
                self.preferred_rx_streams,
                self.he_ops,
                self.cac_duration_ms,
                self.regdomain,
                0,
                self.mbssid_flags,
                self.mbssid_tx_vdev_id,
            ] {
                w.u32(v)
            }
        })?;
        w.tlv(WMI_TAG_CHANNEL, |w| self.channel.encode(w))?;
        w.tlv(WMI_TAG_ARRAY_STRUCT, |_| {})?;
        w.finish(if self.restart {
            WMI_VDEV_RESTART_REQUEST_CMDID
        } else {
            WMI_VDEV_START_REQUEST_CMDID
        })
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VdevUp {
    pub vdev_id: u32,
    pub assoc_id: u32,
    pub bssid: [u8; 6],
    pub tx_bssid: Option<[u8; 6]>,
    pub nontx_profile_idx: u32,
    pub nontx_profile_cnt: u32,
}
impl EncodeCommand for VdevUp {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_VDEV_UP_CMDID, WMI_TAG_VDEV_UP_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(self.assoc_id);
            w.mac(&self.bssid);
            w.mac(&self.tx_bssid.unwrap_or([0; 6]));
            w.u32(self.nontx_profile_idx);
            w.u32(self.nontx_profile_cnt)
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerCreate {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub peer_type: u32,
}
impl EncodeCommand for PeerCreate {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_PEER_CREATE_CMDID, WMI_TAG_PEER_CREATE_CMD, |w| {
            w.u32(self.vdev_id);
            w.mac(&self.peer_addr);
            w.u32(self.peer_type)
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerDelete {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
}
impl EncodeCommand for PeerDelete {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_PEER_DELETE_CMDID, WMI_TAG_PEER_DELETE_CMD, |w| {
            w.u32(self.vdev_id);
            w.mac(&self.peer_addr)
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdevSetParam {
    pub pdev_id: u32,
    pub param_id: u32,
    pub param_value: u32,
}
impl EncodeCommand for PdevSetParam {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_PDEV_SET_PARAM_CMDID, WMI_TAG_PDEV_SET_PARAM_CMD, |w| {
            w.u32(self.pdev_id);
            w.u32(self.param_id);
            w.u32(self.param_value)
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerSetParam {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub param_id: u32,
    pub param_value: u32,
}
impl EncodeCommand for PeerSetParam {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_PEER_SET_PARAM_CMDID, WMI_TAG_PEER_SET_PARAM_CMD, |w| {
            w.u32(self.vdev_id);
            w.mac(&self.peer_addr);
            w.u32(self.param_id);
            w.u32(self.param_value);
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdevSuspend {
    pub suspend_option: u32,
    pub pdev_id: u32,
}
impl EncodeCommand for PdevSuspend {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_PDEV_SUSPEND_CMDID, WMI_TAG_PDEV_SUSPEND_CMD, |w| {
            w.u32(self.pdev_id);
            w.u32(self.suspend_option);
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdevResume {
    pub pdev_id: u32,
}
impl EncodeCommand for PdevResume {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_PDEV_RESUME_CMDID, WMI_TAG_PDEV_RESUME_CMD, |w| {
            w.u32(self.pdev_id)
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VdevSetParam {
    pub vdev_id: u32,
    pub param_id: u32,
    pub param_value: u32,
}
impl EncodeCommand for VdevSetParam {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_VDEV_SET_PARAM_CMDID, WMI_TAG_VDEV_SET_PARAM_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(self.param_id);
            w.u32(self.param_value)
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KeySeqCounter {
    pub low: u32,
    pub high: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VdevInstallKey {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub key_idx: u32,
    pub key_flags: u32,
    pub key_cipher: u32,
    pub key_rsc_counter: KeySeqCounter,
    pub key_data: Vec<u8>,
    pub key_txmic_len: u32,
    pub key_rxmic_len: u32,
}
impl EncodeCommand for VdevInstallKey {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_VDEV_INSTALL_KEY_CMD, |w| {
            w.u32(self.vdev_id);
            w.mac(&self.peer_addr);
            w.u32(self.key_idx);
            w.u32(self.key_flags);
            w.u32(self.key_cipher);
            w.u32(self.key_rsc_counter.low);
            w.u32(self.key_rsc_counter.high);
            w.zeros(48);
            w.u32(self.key_data.len() as u32);
            w.u32(self.key_txmic_len);
            w.u32(self.key_rxmic_len);
            w.u32(0);
            w.u32(0)
        })?;
        w.byte_array(&self.key_data)?;
        w.finish(WMI_VDEV_INSTALL_KEY_CMDID)
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MgmtSend {
    pub vdev_id: u32,
    pub desc_id: u32,
    pub channel_freq: u32,
    pub paddr: u64,
    pub frame: Vec<u8>,
    pub tx_params_valid: bool,
}
impl EncodeCommand for MgmtSend {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let n = self.frame.len().min(64);
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_MGMT_TX_SEND_CMD, |w| {
            for v in [
                self.vdev_id,
                self.desc_id,
                self.channel_freq,
                self.paddr as u32,
                (self.paddr >> 32) as u32,
                self.frame.len() as u32,
                n as u32,
                u32::from(self.tx_params_valid),
            ] {
                w.u32(v)
            }
        })?;
        // Unlike most byte arrays, wmi.c advertises the unpadded download
        // length while reserving a word-aligned value area.
        w.header(WMI_TAG_ARRAY_BYTE, n as u16);
        w.bytes(&self.frame[..n]);
        w.zeros(n.div_ceil(4) * 4 - n);
        if self.tx_params_valid {
            w.tlv(WMI_TAG_TX_SEND_PARAMS, |w| {
                w.u32(0);
                w.u32(1 << 21)
            })?
        }
        w.finish(WMI_MGMT_TX_SEND_CMDID)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanCancelType {
    PdevAll,
    VdevAll,
    Single,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanStop {
    pub requester: u32,
    pub scan_id: u32,
    pub cancel_type: ScanCancelType,
    pub vdev_id: u32,
    pub pdev_id: u32,
}
impl EncodeCommand for ScanStop {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_STOP_SCAN_CMDID, WMI_TAG_STOP_SCAN_CMD, |w| {
            w.u32(self.requester);
            w.u32(self.scan_id);
            w.u32(match self.cancel_type {
                ScanCancelType::PdevAll => 0x0400_0000,
                ScanCancelType::VdevAll => 0x0100_0000,
                ScanCancelType::Single => 0,
            });
            w.u32(self.vdev_id);
            w.u32(self.pdev_id)
        })
    }
}

mod ap;
mod init;
mod lifecycle;
pub use ap::*;
pub use init::*;
pub use lifecycle::*;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    #[test]
    fn vdev_create_layout() {
        let r = VdevCreate {
            vdev_id: 7,
            vdev_type: 2,
            vdev_subtype: 0,
            mac_addr: [0, 1, 2, 3, 4, 5],
            pdev_id: 1,
            mbssid_flags: 0,
            mbssid_tx_vdev_id: 0,
            band_2ghz: TxRxStreams { tx: 2, rx: 2 },
            band_5ghz: TxRxStreams { tx: 1, rx: 1 },
        };
        let c = r.encode_command().unwrap();
        assert_eq!(c.id, WMI_VDEV_CREATE_CMDID);
        assert_eq!(c.tlvs().len(), 76)
    }
    #[test]
    fn ssid_limit_matches_c() {
        let r = VdevStart {
            restart: false,
            vdev_id: 1,
            beacon_interval: 100,
            dtim_period: 2,
            hidden_ssid: false,
            pmf_enabled: false,
            hw_crypto_disabled: false,
            ssid: Some(vec![0; 33]),
            bcn_tx_rate: 0,
            num_noa_descriptors: 0,
            preferred_tx_streams: 1,
            preferred_rx_streams: 1,
            he_ops: 0,
            cac_duration_ms: 0,
            regdomain: 0,
            mbssid_flags: 0,
            mbssid_tx_vdev_id: 0,
            channel: Channel::default(),
        };
        assert_eq!(r.encode_command(), Err(WmiError::Malformed))
    }
    #[test]
    fn scan_stop_uses_sparse_wire_cancel_values() {
        for (cancel_type, expected) in [
            (ScanCancelType::Single, 0),
            (ScanCancelType::VdevAll, 0x0100_0000),
            (ScanCancelType::PdevAll, 0x0400_0000),
        ] {
            let command = ScanStop {
                requester: 1,
                scan_id: 2,
                cancel_type,
                vdev_id: 3,
                pdev_id: 4,
            }
            .encode_command()
            .unwrap();
            assert_eq!(
                u32::from_le_bytes(command.tlvs()[12..16].try_into().unwrap()),
                expected
            );
        }
    }

    #[test]
    fn management_frame_header_uses_unpadded_length() {
        let request = MgmtSend {
            vdev_id: 1,
            desc_id: 2,
            channel_freq: 0,
            paddr: 0,
            frame: vec![1, 2, 3, 4, 5],
            tx_params_valid: false,
        };
        let command = request.encode_command().unwrap();
        assert_eq!(
            u32::from_le_bytes(command.tlvs()[36..40].try_into().unwrap()),
            (u32::from(WMI_TAG_ARRAY_BYTE.0) << 16) | 5
        );
        assert_eq!(&command.tlvs()[40..48], &[1, 2, 3, 4, 5, 0, 0, 0]);
    }

    #[test]
    fn key_padding() {
        let r = VdevInstallKey {
            vdev_id: 1,
            peer_addr: [0; 6],
            key_idx: 0,
            key_flags: 0,
            key_cipher: 0,
            key_rsc_counter: KeySeqCounter::default(),
            key_data: vec![0xaa; 17],
            key_txmic_len: 0,
            key_rxmic_len: 0,
        };
        let c = r.encode_command().unwrap();
        assert_eq!(c.tlvs().len(), 128);
        assert_eq!(&c.tlvs()[125..], &[0; 3])
    }
}
