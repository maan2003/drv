# ARCH-wlan-stack-topology: Userspace Wi-Fi stack process topology

## Status

MT7921 now uses one typed hardware/DMA owner consumed by the same-process
Fuchsia MLME/SME runtime. The binary-local `vfio_read` owner and its executable
are removed, not retained as a compatibility path. Firmware initialization,
bounded passive-scan/RX progression and containment are implemented and have
been exercised with the assigned MT7921 inside KVM through the production CLI.
Regulatory input is a hash-verified inherited database; the chip owner retains
its immutable world-domain policy. Passive scan does not grant transmit
authority. Active scan, scan cancellation, channel/peer/key configuration,
association, TX and MAC override remain unavailable. Earlier Internet
demonstrations used the retired implementation and are not acceptance of this
replacement.

The policy daemon owns persistence and network intent, and drives the pinned
Fuchsia selector/state machine over bounded, fd-free control IPC. Application
queueing, selection scans and retries consume one policy-issued deadline.
The Wi-Fi process retains only device authority and precreated control,
Ethernet and runtime-reactor descriptors after lockdown. Netstack3 remains a
separate process with no device/DMA capabilities, connected through the bounded
Ethernet lifecycle seam. No-VFIO verification does not qualify physical radio
recovery or restart.

The WCN6750
production service now adopts
inert VFIO-platform, iommufd-or-broker, QRTR, interrupt, runtime-reactor,
Ethernet, policy, network-lifecycle, and remoteproc capabilities before
installing its fatal role-specific seccomp filter. Only then does it activate
QMI, firmware, the ath11k adapter, and pinned Fuchsia MLME/SME; Ethernet
generations flow to the existing network supervisor lifecycle seam. This
composition and its exact ARM64 artifact are build-tested but have not yet
received physical association or Internet acceptance on Redwood. Its
Redwood client configuration enables SME-managed SAE, and the ath11k adapter
owns the software BIP/IGTK boundary that pinned Linux likewise keeps above the
firmware key installer. Its
same-process WPSS-first cleanup covers ordinary returned errors, not
uncatchable parent death or fatal-filter termination; production still needs a
surviving external containment owner for that case, and diagnostic association
does not wait on it. Explicit
SoftMAC roam is reported unsupported without disturbing the current link;
the pinned SoftMAC MLME does not implement the fullmac roam request.

MT7921 radio recovery, firmware beacon-loss delivery, connection-monitor
offload, and retry-safe per-attempt cleanup remain future work; the replacement
does not admit those operations.

This document refines [ARCH-network-service](ARCH-network-service.md),
[ARCH-hardware-isolation](ARCH-hardware-isolation.md), and [ARCH-drv](ARCH-drv.md)
for the Wi-Fi data path, and is constrained by [REQ-isolation](REQ-isolation.md),
[REQ-host-portability](REQ-host-portability.md), and
[REQ-application-compatibility](REQ-application-compatibility.md).

## Process topology

The immediate production boundary is three capability-scoped services: Wi-Fi,
wlancfg policy, and networking. Separate DNS is a later refinement. Persistent
credential storage stays out of the Wi-Fi process, and device/DMA authority
stays out of Internet parsers. The Wi-Fi process necessarily receives active
connection authentication material and session keys; it is not secret-free.

| Process | Owns | fs | secrets | hardware | Internet parser | lifecycle |
|---|---|---|---|---|---|---|
| driver + MLME + SME + RSN | VFIO/DMA, 802.11 control, SAE, 4-way handshake, keys | no | active connection only | yes | no | ephemeral |
| netstack | Ethernet/ARP/NDP/IP/ICMP/UDP/TCP/routing | no | no | no | yes | ephemeral |
| dns (later separate process) | name resolution (DoH/DoT) | no | no | no | yes (narrow) | ephemeral |
| policy (wlancfg) | saved networks, config, credentials, connection policy | yes | yes | no | no | stateful |

The netstack remains one service and is not split by DNS domain, remote address,
or connection, per [ARCH-network-service](ARCH-network-service.md). Per-app
network policy is deferred; the design does not foreclose it.

## Seams

The internal structure copies Fuchsia's WLAN component contracts (`WlanSoftmac`,
`fuchsia.wlan.mlme/MLME`). The FIDL/Zircon transport and the C++/Rust bridge
scaffolding (`WlanSoftmacBridge`, raw-pointer FFI frame protocols) exist only to
work around a Fuchsia toolchain gap and are dropped; the method *semantics* are
kept. The seam is a project-owned portable contract per
[REQ-host-portability](REQ-host-portability.md). The SoftMAC seam uses
in-process Rust traits and callbacks with generated FIDL schema value types.
Mutations admit work synchronously and return owned, executor-neutral completion
futures; the host releases the driver lock before awaiting them. Unix `SOCK_SEQPACKET` carries the separate Ethernet process seam,
not these SoftMAC control/raw-frame calls.

- **Control + raw-frame seam (WlanSoftmac).** driver <-> MLME. Methods to copy:
  `Query`/`Query*Support`, `SetChannel`, `JoinBss`, `InstallKey`,
  `NotifyAssociationComplete`, `ClearAssociation`, `StartPassiveScan`/
  `StartActiveScan`/`CancelScan`, `UpdateWmmParameters`, `Start`/`Stop`,
  `QueueTx`; and up: `Recv`, `ReportTxResult`, `NotifyScanComplete`. AP-only
  beaconing methods are out of scope for client bring-up.
- **Connection seam (MLME).** MLME <-> SME. `ConnectReq`/`ConnectConf`, scan
  results, `AssociateInd`/`Resp`, `Deauthenticate*`, `SetKeysReq`/`Conf`,
  `EapolReq`/`EapolConf`/`EapolInd` (EAPOL punt to SME's RSN handshake),
  `SetControlledPort`, `SignalReport`, `QueryDeviceInfo`.
- **Data-plane seam (Ethernet).** driver <-> netstack. Post-controlled-port
  Ethernet frames only; no control verbs. This is the process boundary (below).

## Three gates

The monolith conflated these; they are kept distinct, and each maps to a
bring-up bug already fixed:

1. `InstallKey` (SME -> MLME -> driver): PTK/GTK/IGTK into the WTBL.
2. `SetControlledPort` (SME -> MLME): SME opens the 802.1X port after the 4-way
   handshake completes.
3. `SetEthernetStatus` (MLME -> driver): admits data-plane traffic to netstack.

## Where the process boundary is cut, and why

Fuchsia keeps MLME in the driver-host process for frame-path latency and cuts at
MLME<->SME. Our threat model values a DMA-free Internet parser over frame
latency, so the load-bearing cut is at the **Ethernet data-plane seam**: the
netstack — the largest untrusted parser — runs in its own process with no VFIO,
no filesystem, and no host network, connected to the driver only by a dumb,
`SetEthernetStatus`-gated frame pipe. MLME/SME/RSN stay with the driver because
they are chatty, latency-sensitive, and parse local-air 802.11. This grouping
retains Fuchsia's protocol logic, not its exact process layout. A compromised
netstack may reveal ciphertext and metadata for encrypted flows, plaintext for
unencrypted flows, or affect availability, but must not reach
application memory, device resources, DMA, or the host kernel, per
[REQ-isolation](REQ-isolation.md) and [ARCH-network-service](ARCH-network-service.md).

## Application handoff

All Internet-facing applications and desktop/system components are owned and
modifiable. Applications currently reach Netstack3 through **SOCKS**. The
owner-selected production handoff is the userspace-backed Linux Internet
socket frontend in [ARCH-network-service](ARCH-network-service.md), including
localhost and compile-time removal of native TCP/UDP implementations. The
existing socket-provider spike is not that finished implementation.
[REQ-application-compatibility](REQ-application-compatibility.md) retains scope
for adapting applications beyond the socket interface. System UI
controls Wi-Fi through wlancfg's project-native interface, not a required
NetworkManager, iwd, or nl80211 interface.

## Policy and device lifecycle

A separate wlancfg-like process owns saved networks, persistent credentials,
network selection, and reconnect/roaming policy. It requests connections and
observes status through a narrow interface, supplying only the selected
connection's authentication material. Fuchsia SME/RSN and MLME retain their
protocol state machines; wlancfg does not duplicate authentication or association
logic. Reuse the Fuchsia selection components and rewrite host bindings.

The Wi-Fi service retains driver, MLME, SME, and RSN together. A narrow
supervisor/host binding owns privileged device assignment and recovery, not
network-selection policy. The Wi-Fi process does not launch the network service
or receive general filesystem, host-network, or unrelated device access.

## Async execution and fault recovery

The selected Linux execution model is one Tokio current-thread runtime with a
LocalSet and readiness-driven descriptor integration. Portable driver contracts
remain executor-neutral. Entrypoints own that executor; protocol state does not own a nested runtime.
The selected ownership model is a small number of explicit actors, not an
actor framework: protocol ownership (SME/MLME/RSN) and exclusive driver ownership
(hardware state, DMA, IRQ and pending operations), within the same Wi-Fi process.
Typed bounded command/completion routes connect these owners; they are not
additional process IPC. Do not create an actor per mutation or add mailbox
wrappers around otherwise unchanged shared mutable driver access.

State transitions and hardware mechanics are synchronous and bounded.
Waiting for readiness, completion or deadlines is asynchronous. Long-running
operations and their terminal replies belong to the owner, not to transient
request-handler futures. Control dispatch remains available while cleanup or
hardware work is pending. Queue admission and completed effects are distinct;
mailbox ordering alone is not cancellation or hardware-completion evidence.

MLME has an owning task and the generic driver actor exclusively owns the
device behind a bounded typed mailbox; the protocol bridge has no mutable
device reference. Actor progression remains cooperatively driven by runtime
turns. Inline admission cleanup and manual progress polling remain transitional.
Explicit disconnect/cancel cleanup and its replies are retained and advanced
in bounded service turns; driver cleanup certification can still block in
legacy adapters.
These are transitional implementation details, not the selected actor model.

SoftMAC mutations return owned futures without borrowing the driver. Existing
legacy adapters still perform synchronous work before returning ready futures;
that behavior and manual driver-turn polling remain transitional, not the
completed async lifecycle.

Scan-start commands carry an immutable absolute deadline and independently
revocable scan authority beneath the connection lifetime. Protocol continuations
retain that same authority across scan segments. The mailbox validates admission
and dispatch; drivers must also validate before each deferred publication.
Legacy adapters currently check only at entry, so their internal synchronous
scan sequences are not evidence of deferred-publication safety.

The driver owns submitted operations and their hardware-visible resources
independently of waiting futures. Dropping a waiter or reaching its deadline
does not authorize DMA reclamation. Resources remain owned until terminal
hardware completion or verified containment.

Normal shutdown attempts bounded asynchronous drain and containment. Failure to
quiesce makes the Wi-Fi service unhealthy; an independent supervisor terminates
and restarts it rather than relying on its stalled executor. Replacement device
acquisition waits for old ownership and teardown, including any duplicated VFIO
references. Kernel final-reference cleanup is the process-death safety boundary.
Reset or reinitialization failure leaves the service unavailable with backoff;
it does not authorize reuse of uncertain hardware state. Wi-Fi recovery does
not inherently restart the network service.

## Crate ownership

WLAN protocol code and device-specific driver code remain in separate crates,
even when their actors execute in the same process. Generic mailbox scheduling
may live with the host bindings; chip register, firmware, DMA-descriptor and
radio-operation implementations belong to the chip crates, not the protocol
crate.

Crates enforce dependency boundaries; they do not themselves provide process
isolation. `mt76-core` / `mt7921-core`, the chip SoftMAC adapters, and
`wlan-softmac-host` retain hardware protocol, chip effects, and shared WLAN
runtime responsibilities respectively. `userspace-vfio` and the typed hardware
API/backends retain generic resource mechanics without device protocol policy.

The MT7921 service directly instantiates `mt7921-production-client`'s typed
driver and consumes it into `wlan-softmac-host`'s protocol runtime. Unported
operations fail explicitly rather than using the retired binary-local owner.
Lab tooling is not an alternate production implementation.
Netstack3 binding, SOCKS, and network-service startup live in
`drv-network-service`, connected to `wlan-softmac-host` by the small Ethernet
contract rather than a dependency from Wi-Fi runtime to Internet parsing.
wlancfg owns the policy service. Large implementations can use modules without creating a crate
per mechanism. Oracles and test-only implementations stay outside production
dependencies. Shared sandbox code may implement host mechanics, but each
service declares its own authority rather than inheriting a broad default.

## Fuchsia reuse

Lift the platform-agnostic cores and rewrite only the bindings, which is where
our bugs concentrate:

- Netstack3 (core, not the Fuchsia bindings) for L3+.
- `wlan-mlme` / `wlan-sme` / `wlan-rsn` for MLME/SME/handshake (already
  implement the Connection seam).
- The DNS component *shape* (`//src/connectivity/network/dns`: separate process,
  `fuchsia.net.name` Lookup/LookupAdmin split, hickory/trust-dns backend). It
  ships plain UDP/TCP only; we add DoH/DoT via hickory+rustls ourselves.
- The policy layer role from `wlancfg`; the nl80211-style control surface from
  `wlanix` is available later if standard Linux Wi-Fi tooling must drive the
  stack.
- Dropped: the Fuchsia bridge scaffolding, FIDL transport/endpoints, Zircon
  channels, `fuchsia.io` namespaces, and `component_manager`. Generated FIDL
  schema value types remain in the host bindings.

## Testing

The shared WlanSoftmac conformance runner checks normalized lifecycle,
identity/support queries, channel selection, passive scan progress/completion,
and stop behavior. It does not inspect MCU commands, WTBL/TXWI bytes, or DMA
descriptors. Chip-specific command/descriptor oracles and randomized tests
provide separate evidence; native transcripts are compared for the phases and
fields actually captured. Do not infer complete production-path coverage from
one normalized seam test. A virtual AP remains future work; deterministic
associated Ethernet/network tests already exist, as scoped by
[REQ-hardware-independent-testing](REQ-hardware-independent-testing.md).

## Sandboxing and supervision

Each service is **trusted at startup and sandboxes itself**. The launcher only
starts the binary with transient privilege enough to build its own jail and hands
it its initial capability fds; it never imposes the sandbox. Each service runs
`setup()` (enter namespaces, mount an empty tmpfs, open/bind exactly its fds,
drop all privilege) then `lockdown()` (install the steal-nothing seccomp
allowlist) then `run()`. The hard invariant: **no untrusted byte is processed
before `lockdown()` completes** — no `accept()`, no wire read, no untrusted
config parse. This is the crosvm model; it makes the running allowlist far
smaller than a launch-time jail because setup-only syscalls never appear in it.

`lockdown()` is the per-kernel portability seam (Linux `unshare`+`seccomp`+cap
drop; FreeBSD `cap_enter`+`cap_rights_limit`; a capability microkernel needs
little). No ambient authority: a service starts from an empty namespace and holds
only the capabilities (fds) explicitly passed to it.

systemd is the interim launcher; confinement is established by each service
rather than delegated to unit settings. It is
replaceable by a small project-owned supervisor doing the same launch + fd-pass +
restart, because the scope is a fixed handful of services, not
`component_manager`'s dynamic generality.

## Persistence and restart

Restart is natural and designed for. Only the policy service stores saved
networks and long-lived credentials; the driver, netstack, and dns processes
are ephemeral and reconstructible. On (re)start, configuration flows down from
policy. Association, DHCP leases, and connection tables are reconstructible,
but ordinary lease or DNS changes do not themselves require destroying live
sockets. Production startup is valid offline: external DNS/HTTP proof and lab
deadlines are not service availability conditions. Suspend/resume, reconnect,
bounded recovery, and power-efficient event-driven operation are laptop
requirements. Driver and netstack are independently restartable units.
Wi-Fi reconnect does not automatically restart Netstack3. A Netstack3 crash
terminates its sockets and wakes applications; its replacement accepts new
sockets, not reconstructed TCP connections, as specified in
[ARCH-network-service](ARCH-network-service.md).

## Kernel portability

The kernel's lock-in is its driver ecosystem. Once every device is a userspace
driver, the kernel becomes a thin substrate (IOMMU/DMA, interrupt routing,
memory, scheduling, isolation primitives), and switching it re-ports only two
narrow seams rather than every driver:

1. The userspace-driver HAL (map BAR / DMA behind IOMMU / deliver interrupt),
   kept free of inline Linux VFIO specifics.
2. `lockdown()` (self-sandboxing).

Linux is the initial host for VFIO maturity, per
[REQ-host-portability](REQ-host-portability.md). FreeBSD/Capsicum is a serious
production-today upgrade on the capability axis (its cost is the userspace-driver
path); a capability microkernel (seL4, or Zircon, toward which the vendored
Fuchsia userspace naturally converges) is the small-TCB endgame.
