# Testing Plan

## Goals

Most development must run without root or hardware, use deterministic time, and
finish quickly. Cover component contracts, hostile devices, Wi-Fi behavior,
Linux VFIO mechanics, networking, and recovery separately.
No simulator is evidence that BCM4387 hardware works; physical tests calibrate
models and remain release gates.

## Test Boundaries

1. **Pure logic:** parse firmware, NVRAM, rings, events, and packets as byte
   slices. Use examples, property tests, mutation, and coverage-guided fuzzing.
2. **Component contract:** run the production Wasm component against an in-memory
   broker. Give it only virtual time, explicit artifacts, typed handles, and fuel.
3. **BCM model:** model BAR state, DMA rings, doorbells, interrupts, firmware
   commands, and reset. Start scripted, then make stateful assertions independent
   of the driver implementation.
4. **VFIO mechanism:** run the native broker in a VM and on lab hardware.
5. **Wi-Fi behavior:** connect a test-only controller backend to hwsim/wmediumd.
6. **Network behavior:** test IP and transport independently over a packet link.

The same Wasm binary runs against both brokers; selection occurs below its interface.

## Deterministic Device Harness

Every broker operation is recordable as a versioned event: capability creation,
BAR access, DMA allocation/read/write, interrupt, clock advance, reset, and
artifact hash. Replays reject unexpected operations rather than returning zeros.
DMA buffers use guard regions and generation-tagged handles.

Fault scripts cover malformed lengths and indices, device writes racing reads,
missing/duplicate/reordered interrupts, interrupt storms, partial firmware boot,
timeouts, reset failure, stale handles, worker traps, exhausted fuel, and device
DMA attempts outside mapped ranges. Restart must revoke mappings and recreate
zeroed state before another worker receives the device.

## Cuttlefish and Virtual Wi-Fi

Cuttlefish transports `mac80211_hwsim` messages over virtio device ID 29 to a
vhost-user backend in wmediumd. Wmediumd supplies loss/delay/SNR modeling, PCAP,
multiple stations, and Cuttlefish can run an OpenWRT AP. This is a SoftMAC model,
not Broadcom PCIe/FullMAC hardware, so it cannot validate firmware boot, `msgbuf`,
DMA layout, or reset.

Add a test-only backend implementing our Wi-Fi controller contract through
Linux hwsim. Use it to test scan, WPA2/WPA3 association, bad credentials,
disconnect/reconnect, AP disappearance, weak signal, roaming, and concurrent
traffic. Start with hwsim plus hostapd/wmediumd; adopt full Cuttlefish when its
OpenWRT and environment-control orchestration saves more code than it adds.
Reuse selected hostap `tests/hwsim` scenarios as behavioral cases, not internal
APIs. Production components never depend on nl80211, mac80211, or Cuttlefish.

## VFIO and IOMMU Tests

Use QEMU's `edu` PCI device with a virtual IOMMU to exercise BARs, INTx/MSI, DMA,
binding, mapping, teardown, and broker restart. It tests mechanism, not Wi-Fi.
Later, a separate-process `vfio-user` BCM model can expose arbitrary PCI regions,
interrupts, and malicious DMA while keeping the model outside the VMM.

Run upstream Linux iommufd/VFIO selftests on kernels we support. On `new-plastic`,
repeat broker tests against the MediaTek function and verify IOMMU faults and
recovery. On `m2sh`, run only scheduled BCM boot/scan/association tests and record
broker traces plus firmware console output. Traces bootstrap the model but never
replace generated legal and illegal variations.

## IP and Socket Tests

Connect two stack instances with an in-memory link supporting loss, duplication,
reordering, corruption, MTU changes, and virtual time. Test ARP/NDP, IPv4/IPv6,
fragmentation, ICMP, UDP, TCP state transitions, retransmission, flow control,
and malformed packets. Compare selected results with a mature reference stack.

Adapt packetdrill cases first to the typed application API, then run them through
the host socket adapter to cover file descriptors, blocking, polling, inheritance,
descriptor passing, cancellation, and service restart. Keep TLS/SSH keys and
plaintext outside the network service in all end-to-end tests.

## Execution Tiers

- **Every change:** format, lint, unit/property and Wasm contract tests, deterministic BCM scenarios, and fuzz regressions.
- **Nightly:** longer fuzzing, randomized state machines, QEMU `edu`, network conformance, hwsim/wmediumd, and restart loops.
- **Lab:** physical VFIO/IOMMU tests on `new-plastic` after inventory and isolation.
- **Release/manual:** BCM4387 cold boot, reset, scan, WPA, traffic, worker kill, and host-driver restoration on `m2sh`.

## Initial Implementation Order

1. Freeze the first component interface and implement the deterministic broker.
2. Build a probe component covering DMA, interrupt, timeout, and reset semantics.
3. Add scripted BCM boot/ring scenarios and adversarial variants.
4. Add QEMU `edu`; do not wait for Cuttlefish or physical access.
5. Add hwsim/wmediumd when the controller contract can scan and associate.
6. Capture Asahi traces and correct model assumptions on first hardware access.

## References

- [Cuttlefish Wi-Fi](https://source.android.com/docs/devices/cuttlefish/wifi) and [mac80211_hwsim](https://wireless.docs.kernel.org/en/latest/en/users/drivers/mac80211_hwsim.html)
- [QEMU edu](https://www.qemu.org/docs/master/specs/edu.html), [vfio-user](https://www.qemu.org/docs/master/system/devices/vfio-user.html), and [packetdrill](https://github.com/google/packetdrill)
