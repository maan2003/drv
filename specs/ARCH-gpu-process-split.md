# ARCH-gpu-process-split: Compositor core / GPU process split

## Status

Implemented on the `gpu-process` branch of the niri fork at `/src/niri`
(smithay fork at `/src/smithay`). Builds, passes the test suite under
llvmpipe, not yet run on real hardware. Implements the "GPU process"
component of [DESIGN-multi-user-gui](DESIGN-multi-user-gui.md).

## Goal

The compositor core never opens `/dev/dri` for rendering and never runs
Mesa. Everything that touches Mesa, GBM, EGL, or KMS runs in a separate
process. A Mesa bug yields a process that can draw pixels and read client
buffers, not one that routes input, holds policy, or talks to clients.

The protocol stays in the core. Whoever owns a client connection can send
`wl_keyboard.key` to that client, so the GPU process must never hold one.

## Shape

```text
core process                              gpu process
  wayland clients, focus, input             GlesRenderer, shaders, blur
  layout, animation, damage tracking        texture tables (ids -> GL)
  libseat, udev, libinput                   DrmDevice / GbmDevice / DrmCompositor
  output policy: modes, VRR, gamma, on/off  swapchain, page flips, vblank
  screencast portal, targets, pacing        PipeWire streams and buffers
  recorded frames  ---------------------->  replay onto texture, output or cast
  vblank, scan, cast state  <-------------  events / replies
```

The GPU process is a dumb renderer. It has no scene graph, no layout, no
timing policy. It replays what the core recorded.

One binary. The core spawns `niri gpu-process --socket-fd 3 --mode drm`
(`--mode headless` for tests) via `std::env::current_exe`; setting
`NIRI_GPU_THREAD` runs the server as a thread instead, for debugging.

## Recording renderer

`src/gpu/remote.rs` implements smithay's `Renderer`, `Frame`, `Texture`,
`Bind`, `Offscreen`, `ImportMem`, `ImportDma`, `ExportMem` by recording
`Command`s (`src/gpu/protocol.rs`) instead of issuing GL. Render elements
in `render_helpers/` are unchanged in structure; `GlesRenderer` became
`RemoteRenderer`. Effects that used raw GL (border, shadow, resize,
open/close shaders, blur, framebuffer capture) became commands
(`DrawShader`, `Blur`, `CaptureFramebuffer`, `DrawCaptured`) with the GL
code moved to `src/gpu/gl/`.

Damage tracking lives in the GPU process, where `DrmCompositor` is. The
core records every element each frame wrapped in `BeginElement{id, src,
geometry, damage-since-last-frame, opaque, kind} … EndElement` markers
(framebuffer-effect captures go before a `BeginElementDraw` marker). The
GPU turns each segment into a real smithay element whose `draw` replays
its commands clipped to the damage the compositor hands it. So there is
one damage tracker, per-element culling, and the swapchain's buffer age is
handled by upstream code. `Present` is one-way; the per-element states
for presentation feedback come back as `GpuEvent::Presented`.

Client pixels never touch the core. shm buffers go to the GPU as the pool
fd plus layout (`ImportShm`); the GPU `pread`s damaged rows into a
scratch buffer and uploads them, so neither process maps client memory
and a truncated pool cannot SIGBUS anyone. dmabufs go as fds
(`ImportDmabuf`). The GPU keeps the `Dmabuf` (and a CPU copy for
textures up to 512x512) next to each texture: when an element's recording
is exactly one untinted 1:1 `DrawTexture` of such a texture, the GPU
exposes the buffer as the element's `UnderlyingStorage`, and
`DrmCompositor` can scan it out directly or copy it to the cursor plane.
`ElementMeta.transform` carries the buffer transform for that. Which
planes are allowed comes from the core each frame in `PresentFlags`
(the old `debug` config knobs).

## Protocol

Unix stream socketpair, length-prefixed `postcard` frames, fds via
`SCM_RIGHTS` on the frame that references them. Every `Request` gets
exactly one reply `Event`, except the per-frame `Execute` and `Present`,
which are one-way so the core never blocks on the GPU; their failures
arrive as `GpuEvent::Error`. Unsolicited `Event::Notify(GpuEvent)`
(presented, vblank, error, device error) can arrive at any time; the
client queues those and wakes the core loop through a calloop `Ping`.

Core to GPU: `Execute{commands}` (incl. `ImportShm` + pool fd),
`ImportDmabuf`, `ReadTexture`, `AllocateDmabuf`, `Sync` (used when a
dmabuf render target is finished, so the buffer is complete before it goes
to PipeWire or an image-copy client), `SetCustomShader`,
device lifecycle (`AddDevice`, `RemoveDevice`, `PauseDevices`,
`ResumeDevices`, `RescanDevice`, `CleanupDevice`), output control
(`EnableOutput{.., color, prefer_10bit}`, `DisableOutput`, `SetMode`,
`SetVrr`, `SetColorState` (HDR signalling + max bpc, staged for the next
commit), `SetCtm`, `SetOutputGeometry`, `SetGamma`, `ClearOutputs`,
`SetDebugTint`),
`Present{output, frame, flags}`, screencast streams (`CastStart` replies
with the effective cursor mode, `CastConfigure` for size / refresh,
one-way `CastClear` and `CastStop`), `LoadCursor{theme, names, size,
fallback, first_id}` (Xcursor lookup and upload; replies `Cursor` with
per-frame size, hotspot and delay, the textures being `first_id..`),
one-way `EncodePng{token, id, region}` (result as `Notify(Png{token,
data})`), `Shutdown`.

GPU to core: `Ready{caps}`, `Ack`, `Image`, `Dmabuf` (+ fds),
`CastStarted`, `Cursor`, `DeviceAdded{caps}`, `Scan{connected, changed,
disconnected}`, `OutputState`, `Notify(Presented | VBlank | Error |
DeviceError | Cast(..) | Png)`, `Error`. Cast events: `NodeId` (for the
portal), `State{active, ready_size, min_frame_time}`, `Redraw`,
`Rendered` / `Skipped{target_time}` (frame pacing), `Stop`, `PipeWireFatal`.

Render targets (`Command::Begin`): `Texture(id)`, `Dmabuf(id)` (the GPU
binds the dmabuf itself, needed for image-copy buffers), `Output(ref)`
(recorded and drawn by `Present`), `Cast(stream)` (rendered into the
stream's next PipeWire buffer when the frame ends; `CastFrameInfo` before
it carries scale, target time and cursor position) and
`CastCursor(stream)` (the cursor bitmap for metadata cursor mode).

`Caps` carries what the core needs to answer clients without asking again:
shm and dmabuf formats, dmabuf render formats (screencast, image copy),
which shader programs compiled.

## Split of the old tty backend

Core (`src/backend/tty.rs`): libseat session and VT switching, udev
hotplug, libinput, choosing modes (incl. modelines/CVT), VRR and max-bpc
policy, `Output` objects and IPC output state, frame clock and redraw
state, presentation feedback, dmabuf global. It opens DRM fds through
libseat and hands dups to the GPU process; it never uses them itself.

GPU (`src/gpu/drm.rs`, `src/gpu/server.rs`): `DrmDevice`, `GbmDevice`,
allocator, one `DrmCompositor` per enabled CRTC, connector properties
(max bpc, HDR reset, gamma), EDID parsing for `ConnectorInfo`, page flips,
vblank forwarding, plane assignment (direct scanout, cursor plane), and
allocating capture buffers (`AllocateDmabuf`). The core passes the
primary render node with every `AddDevice`; the GPU probes EGL on each
device and creates the renderer on the one whose EGL device resolves to
that render node (upstream's `try_initialize_gpu`). That is usually the
GPU's own card, but on Asahi the GPU card has no KMS (`DrmDevice::new`
fails with EOPNOTSUPP, tolerated like upstream) and Mesa renders through
the DCP display controller's node, so the renderer lives there.
`RemoveDevice` replies `DeviceRemoved { renderer_dropped }` so the core
knows when the renderer went away regardless of which node owned it.
Ten-bit formats are probed with the compositor that is actually used;
surfaces are created right before the compositor that consumes them,
because dropping a smithay `DrmSurface` disables the CRTC and a surface
created earlier would then page-flip onto a disabled CRTC (EINVAL).
Secondary GPUs are display-only via the rendering device's allocator
with linear buffers.

## Screencasting

Core (`src/screencasting/`): the portal D-Bus session, picking output /
window / dynamic targets, frame pacing (`min_frame_time` mirrored from
the stream, redraw timers), and recording the target's elements into a
`Cast` frame with a per-stream `Recorder` (`src/gpu/record.rs`, the same
element-marker recording outputs use). It never sees PipeWire, buffer fds
or pixel memory.

GPU (`src/gpu/cast.rs`): the PipeWire connection on the GPU event loop,
stream negotiation (dmabuf modifiers with test allocations, shm
fallback), memfd / GBM buffer allocation, per-stream
`OutputDamageTracker` over the replayed `SceneElement`s
(`src/gpu/scene.rs`, shared with the DRM compositor path), rendering into
the dequeued buffer, cursor metadata bitmaps, and queueing buffers back
once their fences signal. A frame the core recorded is dropped when
nothing changed or no buffer is free; every frame is reported back as
`Rendered` or `Skipped` so the core paces on real output (a recorded frame
counts as sent until the report arrives).

## Cursors and screenshots

Xcursor theme files are parsed in the GPU process (`src/gpu/cursor.rs`,
the `xcursor` crate) and uploaded straight into textures; the core's
`CursorManager` (`src/cursor.rs`) asks for an icon by name through
`GpuHandle::load_cursor` and keeps only frame geometry plus a
`RemoteTexture` per frame. Named cursors therefore exist only once the
GPU renderer is up (before that the pointer is hidden), and the cache is
dropped when the primary device goes away. Up to 512x512 cursor frames
keep a CPU copy on the GPU side so `DrmCompositor` can still put them on
the cursor plane.

Screenshots stay on the GPU: the core renders into a texture and sends
`EncodePng`; a GPU-process thread reads the pixels back, encodes them with
the `png` crate and returns the bytes. The core only writes the file, sets
the clipboard selection and emits the IPC event. The `png` and `xcursor`
crates are thereby out of the process that holds client connections.

## HDR and wide gamut

Merged from the `ma/turtle-pig-penguin` branch (protocol v10). The core
keeps all policy: the color-management protocol, per-output image
descriptions, `hdr { mode="auto"|"on"; reference-luminance }`,
`wide-gamut-p3` and the IPC `Ctm` action live in `niri.rs` / the handlers
unchanged. What moved into the GPU process is everything that touched the
renderer or KMS:

- Connector capabilities: `ConnectorInfo` carries `hdr: HdrCaps`
  (driver exposes `Colorspace` with BT2020_RGB and `HDR_OUTPUT_METADATA`,
  EDID advertises PQ; EDID luminances via libdisplay-info) and the
  `max bpc` range. The core stores them as `OutputHdrCaps` in the output
  user data.
- Signalling: `Tty::render` reconciles a `ColorState{hdr: Option<
  HdrMetadataDesc>, max_bpc}` per frame and sends `SetColorState` only
  when it changes; the GPU stages it with smithay's `use_color_state` so
  it rides the compositor's atomic commit (standalone connector-property
  commits hang some drivers). A rejected state is remembered core-side
  (`failed_color_state`) until the config changes or the session resumes.
  Max bpc no longer has its own request; the initial state goes with
  `EnableOutput`.
- Framebuffer formats: SDR outputs are 8-bit. With `prefer_10bit` (HDR
  allowed or `wide-gamut-p3`) the GPU probes each 10-bit format with a
  throwaway compositor + `render_frame` and puts the working ones first.
- Blend space: `Command::Begin{blend: Option<BlendParams>}` (`HdrPq{
  ref_lum_scale}` or `DisplayP3`) tells the GPU to install the
  `TextureHdr` program as the frame-wide default texture override and to
  encode solid colors on the CPU (`src/gpu/gl/blend.rs`). All GPU-side
  shaders end in `niri_blend(color)` (`hdr.frag`); the core appends the
  `niri_blend_mode` / `niri_ref_lum_scale` uniforms to its own shader and
  tex-program draws from `RemoteRenderer::frame_blend`. Overrides form a
  stack in `run_frame`, so an element override or
  `SuspendTexProgramOverride` / `RestoreTexProgramOverride` (used by
  `BlendSurfaceRenderElement` for content already in the blend space)
  restores the frame-wide one. Only the output frame is recorded with a
  blend; casts and screenshots stay SDR. A blend change resets the
  compositor's buffers (full redraw).
- Planes: while blending, the core clears the cursor and overlay plane
  flags in `Present` and allows primary scanout only for fullscreen
  content already encoded in the blend space.
- CTM: `SetCtm{matrix}` writes the CRTC `CTM` blob (S31.32) from the GPU
  process, deferred to resume while the device is inactive.

The GPU smoke test renders a PQ frame with llvmpipe and checks the CPU and
shader encodes agree and that a suspended override passes pixels raw.
Nothing here has run on real HDR hardware yet.

## Smithay fork

niri builds against `../smithay` (branch `niri-gpu-process`, upstream +
small additions): `UnderlyingStorage::Dmabuf` with matching
`ExportBuffer` / `ScanoutBuffer` / framebuffer-cache variants, so an
element can offer a dmabuf for scanout without a `WlBuffer`; lazy shm
pool mapping plus `shm::with_buffer_fd`, so a compositor that only
forwards the fd never maps client memory; `MemoryBuffer::as_mut_slice`.
For HDR it also carries dividebysandwich's three commits: connector color
state in atomic commits (`ConnectorColorState`, `HdrOutputMetadata`,
`use_color_state`), the color-management / color-representation
protocols, and the renderer-level tex program override plus solid color
transform in `GlesRenderer`. niri pins the fork by git rev in
`Cargo.toml` (`rho/niri-gpu-process` on maan2003/smithay).

## Dropped in v1

DRM leasing, multi-GPU rendering, per-surface scanout dmabuf feedback,
the winit (nested) backend, the legacy EGL `wl_drm` path, and the
`wait_for_frame_completion_before_queueing` debug knob. None of these
affect the security story; they are performance or convenience features to
revisit once the split is stable on hardware.

## Testing

llvmpipe via Mesa's surfaceless EGL platform. The dev shell does not ship
a Mesa driver; tests need `LIBGL_ALWAYS_SOFTWARE=1` and the Mesa EGL vendor
file on `__EGL_VENDOR_LIBRARY_FILENAMES`. `cargo test --lib gpu::` runs the
in-process smoke test, `cargo test --test gpu_process` spawns a real
`niri gpu-process` child and checks pixels.

## Next

1. Run on real hardware: startup, hotplug, VT switch, suspend, VRR,
   direct scanout (check `niri msg` / feedback shows ZeroCopy for
   fullscreen dmabuf clients), cursor plane, screencasting (dmabuf and
   shm streams, metadata cursor, window casts, dynamic casts), named
   cursors from the theme, screenshots (file, clipboard, portal).
2. Sandbox the GPU process: own UID, seccomp, only the DRM fds it is given.
3. Restart the GPU process on crash instead of stopping the compositor.
