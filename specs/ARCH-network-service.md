# ARCH-network-service: Network stack outside the host kernel

## Status

The MT7921 path runs Netstack3, DHCP, DNS, and SOCKS in a separate self-sandboxed
process connected to the Wi-Fi service by bounded Ethernet frames. Its startup
still requires external DHCP/DNS/TCP proof and its lifetime is bounded: these
are lab behavior, not the mature service contract. The service implementation
and executable live in `drv-network-service`, independent of Wi-Fi runtime
ownership. A Linux socket-provider spike also exists but is not the required
application path.

One sandboxed portable network service owns Ethernet, ARP/NDP, IP,
fragmentation, ICMP, UDP, TCP, and routing. It is not split by DNS domain, remote
address, or connection. Wi-Fi selection and credential policy belong to wlancfg,
not the network service. DNS currently shares this process; the separate DNS
service described in [ARCH-wlan-stack-topology](ARCH-wlan-stack-topology.md)
remains a later boundary.

Applications retain authenticated-encryption keys and plaintext for encrypted
connections. The network service sees ciphertext and metadata for those flows;
unencrypted application traffic is not confidential from it. Compromise must
not confer access to application memory, device resources, or the host kernel.

Owned applications initially use SOCKS; native capability-scoped stream and
datagram interfaces are the destination. A transparent host socket adapter is
optional, not a prerequisite. Such an adapter, if retained, owns descriptor and
process semantics rather than packet parsing or transport state.

The production service starts offline and remains available through address,
route, and DNS loss. Captive portals or failed external probes change reported
reachability, not permission to start serving applications. Actual transport
loss revokes the affected generation; ordinary lease changes do not themselves
destroy application sockets. The network service and Wi-Fi service are
independently restartable.

This refines [REQ-application-compatibility](REQ-application-compatibility.md),
[REQ-isolation](REQ-isolation.md), and
[REQ-host-portability](REQ-host-portability.md).
