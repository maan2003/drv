# REQ-application-compatibility: Preserve ordinary application interfaces

## Status

No compatibility service is implemented in the initial hardware-interface spike.

Source: project owner. Strength: mandatory for the mature system, incremental
during device bring-up.

Arbitrary ordinary unprivileged applications must continue using the established
interfaces they depend on, including Wayland, PipeWire client APIs, Mesa
GL/EGL/Vulkan, BlueZ application APIs where applicable, and eventually normal
Internet socket behavior.

Replaceable host plumbing is not compatibility surface. Implementations may fork
or replace wpa_supplicant, iwd, BlueZ, PipeWire, and host-kernel facilities such
as cfg80211, nl80211, kernel HCI, ALSA, and TAP. Compatibility services validate
client identity, object ownership, shared-memory descriptors, state, and bounds;
they never delegate unrestricted device or DMA capabilities.
