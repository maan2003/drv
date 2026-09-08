//! WMI host-to-firmware command encoders.
use crate::tags::*;
use crate::trace::{RejectReason, TraceEvent, TraceSink};
use crate::{Command, WmiError};
use alloc::vec::Vec;

pub mod comparison;
mod control;
mod device;
pub mod golden;
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
#[cfg(feature = "proptest")]
pub trait CommandStrategy: Sized + 'static {
    fn strategy() -> proptest::strategy::BoxedStrategy<Self>;
}

pub trait EncodeCommand {
    fn encode_command(&self) -> Result<Command, WmiError>;

    fn trace_fields(&self, _sink: &mut dyn TraceSink) {}

    fn encode_command_with_trace(&self, sink: &mut dyn TraceSink) -> Result<Command, WmiError> {
        match self.encode_command() {
            Ok(command) => {
                trace_tlvs(command.tlvs(), 0, sink);
                self.trace_fields(sink);
                Ok(command)
            }
            Err(error) => {
                sink.record(TraceEvent::Reject {
                    reason: match error {
                        WmiError::UnalignedTlv => RejectReason::Unaligned,
                        WmiError::Malformed => RejectReason::InvalidArgument,
                        WmiError::Timeout | WmiError::Transport => RejectReason::Protocol,
                    },
                    offset: 0,
                });
                Err(error)
            }
        }
    }
}
impl<T: EncodeCommand> CommandEncoder for T {
    type Request = T;
    fn encode(&self, request: &T) -> Result<Command, WmiError> {
        request.encode_command()
    }
}

fn trace_tlvs(bytes: &[u8], base: usize, sink: &mut dyn TraceSink) {
    let mut offset = 0;
    while let Some(header) = bytes.get(offset..offset + 4) {
        let header = u32::from_le_bytes(header.try_into().expect("four-byte slice"));
        let tag = (header >> 16) as u16;
        let len = (header & 0xffff) as usize;
        sink.record(TraceEvent::Tlv {
            tag,
            len,
            offset: base + offset,
        });
        // Pinned ath11k wmi.c:ath11k_wmi_init_cmd_send() sets this length to
        // sizeof(struct wlan_host_mem_chunk), including the four-byte header.
        let value_len = if tag == WMI_TAG_WLAN_HOST_MEMORY_CHUNK.0 && len == 16 {
            len - 4
        } else {
            len
        };
        let padded = value_len.div_ceil(4) * 4;
        let Some(value) = bytes.get(offset + 4..offset + 4 + value_len) else {
            sink.record(TraceEvent::Reject {
                reason: RejectReason::Truncated,
                offset: base + offset + 4,
            });
            return;
        };
        if tag == WMI_TAG_ARRAY_STRUCT.0 {
            trace_tlvs(value, base + offset + 4, sink);
        }
        offset += 4 + padded;
    }
}

pub(crate) fn trace_field(sink: &mut dyn TraceSink, name: &'static str, value: impl Into<u64>) {
    sink.record(TraceEvent::Field {
        name,
        value: value.into(),
    });
}
pub(crate) fn trace_branch(sink: &mut dyn TraceSink, name: &'static str, taken: bool) {
    sink.record(TraceEvent::Branch { name, taken });
}
fn mac_value(mac: &[u8; 6]) -> u64 {
    mac.iter()
        .enumerate()
        .fold(0, |v, (i, byte)| v | (u64::from(*byte) << (i * 8)))
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "VdevCreate.vdev_id", self.vdev_id);
        trace_field(sink, "VdevCreate.vdev_type", self.vdev_type);
        trace_field(sink, "VdevCreate.vdev_subtype", self.vdev_subtype);
        trace_field(sink, "VdevCreate.pdev_id", self.pdev_id);
        trace_field(sink, "VdevCreate.mbssid_flags", self.mbssid_flags);
        trace_field(sink, "VdevCreate.mbssid_tx_vdev_id", self.mbssid_tx_vdev_id);
        trace_field(sink, "VdevCreate.mac_addr", mac_value(&self.mac_addr));
        trace_field(sink, "VdevCreate.band_2ghz.tx", self.band_2ghz.tx);
        trace_field(sink, "VdevCreate.band_2ghz.rx", self.band_2ghz.rx);
        trace_field(sink, "VdevCreate.band_5ghz.tx", self.band_5ghz.tx);
        trace_field(sink, "VdevCreate.band_5ghz.rx", self.band_5ghz.rx);
    }
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
            fn trace_fields(&self, sink: &mut dyn TraceSink) {
                trace_field(sink, concat!(stringify!($n), ".vdev_id"), self.vdev_id);
            }
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "VdevStart.vdev_id", self.vdev_id);
        trace_field(sink, "VdevStart.beacon_interval", self.beacon_interval);
        trace_field(sink, "VdevStart.dtim_period", self.dtim_period);
        trace_field(sink, "VdevStart.bcn_tx_rate", self.bcn_tx_rate);
        trace_field(
            sink,
            "VdevStart.num_noa_descriptors",
            self.num_noa_descriptors,
        );
        trace_field(
            sink,
            "VdevStart.preferred_tx_streams",
            self.preferred_tx_streams,
        );
        trace_field(
            sink,
            "VdevStart.preferred_rx_streams",
            self.preferred_rx_streams,
        );
        trace_field(sink, "VdevStart.he_ops", self.he_ops);
        trace_field(sink, "VdevStart.cac_duration_ms", self.cac_duration_ms);
        trace_field(sink, "VdevStart.regdomain", self.regdomain);
        trace_field(sink, "VdevStart.mbssid_flags", self.mbssid_flags);
        trace_field(sink, "VdevStart.mbssid_tx_vdev_id", self.mbssid_tx_vdev_id);
        trace_branch(sink, "VdevStart.restart", self.restart);
        trace_branch(sink, "VdevStart.hidden_ssid", self.hidden_ssid);
        trace_branch(sink, "VdevStart.pmf_enabled", self.pmf_enabled);
        trace_branch(sink, "VdevStart.ssid.is_some", self.ssid.is_some());
        trace_field(
            sink,
            "VdevStart.ssid.len",
            self.ssid.as_ref().map_or(0, |v| v.len()) as u64,
        );
        trace_field(sink, "VdevStart.channel.mhz", self.channel.mhz);
        trace_field(
            sink,
            "VdevStart.channel.band_center_freq1",
            self.channel.band_center_freq1,
        );
        trace_field(
            sink,
            "VdevStart.channel.band_center_freq2",
            self.channel.band_center_freq2,
        );
        trace_field(sink, "VdevStart.channel.info", self.channel.info);
        trace_field(
            sink,
            "VdevStart.channel.reg_info_1",
            self.channel.reg_info_1,
        );
        trace_field(
            sink,
            "VdevStart.channel.reg_info_2",
            self.channel.reg_info_2,
        );
        trace_branch(
            sink,
            "VdevStart.hw_crypto_disabled",
            self.hw_crypto_disabled,
        );
    }
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "VdevUp.vdev_id", self.vdev_id);
        trace_field(sink, "VdevUp.assoc_id", self.assoc_id);
        trace_field(sink, "VdevUp.bssid", mac_value(&self.bssid));
        trace_branch(sink, "VdevUp.tx_bssid.is_some", self.tx_bssid.is_some());
        if let Some(mac) = &self.tx_bssid {
            trace_field(sink, "VdevUp.tx_bssid", mac_value(mac));
        }
        trace_field(sink, "VdevUp.nontx_profile_idx", self.nontx_profile_idx);
        trace_field(sink, "VdevUp.nontx_profile_cnt", self.nontx_profile_cnt);
    }
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "PeerCreate.vdev_id", self.vdev_id);
        trace_field(sink, "PeerCreate.peer_type", self.peer_type);
        trace_field(sink, "PeerCreate.peer_addr", mac_value(&self.peer_addr));
    }
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "PeerDelete.vdev_id", self.vdev_id);
        trace_field(sink, "PeerDelete.peer_addr", mac_value(&self.peer_addr));
    }
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "PdevSetParam.pdev_id", self.pdev_id);
        trace_field(sink, "PdevSetParam.param_id", self.param_id);
        trace_field(sink, "PdevSetParam.param_value", self.param_value);
    }
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

/// Typed `WMI_PEER_AUTHORIZE` operation used when completing association.
const WMI_PEER_AUTHORIZE_PARAM: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerAuthorize {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub authorized: bool,
}

impl EncodeCommand for PeerAuthorize {
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "PeerAuthorize.vdev_id", self.vdev_id);
        trace_field(sink, "PeerAuthorize.peer_addr", mac_value(&self.peer_addr));
        trace_branch(sink, "PeerAuthorize.authorized", self.authorized);
    }

    fn encode_command(&self) -> Result<Command, WmiError> {
        PeerSetParam {
            vdev_id: self.vdev_id,
            peer_addr: self.peer_addr,
            param_id: WMI_PEER_AUTHORIZE_PARAM,
            param_value: u32::from(self.authorized),
        }
        .encode_command()
    }
}

impl EncodeCommand for PeerSetParam {
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "PeerSetParam.vdev_id", self.vdev_id);
        trace_field(sink, "PeerSetParam.param_id", self.param_id);
        trace_field(sink, "PeerSetParam.param_value", self.param_value);
        trace_field(sink, "PeerSetParam.peer_addr", mac_value(&self.peer_addr));
    }
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "PdevSuspend.pdev_id", self.pdev_id);
        trace_field(sink, "PdevSuspend.suspend_option", self.suspend_option);
    }
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "PdevResume.pdev_id", self.pdev_id);
    }
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "VdevSetParam.vdev_id", self.vdev_id);
        trace_field(sink, "VdevSetParam.param_id", self.param_id);
        trace_field(sink, "VdevSetParam.param_value", self.param_value);
    }
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "VdevInstallKey.vdev_id", self.vdev_id);
        trace_field(sink, "VdevInstallKey.key_idx", self.key_idx);
        trace_field(sink, "VdevInstallKey.key_flags", self.key_flags);
        trace_field(sink, "VdevInstallKey.key_cipher", self.key_cipher);
        trace_field(sink, "VdevInstallKey.key_txmic_len", self.key_txmic_len);
        trace_field(sink, "VdevInstallKey.key_rxmic_len", self.key_rxmic_len);
        trace_field(sink, "VdevInstallKey.peer_addr", mac_value(&self.peer_addr));
        trace_field(
            sink,
            "VdevInstallKey.key_data.len",
            self.key_data.len() as u64,
        );
    }
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "MgmtSend.vdev_id", self.vdev_id);
        trace_field(sink, "MgmtSend.desc_id", self.desc_id);
        trace_field(sink, "MgmtSend.channel_freq", self.channel_freq);
        trace_field(sink, "MgmtSend.paddr", self.paddr);
        trace_field(sink, "MgmtSend.frame.len", self.frame.len() as u64);
        trace_branch(sink, "MgmtSend.tx_params_valid", self.tx_params_valid);
    }
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "ScanStop.requester", self.requester);
        trace_field(sink, "ScanStop.scan_id", self.scan_id);
        trace_field(sink, "ScanStop.vdev_id", self.vdev_id);
        trace_field(sink, "ScanStop.pdev_id", self.pdev_id);
        trace_branch(
            sink,
            "ScanStop.cancel.pdev_all",
            matches!(self.cancel_type, ScanCancelType::PdevAll),
        );
        trace_branch(
            sink,
            "ScanStop.cancel.vdev_all",
            matches!(self.cancel_type, ScanCancelType::VdevAll),
        );
        trace_branch(
            sink,
            "ScanStop.cancel.single",
            matches!(self.cancel_type, ScanCancelType::Single),
        );
    }
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
    fn traced_vdev_create_reports_nested_tlvs_and_fields() {
        struct Sink(Vec<TraceEvent>);
        impl TraceSink for Sink {
            fn record(&mut self, event: TraceEvent) {
                self.0.push(event);
            }
        }
        let request = VdevCreate {
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
        let mut sink = Sink(Vec::new());
        request.encode_command_with_trace(&mut sink).unwrap();
        assert!(sink.0.contains(&TraceEvent::Tlv {
            tag: WMI_TAG_VDEV_CREATE_CMD.0,
            len: 36,
            offset: 0
        }));
        assert!(sink.0.contains(&TraceEvent::Tlv {
            tag: WMI_TAG_VDEV_TXRX_STREAMS.0,
            len: 12,
            offset: 44
        }));
        assert!(sink.0.contains(&TraceEvent::Field {
            name: "VdevCreate.vdev_id",
            value: 7
        }));
    }

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

    #[test]
    fn connect_authorize_and_vdev_down_use_typed_wire_values() {
        let authorize = PeerAuthorize {
            vdev_id: 7,
            peer_addr: [0, 1, 2, 3, 4, 5],
            authorized: true,
        }
        .encode_command()
        .unwrap();
        assert_eq!(authorize.id, WMI_PEER_SET_PARAM_CMDID);
        assert_eq!(
            &authorize.tlvs()[4..],
            &[7, 0, 0, 0, 0, 1, 2, 3, 4, 5, 0, 0, 3, 0, 0, 0, 1, 0, 0, 0]
        );

        let down = VdevDown { vdev_id: 7 }.encode_command().unwrap();
        assert_eq!(down.id, WMI_VDEV_DOWN_CMDID);
        assert_eq!(down.tlvs(), &[4, 0, 0x5e, 0, 7, 0, 0, 0]);
    }
}

#[cfg(feature = "proptest")]
mod strategies {
    use super::*;
    use proptest::{collection::vec, prelude::*, strategy::BoxedStrategy};

    macro_rules! u32_request {
        ($ty:ty, |$v:ident| $body:expr) => {
            impl CommandStrategy for $ty {
                fn strategy() -> BoxedStrategy<Self> {
                    any::<u32>().prop_map(|$v| $body).boxed()
                }
            }
        };
    }
    u32_request!(VdevDelete, |v| VdevDelete { vdev_id: v & 0xff });
    u32_request!(VdevStop, |v| VdevStop { vdev_id: v & 0xff });
    u32_request!(VdevDown, |v| VdevDown { vdev_id: v & 0xff });
    u32_request!(PdevResume, |v| PdevResume { pdev_id: v });

    impl CommandStrategy for PdevSetParam {
        fn strategy() -> BoxedStrategy<Self> {
            any::<(u32, u32, u32)>()
                .prop_map(|(pdev_id, param_id, param_value)| Self {
                    pdev_id,
                    param_id,
                    param_value,
                })
                .boxed()
        }
    }
    impl CommandStrategy for VdevSetParam {
        fn strategy() -> BoxedStrategy<Self> {
            any::<(u32, u32, u32)>()
                .prop_map(|(vdev_id, param_id, param_value)| Self {
                    vdev_id,
                    param_id,
                    param_value,
                })
                .boxed()
        }
    }
    impl CommandStrategy for PeerSetParam {
        fn strategy() -> BoxedStrategy<Self> {
            any::<(u32, [u8; 6], u32, u32)>()
                .prop_map(|(vdev_id, peer_addr, param_id, param_value)| Self {
                    vdev_id,
                    peer_addr,
                    param_id,
                    param_value,
                })
                .boxed()
        }
    }
    impl CommandStrategy for PdevSuspend {
        fn strategy() -> BoxedStrategy<Self> {
            any::<(u32, u32)>()
                .prop_map(|(pdev_id, suspend_option)| Self {
                    pdev_id,
                    suspend_option,
                })
                .boxed()
        }
    }
    impl CommandStrategy for PeerCreate {
        fn strategy() -> BoxedStrategy<Self> {
            any::<(u8, [u8; 6], u32)>()
                .prop_map(|(vdev_id, peer_addr, peer_type)| Self {
                    vdev_id: u32::from(vdev_id),
                    peer_addr,
                    peer_type,
                })
                .boxed()
        }
    }
    impl CommandStrategy for PeerDelete {
        fn strategy() -> BoxedStrategy<Self> {
            any::<(u8, [u8; 6])>()
                .prop_map(|(vdev_id, peer_addr)| Self {
                    vdev_id: u32::from(vdev_id),
                    peer_addr,
                })
                .boxed()
        }
    }
    impl CommandStrategy for VdevCreate {
        fn strategy() -> BoxedStrategy<Self> {
            (
                any::<(u8, u32, u32, [u8; 6], u32, u32, u32)>(),
                any::<(u32, u32, u32, u32)>(),
            )
                .prop_map(
                    |(
                        (
                            vdev_id,
                            vdev_type,
                            vdev_subtype,
                            mac_addr,
                            pdev_id,
                            mbssid_flags,
                            mbssid_tx_vdev_id,
                        ),
                        (tx2, rx2, tx5, rx5),
                    )| Self {
                        vdev_id: u32::from(vdev_id),
                        vdev_type,
                        vdev_subtype,
                        mac_addr,
                        pdev_id,
                        mbssid_flags,
                        mbssid_tx_vdev_id,
                        band_2ghz: TxRxStreams { tx: tx2, rx: rx2 },
                        band_5ghz: TxRxStreams { tx: tx5, rx: rx5 },
                    },
                )
                .boxed()
        }
    }
    impl CommandStrategy for VdevUp {
        fn strategy() -> BoxedStrategy<Self> {
            (
                any::<(u32, u32, [u8; 6], u32, u32)>(),
                prop::option::of(any::<[u8; 6]>()),
            )
                .prop_map(
                    |(
                        (vdev_id, assoc_id, bssid, nontx_profile_idx, nontx_profile_cnt),
                        tx_bssid,
                    )| Self {
                        vdev_id,
                        assoc_id,
                        bssid,
                        tx_bssid,
                        nontx_profile_idx,
                        nontx_profile_cnt,
                    },
                )
                .boxed()
        }
    }
    impl CommandStrategy for PeerAuthorize {
        fn strategy() -> BoxedStrategy<Self> {
            any::<(u32, [u8; 6], bool)>()
                .prop_map(|(vdev_id, peer_addr, authorized)| Self {
                    vdev_id,
                    peer_addr,
                    authorized,
                })
                .boxed()
        }
    }
    impl CommandStrategy for VdevStart {
        fn strategy() -> BoxedStrategy<Self> {
            (
                (
                    any::<(bool, u32, u32, u32, bool, bool, bool)>(),
                    any::<(u32, u32, u32, u32, u32)>(),
                ),
                any::<(u32, u32, u32, u32, u32, u32)>(),
                prop::option::of(vec(any::<u8>(), 0..=32)),
                any::<(u32, u32, u32, u32)>(),
            )
                .prop_map(
                    |(
                        (
                            (
                                restart,
                                vdev_id,
                                beacon_interval,
                                dtim_period,
                                hidden_ssid,
                                pmf_enabled,
                                hw_crypto_disabled,
                            ),
                            (
                                bcn_tx_rate,
                                num_noa_descriptors,
                                preferred_tx_streams,
                                preferred_rx_streams,
                                he_ops,
                            ),
                        ),
                        (
                            cac_duration_ms,
                            regdomain,
                            mbssid_flags,
                            mbssid_tx_vdev_id,
                            mhz,
                            band_center_freq1,
                        ),
                        ssid,
                        (band_center_freq2, info, reg_info_1, reg_info_2),
                    )| Self {
                        restart,
                        vdev_id,
                        beacon_interval,
                        dtim_period,
                        hidden_ssid,
                        pmf_enabled,
                        hw_crypto_disabled,
                        ssid,
                        bcn_tx_rate,
                        num_noa_descriptors,
                        preferred_tx_streams,
                        preferred_rx_streams,
                        he_ops,
                        cac_duration_ms,
                        regdomain,
                        mbssid_flags,
                        mbssid_tx_vdev_id,
                        channel: Channel {
                            mhz,
                            band_center_freq1,
                            band_center_freq2,
                            info,
                            reg_info_1,
                            reg_info_2,
                        },
                    },
                )
                .boxed()
        }
    }
    impl CommandStrategy for VdevInstallKey {
        fn strategy() -> BoxedStrategy<Self> {
            (
                any::<(u32, [u8; 6], u32, u32, u32, u32, u32, u32, u32)>(),
                vec(any::<u8>(), 0..=64),
            )
                .prop_map(
                    |(
                        (
                            vdev_id,
                            peer_addr,
                            key_idx,
                            key_flags,
                            key_cipher,
                            low,
                            high,
                            key_txmic_len,
                            key_rxmic_len,
                        ),
                        key_data,
                    )| Self {
                        vdev_id,
                        peer_addr,
                        key_idx,
                        key_flags,
                        key_cipher,
                        key_rsc_counter: KeySeqCounter { low, high },
                        key_data,
                        key_txmic_len,
                        key_rxmic_len,
                    },
                )
                .boxed()
        }
    }
    impl CommandStrategy for MgmtSend {
        fn strategy() -> BoxedStrategy<Self> {
            (
                any::<(u32, u32, u32, u64, bool)>(),
                vec(any::<u8>(), 0..=256),
            )
                .prop_map(
                    |((vdev_id, desc_id, channel_freq, paddr, tx_params_valid), frame)| Self {
                        vdev_id,
                        desc_id,
                        channel_freq,
                        paddr,
                        frame,
                        tx_params_valid,
                    },
                )
                .boxed()
        }
    }
    impl CommandStrategy for ScanStop {
        fn strategy() -> BoxedStrategy<Self> {
            (
                any::<(u32, u32, u32, u32)>(),
                prop_oneof![
                    Just(ScanCancelType::PdevAll),
                    Just(ScanCancelType::VdevAll),
                    Just(ScanCancelType::Single)
                ],
            )
                .prop_map(
                    |((requester, scan_id, vdev_id, pdev_id), cancel_type)| Self {
                        requester,
                        scan_id,
                        cancel_type,
                        vdev_id,
                        pdev_id,
                    },
                )
                .boxed()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use proptest::strategy::ValueTree;
        use proptest::test_runner::TestRunner;
        #[test]
        fn priority_strategies_generate_encodable_commands() {
            let mut runner = TestRunner::deterministic();
            for _ in 0..8 {
                assert!(
                    VdevStart::strategy()
                        .new_tree(&mut runner)
                        .unwrap()
                        .current()
                        .encode_command()
                        .is_ok()
                );
                assert!(
                    VdevInstallKey::strategy()
                        .new_tree(&mut runner)
                        .unwrap()
                        .current()
                        .encode_command()
                        .is_ok()
                );
                assert!(
                    MgmtSend::strategy()
                        .new_tree(&mut runner)
                        .unwrap()
                        .current()
                        .encode_command()
                        .is_ok()
                );
            }
        }
    }
}
