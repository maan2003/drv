# ARCH-app-policy: Per-UID client policy

## Status

Implemented on the `policy` branch of the niri fork at `/src/niri`
(pushed as `rho/policy`, on top of `gpu-process`). Identities are static
(fixed UIDs from the system configuration), launches are by app name
only, nothing shares a UID, and there is no "trusted" flag: what a
process may do is its globals and grants. Crates: `drv-policy` (types,
the SEQPACKET transport `seq`, the `wire`, the forker channel, compositor
client), `drv-supervisor` (root; starts the trusted set, wires it,
restarts what dies), `drv-appd` (the launcher for untrusted things, plus
the `drv` CLI), `drv-forker` (drv-appd's privileged helper: the sandbox
and the fork), `drv-os` (uid/gid lookups, fd and directory helpers),
`drv-bridge` (the UID-keyed desktop services server and the shim on each
app's private bus: notifications and portals), `drv-seat` (`drv-seatd`,
the seat and GPU-process parent), `drv-auth` (`drv-authd`, the PIN
verifier that unlocks the compositor) and `drv-lock` (the lock screen, an
app). Names are in [CONTEXT.md](../CONTEXT.md).
Builds, passes tests, and runs
end to end in the KVM dev VM (`nix/dev-vm.nix`, `nix/dev-vm-run.sh` in
the fork). The NixOS module `nix/module.nix` (`services.drv`, flake
output `nixosModules.default`) turns one app list into passwd entries,
`appd.toml`, the session bus policy, the units, and a launcher entry
per app (`Exec=drv launch <name>`).
Implements the "identity and policy" part of
[DESIGN-multi-user-gui](DESIGN-multi-user-gui.md); sits beside
[ARCH-gpu-process-split](ARCH-gpu-process-split.md).

## Goal

Every process runs as its own UID. The compositor identifies a client by
the UID on its socket (`SO_PEERCRED`) and shows it only the Wayland
globals its policy grants. A client that never sees
`zwlr_screencopy_manager_v1` cannot bind it, so the capability check
happens once, at the registry, instead of per request in every protocol
handler. Capabilities that are not Wayland globals (asking about other
UIDs, driving the screencast services) are `grants` on the same record.

## Process tree

```text
drv-supervisor (root, the systemd unit)
  starts drv-seatd (root), drv-authd, the compositor, compositor-gpu
  (niri gpu-process, uid drv-gpu), drv-forker (root) and drv-appd as their
  users, from its own command line; takes input from nobody. Every pair of
  peers gets both ends of a socketpair it made, pushed down each child's
  wire (fd 3, DRV_WIRE_FD) as Attach{..} + one fd:
    compositor <-> seatd (Seat / Compositor)
    compositor <-> authd (Auth / Compositor)
    compositor <-> compositor-gpu (Gpu / Compositor; a stream socket)
    appd       <-> authd (Auth / Verifiers)
  Whenever one side (re)starts its pairs are linked afresh. Restarts what
  dies, in two groups: drv-appd with drv-forker (the apps stay up), and
  the compositor with compositor-gpu. Every compositor start is announced
  to drv-appd as Notice::CompositorStarted.

drv-appd (uid drv-appd)                  drv-forker (root)
  fd 3 wire, fd 4 the public socket        fd 3: the channel to drv-appd, its
  /run/drv/appd.sock (bound by the           only input. --range, --group,
  supervisor, 0666), fd 5 notices,           --expose from the command line.
  fd 6 the channel to drv-forker           Launch{uid, groups, argv, env,
  appd.toml: every uid, exec, groups,        network, expose, fds} + fds ->
    globals, grants, autostart, auth         range and group check, dirs,
  Launch{app} from anyone -> Launch  ---->   cgroup app-<uid>, sandbox,
  Lookup{uid}: own uid, or the lookup        setresuid, NNP, exec; the passed
    grant                                    fds land on the numbers in `fds`
  auth=true: a wire for the app with       Running -> the uids whose app-<uid>
    Attach::Auth already on it; the          cgroup has a process (so a new
    other end went to authd as Verifier      forker still knows the old apps)
  autostart on every CompositorStarted:    reaps children, logs their exit
    what Running lacks, once the apps
    socket listens (/proc/net/unix)

drv-seatd (root, supervisor child)
  holds the seat (libseat builtin backend, no seatd) and udev; announces the
  seat's /dev/dri/card* and /dev/input/event* nodes to its one client, the
  compositor connection the supervisor attached (Hello lists them, hotplug
  follows on a second socket with enable/disable), opens only those and
  passes the fds. Forks nothing. A seat daemon restart makes the compositor
  exit and come back.

drv-authd (uid drv-auth, supervisor child)
  argon2id PIN in /var/lib/drv-auth (0700), escalating delay after 5 misses;
  Verify arrives on connections attached as Verifier (by the supervisor, or
  by drv-appd down its Verifiers socket, one per app launched with auth),
  Unlock{idle_timeout} goes to the one attached as Compositor; no socket

drv-compositor (uid,      drv-bridge (uid)      drv-bus (uid)        app-<name> (uid each)
    supervisor child)
  Lookup for each client    Lookup per peer       dbus-daemon with      forked by drv-forker
  D-Bus callers checked     notifications and     per-user own and      on drv-appd's say,
  against grants            portals for apps      send policy           sandboxed, reach
  no IPC socket, no                                                     appd.sock to launch
  device groups
```

Android is the model: the zygote is root and forks on command from
`system_server`, which is the only thing holding its pipe; the package
manager owns UIDs and permissions, unprivileged. Here drv-forker is the
zygote and drv-appd is `system_server` plus the package manager; the
supervisor is init for the trusted set. The forker has no filesystem
socket and no allow list of callers: its only peer is the channel the
supervisor gave it, whose other end is drv-appd's.

## Types (crate `drv-policy`)

`Global` enumerates the optional globals: dmabuf, layer shell, session
lock, data control, foreign toplevel, workspaces, output management,
gamma control, screencopy, image copy capture, virtual keyboard, virtual
pointer, input method, security context. Everything else
(`wl_compositor`, `xdg_wm_base`, `wl_shm`, seats, outputs, pointer
constraints, ...) is always advertised.

`Grant` is `lookup` (ask drv-appd about other UIDs: the
compositor, the bridge) and `screencast` (call the compositor's
screencast, screenshot and service-channel D-Bus services: the portal
backend only).

`AppPolicy { name, gpu, globals, grants, icon }`. `allows(global)`:
`gpu` grants dmabuf, otherwise the global must be listed. `has(grant)`.
`name` and `icon` are what the compositor shows the user; apps never
supply them. `AppPolicy::unknown()` is nothing; `everything(name)` is
every global and grant, for tests.

`rpc`: `Request::{Hello, Lookup { uid }, Launch { app }}`,
`Response::{Hello, Policy, Launched { uid }, Error}`, postcard payloads
behind a little-endian `u32` length, 64 KiB cap, answered in order.
`daemon::Handler { lookup(peer, uid), launch(peer, app) }` gets the
peer's UID from `SO_PEERCRED`; `serve` accepts every peer.

`seq`: the one transport for every daemon-to-daemon socket: `SOCK_SEQPACKET`,
one postcard message per datagram (64 KiB cap) with up to 16 fds in
`SCM_RIGHTS`. `wire`: `Attach::{Auth, Compositor, Verifier, Seat,
Verifiers}` plus one fd, on fd 3 (`DRV_WIRE_FD`). `forker`: the channel
between drv-appd and drv-forker (`CHANNEL_FD = 3` on the forker's side):
`Request::{Launch(Launch), Running}`, `Launch { uid, groups, argv, env,
network, expose, fds }` where `fds` names the child fd numbers (3..10) the
attached fds land on, `Response::{Forked { pid }, Running { uids },
Error}`; `Notice::CompositorStarted` from the supervisor to drv-appd;
`Channel` (a mutex around the socket; one request at a time).

`PolicyClient`: `connect(path)` does the hello now so a missing daemon
fails at startup, `lookup(uid)` caches per UID and reconnects after an
error, `launch(app)`, `reconnect()` for another thread, 2 s timeouts.
There is no mode without a daemon.

## appd.toml

```toml
wayland-socket = "/run/drv-wayland/wayland"   # every app's WAYLAND_DISPLAY

[env]                                          # every app, from the system config
PIPEWIRE_RUNTIME_DIR = "/run/pipewire"

[[app]]
name = "compositor"      # a service: identified, never launched
uid = 902
grants = ["lookup"]

[[app]]
name = "portal-gnome"
uid = 100013
exec = ["/nix/store/.../xdg-desktop-portal-gnome"]
grants = ["screencast"]
autostart = true
```

One UID per entry, one entry per name, no ranges, no default entry:
an unlisted UID is `unknown`. An entry without `exec` is a service
(started by systemd) and cannot be launched. `autostart` entries are
launched by the daemon, in order, once the apps socket exists.

## Launching

- Launch is not a privilege. Anyone who can reach
  `/run/drv/appd.sock` (mode 0666; the forker exposes `/run/drv` to
  apps) may send `Launch { app }`; the launcher's desktop entries run
  `drv launch <name>`, key binds in the compositor do the same over its
  own connection. `app` is a manifest name; arguments are the manifest's
  `exec` and nothing else, so an app never receives caller-chosen
  arguments or environment. The daemon builds the environment from its
  `PATH`, `[env]`, the entry's `env` and `WAYLAND_DISPLAY`.
- Lookups of other UIDs need the `lookup` grant; every UID may look up
  itself.
- drv-forker accepts `Launch` only on the channel, only for UIDs in its
  `--range`, groups on its `--group` list (so drv-appd cannot hand out
  `wheel`) and `/run` entries on its `--expose`/`--expose-optional` lists.
  It creates `/run/drv-apps/<uid>` (`XDG_RUNTIME_DIR`) and
  `/var/lib/drv-apps/<uid>` (`HOME`, cwd), mode 0700 owned by the UID,
  moves the child into `<supervisor cgroup>/app-<uid>` (needs
  `Delegate=yes`), then `setgroups`, `setresgid`, `setresuid`,
  `PR_SET_NO_NEW_PRIVS`, exec with the request's environment and nothing
  else. The fds that came with the request are the child's fds 3.. as the
  request names them; nothing else is inherited. Children are reaped and
  their exit logged.
- Sandbox (drv-forker, as root, between fork and exec): a private mount
  namespace; fresh tmpfs on `/tmp` and `/dev/shm`; `/proc` with
  `hidepid=invisible`; and a fresh read-only tmpfs on `/run` holding only
  the app's own runtime directory plus the `--expose` entries (the
  appd socket directory, the apps' Wayland socket directory, the
  bridge socket directory, `opengl-driver`, `current-system`,
  `/run/pipewire`, `/run/pulse`). No system D-Bus, no services' bus, no
  other app's runtime directory, no setuid wrappers. No user namespaces
  anywhere. Apps without `network = true` also get a new, empty network
  namespace.
- drv-appd runs as the `drv-appd` system user with no filesystem socket
  of its own: the supervisor binds the public socket and hands it over as
  fd 4, the channel to the forker as fd 6. drv-appd and drv-forker are a
  group: when either dies the supervisor stops the other and starts both
  again, with a fresh channel; the apps keep running and the new forker
  sees them through their cgroups.
- Audio is a group: PipeWire runs system-wide with sockets mode 0660
  group `pipewire`; an app whose manifest lists `groups = ["pipewire"]`
  can connect, anyone else gets `EACCES`.
- The compositor listens on `$DRV_APPS_SOCKET` (`/run/drv-wayland/wayland`,
  mode 0666) besides its own runtime directory. Anyone local may connect;
  the policy decides what they get, as with Android's binder services.
- `spawn-sh` and `spawn-at-startup` are disabled: a shell string is not
  an app name, and what starts with the desktop is `autostart` in the
  manifest, not the compositor's config.
- Desktop services (notifications and portals) go through `drv-bridge
  serve`, its own UID on the services' bus (a `dbus-daemon` as user
  `drv-bus`, socket in `/run/drv-session`, which apps never see). The bus
  config lets each listed user own only its names (`sessionBusNames`)
  and lets only `screencast`-granted users send to the compositor's
  names. The bridge socket `/run/drv-bridge/bridge.sock` is mode 0666;
  every connection is keyed on `SO_PEERCRED` plus drv-appd's
  answer for that UID, unknown UIDs are dropped, and what the human sees
  is the manifest name. An app with `bus = true` runs under
  `dbus-run-session -- drv-bridge app -- <exec>`: a private bus in its
  own UID with the shim claiming `org.freedesktop.Notifications` and
  `org.freedesktop.portal.Desktop` and forwarding to the server. The
  shim is compatibility, not a boundary.
- Portals: per app the server holds its own connection on the services'
  bus, registers the app with the portal `Registry` as `drv.app.<name>`,
  forwards `org.freedesktop.portal.*` bodies unchanged (fds included), and
  rewrites request and session handle paths so `Response` and `Closed`
  signals come back to the right caller. xdg-desktop-portal and the GNOME
  backend are apps with their own UIDs; the frontend needs the `pipewire`
  group for `OpenPipeWireRemote`, the backend holds the `screencast`
  grant. The frontend identifies callers by opening `/proc/<pid>/root`
  to look for `.flatpak-info`, which only works within one UID; the
  module builds it with `nix/xdg-desktop-portal-cross-uid.patch`, which
  treats an unreadable root as "not a flatpak" (there is no Flatpak here
  and the portal shares a UID with nobody). Screen sharing is per-session consent: the portal dialog is the
  consent, the portal session is the lease, and no app has a static
  screencast capability. While any session is live the compositor draws
  a "Screen is being shared" indicator above everything (never into the
  cast) naming the `stop-all-casts` key (`Mod+Shift+Escape` by
  default), which closes every session so the portal and app see the
  lease end.
- GPU process: PipeWire 1.6 dlopens `libspa-videoconvert` on the first
  stream connect, after the seccomp lockdown, so the GPU process loads it
  into PipeWire's plugin registry at startup.

## Compositor behaviour

- `Niri::insert_client` reads the peer UID and stores the policy in
  `ClientState.policy` before the client is inserted, so the registry the
  client sees on its first roundtrip is already filtered. A socket that
  arrived over D-Bus (the Mutter service channel) has no peer of its own;
  the D-Bus side supplies the caller's UID from the bus daemon's
  credentials.
- Every optional global is created with a filter from
  `client_allows(Global)`: the policy grants it and the connection is not
  a security-context one. The dmabuf global is filtered the same way.
- D-Bus services (`org.gnome.Mutter.ScreenCast`, `ServiceChannel`,
  `org.gnome.Shell.Screenshot`) resolve the caller's UID through
  `GetConnectionCredentials` and refuse it without the `screencast` grant
  (`dbus/caller.rs`, its own `PolicyClient` on the D-Bus thread).
- No IPC socket. `IpcServer` is never started: a client that can act as
  the compositor would bypass every policy. `niri msg` has nothing to
  talk to.
- Daemon socket: `$DRV_APPD_SOCKET`, else `/run/drv/appd.sock`.
  Cannot connect or hello fails: the compositor exits. A failed lookup
  later gives that client `AppPolicy::unknown()`.

## The lock

Locked is the default; the compositor holds a lease "unlocked until T"
that only `drv-authd` starts. The lock app (`services.drv.apps.lock`,
`drv-lock`, the one app with the `session-lock` global and `auth = true`)
draws the PIN screen and sends `Verify` down the connection drv-appd put
on its wire at launch (the other end went to `drv-authd` as a `Verifier`
down the socket the supervisor linked between them); on a match the
daemon sends `Unlock{idle_timeout}` on the compositor's connection, which
the supervisor handed both of them. The compositor
then grants itself the lease, sends the lock client `finished` and the
app exits. A client's `unlock_and_destroy` releases its surfaces and
nothing else: without a lease the outputs stay black and the compositor
launches the lock app again (3 s backoff, via `Launch` like any app).
Input extends the lease by `idleTimeout` (module option, 300 s); a
visible idle-inhibiting surface extends it too; `lock-session` (bound to
Super+Alt+L) ends it; a compositor restart starts locked. The lease is a
`CLOCK_BOOTTIME` deadline and `Niri::check_lease` is the one place that
turns "expired" into "locked": it runs before every frame, before every
input event and once a second, so nothing is drawn or delivered on a
stale lease, and suspend needs no hook (the clock runs while asleep). The
kernel's own replay of the last framebuffer on resume is switched off by
`nix/linux-drm-blank-on-resume.patch` (`drm_kms_helper.blank_on_resume=1`,
set by the module): the DRM resume helper commits the saved state with
every plane detached, so wake shows black until the compositor's first
commit. `nix/resume-vm.nix` plus `nix/resume-test.sh` check that on QXL. A client `lock`
while unlocked gets `finished`. While locked, casts and screenshots render
only the backdrop. Enrol with `drv-authd set-pin`; the dev VM enrols
`1234` in the unit's pre-start.

## Invariants

- Identity is the socket UID. PIDs are reused and are never used for
  policy. No two processes on the desktop share a UID; there is no human
  UID on the desktop at all.
- No child of drv-forker is root: it always switches to the requested
  UID, which is always inside its range.
- drv-forker's only peer is the channel the supervisor gave it, held by
  drv-appd; there is no path to it from the filesystem. The supervisor
  takes input from nobody.
- A global the policy denies is never in the client's registry. There is
  no second code path that hands out the same capability.
- Unknown UIDs get nothing; an unreachable daemon is the same as unknown.
- The compositor never reads a policy file, never forks, never execs,
  never launches on its own authority: a spawn key bind is a `Launch`
  request like any launcher's, and the GPU process is the supervisor's,
  from the supervisor's command line, as `drv-gpu`, handed to the
  compositor down the wire.
- The compositor holds no device group, no udev socket and no VT. Every
  DRM and evdev fd comes from `drv-seatd`, which serves the one connection
  the supervisor attached and opens only the seat's card and event nodes it
  announced itself (never render nodes or anything else under `/dev`).
- Only `drv-authd` unlocks. No Wayland request, key bind or D-Bus call
  starts a lease; the lock app cannot unlock even if compromised, it can
  only try PINs, and the daemon slows that down.
- Peers are handed over, never found: the auth daemon has no socket, and
  what it takes `Verify` from and pushes `Unlock` to are the fds the
  supervisor attached, or drv-appd forwarded down the supervisor's link.
  Anything that is not the lock app has no path to it.

## Not yet

- The bridge forwards every `org.freedesktop.portal.*` interface alike;
  nothing yet narrows which portals an app may use, and the document
  portal's FUSE view is not exposed into the sandbox.
- The bridge drops notification actions, hints and close signals.
- Files: no UID owns the person's files yet
  ([NOTES-file-ownership](NOTES-file-ownership.md)).
- Network isolation is only on/off. Per-app firewalling is designed
  separately.
- Nothing kills a still-running app when its manifest goes away; nothing
  pushes policy changes to the compositor; sub-UID ranges; exit
  reporting and cgroup kill from drv-forker.
- Launch authority as an fd handed to the launcher; the locker as a
  supervisor service; seatd, forker and supervisor off root with bounded
  capabilities; seccomp on the leaves.
