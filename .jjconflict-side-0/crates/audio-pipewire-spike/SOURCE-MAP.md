# Audio Device Registry source map

| Behavior in this slice | Pinned Fuchsia source | Host adaptation |
| --- | --- | --- |
| Device becomes discoverable only after initialization | `audio_device_registry.cc`: `AddDevice`, `DeviceIsReady` | `DeviceRegistry::register_virtual_playback` constructs the complete device and ring buffer before returning it |
| Registry owns token identity and supported format | `device.h`: `token_id`, `ring_buffer_format_sets`, `RingBufferRecord`; `format.fidl` | `RegisteredDevice` owns token, element, name and the fixed `PcmFormat`; PipeWire Node/Port IDs, properties and PODs are projections of it |
| Ring buffer lifecycle | `device.h`: `RingBufferState`; `ring_buffer_server.cc`: `Start`/`Stop` | `RingBufferEndpoint` begins `Stopped`, transitions to `Started` before accepting output, and remains started across persistent client lifecycles |
| Frame-addressed PCM ring contract | `ring_buffer.fidl`: PCM frame numbering and bytes-per-frame | Each recycled PipeWire buffer becomes a complete-frame quantum written to the registered endpoint, advancing its pinned Fuchsia `TimelineFunction` position |

The upstream files are pristine evidence and are not compiled on the host.
The Rust module is deliberately private to the compatibility executable; this
milestone introduces no application-facing or physical-device API.
