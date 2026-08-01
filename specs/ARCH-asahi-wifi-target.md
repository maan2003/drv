# ARCH-asahi-wifi-target: First physical device

The first physical target is BCM4387C2 FullMAC PCIe Wi-Fi (`14e4:4433`) in the
13-inch M2 MacBook Air (`t8112-j413`, board `apple,hokkaido`). Bluetooth function
`14e4:5f71` shares the upstream PCIe path and probably the DART stream and IOMMU
group; target sysfs must confirm this before assignment.

If grouped together, both functions are assigned to VFIO and form one recovery
domain while only Wi-Fi is driven. The assigned device, firmware, Bluetooth
function, and their availability are outside the protection boundary.

The primary behavioral reference is Asahi Linux commit
`e8efe09d4f378992c890d181d65e2ed8d8cb1194`, especially `brcmfmac` PCIe protocol
v6, firmware/NVRAM loading, common rings, `msgbuf`, MSI boot handling, reset, and
Apple firmware metadata. Port responsibilities, not `cfg80211`, `net_device`,
workqueues, or other Linux abstractions.

The physical feasibility milestone is experimental rather than documentary: bind the
whole group, attach a DART-backed iommufd IOAS, map only a private DMA arena,
receive an interrupt, reset, and restore the normal host drivers.
