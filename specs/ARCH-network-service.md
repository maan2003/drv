# ARCH-network-service: Network stack outside the host kernel

## Status

A native Netstack3 IP/transport service and Linux kernel socket adapter now run
as a deployment-tested spike against a deterministic Ethernet peer. They remain
independently replaceable and production external connectivity still awaits the
Wi-Fi Ethernet owner. The spike currently combines the service and host-adapter
binding in one daemon, so the mature process-isolation boundary described below
is not yet complete. Provider availability follows the attached data-plane
transport rather than DHCP state: address/route/DNS loss changes reachability
without destroying application sockets, while transport loss revokes the
generation.

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
