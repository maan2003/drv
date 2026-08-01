# Network Stack Outside the Host Kernel

## Idea

Move the externally reachable packet-processing path, including IP, ICMP, UDP,
and TCP, into one sandboxed userspace service. The host kernel should not parse
network-controlled packets. Applications keep their existing socket behavior
through a host-specific compatibility layer.

This is a separate direction from the initial Wi-Fi driver milestone. Linux is
one possible substrate, not an architectural dependency. If preserving socket
behavior requires increasingly invasive Linux work, using or adapting a kernel
with a better service model may be simpler and safer.

## Boundary

```text
applications
    | normal socket API or typed capability API
host compatibility adapter
    | project-owned IPC and bounded shared buffers
single sandboxed IP/transport service
    | packet interface
sandboxed device stack
```

The portable service owns Ethernet demultiplexing, ARP/NDP, IP validation and
fragmentation, ICMP, UDP, TCP, routing, and related network policy. It has no
device handles, ambient host sockets, filesystem access, or application keys.

Use one network-stack process rather than processes per domain, remote address,
or connection. Domain names are not stable network identities, and finer splits
would add coordination and IPC without protecting secrets that remain behind
TLS, SSH, or another application-level authenticated encryption protocol.
Network-wide traffic disruption and denial of service are acceptable failures.

## Host Responsibilities

An adapter for ordinary applications may need to retain semantics tied to the
host process model:

- socket file descriptors and inheritance across `fork` and `exec`;
- blocking, nonblocking, polling, cancellation, and process teardown;
- credentials, namespaces, descriptor passing, quotas, and accounting;
- bounded transfer of requests, data, and readiness notifications.

It must remain protocol-agnostic: no packet parsing, TCP state, routing,
fragmentation, or network policy. On Linux, transparent `AF_INET` compatibility
may require a deliberate kernel patch rather than an interposing library or a
new socket family. Other kernels can implement the same internal contract using
their native service mechanisms.

## Security Value

A packet-triggered compromise reaches the sandboxed network service instead of
the host kernel. It may observe ciphertext and metadata, forge or suppress
traffic, redirect unauthenticated connection setup, and destroy availability.
It must not access application plaintext or TLS/SSH keys, arbitrary host memory,
the device, or unrelated services. The host adapter treats every service reply
as hostile and revokes socket objects when the service restarts.

The benefit depends on removing the whole hostile packet path. Moving only TCP
while leaving IP fragmentation, netfilter, tunneling, or other external parsers
in the host kernel does not establish the intended boundary.

## Possible Sequence

1. Expose copied packets from the userspace Wi-Fi stack.
2. Bring up a userspace IP/UDP/TCP implementation behind a typed application API.
3. Specify a portable socket-service protocol without Linux types or ioctls.
4. Prototype normal socket compatibility on the current Asahi Linux kernel.
5. Measure the kernel patch, trusted-code size, semantics, and performance.
6. Compare that result with implementing the adapter on alternative kernels.
