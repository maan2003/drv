# AMD HDA built-in speaker spike

## Phase 1A real-time daemon contract

`amd-hda-rt-daemon` now contains the hardware-disabled Phase 1A fixed executor
and continuous eight-entry BDL model. Its stereo S16LE/48 kHz quantum is 480
frames (10 ms); all audio and scratch cells are preallocated, and the only DMA
input type is produced by the mandatory DC-blocking and peak-limiting stage.
The physical runner below remains the old bounded spike and is not connected to
this executor until the Phase 1B safety gate opens.

Run the complete non-hardware gate with:

```sh
./crates/amd-hda-spike/check-phase1a
```

The gate also smoke-tests the persistent, hardware-disabled process-liveness
harness exposed as `amd-hda-rt-daemon --virtual-daemon`; it never opens VFIO or
an audio device. Its timer emulates the future HDA completion wait and is not
part of the zero-syscall executor audit. Readiness logging runs on the main
thread, not the executor thread.

The gate covers BDL wrap and progress accounting, missing IOC/LPIB and stream
faults, starvation/XRUN accounting, watchdog mute/park ordering, exact Fuchsia
gain and timeline results, 10,000 allocation-audited executor quanta, forbidden
Rust RT surfaces, and disassembly/import audits of the C++ processing and
timeline FFI. The 44.1 kHz resampler remains allocating and is deliberately
absent from the single-clock Phase 1 graph.

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

## Verified bounded speaker playback

On the same no-plastic boot, no headphone jack was sensed, so the guarded
playback operation selected the fallback speaker route DAC `0x02` to fixed pin
`0x14`. It staged one 40 ms, 48 kHz stereo S16LE period through the existing
`PlaybackEndpoint`/ADR PCM shape, containing a 440 Hz sine at only 256/32767
peak, and additionally selected an output-amplifier step approximately 36 dB
below 0 dB before unmuting. The pin was muted while configured; EAPD was enabled
only for the bounded run.

Output stream descriptor 4 advanced from LPIB 0 to 7576 and, after draining all
codec-command events before RUN, the VFIO MSI eventfd reported exactly one IOC interrupt. The driver then muted the codec, stopped and reset the
stream, disconnected converter and pin, disabled EAPD, and the wrapper verified
`snd_hda_intel` restoration. `wlan0` remained the active default route and the
kernel reported no IOMMU, FIFO, or descriptor fault. The private host adapter
implements the already-present `drv_audio_pipewire_spike::PlaybackEndpoint`;
this adds no public audio API.

## Verified stock PipeWire to speaker slice

The explicit `DRV_HDA_PHYSICAL_DAEMON=1` runner mode holds the proven VFIO HDA
controller behind a private implementation of the existing `PlaybackEndpoint`.
The ordinary audio daemon remains virtual by default. On no-plastic, an
untargeted stock PipeWire 1.6.6 `pw-cat` played a 480-frame S16LE/48 kHz/stereo
pattern through the persistent ADR device and exited 0 with empty stderr. The
ADR timeline advanced to 960 frames (the pattern plus the client's final silent
drain quantum); both physical periods completed on speaker stream 4 with LPIB
advancement and exactly one post-command-drain IOC MSI each. The watchdog then
restored `snd_hda_intel` and ALC256, while `wlan0` stayed the active route and
no IOMMU, FIFO, or descriptor fault appeared.

## Sustained playback and restart evidence

On no-plastic boot `0cbedbab-c8f7-4d40-8c4b-ce26a3662011`, one guarded physical
daemon accepted two sequential, untargeted stock `pw-cat` sessions, each with
one second (48,000 frames) of S16LE/48 kHz/stereo PCM. Both clients exited 0
with empty stderr. Including each client's final silent drain quantum, the same
ADR and physical endpoint advanced to 96,960 frames across 202 BDL periods,
proving client stop and restart without rebinding the device.

At the 200-period checkpoint the backend reported 96,000 frames over 2,549 ms,
LPIB `0..1856`, 200 IOC MSI events, and zero FIFO/descriptor underruns. Every
period independently required LPIB movement and a post-command-drain IOC. The
watchdog finally restored `snd_hda_intel` and the
ALC256 proc node; `wlan0` remained the active route and no IOMMU, FIFO, or
descriptor fault was logged.

The persistent controller now makes that ownership model explicit in the
driver library. VFIO, DMA, MSI, CORB/RIRB, codec identity, amplifier
capabilities, and the selected route are initialized once for the daemon.
Completed periods enter a muted, stopped idle state without resetting the
stream, disconnecting the converter, disabling EAPD, or releasing the device.
Headphone presence is checked before each period; a change mutes and disables
the old pin before configuring the new one. Gain steps are bounded by the
ALC256-reported capability. Transport failures mute and reset the stream, while
service shutdown additionally disconnects the converter, disables the pin and
EAPD, powers down the codec, and only then lets the development wrapper restore
`snd_hda_intel`.
