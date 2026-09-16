# ARCH-network-service: Network stack outside the host kernel

## Status

Userspace Wi-Fi and a separately sandboxed Netstack3 service have demonstrated
Internet connectivity through both SOCKS and the production-shaped Linux socket
provider in a VFIO Wi-Fi guest. The independent
Linux socket-provider spike exercises a deterministic Ethernet peer, but is
not the production frontend described below: it retains native loopback and
uses synchronous RPC where native socket buffering and readiness are needed.
The production-shaped frontend now owns both Internet families in a KVM
kernel with native INET and the native loopback device excluded. A namespace registration capability yields
per-socket provider FDs with independent bounded queues and local accept queues;
control interruption revokes only the affected socket. Its sandboxed Netstack3 binding has passed
IPv4/IPv6 localhost TCP/UDP and provider-generation failure/replacement tests;
the same binding now accepts a frame-only Ethernet capability and passes
DHCP, application TCP/HTTP and UDP/DNS against a simulated AP, retaining
localhost after link loss. The reproducible harness and evidence live in
[`kernel-provider/production`](../crates/net/netstack3-port-spike/kernel-provider/production/README.md).
The same provider has demonstrated WPA3, DHCP, application DNS and verified
HTTPS through the userspace MT7921 driver in KVM. It is not yet the continuously
integrated Wi-Fi service. Linux protocol options, notably the error queue required
by glibc’s direct UDP resolver, remain unsupported. A thin Rust NSS module now
provides glibc forward hostname lookup over bounded local IPC to the existing
DNS runtime; a no-INET KVM test exercises dynamic loading and UDP/TCP DNS.
Applications bypassing NSS still need the broader socket compatibility work.
These tests do not establish broad socket
compatibility, hostile-provider robustness, or dependable physical throughput. Optimized localhost tests exceed
100 MB/s; this is not evidence of Wi-Fi deployment throughput.

The service implementation lives in `drv-network-service`. Its production
supervisor starts one offline provider generation and uses a private,
generation-tagged descriptor channel to detach and replace Ethernet links
without replacing the provider process or socket namespace. Provider crash
and restart remain a distinct, terminal generation boundary for old sockets.
Bounded physical validation now exercises disconnect/reconnect with the same
provider and listener processes, including renewed IPv4/IPv6 traffic and SSH.
This does not establish continuous deployment acceptance.

## Owner-selected goal and rationale

The immediate product goal is for np to use our MT7921 service as **the**
Wi-Fi driver continuously, with application and system networking through
sandboxed Netstack3. There is no native kernel Wi-Fi or TCP/UDP runtime
fallback. A separate recovery boot is not a fallback within the running
production system.

The owner selected transparent Linux Internet sockets as the production
application handoff so this is whole-machine networking, not a collection
of applications configured for a demonstration proxy. The kernel preserves
the application socket interface while delegating network protocol behavior
to Netstack3. Native TCP/UDP implementations are to be excluded at compile
time, reducing kernel attack surface rather than merely leaving those
implementations unused. This requires kernel integration work, not just an
existing configuration switch.

Dependability, containment, and understandable ownership matter more than
maximal throughput. The owner's useful Internet throughput target is at most
100 MB/s (about 800 Mb/s); performance beyond that is not a priority.
Double copying is acceptable. Zero-copy is not an acceptance prerequisite,
and actual throughput and CPU/power costs must be measured rather than assumed.

These choices refine [ARCH-drv](ARCH-drv.md) and
[REQ-application-compatibility](REQ-application-compatibility.md), without
making legacy Linux management interfaces or arbitrary unmodified application
compatibility the boundary of the whole project.

## Responsibilities and trust boundaries

One sandboxed portable network service owns Ethernet, ARP/NDP, IP,
fragmentation, ICMP, UDP, TCP, and routing. It is not split by DNS domain,
remote address, connection, or application frontend. SOCKS and the Linux
socket frontend are bindings to this service, not separate network stacks.
Wi-Fi selection and credentials belong to wlancfg. The host deployment uses
the separately sandboxed DNS service described in
[ARCH-wlan-stack-topology](ARCH-wlan-stack-topology.md): one encrypted upstream
engine serves NSS and IPv4/IPv6 loopback UDP/TCP clients through Netstack3.
The service manager owns the NSS listener path and passes the listening
capability to DNS; it never competes with a provider-owned NSS listener.
Legacy standalone network fixtures retain the integrated Hickory 0.26.3
resolver and its launcher-owned, locked Unix endpoint. Host deployments
disable that endpoint rather than running two independent resolver policies.
The NSS C ABI pointer adapter remains isolated from safe Rust lookup,
layout and I/O code.

The production Linux frontend owns the existing `AF_INET` and `AF_INET6`
socket families. It uses native socket objects, local bounded queues,
readiness, errors, and lifetime machinery rather than placing a proxy behind
Linux TCP. Netstack3 owns transport connections, retransmission, congestion
control, and packet processing. Localhost and wildcard listeners belong to
the same Netstack3 instance; there is no special native-loopback backend.

Route metadata is another binding of the same service. Linux retains native
AF_NETLINK transport and subscriptions; a separate namespace registration
delegates NETLINK_ROUTE endpoints to the sandbox. The adapter publishes real
link/address observations and lifecycle events, not a second interface-state
database. Old endpoints cannot join a replacement provider generation, and
provider absence never silently exposes native kernel state. The current
adapter supports read-only link/address discovery; route queries and mutations
remain unsupported. Other Netlink protocols retain their native owners.

Seccomp restricts syscall kinds, commands, flags and other non-descriptor
arguments, never descriptor numbers. Capability identity, access mode and
generation are enforced by the underlying objects; duplication or slot reuse
does not change authority. Launchers retain only the intended capabilities,
open immutable inputs read-only, and isolate filesystem/network namespaces.
Fixed inherited FD numbers are startup conventions only, not security labels.

The kernel mediates provider authority and resource limits. A compromised
provider cannot select arbitrary namespaces, access unrelated sockets or
application memory, or manufacture kernel pointers. Applications retain
authenticated-encryption keys and plaintext for encrypted connections;
Netstack3 sees ciphertext and metadata for those flows. Unencrypted traffic
is not confidential from it. Device/DMA authority remains exclusively with
the Wi-Fi side of the Ethernet capability boundary.

This preserves [REQ-isolation](REQ-isolation.md) and
[REQ-host-portability](REQ-host-portability.md): Linux socket mechanics stay
in the host binding, not the portable network core.

## Recovery without hidden fallback

The network service starts offline. Captive portals and failed external probes
change reachability, not permission to start serving applications. Ordinary
address, route, lease, or DNS changes do not by themselves destroy sockets.

Wi-Fi reconnect does not automatically restart Netstack3. Losing the Ethernet
capability invalidates that link generation, not inherently the provider
process; individual connections may still fail because connectivity was lost.
Wi-Fi and Netstack3 are independently restartable.

The owner explicitly accepts that a Netstack3 crash terminates its existing
sockets and wakes blocked applications. A replacement provider serves new
sockets in a new generation; it does not reconstruct lost TCP connections or
complete requests from its predecessor. Applications reconnect normally.
This avoids pretending that ephemeral transport state survived a crash and
keeps recovery simpler than transparent connection resurrection.

## Network namespace ownership

Unowned Linux network namespaces acquire independent, loopback-only Netstack3
instances lazily on first Internet/route-metadata socket or interface-control
request. Namespace creation itself does not spawn
a service. The generic supervisor receives global provisioning authority, passes
a namespace-bound serving capability to a sandboxed worker, and retains only
separate lifecycle/revocation authority. It neither enters served namespaces nor
gives these workers Ethernet, Wi-Fi, DMA or netcfg authority. Explicitly supervised
namespaces remain reserved for their existing supervisor across provider failure;
lazy provisioning must not replace the machine's network policy.

Application sockets and namespace handles retain native namespace lifetime.
Service capabilities retain only safe backing memory: namespace teardown revokes
them and causes workers to be reaped. Authority follows the socket or serving
object across descriptor transfer and `setns`, not the caller's current namespace.
A single namespace generation owns Internet sockets, route metadata and interface
control; explicit and lazy launchers use the same serving object. Failure and
replacement invalidate all three together. Old capabilities cannot mutate,
provision or revive a replacement.

Loopback administration changes actual core device state before completion is
acknowledged. Newly isolated namespaces start with loopback down and no assigned
addresses; address/link views reflect core observations, not invented Ethernet
or configured-address fixtures. Interface ioctl and rtnetlink interpretation,
authorization policy and configuration belong to the userspace wrapper, backed by
the same core state. Linux only authenticates namespace-relative credentials,
marshals user memory and transports bounded, revocable requests. There is no native
loopback device or flags mirror. A replacement isolated worker starts down again;
configuration does not survive its owning generation. Unsupported management
operations fail explicitly.

## Validation without a production escape hatch

KVM on np isolates kernel and service development from the host's management
networking. The owner chose VM-based testing rather than adding native-backed
test namespaces to the production frontend. VMs are a test environment,
not the deployed runtime architecture.

A fake provider can establish socket and failure semantics before real
Netstack3 uses a virtual Ethernet link. Full-stack acceptance then runs the
kernel frontend, sandboxed Netstack3, and userspace MT7921 inside the guest,
with the physical Wi-Fi device assigned through VFIO. Host device assignment
and guest driver DMA confinement are distinct boundaries; a virtual IOMMU
configuration must be verified rather than inferred from host passthrough.

## Binding reference

Fuchsia's Netstack3 bindings are the primary reference for buffer ownership,
capacity changes, readiness and core notifications. Adapt their contracts to
Linux capabilities and the service executor; do not replace Netstack3 transport
policy with Linux TCP policy. Linux remains the reference for the application
socket ABI and kernel resource mechanics.

## Device introduction and observation binding

The launcher owns process lifetime, containment and prebound listeners. A
separate sandboxed `netcfg-service` receives device introduction/removal and a
private Netstack administration capability; it does not receive credentials,
device registers, DMA mappings or process-control authority. A logical Ethernet
interface and the Netstack socket namespace survive link loss. Frame endpoints
are link-scoped and created as needed, so old queued frames cannot cross a new
link and reconnect is not limited by a bootstrap pool.

Core IP-device, neighbor, router-advertisement and Ethernet multicast events
update a typed, bounded observation view. Netcfg watches coalesced snapshots,
and the read-only `netcfg-status` endpoint exposes assignment state, lifetimes,
neighbors, membership and the latest RA. This is not an unbounded event history:
address/neighbor/membership observations are capped at 64 each, RA options at
8192 bytes, and overflow explicitly marks the view incomplete. Observations
retain scalar device identifiers, not core device ownership. Raw RA options are
excluded from implicit debug output.

The current MT7921 frame binding receives multicast without a programmable
group-address filter. This matches pinned Linux `mt7921_configure_filter`,
which does not implement the multicast-list argument or `FIF_ALLMULTI`.
Netstack3's existing IPv4/IPv6 group-membership checks decide local delivery;
the host binding must not create a second IGMP/MLD policy or invent firmware
filter commands. Ethernet join/leave notifications also update the observation
view. This all-multicast receive contract does not imply multicast forwarding.

Watcher pressure cannot block packet processing: acknowledgements are retained,
control intake pauses behind a blocked acknowledgement, and observations
coalesce until writable readiness. Netcfg's accepted status clients are bounded
and time-limited. Its frame monitor owns the received capability directly and drops it on
revocation; there is no reserved descriptor slot. Link-down disables both IP families on the Ethernet
device, flushing its neighbors while leaving the separate loopback device
available. DHCP run effects, waits and DNS resolver work are cancelled on
revocation; unchanged DNS configuration preserves pending queries and cache.

These bindings have host/core tests and bounded physical MT7921 KVM coverage:
offline sockets, NSS resolution and IPv4/IPv6 applications, repeated link
replacement, power transitions, saved-state restart and certified hardware
stop. This does not establish continuously deployed host operation or complete
Linux socket-option compatibility.

## Implementation direction and open choices

These details support the decisions above; they are not additional product
goals or a claim that the existing spike implements them.

- Use socket-family registration, `struct sock`/`proto_ops`, native wait and
  notification mechanisms, and kernel-accounted queues. Borrow vsock's
  asynchronous buffering, flow-control, and lifetime patterns where suitable,
  not its addressing model or a requirement for virtio.
- Data moves asynchronously with bounded credits across both processes.
  Reads and writes operate on local queue state rather than requiring a
  userspace RPC round trip for every syscall. Control operations still need
  explicit completion and cancellation semantics.
- Copied, batched transfers over a scoped, pollable provider endpoint are the
  initial baseline. Endpoint framing, queue sizes, and exact kernel helpers
  remain implementation choices.
- The owner suggested representing the **provider-facing channel** as an
  `AF_*` socket family too. This is an option to explore, distinct from the
  application-facing `AF_INET`/`AF_INET6` implementation. It may enable reuse
  of socket buffer-transfer mechanisms and reduce kernel-side copying.
  Family registration alone does not establish zero-copy: userspace buffer
  ownership, transfer, bounds, and revocation still need implementation and
  verification. Neither a character-device endpoint nor an `AF_*` endpoint
  is mandated; zero-copy, shared rings, and io_uring are not prerequisites.
