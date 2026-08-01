# ARCH-network-service: Network stack outside the host kernel

## Status

No IP/transport service or socket adapter is implemented. It follows the Wi-Fi
hardware milestone and remains independently replaceable.

One sandboxed portable service owns externally reachable Ethernet, ARP/NDP, IP,
fragmentation, ICMP, UDP, TCP, routing, and network policy. Splitting it by DNS
domain, remote address, or connection is not part of the architecture: those are
unstable identities and add coordination without protecting application keys.

Applications retain authenticated-encryption keys and plaintext. Compromise of
the network service may reveal ciphertext and metadata or control availability,
but must not reach application memory, device resources, or the host kernel.

A small host adapter preserves process-bound socket behavior such as descriptors,
inheritance, polling, cancellation, credentials, and descriptor passing. It is
protocol-agnostic and never parses packets or implements transport state. Linux
may require a kernel patch; another host kernel may provide a cleaner adapter.
This refines [REQ-application-compatibility](REQ-application-compatibility.md)
without weakening [REQ-host-portability](REQ-host-portability.md).
