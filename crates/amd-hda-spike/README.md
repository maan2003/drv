# AMD HDA built-in speaker spike

This explicit no-plastic spike dynamically discovers exactly one PCI audio-class function with AMD
`1022:15e3` subsystem `1043:1513`, isolated IOMMU group 27, and the ALC256
codec `10ec:0256` at address 0. It is not a general VFIO or public audio API.
The host adaptation establishes D0, memory decoding, bus mastering, one MSI
vector, a 40-bit-safe low IOVA, the 32 KiB BAR, HDA GCTL/stream reset,
CORB/RIRB, and codec/widget enumeration.

Run `sudo ./crates/amd-hda-spike/run-physical`. The wrapper discovers the current BDF and IOMMU group, then validates every ID
and exact group membership *before* unbinding, so it cannot claim the Wi-Fi
function or another group member. It builds before unbinding, installs an
independent local recovery/reboot watchdog, and restores `snd_hda_intel` on
all ordinary exits. This HDA function has no FLR/sysfs reset; a reboot can be
required after a controller wedge.

The first milestone is deliberately enumeration-only. It does not configure a
converter, pin, stream BDL, or EAPD and therefore cannot energize speakers.
The later playback boundary remains the Fuchsia ADR/ring-buffer contract in
`audio-pipewire-spike`; no separate audio API is introduced here.

## Verified no-plastic enumeration

On boot `7d8f0e8d-aad0-455c-b91f-94295d8cdbd8`, the guarded remote run selected
only `0000:67:00.6` (`1022:15e3`, subsystem `1043:1513`) after correlating it
to `/proc/asound/card2/codec#0` (`Realtek ALC256`, `10ec:0256`). The current
IOMMU group 27 contained only that function; the two ATI HDMI HDA functions
were inventoried and rejected.

The run attached the VFIO cdev to a fresh iommufd IOAS before querying device
capabilities, established the 32 KiB BAR, D0/BME, low 40-bit-safe DMA mapping
and sole MSI vector, completed stream/GCTL reset and CORB/RIRB setup, and read
codec revision `00100002` plus widgets `0x02` through `0x24`. In particular,
DAC `0x02`, speaker pin `0x14`, and headphone pin `0x21` were present. The
wrapper then verified native restoration to `snd_hda_intel`; the ALC256 proc
node returned, `wlan0` remained the active route, and the kernel log showed no
IOMMU fault. No converter, BDL playback stream, pin output, or EAPD was enabled.
