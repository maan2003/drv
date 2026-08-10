# PipeWire audio spike

This first slice supplies a hardware-free, deterministic S16LE/48 kHz/stereo
playback endpoint and encodes its standard `SPA_PARAM_EnumFormat` object. Run it
with:

```sh
cargo run -p drv-audio-pipewire-spike
cargo test -p drv-audio-pipewire-spike
```

The SPA POD bytes use PipeWire's native ABI and can be placed directly in a
future node/port parameter event. The endpoint boundary contains project-owned
PCM types only, so the protocol frontend does not leak into the eventual
Fuchsia Audio Device Registry, mixer, timeline, processing, and ring-buffer
implementation. This milestone neither accesses ALSA nor physical hardware.

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

An unmodified client cannot connect yet. The next frontend operation is
server-side native-protocol handling of `Core.GetRegistry`, followed by
`Registry.Global` events for a Node and its input Port. After discovery, stream
use still requires node/port bind and parameter enumeration, link creation,
shared-buffer negotiation, activation, clock/quantum scheduling, and processing.
