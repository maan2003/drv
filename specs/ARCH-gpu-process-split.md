# ARCH-gpu-process-split: Compositor core / GPU process split

## Status

The niri fork at `/src/niri` now uses Vello/Vulkan instead of GLES in the
GPU worker. The migration builds and passes software-rendering tests on Lavapipe,
including the sealed GPU worker. Vulkan rendering and dma-buf/KMS scanout also
run in the M2 crosvm DRM native-context guest on Honeykrisp, with the guest Mesa
PCI-probe fix. Physical display scanout remains unverified. This
implements the GPU-process component of [DESIGN-multi-user-gui](DESIGN-multi-user-gui.md).

## Goal

The compositor core never opens `/dev/dri` for rendering and never runs
Mesa. Everything that touches Mesa, GBM, Vulkan, or KMS runs in a separate
process. A Mesa bug yields a process that can draw pixels and read client
buffers, not one that routes input, holds policy, or talks to clients.

The protocol stays in the core. Whoever owns a client connection can send
`wl_keyboard.key` to that client, so the GPU process must never hold one.

## Shape

```text
core process                              gpu process
  wayland clients, focus, input             Vello/Vulkan renderer, effects
  layout, animation, damage tracking        texture tables (ids -> Vulkan)
  seat daemon client, udev, libinput        DrmDevice / GbmDevice / DrmCompositor
  output policy: modes, VRR, gamma, on/off  swapchain, page flips, vblank
  screencast portal, targets, pacing        PipeWire streams and buffers
  scene frames  ------------------------->  damage-track and draw to texture, output or cast
  vblank, scan, cast state  <-------------  events / replies
```

The GPU process has no layout and no timing policy. Per frame it gets a
flat scene (nodes bottom to top, each a list of draw ops) and owns
everything after that: damage history, buffer ages, culling, scanout.

One binary. The core spawns `niri gpu-process --socket-fd N --mode drm
--device dev_t:fd ...` (`--mode headless` for tests) via
`std::env::current_exe`. The tty backend always uses a real process; only
the headless backend and the unit tests run the server on a thread.

## Recording renderer

`src/gpu/remote.rs` implements smithay's `Renderer`, `Frame`, `Texture`,
`Bind`, `Offscreen`, `ImportMem`, `ImportDma`, `ExportMem`. Resource calls
(create / import / update / destroy textures, blur) become `Command`s
(`src/gpu/protocol.rs`) right away. Draw calls do not: a `RemoteFrame`
collects them into a `SceneFrame` that goes out as one
`Command::Frame` when the frame finishes. Render elements in
`render_helpers/` use `RemoteRenderer`. GPU-side Vello translates the recorded
ops into native drawing and built-in WGSL effects on one Vulkan device.
User-provided GLSL programs are not supported; ordinary open/close animations
remain core-side scale/fade operations.

A `SceneFrame` is `{target, size, transform, blend, clear, generation,
cast, nodes}`. A `Node` is `{id, src, geometry, damage, opaque, kind,
transform, capture: [Op], draw: [Op]}`; ops are `Solid`, `Texture`,
`Paint`, `Capture`, and `Captured`. Built-in paints have typed border,
shadow or resize parameters; texture draws carry typed effects and an explicit
source encoding. There are no named shader uniforms or program scopes on the
wire. One coordinate rule: everything in a frame
(geometry, damage, opaque, op `dst`) is in frame coordinates, the
untransformed buffer. Ops carry no damage. Draws outside any node (the
offscreen helpers in `render_helpers/mod.rs`) become an anonymous node.

Damage tracking lives in the GPU process, where `DrmCompositor` is. The
core `Recorder` (`src/gpu/record.rs`, one per target) only maps smithay
element ids to stable node ids and asks smithay's element model for
`damage_since` the last frame that target saw; the GPU `NodeTracks`
(`src/gpu/scene.rs`) keeps the history buffer ages need and turns each
node into a real smithay `SceneElement`. `draw_ops` (`src/gpu/draw.rs`)
is the single interpreter: it clips every op to the node damage the
compositor hands it (frame space, intersected with the op's `dst`, then
made `dst`-relative) and runs `DrmCompositor` / `OutputDamageTracker`
paths and offscreen targets alike. The core bumps `generation` whenever
it forgot its history (geometry change, failed send); the GPU drops its
history when it changes, so the two sides cannot drift. `Present` is
one-way; the per-node states for presentation feedback come back as
`GpuEvent::Presented`.

Client pixels never touch the core. shm buffers go to the GPU as the pool
fd plus layout (`ImportShm`); the GPU `pread`s damaged rows into a
scratch buffer and uploads them, so neither process maps client memory
and a truncated pool cannot SIGBUS anyone. dmabufs go as fds
(`ImportDmabuf`). The GPU keeps the `Dmabuf` (and a CPU copy for
textures up to 512x512) next to each texture: when a node's draw is
exactly one untinted 1:1 `Op::Texture` of such a texture, the GPU
exposes the buffer as the element's `UnderlyingStorage`, and
`DrmCompositor` can scan it out directly or copy it to the cursor plane.
`Node.transform` carries the buffer transform for that. Which
planes are allowed comes from the core each frame in `PresentFlags`
(the old `debug` config knobs).

## Compositor UI text and paint

The core shapes compositor-owned UI text with Parley and records rectangles,
circles, and positioned glyph runs with scene-local font data. It sends this
paint description before Vello scene preparation: Vello scene construction,
glyph preparation, Vulkan/wgpu submission, and rasterization run only in the
GPU process. UI texture creation is acknowledged after rendering and import,
and uses the existing remote texture lifetime.

Vello draws UI and client textures into resident Vulkan render targets. There
is no GLES renderer or UI readback/upload bridge. Explicit screenshots and
shm screencasts still read pixels back because their consumers require CPU
memory. Smithay retains KMS, plane selection, damage tracking and GBM buffer
allocation; Vello imports GBM scanout/capture targets and client dma-bufs.

Imported images are acquired from foreign queue ownership before tracked
rendering and released afterward. Separate raw Vulkan acquire/release command
buffers bracket wgpu command buffers, whose final states are restored before
handoff. The initial synchronization path waits for input fences and render
completion; it does not copy pixels to synchronize. Format advertisement is
restricted to probed single-memory-plane RGB formats/modifiers. Multi-plane
imports and asynchronous external-fence submission are not implemented.

Built-in borders, clipping, shadows and blur use native Vello. Blur uses a
Gaussian footprint rather than the former Kawase pyramid, so its appearance
is not pixel-identical. Resize mixing, postprocessing and color conversion use
private WGSL passes on the same Vulkan device. User-supplied GLSL is rejected;
normal opening, closing and resizing animations remain built in.

Built-in effects are mandatory renderer functionality, initialized before the
worker seals itself. Capabilities describe hardware-dependent formats/modifiers,
not optional shader programs.

The separate shell and chooser are outside this compositor migration.

## Protocol

Unix stream socketpair, length-prefixed `postcard` frames, fds via
`SCM_RIGHTS` on the frame that references them. Every `Request` gets
exactly one reply `Event`, except the per-frame `Execute` and `Present`,
which are one-way so the core never blocks on the GPU; their failures
arrive as `GpuEvent::Error`. Unsolicited `Event::Notify(GpuEvent)`
(presented, vblank, error, device error) can arrive at any time; the
client queues those and wakes the core loop through a calloop `Ping`.

Core to GPU: `Execute{commands}` (incl. `ImportShm` + pool fd),
`ImportDmabuf`, `RenderUi`, `ReadTexture`, `AllocateDmabuf`, `Sync` (used when a
dmabuf render target is finished, so the buffer is complete before it goes
to PipeWire or an image-copy client),
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

Render targets (`SceneFrame.target`): `Texture(id)`, `Dmabuf(id)` (the
GPU binds the dmabuf itself, needed for image-copy buffers; both are
one-shot full renders with no history), `Output(ref)` (kept and drawn by
`Present`), `Cast(stream)` (rendered into the stream's next PipeWire
buffer; `SceneFrame.cast` carries scale, target time and cursor position)
and `CastCursor(stream)` (the cursor bitmap for metadata cursor mode).

`Caps` carries what the core needs to answer clients without asking again:
shm and dmabuf formats, dmabuf render formats (screencast, image copy),
which built-in rendering effects are available.

## Split of the old tty backend

Core (`src/backend/tty.rs`): the seat daemon's session (`drv-seatd`,
[ARCH-app-policy](ARCH-app-policy.md)), VT switching and the device
list it announces (hotplug included; the core has no udev socket),
libinput on the path backend fed from that list, choosing modes (incl.
modelines/CVT), VRR and max-bpc policy, `Output` objects and IPC output
state, frame clock and redraw state, presentation feedback, dmabuf
global. It opens DRM fds through the seat daemon and hands dups to the
GPU process; it never uses them itself.

GPU (`src/gpu/drm.rs`, `src/gpu/server.rs`): `DrmDevice`, `GbmDevice`,
allocator, one `DrmCompositor` per enabled CRTC, connector properties
(max bpc, HDR reset, gamma), EDID parsing for `ConnectorInfo`, page flips,
vblank forwarding, plane assignment (direct scanout, cursor plane), and
allocating capture buffers (`AllocateDmabuf`).

The GPU process owns the device model. The core opens every card it may use
and sends the fds and an optional configured/boot-VGA render-node hint. The
worker matches Vulkan adapters by DRM device identity, initializes rendering
before sealing the sandbox, and reports the selected render node to the core.
Rendering is independent of KMS: a render-only GPU can allocate buffers for a
separate display-only device. Output creation probes both scanout and Vulkan
renderability for the selected formats/modifiers.

## Screencasting

Core (`src/screencasting/`): the portal D-Bus session, picking output /
window / dynamic targets, frame pacing (`min_frame_time` mirrored from
the stream, redraw timers), and recording the target's elements into a
`Cast` frame with a per-stream `Recorder` (`src/gpu/record.rs`, the same
scene recording outputs use). It never sees PipeWire, buffer fds
or pixel memory.

GPU (`src/gpu/cast.rs`): the PipeWire connection on the GPU event loop
(the context, which loads PipeWire's modules and config, is created at
startup; the socket comes from the core with `CastStart`, see Sandbox),
stream negotiation (dmabuf modifiers with test allocations, shm
fallback), memfd / GBM buffer allocation, per-stream
`OutputDamageTracker` over the `SceneElement`s
(`src/gpu/scene.rs`, shared with the DRM compositor path), rendering into
the dequeued buffer, cursor metadata bitmaps, and queueing buffers back
once their fences signal. A frame the core recorded is dropped when
nothing changed or no buffer is free; every frame is reported back as
`Rendered` or `Skipped` so the core paces on real output (a recorded frame
counts as sent until the report arrives).

## Cursors and screenshots

The core's `CursorManager` (`src/cursor.rs`) resolves an icon name to a
file in the theme (`xcursor::CursorTheme`), opens it and passes the fd in
`LoadCursor`; the GPU process parses it (`src/gpu/cursor.rs`,
`xcursor::parser`) and uploads the frames straight into textures. Through
`GpuHandle::load_cursor` the core keeps only frame geometry plus a
`RemoteTexture` per frame. Named cursors therefore exist only once the
GPU renderer is up (before that the pointer is hidden), and the cache is
dropped when the rendering device goes away. Up to 512x512 cursor frames
keep a CPU copy on the GPU side so `DrmCompositor` can still put them on
the cursor plane.

Screenshots stay on the GPU: the core renders into a texture and sends
`EncodePng`; a GPU-process thread reads the pixels back, encodes them with
the `png` crate and returns the bytes. The core only writes the file, sets
the clipboard selection and emits the IPC event. The `png` crate and the
Xcursor parser are thereby out of the process that holds client
connections.

## Sandbox

`src/gpu/sandbox.rs`. The GPU process holds nothing ambient: it gets
everything it will ever need before it seals itself, and seals itself
before it says hello.

Startup: drv-supervisor starts `niri gpu-process --mode drm` as user
`drv-gpu` (group `render` for the render nodes Mesa opens itself) with
the environment exactly as listed on the supervisor's command line
(`MESA_SHADER_CACHE_DISABLE=true`: no home directory after the seal),
no new privileges and a wire on fd 3, alongside the compositor; the
supervisor links the two with a stream socketpair (`Attach::Gpu` to the
core, `Attach::Compositor` to the process). The process takes the core's
connection off the wire and waits. `Tty::new` (core) opens every primary
DRM node through the seat daemon, takes the GPU connection off its own
wire, and sends `Start { devices, render_node_hint }` with the fds
attached. The process adds the devices (Mesa loads drivers and opens
render nodes here), applies the seccomp filter, and only then sends
`Ready { caps, devices }` reporting on each device. The core registers
the accepted ones in `Tty::init` and closes the rejected ones. There is
no "seal now" request: an unsealed process is never talked to. `Start`
is refused after startup. `NIRI_GPU_SANDBOX=0` in the process's
environment skips the seal, for debugging. Without a supervisor
(`--socket-fd` and `--device` on the command line, tests) the devices
come with the command line as before.

The core and the GPU process are one group at the supervisor: either
dying stops the other and both come back. A sealed process cannot bring
up a renderer for a new core, and a core has nothing to draw with
without its process, so there is no other useful recovery.

The allowlist is read / write / sendmsg / recvmsg and friends, memory
management, epoll / poll / timers / eventfd, futex and thread creation
(`clone` only with `CLONE_THREAD`; `clone3` gets ENOSYS from a separate
errno filter because glibc calls it with all signals blocked, where a
trap would kill the process), signals, time and identity queries,
`memfd_create` / `ftruncate`, `prctl` for names only, `tgkill` to our
own pid, stat by fd only (`AT_EMPTY_PATH`), and `ioctl` restricted by
request type to DRM (`'d'`), dma-buf (`'b'`), sync_file (`'>'`) plus
`FIONBIO` / `FIONREAD`. No open, socket, connect, exec, ptrace, or
directory listing. Everything else traps to a SIGSYS handler that
prints `niri gpu-process: seccomp blocked syscall N` to stderr and fails
the call with EPERM, so a library degrades instead of the process dying,
and the log says what to add.

Things the process used to open itself now arrive as fds on the request
that needs them: the core resolves and opens Xcursor icon files
(`LoadCursor`), and connects the PipeWire socket for every `CastStart`
(`PIPEWIRE_REMOTE` / `XDG_RUNTIME_DIR` like libpipewire; the process
uses it when it has no connection, so a lost connection heals on the
next cast). The PipeWire context, which loads modules and config, is
created at process startup.

Devices after the seal: `AddDevice` still works for hot-plug, but Mesa
cannot initialize on a device it did not start with, so such a device is
display-only at best. When the core has no renderer and the device set
changes (compositor started on an inactive VT, rendering device
unplugged and back), it exits (`Tty::respawn_gpu`) and the supervisor
starts the compositor group again on the current devices: only the
supervisor can start a GPU process, and without a renderer nothing was
on the GPU side to lose. A device the process already failed on does
not trigger another restart.

The cross-process test (`tests/gpu_process.rs`) runs the smoke test
against a self-sealed headless process, so rendering, cursor upload and
PNG encoding all run under the filter (with llvmpipe). In production it runs
as its own UID, started by drv-supervisor.

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
  allowed or `wide-gamut-p3`) the 10-bit formats go first and each is
  proven renderable by the compositor's buffer test.
- Blend space: `SceneFrame.blend` selects encoded PQ/BT.2020 or Display P3
  compositing. Each texture draw carries `SourceColor` independently of its
  effect: sRGB content is converted, matching HDR/P3 content passes through,
  and captured target pixels are never encoded again. Nonmatching descriptions
  retain the existing SDR-conversion fallback; this is not a new tone-mapping
  implementation. Casts and screenshots stay SDR.
  Changing output blend space forces a full redraw. Native targets and private
  effect passes support float/10-bit output, but stock Vello gradient tables and
  isolated/filter layers use RGBA8 intermediates; those effects can quantize HDR.
- Planes: while blending, the core clears the cursor and overlay plane
  flags in `Present` and allows primary scanout only for fullscreen
  content already encoded in the blend space.
- CTM: `SetCtm{matrix}` writes the CRTC `CTM` blob (S31.32) from the GPU
  process, deferred to resume while the device is inactive.

The GPU smoke test renders a PQ frame with Lavapipe and checks the CPU and
GPU encodes agree and that explicit target-encoded content passes pixels raw.
Core-side draw context restoration is checked across nested calls and returned errors.
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

The dev shell supplies the Vulkan loader; tests also need a Vulkan ICD
(Lavapipe for software testing). `cargo test --lib gpu::` exercises the renderer
in-process; `cargo test --test gpu_process` exercises the sealed worker.
The ignored `ui::paint::tests::affected_ui_visual_grid` renders actual UI scenes;
`NIRI_UI_GRID_PNG` saves its output and `NIRI_UI_GRID_SCALE` selects a scale.
The software suite covers transformed texture sampling, partial damage,
premultiplied resize mixing, ordered/clipped backdrop capture, blur and explicit
source-color isolation. Physical-device validation remains outstanding; software
tests and earlier GLES hardware results do not establish Vulkan dma-buf correctness.

## Next

1. Run on real hardware: startup, hotplug, VT switch, suspend, VRR,
   direct scanout (check `niri msg` / feedback shows ZeroCopy for
   fullscreen dmabuf clients), cursor plane, screencasting (dmabuf and
   shm streams, metadata cursor, window casts, dynamic casts), named
   cursors from the theme, screenshots (file, clipboard, portal).
2. Restart the GPU process on crash instead of stopping the compositor.
