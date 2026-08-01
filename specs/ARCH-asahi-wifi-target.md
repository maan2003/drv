# ARCH-asahi-wifi-target: First physical device

The first physical target is the complete BCM4387C2 connectivity device in the
13-inch M2 MacBook Air (`t8112-j413`, board `apple,hokkaido`): FullMAC PCIe Wi-Fi
function `14e4:4433` and Bluetooth function `14e4:5f71`. They share the
deployment, assignment, reset, and recovery boundary and must not be split
between the project and normal host drivers. Target sysfs must still record the
exact PCI, DART, and IOMMU topology before assignment.

Both functions are assigned together and the production deployment must drive
both Wi-Fi and Bluetooth. Bring-up may exercise one protocol at a time, but the
other function remains detached and unavailable rather than returning to a host
driver. The assigned device, firmware, both functions, and their availability
are outside the protection boundary.

The primary behavioral reference is Asahi Linux commit
`e8efe09d4f378992c890d181d65e2ed8d8cb1194`, especially `brcmfmac` PCIe protocol
v6, firmware/NVRAM loading, common rings, `msgbuf`, MSI boot handling, reset, and
Apple firmware metadata. Port responsibilities, not `cfg80211`, `net_device`,
workqueues, or other Linux abstractions.

The physical feasibility milestone is experimental rather than documentary:
bind the whole connectivity device, attach a DART-backed iommufd IOAS, map only
private DMA arenas, exercise interrupt delivery, reset the shared device, and
restore both normal host drivers.
