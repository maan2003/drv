#![deny(unsafe_op_in_unsafe_fn)]

use ath11k_qmi::wire::{HostCapabilityRequest, QmiResponse, StandardResponse};
use ath11k_qmi::{QmiError, Response, TransactionId};
use core::ffi::c_int;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceEvent {
    Tlv { tag: u8, len: usize, offset: usize },
    Field { name: &'static str, value: u64, offset: usize },
    Reject { reason: RejectReason, offset: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectReason { InvalidArgument, Malformed }

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CEvent { kind: u8, tag: u8, reserved: u16, offset: usize, len: usize, value: u64 }

unsafe extern "C" {
    fn oracle_qmi_host_cap_encode(present: u8, value: u32, out: *mut u8,
        capacity: usize, events: *mut CEvent, event_count: *mut usize) -> c_int;
    fn oracle_qmi_response_decode(body: *const u8, body_len: usize,
        result: *mut u16, error: *mut u16) -> c_int;
}

pub fn c_host_capability_num_clients(value: Option<u32>) -> Result<(Vec<u8>, Vec<TraceEvent>), i32> {
    let mut out = vec![0u8; 261];
    let mut raw_events = [CEvent::default(); 2];
    let mut event_count = raw_events.len();
    // SAFETY: all pointers reference writable/readable allocations for the supplied lengths;
    // the C wrapper bounds output by `capacity` and at most two trace records.
    let rc = unsafe {
        oracle_qmi_host_cap_encode(value.is_some() as u8, value.unwrap_or(0), out.as_mut_ptr(),
            out.len(), raw_events.as_mut_ptr(), &mut event_count)
    };
    if rc < 0 { return Err(rc); }
    out.truncate(rc as usize);
    let trace = raw_events[..event_count].iter().map(|event| match event.kind {
        1 => TraceEvent::Tlv { tag: event.tag, len: event.len, offset: event.offset },
        2 => TraceEvent::Field { name: "host_capability.num_clients", value: event.value, offset: event.offset },
        _ => unreachable!("C oracle emitted an unknown event kind"),
    }).collect();
    Ok((out, trace))
}

pub fn rust_host_capability_num_clients(value: Option<u32>) -> Result<(Vec<u8>, Vec<TraceEvent>), QmiError> {
    let request = HostCapabilityRequest { num_clients: value, ..HostCapabilityRequest::default() }.encode()?;
    let trace = value.map_or_else(Vec::new, |value| vec![
        TraceEvent::Tlv { tag: 0x10, len: 4, offset: 0 },
        TraceEvent::Field { name: "host_capability.num_clients", value: u64::from(value), offset: 3 },
    ]);
    Ok((request.bytes().to_vec(), trace))
}

pub fn c_standard_response(body: &[u8]) -> Result<QmiResponse, i32> {
    let mut result = 0;
    let mut error = 0;
    // SAFETY: `body` is readable for `body.len()` and both output pointers are valid u16s.
    let rc = unsafe { oracle_qmi_response_decode(body.as_ptr(), body.len(), &mut result, &mut error) };
    if rc < 0 { Err(rc) } else { Ok(QmiResponse { result, error }) }
}

pub fn rust_standard_response(body: &[u8]) -> Result<QmiResponse, QmiError> {
    let response = Response::checked(TransactionId::new(1), ath11k_qmi::wire::MessageId::HostCapability, body.to_vec())?;
    Ok(StandardResponse::decode(&response)?.response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qmi_host_cap_num_clients_matches_original_c_codec_and_trace() {
        for value in [None, Some(0), Some(1), Some(u32::MAX), Some(0x4b4e454c)] {
            let c = c_host_capability_num_clients(value).unwrap();
            let rust = rust_host_capability_num_clients(value).unwrap();
            assert_eq!(rust, c, "num_clients={value:?}");
        }
    }

    #[test]
    fn qmi_response_values_match_original_c_codec() {
        for (result, error) in [(0, 0), (1, 1), (1, 94), (u16::MAX, u16::MAX)] {
            let mut body = vec![2, 4, 0];
            body.extend_from_slice(&result.to_le_bytes());
            body.extend_from_slice(&error.to_le_bytes());
            assert_eq!(rust_standard_response(&body).unwrap(), c_standard_response(&body).unwrap());
        }
    }

    #[test]
    fn qmi_response_malformed_acceptance_matches_original_c_codec() {
        let corpus: &[&[u8]] = &[
            &[], &[2], &[2, 4], &[2, 4, 0], &[2, 4, 0, 0],
            &[2, 3, 0, 0, 0, 0], &[1, 0, 0], &[0x10, 0, 0],
            &[2, 4, 0, 0, 0, 0, 0, 0xff],
        ];
        for body in corpus {
            let c = c_standard_response(body);
            let rust = rust_standard_response(body);
            if *body == [2] {
                assert_eq!(rust, Err(QmiError::Malformed));
                assert_eq!(c, Ok(QmiResponse { result: 0, error: 0 }));
                continue;
            }
            assert_eq!(rust.is_ok(), c.is_ok(), "acceptance differs for {body:02x?}: rust={rust:?}, C={c:?}");
            if let (Ok(rust), Ok(c)) = (rust, c) { assert_eq!(rust, c, "fields differ for {body:02x?}"); }
        }
    }
}
