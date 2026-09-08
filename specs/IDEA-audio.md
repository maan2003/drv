# IDEA-audio: Fuchsia-derived Rust media service

## Status

The broad media-service direction remains exploratory. The real-time executor,
process boundary, speaker containment, and kernel-PCM transition are now owned
by [ARCH-audio-runtime](ARCH-audio-runtime.md). This record retains the wider
Fuchsia-derived media direction that is not yet committed architecture.

## Idea

The mature system would use a project-owned Rust media core derived from
Fuchsia's audio hardware, timing, mixer, registry, and testing designs. PipeWire
would become an application compatibility personality rather than the internal
architecture. Significant forks or a protocol-compatible Rust replacement are
acceptable. Preserving the selected client protocols is this proposed audio
integration strategy, not a requirement to retain existing desktop plumbing;
see [REQ-application-compatibility](REQ-application-compatibility.md).

```text
PipeWire, PulseAudio, JACK, and ALSA application clients
        -> compatibility protocols and client libraries
        -> project-owned Rust media graph and policy
        -> Fuchsia-derived device, clock, ring, and processing model
        -> safe Rust audio driver and hardware capability crate
        -> host backend and physical audio subsystem
```

Existing `libpipewire` and compatibility client libraries can remain during the
transition. The replacement server must eventually preserve the native protocol,
registry, SPA PODs, object lifecycle, shared-buffer negotiation, node activation,
clocks, quantum, latency, and permission behavior used by applications. Video
and MIDI compatibility are separate increments and must not be implied by an
audio-only implementation.

## Fuchsia source model

The driver-facing contract should begin with Fuchsia's modern Audio Composite
model:

- ring-buffer and DAI endpoints;
- separate controller, codec, and signal-processing elements;
- format negotiation, gain, plug, health, reset, and topology state;
- explicit start times, position notifications, FIFO and external latency;
- recovered clocks and monotonic-to-frame timeline transformations; and
- virtual devices and strict lifecycle validation.

The media engine can reuse or port Fuchsia's graph, clock synchronization,
resampling, gain, channel mixing, synthetic-clock, and deterministic test
designs. Fuchsia's implementation is substantially C++, so the first value is
its semantics and tests rather than direct Rust reuse. Project-owned Rust types
must sit between FIDL-derived hardware concepts and PipeWire/SPA compatibility
objects so neither foreign representation becomes the internal truth.

## M2 hardware boundary

M2 audio is not a self-contained PCI function. It combines the Apple MCA audio
controller, ADMAC DMA engine behind SIO DART, clocks, power domains, resets, and
I2C-connected codecs or smart amplifiers. Generic `vfio-platform` may expose
pieces but does not provide a complete safe assignment boundary.

Initial development should use the proven Asahi ALSA/ASoC backend beneath the
new media core. A later trusted Rust driver can use a dedicated platform broker
or kernel interface that exposes typed MCA, ADMAC, DART, clock, power, reset,
and amplifier capabilities. It should follow the OSTD-derived safe API and
unsafe-backend split in
[IDEA-rust-first-wifi-drivers](IDEA-rust-first-wifi-drivers.md).

Asahi's model-specific speaker DSP, calibration, thermal estimation, excursion
limits, and smart-amplifier feedback are safety requirements. No replacement
may enable physical speakers until equivalent protection passes deterministic
and hardware validation.

## Bluetooth integration

The Bluetooth service in [ARCH-bluetooth](ARCH-bluetooth.md) connects A2DP and
HFP directly to the native media graph. Bluetooth devices appear as ordinary
PipeWire-compatible sinks and sources to applications without BlueZ or a
WirePlumber BlueZ monitor. AVRCP, call state, codec selection, and profile policy
remain native system-service interactions rather than application-visible
hardware control.

## Application and system interfaces

Compatibility is for ordinary media applications, not existing desktop audio
management implementations. The project may replace PipeWire itself,
WirePlumber, configuration files, desktop settings panels, policy scripts,
ALSA's kernel model, and Bluetooth integration.

A vendored system UI and policy service use native capability APIs for default
devices, routes, volume, privacy, profile selection, and diagnostics. Future
application sandboxes receive explicit playback, capture, graph, and device
visibility capabilities. The compatibility frontend validates client identity,
shared-memory ownership, formats, ranges, state transitions, and permissions;
it never exposes physical DMA or device authority.

## Real-time boundary

Real-time processing threads must avoid allocation, deallocation, blocking
locks, control IPC, filesystem access, logging, and panic paths. Graph changes
use preallocated immutable snapshots or another bounded handoff. DSP execution,
buffer pools, clock correction, underrun handling, and SIMD behavior need
deterministic tests before replacing upstream PipeWire execution.

## Adoption consequences

The smallest useful slice is a virtual Fuchsia-style audio device feeding a Rust
graph that is exposed to an unmodified `libpipewire` client. Subsequent slices
add deterministic clock drift and underrun tests, the Asahi ALSA backend,
Bluetooth A2DP, PulseAudio/JACK/ALSA client compatibility, and finally native M2
audio drivers. Existing desktop-management compatibility is not a milestone.
