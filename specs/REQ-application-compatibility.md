# REQ-application-compatibility: Preserve ordinary application interfaces

## Status

The separately tested Linux kernel provider and native Netstack3 daemon now
implement ordinary remote IPv4/IPv6 UDP and TCP socket operations. Deterministic
deployment testing covers pre-lease non-capture, link lifecycle, and fail-closed
restart, but not yet a composed DHCP-bound application flow. Real production
connectivity still awaits the Wi-Fi Ethernet owner, and the wider application
compatibility requirement remains incremental.

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
