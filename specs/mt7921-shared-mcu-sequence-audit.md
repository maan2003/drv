# MT7921 shared MCU sequence audit

This audit covers every production command reachable after firmware bootstrap becomes ready and before universal cleanup completes. The sole wire authority is `LoaderMechanics`; every client sender replaces byte 39 from that cursor at the physical publication edge and wraps 15 → 1.

| Lifecycle area | Commands / encoders | Physical sender | Completion |
|---|---|---|---|
| receive preparation | EEPROM buffer mode, protect control, MAC enable | `send_passive_command` | response policy from command |
| PHY/regulatory | RX path, channel switch, channel domain | passive sender after bootstrap; bootstrap domain uses the same loader cursor | ACK or DMA consumption |
| power | eight SET_RATE_TX_POWER pages | `send_rate_power_bytes` | no-ACK DMA consumption |
| interface | DEV_INFO, BSS_INFO, early EDCA | acknowledged UNI / CE no-ACK | ACK / DMA consumption |
| receive and scan | RX filter, scan start, scan cancel | `send_passive_command` | command policy |
| join lease | ROC acquire, ROC abort | unacknowledged UNI | DMA consumption plus ROC grant for acquire |
| association | BSS update, STA_REC, WTBL, association EDCA | acknowledged UNI / CE no-ACK | ACK / DMA consumption |
| post-association | beacon offload, RX filter, RLM | acknowledged or unacknowledged UNI / CE no-ACK | command policy |
| teardown | key/WTBL/STA removal, BSS disable, DEV disable, ROC abort | the same three client senders | attempt-all cleanup |

Management frames and EAPOL frames are data-ring publications, not MCU commands. Patch-table diagnostics have an isolated diagnostic cursor and are not reachable in the production full-firmware operation. Firmware download, patch semaphore, CLC, and initial channel-domain commands precede firmware-ready; they allocate from the same `LoaderMechanics` cursor via `FirmwareLoaderTransport::next_sequence`.

## Authority and failure boundary

Encoders receive a non-authoritative template sequence because their payload goldens include a complete Connac2 header. `candidate_and_stamp` overwrites that byte immediately before physical submission, so a literal or stale encoder value cannot reach firmware. `SourceExactPassiveTransport.mcu_sequence` is only a mirror used to form templates and is resynchronized from `current_mcu_sequence` after success. ROC APIs no longer accept sequence values, eliminating the second lifecycle allocator.

Envelope, size, ring ownership, cancellation, and required MMIO checks happen before cursor consumption. At the producer publication boundary the cursor advances. A failure after that boundary is publication-uncertain and cannot reuse the number or DMA slot. ACK matching remains keyed to the stamped value; unsolicited or late responses cannot satisfy a later command. No-wait commands consume on publication-uncertain or published outcomes, while local validation failures retain the cursor.

## Enforcement and regression matrix

- Production source lint requires all five client publication paths (CE no-ACK, UNI no-ACK, UNI ACK, passive, and rate-power) to call the physical stamper and forbids mechanics-side loader assignment or ROC reservation.
- The lifecycle model starts at 0, 7, and 14, injects local and uncertain failure at every command, verifies relative order and wrap, and verifies all teardown commands are attempted.
- Existing byte/header goldens remain exact; sequence assertions are relative to the prior cursor rather than phase constants.
- Physical build and packaged integration cover the feature-complete source composition. That evidence does not itself grant a hardware window: existing authorization, exact-revision review, and containment requirements still govern physical runs, with review scaled to the run's risk.
