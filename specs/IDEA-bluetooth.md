# IDEA-bluetooth: Fuchsia-derived Bluetooth service

## Status

This is an unfinalized direction for discussion. It does not supersede the
hardware and isolation records, especially
[ARCH-asahi-wifi-target](ARCH-asahi-wifi-target.md) and
[REQ-isolation](REQ-isolation.md).

## Idea

The mature system would replace BlueZ with a project-owned Bluetooth service
derived from Fuchsia's Bluetooth architecture and tests. Existing desktop
Bluetooth management APIs are not compatibility surfaces. Ordinary applications
receive audio, input, and explicit Bluetooth capabilities through their normal
application-facing interfaces.

```text
project system UI and sandbox capability policy
        -> Rust Bluetooth coordinator and pairing
        -> Fuchsia-derived Rust profiles and services
        -> Sapphire host stack, sandboxed initially and ported incrementally
        -> safe Rust Apple HCI transport
        -> typed hardware crate and private IOMMU domain
        -> BCM4387 Bluetooth function
```

Fuchsia supplies Rust GAP coordination, common Bluetooth libraries, A2DP,
AVRCP, HFP, HID, RFCOMM, Fast Pair, test harnesses, a virtual HCI controller,
mock piconets, fuzz targets, and Pandora/conformance integration. Their FIDL and
Zircon bindings would be replaced with project-owned capability interfaces.

The core Sapphire host stack implements HCI, L2CAP, ATT, GATT, GAP, SDP,
security management, SCO, and ISO. It is certified and production-proven, but
is currently C++ and has moved from Fuchsia to Pigweed. It can provide the
initial host implementation and behavioral oracle, but must run without raw
device authority in a process sandbox. Host layers may then be ported to safe
Rust independently while retaining Sapphire's protocol tests.

## M2 hardware boundary

The Apple-specific work remains below HCI. A safe Rust transport would preserve
the behavior of Asahi's `hci_bcm4377` driver for PCI lifecycle, firmware,
command, event, ACL, SCO, power, and reset. It would use the safe hardware crate
described by [IDEA-rust-first-wifi-drivers](IDEA-rust-first-wifi-drivers.md),
with VFIO/iommufd on the initial Linux host.

Wi-Fi and Bluetooth are one indivisible deployment and recovery unit on the M2
target, regardless of their presentation as separate PCI functions. Both are
detached, assigned, reset, and restored together, while each protocol service
receives only its typed function capability. Software bring-up may focus on one
protocol, but a production deployment cannot return the other function to the
host. Firmware remains untrusted and may DMA only into dedicated mapped arenas.

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

The trusted safe Rust transport owns the hardware capability. Bluetooth packets
and peer-controlled protocol state are hostile input, so the initial C++
Sapphire process receives only bounded HCI packets and cannot access VFIO,
IOMMU configuration, firmware storage, or unrelated host services. Pairing,
bond storage, microphone use, input injection, and application GATT access are
separate capabilities controlled by system policy.

## Adoption consequences

Adoption would remove BlueZ and its D-Bus object model from the target
architecture. It requires an Apple HCI transport, a project Bluetooth API,
Sapphire integration or Rust ports, direct media/input integration, persistent
bond storage, and a vendored system UI. The smallest useful slice is virtual HCI
plus discovery and pairing, followed by A2DP output through the proposed audio
service.
