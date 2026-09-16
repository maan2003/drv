# ARCH-gpu-process-split: Compositor core / GPU process split

## Status

Design and first implementation slice. Nothing user-visible yet. Implements
the "GPU process" component of [DESIGN-multi-user-gui](DESIGN-multi-user-gui.md).
Work happens on the `gpu-process` branch of the niri fork at `/src/niri`, with
smithay at `/src/smithay` for the storage change described below.

## Goal

The compositor core never opens `/dev/dri` and never maps client memory.
Everything that touches Mesa, GBM, EGL, or KMS runs in a separate process
with its own UID. A Mesa bug yields a process that can draw pixels and read
client buffers, not one that routes input, holds policy, or owns the lock
lease.

The protocol stays in the core. Whoever owns a client connection can send
`wl_keyboard.key` to that client, so the GPU process must never hold one.

## What niri looks like today

- `Backend` enum (`src/backend/mod.rs`) with tty, winit, headless variants.
  The rest of niri reaches the renderer only through
  `Backend::with_primary_renderer` (~35 call sites) and `Backend::render`.
- `tty.rs` owns libseat, udev, libinput, GBM, EGL, `DrmCompositor`,
  vblank handling. It is the GPU process in embryo.
- Render elements are generic over `R: NiriRenderer`, which is bound to
  `GlesRenderer` (`render_helpers/renderer.rs`). Effect elements (border,
  shadow, blur, resize, custom shaders) hold GL programs and textures.
  `OffscreenBuffer::render` renders sub-trees into textures while elements
  are being built, so the scene is a tree, not a list.
- smithay caches textures per renderer inside each `wl_surface`'s user
  data (`RendererSurfaceState`) and imports lazily during render.
- smithay's direct scanout path (`DrmCompositor`, `UnderlyingStorage`)
  only understands `WlBuffer`-backed storage.

## Target shape

```text
core process                         gpu process
  wayland clients (SO_PEERCRED)        render node + DRM master
  policy, focus, input, layout         GlesRenderer, shaders, textures
  buffer fds: validate, seal, forward  buffer registry: BufferId -> texture
  scene tree per frame  ------------>  DrmCompositor, plane assignment
  frame callbacks, feedback <--------  presented / released events
```

One binary. The GPU process is `niri --gpu-process` with the socket on an
inherited fd, the way Chromium does `--type=gpu-process`. This lets it reuse
`render_helpers` shaders without splitting the crate.

## Protocol

Unix stream socketpair, length-prefixed `postcard` frames, fds attached via
`SCM_RIGHTS` to the frame that references them and consumed in order. All
ids are `u64` chosen by the core; the GPU process maps them to smithay
`Id`s and textures.

Core to GPU:

- `RegisterShm { id, fd, size, offset, stride, width, height, format }`,
  `RegisterDmabuf { id, planes[], width, height, format, modifier }`,
  `UpdateShm { id, damage[] }`, `DestroyBuffer { id }`.
- `Frame { output, scene }` per redraw. `scene` is a tree of nodes:
  `Surface { buffer, geometry, src, transform, alpha, damage, opaque, kind }`,
  `SolidColor`, `Memory` (CPU-rendered panels), `Shader { program, uniforms,
  textures }`, `Offscreen { id, children }`, and the geometry wrappers
  `Crop`, `Relocate`, `Rescale`. Each node carries a stable id and a commit
  counter so damage tracking works across frames.
- `RenderToImage { scene, format }` for screenshots, colour pick, tests.
- Output control: mode set, VRR, gamma, power, cursor position. Later.

GPU to core:

- `Presented { output, time, sequence, per-element scanout state }` so the
  core fires frame callbacks and presentation feedback.
- `BufferReleased { id }` so the core sends `wl_buffer.release`.
- `Image { … }`, `OutputsChanged`, `Error`.

## Changes

**smithay.** Add a non-Wayland `UnderlyingStorage` variant carrying a
`Dmabuf` (and a memory variant for shm) and teach `DrmCompositor` and the
GBM exporter to scan out from it. Without this, direct scanout is
impossible from a process with no `WlBuffer`s. Also let `ShmState` track
pools without mapping them, so the core never maps client memory.

**niri core.** `Backend::Remote`. Elements become renderer-agnostic:
`NiriRenderer` bounds drop `GlesTexture`/`AsGlesRenderer`, textures and
offscreen buffers become ids, `Shaders` become an enum of program kinds
plus uniforms. `with_primary_renderer` call sites become remote requests.
Client-side policy (`ClientState.restricted`) becomes a per-UID record.

**niri GPU side.** `tty.rs` largely moves here: seat fds from the seat
daemon, `DrmCompositor`, vblank. Plus the buffer registry and scene
reconstruction into smithay render elements. Screencast rendering to
PipeWire lives here too.

## Order

1. `src/gpu/{scene,protocol,transport,server,client}.rs`. GPU side renders
   `SolidColor` and shm `Surface` nodes to an offscreen texture and returns
   pixels. Test: spawn the process under llvmpipe, register a buffer, render,
   check pixels. Proves IPC, fd passing, and rendering out of process with
   no DRM and no hardware.
2. Move the DRM output path into the GPU side behind `Frame`/`Presented`.
   Headless tests keep using `RenderToImage`.
3. Make niri's elements renderer-agnostic and add `Backend::Remote`.
   This is the bulk of the work and is mostly mechanical.
4. smithay storage variant for direct scanout.
5. Sandbox the GPU process: own UID, seccomp, no client sockets.

## Testing

llvmpipe via Mesa's surfaceless EGL platform. The dev shell does not ship
a Mesa driver; tests need `LIBGL_ALWAYS_SOFTWARE=1` and the Mesa EGL vendor
file on `__EGL_VENDOR_LIBRARY_FILENAMES`. The existing `Headless` backend
already does surfaceless EGL, so the same environment covers both.
