# MT7921 hardware core

This `no_std` crate owns the hardware-facing MT76/MT7921 behavior used by the
userspace driver: DMA descriptors and rings, firmware/MCU encodings and
completions, WFDMA sequencing, register/ownership/reset state, channel-domain
commands, BSS/WCID/key commands, management/data TX and RX parsing, and ordered
teardown.

## Pinned authority

The source authority is the Linux **v7.1.5** tree at
`reference/linux-7.1.5/drivers/net/wireless/mediatek/mt76`. The complete MT7921
directory and the reached mt76/Connac files were compared with the previously
pinned `e8efe09d4f378992c890d181d65e2ed8d8cb1194` snapshot and are byte-identical;
there is therefore no deployed-firmware/source semantic change in this
extraction. `SOURCE-MAP.md` and `SOURCE-ITEMS.tsv` remain the symbol-level
provenance inventory.

The operation groups follow Linux v7.1 boundaries (`dma`, `pci`/`pci_mac`,
`mcu`/`pci_mcu`, `mac`, `init`/`main`) in their names and call order. Rust trait
boundaries replace kernel allocation, MMIO, clocks, workqueues, and bus access;
they do not reorder hardware effects.

Excluded ownership: Fuchsia SME/MLME/SoftMAC policy, credentials, CLI/lab
orchestration, Netstack, and Linux mac80211/cfg80211 protocol state. Generic
VFIO/iommufd memory and interrupt ownership lives in `userspace-vfio`.
Device-neutral bounded completion, publication ownership, IRQ lifecycle, and
structured transcript primitives live in `driver-runtime`; this crate retains
the MT7921 register, command, firmware-image, and descriptor protocols.
