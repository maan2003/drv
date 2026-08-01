# ARCH-asahi-wifi-target: First physical device

The first physical target is the complete BCM4387C2 connectivity device in the
13-inch M2 MacBook Air (`t8112-j413`, board `apple,hokkaido`): FullMAC PCIe Wi-Fi
function `14e4:4433` and Bluetooth function `14e4:5f71`. They belong to one
physical connectivity device but are separate software delivery targets. Target
sysfs on `m2sh` places both `0000:01:00.0` and `0000:01:00.1` in IOMMU group 10.
The remaining PCI, DART, power, and reset topology must be recorded before
choosing how either function is assigned.

Bluetooth should be independently testable and deployable while the normal
Wi-Fi driver remains active, and vice versa, when the platform can isolate the
selected function. Group 10 prevents that with stock VFIO on `m2sh`, and a
shared reset may require further coordination, but those backend constraints do
not make combined software delivery an architectural requirement. Bluetooth
host and profile development can retain the kernel `hci_bcm4377` transport and
take exclusive userspace ownership through an HCI user channel. A later
function-specific kernel broker is permitted when it can retain DMA isolation
while exposing only the selected function to safe Rust.

The assigned function, its firmware, and its availability are outside the
protection boundary. An unassigned sibling remains protected host state and
must not be reset, reconfigured, or reached by DMA from the assigned function.

The primary behavioral reference is Asahi Linux commit
`e8efe09d4f378992c890d181d65e2ed8d8cb1194`, especially `brcmfmac` PCIe protocol
v6, firmware/NVRAM loading, common rings, `msgbuf`, MSI boot handling, reset, and
Apple firmware metadata. Port responsibilities, not `cfg80211`, `net_device`,
workqueues, or other Linux abstractions.

The physical feasibility milestone is experimental rather than documentary:
inventory both functions, prove whether they have independently controllable
DART streams and resets, then either assign group 10 together or exercise the
smallest safe kernel-owned interface. Only private DMA arenas are mapped, and
every displaced host driver is restored after interrupt and reset testing.
