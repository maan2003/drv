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
PIPEWIRE_RUNTIME_DIR=/tmp/drv-pw cargo run -p drv-audio-pipewire-spike -- daemon
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
Production graph policy and multi-node clock/quantum coordination remain
outside this spike.

Stock `pw-cli ls Node` reports registry-projected `object.serial = "2"`,
`device.api = "fuchsia.audio.device"`, `node.name =
"drv.adr-virtual-sink"`, and the registered S16LE/48000/stereo properties. Two
stock `pw-cat` clients writing the example streams above advance the same
registered ring-buffer endpoint to frame 480 and produce checksum 5,280,000.

## Persistent daemon loop

`daemon` keeps the native `pipewire-0` socket and the single registered ADR
device alive across client disconnects. Each connection has independent native
protocol, activation and shared-buffer state. Every produced PipeWire buffer is
consumed, recycled, and sent as a separate quantum to the one registry-owned
ring-buffer worker; a stream is not retained as one completed PCM blob.
Non-overlapping streams advance separately. While two streams are active, their
queued quantum slices are incrementally accumulated through the pinned Fuchsia
sampler before each result is written to the ADR ring. Discovery-only clients
never mutate ring state, and malformed or disconnected clients do not stop the
listener.

A stock PipeWire 1.6.6 probe ran `pw-cli ls Node` concurrently with playback,
then two sequential stock `pw-cat` sessions followed by two overlapping stock
`pw-cat` sessions. The same daemon reported monotonically increasing positions
480, 960 and 1440, with cumulative post-Fuchsia-processing checksums 2,880,000,
5,280,000 and 10,560,000. All five clients exited successfully with empty
stderr; the daemon remained alive until the probe explicitly terminated it.

A three-second stock `pw-cat` stream (576,000 bytes, far larger than the 8 KiB
shared-buffer data area) produced 300 patterned 480-frame quanta plus the stock
client's final 480-frame silent drain quantum. The daemon reported 301 monotonic
ring writes and final position 144,480 with exact patterned checksum
864,000,000. `pw-cli` discovery succeeded while this stream was active and all
stderr was empty. Two concurrent three-second streams produced the same 301
output quanta through incremental Fuchsia mixing, ending at position 144,480
and exact checksum 1,584,000,000.

## Standard default sink

The persistent daemon advertises a standard version-3 Metadata global named
`default`. Binding it emits `default.audio.sink` with the SPA JSON value
`{"name":"drv.adr-virtual-sink"}`. It also advertises and implements the
version-3 `client-node` Factory Info corresponding to the existing standard
`Core.CreateObject("client-node")` path. Registry binds receive standard
`Core.BoundId`, and the initial Hello receives Core Info with the fixed clock
rate and quantum. These additions keep longer-lived WirePlumber-style object
managers bound instead of relying on the old discovery timeout.

With no `--target`, an unmodified PipeWire 1.6.6 `pw-cat` played the full
three-second pattern through the same multi-buffer ADR path. Concurrent
`pw-cli ls Metadata` reported object 4 as `metadata.name = "default"`, while
stock WirePlumber 0.5.14 `wpctl inspect 2` bound the node and displayed all ADR
sink properties. Playback again produced 301 quantum writes and ended at exact
position 144,480 and checksum 864,000,000; client and server stderr were empty
(apart from wpctl's expected host RTKit warnings).

## Opt-in Nix package and user service

The flake package `audio-pipewire-daemon` installs the release binary, the
`drv-audio-pipewire-private` lifecycle launcher, and an opt-in systemd user
unit. It does not enable itself or replace the host `pipewire.service`. The
launcher exclusively uses `$XDG_RUNTIME_DIR/drv-audio-pipewire`, enforces mode
0700, removes a stale `pipewire-0` before launch, forwards TERM/INT to the
daemon, waits for it, and removes the socket on exit. The user unit adds
`RuntimeDirectory`, restart-on-failure, a bounded graceful-stop timeout, and
single-user sandboxing.

Exact activation commands:

```sh
nix build .#audio-pipewire-daemon
systemctl --user link "$(readlink -f result)/share/systemd/user/drv-audio-pipewire.service"
systemctl --user enable --now drv-audio-pipewire.service

PIPEWIRE_RUNTIME_DIR="$XDG_RUNTIME_DIR/drv-audio-pipewire" pw-cli ls Metadata
PIPEWIRE_RUNTIME_DIR="$XDG_RUNTIME_DIR/drv-audio-pipewire" \
  pw-cat --playback --raw --rate 48000 --channels 2 --format s16 audio.raw

systemctl --user disable --now drv-audio-pipewire.service
```

For an ephemeral opt-in run without installing the unit:

```sh
nix run .#audio-pipewire-daemon
```

The packaged launcher was tested against a pre-existing stale socket, then
with untargeted stock `pw-cat`, `pw-cli ls Metadata`, and `wpctl inspect 2` on
the private socket. TERM reaped the daemon and removed the socket; a second
launch recreated it and passed stock Node discovery. Playback retained exact
accounting for the 480-frame pattern (checksum 2,880,000) plus the stock silent
drain quantum (final ring position 960). Package tests, service-unit validation,
and all client probes succeeded.

## Explicit physical HDA backend

The virtual ADR endpoint remains the default for the daemon and all tests. The
no-plastic-only HDA spike can explicitly start the same persistent native
PipeWire/ADR daemon with a private physical PlaybackEndpoint. Stock,
untargeted pw-cat S16LE/48 kHz/stereo quanta still pass through the registry
worker, pinned Fuchsia mixing/gain processing and timeline accounting; the
private sink then submits each processed quantum as one AMD HDA BDL period.
No new application protocol or public hardware-selection API is exposed.

