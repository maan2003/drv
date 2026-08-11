# MT7921 hardware core

This `no_std` crate owns MT7921 device policy and preserves the existing public
compatibility facade for the userspace driver: the MT7921 PCI 32-bit DMA-mask
gate and ring allocation, WFDMA sequencing, register/ownership/reset state,
firmware/NIC/channel policy, BSS/WCID/key commands, MT7921 TX/RX handling, and
ordered teardown. Shared Linux mt76 descriptor/ring-format primitives, Connac
image and MCU envelope formats, MMIO semantics, and PCI capability policy live
in the dependency crate `mt76-core` and are re-exported here where compatibility
requires it. Producer publication and ring ownership remain device-specific.

## Pinned authority

The source authority is the Linux **v7.1.5** tree at
`reference/linux-7.1.5/drivers/net/wireless/mediatek/mt76`. The complete MT7921
directory and the reached mt76/Connac files were compared with the previously
pinned `e8efe09d4f378992c890d181d65e2ed8d8cb1194` snapshot and are byte-identical;
there is therefore no deployed-firmware/source semantic change in this
extraction. `SOURCE-MAP.md` and `SOURCE-ITEMS.tsv` remain the symbol-level
provenance inventory.

The operation groups follow Linux v7.1.5 boundaries (`dma`, `pci`/`pci_mac`,
`mcu`/`pci_mcu`, `mac`, `init`/`main`) in their names and call order. Rust trait
boundaries replace kernel allocation, MMIO, clocks, workqueues, and bus access;
they do not reorder hardware effects.

Excluded ownership: Fuchsia SME/MLME/SoftMAC policy, credentials, CLI/lab
orchestration, Netstack, and Linux mac80211/cfg80211 protocol state. Generic
VFIO/iommufd memory and interrupt ownership lives in `userspace-vfio`.
