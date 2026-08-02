# MT7921/MT7922 port spike (not a driver)

This crate is an exploratory port of two hardware-independent format seams from
Linux mt76: the 16-byte DMA descriptor construction and the Connac2 RAM firmware
trailer/region parser. It does not access hardware, load firmware, implement
802.11, or claim support for any device. Its purpose is to make a small amount
of source-corresponding code compile while exposing what the current hardware
broker cannot yet express.

The BCM4387C2 first target in `specs/ARCH-asahi-wifi-target.md` is unchanged.

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
