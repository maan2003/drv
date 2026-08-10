# AMD HDA source map

| Spike behavior | Direct Fuchsia reference | Host adaptation / other source |
| --- | --- | --- |
| HDA reset, CORB/RIRB setup and command flow | `intel-hda/controller/intel-hda-controller-init.cc`, `intel-hda-irq.cc`; `drivers/lib/intel-hda/include/intel-hda/utils/intel-hda-registers.h` | Rust MMIO and iommufd/VFIO boundary in this crate |
| Codec parameter and widget enumeration | `drivers/lib/intel-hda/{utils/codec-caps.cc,include/intel-hda/utils/codec-commands.h}` | Rust verb encoder/parser in this crate |
| Realtek codec routing | `intel-hda/codecs/realtek/{realtek-codec.cc,realtek-stream.cc,utils.h}` | ALC256 ASUS subsystem topology is explicit host policy because upstream Fuchsia has no matching machine entry |
| AMD stream position | none | Linux `sound/pci/hda/hda_controller.c` FIFO/LPIB handling, used only by a later PCM milestone |

The host code is an explicit `1022:15e3` / `1043:1513` spike, not a new public
audio or general VFIO API. Its eventual output endpoint must remain behind the
existing Audio Device Registry/ring-buffer boundary in `audio-pipewire-spike`.

The bounded playback path directly follows Fuchsia `intel-hda-stream.cc` for
stream reset, format/BDL programming, RUN/interrupt control, and stop ordering.
Its all-exit quiesce preserves that stop/reset ordering and the Realtek mute,
pin-control, and EAPD verbs even when VFIO MMIO or eventfd access fails; the
failure-path structure is local Rust code rather than copied upstream code.
The ALC256 DAC `0x02`, speaker pin `0x14`, headphone pin `0x21`, connection,
amp-mute, pin-control, and EAPD verbs are isolated host topology policy. PCM
enters through the existing private ADR-compatible `PlaybackEndpoint` adapter.
