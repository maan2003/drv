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
  recorded frames  ---------------------->  replay onto texture or output
  vblank, connector scan  <---------------  events / replies
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
handled by upstream code. The reply to `Present` carries per-element
states for presentation feedback.

## Protocol

Unix stream socketpair, length-prefixed `postcard` frames, fds via
`SCM_RIGHTS` on the frame that references them. Every `Request` gets
exactly one reply `Event`. Unsolicited `Event::Notify(GpuEvent)` (vblank,
device error) can arrive at any time; the client queues those and wakes
the core loop through a calloop `Ping`.

Core to GPU: `Execute{commands}`, `ImportDmabuf`, `ReadTexture`,
`SetCustomShader`, device lifecycle (`AddDevice`, `RemoveDevice`,
`PauseDevices`, `ResumeDevices`, `RescanDevice`, `CleanupDevice`), output
control (`EnableOutput`, `DisableOutput`, `SetMode`, `SetVrr`,
`SetMaxBpc`, `SetOutputGeometry`, `SetGamma`, `ClearOutputs`,
`SetDebugTint`), `Present`, `Shutdown`.

GPU to core: `Ready{caps}`, `Ack`, `Image`, `DeviceAdded{caps}`,
`Scan{connected: ConnectorInfo[], disconnected}`, `OutputState`,
`Presented{submitted}`, `Notify(VBlank | DeviceError)`, `Error`.

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
vblank forwarding. Secondary GPUs are display-only via the primary's
allocator with linear buffers.

## Dropped in v1

DRM leasing, direct scanout and cursor plane, multi-GPU rendering,
per-surface scanout dmabuf feedback, the winit (nested) backend, the legacy
EGL `wl_drm` path, and the gnome-screencast default feature. None of these
affect the security story; they are performance or convenience features to
revisit once the split is stable on hardware.

## Testing

llvmpipe via Mesa's surfaceless EGL platform. The dev shell does not ship
a Mesa driver; tests need `LIBGL_ALWAYS_SOFTWARE=1` and the Mesa EGL vendor
file on `__EGL_VENDOR_LIBRARY_FILENAMES`. `cargo test --lib gpu::` runs the
in-process smoke test, `cargo test --test gpu_process` spawns a real
`niri gpu-process` child and checks pixels.

## Next

1. Run on real hardware: startup, hotplug, VT switch, suspend, VRR.
2. Sandbox the GPU process: own UID, seccomp, only the DRM fds it is given.
3. Screencast buffers allocated GPU-side; direct scanout via a
   non-`WlBuffer` `UnderlyingStorage` in smithay; cursor plane.
