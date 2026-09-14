# Wi-Fi target architecture

This is the agreed destination, not a claim that the current implementation
already has these boundaries. The directory move is mechanical; it does not
remove the existing production dependency on `vfio_read`.

## One Wi-Fi process, separate policy and IP services

**The Wi-Fi protocol engine and hardware driver run in the same process.**
Their interface is a direct typed Rust boundary, not radio IPC. Keep one
runtime/event-loop owner rather than parallel forwarding runtimes. Existing
Fuchsia-shaped APIs and crate boundaries are not constraints on this design;
reuse substantive protocol implementations without preserving unnecessary
transport scaffolding.

```text
Trusted launcher / surviving containment authority
  ├─ Policy service
  │    saved networks, durable credentials, selection, reconnect/backoff
  ├─ Wi-Fi service (one process)
  │    protocol runtime: MLME / SME / RSN, actual connection, controlled port
  │    hardware driver: firmware, channel/peer/key programming, DMA/rings/IRQ
  │    typed hardware API → Linux VFIO backend
  └─ Netstack3
       Ethernet / IP / DHCP / routes → application socket frontend
       separate sandboxed DNS service; no native runtime fallback
```

Policy owns **desired network state**. The protocol runtime owns **actual
connection state**, including peer identity and data authorization. The driver
owns **installed hardware facts** and effect ordering. These represent different
facts, not three copies of a “connected” state.

Durable credentials stay in policy. Wi-Fi receives only the active attempt's
authentication material and session keys. Netstack3 and DNS have no device or
DMA authority. Device assignment, sandboxing and child-specific restart belong
to one lifecycle authority, not connection policy. A surviving containment
owner governs restart after driver death. Kernel VFIO/IOMMUFD final-reference
cleanup already owns DMA isolation and pin lifetime; do not duplicate that
ownership without a demonstrated gap. Process exit alone is not proof that
chip firmware has reset successfully or that immediate restart will work.

### Kernel cleanup owns DMA memory safety

Review of Linux 6.18.43 VFIO/IOMMUFD source, matching the running host version,
contradicts the earlier proposal that surviving Wi-Fi process death requires
supervisor-owned DMA backing. That requirement is withdrawn, including by
advisor adv-6u68. Keep DMA ownership in the driver/backend unless a concrete
kernel guarantee gap or another product requirement justifies moving it.

The cdev final-release path invokes the device's close operation before
IOMMUFD unbinding. VFIO PCI clears bus mastering, disables interrupts and
attempts reset. Physical unbinding detaches the device from its IOAS.
IOMMUFD retains context/page references and unmaps DMA before unpinning;
the unmap path synchronizes IOTLB invalidation. Rust destructors are not the
sole protection, and SIGKILL does not skip kernel file-release cleanup.

These guarantees concern final **references**, not merely one numeric FD:
duplicated/inherited descriptors and retained mappings can extend lifetimes.
The blocked domain used during detach is not permanent; releasing DMA
ownership restores the group's default domain. Device-specific reset is
best-effort, so DMA memory safety and successful firmware recovery/restart
remain distinct. Reboot-time IOMMU transitions are another separate boundary.

Source evidence retained at `/src/kernel-6.18.43-review/`:
`drivers/vfio/vfio_main.c` (last-close/release),
`drivers/vfio/pci/vfio_pci_core.c` (disable),
`drivers/vfio/iommufd.c` (physical unbind),
`drivers/iommu/iommufd/{main.c,device.c,pages.c}` (references/unmap/unpin),
and `drivers/iommu/iommu.c` (detach/default domain/IOTLB sync).
No physical kill/reset experiment was performed for this review.

## Types establish ownership boundaries

Constructors must establish the invariant represented by their return type.
Use a consuming transition from prepared device authority to an initialized
driver, and produce contained authority only after the actual containment
checks succeed. Diagnostic reports and copied status values are not authority.

Likewise, distinguish device generations, connection epochs and operation IDs.
A committed RX occurrence is created only after descriptor repost/publication,
and cannot be reconstructed from public ring/slot integers. A failed transition
retains the authority needed for cleanup or quarantine rather than returning
an apparently usable owner. Keep constructors and raw representations private
where fabrication would bypass the boundary.

These types replace unchecked flags and duplicated state; do not add a type
for every phase or retain redundant wrappers merely to increase type count.

## Small APIs with explicit lifetimes

Application → policy:
- Save/forget a network; set intent (disabled, auto, preferred network).
- Request scan; snapshot/watch desired and actual state.

Policy → Wi-Fi (bounded, validated IPC):
- Capabilities, scan, connect to a candidate, cancel, disconnect.
- Snapshot plus sequenced state/events, with actual connection identity.
- Service epoch, operation ID and absolute deadline identify each operation;
  a connection epoch identifies each association.

Protocol → driver (direct calls/events within Wi-Fi):
- Capabilities; scan/cancel; channel and peer configuration/removal.
- Key installation/removal; transmit and RX/TX-completion delivery.
- Scan completion, firmware link loss and device faults.

Exactly one terminal result belongs to each operation. Cancellation acknowledges
quiescence, not receipt; stale callbacks cannot mutate a later attempt.
Queueing/retries consume the original deadline. Cleanup failure faults the
device session. Busy, unsupported, authentication rejection and device faults
remain distinct. Status/disconnect must not wait behind a blocking connect
handler. Slow observers may resynchronize from a snapshot; safety-critical
completion must not silently disappear.

The deadline migration is partial: WLCP v2 carries checked absolute Linux
CLOCK_MONOTONIC deadlines on commands, rejects expired commands before runtime
admission, and bounds pending reply drain and outbound delivery. Both peers
must share the same monotonic time namespace. The current transport still
creates per-submission budgets; policy selection, persistence and retries
must be changed to consume one original operation budget before this contract
is complete. Active runtime timeout remains terminal, not a reusable timeout
without proved quiescence. No architecture-wide KVM acceptance is claimed yet.

Device, connection and Ethernet attachment lifetimes are distinct. Replace
the finite boot-time inventory of Ethernet generations with reusable
epoch-enforced attachments or supervisor-provisioned fresh endpoints.
Do not restart the entire IP service for every transient link change, or
preserve DHCP/TCP state across arbitrary network changes.

## Remove experimental production ownership

The production MT7921 executable currently imports `vfio_read.rs`, which owns
physical initialization, target preparation and service construction. Remove
that ownership, not just the filename.

Keep the generic hardware API/backends and MT76/MT7921 codecs and verified
sequencing. Extend the existing typed resource owner in
`mt7921-production-client` into the long-lived driver. Raw mappings and IOVA
allocation remain backend-owned; chip register ordering remains driver-owned.

Collapse `MtWifiRuntime` → `Mt7921ProductionClient` (adapter façade) →
`RuntimeOwner` forwarding into one runtime boundary. Preserve exclusive
ownership through consuming construction. Collapse chip effects into the
driver; move final association-frame construction and EAPOL liveness into
the protocol runtime while preserving packet behavior. Retain only necessary
protocol-library bindings. Control transport validation belongs in the service
IPC module, not another conceptual runtime layer.

Replace construction-time fixed-BSS authority with attempt-scoped candidate
authorization. Saved-network selection must not be preempted by a driver
already prepared for one peer.

## Migration order

1. Mechanical directory moves, with unchanged package names, workspace
   boundaries, APIs and behavior.
2. Port firmware/bootstrap-and-contain operations to the typed owning session.
   Preserve descriptor bytes, effect ordering, completion routing, generation
   invalidation and partial-acquisition cleanup through traces/fault injection.
3. Move scan/peer/key/TX/RX operations onto that owner in bounded increments;
   retire the matching binary-local implementations. Use a thin dedicated
   executable, never a wrapper calling or spawning `vfio_read`.
4. Collapse redundant façades into the single Wi-Fi runtime and driver seam.
5. Establish surviving containment before unattended restart; separately
   implement cancellable policy, unlimited reconnect lifetimes, general target
   selection, roaming and suspend/resume.

Separate behavior-preserving structural changes from behavior changes.
Source-string assertions are not a substitute for behavioral ordering tests.
Physical acceptance and automatic recovery remain separate gates; directory
moves and no-radio VM passes do not establish either.

## Repository placement

`crates/platform/` holds shared capabilities/backends; `crates/wifi/` holds
Wi-Fi services and `drivers/{mt76,mt7921,ath11k}/`; `crates/net/`,
`crates/audio/`, `crates/bluetooth/` and `crates/qualcomm/` group their owners.
Top-level `lab/` holds experimental/oracle crates and lab scripts, outside
`crates/`. Keep crate basenames/package names during the mechanical move.
Directories are navigation, not extra crates or processes.

Mixed production/experimental MT7921 and Netstack3 spike crates remain with
their subsystem until their production responsibilities are extracted.
The current composition is described in
[ARCH-wlan-stack-topology](../specs/ARCH-wlan-stack-topology.md); this note
records the intended replacement, including the explicit same-process decision.

## Architecture review continuity

Fresh architecture advisor: **adv-6u68** (mailbox `6u6878qdm4pq`).
Reuse this advisor for a follow-up review after the primary agent refactors
the driver/runtime. Its initial separate-protocol-process recommendation was
explicitly withdrawn: protocol and hardware remain in the same Wi-Fi process.
Review the resulting ownership and removed forwarding layers against this
target, not just renamed files or passing tests.
