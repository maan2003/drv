# Differential harness issues

## WMI init memory-chunk trace

Owner: `eng-6j4g`.

`EncodeCommand::encode_command_with_trace` reports a false
`Reject { reason: Truncated, offset: 332 }` for a valid `Init` command with a
nonempty host-memory-chunk array. The pinned chunk TLV advertises
`sizeof(struct wlan_host_mem_chunk)` (16 bytes), including its four-byte TLV
header, while the generic nested tracer interprets 16 as a value-only length.
C and Rust command bytes are identical. Checkpoint 2 trace equivalence remains
open until the hook handles this documented init-command length convention.
