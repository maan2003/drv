# Deferred AP WMI command port map

Pinned source: Linux `509ce3d952d550f93b544c8d94c99e798f09a9b4`,
`drivers/net/wireless/ath/ath11k/wmi.[ch]`.

| C symbol | Rust request / encoder | Status |
|---|---|---|
| `ath11k_wmi_send_bcn_offload_control_cmd` | `BeaconOffloadControl` | ported |
| `ath11k_wmi_p2p_go_bcn_ie` | `P2pGoBeaconIe` | ported |
| `ath11k_wmi_bcn_tmpl` | `BeaconTemplate` | ported |
| `ath11k_wmi_send_wmm_update_cmd_tlv` | `WmmUpdate` | ported |
| `ath11k_wmi_delba_send` | `DelbaSend` | ported |
| `ath11k_wmi_addba_set_resp` | `AddbaSetResponse` | ported |
| `ath11k_wmi_addba_send` | `AddbaSend` | ported |
| `ath11k_wmi_addba_clear_resp` | `AddbaClearResponse` | ported |
| `ath11k_wmi_fils_discovery_tmpl` | `FilsDiscoveryTemplate` | ported |
| `ath11k_wmi_fils_discovery` | `FilsDiscovery` | ported |
| `ath11k_wmi_peer_set_cfr_capture_conf` | `PeerCfrCapture` | ported |
| `ath11k_wmi_probe_resp_tmpl` | `ProbeResponseTemplate` | ported |

Source-derived fixtures cover aligned byte-array declaration, full beacon
template offsets, inline WMM TLVs, and malformed P2P IE handling. Native golden
verification awaits the tracing-kexec WMI transcript.
