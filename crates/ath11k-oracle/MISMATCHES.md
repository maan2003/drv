# Differential conformance findings

The C reference is Linux `509ce3d952d550f93b544c8d94c99e798f09a9b4`.
Findings remain explicit regression cases even when the safer Rust behavior is
intentional; the oracle must not silently normalize a divergence.

## QMI-001: truncated standard-response TLV header is accepted by C

- C function: `qmi_decode_message`, through the WLFW host-capability response
  element table
- input QMI body: `02` (TLV type only; two length bytes are absent)
- C result: success, decoded `result=0`, `error=0`
- Rust function: `Response::checked` followed by `StandardResponse::decode`
- Rust result: `QmiError::Malformed`
- first divergent operation: C's `QMI_ENCDEC_DECODE_TLV` reads a three-byte
  header before checking `in_buf_len`; Rust rejects the truncated header
- disposition: retain Rust rejection. The C harness provides zero guard bytes
  outside the logical input length to make the pinned C out-of-bounds read safe
  in userspace; it does not alter the length passed to the codec.
- regression test: `qmi_response_malformed_acceptance_matches_original_c_codec`
