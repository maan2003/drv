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
