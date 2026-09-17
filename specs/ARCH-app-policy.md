# ARCH-app-policy: Per-UID client policy

## Status

Implemented on the `policy` branch of the niri fork at `/src/niri`
(pushed as `rho/policy`, on top of `gpu-process`). Identities are static
(fixed UIDs from the system configuration), launches are by app name only,
and the forker enforces groups and puts each app in a cgroup. Four crates:
`niri-policy` (types, protocol, compositor client), `niri-identity`
(`niri-identityd`, the unprivileged brain), `niri-forker` (`niri-forker`,
the root forker), `niri-bridge` (the UID-keyed desktop services server
and the shim on each app's private bus; notifications so far). Builds, passes tests, and runs end to end in the
KVM dev VM (`nix/dev-vm.nix`, `nix/dev-vm-run.sh` in the fork): seatd,
the TTY backend and the GPU process on a virgl GPU, apps as their own
UIDs with the sandbox below (mounts, processes, network), Chromium with
GPU, audio, network and a private session bus, a stock launcher. The NixOS
module `nix/module.nix` (`services.niri-desktop`, flake output
`nixosModules.default`) turns one app list into passwd entries,
`identity.toml`, the forker's allow and expose lists, the units, and a
launcher entry per app (`Exec=niri msg action spawn -- <name>`), so a
stock launcher (fuzzel, as the human's trusted tool) starts apps
through the compositor.
Implements the "identity and policy" part of
[DESIGN-multi-user-gui](DESIGN-multi-user-gui.md); sits beside
[ARCH-gpu-process-split](ARCH-gpu-process-split.md).

## Goal

Every app runs as its own UID. The compositor identifies a client by the
UID on its socket (`SO_PEERCRED`) and shows it only the Wayland globals its
policy grants. A client that never sees `zwlr_screencopy_manager_v1` cannot
bind it, so the capability check happens once, at the registry, instead of
per request in every protocol handler.

## Shape

```text
niri-forker (root)            niri-identityd (session user)        compositor core
  --allow 1000:100000:65536:    identity.toml: apps with fixed       accept -> SO_PEERCRED -> uid
    render                        uids, exec, groups, policy         Lookup{uid} -> AppPolicy
  {uid, groups, argv, env}      Lookup{uid} -> app's policy   <----    cached per uid
    from an allowed peer, uid   Launch{app, env} -> forker    <----  Launch on spawn keybind,
    and groups in its lists:      request, reply Launched{uid}         spawn-at-startup, cli
    dirs, cgroup, setgroups/                                         world-connectable apps
    setresgid/setresuid,                                               socket for other uids
    no_new_privs, exec
```

Android is the model: PackageManager assigns app UIDs (10000 to 19999,
plus 100000 per user) and holds permissions, unprivileged; zygote is root
and only forks on command from `system`. Here the identity daemon is the
package manager and the forker is zygote.

## Types (crate `niri-policy`)

`Global` enumerates the optional globals: dmabuf, layer shell, session
lock, data control, foreign toplevel, workspaces, output management, gamma
control, screencopy, image copy capture, virtual keyboard, virtual pointer,
input method, security context. Everything else (`wl_compositor`,
`xdg_wm_base`, `wl_shm`, seats, outputs, pointer constraints, ...) is
always advertised.

`AppPolicy { name, trusted, gpu, globals, icon }` is the record the
identity daemon will own. `allows(global)` is: trusted grants all; `gpu`
grants dmabuf; otherwise the global must be listed. `name` and `icon` are
what the compositor shows the user; apps never supply them.

`PolicyFile { default, app: [AppEntry { uid, uid-end?, ..AppPolicy }] }`
is the TOML shape. Overlapping UID ranges are rejected at load.

`PolicyStore::lookup(uid)` (daemon side) returns the entry covering the
UID, else `default`.

`rpc`: `Request::{Hello { version }, Lookup { uid }}`,
`Response::{Hello { version }, Policy(AppPolicy)}`, postcard payloads
behind a little-endian `u32` length, 64 KiB cap, requests answered in
order. `rpc::VERSION` is checked in the hello.

`PolicyClient` (compositor side): `connect(path)` does the hello now so a
missing daemon fails at startup, `lookup(uid)` caches per UID and
reconnects after an error, 2 s timeouts. There is no mode without a
daemon. `daemon::serve_connection` is the serving loop, reused by tests
over a socket pair (the test fixture's daemon says "everyone trusted").

## Launching

- `Launch { app, env }`: `app` is a manifest name in `identity.toml`;
  arguments are the manifest's `exec` and nothing else, so an app never
  receives caller-chosen arguments. `env` is what the compositor's
  children used to inherit: its session's `XDG_RUNTIME_DIR` and relative
  `WAYLAND_DISPLAY`, `NIRI_SOCKET`, the config's `environment {}` block,
  plus `NIRI_APPS_WAYLAND_DISPLAY` (the apps socket path). The daemon
  prepends its own `PATH`, `LANG`, `TZ`, `TERM` and the config's `[env]`.
  Apps on the human's own UID (launcher, bar: allowed only if every
  entry on that UID is `trusted`) keep the session and get the daemon's
  `HOME`; every other app gets the apps socket as `WAYLAND_DISPLAY` and
  the forker's `HOME` and `XDG_RUNTIME_DIR`.
- Every `[[app]]` has a fixed `uid`, generated from the system
  configuration alongside its passwd entry. The identity daemon never
  allocates; there is no registry file and no scratch identity. Sub-UID
  ranges (`isolated_app`) are the only planned dynamic use, not yet built.
- The forker accepts `{uid, groups, argv, env}` only from peers on its
  `--allow peer:start:count[:group,group]` list, checked with
  `SO_PEERCRED`, and only for UIDs in that peer's range or the peer's own
  UID and groups on that peer's list (so an unprivileged identity daemon
  cannot hand out `wheel`). For range UIDs it creates
  `/run/niri-apps/<uid>` (`XDG_RUNTIME_DIR`) and `/var/lib/niri-apps/<uid>`
  (`HOME`, cwd), mode 0700 owned by the UID, moves the child into
  `<forker cgroup>/app-<uid>` (needs `Delegate=yes`), then `setgroups`,
  `setresgid`, `setresuid`, `PR_SET_NO_NEW_PRIVS`, exec with a cleared
  environment. Children are reaped and their exit logged. It has no
  config file and no notion of an app.
- Sandbox (the forker, as root, between fork and exec, for range UIDs):
  a private mount namespace; fresh tmpfs on `/tmp` and `/dev/shm`;
  `/proc` with `hidepid=invisible`; and a fresh read-only tmpfs on `/run`
  holding only the app's own runtime directory plus the forker's
  `--expose` entries (bind-mounted directories or recreated symlinks:
  the apps' Wayland socket directory, `opengl-driver`, `current-system`,
  `/run/pipewire`). So no system D-Bus, no forker or identity socket, no
  setuid wrappers, no other app's runtime directory. Same UID plus this
  is the floor; anything more an app may reach is a group or a socket.
  No user namespaces anywhere. Apps without `network = true` also get a
  new, empty network namespace (`CLONE_NEWNET`): no interfaces but a
  down loopback.
- The identity daemon serves its own UID only (`SO_PEERCRED`), so only
  the compositor of the same human can look up policy or launch.
- `identity.toml` has a static `[env]` table (from the system
  configuration) every app gets: where the PipeWire socket is, for one.
- Audio is a group: PipeWire runs system-wide as its own user with its
  sockets mode 0660 group `pipewire`; an app whose manifest lists
  `groups = ["pipewire"]` (and whose forker allow list includes it) can
  connect, anyone else gets `EACCES`. WirePlumber's default access
  rules give such clients play and record but no management.
- Apps run as other UIDs cannot enter the session's `XDG_RUNTIME_DIR`, so
  with `NIRI_APPS_SOCKET=/run/niri/<user>/wayland` the compositor also
  listens on that absolute path, socket mode 0666. Anyone local may
  connect; the policy decides what they get, as with Android's binder
  services. Launched apps get that path as `WAYLAND_DISPLAY`.
- `spawn-sh` stays disabled: a shell string is not an app name.
- Desktop services (notifications today, portals later) go through
  `niri-bridge serve`, running as the human on the human's session bus
  (`dbus-daemon` unit `niri-session-bus`, socket in `/run/niri-session`,
  which apps never see). Its socket `/run/niri-bridge/bridge.sock` is
  mode 0666; every connection is keyed on `SO_PEERCRED` plus the identity
  daemon's answer for that UID, unknown UIDs are dropped, and what the
  human sees is the manifest name, never anything the app sent. An app
  with `bus = true` runs under `dbus-run-session -- niri-bridge app --
  <exec>`: a private bus in its own UID with the shim claiming
  `org.freedesktop.Notifications` and forwarding to the server. The shim
  is compatibility for apps that expect a bus, not a boundary; the
  sandbox already hides every other bus.

## Compositor behaviour

- `Niri::insert_client` reads the peer UID and stores the policy in
  `ClientState.policy` before the client is inserted, so the registry the
  client sees on its first roundtrip is already filtered.
- Every optional global is created with a filter from `client_allows(Global)`:
  the policy grants it and the connection is not a security-context
  (sandboxed) one. Security-context clients see nothing optional regardless.
- The dmabuf global (created in `backend/tty.rs` once the GPU process is
  ready) is filtered the same way; a client without `gpu` gets `wl_shm` only.
- Gamma control additionally requires the TTY backend, as before.
- Daemon socket: `$NIRI_IDENTITY_SOCKET`, else
  `$XDG_RUNTIME_DIR/niri-identity.sock`. Cannot connect or hello fails: the
  compositor exits. Nobody is trusted unless the daemon says so; the
  human's own UID needs an `[[app]]` entry with `trusted = true`.
- Once connected, a failed lookup (daemon died, garbage reply, timeout)
  gives that client `AppPolicy::unknown()`: nothing optional, no GPU.
  Cached UIDs keep their answers. Fail closed, never open.
- `niri-identityd --config /etc/niri/identity.toml [--socket ..] [--forker ..]`
  and `niri-forker --allow peer:start:count[:groups] [--socket ..]`, both
  also under systemd socket activation (`LISTEN_FDS=1`).

## No spawning

The compositor never forks. `spawn`, `spawn-at-startup` and the
command-line command become `Launch` requests (`Niri::launch`);
`spawn-sh` and xwayland-satellite are logged as disabled, the
`utils::xwayland` module is gone, and `DISPLAY` is unset.

## Invariants

- Identity is the socket UID. PIDs are reused and are never used for policy.
- No child of the forker is root: a root forker always switches to the
  requested UID, also when a peer asks for its own UID. (A first version
  skipped the switch for "as self" launches and left the child as root;
  the compositor then saw an unknown UID and gave it nothing, but that
  was a privilege escalation reachable from the identity daemon.)
- A global the policy denies is never in the client's registry. There is no
  second code path that hands out the same capability.
- Unknown UIDs get the daemon's `default`; an unreachable daemon is the
  same as `unknown`: nothing optional, no GPU.
- The compositor never reads a policy file. Only the daemon knows where
  policy comes from.

## Not yet

- Portals for other UIDs. The bridge carries notifications only; the
  portal interfaces (file chooser, screen share, camera) on the app's
  private bus, forwarded to a UID-keyed portal service on the human's
  side, are not built. See [NOTES-dbus-per-app](NOTES-dbus-per-app.md).
- The bridge drops notification actions, hints and close signals.
- Network isolation is only on/off (`network = true` in the manifest, off
  by default: a fresh, empty network namespace). Per-app firewalling is
  designed separately.
- Nothing kills a still-running app when its manifest goes away.
- Nothing pushes policy changes to the compositor; lookups are cached per
  UID for the compositor's lifetime.
- Sub-UID ranges, and a way for an app to ask for them.
- The forker does not yet pass fds (a log fd, a pre-connected socket) or
  report exits to the identity daemon; nothing kills an app's cgroup yet.
- Per-client `wl_shm` sealing and other per-request policy.
- Any policy on what a client may do once it has bound a global.
