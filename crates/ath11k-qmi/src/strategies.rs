//! Bounded generators for exercising the public WLFW codecs.

use crate::wire::*;
use crate::{RawIndication, Response, TransactionId};
use alloc::vec::Vec;
use proptest::collection::vec;
use proptest::prelude::*;

fn flag() -> impl Strategy<Value = u8> {
    prop_oneof![Just(0), Just(1), any::<u8>()]
}
fn optional_flag() -> impl Strategy<Value = Option<u8>> {
    proptest::option::of(flag())
}
fn memory_type() -> impl Strategy<Value = MemoryType> {
    prop_oneof![0i32..=5, any::<i32>()].prop_map(MemoryType)
}
fn direction() -> impl Strategy<Value = PipeDirection> {
    prop_oneof![
        Just(PipeDirection::None),
        Just(PipeDirection::In),
        Just(PipeDirection::Out),
        Just(PipeDirection::InOut)
    ]
}

pub fn capability_request_strategy() -> impl Strategy<Value = CapabilityRequest> {
    Just(CapabilityRequest)
}
pub fn device_info_request_strategy() -> impl Strategy<Value = DeviceInfoRequest> {
    Just(DeviceInfoRequest)
}

pub fn host_capability_request_strategy() -> impl Strategy<Value = HostCapabilityRequest> {
    (
        (
            proptest::option::of(any::<u32>()),
            proptest::option::of(any::<u32>()),
            proptest::option::of(vec(any::<u32>(), 0..=MAX_GPIOS)),
            optional_flag(),
            optional_flag(),
            optional_flag(),
            optional_flag(),
        ),
        (
            optional_flag(),
            optional_flag(),
            optional_flag(),
            optional_flag(),
            proptest::option::of(any::<u32>()),
            optional_flag(),
        ),
    )
        .prop_map(
            |(
                (
                    num_clients,
                    wake_msi,
                    gpios,
                    nm_modem,
                    bdf_support,
                    bdf_cache_support,
                    m3_support,
                ),
                (
                    m3_cache_support,
                    cal_filesys_support,
                    cal_cache_support,
                    cal_done,
                    mem_bucket,
                    mem_cfg_mode,
                ),
            )| HostCapabilityRequest {
                num_clients,
                wake_msi,
                gpios,
                nm_modem,
                bdf_support,
                bdf_cache_support,
                m3_support,
                m3_cache_support,
                cal_filesys_support,
                cal_cache_support,
                cal_done,
                mem_bucket,
                mem_cfg_mode,
            },
        )
}

pub fn indication_register_request_strategy() -> impl Strategy<Value = IndicationRegisterRequest> {
    (
        (
            optional_flag(),
            optional_flag(),
            optional_flag(),
            optional_flag(),
            optional_flag(),
            proptest::option::of(any::<u32>()),
        ),
        (
            optional_flag(),
            optional_flag(),
            optional_flag(),
            optional_flag(),
            optional_flag(),
            optional_flag(),
        ),
    )
        .prop_map(
            |(
                (
                    fw_ready,
                    initiate_cal_download,
                    initiate_cal_update,
                    msa_ready,
                    pin_connect_result,
                    client_id,
                ),
                (request_memory, fw_memory_ready, fw_init_done, rejuvenate, xo_cal, cal_done),
            )| IndicationRegisterRequest {
                fw_ready,
                initiate_cal_download,
                initiate_cal_update,
                msa_ready,
                pin_connect_result,
                client_id,
                request_memory,
                fw_memory_ready,
                fw_init_done,
                rejuvenate,
                xo_cal,
                cal_done,
            },
        )
}

fn memory_segment_response_strategy() -> impl Strategy<Value = MemorySegmentResponse> {
    (any::<u64>(), any::<u32>(), memory_type(), flag()).prop_map(
        |(address, size, kind, restore)| MemorySegmentResponse {
            address,
            size,
            kind,
            restore,
        },
    )
}
pub fn respond_memory_request_strategy() -> impl Strategy<Value = RespondMemoryRequest> {
    vec(memory_segment_response_strategy(), 0..=MAX_MEMORY_SEGMENTS)
        .prop_map(|segments| RespondMemoryRequest { segments })
}

pub fn bdf_download_request_strategy() -> impl Strategy<Value = BdfDownloadRequest> {
    (
        flag(),
        proptest::option::of(any::<i32>()),
        proptest::option::of(any::<u32>()),
        proptest::option::of(any::<u32>()),
        proptest::option::of(vec(any::<u8>(), 0..=MAX_DATA_SIZE)),
        optional_flag(),
        optional_flag(),
    )
        .prop_map(
            |(valid, file_id, total_size, segment_id, data, end, bdf_type)| BdfDownloadRequest {
                valid,
                file_id,
                total_size,
                segment_id,
                data,
                end,
                bdf_type,
            },
        )
}
pub fn m3_info_request_strategy() -> impl Strategy<Value = M3InfoRequest> {
    (any::<u64>(), any::<u32>()).prop_map(|(address, size)| M3InfoRequest { address, size })
}
pub fn wlan_mode_request_strategy() -> impl Strategy<Value = WlanModeRequest> {
    (any::<u32>(), optional_flag()).prop_map(|(mode, hardware_debug)| WlanModeRequest {
        mode,
        hardware_debug,
    })
}
pub fn wlan_ini_request_strategy() -> impl Strategy<Value = WlanIniRequest> {
    optional_flag().prop_map(|enable_firmware_log| WlanIniRequest {
        enable_firmware_log,
    })
}

fn qmi_string(max: usize) -> impl Strategy<Value = QmiString> {
    vec(any::<u8>(), 0..=max).prop_map(move |v| QmiString::new(v, max).unwrap())
}
fn target_pipe() -> impl Strategy<Value = TargetPipeConfig> {
    (
        any::<u32>(),
        direction(),
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
    )
        .prop_map(
            |(pipe_num, direction, entries, max_bytes, flags)| TargetPipeConfig {
                pipe_num,
                direction,
                entries,
                max_bytes,
                flags,
            },
        )
}
fn service_pipe() -> impl Strategy<Value = ServicePipeConfig> {
    (any::<u32>(), direction(), any::<u32>()).prop_map(|(service_id, direction, pipe_num)| {
        ServicePipeConfig {
            service_id,
            direction,
            pipe_num,
        }
    })
}
fn shadow_register() -> impl Strategy<Value = ShadowRegister> {
    (any::<u16>(), any::<u16>()).prop_map(|(id, offset)| ShadowRegister { id, offset })
}
pub fn wlan_config_request_strategy() -> impl Strategy<Value = WlanConfigRequest> {
    (
        proptest::option::of(qmi_string(16)),
        proptest::option::of(vec(target_pipe(), 0..=MAX_TARGET_PIPES)),
        proptest::option::of(vec(service_pipe(), 0..=MAX_SERVICE_PIPES)),
        proptest::option::of(vec(shadow_register(), 0..=MAX_SHADOW_REGS)),
        proptest::option::of(vec(any::<u32>(), 0..=MAX_SHADOW_REGS_V2)),
    )
        .prop_map(
            |(host_version, target_pipes, service_pipes, shadow_registers, shadow_registers_v2)| {
                WlanConfigRequest {
                    host_version,
                    target_pipes,
                    service_pipes,
                    shadow_registers,
                    shadow_registers_v2,
                }
            },
        )
}

fn response_body(extra: impl Strategy<Value = Vec<u8>>) -> impl Strategy<Value = Response> {
    (any::<u16>(), any::<u16>(), any::<u16>(), extra).prop_map(|(tx, result, error, mut extra)| {
        let mut body = alloc::vec![2, 4, 0];
        body.extend_from_slice(&result.to_le_bytes());
        body.extend_from_slice(&error.to_le_bytes());
        body.append(&mut extra);
        Response::checked(TransactionId::new(tx), MessageId::Capability, body).unwrap()
    })
}
pub fn standard_response_input_strategy() -> impl Strategy<Value = Response> {
    response_body(Just(Vec::new()))
}
pub fn capability_response_input_strategy() -> impl Strategy<Value = Response> {
    response_body(
        (
            proptest::option::of((any::<u32>(), any::<u32>())),
            proptest::option::of(any::<u32>()),
            proptest::option::of(any::<u32>()),
        )
            .prop_map(|(chip, board, soc)| {
                let mut b = Vec::new();
                if let Some((a, c)) = chip {
                    push_tlv(&mut b, 0x10, &[a.to_le_bytes(), c.to_le_bytes()].concat())
                }
                if let Some(x) = board {
                    push_tlv(&mut b, 0x11, &x.to_le_bytes())
                }
                if let Some(x) = soc {
                    push_tlv(&mut b, 0x12, &x.to_le_bytes())
                }
                b
            }),
    )
}
pub fn device_info_response_input_strategy() -> impl Strategy<Value = Response> {
    (
        any::<u16>(),
        any::<u16>(),
        any::<u16>(),
        proptest::option::of(any::<u64>()),
        proptest::option::of(any::<u32>()),
    )
        .prop_map(|(tx, result, error, address, size)| {
            let mut b = alloc::vec![2, 4, 0];
            b.extend_from_slice(&result.to_le_bytes());
            b.extend_from_slice(&error.to_le_bytes());
            if let Some(x) = address {
                push_tlv(&mut b, 0x10, &x.to_le_bytes())
            }
            if let Some(x) = size {
                push_tlv(&mut b, 0x11, &x.to_le_bytes())
            }
            Response::checked(TransactionId::new(tx), MessageId::DeviceInfo, b).unwrap()
        })
}
pub fn indication_register_response_input_strategy() -> impl Strategy<Value = Response> {
    (
        standard_response_input_strategy(),
        proptest::option::of(any::<u64>()),
    )
        .prop_map(|(r, status)| {
            let mut b = r.bytes().to_vec();
            if let Some(x) = status {
                push_tlv(&mut b, 0x10, &x.to_le_bytes())
            }
            Response::checked(r.transaction_id(), MessageId::IndicationRegister, b).unwrap()
        })
}

fn memory_config() -> impl Strategy<Value = MemoryConfig> {
    (any::<u64>(), any::<u32>(), flag()).prop_map(|(offset, size, secure)| MemoryConfig {
        offset,
        size,
        secure,
    })
}
fn memory_segment() -> impl Strategy<Value = MemorySegment> {
    (
        any::<u32>(),
        memory_type(),
        vec(memory_config(), 0..=MAX_MEMORY_CONFIGS),
    )
        .prop_map(|(size, kind, configs)| MemorySegment {
            size,
            kind,
            configs,
        })
}
pub fn request_memory_indication_input_strategy() -> impl Strategy<Value = RawIndication> {
    vec(memory_segment(), 0..=MAX_MEMORY_SEGMENTS).prop_map(|segments| {
        let mut value = alloc::vec![segments.len() as u8];
        for s in segments {
            value.extend_from_slice(&s.size.to_le_bytes());
            value.extend_from_slice(&s.kind.0.to_le_bytes());
            value.push(s.configs.len() as u8);
            for c in s.configs {
                value.extend_from_slice(&c.offset.to_le_bytes());
                value.extend_from_slice(&c.size.to_le_bytes());
                value.push(c.secure)
            }
        }
        let mut body = Vec::new();
        push_tlv(&mut body, 1, &value);
        RawIndication::checked(MessageId::RequestMemory, body).unwrap()
    })
}
pub fn empty_indication_input_strategy() -> impl Strategy<Value = RawIndication> {
    prop_oneof![
        Just(MessageId::FirmwareMemoryReady),
        Just(MessageId::FirmwareReady),
        Just(MessageId::FirmwareInitDone),
        Just(MessageId::ColdBootCalibrationDone)
    ]
    .prop_map(|id| RawIndication::checked(id, Vec::new()).unwrap())
}

fn push_tlv(out: &mut Vec<u8>, kind: u8, value: &[u8]) {
    out.push(kind);
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    proptest! { #[test] fn generated_requests_encode(r in wlan_config_request_strategy()){prop_assert!(r.encode().is_ok())} #[test] fn generated_memory_decodes(i in request_memory_indication_input_strategy()){prop_assert!(RequestMemoryIndication::decode(i.bytes()).is_ok())} }
}
