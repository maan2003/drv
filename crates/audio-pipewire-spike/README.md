# PipeWire audio spike

This slice registers a hardware-free virtual playback device behind a narrow
host adaptation of Fuchsia's Audio Device Registry and ring-buffer contracts.
The registered device owns its identity, S16LE/48 kHz/stereo format and
ring-buffer endpoint; the PipeWire frontend derives its Node/Port globals,
properties and `SPA_PARAM_EnumFormat` object from that record. The endpoint's
consumed-byte to frame-position mapping directly executes the unchanged pinned
Fuchsia audio `TimelineFunction`/`TimelineRate` implementation packaged in
`../fuchsia-audio-timeline`; it is not a retyped local equivalent. Run it with:

```sh
cargo run -p drv-audio-pipewire-spike
cargo test -p drv-audio-pipewire-spike
```

The pristine Fuchsia ADR/device/ring-buffer sources, hashes, host adaptation,
and exact ownership mapping are recorded in [PROVENANCE.md](PROVENANCE.md) and
[SOURCE-MAP.md](SOURCE-MAP.md). The Fuchsia implementation cannot be compiled
on this host without Zircon and generated FIDL bindings, so it is preserved
unchanged while the private Rust adapter implements only this bounded contract.
The protocol frontend does not leak into playback state. This milestone neither
accesses ALSA nor physical hardware.

Every negotiated S16 sample also runs through the unchanged Fuchsia processing
library's `DbToScale` and `ApplyGain<GainType::kNonUnity>` at a fixed
-6.0206003 dB, packaged in `../fuchsia-audio-processing`. Only the C ABI and
S16 representation conversion are host-owned; no pinned processing source is
patched. A post-gain sample checksum makes the processing observable in
stock-client probes rather than a decorative dependency.

`serve-two` accepts two ordinary stock playback clients through independent
native-protocol transports and shared buffer pools. Their equal fixed-format
chunks are accumulated through the unchanged pinned Fuchsia `ChannelStrip`
planar processing component and sampler `MixSample` accumulation primitive
before entering the gain and timeline endpoint.
For two 480-frame streams containing `{10000, 2000}` and `{4000, 6000}`, both
stock clients and the server exit successfully and the mixed post-gain checksum
is 5,280,000 at the registered ring-buffer frame position 480.

## Rust PipeWire libraries

`pipewire` (`pipewire-rs`) is a safe wrapper over the installed C
`libpipewire`; it is useful for writing ordinary clients and plugins, but is not
a native Rust server implementation. `pipewire-native` implements the native
protocol in Rust but describes itself as a client library: its protocol
dispatch and proxies are client-oriented, and it has no complete server-side
registry or processing/data-plane implementation. This crate therefore pins
only `pipewire-native-spa` 0.1.4 for its wire-compatible POD codec and keeps it
on the compatibility side of the PCM boundary.

## Precisely supported and missing

Supported now: deterministic virtual PCM consumption and the complete fixed
`EnumFormat` POD that a playback port must advertise (media type/subtype,
sample format, rate, channels, and channel positions). Tests parse the object
and independently check its native object type and parameter ID.

The `serve` mode binds the standard `$PIPEWIRE_RUNTIME_DIR/pipewire-0` Unix
socket, handles native `Core.GetRegistry`, and advertises the virtual sink Node
and its input Port with `Registry.Global`. It also handles the mandatory Hello,
client-property, and Sync/Done bootstrap around discovery. The bounded probe
serves one client and exits after that client's post-sync activity becomes idle.
No alternate application protocol is exposed.

For example, start `PIPEWIRE_RUNTIME_DIR=/tmp/drv-pw cargo run -p
drv-audio-pipewire-spike -- serve`, then use the unmodified `pw-cli ls Node` or
`pw-cli info 2` or `pw-cli enum-params 3 EnumFormat` with the same environment.

An unmodified client can connect, discover, and bind both globals. `pw-cli info
2`, `pw-cli info 3`, and `pw-cli enum-params 2 EnumFormat` (or object 3) receive
standard Node/Port Info and the S16LE/48 kHz/stereo format POD. The server also
accepts the stock playback client's standard
`Core.CreateObject("client-node")` and `ClientNode.GetNode`, including bounded
properties, versions, and collision-free client proxy IDs. A stock `pw-cat`
playback now completes `ClientNode.Update` and `ClientNode.PortUpdate`. The
server binds the exported stream, imports its stock PortConfig, selects a
playback port, and sends standard `Core.AddMem` and `ClientNode.Transport`
events carrying a shared activation memfd and two eventfds. It accepts the
stock port's advertised formats, selects the fixed S16LE/48 kHz/stereo Format,
then exports two real memfd-backed PCM buffer descriptors with
`ClientNode.PortUseBuffers`. The bounded spike also supplies shared
`SPA_IO_Buffers`, starts and activates the client node, recycles its buffers,
and writes each finite PCM chunk into the Fuchsia TimelineFunction-backed
endpoint. A 1,920-byte stock `pw-cat` playback exits successfully with a
reported position of 480 frames and a checksum of the post-gain samples.
Production graph policy, realtime pacing, multi-node clock/quantum coordination,
and long-running playback remain outside this spike.

Stock `pw-cli ls Node` reports registry-projected `object.serial = "2"`,
`device.api = "fuchsia.audio.device"`, `node.name =
"drv.adr-virtual-sink"`, and the registered S16LE/48000/stereo properties. Two
stock `pw-cat` clients writing the example streams above advance the same
registered ring-buffer endpoint to frame 480 and produce checksum 5,280,000.
