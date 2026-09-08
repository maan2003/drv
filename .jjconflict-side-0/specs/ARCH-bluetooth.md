# ARCH-bluetooth: Fuchsia-derived Bluetooth service

## Status

Sapphire is the initial host stack and all Sapphire C++ executes inside one
least-authority WebAssembly instance in a dedicated sandboxed worker process.
A native Rust supervisor owns the controller and external service capabilities.
The current implementation is limited to a deterministic and
physically verified Linux HCI transport oracle; it does not yet execute
Sapphire or the Fuchsia-derived service layer. This architecture does not
supersede the hardware and isolation records, especially
[ARCH-asahi-wifi-target](ARCH-asahi-wifi-target.md) and
[REQ-isolation](REQ-isolation.md).

## Architecture

The mature system replaces BlueZ with a project-owned Bluetooth service
derived from Fuchsia's Bluetooth architecture and tests. Existing desktop
Bluetooth management APIs are not compatibility surfaces. Ordinary applications
receive audio, input, and explicit Bluetooth capabilities through their normal
application-facing interfaces.

```text
project system UI and sandbox capability policy
        -> native Rust Bluetooth supervisor
             -> Rust coordinator, profiles, and policy tasks
             -> native Rust bounded HCI controller broker
             -> bounded framed IPC
                  -> sandboxed Wasmtime worker process
                       -> one least-authority Sapphire C++ WebAssembly instance
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
is currently C++ and has moved from Fuchsia to Pigweed. It provides the initial
host implementation and behavioral oracle inside WebAssembly; Sapphire C++
never executes natively. Host layers may then be ported to safe Rust
independently while retaining Sapphire's protocol tests.

The WASM boundary is a stable copied C/WIT-style ABI rather than a C++ ABI or
shared pointers. Standard WASI Preview 2 monotonic-clock, timer/poll, and secure
random interfaces provide safe platform primitives. Project imports are
limited to bounded HCI command, ACL, SCO, and ISO packet delivery; logs and
metrics; and identity-keyed persistence requests. Wasmtime implements the
project imports in the worker by forwarding bounded copied messages over framed Unix
`SOCK_SEQPACKET` IPC to the native Rust supervisor. The same transport carries
bounded high-level host operations and multiplexed HCI command/event, ACL, SCO,
and ISO planes. The raw controller descriptor,
lifecycle and reset operations, USB or VFIO authority, firmware, unrestricted
storage, audio, input injection, and external application APIs are never
available to WASM.

The worker receives no HCI, device, filesystem, or network descriptors. It is
linked only to the selected WASI interfaces, not a broadly configured
`WasiCtx`; filesystem preopens, sockets, environment, arguments, process/exec,
terminal, wall-clock, and device interfaces remain absent. The component import
allowlist verifies those exclusions. The worker is additionally constrained with seccomp, Landlock,
namespaces, and resource limits. Worker exit or protocol failure makes the
supervisor cancel outstanding operations and clean up or reset the controller.
Discovery and representative ACL, SCO/audio, and ISO loads are benchmarked
before considering shared-memory rings or eventfd; those mechanisms are added
only if the framed copied transport is measurably inadequate.

## M2 hardware boundary

The Apple-specific work remains below HCI. A safe Rust transport would preserve
the behavior of Asahi's `hci_bcm4377` driver for PCI lifecycle, firmware,
command, event, ACL, SCO, power, and reset. It would use the safe hardware crate
described by [IDEA-rust-first-wifi-drivers](IDEA-rust-first-wifi-drivers.md),
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

The trusted safe Rust supervisor and transport own the hardware capability.
Bluetooth packets and peer-controlled protocol state are hostile input, so the
sandboxed Sapphire worker receives only explicit bounded copied capabilities
and the selected monotonic-clock, poll, and secure-random WASI interfaces. It
cannot access native descriptors or pointers, ambient WASI facilities, VFIO,
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
