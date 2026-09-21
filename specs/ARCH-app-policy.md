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
shm buffers, sealing), `drv-lock` (the lock screen), `drv-menu` (the
app menu) and `drv-portal` (the file chooser and the documents mount),
supervisor services on `drv-ui`. Names are in [CONTEXT.md](../CONTEXT.md).
Builds, passes tests, and runs
end to end in the KVM dev VM (`nix/dev-vm.nix`, `nix/dev-vm-run.sh` in
the fork); `nix/smoke.sh` drives a fresh boot through unlock, the
isolation probe, notification, chooser, cast, mic, OpenURI and the
revoke and fails on missing evidence; `nix/drill.sh` then kills each
member and checks the set restarts, the apps die with it and the
desktop comes back. The same two scripts (`VM_BACKEND=m2`) run against a
crosvm guest on an Apple M2 (`nix/m2-vm.nix`, `nix/m2-vm-run.sh`), where
the GPU process drives the real Asahi GPU through a virtio-gpu native
context and the screen is a window on a headless host compositor served
over noVNC. The NixOS module `nix/module.nix` (`services.drv`, flake
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
UIDs) are `grants` on the same record.

## Process tree

```text
drv-supervisor (the systemd unit; uid drv-supervisor with CAP_SETUID, SETGID,
                SETPCAP, CHOWN, KILL, SYS_ADMIN, SYS_TTY_CONFIG from the unit's
                AmbientCapabilities, NoNewPrivileges; nothing here is root)
  starts drv-seatd, drv-authd, compositor-gpu (niri gpu-process, uid
  drv-gpu), the compositor, the locker (drv-lock, uid drv-lock), the
  menu (drv-menu, uid drv-menu), the portal (drv-portal, uid
  drv-portal), drv-forker (uid drv-forker), drv-appd and the bridge
  (drv-bridge, uid drv-bridge) as their users, from its own
  command line; takes input from nobody. A non-root service keeps only
  the capabilities listed for it (`--seatd-cap`, `--forker-cap`),
  ambient, as its whole bounding set. Every member gets a sandbox on the
  host root (`drv_os::sandbox::Sandbox`): a private mount namespace,
  fresh `/tmp` and `/dev/shm`, `/proc` with hidepid, a read-only `/run`
  holding only its `--<member>-expose` entries, and an empty network
  namespace (the forker keeps the host's: apps with `network` get it
  from there). Apps get a root of their own instead (below). Today: seatd `/run/udev`; the compositor
  `/run/udev` (libinput), its runtime and apps socket directories,
  `/run/drv`, the session bus and `/run/pipewire`; the GPU process
  `/run/opengl-driver`; the forker `/run/drv-apps` plus everything an
  app may be shown; the bridge `/run/drv` and the session bus; authd,
  the locker, the menu, the portal and drv-appd nothing.
  Nobody has the system bus (`/run/dbus`): the compositor's logind and
  locale1 watchers fail closed and log it.
  Every link between two members is
  a socketpair the supervisor makes before the first fork; each member
  gets its ends at startup as named fds (`drv_os::fds`: the systemd
  LISTEN_FDS/LISTEN_FDNAMES convention, fds 3.. with names in order):
    seatd        compositor
    authd        compositor, locker
    gpu          compositor
    compositor   seat, auth, gpu, locker, appd, menu, menu-client,
                 portal-client, portal (the cast line)
    locker       compositor (its Wayland connection), auth
    menu         compositor (a byte per show-launcher), wayland (its
                 Wayland connection), appd (its launch channel)
    portal       wayland (its Wayland connection), compositor (the
                 cast line), bridge (chooser and cast requests), fuse
                 (the /dev/fuse end of the documents mount the
                 supervisor made at --docs, /run/drv-doc)
    forker       channel
    appd         listener (/run/drv/appd.sock, bound by the supervisor,
                 0666), channel (to the forker), compositor and menu
                 (the two launch channels)
    bridge       listener (/run/drv-bridge/bridge.sock, bound by the
                 supervisor, 0666), portal
  seat, auth, the forker channel, the bridge listener, the bridge's
  portal line and the portal's cast line are SEQPACKET, the rest
  streams.
  Nothing is linked at runtime: the ten are one set, and when any
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

drv-portal (uid drv-portal; one process, sealed like the locker with
            file writes allowed; owns the person's files, `--files`)
  fd `bridge`: Choose{id, app, uid, title, Open | Save{name}} and
  Cast{id, app, uid, cursor} from the bridge, one dialog at a time on
  a layer surface. Choose shows the tree under --files,
  Enter descends or picks (Save: types a name; an existing one is
  picked to overwrite), Escape cancels; Cancel{id} takes a request
  down unanswered. Cast lists the screens and windows the compositor
  reports (fd `compositor`: Outputs and Windows are asked with every
  request; a window shows its app's manifest name first, its own title
  after); Enter sends Start{cast: id, source, cursor} down that line and
  the dialog goes down; Started{node_id, size} is answered as Cast{id,
  node_id, source, size}, Stopped as Closed{id}; Cancel{id} on a live
  cast sends Stop. A pick opens the file itself (O_NOFOLLOW, regular
  files only, created for Save) and files a grant {uid, name, fd,
  write}; the answer is Chosen{paths: ["/run/drv-doc/<id>/<name>"]}.
  fd `fuse`: the documents mount, served in a thread (fuser): the
  kernel reports the caller's UID on every request, a grant's directory
  and file exist only for that UID (a stranger gets ENOENT), reads and
  writes go through the held fd, writes and truncation only on a Save
  grant. Apps see the mount because `/run/drv-doc` is on their expose
  list; the forker binds it from the mount the supervisor made before
  the set started, so a set restart drops every grant with the apps.

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
  passes the fds. libseat's builtin backend forks the seatd server that does
  the opening; the seccomp seal goes on before that fork, so both processes
  wear it (the parent drops fork and netlink again once the seat and udev
  are up). A seat daemon restart makes the compositor exit and come back. Sits on its own VT (`--vt`, default 7, above logind's
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
compositor, the bridge); there is no other grant.

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
name = "flower"
uid = 100003
exec = ["/nix/store/.../weston-flower"]
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
- The app root (`drv_os::sandbox::Root`, DESIGN-app-namespace; planned
  by drv-forker before the fork, applied between fork and exec with
  CAP_SYS_ADMIN, ending in `pivot_root`): a fresh read-only tmpfs
  holding `/nix/store` (read-only, nosuid), `/etc` (a store path built
  per app by the module: passwd and group for its own UID, nsswitch,
  hosts, machine-id, localtime, CA bundle and an empty `resolv.conf`
  with the host's bound over it for `network` apps, plus the
  `services.drv.etc` entries of the host's), `/dev` and `/sys` (the
  host's generated views under `/run/drv-host`, written by
  `drv-host-views.service` at boot: basic nodes and the CPU topology;
  gpu apps also get the render nodes and their device directories), a
  fresh tmpfs on `/dev/shm`, `/proc` with `hidepid=invisible`, its home
  at `/var/lib/drv-apps/<uid>` and its `/tmp` from
  `/run/drv-apps/tmp/<uid>` (kept between launches, noexec), and `/run`
  holding only its own runtime directory plus the `--expose` entries
  (the appd socket directory, the apps' Wayland socket directory, the
  bridge socket directory, `opengl-driver`, `/run/drv-audio` and
  `/run/drv-pulse` for audio apps). No host `/etc`, `/var`, `/home`,
  `/run/current-system`. HOME is `/home/<name>`, a 256M tmpfs of the
  run, with `/var/lib/drv-apps/<uid>` bound at `.state` inside it. The
  forker locks the securebits, drops its capabilities and execs not the
  app but `drv-trampoline`, as the app: it makes the manifest's `state`
  directories under `.state` and links them from HOME, links the `files`
  defaults from the store, applies Landlock (read and execute on the
  closure of the manifest's command, `/etc`, the data profile and the
  graphics drivers, from `closureInfo`; read on `/etc`, `/sys`, `/proc`
  and the exposed `/run` entries; read, write and ioctl on `/dev`;
  everything on HOME, `/tmp`, `/dev/shm`, its runtime directory and the
  documents mount; abstract sockets and signals scoped to the app),
  refuses writable-then-executable memory unless the manifest says
  `jit`, and execs the command. No system D-Bus, no services' bus, no
  other app's runtime directory, no setuid wrappers. No user namespaces
  anywhere. Apps without `network = true` also get a new, empty network
  namespace.
- drv-appd runs as the `drv-appd` system user with no filesystem socket
  of its own: the supervisor binds the public socket and hands it over as
  fd `listener`, the channel to the forker as `channel`. There is one
  set: when any member dies the supervisor kills the apps through
  `apps/cgroup.kill` and restarts everything, and autostart brings the
  apps back.
- Audio is a manifest flag, `audio = true`: PipeWire runs system-wide,
  and such an app gets the apps' socket (`/run/drv-audio/apps`; the
  daemon tags every connection through it `pipewire.access = "drv-app"`
  with the uid it saw) and its own `pipewire-pulse` on it
  (`/run/drv-pulse/<app>`), nothing else. What a tagged client sees
  and may do is `nix/drv-access.lua` in WirePlumber: play and list
  devices freely, nothing of another app's, capture only under a
  grant. A capture stream (audio, sink monitors included, is "mic";
  video is "camera") with no grant waits unlinked while the script asks
  the bridge through the bridge's `drv-access` metadata
  (`request:<uid>:<kind>`), the bridge asks drv-portal, drv-portal asks
  the person; yes writes `grant:<uid>` and the stream links, no
  destroys it. A grant lasts until the app's last connection closes or
  the person revokes everything with Mod+Shift+Esc, which destroys the
  streams and disconnects camera remotes. Cameras also come the portal
  way: `org.freedesktop.portal.Camera.AccessCamera` asks the same
  question, and `OpenPipeWireRemote` hands out a connection that sees
  every camera node (browsers use that, not V4L2).
- The compositor listens on `$DRV_APPS_SOCKET` (`/run/drv-wayland/wayland`,
  mode 0666) besides its own runtime directory. Anyone local may connect;
  the policy decides what they get, as with Android's binder services.
- `spawn-sh` and `spawn-at-startup` are disabled: a shell string is not
  an app name, and what starts with the desktop is `autostart` in the
  manifest, not the compositor's config.
- Desktop services (notifications and portals) go through `drv-bridge
  serve`, a member of the set on the services' bus (a `dbus-daemon` as
  user `drv-bus`, socket in `/run/drv-session`, which apps never see).
  The bus config lets the notification daemon (`services.drv.notifier`,
  mako by default: a member of the set as uid drv-notifier, its Wayland
  connection the supervisor's fd 3) own `org.freedesktop.Notifications`
  and nobody else own anything; apps are never on it. The bridge socket `/run/drv-bridge/bridge.sock`
  is bound by the supervisor (fd `listener`), mode 0666;
  every connection is keyed on `SO_PEERCRED` plus drv-appd's
  answer for that UID, unknown UIDs are dropped, and what the human sees
  is the manifest name. An app with `bus = true` runs under
  `dbus-run-session -- drv-bridge app -- <exec>`: a private bus in its
  own UID with the shim claiming `org.freedesktop.Notifications` and
  `org.freedesktop.portal.Desktop`. The shim terminates all of the
  app's D-Bus (handles, sessions, `Response` and `Closed` signals, the
  in-place answers) and speaks `drv_bridge::wire` to the server:
  postcard over SEQPACKET, a fixed set of small variants (`Choose`,
  `Cast`, `CastRemote`, `CastClose`, `Camera`, `CameraRemote`,
  `CameraPresent`, `Open`, `Notify`, `Cancel`), texts clipped at 2 KiB,
  file descriptors only from server to shim. The server never parses
  D-Bus from an app. The shim is compatibility, not a boundary: it runs
  as the app and can lie, and everything it says is checked as if the
  app said it.
- Screen sharing is ours: `org.freedesktop.portal.ScreenCast` (version
  4, source types monitor and window, cursor modes
  hidden/embedded/metadata) and
  `org.freedesktop.portal.Session` on the app's bus are answered by the
  bridge. `CreateSession` and `SelectSources` are bookkeeping there;
  `Start` asks drv-portal (`Cast{id, app, uid, cursor, screens, windows,
  again}`, the two flags from `SelectSources.types`), whose dialog lists
  what the compositor reports and is the consent;
  a consent gets a token (`Response::Cast.token`), handed to the app as
  `restore_token` when it asked for any `persist_mode`, and answered
  with `persist_mode` 1: a later `Cast` with it (`again`) from the same
  app and uid starts the same source with no dialog. drv-portal drops
  an app's tokens when the bridge says its connection ended
  (`Forget{app, uid}`), so a consent never outlives the app's run.
  Chromium needs this: its picker previews a screen in one session,
  then captures in a second with the first's token. The pick goes to
  the compositor over the portal's own cast line
  (`drv_portal::compositor`: Outputs, Windows, Start{cast, source,
  cursor}, Stop; back Outputs, Windows, Started{cast, node_id, size},
  Stopped), which starts the cast with no D-Bus and no grant involved,
  the line being the authority. The node comes back as `Cast{id,
  node_id, source, size}` and the bridge emits `Response` with `streams`
  (`source_type` 1 or 2, `id` the connector name or the window id).
  `OpenPipeWireRemote` is a PipeWire connection the bridge makes and
  restricts before handing it over: the client's permissions are set to
  the core, the one node, and read on the client-node factory the app
  makes its own stream node through (everything else none, as
  xdg-desktop-portal does), a round trip makes sure the daemon has
  them, then the fd is stolen from the core and sent as the reply. The permissions live in the daemon, so they hold
  whatever the app does with the fd. Linking is WirePlumber's, not the
  app's, and would join a stream from the remote to a default source
  the remote cannot see, so before replying the bridge marks the remote
  in its `drv-access` metadata under the client id (`drv.remote`:
  `node:<id>` for a cast, `camera` for cameras), and the script lets a
  marked client's streams reach that only, destroying any other. The
  mark goes when the client does, and a cast's remotes are disconnected
  when the cast ends. WirePlumber would hand every new
  client everything a moment later, so a WirePlumber rule keyed on the
  bridge's uid (set by PipeWire from the socket, not forgeable) gives
  the bridge's clients no default permissions and no permission
  manager; the bridge has no other use for PipeWire. `Session.Close`, `Request.Close`
  before consent, or the app's connection ending send `Cancel{id}`,
  which takes the dialog down or stops the cast; the compositor ending
  it (`stop-all-casts`, the output going away) comes back as `Stopped`,
  then `Closed{id}`, then the `Session.Closed` signal. While any cast
  is live the compositor draws the "Screen is being shared" indicator
  above everything (never into the cast). The same indicator names the
  apps holding the microphone and the camera: drv-portal sends
  `Devices{mic, camera}` down its cast line whenever a grant starts or
  ends, and the compositor is the only one who can draw there.
- The file chooser is ours: `org.freedesktop.portal.FileChooser`
  (`OpenFile`, `SaveFile`, `version` 4) on the app's bus is answered by
  the bridge itself, which asks drv-portal down its supervisor link
  (`drv_portal::protocol`, postcard over SEQPACKET) with the manifest
  name and UID, hands the handle back at once and emits the `Response`
  signal (`uris` as `file:///run/drv-doc/<id>/<name>`) when the person
  has picked; `Request.Close` cancels at the portal. `directory` and
  `SaveFiles` are refused. Apps get `GTK_USE_PORTAL=1`. Nothing about
  this goes through xdg-desktop-portal, and no app ever sees the
  person's tree, only the file it was given, as its own UID.
- `org.freedesktop.portal.OpenURI` (`OpenURI` only, `version` 1) the
  bridge answers by asking drv-appd, over a launch channel of its own
  (`Request::Open{uri}`), to start the app whose manifest `opens` the
  scheme with the URI as its last argument: the one argument that ever
  comes from outside the manifest, and only a well-formed absolute URI
  (`rpc::uri_scheme`: ASCII printable, RFC 3986 scheme, at most 8 KiB)
  for a scheme some app declares; one handler per scheme. No prompt:
  the handler is what the manifest says it is. `writable`, `ask` and
  the parent window are ignored; `OpenFile` and `OpenDirectory` are
  not offered. The forker binds an app's `/tmp` from
  `/run/drv-apps/tmp/<uid>` (kept between launches, gone with the boot) so a second
  launch reaches the first one's single-instance socket instead of
  fighting it over the profile.
- No other portal. `org.freedesktop.portal.Settings` (version 2:
  `Read`, `ReadOne`, `ReadAll`) the bridge answers itself with the one
  look every app gets (`org.freedesktop.appearance`: `color-scheme` 1,
  dark; `contrast` 0). Every other portal call gets
  `org.freedesktop.DBus.Error.UnknownMethod`: chromium asks for
  `Secret` and `Realtime` and goes on without them. Nothing an app says
  reaches the services' bus; xdg-desktop-portal is not installed.
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
- No D-Bus services of its own: the Mutter screencast, service-channel
  and Shell screenshot services are gone, and the rest of upstream's
  (display config, screensaver, introspect, a11y) never start under the
  supervisor (not a session instance; the bus lets it own no name).
  Casts start only down the drv-portal link (`src/screencasting`).
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
there, and so is every cast, windows included.
Input extends the lease by `idleTimeout` (module option, 300 s); a
visible idle-inhibiting surface extends it too; `lock-session` (bound to
Super+Alt+L) ends it; a compositor restart starts locked. The lease is a
`CLOCK_BOOTTIME` deadline and `Niri::check_lease` is the one place that
turns "expired" into "locked": it runs before every frame, before every
input event and once a second, so nothing is drawn or delivered on a
stale lease, and suspend needs no hook (the clock runs while asleep);
waking locks at once regardless: the first `check_lease` after resume
sees `CLOCK_BOOTTIME` jump ahead of `CLOCK_MONOTONIC`. The
kernel's own replay of the last framebuffer on resume is switched off by
`nix/linux-drm-blank-on-resume.patch` (`drm_kms_helper.blank_on_resume=1`,
set by the module): the DRM resume helper commits the saved state with
every plane detached, so wake shows black until the compositor's first
commit. `nix/resume-vm.nix` plus `nix/resume-test.sh` check that on QXL.
Locking (idle, `lock-session`, waking) also
stops every cast and revokes every microphone and camera grant, as
Super+Shift+Escape does: the person is gone, nothing streams on their
behalf. Enrol with `drv-authd set-pin`; the dev VM enrols
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
- The leaves are sealed. The compositor core, compositor-gpu, drv-seatd
  (both of its processes), drv-authd, drv-appd, drv-portal, drv-menu and
  the locker apply one seccomp allowlist (`drv_os::seccomp`: the fds they
  hold, memory, threads, time, signals; never socket, exec, a new process
  or an ioctl outside the listed ones) once their fds are in place, plus
  what each needs: DRM/dma-buf/sync-file ioctls and a stat by path (Mesa
  reads its device's PCI ids off sysfs) for the GPU process; accept,
  read-only opens, anonymous files (the keymap copy for old
  wl_keyboards), DRM/dma-buf/sync-file/evdev ioctls, connecting to
  Unix sockets (PipeWire, per cast) and unlinking (its own socket and
  lock file on exit) for the core, which can therefore not write a
  screenshot to disk; opening existing nodes
  (never creating), DRM/evdev/VT ioctls and udev's database for
  drv-seatd; accept and read-only opens for drv-appd; its state
  directory for drv-authd; read-only opens (fonts) for the locker and
  the menu; file writes for drv-portal. A denied call fails with EPERM
  and the journal names the syscall number, the path for an open or
  stat, the request for an ioctl. `DRV_SECCOMP=0` from the supervisor's
  command line is the only way to run one open.
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

- The bridge forwards every other `org.freedesktop.portal.*` interface
  alike; nothing yet narrows which portals an app may use.
- The bridge drops notification actions, hints and close signals.
- Files: drv-portal owns the tree and hands out single files; no
  directory grants, no multiple selection, no filters, no overwrite
  confirmation, no file manager, no sync or backup
  ([NOTES-file-ownership](NOTES-file-ownership.md)).
- Network isolation is only on/off. Per-app firewalling is designed
  separately.
- Nothing kills a still-running app when its manifest goes away; nothing
  pushes policy changes to the compositor; sub-UID ranges; exit
  reporting and cgroup kill from drv-forker.
- Screen sharing: more than one source per session, consents that
  outlive the app's run (`persist_mode` 2).
- Portals apps may still want: screenshot. OpenURI has no consent and
  no rate limit: an app may start its handler as often as it likes.
- Capture is gated on the apps' socket only; a client on PipeWire's
  own sockets (a system service) is not. A revoked camera reaches the
  app as a connection error and no more: Chromium keeps the camera it
  enumerated until its capture service restarts.
- The GPU process dying restarts the set, apps included. Deliberate: a
  core that outlives its GPU process is not worth the buffer
  bookkeeping, and Wayland clients do not survive a compositor restart
  anyway.
- `OpenPipeWireRemote` timed out once in 27 tries, right after a cast
  was closed and restarted, and never under a stress loop since; the
  bridge names the round trip and tries once more before failing.
- Icons in the menu: reading image files an app controls needs a
  decoder in a sandbox first.
