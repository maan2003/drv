# Differential conformance findings

The C reference is Linux `509ce3d952d550f93b544c8d94c99e798f09a9b4`.
The port does not reproduce defects in the reference C. Faithful-port means
faithful to documented hardware and protocol behaviour, not to C behaviour
that the governing documentation does not sanction. Such a behaviour
difference is recorded here with its minimal input; the Rust behaviour remains
the expected result and the owning crate's `PORT-MAP.md` records the exception.

Equivalence tests exercise valid typed messages only. A finding originating
from earlier malformed-input investigation remains documented, but is not part
of the differential suite.

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
- test status: not exercised by the valid-input differential suite
