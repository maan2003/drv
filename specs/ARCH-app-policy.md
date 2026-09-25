# ARCH-app-policy: Per-UID client policy

## Status

Implemented on the `policy` branch of the niri fork at `/src/niri`
(pushed as `rho/policy`, on top of `gpu-process`). Identities are static
(fixed UIDs from the system configuration), launches are by app name
only, nothing shares a UID, and there is no "trusted" flag: what a
process may do is its globals and grants. Crates: `drv-policy` (types,
the SEQPACKET transport `seq`, the forker channel, the compositor
client), `drv-supervisor` (starts the set with its sockets already made,
restarts it whole when anything dies, and gives up after five deaths in a
minute: every start takes the VT), `drv-appd` (the launcher for untrusted things, plus
the `drv` CLI), `drv-forker` (drv-appd's privileged helper: the sandbox
and the fork), `drv-os` (uid/gid lookups, the named startup fds `fds`, seccomp,
directory helpers),
`drv-dbus-shim` (the shim on each app's private bus: the desktop's D-Bus
names, answered by asking the set's services over their sockets),
`drv-seat` (`drv-seatd`, the seat and GPU-process parent), `drv-auth`
(`drv-authd`, the PIN verifier that unlocks the compositor), `drv-ui`
(what the set's windows share: a connection on a supervisor fd, the
toolkit boilerplate, text in shm buffers, sealing), `drv-shell` (the
layer above the compositor: lock screen, app menu, the person's prompts,
notifications) and `drv-files` (the file chooser and the documents
mount), supervisor services on `drv-ui`; `drv-cast` (screencasts,
cameras and microphones: consent at the shell, the compositor's streams,
PipeWire remotes), `drv-agent` (OpenSSH's ssh-agent behind a door that
admits the UIDs whose manifest says `agent`) and `drv-keys` (the media
keys: volume through PipeWire, backlight through sysfs, on the
compositor's word), supervisor services without a window. Every service
that apps reach (`drv-files`, `drv-cast`, `drv-shell`, `drv-agent`) keys
each connection on the peer UID and drv-appd's record for it
(`drv_policy::door`); there is no proxy between an app and a service. The
one member that is not a service and not an app is the host workspace
([DESIGN-host-workspace](DESIGN-host-workspace.md)): the person's own
terminal, started by the supervisor as their account. Names are in [CONTEXT.md](../CONTEXT.md).
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
over noVNC. The NixOS module (`services.drv`) lives in the nixos repo at
`config/system/drv/module.nix`, next to the m2sh manifests; it turns one
app list into passwd entries, `appd.json` and the units. The nixos repo
takes the binaries prebuilt and has no niri flake input; this repo's
development VMs import the module from the nixos repo.
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
                SETPCAP, SYS_ADMIN, SYS_TTY_CONFIG from the unit's
                AmbientCapabilities, plus CHOWN until the apps cgroup is made at
                startup, then dropped for good; NoNewPrivileges; nothing here
                is root; single-threaded, one waitid for the whole set)
  starts drv-seatd, drv-authd, compositor-gpu (niri gpu-process, uid
  drv-gpu), the compositor, drv-files (uid drv-files; first of the rest
  because it serves the documents mount, which anything touching
  `/run/drv/doc` blocks on), drv-forker (uid drv-forker), drv-appd, the
  shell (drv-shell, uid drv-shell), drv-cast (uid drv-cast, group
  pipewire), the ssh agent (drv-agent, uid drv-agent) and the media keys
  (drv-keys, uid drv-keys, groups pipewire and video, its `/sys`
  writable) as their users, from its own command line; takes input from
  nobody. The services that ask drv-appd who their callers are connect
  to its socket at startup and wait for it to answer. A non-root service keeps only
  the capabilities listed for it (`--seatd-cap`, `--forker-cap`),
  ambient, as its whole bounding set. Every member gets a root of its own,
  built by the same primitive as an app's (`drv_os::root`: a fresh tmpfs
  pivoted in, detached clones attached inside, the host's root detached):
  the store, the real `/dev` and `/sys` (the cgroup tree writable for the
  forker only), the host's `/etc` read-only, fresh `/tmp`, `/dev/shm` and
  `/proc` with hidepid, under `/run` only its `--<member>-expose` entries,
  its `--<member>-dir` directories, nothing else of the host's, and an
  empty network namespace (the forker keeps the host's: apps with
  `network` get it from there). The credential switch is shared too
  (`drv_os::creds`). `nix/kernel-state.sh` checks a member the way it
  checks an app. Apps' roots differ in content, not in kind (below). Today: seatd `/run/udev`; the compositor
  `/run/udev` (libinput), its runtime and apps socket directories,
  `/run/drv` and `/run/pipewire`; the GPU process `/run/opengl-driver`;
  the forker `/run/drv-apps` (where it builds the roots), `/run/drv-host`
  (the views) and `/run/drv` (the doors, bound into every app) plus the
  nix daemon's socket directory; the shell, drv-files and the agent
  `/run/drv` (drv-appd's socket, to name their callers), drv-cast
  `/run/drv` and `/run/pipewire`; authd and drv-appd nothing. Nobody has any D-Bus: not the system bus
  (`/run/dbus`; the compositor's logind and locale1 watchers fail closed
  and log it), and there is no session bus on the desktop at all. D-Bus
  exists only inside an app with `bus`, on its private daemon.
  Every link between two members is
  a socketpair the supervisor makes before the first fork; each member
  gets its ends at startup as named fds (`drv_os::fds`: the systemd
  LISTEN_FDS/LISTEN_FDNAMES convention, fds 3.. with names in order):
    seatd        compositor
    authd        compositor, locker (the shell's)
    gpu          compositor
    compositor   seat, auth, gpu, appd, menu (the poke line to the
                 shell), keys, shell-client, files-client, cast (the
                 cast line), apps (the apps' Wayland listener,
                 /run/drv/wayland, 0666)
    shell        wayland (its Wayland connection: session-lock and
                 layer-shell), auth, appd (its launch channel), poke (a
                 byte per show-launcher), cast and agent (their
                 questions for the person), listener
                 (/run/drv/notify.sock, bound by the supervisor, 0666)
    files        wayland (its Wayland connection), fuse (the /dev/fuse
                 end of the documents mount the supervisor made at
                 --docs, /run/drv/doc), listener
                 (/run/drv/files.sock, 0666)
    cast         compositor (the cast line), shell, listener
                 (/run/drv/cast.sock, 0666)
    agent        shell, listener (/run/drv/agent, 0666, a stream)
    keys         compositor
    forker       channel
    appd         listener (/run/drv/appd.sock, bound by the supervisor,
                 0666), channel (to the forker), compositor and shell
                 (the two launch channels)
  seat, auth, the forker channel, the cast line, the shell's lines from
  drv-cast and the agent, and the three app-facing listeners are
  SEQPACKET, the rest streams.
  Nothing is linked at runtime: the eleven are one set, and when any
  member exits the supervisor kills every app (writes 1 to
  `apps/cgroup.kill`) and every member (1 to `set/cgroup.kill`: each
  member's child put itself in that cgroup before switching user, so no
  CAP_KILL), waits, and starts the whole set again with fresh
  socketpairs. The forker owns `<supervisor cgroup>/apps` (chowned to
  it; both `cgroup.kill` files stay the supervisor's). The doors' directory
  (`/run/drv`, the supervisor's, with `doc` and PipeWire's `audio`), the
  apps' `/var/lib/drv-apps/<uid>` and the members' directories
  (`/run/drv-compositor`, the state directories) are tmpfiles rules,
  owned by their UID: the supervisor checks owner and mode and makes
  nothing. Every door lives in `/run/drv` and is world-connectable; the
  gate is on the service's side (the peer UID, drv-appd's answer).

drv-appd (uid drv-appd)                  drv-forker (uid drv-forker; caps setuid,
                                           setgid, setpcap, sys_admin)
  fds: listener, channel, compositor,      fd `channel` from the supervisor,
    shell                                    its only input. --range and where
  appd.json: every uid, exec, features,      host things live, from the command
    globals, grants, agent, autostart        line. Launch{name, uid, argv, env,
  Launch{app} on a launch channel ------->   network, gpu, audio, bus, jit,
    (the compositor's or the shell's) ->     closure} -> type checks, dirs,
    the manifest's exec; Apps -> the         root and ruleset built, cgroup
    names with an exec                       apps/app-<uid>, setresuid, NNP,
                                             Landlock, exec; the child
  Lookup{uid} on the listener: own uid,      inherits no fd at all
    or the lookup grant; Open{uri} there   reaps children, logs their exit
    for a uid that is an app
  autostart once, on the compositor's
    Hello down its channel (its apps
    socket listens by then)

drv-shell (uid drv-shell; one process, sealed after the first render;
           the layer above the compositor)
  The lock screen: fd `wayland` is its Wayland connection, inserted by
  the compositor as the one client with the session-lock global (and
  layer-shell), no lookup; it locks at start, draws the PIN screen,
  sends Verify down fd `auth` (drv-authd's fd `locker`), and on
  `finished` releases its surfaces and asks to lock again. The app
  menu: fd `poke` carries a byte per show-launcher bind; Apps on fd
  `appd` (its launch channel) fills a layer surface listing the names,
  typed filter, Up/Down, Enter -> Launch{app}, Escape -> gone. The
  person's prompts: fds `cast` and `agent` speak `drv_shell::ask`
  (Confirm{what, note}, Secret{prompt}, Touch{prompt}, Pick{choices},
  Cancel; back Yes, Secret, Picked{key}, Cancelled), each naming the
  app and uid; one dialog at a time, the rest queued, "<app> wants to
  <what>" as the header, Escape cancels. Notifications: fd `listener`
  is `/run/drv/notify.sock`, keyed on the peer UID and drv-appd's
  name for it (`drv_shell::notify`: Notify{summary, body}, Close{id};
  back Notified{id}); cards at the top right, the manifest name as the
  sender, text clipped at 2 KiB and rendered by the shell itself (the
  lock is the compositor's, so the shell may draw app text: a
  compromised shell shows things, it unlocks nothing). While locked
  the menu, the dialogs and the notes are down; a finished lock brings
  the queue back.

drv-files (uid drv-files; one process, sealed like the shell with file
           writes allowed; owns the person's files, `--files`)
  fd `listener`: `/run/drv/files.sock`, keyed like the shell's;
  `drv_files::wire`: Choose{req, Open | Save{name}} and Cancel{req}
  from the app's shim, one dialog at a time on a layer surface (fd
  `wayland`), "<app> wants to open/save a file" as the header. Choose
  shows the tree under --files, Enter descends or picks (Save: types a
  name; an existing one is picked to overwrite), Escape cancels;
  Cancel{req} takes a request down unanswered. A pick opens the file
  itself (O_NOFOLLOW, regular files only, created for Save) and files a
  grant {uid, name, fd, write}; the answer is Chosen{req, paths:
  ["/run/drv/doc/<id>/<name>"]}. fd `fuse`: the documents mount, served
  in a thread (fuser): the kernel reports the caller's UID on every
  request, a grant's directory and file exist only for that UID (a
  stranger gets ENOENT), reads and writes go through the held fd,
  writes and truncation only on a Save grant. Apps see the mount
  inside the doors' directory the forker binds into every root, from
  the mount the supervisor made before the set started, so a set
  restart drops every grant with the apps.

drv-cast (uid drv-cast, group pipewire; one process, not sealed:
          PipeWire loads plugins as it goes)
  fd `listener`: `/run/drv/cast.sock`, keyed like the shell's;
  `drv_cast::wire` from the app's shim: Cast{req, session, cursor,
  screens, windows, again}, CastRemote{session}, CastClose{session},
  CameraPresent, Cancel{req}. Cast asks the
  compositor (fd `compositor`, `drv_cast::compositor`) for Outputs and
  Windows and the person at the shell (fd `shell`, Pick: "see your
  screen"; a window shows its app's manifest name first, its own title
  after); Picked sends Start{cast, source, cursor} down the compositor
  line, Started{node_id, size} is answered as Cast{req, node_id,
  source, size, token}, Stopped as CastClosed{session}. The microphone
  and the camera are asked (Confirm, "use your camera") when
  WirePlumber's `drv-access` metadata reports a capture stream
  (`request:<uid>:<kind>`), and a yes writes `grant:<uid>`. Devices
  {mic, camera} go down the compositor line for its indicator. A cast's
  remote (CastRemote) is a PipeWire connection cut down to the one node,
  sent as an fd. Revoke from the compositor
  (Mod+Shift+Esc, locking) ends every cast and grant.

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
  tty. tty1 keeps its getty (ctrl-alt-f1). With `services.drv.homeVt`
  (`DRV_HOME_VT` on the compositor) the compositor switches to that VT once
  its outputs are up: the desktop starts in the background, beside a
  greetd session of the host's own, and ctrl-alt-f7 brings it forward; the
  release/acquire handshake is libseat's, answered by the compositor's
  pause and resume.

drv-authd (uid drv-auth, supervisor child)
  argon2id PIN in /var/lib/drv-auth (0700), escalating delay after 5 misses;
  Verify arrives on connections attached as Verifier (by the supervisor, or
  by drv-appd down its Verifiers socket, one per app launched with auth),
  Unlock{idle_timeout} goes to the one attached as Compositor; no socket

drv-compositor (uid,      drv-shell, drv-files, drv-cast,     app-<name> (uid each)
    supervisor child)       drv-agent (uid each)
  Lookup for each client    Lookup per peer on their own        forked by drv-forker
  no D-Bus at all           sockets; the shim in the app         on drv-appd's say,
  no IPC socket, no         speaks their wires; no bus, no       sandboxed; launch
  device groups             proxy, nothing in between            only with a channel
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

`Grant` is `lookup` (ask drv-appd about other UIDs: the compositor,
the shell, drv-files, drv-cast, the agent); there is no other grant.

`AppPolicy { name, gpu, globals, grants, icon, agent }`. `allows(global)`:
`gpu` grants dmabuf, otherwise the global must be listed. `has(grant)`.
`agent` is the ssh agent's door: drv-agent checks it on every
connection.
`name` and `icon` are what the compositor shows the user; apps never
supply them. `AppPolicy::unknown()` is nothing; `everything(name)` is
every global and grant, for tests.

`rpc`: `Request::{Hello, Lookup { uid }, Launch { app }, Apps, Open {
uri }}`, `Response::{Hello, Policy, Launched { uid }, Apps, Error}`,
postcard payloads behind a little-endian `u32` length, 64 KiB cap,
answered in order. `Launch` and `Apps` are answered only on a launch
channel (`Launcher`, one per holder); the public socket refuses them,
and a channel refuses `Lookup`. `Open` is answered on both: on the
public socket for a peer that is an app (the shim's OpenURI), never for
a stranger. `daemon::Handler { lookup, launch, apps, open, hello }`
gets the peer's UID from `SO_PEERCRED`; `serve` accepts every peer.
`door::Door` is the app-facing services' side of it: `open()` connects
to the public socket, `who(uid)` is the record or an error for a UID
drv-appd does not know, `serve(listener, tag, on)` accepts, refuses
strangers at accept and runs `on(sock, uid, policy)` per app
connection.

`seq`: the one transport for every daemon-to-daemon socket: `SOCK_SEQPACKET`,
one postcard message per datagram (64 KiB cap) with up to 16 fds in
`SCM_RIGHTS`. `drv_os::fds`: the startup fds by name (`LISTEN_FDS`,
`LISTEN_FDNAMES`; `take()` checks the count, unique non-empty names, that
each fd is a socket with `FD_CLOEXEC`, and unsets the variables;
`socket(name, kind)` checks AF_UNIX, the type and that it is not
listening, `listener(name)` the reverse; `handoff` builds the giving
side). `forker`: the channel between drv-appd and drv-forker (fd
`channel`): `Request::Launch(Launch)`, `Launch { uid, argv, env,
network, gpu, nix }`, `Response::{Forked, Error}`; `Channel` (a
mutex around the socket; one request at a time).

`PolicyClient`: `connect(path)` does the hello now so a missing daemon
fails at startup, `lookup(uid)` caches per UID and reconnects after an
error, `launch(app)`, `reconnect()` for another thread, 2 s timeouts;
`from_stream(sock)` for a launch channel (never reconnects: its peer
was handed over, not found).
There is no mode without a daemon.

## appd.json

```toml
wayland-socket = "/run/drv/wayland"           # every app's WAYLAND_DISPLAY

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
  side (spawn key binds) and on drv-shell's (what the menu picked), each
  a named fd on drv-appd's side. `Apps` on a channel lists the names
  `Launch` takes. Apps get no fd from anyone; something that must launch
  is a supervisor service, never an app. `/run/drv/appd.sock` (0666) answers
  lookups, and `Open` for apps. `app` is a manifest name; arguments are the manifest's
  `exec` and nothing else, so an app never receives caller-chosen
  arguments or environment. The daemon builds the environment from its
  `PATH`, `[env]`, the entry's `env` and `WAYLAND_DISPLAY`.
- Lookups of other UIDs need the `lookup` grant; every UID may look up
  itself.
- drv-forker accepts `Launch` only on the channel, only for UIDs in its
  `--range`; there are no lists to be on, the request is the app's
  features and the forker defines what each means. It forks before it
  decodes: the parent (one thread) receives bytes, forks and reaps,
  logging each exit; the child builds the root, moves itself into
  `<supervisor cgroup>/apps/app-<uid>` (needs `Delegate=yes`), then
  `setgroups`, `setresgid`, `setresuid`, `PR_SET_NO_NEW_PRIVS`, exec with
  the request's environment plus `HOME` and `XDG_RUNTIME_DIR`. The child
  inherits no fd but the channel, for its one reply.
- The app root (DESIGN-app-namespace, "Who does what"; built by the
  child in a mount namespace of its own, the fixed part before the
  request is decoded and the UID's part after, the host's root stacked
  out of reach beneath until then; `drv_os::mounts` is the new mount
  API it uses):
  a fresh read-only tmpfs
  holding `/nix/store` (read-only, nosuid), an empty `/etc` tmpfs of
  the app's own (the linker fills it from a store path built per app by
  the module: passwd and group for its own UID, nsswitch, hosts,
  machine-id, localtime, CA bundle, plus the `services.drv.etc` entries
  of the host's; `resolv.conf` is a copy of the host's live one, from
  the fd the forker hands a `network` app), `/dev` and `/sys` (the
  host's generated views under `/run/drv-host`, written by
  `drv-host-views.service` at boot: basic nodes and the CPU topology;
  gpu apps also get the render nodes and their device directories), a
  fresh tmpfs on `/dev/shm` (noexec), `/proc` with
  `subset=pid,hidepid=invisible` of the app's own PID namespace,
  `/run/drv` (the doors, read-only, the documents mount writable
  inside), the nix daemon's socket directory for `nix`, and `/state`
  from `/var/lib/drv-apps/<uid>`; the root itself is a tmpfs the app
  owns (noexec, size-capped), of the run. No host `/etc`, `/var`,
  `/home`, `/run/current-system`. The request is the parsed manifest
  (UID, argv, env, `network`, `gpu`, `nix`): the forker's one check is
  the UID range; every value is consumed by construction, and argv and
  env are read only once the process is the app. The child locks the
  securebits before the request, mounts as the app's effective UID
  (so what it makes is the app's), switches UID, drops its capabilities
  and execs the command as PID 1 of the app's PID namespace (IPC, UTS
  and cgroup namespaces are its own too). The module puts `drv-init` in
  front of every app's command, with what it needs as arguments: as the
  app, it makes `/tmp`, `/etc` (linked from the store, `resolv.conf`
  copied from the forker's fd), `/run/app` (`XDG_RUNTIME_DIR`), the
  links into the store (`/bin/sh`, `/usr/bin/env`, `/run/opengl-driver`,
  the manifest's `links`), HOME (`home = "run"`: `/home/app`, with the
  `state` directories under `/state` linked from it; `"persist"`:
  `/state` itself) with the `files` defaults linked from the store; then
  the Landlock ruleset: read and execute on the closure of the manifest's
  command, `/etc`, the data profile and the links' targets (from
  `closureInfo`; the whole store for `nix`); read on `/sys` and `/run`;
  read and write on `/proc`; read, write and ioctl on `/dev`; everything
  on `/etc`, HOME, `/tmp`, `/run/app`, `/state` and the documents mount,
  everything but execute on `/dev/shm`; abstract sockets and signals
  scoped to the app. Then the seccomp denylist (no executable memfd, no
  io_uring, no user namespace unless the manifest says `userns`), MDWE
  unless the manifest says `jit`, fork, and it stays as the app's init:
  reaps, forwards signals, exits with the app's status. No system D-Bus, no services' bus, no
  other app's runtime directory, no setuid wrappers. No user namespaces
  except for a `userns` app (the browser). Apps without `network = true` also get a new, empty network
  namespace.
- drv-appd runs as the `drv-appd` system user with no filesystem socket
  of its own: the supervisor binds the public socket and hands it over as
  fd `listener`, the channel to the forker as `channel`. There is one
  set: when any member dies the supervisor kills the apps through
  `apps/cgroup.kill` and restarts everything, and autostart brings the
  apps back.
- Audio is a manifest flag, `audio = true`: PipeWire runs system-wide,
  and such an app gets the apps' socket (`/run/drv/audio/apps`; the
  daemon tags every connection through it `pipewire.access = "drv-app"`
  with the uid it saw) and its own `pipewire-pulse` on it
  (`/run/drv/pulse/<app>`), nothing else. What a tagged client sees
  and may do is `nix/drv-access.lua` in WirePlumber: play and list
  devices freely, nothing of another app's, capture only under a
  grant. A capture stream (audio, sink monitors included, is "mic";
  video is "camera") with no grant waits unlinked while the script asks
  drv-cast through its `drv-access` metadata (`request:<uid>:<kind>`),
  drv-cast asks the person at the shell; yes writes `grant:<uid>` and
  the stream links, no destroys it. A grant lasts until the app's last connection closes or
  the person revokes everything with Mod+Shift+Esc, which destroys the
  streams and disconnects camera remotes. Cameras also come the portal
  way: `org.freedesktop.portal.Camera.AccessCamera` is granted without
  a question, because Chromium asks for access to list the cameras at
  the first page that enumerates devices, not when one captures; and
  `OpenPipeWireRemote` is the shim's own connection to the apps' socket
  (browsers use that, not V4L2), a tagged client like any, so the
  question comes when a page captures. An app without `audio` has no
  socket and no camera.
- The compositor serves the apps' Wayland socket the supervisor bound
  (`/run/drv/wayland`, mode 0666, fd `apps`; `$DRV_APPS_SOCKET` names a
  path to bind without a supervisor) besides its own runtime directory.
  Anyone local may connect; the policy decides what they get, as with
  Android's binder services.
- `spawn-sh` and `spawn-at-startup` are disabled: a shell string is not
  an app name, and what starts with the desktop is `autostart` in the
  manifest, not the compositor's config.
- Desktop services (notifications and portals) are the set's own
  services, each on a socket the supervisor bound (mode 0666) and
  handed over as fd `listener`: drv-files (`/run/drv/files.sock`),
  drv-cast (`/run/drv/cast.sock`), drv-shell (`/run/drv/notify.sock`),
  drv-agent (`/run/drv/agent`). Every connection is keyed on
  `SO_PEERCRED` plus drv-appd's answer for that UID
  (`drv_policy::door`), unknown UIDs are refused at accept, and what
  the human sees is the manifest name. There is no bus between apps and
  services and no server in front of them. An app with `bus = true`
  runs under `dbus-run-session -- drv-dbus-shim -- <exec>`: a private
  bus in its own UID with the shim claiming
  `org.freedesktop.Notifications` and `org.freedesktop.portal.Desktop`
  and connecting to each service's socket on first use.
- The ssh agent is a member, not an app: `drv-agent serve` runs OpenSSH's
  `ssh-agent` as uid drv-agent (the authenticators' hidraw nodes are that
  group's by udev rule, `/run/udev` exposed for libfido2) on a socket in
  its private `/tmp`, and fronts it on `/run/drv/agent` (fd `listener`,
  bound by the supervisor), which every app's root holds. Each connection is checked on `SO_PEERCRED` plus
  drv-appd's record for the UID (`AppPolicy.agent`, the manifest's
  `agent = true`; those apps get `SSH_AUTH_SOCK`) and refused at accept
  otherwise; ssh-agent itself would refuse every foreign uid. The
  authenticator's PIN never passes through an app: on a list or a sign
  request while the agent holds no keys and a hidraw node of ours is
  present, the door asks the shell (fd `shell`, `drv_shell::ask`:
  `Secret`, answered `Secret`/`Cancelled`; `Touch`, taken down by
  `Cancel`) naming the app, and runs `ssh-add -K` itself with the answer.
  ssh-agent's own prompts while signing (the PIN of a verify-required
  key, a touch) reach the door the same way: `SSH_ASKPASS` is drv-agent
  again, which carries the prompt over a socket in the private `/tmp`.
  Every message is relayed unread.
- The media keys are a member too: the compositor spawns nothing, so
  `volume-up`, `volume-down`, `volume-mute`, `mic-mute`, `brightness-up`
  and `brightness-down` are actions that write a line down its `keys`
  wire to `drv-keys`, which runs `wpctl` on the default sink or source
  and writes the backlight's `brightness` (group video, floor 2).
- The shim terminates all of the app's D-Bus (handles, sessions,
  `Response` and `Closed` signals, the in-place answers) and speaks the
  services' wires: postcard over SEQPACKET, a fixed set of small
  variants per service (`drv_files::wire`: Choose, Cancel;
  `drv_cast::wire`: Cast, CastRemote, CastClose, CameraPresent, Cancel; `drv_shell::notify`: Notify, Close), texts
  clipped at 2 KiB by the receiver, file descriptors only from service
  to shim. No service parses D-Bus. The shim is compatibility, not a
  boundary: it runs as the app and can lie, and everything it says is
  checked as if the app said it, because it could have.
- Screen sharing is ours: `org.freedesktop.portal.ScreenCast` (version
  4, source types monitor and window, cursor modes
  hidden/embedded/metadata) and
  `org.freedesktop.portal.Session` on the app's bus are answered by the
  shim. `CreateSession` and `SelectSources` are bookkeeping there;
  `Start` asks drv-cast (`Cast{req, session, cursor, screens, windows,
  again}`, the two flags from `SelectSources.types`), which asks the
  person at the shell (a `Pick` listing what the compositor reports),
  and that dialog is the consent;
  a consent gets a token (`Response::Cast.token`), handed to the app as
  `restore_token` when it asked for any `persist_mode`, and answered
  with `persist_mode` 1: a later `Cast` with it (`again`) from the same
  app and uid starts the same source with no dialog. drv-cast drops an
  app's tokens when the shim's connection ends, so a consent never
  outlives the app's run.
  Chromium needs this: its picker previews a screen in one session,
  then captures in a second with the first's token. The pick goes to
  the compositor over drv-cast's own cast line
  (`drv_cast::compositor`: Outputs, Windows, Start{cast, source,
  cursor}, Stop, Devices; back Outputs, Windows, Started{cast, node_id,
  size}, Stopped, Revoke), which starts the cast with no D-Bus and no
  grant involved, the line being the authority. The node comes back as
  `Cast{req, node_id, source, size, token}` and the shim emits
  `Response` with `streams` (`source_type` 1 or 2, `id` the connector
  name or the window id).
  `OpenPipeWireRemote` is a PipeWire connection drv-cast makes and
  restricts before handing it over: the client's permissions are set to
  the core, the one node, and read on the client-node factory the app
  makes its own stream node through (everything else none, as
  xdg-desktop-portal does), a round trip makes sure the daemon has
  them, then the fd is stolen from the core and sent as the reply. The permissions live in the daemon, so they hold
  whatever the app does with the fd. Linking is WirePlumber's, not the
  app's, and would join a stream from the remote to a default source
  the remote cannot see, so before replying drv-cast marks the remote
  in its `drv-access` metadata under the client id (`drv.remote`:
  `node:<id>` for a cast), and the script lets a
  marked client's streams reach that only, destroying any other. The
  mark goes when the client does, and a cast's remotes are disconnected
  when the cast ends. WirePlumber would hand every new
  client everything a moment later, so a WirePlumber rule keyed on
  drv-cast's uid (set by PipeWire from the socket, not forgeable) gives
  its clients no default permissions and no permission manager; its own
  connection is on the manager socket. `Session.Close`, `Request.Close`
  before consent, or the app's connection ending send `CastClose` or
  `Cancel{req}`, which takes the dialog down or stops the cast; the
  compositor ending it (`stop-all-casts`, the output going away) comes
  back as `Stopped`, then `CastClosed{session}`, then the
  `Session.Closed` signal. While any cast
  is live the compositor draws the "Screen is being shared" indicator
  above everything (never into the cast). The same indicator names the
  apps holding the microphone and the camera: drv-cast sends
  `Devices{mic, camera}` down its cast line whenever a grant starts or
  ends, and the compositor is the only one who can draw there.
- The file chooser is ours: `org.freedesktop.portal.FileChooser`
  (`OpenFile`, `SaveFile`, `version` 4) on the app's bus is answered by
  the shim, which asks drv-files over its socket (`drv_files::wire`;
  drv-files names the app from the socket's UID), hands the handle back
  at once and emits the `Response` signal (`uris` as
  `file:///run/drv/doc/<id>/<name>`) when the person has picked;
  `Request.Close` cancels at drv-files. `directory` and
  `SaveFiles` are refused. Apps get `GTK_USE_PORTAL=1`. Nothing about
  this goes through xdg-desktop-portal, and no app ever sees the
  person's tree, only the file it was given, as its own UID.
- `org.freedesktop.portal.OpenURI` (`OpenURI` only, `version` 1) the
  shim answers by asking drv-appd on the public socket
  (`Request::Open{uri}`, answered for a peer that is an app and logged
  under its name) to start the app whose manifest `opens` the
  scheme with the URI as its last argument: the one argument that ever
  comes from outside the manifest, and only a well-formed absolute URI
  (`rpc::uri_scheme`: ASCII printable, RFC 3986 scheme, at most 8 KiB)
  for a scheme some app declares; one handler per scheme. No prompt:
  the handler is what the manifest says it is. `writable`, `ask` and
  the parent window are ignored; `OpenFile` and `OpenDirectory` are
  not offered. An app's `/tmp` is of the run, so a browser that keeps
  its single-instance socket under `TMPDIR` points that at a `state`
  directory in its manifest: a second launch then reaches the first
  one's socket instead of fighting it over the profile.
- No other portal. `org.freedesktop.portal.Settings` (version 2:
  `Read`, `ReadOne`, `ReadAll`) the shim answers itself with the one
  look every app gets (`org.freedesktop.appearance`: `color-scheme` 1,
  dark; `contrast` 0). Every other portal call gets
  `org.freedesktop.DBus.Error.UnknownMethod`: chromium asks for
  `Secret` and `Realtime` and goes on without them. D-Bus ends in the
  shim; xdg-desktop-portal is not installed.
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
  Casts start only down the drv-cast link (`src/screencasting`).
- No IPC socket. `IpcServer` is never started: a client that can act as
  the compositor would bypass every policy. `niri msg` has nothing to
  talk to.
- Daemon socket: `$DRV_APPD_SOCKET`, else `/run/drv/appd.sock`.
  Cannot connect or hello fails: the compositor exits. A failed lookup
  later gives that client `AppPolicy::unknown()`.

## The lock

Locked is the default; the compositor holds a lease "unlocked until T"
that only `drv-authd` starts. The shell (`drv-shell`, uid drv-shell, a
supervisor service) draws the PIN screen. Its Wayland connection is its
startup fd `wayland` and the compositor inserted it as the one client
with the `session-lock` global, no lookup; its `drv-authd` connection is
fd `auth` (the daemon's fd `locker`). `Verify` goes
down that connection; on a match the daemon sends `Unlock{idle_timeout}`
on the compositor's connection, which the supervisor handed both of them.
The compositor then grants itself the lease and sends the lock client
`finished`; the shell releases its surfaces and immediately asks to lock
again, and the compositor holds that request (`LockState::Pending`) until
the lease ends, when it becomes the lock without anything being launched.
The shell also draws the menu, the prompts and the notifications; none
of that bears on the lock, which is the compositor's: a shell that dies
or lies leaves the outputs black.
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
  (both of its processes), drv-authd, drv-appd, drv-shell and drv-files
  apply one seccomp allowlist (`drv_os::seccomp`: the fds they
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
  directory for drv-authd; read-only opens (fonts), accept and
  connecting to Unix sockets (drv-appd) for the shell; the same plus
  file writes for drv-files. drv-cast is not sealed: PipeWire loads its
  plugins as it goes. A denied call fails with EPERM
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
  starts a lease; the shell cannot unlock even if compromised, it can
  only try PINs, and the daemon slows that down.
- Peers are handed over, never found: the auth daemon has no socket, and
  what it takes `Verify` from and pushes `Unlock` to are its startup fds
  `locker` and `compositor`. Anything that is not the shell has no path
  to it.

## Not yet

- Nothing narrows which portals an app may use: every `bus` app may ask
  for files, screens, cameras, notifications and URIs; the person
  answers each time.
- The shim drops notification actions, hints and icons; the shell has
  no close signal back and no history.
- Files: drv-files owns the tree and hands out single files; no
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
  was closed and restarted, and never under a stress loop since;
  drv-cast names the round trip and tries once more before failing.
- Icons in the menu: reading image files an app controls needs a
  decoder in a sandbox first.
