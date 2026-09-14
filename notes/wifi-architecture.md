# Wi-Fi target architecture

## Current stopping point: real Wi-Fi Internet inside KVM

The owner has superseded the reduced architecture-only stopping point below.
Completion now requires the production CLI and services to provide working
Internet through the assigned MT7921 inside KVM, not a fake radio, host NAT,
SOCKS-only proof, or the retired one-shot implementation. KVM orchestration,
AP controls, fault injection and proof commands belong in the external test
harness, not special production modes or fixed-target policy overrides.

The owner explicitly includes IPv6 and suspend/resume as blocking daily-laptop
requirements. Enterprise/EAP and hidden-network support are outside the initial
profile; WPA2-Personal/WPA3-SAE remain required. Normal use must not depend on
manual process restarts, finite reconnect counts or fresh boot-only credentials.

Acceptance must distinguish physically exercised cases from deterministic
fault coverage; no finite test suite proves every possible Wi-Fi environment.
The concrete matrix is:

- CLI scan, save/list/forget credentials, select/connect, status/watch, cancel,
  disconnect, disable/enable, and persistence across policy restart.
- Open/WPA2/WPA3 supported modes, wrong credentials, absent network, rejected
  authentication/association, deadline expiry, duplicate/stale events, and
  cancellation while scanning/authenticating/associating. Unsupported security
  and channel modes must fail explicitly, never silently weaken security.
- Real firmware-backed channel/peer/key programming and TX/RX, controlled-port
  enforcement, replay/key-generation isolation and cleanup after failure.
- IPv4 and IPv6 address configuration, DHCP/RA lifetime and DNS renewal,
  DNS/NSS and ordinary application TCP/UDP/verified HTTPS through the
  sandboxed network service and Linux socket provider; no native INET fallback
  or alternate guest Internet interface. Repeated transfers and idle periods.
- AP/link loss, retry/backoff, repeated reconnect beyond the current finite
  Ethernet inventory, service death/restart, resource exhaustion and shutdown.
  Suspend/resume is a blocking requirement: idle and connected suspend, repeated
  resume and automatic reconnect must work without reboot or manual repair.
  Rekey, network switching and ordinary multi-AP roaming need explicit tests
  rather than inference from one successful association.
- Negative/fault cases use deterministic tests when safe physical injection or
  an appropriately controllable AP is unavailable; those rows remain physically
  unverified. Available AP/security/control coverage must be recorded before
  claiming the complete matrix.

Physical handoff requires a verified independent USB management path and an
agreed recovery window. USB link presence alone does not prove reboot-time
recovery or authorize host boot/security changes. Keep the known-good host;
perform no host deployment or reboot as an implicit part of guest testing.


The reduced cutover now implements the same-process typed ownership boundary
below. Passive scanning/RX now run in that owner; association and TX remain pending.
This is not a claim of Internet connectivity or complete recovery qualification.

## Advisor cadence

The owner requires autonomous engineering decisions during implementation.
Do not consult or update an advisor for each fix, small design choice, or test
result. Request final review after a substantial subsystem is implemented,
aiming for roughly one advisor message per hour. Advisors should push back on
unnecessary check-ins. This does not replace user approval for shared or
irreversible actions.

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

## Agreed actor-model implementation direction

The owner selected explicit actor-style ownership without an actor framework:
**synchronous bounded state transitions, asynchronous waiting, exclusive owners**.
The durable boundaries are described in
[ARCH-wlan-stack-topology](../specs/ARCH-wlan-stack-topology.md#async-execution-and-fault-recovery).
Keep protocol and driver actors in the existing Wi-Fi process and Tokio LocalSet;
do not multiply actors per operation or wrap the existing shared driver mutex
in another forwarding layer.

Next implementation order:
1. Retain pending cleanup and terminal replies in the service owner instead of
   blocking control dispatch while awaiting them.
2. Establish exclusive driver ownership with bounded commands and completion
   delivery, reusing the existing bounded MCU begin/poll mechanics.
3. Port firmware-backed scan through the real CLI/service path, then association
   and bidirectional Ethernet, Netstack Internet, security and recovery coverage.
   The pinned Linux kernel driver is the hardware-behavior source of truth.
   The old working MT7921 implementation supplies regression evidence and paths
   to investigate, not authoritative command bytes, sequencing or semantics.
   Validate those against Linux; do not restore the retired resource owner or
   one-shot production path.

Use an advisor at the final implementation review to check architectural fit:
exclusive ownership rather than extra wrappers; responsive control during held
hardware completion; bounded queues; owner-held pending work and replies;
stale-authority rejection before publication; DMA retained after waiter loss;
exactly one correctly ordered terminal reply; and actual radio progress.
Report remaining deviations explicitly. Actor organization does not supersede
the real KVM Internet acceptance matrix or authorize physical handoff.

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

WLCP v2 carries checked absolute Linux CLOCK_MONOTONIC deadlines on commands,
rejects expired commands before runtime admission, and bounds pending reply
drain and outbound delivery. Both peers must share the same monotonic time
namespace. Application admission starts the policy budget before queueing and
persistence; selection scans, augmentation and connection retries receive that
same deadline explicitly. Repeated cancellation does not renew its original
cleanup deadline. Active runtime timeout remains terminal, not a reusable
timeout without proved quiescence. Autonomous SME recovery without a prior
policy budget is cancelled; policy-authorized subsequent recovery and roaming
still need complete operation/intent identity integration.

Device, connection and Ethernet attachment lifetimes are distinct. Replace
the finite boot-time inventory of Ethernet generations with reusable
epoch-enforced attachments or supervisor-provisioned fresh endpoints.
Do not restart the entire IP service for every transient link change, or
preserve DHCP/TCP state across arbitrary network changes.

## Remove experimental production ownership

The production MT7921 executable no longer imports `vfio_read.rs`; that
binary-local owner and its executable target are deleted. The replacement
consumes `Mt7921Driver` directly into the shared MLME/SME runtime.

The thin service accepts `--run-wifi-service` and a hardware-free `--describe`.
Its trusted launcher supplies the policy/supervisor FDs and generation, PCI BDF
and VFIO cdev, the firmware's MAC identity, and uncompressed firmware paths in
`DRV_MT7921_PATCH_IMAGE` / `DRV_MT7921_RAM_IMAGE`. Exact build-pinned firmware
lengths and hashes are checked before lockdown. Legacy artifact flavors and
one-shot lab modes are removed; old lab launch scripts are not a supported
entrypoint for this cutover. The existing external-watchdog activation gate
remains while physical recovery qualification is outstanding.

The `mt7921-wifi-service` package exposes the raw service binary; its lifecycle
launcher supplies `--run-wifi-service` exactly once and the required firmware
paths and setup capabilities. `--describe` remains available without hardware. The old
validation/flavor packages, launchers and artifact checks are no longer flake
outputs; their historical lab scripts are not a deployment interface.

A Linux 7.3 KVM guest with CONFIG_INET disabled and **no VFIO/passthrough**
passed 115 Rust tests plus the separate-process policy fixture: typed driver
fault/containment tests, sandbox FD probes, runtime, wire-policy selection and
deadline paths, release filtered control owner, and the replacement entrypoint.
The entrypoint check is hardware-free `--describe`, not physical initialization.
Evidence on np is under
`/var/lib/poco-linux/redwood/work/crate-layout-primary/kvm-typed-cutover/output-2/`;
serial SHA-256:
`caffda797233333c0e6fe807f6cee3b1731d5d0eb46a6d5e65d0d7edf0aa93e7`.
The empty-capability prepared runtime constructs and shuts down; a scan fails
without driver scan calls. Pinned MLME/SME currently translates this empty-channel
failure to `InternalError`, not `NotSupported`; clearer reporting is future work.
Offline Nix parse and package derivation evaluation pass, but no Nix package
build was run. Advisor adv-6u68 approved the reduced ownership design and final
packaging/test changes. Physical firmware initialization, radio traffic and
recovery remain unverified.

The owner explicitly accepts removing old functionality to complete this
cutover. Do not retain a compatibility owner or make full feature parity a
prerequisite. Unsupported operations must fail explicitly, without falling
back to the lab implementation. Scheduled recovery, comprehensive roaming
budgets and reusable Ethernet attachments may remain documented future work.
Acceptance for this cutover is the direct typed owner in the same-process
protocol runtime, the old production owner removed, KVM verification, and
the same advisor's final gap review.

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
3. Replace the production entrypoint with the typed driver and retire the
   binary-local owner. Port only coherent operations now; mark remaining
   scan/peer/key/TX/RX functionality unavailable rather than retaining a second
   owner. Never call or spawn `vfio_read` as a compatibility path.
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
