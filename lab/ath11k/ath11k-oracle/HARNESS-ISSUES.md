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

The install-key, peer-association, and management-send oracles execute exact
build-time extractions of their pinned `ath11k/wmi.c` builder bodies. Those
paths also execute the extracted WMI allocation/zeroing helper and, for
management send, the extracted CE byte-swap helper; only the allocator and
command-send boundaries are stubbed. The remaining WMI command oracles are
wire-level transcriptions. In particular, the `VdevStart` generator supplies
arbitrary already-normalized channel words that do not necessarily have a
semantic input accepted by the pinned channel helper. Those remaining
differentials prove the transcribed wire contract, but do not independently
exercise that helper's channel normalization.

## Newly typed event provenance

Status: Open.

The pinned `ath11k_wmi_tlv_op_rx` has no cases for event IDs `0x16005`,
`0xb00b`, `0x1d00a`, or `0x601a`. Their C-side semantic checks are
FW-table-derived transcriptions layered on the pinned generic TLV iterator,
rather than calls through the pinned Linux event dispatcher.

## Connection-event pull provenance

Status: Partially fixed.

Generated valid service-ready-ext, peer-association-confirmation, vdev-start
response, and management-RX messages execute build-time extractions of the
corresponding pinned `ath11k/wmi.c` pull bodies. Peer association, vdev start,
and management RX also execute the extracted pinned `ath11k_wmi_tlv_iter`
body. The harness supplies only the three relevant minimum-length policy
entries plus the service-ready-ext fixed-length check, and replaces
`ath11k_wmi_tlv_parse_alloc`'s allocation/table callback,
kernel skb mutation helpers, logging, and the little-endian CE byte-swap
boundary. It does not invoke the full WMI event dispatcher or its device-state
effects. The service-ready-ext pull body consumes only the fixed event struct;
the additional array-group parsing remains covered by the existing
wire-level oracle.

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
`hw_srng_config_template`,
`ath11k_hal_tx_init_data_ring`,
`ath11k_hal_tx_set_dscp_tid_map`,
`ath11k_hal_reo_cmd_queue_stats`, `ath11k_hal_reo_cmd_flush_cache`,
`ath11k_hal_reo_cmd_update_rx_queue`, `ath11k_hal_reo_process_status`,
`ath11k_hal_reo_qdesc_setup`, `ath11k_hal_reo_init_cmd_ring`,
`ath11k_hw_wcn6855_reo_setup`,
`ath11k_hal_srng_setup`, `ath11k_hal_srng_src_hw_init`,
`ath11k_hal_srng_dst_hw_init`,
`ath11k_hal_rx_msdu_link_info_get`,
`ath11k_hal_set_link_desc_addr`, `ath11k_hal_rx_msdu_link_desc_set`,
`ath11k_dp_tx`'s descriptor field selection, `ath11k_dp_tx_encap_nwifi`,
`ath11k_dp_tx_htt_h2t_ppdu_stats_req`, `ath11k_dp_tx_htt_h2t_ext_stats_req`,
`ath11k_dp_tx_process_htt_tx_complete`, and
`ath11k_dp_tx_status_parse`. These functions depend on kernel skb, DMA, ring,
and mac80211 infrastructure, so the userspace harness exercises their pinned
field and buffer transformations rather than directly linking the symbols.
