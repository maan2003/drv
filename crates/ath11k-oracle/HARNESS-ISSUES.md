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

## RX helper provenance

Status: Open.

The RX C oracle is a wire-level transcription of the QCN9074 descriptor
accessors selected by `wcn6750_ops` and the small `dp_rx.c` attention helpers.
It does not compile those helpers directly because they depend on kernel skb,
mac80211, and hardware-operation-table infrastructure. The generated-input
differential therefore proves agreement with the pinned bit and frame
transformations, but not an independently linked invocation of those symbols.

## TX helper provenance

Status: Open.

The TX C oracle is a wire-level transcription of `ath11k_hal_tx_cmd_desc_setup`,
`ath11k_hal_reo_cmd_queue_stats`, `ath11k_hal_reo_cmd_flush_cache`,
`ath11k_hal_reo_cmd_update_rx_queue`,
`ath11k_dp_tx_encap_nwifi`, `ath11k_dp_tx_process_htt_tx_complete`, and
`ath11k_dp_tx_status_parse`. These functions depend on kernel skb, DMA, ring,
and mac80211 infrastructure, so the userspace harness exercises their pinned
field and buffer transformations rather than directly linking the symbols.
