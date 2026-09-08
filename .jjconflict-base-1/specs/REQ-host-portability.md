# REQ-host-portability: Keep portable components host-independent

## Status

The component contract contains no Linux types; only a deterministic host exists.

Source: project owner. Strength: architectural requirement.

Hardware, policy, service, and network components must be portable across host
kernels. Their contracts must not expose Linux file descriptors, ioctls, kernel
types, VFIO objects, cfg80211, nl80211, TAP, or other host plumbing.

Host-specific native brokers and compatibility adapters may use kernel modules
or deliberate kernel patches. Linux is the initial hardware host, not a required
long-term substrate; FreeBSD, Redox, or a more suitable kernel may implement the
same project-owned contracts.
