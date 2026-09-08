# Differential harness issues

## WMI init memory-chunk trace

Status: Fixed.

`EncodeCommand::encode_command_with_trace` previously reported a false
`Reject { reason: Truncated, offset: 332 }` for a valid `Init` command with a
nonempty host-memory-chunk array. The pinned chunk TLV advertises
`sizeof(struct wlan_host_mem_chunk)` (16 bytes), including its four-byte TLV
header, while the generic nested tracer interprets 16 as a value-only length.
C and Rust command bytes are identical. The trace walker now handles this
documented init-command length convention without reporting truncation.

## Connect-flow C provenance

Status: Open.

The WMI command oracle is a wire-level transcription of the pinned builders in
`ath11k/wmi.c`, not a compiled invocation of those functions. In particular,
the `VdevStart` generator supplies arbitrary already-normalized channel words
that do not necessarily have a semantic input accepted by the pinned channel
helper. The differential run therefore proves the transcribed wire contract,
but does not independently exercise that helper's channel normalization.

## Newly typed event provenance

Status: Open.

The pinned `ath11k_wmi_tlv_op_rx` has no cases for event IDs `0x16005`,
`0xb00b`, `0x1d00a`, or `0x601a`. Their C-side semantic checks are
FW-table-derived transcriptions layered on the pinned generic TLV iterator,
rather than calls through the pinned Linux event dispatcher.
