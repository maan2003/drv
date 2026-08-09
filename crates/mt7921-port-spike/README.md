# MT7921/MT7922 port spike (not yet a driver)

This crate is an exploratory port of two hardware-independent format seams from
Linux mt76: the 16-byte DMA descriptor construction and the Connac2 RAM firmware
trailer/region parser. It does not access hardware, load firmware, implement
802.11, or claim support for any device. Its purpose is to make a small amount
of source-corresponding code compile while exposing what the current hardware
broker cannot yet express.

The BCM4387C2 first target in `specs/ARCH-asahi-wifi-target.md` is unchanged.

It also contains the first portable SoftMAC seam: `AccessPoint::from_beacon`
ports the behavior of pinned Fuchsia's
`mlme/rust/src/client/convert_beacon.rs::construct_bss_description` without its
FIDL types. It consumes raw beacon/probe-response information elements and
produces the BSSID, SSID, channel, signal, capabilities and security summary
needed by the host. Its main fixture is copied from Fuchsia's corresponding
test. Security AKM classification follows the suite handling in Fuchsia WLAN
common/SME protection code. Fuchsia sources are BSD-3-Clause licensed; the
ported implementation was rewritten against the pinned source rather than
copied with component-runtime dependencies.

The `mt7921-scan` binary is an explicitly temporary Linux SoftMAC adapter. It
converts `iw scan` text into newline-delimited JSON so the portable result
shape and durable physical evidence can be exercised before the VFIO WFDMA/MCU
transport exists. Its output proves real RF enumeration but **does not** prove
userspace firmware initialization, DMA, IRQ, or scan operation.

`mt7921-vfio-read` is the first physical MT7921 transport slice. It validates
the exact no-plastic PCI and subsystem IDs, attaches the VFIO cdev to a fresh
iommufd IOAS, and maps only two 4 KiB BAR pages read-only. Calls can read only
five enum-selected registers: MCU state, host interrupt status, WFDMA global
configuration, PCIe ownership synchronization, and firmware power/readiness.
The offsets come from pinned Linux `mt792x_regs.h` and the fixed map in
`mt7921/pci.c`. It cannot write MMIO, remap arbitrary chip addresses, allocate
DMA, arm an interrupt, or reset the function.

With `--acquire-driver-ownership`, the same binary additionally ports
`__mt792xe_mcu_drv_pmctrl` from pinned Linux `mt792x_core.c`: it writes only
`PCIE_LPCR_HOST_CLR_OWN` to `MT_CONN_ON_LPCTL`, then polls only that register's
`PCIE_LPCR_HOST_OWN_SYNC` bit. It preserves Linux's ten 50 ms attempts and 1 ms
poll tick while adding a 500 ms absolute deadline and rejecting command bits
on readback. Every write, status sample, retry, terminal success, timeout, or
unexpected state is emitted as a structured event. No firmware-ownership or
dynamic L1-remap write is admitted.

The physical MT7961 at `0000:05:00.0` completed this transition on the first
attempt: writing `PCIE_LPCR_HOST_CLR_OWN` produced a zero status response and
driver ownership at 0 ms. The bounded run then observed firmware power set,
N9 readiness clear, and all TX/RX DMA enable/busy bits clear before issuing a
VFIO function reset. The root-only report is
`/var/lib/wifi-driver-lab/reports/20260802T170812Z-0000_05_00.0.log`.

## Inactive WFDMA and firmware prerequisites

`WfdmaRing` ports the inactive mt76 TX-ring invariants without enabling DMA:
descriptor storage must be aligned and wholly below 4 GiB, reset descriptors
are CPU-owned via `DMA_DONE`, enqueue writes a device-owned descriptor before a
release fence and producer-index publication, reclaim requires device
completion and an acquire fence, indices wrap within the allocation, and
teardown resets descriptors before releasing the allocation. The model has no
register or DMA-enable operation.

`Patch` adds bounds-checked parsing of the big-endian Connac2 patch header and
section table consumed by pinned Linux `mt76_connac2_load_patch`.
`mt7921-firmware-inspect` reads and decompresses only the exact installed
MT7961 patch/RAM artifact names, validates their target metadata and every
payload bound, and emits structured metadata. It does not retain, map for DMA,
or send firmware bytes.

`--read-dynamic-identity` admits the minimum dynamic-L1 selector operation used
by pinned Linux `mt7921_reg_map_l1`, but closes over four read-only targets:
`MT_HW_CHIPID`, `MT_HW_REV`, `MT_HW_BOUND`, and `MT_TOP_LPCR_HOST_BAND0`.
It saves the selector, selects only bases `0x7001` and `0x1806`, verifies each
posted selector write, reads only the enum-selected offsets through a separate
read-only window mapping, and restores the original selector on success or
failure. This mode cannot write the dynamic window or request MT_TOP ownership.

`--acquire-top-ownership` separately ports pinned Linux
`mt7921e_driver_own`. It selects only `MT_TOP_LPCR_HOST_BAND0`, writes only
`MT_TOP_LPCR_HOST_DRV_OWN`, and polls `MT_TOP_LPCR_HOST_FW_OWN` clear with a
500 ms hard deadline and 1 ms ticks. Command-bit readback is rejected, every
transition is logged, and the saved remap selector is restored on every exit.

`--program-disabled-fwdl-ring` allocates one anonymous page, maps it through
iommufd at fixed low-32-bit IOVA `0x01000000`, initializes the first 128
descriptors to CPU-owned `DMA_DONE`, and refuses to continue unless WFDMA TX/RX
enable bits and the complete host interrupt-enable register are zero. It then
temporarily programs only firmware-download ring 16's descriptor base, count,
and CPU index; verifies readback including the untouched DMA index; restores
the original ring registers; explicitly unmaps the complete arena; and emits
an event for every step. Restoring those visible resources did not make the
kernel fallback usable in the first physical run, despite a successful script
and supervisor restoration; the machine required a cold power cycle. The mode
therefore now requires VFIO reset capability and issues `VFIO_DEVICE_RESET`
after arena unmap and before returning the function to the supervisor. It
cannot write WFDMA enable, interrupt, or DMA-index registers. Any repeat must
run through the externally renewed `wifi-driver-lab-remote` reboot watchdog.

`reset_wfsys` also ports the device-specific recovery sequence from pinned
Linux `mt792x_wfsys_reset`: clear `WFSYS_SW_RST_B`, hold for 50 ms, set it, and
poll `WFSYS_SW_INIT_DONE` for at most 500 ms. No physical adapter for address
`0x18000140` is admitted yet; deterministic success and timeout behavior must
precede that additional dynamic-L1 write surface.

`--mask-ack-disabled-fwdl` ports the ring-16 subset of pinned Linux interrupt
handling. It refuses active DMA or a nonzero host mask, snapshots status,
writes only the zero mask, acknowledges only `HOST_TX_DONE_INT_STS16` using
W1C semantics, verifies readback without clearing unrelated sources, restores
the zero mask, and performs the mandatory VFIO reset. It does not arm a VFIO
IRQ or enable any device interrupt.

`--stage-disabled-firmware-descriptor` decompresses and bounds-checks the exact
installed MT7961 ROM patch, maps separate one-page descriptor and payload
arenas at fixed low-32-bit IOVAs, and stages only its first 4096-byte raw
`FW_SCATTER` chunk. It first requires TX/RX DMA and every host interrupt to be
disabled. The payload is copied before a release fence publishes one ring-16
descriptor, which is read back and then reset to CPU ownership; the payload is
zeroed, both mappings are explicitly removed, and VFIO reset is mandatory.
No ring register, producer index, DMA-enable bit, interrupt mask, or MCU command
is written, so the device cannot observe the staged descriptor.

The offline patch-protocol slice now derives Linux's Connac2 download mode
from each parsed section security word and encodes the `PATCH_FINISH_REQ` that
must follow all scatter chunks. Unknown encryption modes fail closed. These
helpers are fixture-tested only and are not connected to MMIO or active DMA.
The same pure encoder now includes the terminal `FW_START_REQ` used after the
exact installed RAM regions, rejects an address/option pair other than their
derived `0x00915000`/override values, and emits Linux's required legacy command
queue ID (`0x8000`) in every command TXD.
`firmware_download_mode` separately ports the Connac2 RAM-region feature-byte
translation, including encryption, key index, encryption mode, response, and
optional CR4 working-PDA bits. Address override and non-download remain region
flow controls rather than download-mode bits.

`load_mt7921_firmware` composes those pure pieces behind a typed transport. It
powers the NIC, bounds the download-ready poll, and always attempts release of
an acquired patch semaphore. It initializes and scatters every patch section in completed
chunks of at most 4096 bytes, finishes the patch, downloads only RAM regions,
starts the exact installed image, and bounds the N9-ready poll. Cleanup runs
after success and every injected failure, preserving both primary and cleanup
errors when necessary. Golden-trace and per-operation error-injection tests
cover the transaction. `VfioFirmwareLoader` is the bounded physical adapter: it
uses the fully owned command/FWDL/RX rings, matched MCU responses, modular DIDX
completion, cancellation, IRQ disable, DMA quiescence, and reset-while-pinned
teardown before any mapping is released. `--run-one-shot-fwdl` is the explicit
lab-only entry and must run through `wifi-driver-lab-remote`'s reboot watchdog.
Like pinned Linux, a one-second download-ready timeout is recorded as a warning
and loading continues; N9 readiness remains a terminal 1.5-second timeout. The
offline safety model is stricter than Linux scatter submission: it requires an
explicit completion for every chunk under a three-second deadline. Transport
sequence allocation persists across transactions and skips zero on four-bit
wrap. Fail-closed cleanup after `Ready` is lab transaction policy; Linux keeps
the live device resources instead. The operation stops immediately after clean
N9 readiness; it sends no capability or post-boot configuration command.

A watchdog-guarded physical run completed this boundary for the exact installed
MT7961 artifacts. It downloaded one patch section and four RAM regions in 196
individually completed chunks (795,264 bytes), skipped the CLC region, reached
N9 `Ready`, disabled DMA and PCI bus mastering, reset while all mappings were
pinned, then released every mapping. The lab restored `mt7921e`, iwd, network,
and SSH with `failed=0`. The durable root-only report is
`/var/lib/wifi-driver-lab/reports/20260809T121656Z-0000_05_00.0.log`.

The earlier `--run-one-shot-fwdl` rejection identified that the
global TX-DMA enable can fetch every TX ring, including stale kernel ring bases,
and that raw patch scatter is invalid until the MCU has accepted patch
semaphore and `PATCH_START` commands. Active DMA therefore remains unavailable
until the backend owns or guards every TX ring, resets and verifies every DMA
index, installs a VFIO IRQ before unmasking it, implements the MCU command/RX
response path, and keeps every mapping pinned through quiescence and function
reset. The bounded VFIO adapter now satisfies those gates for this explicit
operation; other commands cannot enter its active DMA path.

`prepare_global_tx_rings` is the deterministic replacement preflight. While TX
DMA and all host interrupts remain disabled, it inventories all 18 hardware TX
ring slots, rejects invalid MMIO or any `CIDX != DIDX`, verifies MT7921 ring
16's pinned-Linux prefetch value `0x03400004`, and replaces every non-target
base with one pinned guard page while assigning separate pinned backing to ring
16. Only after every base/count/CPU index is owned does it issue Linux's global
DTX-index reset and require every DIDX to read zero. Old kernel DMA bases are
never restored. Its fake transport remains the exhaustive failure-path model;
the physical inactive adapter below now covers the successful MMIO path.
Ring 17 now receives its own 256-descriptor MCU-command page rather than guard
backing. `prepare_mcu_rx_ring` separately builds Linux's eight-entry,
2048-byte-buffer pre-firmware response queue with seven device-owned buffers
and one empty slot, using a distinct aligned low-32-bit ring page and 16 KiB
buffer mapping. It rejects overlapping or out-of-range arenas.
`program_disabled_mcu_rx_ring` requires that old ring zero is idle, writes its
owned base/count with both CPU and DMA indices zero, verifies that state, then
publishes the seven receive buffers only after a release fence. RX DMA and its
interrupt remain disabled; the physical adapter is still pending.

`--prepare-owned-global-tx-rings` is the inactive physical adapter for this
preflight. It maps three separate low-32-bit pages filled entirely with
CPU-owned reset descriptors, applies and verifies all 18 ring slots plus the
global DTX reset while DMA and interrupts remain disabled, VFIO-resets while
all pages are still pinned, and only then unmaps them. It cannot enable DMA,
publish a producer index, install an IRQ, or send an MCU command.

The original physical attempt faulted because this operation's WFDMA BAR page
was accidentally mapped read-only before the first ring write. Page access is
now an explicit per-operation contract, and VFIO READ/WRITE/MMAP region flags
are checked before `mmap`. A guarded rerun wrote and read back all 18 owned
rings, reset every DTX index, reset the device while all three IOVAs remained
pinned, and then unmapped them. The root-only report is
`/var/lib/wifi-driver-lab/reports/20260802T165342Z-0000_05_00.0.log`.

`encode_download_command` ports the exact 64-byte legacy Connac2 command TXD
and request bodies for patch-semaphore acquisition, `PATCH_START`,
`TARGET_ADDRESS_LEN`, and `FW_START_REQ`. It rejects sequence zero/outside the four-bit firmware
range, an empty download, and a patch-start address other than MT7961's
`0x00900000`. Encoding these commands is not permission to send them: an owned
MCU TX ring, RX response ring, parsed matching response, VFIO IRQ, and safe
reset-while-pinned teardown must all exist first.
`parse_download_response` bounds the fixed 36-byte Connac2 MCU RX header and
matches the four-bit command sequence before exposing event identifiers; it is
the first pure parser needed by the future owned RX response ring.

`--query-patch-semaphore` is the first bounded active boot-ROM transaction.
It retains all mappings in one operation, replaces all 18 TX and all eight RX
ring slots with pinned userspace backing, and gives RX ring zero seven distinct
2 KiB response buffers. Before DMA it performs the pinned Linux conn-on
ownership and WFSYS reset sequences, installs an eventfd-backed MSI/MSI-X
vector while sources remain masked, disables L0s, acquires MT_TOP ownership,
and selects normal firmware mode. It then enables only RX0 completion, sends
`NIC_POWER_CTRL`, requires firmware-download state, and sends
`PATCH_SEM_CONTROL(GET)`. A response is accepted only from a completed RX
descriptor with a matching sequence and patch-semaphore event ID. Result 2 is
immediately followed by a matched semaphore release; result 1 needs no
release. Every exit masks both interrupt gates, disables and polls both DMA
directions, disables PCI bus mastering, resets through VFIO while all IOVAs
remain pinned, and only then unmaps. This operation does not scatter firmware
or claim N9/NIC capability readiness.

The guarded physical run completed this boot-ROM boundary through a VFIO MSI
vector. WFSYS became ready at 57 ms; the `NIC_POWER_CTRL` response (sequence 1,
event 3) was drained as unrelated; patch semaphore GET returned result 2 on
sequence 2; and the mandatory release returned result 3 on sequence 3. Each
response arrived through RX descriptors 0, 1, and 2 respectively with an
eventfd count, and the run disabled PCI bus mastering, reset while all mappings
remained pinned, and restored `mt7921e`, iwd, network reachability, and SSH.
N9 remained deliberately not ready. The root-only durable report is
`/var/lib/wifi-driver-lab/reports/20260802T174640Z-0000_05_00.0.log`.

`--inventory-vfio-irqs` queries the standard VFIO INTx, MSI, and MSI-X
capabilities without installing or triggering one, rejects modes without
eventfd support, and reports the preferred MSI-X/MSI/INTx choice. Device
interrupt unmasking remains unavailable until that chosen vector is actually
installed and exercised by the deterministic completion path.
`IrqLifecycle` prevents the device source from being enabled before an
eventfd-capable VFIO vector is installed, rejects a zero eventfd counter, and
requires explicit disable after an observed completion. The native backend now
has an unexposed `VfioIrq` owner which creates a nonblocking close-on-exec
eventfd, installs exactly one selected vector with `VFIO_DEVICE_SET_IRQS`,
drains 64-bit counters, explicitly disables the vector, and repeats disable in
`Drop`. Its Linux UAPI layout is tested, but it cannot yet be invoked physically
or unmask a device source.

`--install-disable-vfio-irq` exposes only the source-disabled lifecycle check:
select one eventfd-capable VFIO vector, install it, require its nonblocking
counter to remain empty while the device mask is zero, explicitly disable it,
and VFIO-reset. It never writes the device interrupt mask or enables DMA.

`teardown_pinned_dma` makes reset ordering explicit for the future active path:
mask and disable are attempted, TX busy is polled for at most 100 ms, and VFIO
function reset is issued while every IOVA remains pinned regardless of the poll
result. Mappings are released only after reset succeeds. A reset failure never
calls unmap, so the external reboot watchdog remains the containment boundary.
The native backend has an active-operation signal guard for
SIGHUP, SIGINT, and SIGTERM which performs only an atomic cancellation request
in the handler and restores previous handlers on drop. The future physical
control loop checks that request throughout command, scatter, and readiness
waits and enters the same reset-while-pinned containment path.

## Verified against pinned Linux 7.2-rc5 source

All paths below are relative to
`reference/linux-7.2-rc5/drivers/net/wireless/mediatek/mt76/`.

* `dma.h` defines `struct mt76_desc` and its length/last-section/done bits.
  `dma.c:mt76_dma_add_buf` fills up to two buffers per descriptor and
  `dma.c:mt76_dma_queue_reset` marks cleared descriptors DMA-done. The Rust
  `DmaDescriptor` ports only those word layouts. `mt7921/pci.c:mt7921_pci_probe`
  calls `dma_set_mask(..., DMA_BIT_MASK(32))`, so the Rust constructor rejects
  IOVAs above 32 bits rather than truncating them.
* `mt76_connac_mcu.h` defines packed `mt76_connac2_fw_trailer` (36 bytes) and
  `mt76_connac2_fw_region` (40 bytes). The region records precede the final
  trailer; their payloads are concatenated at the beginning of the image.
  `mt76_connac_mcu.c:mt76_connac_mcu_send_ram_firmware` walks those records,
  skips `FW_FEATURE_NON_DL`, initializes each download, sends `FW_SCATTER`, and
  starts firmware. `mt7921/mcu.c:mt7921_load_clc` walks the same layout to find
  a non-download `FW_TYPE_CLC` region. `Firmware` ports the common, bounded
  layout parsing only; it deliberately performs no MCU operation.
* PCI IDs `14c3:7961` and `14c3:7922` (plus the aliases in the table) select
  MT7921 and MT7922 firmware in `mt7921/pci.c:mt7921_pci_device_table`.
  Firmware names are `mediatek/WIFI_RAM_CODE_MT7961_1.bin`,
  `mediatek/WIFI_MT7961_patch_mcu_1_2_hdr.bin`,
  `mediatek/WIFI_RAM_CODE_MT7922_1.bin`, and
  `mediatek/WIFI_MT7922_patch_mcu_1_1_hdr.bin` in `mt792x.h`.

The Rust parser validates that the complete region table exists and that the
sum of payload lengths does not overlap metadata. This is memory-safe input
validation around the documented Linux layout, not a claim that every semantic
firmware constraint or CRC is understood. In particular, CRC validation and
the separate big-endian ROM-patch format remain unported.

## Responsibility boundary that did not port

The rest is not device-independent glue:

* **PCI and power:** `mt7921_pci_probe` enables the function and memory space,
  enables bus mastering, requests one IRQ vector of any PCI type, selects a
  32-bit DMA mask, maps BAR 0, optionally changes ASPM, negotiates firmware and
  driver ownership, reads chip revision, performs Wi-Fi-subsystem reset, and
  enables PCI MAC interrupts. `mt7921/pci_mcu.c:mt7921e_driver_own` and
  `mt792x_core.c:mt792xe_mcu_{drv,fw}_pmctrl` poll ownership registers.
  Suspend/resume in `mt7921/pci.c` coordinates MCU HIF suspend/deep sleep, DMA
  idle/disable, interrupt synchronization, ownership, and WPDMA reinit.
* **DMA and interrupts:** `mt7921/pci.c:mt7921_dma_init` allocates coherent
  descriptor rings plus mapped RX/TX buffers, programs base/count/CPU indices,
  and enables WFDMA. The configured sizes include 2048 data TX descriptors,
  256 MCU TX, 128 firmware-download TX, 1536 data RX, and device-dependent MCU
  RX rings. `mt792x_dma.c:mt792x_irq_handler` masks the device interrupt and
  schedules a tasklet; `mt792x_irq_tasklet` reads and acknowledges causes,
  disables individual causes, and schedules TX/RX NAPI polls. Poll completion
  re-enables a cause. Correctness depends on coherent visibility, ordering
  descriptor writes before producer-index MMIO, and draining rings before
  re-enabling interrupts.
* **Firmware and reset:** `mt792x_core.c:mt792x_load_firmware` restarts the MCU,
  waits for power, loads the ROM patch under a firmware semaphore, downloads
  RAM regions over the firmware DMA queue, starts firmware, and waits for N9
  ready. `mt7921/pci_mac.c:mt7921e_mac_reset` quiesces interrupts/work/NAPI,
  discards pending MCU state and tokens, resets WPDMA, reloads firmware and
  EEPROM state, reinitializes MAC state, and restarts the PHY. A generic PCI
  function reset is not equivalent to either WPDMA or Wi-Fi-subsystem reset.
* **SoftMAC:** this Linux driver is not an Ethernet FullMAC boundary.
  `mt7921/main.c:mt7921_ops` implements `ieee80211_ops` for interface/station
  lifecycle, keys, BSS changes, AMPDU, scans, channel contexts/switches, remain
  on channel, suspend/WoWLAN, SAR/regulatory handling, and TX queue wakeup.
  `mt76/mac80211.c`, `tx.c`, `agg-rx.c`, and `channel.c` supply shared mac80211
  station/WCID, TXQ, aggregation/reorder, channel, survey, and status behavior.
  skb ownership, NAPI, workqueues/tasklets/timers, cfg80211 regulatory state,
  and mac80211 callbacks must be replaced by an explicit portable SoftMAC
  service contract and scheduler; none belongs in this format crate.

This is an explicit **SoftMAC result**, despite substantial MCU and hardware
offload. The source exposes more than 50 `ieee80211_ops` callbacks, and TX still
arrives as mac80211 frames through `mt792x_tx`/`wake_tx_queue`, while RX is
reported through mt76/mac80211 status and reorder paths. Firmware commands
offload operations such as scanning, key/station programming, beaconing, and
aggregation setup; they do not expose the Ethernet-oriented FullMAC contract
used by the BCM4387 product direction. A userspace port would therefore need to
own at least: 802.11 frame TX/RX and status, authentication/association and
management exchange, station/BSS/key state, TXQ scheduling and rate/status
feedback, AMPDU reorder/BA lifecycle, scan/ROC/channel-context state machines,
regulatory/SAR/channel selection, power-save/WoWLAN, and timers/concurrency.
That is a portable SoftMAC stack plus the mt76 hardware transport, not merely a
replacement for Linux PCI/DMA calls. This makes MT7921/MT7922 useful here as a
broker/interface stress test, but a poor fit for the current FullMAC milestone.

## Concrete gaps in `wit/hardware.wit`

The present broker is enough to allocate an arena, copy bytes into it, obtain
an IOVA, access allowlisted BAR words, wait for a notification, and request an
opaque reset. It is not yet a suitable MT7921 PCI transport because it lacks:

1. a way to constrain DMA allocation to the device's 32-bit address mask;
2. explicit DMA publish/acquire or cache-maintenance operations and ordering
   relative to MMIO producer/consumer-index writes (required on non-coherent
   hosts; copied `read`/`write` alone does not state this contract);
3. firmware/artifact resources, so neither named RAM firmware nor ROM patch can
   be supplied to a no-WASI component;
4. PCI identity/revision/configuration capabilities for matching the variant,
   enabling memory decoding and bus mastering, selecting IRQ mode, controlling
   ASPM/power/wakeup, or querying reset support;
5. reset scopes that distinguish PCI function reset, WFSYS reset, WPDMA reset,
   and recovery which preserves/rebuilds DMA mappings;
6. an interrupt mask/ack/drain/re-enable contract. BAR operations can perform
   device masking and acknowledgement, but `interrupt.wait-until` does not say
   how a shared/level interrupt remains quiesced or how notifications interact
   with those MMIO writes;
7. cancellation/concurrent waiting suitable for integrating IRQ, MCU timeout,
   power, and SoftMAC timers into one portable event loop.

The broker also chooses allowlisted BAR regions, but the needed translated
register windows in `mt7921/pci.c:__mt7921_reg_addr` and
`mt7921_reg_map_l1` cannot be confirmed until the actual device and BAR policy
are inventoried.

## Verified no-plastic hardware boundary

`no-plastic` exposes MediaTek `14c3:7961` with subsystem `1a3b:4680` at
`0000:05:00.0`, normally bound to `mt7921e`. It is the sole member of IOMMU
group 19 and advertises function-level and bus reset methods. Bluetooth is not
in that PCI group. The native backend has attached its VFIO cdev to iommufd,
mapped and unmapped one private page at IOVA `0x0100_0000`, explicitly destroyed
the IOAS, and returned the function to `mt7921e`; it did not map BARs or arm an
interrupt. iwd reconnects after each handoff, although the kernel interface name
advances from `wlan0` to `wlanN` after reprobe.

BAR 0 policy and translation, interrupt topology, DMA coherence, firmware and
calibration sources, RF-kill/wakeup wiring, ASPM quirks, and device-specific
reset behavior remain unverified.
