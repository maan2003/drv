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
