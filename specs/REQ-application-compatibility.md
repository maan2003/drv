# REQ-application-compatibility: Preserve ordinary application interfaces

## Status

The Linux kernel provider and native Netstack3 daemon implement ordinary remote
IPv4/IPv6 UDP and TCP socket operations. Deterministic deployment testing covers
offline socket setup, DHCP-bound UDP/TCP, live sockets across lease
loss/reacquisition, static IPv4 without a DHCP server, and generation revocation
on peer transport loss. Wildcard sockets currently select the remote provider
stack rather than simultaneously listening on private Linux loopback. Real
production connectivity still awaits the Wi-Fi Ethernet owner. Deferred
offline UDP connect supports later sends, but peer-filtered receive and
disconnect after deferred configuration remain incremental, as does the wider
application compatibility requirement.

Source: project owner. Strength: mandatory for the mature system, incremental
during device bring-up.

Arbitrary ordinary unprivileged applications must continue using the established
interfaces they depend on, including Wayland, PipeWire client APIs, Mesa
GL/EGL/Vulkan, and eventually normal Internet socket behavior. Applications
that require Bluetooth device access use a project capability API; BlueZ D-Bus
compatibility is not required.

Replaceable host plumbing is not compatibility surface. Implementations may fork
or replace wpa_supplicant, iwd, BlueZ, PipeWire, and host-kernel facilities such
as cfg80211, nl80211, kernel HCI, ALSA, and TAP. Compatibility services validate
client identity, object ownership, shared-memory descriptors, state, and bounds;
they never delegate unrestricted device or DMA capabilities.

Desktop shells, settings panels, pairing agents, policy daemons, portals, and
other system-management components are vendored parts of the system rather than
compatibility targets. They may use project-native capability APIs and will
evolve with future application sandboxing.

Potential native replacements are recorded in
[IDEA-audio](IDEA-audio.md) and [ARCH-bluetooth](ARCH-bluetooth.md).
