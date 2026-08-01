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

## Development handoff

Wi-Fi is the first hardware priority. During incremental testing, a small kernel
broker may bind only `0000:01:00.0` in place of `brcmfmac`, retain kernel
ownership of the shared DART domain, and expose bounded PCI, DMA, interrupt, and
reset operations to the safe Rust driver. `hci_bcm4377` can remain bound to the
Bluetooth function. This is a development backend, not the production hardware
architecture.

Handoff is transactional and supervised locally. A test job quiesces and
unbinds `brcmfmac`, binds the development broker, runs with a hard deadline,
persists its complete report locally, revokes DMA and interrupts, resets when
safe, rebinds `brcmfmac`, and waits for normal connectivity. Process failure,
timeout, or a lost remote session must enter the same restoration path. Reports
are uploaded only after the management connection returns, so test correctness
does not depend on the experimental Wi-Fi path.

Bluetooth host-stack work may similarly use the existing `hci_bcm4377`
transport and an exclusive HCI user channel, but it is secondary to Wi-Fi.

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
host drivers. Development-broker tests can exercise Wi-Fi firmware and protocol
behavior before this production-path milestone.
