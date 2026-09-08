# Linux mt76 shared core

This GPL-2.0-only, `no_std` crate contains hardware primitives shared by Linux
mt76 devices. Its source authority is Linux **v7.1.5** under
`drivers/net/wireless/mediatek/mt76`: `dma.[ch]`, `pci.c`, `mmio.c`, and the
reached `mt76_connac*` formats. The declaration-level inventory remains in the
MT7921 consumer's `SOURCE-MAP.md` and `SOURCE-ITEMS.tsv`.

Device DMA-mask policy, register maps, firmware/NIC/channel decisions, and
device sequencing do not belong here. In particular, generic mt76 descriptors
retain their Linux 36-bit address fields; MT7921's PCI 32-bit mask is enforced
by `mt7921-core` before descriptor construction.
