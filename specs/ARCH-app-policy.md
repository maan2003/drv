# ARCH-app-policy: Per-UID client policy

## Status

Implemented on the `policy` branch of the niri fork at `/src/niri`
(pushed as `rho/policy`, on top of `gpu-process`). Identities are static
(fixed UIDs from the system configuration), launches are by app name
only, nothing shares a UID, and there is no "trusted" flag: what a
process may do is its globals and grants. Four crates: `drv-policy`
(types, protocol, compositor client), `drv-identity` (the unprivileged
brain, plus the `drv` CLI), `drv-spawn` (`drv-spawnd`, the root
supervisor that forks the identity daemon and the apps), `drv-bridge`
(the UID-keyed desktop services server and the shim on each app's
private bus: notifications and portals), `drv-seat` (`drv-seatd`, the
seat and GPU-process parent), `drv-auth` (`drv-authd`, the PIN verifier
that unlocks the compositor) and `drv-lock` (the lock screen, an app).
Builds, passes tests, and runs
end to end in the KVM dev VM (`nix/dev-vm.nix`, `nix/dev-vm-run.sh` in
the fork). The NixOS module `nix/module.nix` (`services.drv`, flake
output `nixosModules.default`) turns one app list into passwd entries,
`identity.toml`, the session bus policy, the units, and a launcher entry
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
drv-spawnd (root)                     drv-spawnd identityd (uid drv-identity)
  binds /run/drv/identity.sock 0666     fd 3: the public socket, fd 4: the channel
  socketpair -> forks identityd   ---->   identity.toml: every uid, exec, groups,
  with fd 3 + fd 4, respawns it            globals, grants, autostart, auth
  channel: {uid, groups, argv,    <----   Launch{app} from anyone -> channel
    env, network, auth} -> range           Lookup{uid}: own uid, or peer has
    and group check, dirs, cgroup,           the lookup grant
    sandbox, setresuid, NNP, exec           autostart once the apps socket exists
  forks drv-seatd (root), drv-authd and the compositor too, restarts them,
  and wires: each child with peers gets a wire on fd 3 (DRV_WIRE_FD) down
  which the spawner pushes Attach{Seat|Auth|Compositor|Verifier} + one fd,
  both ends of a socketpair it made. Whenever a daemon or the compositor
  (re)starts, that pair is linked afresh; an app with auth=true (the lock
  app) gets a pair to authd at launch. Nobody connects to anybody, nobody
  checks a peer's UID.

drv-seatd (root, spawner child)
  holds the seat (libseat builtin backend, no seatd) and udev; announces the
  seat's /dev/dri/card* and /dev/input/event* nodes to its one client, the
  compositor connection the spawner attached (Hello lists them, hotplug
  follows on a second socket with enable/disable), opens only those and
  passes the fds; forks the GPU process as uid drv-gpu on the compositor's
  request (StartGpu). A seat daemon restart makes the compositor exit and
  come back.

drv-authd (uid drv-auth, spawner child)
  argon2id PIN in /var/lib/drv-auth (0700), escalating delay after 5 misses;
  Verify arrives on connections the spawner attached as Verifier,
  Unlock{idle_timeout} goes to the one it attached as Compositor; no socket

drv-compositor (uid,      drv-bridge (uid)      drv-bus (uid)        app-<name> (uid each)
    spawner child)
  Lookup for each client    Lookup per peer       dbus-daemon with      launched by the
  D-Bus callers checked     notifications and     per-user own and      spawner, sandboxed,
  against grants            portals for apps      send policy           reach the identity
  no IPC socket, no                                                     socket to launch
  device groups
```

Android is the model: the zygote is root and forks on command from
`system_server`, which is the only thing holding its pipe; the package
manager owns UIDs and permissions, unprivileged. Here the spawner is the
zygote and the identity daemon is `system_server` plus the package
manager. The spawner has no filesystem socket and no allow list: its
only peer is the child it forked over a socketpair.

## Types (crate `drv-policy`)

`Global` enumerates the optional globals: dmabuf, layer shell, session
lock, data control, foreign toplevel, workspaces, output management,
gamma control, screencopy, image copy capture, virtual keyboard, virtual
pointer, input method, security context. Everything else
(`wl_compositor`, `xdg_wm_base`, `wl_shm`, seats, outputs, pointer
constraints, ...) is always advertised.

`Grant` is `lookup` (ask the identity daemon about other UIDs: the
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

`spawn`: the spawner channel. `Request { uid, groups, argv, env,
network }`, `Response::{Forked { pid }, Error}`, `CHANNEL_FD = 4`,
`Channel` (a mutex around the stream; one request at a time).

`PolicyClient`: `connect(path)` does the hello now so a missing daemon
fails at startup, `lookup(uid)` caches per UID and reconnects after an
error, `launch(app)`, `reconnect()` for another thread, 2 s timeouts.
There is no mode without a daemon.

## identity.toml

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
  `/run/drv/identity.sock` (mode 0666; the spawner exposes `/run/drv` to
  apps) may send `Launch { app }`; the launcher's desktop entries run
  `drv launch <name>`, key binds in the compositor do the same over its
  own connection. `app` is a manifest name; arguments are the manifest's
  `exec` and nothing else, so an app never receives caller-chosen
  arguments or environment. The daemon builds the environment from its
  `PATH`, `[env]`, the entry's `env` and `WAYLAND_DISPLAY`.
- Lookups of other UIDs need the `lookup` grant; every UID may look up
  itself.
- The spawner accepts `{uid, groups, argv, env, network}` only on the
  channel, only for UIDs in its `--range` and groups on its `--group`
  list (so the identity daemon cannot hand out `wheel`). It creates
  `/run/drv-apps/<uid>` (`XDG_RUNTIME_DIR`) and `/var/lib/drv-apps/<uid>`
  (`HOME`, cwd), mode 0700 owned by the UID, moves the child into
  `<spawner cgroup>/app-<uid>` (needs `Delegate=yes`), then `setgroups`,
  `setresgid`, `setresuid`, `PR_SET_NO_NEW_PRIVS`, exec with the request's
  environment and nothing else. Children are reaped and their exit logged.
- Sandbox (the spawner, as root, between fork and exec): a private mount
  namespace; fresh tmpfs on `/tmp` and `/dev/shm`; `/proc` with
  `hidepid=invisible`; and a fresh read-only tmpfs on `/run` holding only
  the app's own runtime directory plus the `--expose` entries (the
  identity socket directory, the apps' Wayland socket directory, the
  bridge socket directory, `opengl-driver`, `current-system`,
  `/run/pipewire`, `/run/pulse`). No system D-Bus, no services' bus, no
  other app's runtime directory, no setuid wrappers. No user namespaces
  anywhere. Apps without `network = true` also get a new, empty network
  namespace.
- The identity daemon runs as the `drv-identity` system user with no
  filesystem socket of its own: the spawner binds the public socket and
  hands it over as fd 3, the channel as fd 4. When it dies the spawner
  forks a new one.
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
  every connection is keyed on `SO_PEERCRED` plus the identity daemon's
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
- Daemon socket: `$DRV_IDENTITY_SOCKET`, else `/run/drv/identity.sock`.
  Cannot connect or hello fails: the compositor exits. A failed lookup
  later gives that client `AppPolicy::unknown()`.

## The lock

Locked is the default; the compositor holds a lease "unlocked until T"
that only `drv-authd` starts. The lock app (`services.drv.apps.lock`,
`drv-lock`, the one app with the `session-lock` global and `auth = true`)
draws the PIN screen and sends `Verify` down the connection the spawner
handed it at launch; on a match the daemon sends `Unlock{idle_timeout}` on
the compositor's connection, which the spawner handed both of them. The compositor
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
- No child of the spawner is root: it always switches to the requested
  UID, which is always inside its range.
- The spawner's only peer is the identity daemon it forked; there is no
  path to it from the filesystem.
- A global the policy denies is never in the client's registry. There is
  no second code path that hands out the same capability.
- Unknown UIDs get nothing; an unreachable daemon is the same as unknown.
- The compositor never reads a policy file, never forks, never execs,
  never launches on its own authority: a spawn key bind is a `Launch`
  request like any launcher's, and the GPU process is the seat daemon's
  child, from the daemon's configured binary, as `drv-gpu`.
- The compositor holds no device group, no udev socket and no VT. Every
  DRM and evdev fd comes from `drv-seatd`, which serves the one connection
  the spawner attached and opens only the seat's card and event nodes it
  announced itself (never render nodes or anything else under `/dev`).
- Only `drv-authd` unlocks. No Wayland request, key bind or D-Bus call
  starts a lease; the lock app cannot unlock even if compromised, it can
  only try PINs, and the daemon slows that down.
- Peers are handed over, never found: the auth daemon has no socket, and
  what it takes `Verify` from and pushes `Unlock` to are the fds the root
  spawner attached. Anything that is not the lock app has no path to it.

## Not yet

- The bridge forwards every `org.freedesktop.portal.*` interface alike;
  nothing yet narrows which portals an app may use, and the document
  portal's FUSE view is not exposed into the sandbox.
- The bridge drops notification actions, hints and close signals.
- Files: no UID owns the person's files yet
  ([NOTES-file-ownership](NOTES-file-ownership.md)).
- Autostart happens once, when the identity daemon starts; a compositor
  restart does not relaunch the apps that died with it.
- Network isolation is only on/off. Per-app firewalling is designed
  separately.
- Nothing kills a still-running app when its manifest goes away; nothing
  pushes policy changes to the compositor; sub-UID ranges; exit
  reporting and cgroup kill from the spawner.
