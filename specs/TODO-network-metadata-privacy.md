# TODO-network-metadata-privacy: Caller-scoped network metadata

Deferred design work, not an implemented privacy guarantee or a change gate.
The current trial behavior remains unchanged.

Network compatibility consumers need information about configured address
families, interfaces, addresses, and routes. Publishing the provider's complete
view to every application could disclose local addresses, MACs, topology, or
network changes beyond that application's authority.

Before broadening metadata exposure, decide:

- Which metadata each caller's network capability permits it to observe.
- Whether family availability can be disclosed without concrete addresses.
  Stock glibc derives `AI_ADDRCONFIG` from interface/address enumeration;
  Tailscale's pure-Go interface monitor is a separate consumer.
- How native metadata APIs and any Linux compatibility views apply the same
  visibility policy, without alternate enumeration paths bypassing it.
- How snapshots, cached answers, and change notifications respect revocation
  and provider/link replacement.
- How cross-sandbox tests establish that unrelated network state is hidden.

Keep configured-family availability distinct from Internet reachability.
Hiding local metadata does not conceal a connection's public source address
from its destination; enforced proxy/VPN routing is a separate policy question.

This follow-up belongs at the boundary between the network service, capability
policy, and application/host adapters; it does not imply putting Linux API
semantics into the portable protocol core. See
[ARCH-network-service](ARCH-network-service.md),
[REQ-application-compatibility](REQ-application-compatibility.md), and
[REQ-isolation](REQ-isolation.md).
