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

## TX-001: unknown firmware completion releases a live owner

- C function: `ath11k_dp_tx_process_htt_tx_complete`, `dp_tx.c:390-431`
- input: firmware WBM completion with an unrecognized four-bit HTT status and
  a software cookie naming a live TX owner
- C result: warns and retains the TX owner indefinitely
- Rust result: releases the DMA owner and reports a failed completion
- disposition: retain the Rust behavior. At the host seam, an untrusted
  firmware status must not strand a live DMA mapping.
- test status: exercised as an explicit accepted branch by the completion
  decision differential

## TX-002: native-WiFi QoS TID comes from the frame

- C function: `ath11k_dp_tx_get_tid`, `dp_tx.c:43-54`
- input: native-WiFi QoS frame whose QoS-control TID differs from the skb
  priority maintained by mac80211
- C result: uses `skb->priority & IEEE80211_QOS_CTL_TID_MASK`
- Rust result: uses the QoS-control TID removed by native-WiFi encapsulation
- disposition: retain the Rust behavior. The host seam has no separate skb
  priority; mac80211 derives that priority from the same QoS TID.
- test status: generated encap coverage varies the frame TID directly
