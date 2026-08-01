# ARCH-asahi-wifi-target: First physical device

The first physical target is the complete BCM4387C2 connectivity device in the
13-inch M2 MacBook Air (`t8112-j413`, board `apple,hokkaido`): FullMAC PCIe Wi-Fi
function `14e4:4433` and Bluetooth function `14e4:5f71`. They belong to one
physical connectivity device but are separate software delivery targets. Target
sysfs on `m2sh` places both `0000:01:00.0` and `0000:01:00.1` in IOMMU group 10.
Those are the only two functions in the group, and Bluetooth controller `hci0`
is backed by `0000:01:00.1`. The remaining PCI, DART, power, and reset topology
must be recorded before hardware bring-up.

The production safe hardware backend owns and assigns the complete group. Wi-Fi
and Bluetooth remain independent driver and service modules above that shared
hardware lifecycle; their delivery APIs must not encode the group layout.

During incremental Bluetooth testing only, the normal kernel drivers may retain
group 10: `hci_bcm4377` owns PCI, DMA, firmware, and reset while the experimental
host stack takes exclusive userspace control through an HCI user channel. This
keeps host Wi-Fi available but is a test adapter, not the production hardware
architecture.

The assigned connectivity device, its firmware, and its availability are
outside the protection boundary.

The primary behavioral reference is Asahi Linux commit
`e8efe09d4f378992c890d181d65e2ed8d8cb1194`, especially `brcmfmac` PCIe protocol
v6, firmware/NVRAM loading, common rings, `msgbuf`, MSI boot handling, reset, and
Apple firmware metadata. Port responsibilities, not `cfg80211`, `net_device`,
workqueues, or other Linux abstractions.

The physical feasibility milestone is experimental rather than documentary:
assign group 10 together, attach a DART-backed iommufd IOAS, map only private
DMA arenas, exercise interrupt and reset behavior, and restore both displaced
host drivers. HCI user-channel tests can proceed before this milestone.
