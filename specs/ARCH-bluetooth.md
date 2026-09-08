# ARCH-bluetooth: Fuchsia-derived Bluetooth service

## Status

The current implementation is a deterministic and physically verified Linux
HCI transport oracle plus legacy Sapphire/Wasm scaffolding, not a production
Bluetooth service. Under [ARCH-drv](ARCH-drv.md), native Rust and strong process
sandboxing replace the previously proposed Sapphire Wasm execution direction.
The complete Rust host-stack implementation remains future work; this decision
does not claim a completed Sapphire port.

## Architecture

The mature system replaces BlueZ with a project-owned Bluetooth service
derived from Fuchsia's Bluetooth architecture and tests. Existing desktop
Bluetooth management APIs are not compatibility surfaces. Ordinary applications
receive audio, input, and explicit Bluetooth capabilities through their normal
application-facing interfaces.

Fuchsia supplies architectural inspiration, Rust coordination/profile code,
and protocol tests. Portable components use project-owned capability bindings
rather than FIDL/Zircon host plumbing. Native Rust services separate persistent
pairing policy and credentials from hostile Bluetooth protocol input and
hardware authority. Exact host/profile process cuts remain to be determined;
this record does not prescribe a replacement microservice graph.

Sapphire's C++ protocol knowledge and tests may inform Rust ports and offline
oracles. Sapphire-in-Wasm is not a production path. Monotonic time, secure
randomness, HCI frames, persistence requests, audio, and input are explicit
bounded capabilities, not ambient access. Worker failure requires cancellation
and bounded controller cleanup/recovery by the resource owner.

## M2 hardware boundary

The Apple-specific work remains below HCI. A safe Rust transport would preserve
the behavior of Asahi's `hci_bcm4377` driver for PCI lifecycle, firmware,
command, event, ACL, SCO, power, and reset. It would use the safe hardware crate
described by [ARCH-hardware-isolation](ARCH-hardware-isolation.md),
with VFIO/iommufd on the initial Linux host.

Wi-Fi and Bluetooth share one physical connectivity device on the M2 target but
remain separate software delivery units. Their APIs do not expose or depend on
the physical IOMMU grouping, while the production safe hardware backend owns
their shared lifecycle. On `m2sh`, both PCI functions are in IOMMU group 10, so
the production VFIO backend assigns them together. Initial Bluetooth host-stack
testing can keep `hci_bcm4377` bound and use an exclusive Linux HCI user channel
without replacing the active Wi-Fi driver. That adapter is only a testing
workaround. Firmware remains untrusted and may DMA only into dedicated mapped
arenas in the production backend.

## Application and system interfaces

BlueZ D-Bus compatibility is explicitly out of scope. In particular, existing
GNOME/KDE Bluetooth panels, pairing agents, `bluetoothctl`, and applications
written directly against BlueZ are not promised to work.

Instead:

- A2DP and HFP connect directly to the project audio service, which publishes
  PipeWire-compatible streams to ordinary media applications.
- AVRCP and headset controls connect to the project media-session model.
- HID devices enter the project input service and its Wayland-facing path.
- Applications needing BLE or GATT receive explicit, identity-scoped
  capabilities through a project API rather than ambient system-bus access.
- A project-owned Bluetooth settings and pairing UI uses the native management
  API as a system component.

This distinction anticipates application sandboxing. The desktop shell,
settings panels, pairing UI, policy services, and compatibility servers are
vendored system components, not arbitrary applications whose implementation or
legacy management APIs must be preserved.

## Security boundary

Native Rust hardware owners use the typed resource boundary. Bluetooth packets
and peer-controlled protocol state remain hostile input. Strong process
sandboxing and bounded capability interfaces must prevent a compromised parser
from reaching unrelated devices, filesystem state, applications, or the kernel.
Pairing, bond storage, microphone use, input injection, and application GATT
access are distinct policy-controlled capabilities. Rust safety complements,
rather than replaces, process and IOMMU confinement.

## Adoption consequences

Adoption would remove BlueZ and its D-Bus object model from the target
architecture. It requires an Apple HCI transport, a project Bluetooth API,
native Rust protocol implementation, direct media/input integration, persistent
bond storage, and a vendored system UI. The smallest useful slice is virtual HCI
plus discovery and pairing, followed by A2DP output through the proposed audio
service.
