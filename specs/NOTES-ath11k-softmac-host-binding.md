# NOTES-ath11k-softmac-host-binding: Ath11k binding to the chip-neutral SoftMAC host

## Status

Ath11k is the second hardware backend. `Ath11kClientDevice` binds the
chip-neutral traits to either deterministic `ModelSubsystems` or real
`Wcn6750Subsystems`; its deterministic lifecycle, query, 20 MHz channel, and
passive-scan path passes the shared host conformance runner. The current
`ath11k-bringup` stages remain a diagnostic harness, while `ath11k-core`
implements only part of the hardware effects and completion waits described
below.

The binding follows [ARCH-wlan-stack-topology](ARCH-wlan-stack-topology.md) and
keeps the WCN6750-specific resources described by
[ARCH-redwood-wifi-target](ARCH-redwood-wifi-target.md) below the portable
SoftMAC boundary.

## Ownership and execution

The backend implements `WlanSoftmac`, `WlanSoftmacLifecycle`, and
`ClientRuntimeDriver`. Pinned Fuchsia `DeviceOps` remains private host
bridging and is not an ath11k implementation surface.

`WlanSoftmacLifecycle::start` absorbs the diagnostic runner's resources,
firmware-asset, QMI, and core stages into one transactional startup. It acquires
VFIO/QRTR, completes QMI and CE/HTC/WMI/HTT initialization, allocates DP rings,
creates the station vdev, and installs `WlanSoftmacUpcalls` before the runtime
begins polling. `stop` revokes callbacks first and then performs the existing
core unwind. The runner's passive-scan stage becomes
`WlanSoftmac::start_passive_scan`; its scan-results loop becomes ordinary
`ClientRuntimeDriver::poll` work, not another production lifecycle phase.

The host schedules bounded `poll` calls. Each call services CE/WMI and the
polling-first DP rings, then delivers at most the deterministic receive slot
allowed by the host contract. Unsolicited WMI management RX, scan completion,
and management TX completion invoke `recv`, `notify_scan_complete`, and
`report_tx_result` respectively. DP RX also invokes `recv` with a raw 802.11
frame and `WlanRxInfo`; MLME performs data decapsulation and writes the
post-controlled-port Ethernet frame to `DriverEthernetPort`. In the other
direction the host encapsulates Ethernet before calling `queue_tx`. There is
no backend-to-host Ethernet callback.

## Downcall and WMI mapping

| Host operation | Ath11k action and completion |
|---|---|
| `start` | QMI mission-mode startup; CE/HTC service connection; WMI `INIT`; HTT/DP setup; WMI `VDEV_CREATE` for the station vdev. |
| `set_channel` | WMI `VDEV_START` (or `VDEV_RESTART` when already started) with the requested channel, followed by matching `VDEV_START_RESP`. |
| `join_bss` | Retain the selected BSSID and create it with WMI `PEER_CREATE`; wait for the peer-created indication before authentication frames are queued. |
| `queue_tx` management frame | WMI `MGMT_TX`; correlate its buffer ID with `MGMT_TX_COMPLETION`, return acceptance synchronously, and later report air completion through `report_tx_result`. |
| `queue_tx` data frame | Publish through `ClientDataPath` TCL; WBM completions become `report_tx_result`. Frames cannot enter this route until the host-controlled port and backend link are both open. |
| `notify_association_complete` | Translate the BSSID, AID, rates, capability bits, HT/VHT capabilities and channel width into WMI `PEER_ASSOC`; wait for `PEER_ASSOC_CONF`; apply peer SMPS; then WMI `VDEV_UP` and the best-effort OBSS/DTIM parameters. |
| `install_key` | Translate cipher, key kind/index, peer and key bytes into WMI `VDEV_INSTALL_KEY`; return only after matching `VDEV_INSTALL_KEY_COMPLETE`. Credentials and handshake state remain host-owned. |
| controlled-port/link downcall | WMI peer-authorize for the current peer before opening backend data TX/RX admission. Close admission first during clear/reset. |
| `clear_association` | Close admission, WMI `VDEV_DOWN`, `PEER_DELETE` plus completion, and clear selected-peer state. Stop/delete the vdev only during reconfiguration or lifecycle teardown. |
| passive/active scan | WMI `START_SCAN` with device-owned scan ID; `SCAN_EVENT` completes or fails it. `STOP_SCAN` implements cancellation. |
| `update_wmm_parameters` | Program ath11k WMI WMM parameters once the corresponding typed encoder is exposed; do not silently accept the call. |

The WMI event pump must preserve firmware order across events buffered during
service-ready waits. `MGMT_RX` supplies beacon/probe, authentication,
association, and other management frames to `recv`; `SCAN_EVENT` changes
scan state and invokes `notify_scan_complete`. All command-completion events
remain adapter-internal so the public downcalls keep their synchronous
complete-before-return contract.

## Contract gaps to close

Ath11k currently needs these additions before it can implement the traits
without invented values:

- The real subsystem must implement the existing vdev start/up, peer
  create/associate/authorize, key, management-TX, stop/delete, and correlated
  wait operations. The typed WMI encoders exist for most of them, but the real
  dispatcher currently rejects the operations.
- `associate_peer(vdev, address)` is too small. The adapter must translate the
  host's `WlanAssociationConfig` into the full `PeerAssocParams`. The pinned
  host config carries legacy rates and HT/VHT data; HE/EHT and any required
  rate-set detail need a later chip-neutral association-data extension rather
  than ath11k defaults.
- `WlanEvent::ManagementReceived` currently drops WMI PHY mode, rate, status,
  and timing fields needed to construct accurate `WlanRxInfo`. Preserve the
  required metadata through the adapter. Conversely, ath11k's pdev ID, WMI
  flags, and firmware-only scan reasons do not belong in the chip-neutral
  trait unless the host demonstrates a policy use.
- Repeated `set_channel` requires explicit start-versus-restart state and
  completion correlation. Scan IDs, management buffer IDs, peer completions,
  and key completions likewise need adapter-owned correlation tables.
- The controlled-port/link downcall being added with `ClientRuntimeDriver`
  must not report link-up until WMI peer authorization succeeds. Ath11k's
  separate authorize operation therefore maps to that downcall, not to
  `notify_association_complete` or `install_key`.
- Query methods need a stable projection of service-ready capabilities,
  supported bands/channels, security offload support, and the station MAC.
  Unsupported active scan or WMM programming must return an explicit status
  until implemented.

Ath11k provides richer firmware event and peer capability data than the current
host traits expose; keep that detail adapter-local unless a second backend
demonstrates a chip-neutral need. The host provides policy decisions, BSS
descriptions, credentials, and handshake products that ath11k must consume but
must never synthesize.
