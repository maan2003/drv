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
(`EnableOutput`, `DisableOutput`, `SetMode`, `SetVrr`, `SetMaxBpc`,
`SetOutputGeometry`, `SetGamma`, `ClearOutputs`, `SetDebugTint`),
`Present{output, frame, flags}`, screencast streams (`CastStart` replies
with the effective cursor mode, `CastConfigure` for size / refresh,
one-way `CastClear` and `CastStop`), `Shutdown`.

GPU to core: `Ready{caps}`, `Ack`, `Image`, `Dmabuf` (+ fds),
`CastStarted`, `DeviceAdded{caps}`, `Scan{connected, changed,
disconnected}`, `OutputState`, `Notify(Presented | VBlank | Error |
DeviceError | Cast(..))`, `Error`. Cast events: `NodeId` (for the
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
allocating capture buffers (`AllocateDmabuf`). Secondary GPUs are
display-only via the primary's allocator with linear buffers.

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

## Smithay fork

niri builds against `../smithay` (branch `niri-gpu-process`, upstream +
small additions): `UnderlyingStorage::Dmabuf` with matching
`ExportBuffer` / `ScanoutBuffer` / framebuffer-cache variants, so an
element can offer a dmabuf for scanout without a `WlBuffer`; lazy shm
pool mapping plus `shm::with_buffer_fd`, so a compositor that only
forwards the fd never maps client memory; `MemoryBuffer::as_mut_slice`.

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
   shm streams, metadata cursor, window casts, dynamic casts).
2. Sandbox the GPU process: own UID, seccomp, only the DRM fds it is given.
3. Restart the GPU process on crash instead of stopping the compositor.
