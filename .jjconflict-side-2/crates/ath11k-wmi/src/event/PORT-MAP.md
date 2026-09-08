# ath11k WMI event port map

Pinned source: Linux `509ce3d952d550f93b544c8d94c99e798f09a9b4`,
`drivers/net/wireless/ath/ath11k/wmi.[ch]`.

| C symbol / dispatch case | Rust item | Status |
|---|---|---|
| `ath11k_wmi_tlv_iter` | `event::TlvIter` | ported |
| `ath11k_wmi_tlv_iter_parse`, `ath11k_wmi_tlv_parse[_alloc]` | `event::find_tlv` | ported |
| service-ready, service-ready-ext, service-ready-ext2 handlers | `ServiceReady{,Ext,Ext2}Decoder`, `EventStream::wait_for_service_ready` | ported; nested ext groups retained positionally |
| ready handler | `Ready`, `EventStream::wait_for_unified_ready` | ported |
| regulatory legacy/ext handlers | `RegulatoryChannelListLegacy`, `RegulatoryChannelListExtended` | ported |
| peer delete/assoc/kickout handlers | `PeerDeleteResponse`, `PeerAssocConfirmation`, `PeerStaKickout` | ported |
| vdev start/stop/delete handlers | `VdevStartResponse`, `VdevStopped`, `VdevDeleteResponse` | ported |
| install-key completion handler | `InstallKeyCompletion` | ported |
| beacon/probe/FILS TX handlers | `BeaconTxStatus`, `ProbeResponseTxStatus`, `FilsDiscovery` | ported |
| management RX/TX completion handlers | `MgmtRx`, `MgmtTxCompletion` | ported |
| scan, roam, channel-info, BSS-channel-info handlers | `Scan`, `Roam`, `ChannelInfo`, `PdevBssChannelInfo` | ported |
| service-available handler | `ServiceAvailable` | ported |
| update-stats handler | `UpdateStats` | ported for the pdev/vdev/beacon prefixes Linux consumes |
| CTL failsafe, CSA, temperature, DFS handlers | `PdevCtlFailsafeCheck`, `PdevCsaSwitchCount`, `PdevTemperature`, `PdevDfsRadar` | ported |
| DMA-ring release handler | `DmaRingBufferRelease` | ported |
| OBSS, TWT, 11d, peer-PS handlers | `ObssColorCollision`, `TwtAddDialog`, `NewCountry`, `PeerStaPowerSaveStateChange` | ported |
| WOW, GTK, P2P NoA handlers | `WowWakeupHost`, `GtkOffloadStatus`, `P2pNoa` | ported |
| peer CFR handler | `PeerCfrCaptureDecoder` | ported |
| DIAG and PDEV UTF opaque forwarding | `OpaqueEventDecoder` | ported |

All fixed decoders use the generic `Decoder<T>` `EventDecoder`
implementation unless a source callback accepts optional tags or bypasses TLV
parsing, in which case the table names its dedicated decoder. Golden trace
verification is pending the tracing-kexec capture; source-derived fixtures and
malformed-input tests are present.
