# PipeWire audio spike

This slice supplies a hardware-free, deterministic S16LE/48 kHz/stereo
playback endpoint and encodes its standard `SPA_PARAM_EnumFormat` object. Its
consumed-byte to frame-position mapping directly executes the unchanged pinned
Fuchsia audio `TimelineFunction`/`TimelineRate` implementation packaged in
`../fuchsia-audio-timeline`; it is not a retyped local equivalent. Run it with:

```sh
cargo run -p drv-audio-pipewire-spike
cargo test -p drv-audio-pipewire-spike
```

The SPA POD bytes use PipeWire's native ABI and can be placed directly in a
future node/port parameter event. The protocol frontend does not leak into the
playback state. This milestone neither accesses ALSA nor physical hardware.

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
reported position of 480 frames. Production graph policy, realtime pacing,
multi-node clock/quantum coordination, and long-running playback remain outside
this spike.
