# REQ-application-compatibility: Application integration without legacy plumbing lock-in

## Status

Owned applications can use SOCKS through the MT7921 userspace Netstack3 path.
A separate Linux socket-provider spike exercises transparent socket behavior
against a deterministic Ethernet peer; it does not yet implement the selected
production handoff in [ARCH-network-service](ARCH-network-service.md).

Source: project owner. Strength: product scope and architectural requirement.

All Internet-facing applications are assumed owned and modifiable. All system
and desktop userspace may be replaced, including shells, settings, portals,
policy daemons, network managers, and compatibility servers. They may adopt
project-native capability interfaces. For np's production networking, the owner
selected a Linux Internet socket frontend backed by sandboxed Netstack3:
existing socket calls must reach our stack without per-application SOCKS
configuration. This does not require reproducing every Linux networking API
or supporting arbitrary unmodified applications. Applications and system
services may still be adapted where they depend on native kernel networking
beyond the socket interface. Native capability interfaces remain compatible
with this direction, but are not a prerequisite for np's handoff.

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
