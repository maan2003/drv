# Fuchsia Audio Device Registry provenance

The files below `upstream-fuchsia/` are byte-for-byte copies from Fuchsia
commit `1e1219e3fac944c9a906aea9646939746b6062b3`, the repository pin already
recorded in `nix/fuchsia-reference.json`. They are governed by
`LICENSE.fuchsia` (BSD 2-Clause).

| Upstream path | SHA-256 |
| --- | --- |
| `src/media/audio/services/device_registry/device.h` | `3eb7b26d91842c1e1ea4d842b9b72a695874aa6c3e665c07aee5c7c5335f924a` |
| `src/media/audio/services/device_registry/audio_device_registry.cc` | `baf6d09ecde7e1f4d399aadeddf48ee9efb746ab60c71c7007f5f62160bdc4f5` |
| `src/media/audio/services/device_registry/ring_buffer_server.cc` | `9ef3bde3ec57ad18eef4ac7ebcfb55be346d59ba75a68c282134a00bedb5c19f` |
| `sdk/fidl/fuchsia.audio/format.fidl` | `e2331fa7958338a6db15faaecdb6259f11deec780efb6ff182985b26471c27a9` |
| `sdk/fidl/fuchsia.audio/ring_buffer.fidl` | `d414bd25b47401799e0790cfbc8332aa50a9ba5d4265ae7cccd68f55bc7219fc` |

Fuchsia's ADR implementation depends on Zircon, FIDL-generated bindings,
component runtime, and async dispatchers that do not exist on the host. It is
therefore retained unchanged as the source contract rather than patched into a
false host build. `src/device_registry.rs` is the isolated host adaptation of
this slice: it preserves ready-before-advertisement, registry-owned device
identity and format, the ADR `Stopped` to `Started` ring-buffer lifecycle, and
the Fuchsia ring-buffer frame contract. PipeWire native protocol and memfd
transport remain compatibility-side host code.
