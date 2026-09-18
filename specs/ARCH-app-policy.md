# ARCH-app-policy: Per-UID client policy

## Status

Implemented on the `policy` branch of the niri fork at `/src/niri`
(pushed as `rho/policy`, on top of `gpu-process`). Identities are static
(fixed UIDs from the system configuration), launches are by app name
only, nothing shares a UID, and there is no "trusted" flag: what a
process may do is its globals and grants. Crates: `drv-policy` (types,
the SEQPACKET transport `seq`, the forker channel, the compositor
client), `drv-supervisor` (starts the set with its sockets already made,
restarts it whole when anything dies), `drv-appd` (the launcher for untrusted things, plus
the `drv` CLI), `drv-forker` (drv-appd's privileged helper: the sandbox
and the fork), `drv-os` (uid/gid lookups, the named startup fds `fds`, seccomp,
directory helpers),
`drv-bridge` (the UID-keyed desktop services server and the shim on each
app's private bus: notifications and portals), `drv-seat` (`drv-seatd`,
the seat and GPU-process parent), `drv-auth` (`drv-authd`, the PIN
verifier that unlocks the compositor), `drv-ui` (what the set's windows
share: a connection on a supervisor fd, the toolkit boilerplate, text in
shm buffers, sealing), `drv-lock` (the lock screen) and `drv-menu` (the
app menu), both supervisor services on `drv-ui`. Names are in [CONTEXT.md](../CONTEXT.md).
Builds, passes tests, and runs
end to end in the KVM dev VM (`nix/dev-vm.nix`, `nix/dev-vm-run.sh` in
the fork). The NixOS module `nix/module.nix` (`services.drv`, flake
output `nixosModules.default`) turns one app list into passwd entries,
`appd.toml`, the session bus policy and the units.
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
drv-supervisor (the systemd unit; uid drv-supervisor with CAP_SETUID, SETGID,
                SETPCAP, CHOWN, KILL, SYS_ADMIN, SYS_TTY_CONFIG from the unit's
                AmbientCapabilities, NoNewPrivileges; nothing here is root)
  starts drv-seatd, drv-authd, compositor-gpu (niri gpu-process, uid
  drv-gpu), the compositor, the locker (drv-lock, uid drv-lock), the
  menu (drv-menu, uid drv-menu), drv-forker (uid drv-forker) and
  drv-appd as their users, from its own
  command line; takes input from nobody. A non-root service keeps only
  the capabilities listed for it (`--seatd-cap`, `--forker-cap`),
  ambient, as its whole bounding set. Every member gets the sandbox an
  app gets (`drv_os::sandbox`, one primitive for both): a private mount
  namespace, fresh `/tmp` and `/dev/shm`, `/proc` with hidepid, a
  read-only `/run` holding only its `--<member>-expose` entries, and an
  empty network namespace (the forker keeps the host's: apps with
  `network` get it from there). Today: seatd `/run/udev`; the compositor
  `/run/udev` (libinput), its runtime and apps socket directories,
  `/run/drv`, the session bus and `/run/pipewire`; the GPU process
  `/run/opengl-driver`; the forker `/run/drv-apps` plus everything an
  app may be shown; authd, the locker, the menu and drv-appd nothing.
  Nobody has the system bus (`/run/dbus`): the compositor's logind and
  locale1 watchers fail closed and log it.
  Every link between two members is
  a socketpair the supervisor makes before the first fork; each member
  gets its ends at startup as named fds (`drv_os::fds`: the systemd
  LISTEN_FDS/LISTEN_FDNAMES convention, fds 3.. with names in order):
    seatd        compositor
    authd        compositor, locker
    gpu          compositor
    compositor   seat, auth, gpu, locker, appd, menu, menu-client
    locker       compositor (its Wayland connection), auth
    menu         compositor (a byte per show-launcher), wayland (its
                 Wayland connection), appd (its launch channel)
    forker       channel
    appd         listener (/run/drv/appd.sock, bound by the supervisor,
                 0666), channel (to the forker), compositor and menu
                 (the two launch channels)
  seat, auth and the forker channel are SEQPACKET, the rest streams.
  Nothing is linked at runtime: the eight are one set, and when any
  member exits the supervisor kills every app (writes 1 to
  `apps/cgroup.kill`), stops the rest, waits, and starts the whole set
  again with fresh socketpairs. The forker owns `<supervisor
  cgroup>/apps` (chowned to it; `cgroup.kill` stays the supervisor's)
  and `--forker-dir` parents for the apps' runtime and home directories.

drv-appd (uid drv-appd)                  drv-forker (uid drv-forker; caps setuid,
                                           setgid, setpcap, sys_admin, chown)
  fds: listener, channel, compositor,      fd `channel` from the supervisor,
    menu                                     its only input. --range, --group,
  appd.toml: every uid, exec, groups,        --expose from the command line.
    globals, grants, autostart             Launch{uid, groups, argv, env,
  Launch{app} on a launch channel ------->   network, expose} -> range and
    (the compositor's or the menu's) ->      group check, dirs, cgroup
    the manifest's exec; Apps -> the         apps/app-<uid>, sandbox,
    names with an exec                       setresuid, NNP, exec; the child
  Lookup{uid} on the listener: own uid,      inherits no fd at all
    or the lookup grant                    reaps children, logs their exit
  autostart once, on the compositor's
    Hello down its channel (its apps
    socket listens by then)

drv-menu (uid drv-menu; one process, no children, sealed after the first
          render like the locker)
  fd `compositor`: a byte per show-launcher bind; fd `wayland`: its
  Wayland connection, which the compositor inserted as a layer-shell
  client (no lookup, no manifest entry); fd `appd`: its launch channel.
  On each byte: Apps -> a layer surface listing the names, typed
  filter, Up/Down, Enter -> Launch{app}, Escape -> gone. Drawn with
  drv-ui, like the lock screen.

drv-seatd (uid drv-seat: groups video, input, tty; CAP_SYS_TTY_CONFIG)
  holds the seat (libseat builtin backend, no seatd) and udev; announces the
  seat's /dev/dri/card* and /dev/input/event* nodes to its one client, the
  compositor connection the supervisor attached (Hello lists them, hotplug
  follows on a second socket with enable/disable), opens only those and
  passes the fds. Forks nothing. A seat daemon restart makes the compositor
  exit and come back. Sits on its own VT (`--vt`, default 7, above logind's
  autovt range) because agetty resets the VT it owns to 0620 and a non-root
  seatd could not open it; a udev rule makes tty0 and that tty 0660 group
  tty. tty1 keeps its getty (ctrl-alt-f1).

drv-authd (uid drv-auth, supervisor child)
  argon2id PIN in /var/lib/drv-auth (0700), escalating delay after 5 misses;
  Verify arrives on connections attached as Verifier (by the supervisor, or
  by drv-appd down its Verifiers socket, one per app launched with auth),
  Unlock{idle_timeout} goes to the one attached as Compositor; no socket

drv-compositor (uid,      drv-bridge (uid)      drv-bus (uid)        app-<name> (uid each)
    supervisor child)
  Lookup for each client    Lookup per peer       dbus-daemon with      forked by drv-forker
  D-Bus callers checked     notifications and     per-user own and      on drv-appd's say,
  against grants            portals for apps      send policy           sandboxed; launch
  no IPC socket, no                                                     only with a channel
  device groups
```

Android is the model: the zygote is privileged and forks on command from
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
`Launch` is answered only on a launch channel (`Launcher`, one per
holder); the public socket refuses it, and a channel refuses `Lookup`.
`daemon::Handler { lookup(peer, uid), launch(peer, app) }` gets the
peer's UID from `SO_PEERCRED`; `serve` accepts every peer.

`seq`: the one transport for every daemon-to-daemon socket: `SOCK_SEQPACKET`,
one postcard message per datagram (64 KiB cap) with up to 16 fds in
`SCM_RIGHTS`. `drv_os::fds`: the startup fds by name (`LISTEN_FDS`,
`LISTEN_FDNAMES`; `take()` checks the count, unique non-empty names, that
each fd is a socket with `FD_CLOEXEC`, and unsets the variables;
`socket(name, kind)` checks AF_UNIX, the type and that it is not
listening, `listener(name)` the reverse; `handoff` builds the giving
side). `forker`: the channel between drv-appd and drv-forker (fd
`channel`): `Request::Launch(Launch)`, `Launch { uid, groups, argv, env,
network, expose }`, `Response::{Forked { pid }, Error}`; `Channel` (a
mutex around the socket; one request at a time).

`PolicyClient`: `connect(path)` does the hello now so a missing daemon
fails at startup, `lookup(uid)` caches per UID and reconnects after an
error, `launch(app)`, `reconnect()` for another thread, 2 s timeouts;
`from_stream(sock)` for a launch channel (never reconnects: its peer
was handed over, not found).
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
launched by the daemon, in order, when the compositor says hello on its
launch channel (its apps socket listens by then). `menu = false` keeps
an entry (a daemon, a probe) out of the app menu.

## Launching

- Launch authority is an fd, never a grant and never the public socket.
  drv-appd serves `Launch { app }` only on launch channels, and a
  launch channel is a supervisor socketpair: `appd` on the compositor's
  side (spawn key binds) and on drv-menu's (what the menu picked), each
  a named fd on drv-appd's side. `Apps` on a channel lists the names
  `Launch` takes. Apps get no fd from anyone; something that must launch
  is a supervisor service, never an app. `/run/drv/appd.sock` (0666) answers
  lookups only. `app` is a manifest name; arguments are the manifest's
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
  moves the child into `<supervisor cgroup>/apps/app-<uid>` (needs
  `Delegate=yes`), then `setgroups`, `setresgid`, `setresuid`,
  `PR_SET_NO_NEW_PRIVS`, exec with the request's environment and nothing
  else. The child inherits no fd. Children are reaped and
  their exit logged.
- Sandbox (`drv_os::sandbox`, applied by drv-forker between fork and
  exec with CAP_SYS_ADMIN; the supervisor applies the same one to every
  member of the set, with its own expose list): a private mount
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
  fd `listener`, the channel to the forker as `channel`. There is one
  set: when any member dies the supervisor kills the apps through
  `apps/cgroup.kill` and restarts everything, and autostart brings the
  apps back.
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
that only `drv-authd` starts. The locker (`drv-lock`, uid drv-lock, a
supervisor service) draws the PIN screen. Its Wayland connection is its
startup fd `compositor` and the compositor inserted it as the one client
with the `session-lock` global, no lookup; its `drv-authd` connection is
fd `auth` (the daemon's fd `locker`). `Verify` goes
down that connection; on a match the daemon sends `Unlock{idle_timeout}`
on the compositor's connection, which the supervisor handed both of them.
The compositor then grants itself the lease and sends the lock client
`finished`; the locker releases its surfaces and immediately asks to lock
again, and the compositor holds that request (`LockState::Pending`) until
the lease ends, when it becomes the lock without anything being launched.
Without a lease the outputs stay black whether or not a lock client is
there.
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
commit. `nix/resume-vm.nix` plus `nix/resume-test.sh` check that on QXL.
While locked, casts and screenshots render
only the backdrop. Enrol with `drv-authd set-pin`; the dev VM enrols
`1234` in the unit's pre-start.

## Invariants

- Identity is the socket UID. PIDs are reused and are never used for
  policy. No two processes on the desktop share a UID; there is no human
  UID on the desktop at all.
- No child of drv-forker is root or keeps a capability: it always
  switches to the requested UID, which is always inside its range, then
  empties its bounding, ambient, permitted, effective and inheritable
  sets (a non-root forker's would otherwise survive the UID switch and,
  ambient, the exec) before `PR_SET_NO_NEW_PRIVS`.
- The leaves are sealed. compositor-gpu, drv-authd, drv-appd and the
  locker apply one seccomp allowlist (`drv_os::seccomp`: the fds they
  hold, memory, threads, time, signals; never socket, exec, a new process
  or an ioctl outside the listed ones) once their fds are in place, plus
  what each needs: DRM/dma-buf/sync-file ioctls for the GPU process,
  accept and read-only opens for drv-appd, its state directory for
  drv-authd, read-only opens (fonts) for the locker. A denied call fails
  with EPERM and the journal names the syscall number. `DRV_SECCOMP=0`
  from the supervisor's command line is the only way to run one open.
- drv-forker's only peer is the channel the supervisor gave it, held by
  drv-appd; there is no path to it from the filesystem. The supervisor
  takes input from nobody.
- A global the policy denies is never in the client's registry. There is
  no second code path that hands out the same capability.
- Unknown UIDs get nothing; an unreachable daemon is the same as unknown.
- The compositor never reads a policy file, never forks, never execs,
  never launches on its own authority: a spawn key bind is a `Launch`
  request down the channel the supervisor linked to drv-appd (fd
  `appd`), and the GPU process is the supervisor's, from the
  supervisor's command line, as `drv-gpu`, linked to the compositor by
  fd `gpu`.
- The compositor holds no device group, no udev socket and no VT. Every
  DRM and evdev fd comes from `drv-seatd`, which serves the one connection
  the supervisor attached and opens only the seat's card and event nodes it
  announced itself (never render nodes or anything else under `/dev`). It
  is not root: the device groups open the nodes, `CAP_SYS_TTY_CONFIG`
  covers the VT ioctls, and DRM master needs no privilege for the process
  that opened the card.
- Only `drv-authd` unlocks. No Wayland request, key bind or D-Bus call
  starts a lease; the locker cannot unlock even if compromised, it can
  only try PINs, and the daemon slows that down.
- Peers are handed over, never found: the auth daemon has no socket, and
  what it takes `Verify` from and pushes `Unlock` to are its startup fds
  `locker` and `compositor`. Anything that is not the locker has no path
  to it.

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
- Portals become supervisor services, like the menu, once they need
  authority (launch with data, intents over the bus).
- Seccomp on drv-seatd (libseat, udev's netlink and the VT ioctls are
  not listed yet) and the compositor core.
- Icons in the menu: reading image files an app controls needs a
  decoder in a sandbox first.
