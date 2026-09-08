# ARCH-wlan-stack-topology: Userspace Wi-Fi stack process topology

## Status

Full real-internet connectivity is proven end to end through the userspace
MT7921 driver: it associates to a WPA3-SAE hotspot, completes the 4-way
handshake, and passes DHCP, DNS, TCP, and HTTP to the public Internet. The
current stack is split at the Ethernet seam: VFIO/DMA and Fuchsia MLME/SME stay
in the driver process, while Netstack3 and the SOCKS service run in a second,
self-sandboxed process reached only through a bounded Unix `SOCK_SEQPACKET`
frame channel. The netstack process has an empty filesystem root, a private
network namespace, no capabilities or device-backed mappings, and a seccomp
allowlist; its run-state descriptors are only standard streams, the frame
channel, the pre-bound SOCKS listener, and accepted clients. The separate DNS
and wlancfg policy processes in the mature topology below remain future work,
as does full Wi-Fi process sandboxing. The typed hardware-resource migration
is incomplete; production mechanics still share the lab runner. The
current MT7921 path deliberately leaves BCNFT disabled, retains
`MT_WF_RFCR_DROP_OTHER_BEACON`, and keeps the MLME's host lost-BSS monitor
active: firmware beacon-loss event `0x13` is recognized but not yet routed into
MLME teardown. This temporarily diverges from current Linux mt7921, which
enables BCNFT at association. The tracked destination is to complete the `0x13`
event route, enable BCNFT and firmware connection-monitor offload, and suppress
the host monitor as Linux `IEEE80211_HW_CONNECTION_MONITOR` does; those changes
must land together.

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
[REQ-host-portability](REQ-host-portability.md); its host transport (Unix
`SOCK_SEQPACKET` for control, a swappable fast path for bulk frames) is not part
of the contract.

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
`SetEthernetStatus`-gated frame pipe. MLME/SME/RSN stay with the driver, matching
Fuchsia, because they are chatty, latency-sensitive, and parse only local-air
802.11. A further cut at the WlanSoftmac seam (driver alone) is available later
as pure hardening; it is not required for the isolation goal. A compromised
netstack may reveal ciphertext and metadata for encrypted flows, plaintext for
unencrypted flows, or affect availability, but must not reach
application memory, device resources, DMA, or the host kernel, per
[REQ-isolation](REQ-isolation.md) and [ARCH-network-service](ARCH-network-service.md).

## Application handoff

All Internet-facing applications and desktop/system components are owned and
modifiable. Applications currently reach Netstack3 through **SOCKS**; native
capability-scoped streams and datagrams are the destination. Transparent Linux
socket compatibility is optional, not a prerequisite, per
[REQ-application-compatibility](REQ-application-compatibility.md). System UI
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

## Crate ownership

Crates enforce dependency boundaries; they do not themselves provide process
isolation. `mt76-core` / `mt7921-core`, the chip SoftMAC adapters, and
`wlan-softmac-host` retain hardware protocol, chip effects, and shared WLAN
runtime responsibilities respectively. `userspace-vfio` and the typed hardware
API/backends retain generic resource mechanics without device protocol policy.

Remaining production transport mechanics move out of the lab binary into their
library owner; production and lab entrypoints instantiate the same driver.
The lab runner owns experiments and reporting, not an alternate implementation.
Netstack3 binding, SOCKS, and network-service startup belong outside
`wlan-softmac-host`, connected by the small existing Ethernet contract rather
than a dependency from Wi-Fi runtime to Internet parsing. wlancfg owns the
policy service. Large implementations can use modules without creating a crate
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
- Dropped: all `*Bridge` protocols, FIDL, Zircon channels, `fuchsia.io`
  namespaces, and `component_manager` (heavily Zircon-bound; see Supervision).

## Testing

The "match the native Linux transcript" oracle is retained but rescoped to a
**driver bring-up conformance harness at the WlanSoftmac seam**: it checks that a
chip emits the same MCU/WTBL/TXWI/DMA command sequences as mac80211/mt76, which
is what makes the next device tractable. The invented-guard validation glue above
the seam is deleted. A hardware-independent net above the seam (hwsim virtual AP
or frame replay) is future work per
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
