//! Feature-gated semantic events for differential codec oracles.

use crate::wire::{self, MessageId};
use alloc::format;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectReason {
    TruncatedTlvHeader,
    TruncatedTlvValue,
    UnknownMandatoryTlv,
    WrongLength,
    ArrayTooLong,
    StringTooLong,
    InvalidMessageId,
    UnexpectedMessageType,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceEvent<'a> {
    Elem {
        tlv_type: u8,
        offset: usize,
        len: usize,
    },
    Field {
        name: &'a str,
        value: u64,
    },
    Branch {
        name: &'a str,
    },
    Reject {
        reason: RejectReason,
        offset: usize,
    },
}

pub trait TraceSink {
    fn event(&mut self, event: TraceEvent<'_>);
}

pub(crate) fn reject(sink: &mut dyn TraceSink, reason: RejectReason, offset: usize) {
    sink.event(TraceEvent::Reject { reason, offset });
}

pub(crate) fn branch(sink: &mut dyn TraceSink, name: &'static str) {
    sink.event(TraceEvent::Branch { name });
}

pub(crate) fn field(sink: &mut dyn TraceSink, name: &'static str, value: u64) {
    sink.event(TraceEvent::Field { name, value });
}

pub(crate) fn scalar(sink: &mut dyn TraceSink, path: &'static str, value: &[u8], signed: bool) {
    let raw = match value.len() {
        1 => value[0] as u64,
        2 => u16::from_le_bytes([value[0], value[1]]) as u64,
        4 => u32::from_le_bytes(value.try_into().unwrap()) as u64,
        8 => u64::from_le_bytes(value.try_into().unwrap()),
        _ => return,
    };
    let value = if signed {
        match value.len() {
            1 => (raw as u8 as i8 as i64) as u64,
            2 => (raw as u16 as i16 as i64) as u64,
            4 => (raw as u32 as i32 as i64) as u64,
            _ => raw,
        }
    } else {
        raw
    };
    field(sink, path, value);
}

fn dynamic_field(sink: &mut dyn TraceSink, name: &str, value: u64) {
    sink.event(TraceEvent::Field { name, value });
}

fn wrong_len(sink: &mut dyn TraceSink, offset: usize) {
    reject(sink, RejectReason::WrongLength, offset);
}

/// Trace the semantic fields of an encoded WLFW request.
pub fn trace_request(id: MessageId, body: &[u8], sink: &mut dyn TraceSink) -> bool {
    if !trace_tlvs(body, sink) {
        return false;
    }
    each_tlv(body, |kind, offset, value| {
        let base = offset + 3;
        match id {
            MessageId::HostCapability => match kind {
                0x10 => scalar_checked(sink, "host_capability.num_clients", value, 4, false, base),
                0x11 => scalar_checked(sink, "host_capability.wake_msi", value, 4, false, base),
                0x12 => {
                    trace_u32_array(sink, "host_capability.gpios", value, wire::MAX_GPIOS, base)
                }
                0x13 => scalar_checked(sink, "host_capability.nm_modem", value, 1, false, base),
                0x14 => scalar_checked(sink, "host_capability.bdf_support", value, 1, false, base),
                0x15 => scalar_checked(
                    sink,
                    "host_capability.bdf_cache_support",
                    value,
                    1,
                    false,
                    base,
                ),
                0x16 => scalar_checked(sink, "host_capability.m3_support", value, 1, false, base),
                0x17 => scalar_checked(
                    sink,
                    "host_capability.m3_cache_support",
                    value,
                    1,
                    false,
                    base,
                ),
                0x18 => scalar_checked(
                    sink,
                    "host_capability.cal_filesys_support",
                    value,
                    1,
                    false,
                    base,
                ),
                0x19 => scalar_checked(
                    sink,
                    "host_capability.cal_cache_support",
                    value,
                    1,
                    false,
                    base,
                ),
                0x1a => scalar_checked(sink, "host_capability.cal_done", value, 1, false, base),
                0x1b => scalar_checked(sink, "host_capability.mem_bucket", value, 4, false, base),
                0x1c => scalar_checked(sink, "host_capability.mem_cfg_mode", value, 1, false, base),
                _ => branch(sink, "host_capability.unknown_optional"),
            },
            MessageId::IndicationRegister => {
                let name = match kind {
                    0x10 => "indication_register.fw_ready",
                    0x11 => "indication_register.initiate_cal_download",
                    0x12 => "indication_register.initiate_cal_update",
                    0x13 => "indication_register.msa_ready",
                    0x14 => "indication_register.pin_connect_result",
                    0x15 => "indication_register.client_id",
                    0x16 => "indication_register.request_memory",
                    0x17 => "indication_register.fw_memory_ready",
                    0x18 => "indication_register.fw_init_done",
                    0x19 => "indication_register.rejuvenate",
                    0x1a => "indication_register.xo_cal",
                    0x1b => "indication_register.cal_done",
                    _ => "indication_register.unknown_optional",
                };
                if kind == 0x15 {
                    scalar_checked(sink, name, value, 4, false, base)
                } else if (0x10..=0x1b).contains(&kind) {
                    scalar_checked(sink, name, value, 1, false, base)
                } else {
                    branch(sink, name)
                }
            }
            MessageId::RespondMemory if kind == 1 => trace_respond_memory(sink, value, base),
            MessageId::BdfDownload => match kind {
                1 => scalar_checked(sink, "bdf_download.valid", value, 1, false, base),
                0x10 => scalar_checked(sink, "bdf_download.file_id", value, 4, true, base),
                0x11 => scalar_checked(sink, "bdf_download.total_size", value, 4, false, base),
                0x12 => scalar_checked(sink, "bdf_download.segment_id", value, 4, false, base),
                0x13 => {
                    trace_counted_bytes(sink, "bdf_download.data", value, wire::MAX_DATA_SIZE, base)
                }
                0x14 => scalar_checked(sink, "bdf_download.end", value, 1, false, base),
                0x15 => scalar_checked(sink, "bdf_download.bdf_type", value, 1, false, base),
                _ => branch(sink, "bdf_download.unknown_optional"),
            },
            MessageId::M3Info => match kind {
                1 => scalar_checked(sink, "m3_info.address", value, 8, false, base),
                2 => scalar_checked(sink, "m3_info.size", value, 4, false, base),
                _ => reject(sink, RejectReason::UnknownMandatoryTlv, offset),
            },
            MessageId::WlanMode => match kind {
                1 => scalar_checked(sink, "wlan_mode.mode", value, 4, false, base),
                0x10 => scalar_checked(sink, "wlan_mode.hardware_debug", value, 1, false, base),
                _ if kind < 0x10 => reject(sink, RejectReason::UnknownMandatoryTlv, offset),
                _ => branch(sink, "wlan_mode.unknown_optional"),
            },
            MessageId::WlanConfig => trace_wlan_config(sink, kind, value, base),
            MessageId::WlanIni if kind == 0x10 => {
                scalar_checked(sink, "wlan_ini.enable_firmware_log", value, 1, false, base)
            }
            MessageId::Capability | MessageId::DeviceInfo if kind >= 0x10 => {
                branch(sink, "request.empty.unknown_optional")
            }
            _ if kind < 0x10 => reject(sink, RejectReason::UnknownMandatoryTlv, offset),
            _ => branch(sink, "request.unknown_optional"),
        }
    });
    true
}

fn scalar_checked(
    sink: &mut dyn TraceSink,
    path: &'static str,
    v: &[u8],
    len: usize,
    signed: bool,
    offset: usize,
) {
    if v.len() != len {
        wrong_len(sink, offset)
    } else {
        scalar(sink, path, v, signed)
    }
}
fn trace_u32_array(sink: &mut dyn TraceSink, path: &str, v: &[u8], max: usize, offset: usize) {
    let Some((&count, rest)) = v.split_first() else {
        wrong_len(sink, offset);
        return;
    };
    dynamic_field(sink, &format!("{path}.len"), count as u64);
    if count as usize > max {
        reject(sink, RejectReason::ArrayTooLong, offset);
        return;
    }
    if rest.len() != count as usize * 4 {
        wrong_len(sink, offset);
        return;
    }
    for (i, x) in rest.chunks_exact(4).enumerate() {
        dynamic_field(
            sink,
            &format!("{path}[{i}]"),
            u32::from_le_bytes(x.try_into().unwrap()) as u64,
        )
    }
}
fn trace_counted_bytes(sink: &mut dyn TraceSink, path: &str, v: &[u8], max: usize, offset: usize) {
    if v.len() < 2 {
        wrong_len(sink, offset);
        return;
    }
    let n = u16::from_le_bytes([v[0], v[1]]) as usize;
    dynamic_field(sink, &format!("{path}.len"), n as u64);
    if n > max {
        reject(sink, RejectReason::ArrayTooLong, offset);
        return;
    }
    if v.len() != 2 + n {
        wrong_len(sink, offset);
        return;
    }
    for (i, x) in v[2..].iter().enumerate() {
        dynamic_field(sink, &format!("{path}[{i}]"), *x as u64)
    }
}
fn trace_respond_memory(sink: &mut dyn TraceSink, v: &[u8], offset: usize) {
    let Some((&n, rest)) = v.split_first() else {
        wrong_len(sink, offset);
        return;
    };
    field(sink, "respond_memory.segments.len", n as u64);
    if n as usize > wire::MAX_MEMORY_SEGMENTS {
        reject(sink, RejectReason::ArrayTooLong, offset);
        return;
    }
    if rest.len() != n as usize * 17 {
        wrong_len(sink, offset);
        return;
    }
    for (i, x) in rest.chunks_exact(17).enumerate() {
        dynamic_field(
            sink,
            &format!("respond_memory.segments[{i}].address"),
            u64::from_le_bytes(x[0..8].try_into().unwrap()),
        );
        dynamic_field(
            sink,
            &format!("respond_memory.segments[{i}].size"),
            u32::from_le_bytes(x[8..12].try_into().unwrap()) as u64,
        );
        dynamic_field(
            sink,
            &format!("respond_memory.segments[{i}].kind"),
            (i32::from_le_bytes(x[12..16].try_into().unwrap()) as i64) as u64,
        );
        dynamic_field(
            sink,
            &format!("respond_memory.segments[{i}].restore"),
            x[16] as u64,
        )
    }
}
fn trace_wlan_config(sink: &mut dyn TraceSink, k: u8, v: &[u8], offset: usize) {
    match k {
        0x10 => {
            if v.len() > 16 {
                reject(sink, RejectReason::StringTooLong, offset)
            } else {
                dynamic_field(sink, "wlan_config.host_version.len", v.len() as u64);
                for (i, x) in v.iter().enumerate() {
                    dynamic_field(sink, &format!("wlan_config.host_version[{i}]"), *x as u64)
                }
            }
        }
        0x11 => trace_struct_array(
            sink,
            "wlan_config.target_pipes",
            v,
            wire::MAX_TARGET_PIPES,
            20,
            &[4, 4, 4, 4, 4],
            &["pipe_num", "direction", "entries", "max_bytes", "flags"],
            offset,
        ),
        0x12 => trace_struct_array(
            sink,
            "wlan_config.service_pipes",
            v,
            wire::MAX_SERVICE_PIPES,
            12,
            &[4, 4, 4],
            &["service_id", "direction", "pipe_num"],
            offset,
        ),
        0x13 => trace_struct_array(
            sink,
            "wlan_config.shadow_registers",
            v,
            wire::MAX_SHADOW_REGS,
            4,
            &[2, 2],
            &["id", "offset"],
            offset,
        ),
        0x14 => trace_struct_array(
            sink,
            "wlan_config.shadow_registers_v2",
            v,
            wire::MAX_SHADOW_REGS_V2,
            4,
            &[4],
            &["value"],
            offset,
        ),
        _ => branch(sink, "wlan_config.unknown_optional"),
    }
}
#[allow(clippy::too_many_arguments)]
fn trace_struct_array(
    sink: &mut dyn TraceSink,
    path: &str,
    v: &[u8],
    max: usize,
    width: usize,
    widths: &[usize],
    names: &[&str],
    offset: usize,
) {
    let Some((&n, rest)) = v.split_first() else {
        wrong_len(sink, offset);
        return;
    };
    dynamic_field(sink, &format!("{path}.len"), n as u64);
    if n as usize > max {
        reject(sink, RejectReason::ArrayTooLong, offset);
        return;
    }
    if rest.len() != n as usize * width {
        wrong_len(sink, offset);
        return;
    }
    for (i, item) in rest.chunks_exact(width).enumerate() {
        let mut p = 0;
        for (&w, name) in widths.iter().zip(names) {
            let raw = match w {
                2 => u16::from_le_bytes(item[p..p + 2].try_into().unwrap()) as u64,
                4 => u32::from_le_bytes(item[p..p + 4].try_into().unwrap()) as u64,
                _ => 0,
            };
            let raw = if *name == "direction" {
                (raw as u32 as i32 as i64) as u64
            } else {
                raw
            };
            dynamic_field(sink, &format!("{path}[{i}].{name}"), raw);
            p += w
        }
    }
}

/// Trace TLV framing. Semantic entry points add fields after this succeeds.
pub fn trace_tlvs(body: &[u8], sink: &mut dyn TraceSink) -> bool {
    let mut offset = 0;
    while offset < body.len() {
        if body.len() - offset < 3 {
            reject(sink, RejectReason::TruncatedTlvHeader, offset);
            return false;
        }
        let len = u16::from_le_bytes([body[offset + 1], body[offset + 2]]) as usize;
        sink.event(TraceEvent::Elem {
            tlv_type: body[offset],
            offset,
            len,
        });
        if body.len() - offset - 3 < len {
            reject(sink, RejectReason::TruncatedTlvValue, offset + 3);
            return false;
        }
        offset += 3 + len;
    }
    true
}

pub(crate) fn each_tlv(mut body: &[u8], mut f: impl FnMut(u8, usize, &[u8])) {
    let mut offset = 0;
    while body.len() >= 3 {
        let len = u16::from_le_bytes([body[1], body[2]]) as usize;
        if body.len() < 3 + len {
            return;
        }
        f(body[0], offset, &body[3..3 + len]);
        body = &body[3 + len..];
        offset += 3 + len;
    }
}

/// Report an invalid numeric WLFW message id without changing normal parsing.
pub fn trace_message_id(
    value: u16,
    sink: &mut dyn TraceSink,
) -> Result<MessageId, crate::QmiError> {
    match MessageId::from_u16(value) {
        Ok(id) => {
            field(sink, "message.id", value as u64);
            Ok(id)
        }
        Err(error) => {
            reject(sink, RejectReason::InvalidMessageId, 0);
            Err(error)
        }
    }
}

macro_rules! traced_encode_ref {
    ($ty:ty) => {
        impl $ty {
            pub fn encode_with_trace(
                &self,
                trace: Option<&mut dyn TraceSink>,
            ) -> Result<crate::Request, crate::QmiError> {
                let result = self.encode();
                trace_encode_result(result, trace)
            }
        }
    };
}
macro_rules! traced_encode_copy {
    ($ty:ty) => {
        impl $ty {
            pub fn encode_with_trace(
                self,
                trace: Option<&mut dyn TraceSink>,
            ) -> Result<crate::Request, crate::QmiError> {
                let result = self.encode();
                trace_encode_result(result, trace)
            }
        }
    };
}
fn trace_encode_result(
    result: Result<crate::Request, crate::QmiError>,
    trace: Option<&mut dyn TraceSink>,
) -> Result<crate::Request, crate::QmiError> {
    if let Some(sink) = trace {
        match &result {
            Ok(request) => {
                trace_request(request.message_id(), request.bytes(), sink);
            }
            Err(crate::QmiError::MessageTooLong) => reject(sink, RejectReason::ArrayTooLong, 0),
            Err(_) => reject(sink, RejectReason::WrongLength, 0),
        }
    }
    result
}

traced_encode_copy!(wire::CapabilityRequest);
traced_encode_copy!(wire::DeviceInfoRequest);
traced_encode_ref!(wire::IndicationRegisterRequest);
traced_encode_copy!(wire::M3InfoRequest);
traced_encode_copy!(wire::WlanModeRequest);
traced_encode_copy!(wire::WlanIniRequest);

impl wire::HostCapabilityRequest {
    pub fn encode_with_trace(
        &self,
        trace: Option<&mut dyn TraceSink>,
    ) -> Result<crate::Request, crate::QmiError> {
        if let Some(s) = trace {
            if self
                .gpios
                .as_ref()
                .is_some_and(|v| v.len() > wire::MAX_GPIOS)
            {
                reject(s, RejectReason::ArrayTooLong, 0);
                return self.encode();
            }
            let r = self.encode();
            if let Ok(x) = &r {
                trace_request(x.message_id(), x.bytes(), s);
            }
            return r;
        }
        self.encode()
    }
}
impl wire::RespondMemoryRequest {
    pub fn encode_with_trace(
        &self,
        trace: Option<&mut dyn TraceSink>,
    ) -> Result<crate::Request, crate::QmiError> {
        if let Some(s) = trace {
            if self.segments.len() > wire::MAX_MEMORY_SEGMENTS {
                reject(s, RejectReason::ArrayTooLong, 0);
                return self.encode();
            }
            let r = self.encode();
            if let Ok(x) = &r {
                trace_request(x.message_id(), x.bytes(), s);
            }
            return r;
        }
        self.encode()
    }
}
impl wire::BdfDownloadRequest {
    pub fn encode_with_trace(
        &self,
        trace: Option<&mut dyn TraceSink>,
    ) -> Result<crate::Request, crate::QmiError> {
        if let Some(s) = trace {
            if self
                .data
                .as_ref()
                .is_some_and(|v| v.len() > wire::MAX_DATA_SIZE)
            {
                reject(s, RejectReason::ArrayTooLong, 0);
                return self.encode();
            }
            let r = self.encode();
            if let Ok(x) = &r {
                trace_request(x.message_id(), x.bytes(), s);
            }
            return r;
        }
        self.encode()
    }
}
impl wire::WlanConfigRequest {
    pub fn encode_with_trace(
        &self,
        trace: Option<&mut dyn TraceSink>,
    ) -> Result<crate::Request, crate::QmiError> {
        if let Some(s) = trace {
            let reason = if self
                .host_version
                .as_ref()
                .is_some_and(|v| v.as_bytes().len() > 16)
            {
                Some(RejectReason::StringTooLong)
            } else if self
                .target_pipes
                .as_ref()
                .is_some_and(|v| v.len() > wire::MAX_TARGET_PIPES)
                || self
                    .service_pipes
                    .as_ref()
                    .is_some_and(|v| v.len() > wire::MAX_SERVICE_PIPES)
                || self
                    .shadow_registers
                    .as_ref()
                    .is_some_and(|v| v.len() > wire::MAX_SHADOW_REGS)
                || self
                    .shadow_registers_v2
                    .as_ref()
                    .is_some_and(|v| v.len() > wire::MAX_SHADOW_REGS_V2)
            {
                Some(RejectReason::ArrayTooLong)
            } else {
                None
            };
            if let Some(reason) = reason {
                reject(s, reason, 0);
                return self.encode();
            }
            let r = self.encode();
            if let Ok(x) = &r {
                trace_request(x.message_id(), x.bytes(), s);
            }
            return r;
        }
        self.encode()
    }
}

fn trace_response_fields(body: &[u8], sink: &mut dyn TraceSink) {
    if !trace_tlvs(body, sink) {
        return;
    }
    each_tlv(body, |kind, offset, v| {
        if kind == 2 {
            if v.len() != 4 {
                wrong_len(sink, offset + 3)
            } else {
                scalar(sink, "response.result", &v[..2], false);
                scalar(sink, "response.error", &v[2..], false)
            }
        } else if kind < 0x10 {
            reject(sink, RejectReason::UnknownMandatoryTlv, offset)
        } else {
            branch(sink, "response.unknown_optional")
        }
    })
}

impl wire::StandardResponse {
    pub fn decode_with_trace(
        response: &crate::Response,
        trace: Option<&mut dyn TraceSink>,
    ) -> Result<Self, crate::QmiError> {
        if let Some(s) = trace {
            trace_response_fields(response.bytes(), s)
        }
        Self::decode(response)
    }
}
impl wire::IndicationRegisterResponse {
    pub fn decode_with_trace(
        response: &crate::Response,
        trace: Option<&mut dyn TraceSink>,
    ) -> Result<Self, crate::QmiError> {
        if let Some(s) = trace {
            if response.message_id() != MessageId::IndicationRegister {
                reject(s, RejectReason::UnexpectedMessageType, 0);
                return Err(crate::QmiError::Malformed);
            }
            trace_response_fields(response.bytes(), s);
            each_tlv(response.bytes(), |kind, offset, v| {
                if kind == 0x10 {
                    scalar_checked(
                        s,
                        "indication_register_response.firmware_status",
                        v,
                        8,
                        false,
                        offset + 3,
                    )
                }
            })
        }
        Self::decode(response)
    }
}
impl wire::DeviceInfoResponse {
    pub fn decode_with_trace(
        response: &crate::Response,
        trace: Option<&mut dyn TraceSink>,
    ) -> Result<Self, crate::QmiError> {
        if let Some(s) = trace {
            if response.message_id() != MessageId::DeviceInfo {
                reject(s, RejectReason::UnexpectedMessageType, 0);
                return Err(crate::QmiError::Malformed);
            }
            trace_response_fields(response.bytes(), s);
            each_tlv(response.bytes(), |kind, offset, v| match kind {
                0x10 => scalar_checked(s, "device_info.bar_address", v, 8, false, offset + 3),
                0x11 => scalar_checked(s, "device_info.bar_size", v, 4, false, offset + 3),
                _ => {}
            })
        }
        Self::decode(response)
    }
}
impl wire::CapabilityResponse {
    pub fn decode_with_trace(
        response: &crate::Response,
        trace: Option<&mut dyn TraceSink>,
    ) -> Result<Self, crate::QmiError> {
        if let Some(s) = trace {
            if response.message_id() != MessageId::Capability {
                reject(s, RejectReason::UnexpectedMessageType, 0);
                return Err(crate::QmiError::Malformed);
            }
            trace_capability(response.bytes(), s)
        }
        Self::decode(response)
    }
}
fn trace_capability(body: &[u8], sink: &mut dyn TraceSink) {
    if !trace_tlvs(body, sink) {
        return;
    }
    each_tlv(body, |k, o, v| match k {
        2 => {
            if v.len() != 4 {
                wrong_len(sink, o + 3)
            } else {
                scalar(sink, "capability.response.result", &v[..2], false);
                scalar(sink, "capability.response.error", &v[2..], false)
            }
        }
        0x10 => {
            if v.len() != 8 {
                wrong_len(sink, o + 3)
            } else {
                scalar(sink, "capability.chip.chip_id", &v[..4], false);
                scalar(sink, "capability.chip.chip_family", &v[4..], false)
            }
        }
        0x11 => scalar_checked(sink, "capability.board_id", v, 4, false, o + 3),
        0x12 => scalar_checked(sink, "capability.soc_id", v, 4, false, o + 3),
        0x13 => {
            if v.len() < 5 {
                wrong_len(sink, o + 3)
            } else {
                scalar(sink, "capability.firmware.version", &v[..4], false);
                trace_nested_string(sink, "capability.firmware.timestamp", &v[4..], 32, o + 7)
            }
        }
        0x14 => trace_string(sink, "capability.firmware_build_id", v, 128, o + 3),
        0x15 => scalar_checked(sink, "capability.num_macs", v, 1, false, o + 3),
        0x16 => scalar_checked(sink, "capability.voltage_mv", v, 4, false, o + 3),
        0x17 => scalar_checked(sink, "capability.time_frequency_hz", v, 4, false, o + 3),
        0x18 => scalar_checked(sink, "capability.otp_version", v, 4, false, o + 3),
        0x19 => scalar_checked(sink, "capability.eeprom_read_timeout", v, 4, false, o + 3),
        _ if k < 0x10 => reject(sink, RejectReason::UnknownMandatoryTlv, o),
        _ => branch(sink, "capability.unknown_optional"),
    })
}
fn trace_string(sink: &mut dyn TraceSink, path: &str, v: &[u8], max: usize, offset: usize) {
    if v.len() > max {
        reject(sink, RejectReason::StringTooLong, offset);
        return;
    }
    dynamic_field(
        sink,
        &format!("{path}.len"),
        v.iter().position(|x| *x == 0).unwrap_or(v.len()) as u64,
    );
    for (i, x) in v.iter().take_while(|x| **x != 0).enumerate() {
        dynamic_field(sink, &format!("{path}[{i}]"), *x as u64)
    }
}
fn trace_nested_string(sink: &mut dyn TraceSink, path: &str, v: &[u8], max: usize, offset: usize) {
    let Some((&n, rest)) = v.split_first() else {
        wrong_len(sink, offset);
        return;
    };
    if n as usize > max {
        reject(sink, RejectReason::StringTooLong, offset);
        return;
    }
    if rest.len() != n as usize {
        wrong_len(sink, offset);
        return;
    }
    dynamic_field(sink, &format!("{path}.len"), n as u64);
    for (i, x) in rest.iter().enumerate() {
        dynamic_field(sink, &format!("{path}[{i}]"), *x as u64)
    }
}

impl wire::RequestMemoryIndication {
    pub fn decode_with_trace(
        bytes: &[u8],
        trace: Option<&mut dyn TraceSink>,
    ) -> Result<Self, crate::QmiError> {
        if let Some(s) = trace {
            trace_request_memory(bytes, s)
        }
        Self::decode(bytes)
    }
}
fn trace_request_memory(body: &[u8], sink: &mut dyn TraceSink) {
    if !trace_tlvs(body, sink) {
        return;
    }
    each_tlv(body, |k, o, v| {
        if k != 1 {
            if k < 0x10 {
                reject(sink, RejectReason::UnknownMandatoryTlv, o)
            } else {
                branch(sink, "request_memory.unknown_optional")
            }
            return;
        }
        let Some((&n, mut rest)) = v.split_first() else {
            wrong_len(sink, o + 3);
            return;
        };
        field(sink, "request_memory.segments.len", n as u64);
        if n as usize > wire::MAX_MEMORY_SEGMENTS {
            reject(sink, RejectReason::ArrayTooLong, o + 3);
            return;
        }
        let mut consumed = 1;
        for i in 0..n as usize {
            if rest.len() < 9 {
                wrong_len(sink, o + 3 + consumed);
                return;
            }
            dynamic_field(
                sink,
                &format!("request_memory.segments[{i}].size"),
                u32::from_le_bytes(rest[..4].try_into().unwrap()) as u64,
            );
            dynamic_field(
                sink,
                &format!("request_memory.segments[{i}].kind"),
                (i32::from_le_bytes(rest[4..8].try_into().unwrap()) as i64) as u64,
            );
            let cn = rest[8] as usize;
            dynamic_field(
                sink,
                &format!("request_memory.segments[{i}].configs.len"),
                cn as u64,
            );
            if cn > wire::MAX_MEMORY_CONFIGS {
                reject(sink, RejectReason::ArrayTooLong, o + 3 + consumed + 8);
                return;
            }
            rest = &rest[9..];
            consumed += 9;
            for j in 0..cn {
                if rest.len() < 13 {
                    wrong_len(sink, o + 3 + consumed);
                    return;
                }
                dynamic_field(
                    sink,
                    &format!("request_memory.segments[{i}].configs[{j}].offset"),
                    u64::from_le_bytes(rest[..8].try_into().unwrap()),
                );
                dynamic_field(
                    sink,
                    &format!("request_memory.segments[{i}].configs[{j}].size"),
                    u32::from_le_bytes(rest[8..12].try_into().unwrap()) as u64,
                );
                dynamic_field(
                    sink,
                    &format!("request_memory.segments[{i}].configs[{j}].secure"),
                    rest[12] as u64,
                );
                rest = &rest[13..];
                consumed += 13
            }
        }
        if !rest.is_empty() {
            wrong_len(sink, o + 3 + consumed)
        }
    })
}

impl wire::Indication {
    pub fn decode_with_trace(
        id: MessageId,
        bytes: &[u8],
        trace: Option<&mut dyn TraceSink>,
    ) -> Result<Self, crate::QmiError> {
        if let Some(s) = trace {
            match id {
                MessageId::RequestMemory => trace_request_memory(bytes, s),
                MessageId::FirmwareMemoryReady
                | MessageId::FirmwareReady
                | MessageId::ColdBootCalibrationDone
                | MessageId::FirmwareInitDone => {
                    trace_tlvs(bytes, s);
                    each_tlv(bytes, |k, o, _| {
                        if k < 0x10 {
                            reject(s, RejectReason::UnknownMandatoryTlv, o)
                        } else {
                            branch(s, "indication.unknown_optional")
                        }
                    })
                }
                _ => reject(s, RejectReason::UnexpectedMessageType, 0),
            }
        }
        Self::decode(id, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{string::String, vec::Vec};

    #[derive(Debug, Eq, PartialEq)]
    enum Event {
        Elem(u8, usize, usize),
        Field(String, u64),
        Branch(String),
        Reject(RejectReason, usize),
    }
    #[derive(Default)]
    struct Sink(Vec<Event>);
    impl TraceSink for Sink {
        fn event(&mut self, event: TraceEvent<'_>) {
            self.0.push(match event {
                TraceEvent::Elem {
                    tlv_type,
                    offset,
                    len,
                } => Event::Elem(tlv_type, offset, len),
                TraceEvent::Field { name, value } => Event::Field(name.into(), value),
                TraceEvent::Branch { name } => Event::Branch(name.into()),
                TraceEvent::Reject { reason, offset } => Event::Reject(reason, offset),
            });
        }
    }

    #[test]
    fn framing_rejects_have_body_relative_offsets() {
        let mut sink = Sink::default();
        assert!(!trace_tlvs(&[0x10, 4], &mut sink));
        assert_eq!(sink.0, [Event::Reject(RejectReason::TruncatedTlvHeader, 0)]);
        let mut sink = Sink::default();
        assert!(!trace_tlvs(&[0x10, 4, 0, 1], &mut sink));
        assert_eq!(
            sink.0,
            [
                Event::Elem(0x10, 0, 4),
                Event::Reject(RejectReason::TruncatedTlvValue, 3)
            ]
        );
    }

    #[test]
    fn nested_paths_and_signed_values_are_stable() {
        let request = wire::RespondMemoryRequest {
            segments: alloc::vec![wire::MemorySegmentResponse {
                address: 7,
                size: 8,
                kind: wire::MemoryType(-1),
                restore: 1
            }],
        };
        let mut sink = Sink::default();
        request.encode_with_trace(Some(&mut sink)).unwrap();
        assert!(sink.0.contains(&Event::Field(
            "respond_memory.segments[0].kind".into(),
            u64::MAX
        )));
        assert!(sink.0.contains(&Event::Field(
            "respond_memory.segments[0].address".into(),
            7
        )));
    }

    #[test]
    fn semantic_rejects_distinguish_length_causes() {
        let response = crate::Response::checked(
            crate::TransactionId::new(1),
            MessageId::Capability,
            alloc::vec![2, 3, 0, 0, 0, 0],
        )
        .unwrap();
        let mut sink = Sink::default();
        assert!(wire::CapabilityResponse::decode_with_trace(&response, Some(&mut sink)).is_err());
        assert!(
            sink.0
                .contains(&Event::Reject(RejectReason::WrongLength, 3))
        );
        let mut sink = Sink::default();
        let string = alloc::vec![0x14, 129, 0];
        let mut body = alloc::vec![2, 4, 0, 0, 0, 0, 0];
        body.extend_from_slice(&string);
        body.extend(core::iter::repeat_n(b'x', 129));
        let response =
            crate::Response::checked(crate::TransactionId::new(1), MessageId::Capability, body)
                .unwrap();
        assert!(wire::CapabilityResponse::decode_with_trace(&response, Some(&mut sink)).is_err());
        assert!(
            sink.0
                .iter()
                .any(|e| matches!(e, Event::Reject(RejectReason::StringTooLong, 10)))
        );
    }
}
