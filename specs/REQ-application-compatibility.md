# REQ-application-compatibility: Application integration without legacy plumbing lock-in

## Status

Owned applications can use SOCKS through the MT7921 userspace Netstack3 path.
A separate Linux socket-provider spike exercises transparent socket behavior
against a deterministic Ethernet peer; it is not the required production
handoff. Native application stream/datagram capabilities remain a destination.

Source: project owner. Strength: product scope and architectural requirement.

All Internet-facing applications are assumed owned and modifiable. All system
and desktop userspace may be replaced, including shells, settings, portals,
policy daemons, network managers, and compatibility servers. They may adopt
project-native capability interfaces. Transparent compatibility with arbitrary
unmodified Internet applications and Linux socket plumbing is not a production
prerequisite.

Useful established application interfaces such as Wayland, PipeWire client
APIs, and Mesa GL/EGL/Vulkan may be retained; their existence does not make the
underlying system implementation or management APIs immutable. Replacing
cfg80211, nl80211, iwd, BlueZ, kernel HCI, ALSA, or TAP is permitted. Bluetooth
applications use explicit capabilities rather than requiring BlueZ D-Bus.

Application and desktop integration must preserve identity, object ownership,
shared-memory bounds, and capability scoping. Owning an application does not
make its network input trusted. No application API delegates unrestricted
device or DMA authority. This scope supports the secure-laptop goal in
[ARCH-drv](ARCH-drv.md) without weakening [REQ-isolation](REQ-isolation.md).
