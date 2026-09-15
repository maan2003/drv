# ARCH-network-service: Network stack outside the host kernel

## Status

Userspace Wi-Fi and a separately sandboxed Netstack3 service have demonstrated
Internet connectivity through both SOCKS and the production-shaped Linux socket
provider in a VFIO Wi-Fi guest. The independent
Linux socket-provider spike exercises a deterministic Ethernet peer, but is
not the production frontend described below: it retains native loopback and
uses synchronous RPC where native socket buffering and readiness are needed.
The production-shaped frontend now owns both Internet families in a KVM
kernel with native INET excluded. A namespace registration capability yields
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
Continuous physical validation of link replacement remains pending.

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
Wi-Fi selection and credentials belong to wlancfg. DNS currently shares
the network process and uses Hickory 0.26.3 with native Netstack3 transport;
the optional NSS endpoint carries lookup requests, not DNS parsing or a second
network stack. Its C ABI pointer adapter is isolated from safe Rust lookup,
layout and I/O code; the separate DNS service described in
[ARCH-wlan-stack-topology](ARCH-wlan-stack-topology.md) remains a later boundary.

The production Linux frontend owns the existing `AF_INET` and `AF_INET6`
socket families. It uses native socket objects, local bounded queues,
readiness, errors, and lifetime machinery rather than placing a proxy behind
Linux TCP. Netstack3 owns transport connections, retransmission, congestion
control, and packet processing. Localhost and wildcard listeners belong to
the same Netstack3 instance; there is no special native-loopback backend.

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
