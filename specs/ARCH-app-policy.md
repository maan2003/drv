# ARCH-app-policy: Per-UID client policy

## Status

Implemented on the `policy` branch of the niri fork at `/src/niri`
(pushed as `rho/policy`, on top of `gpu-process`; crate `niri-policy`,
wired into `src/niri.rs`). The compositor asks a daemon over a socket;
`niri-policyd` is a file-backed stand-in for the identity daemon, which is
not written. Implements the "identity and policy" part of
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
policy daemon                               compositor core
  today: niri-policyd, answers from a         accept() -> SO_PEERCRED -> uid
    TOML file (PolicyStore)                   PolicyClient::lookup(uid) -> Arc<AppPolicy>
  later: identity daemon that also   <-----    (one Lookup per UID, cached; postcard
    allocates UIDs and launches apps           over $XDG_RUNTIME_DIR/niri-policy.sock)
  same rpc either way                         ClientState.policy
                                              global filters: policy.allows(Global)
```

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
- Daemon socket: `$NIRI_POLICY_SOCKET`, else
  `$XDG_RUNTIME_DIR/niri-policy.sock`. Cannot connect or hello fails: the
  compositor exits. Nobody is trusted unless a daemon says so; a
  single-user setup runs `niri-policyd` with a file whose default is
  `trusted = true`.
- Once connected, a failed lookup (daemon died, garbage reply, timeout)
  gives that client `AppPolicy::unknown()`: nothing optional, no GPU.
  Cached UIDs keep their answers. Fail closed, never open.
- `niri-policyd --policy FILE [--socket PATH]`, or under systemd socket
  activation (`LISTEN_FDS=1`), serves the TOML file.

## No spawning

The compositor does not spawn processes. `spawn`, `spawn-sh`,
`spawn-at-startup`, `spawn-sh-at-startup`, the command-line command, and
xwayland-satellite are all logged as disabled (`spawn_disabled`), the
`utils::xwayland` module is gone, and `DISPLAY` is unset. A separate
launcher, running with the identity daemon, starts apps under their UIDs.
The `environment {}` config block is still parsed into `CHILD_ENV` for
the launcher to consume later.

## Invariants

- Identity is the socket UID. PIDs are reused and are never used for policy.
- A global the policy denies is never in the client's registry. There is no
  second code path that hands out the same capability.
- Unknown UIDs get the daemon's `default`; an unreachable daemon is the
  same as `unknown`: nothing optional, no GPU.
- The compositor never reads a policy file. Only the daemon knows where
  policy comes from.

## Not yet

- The identity daemon and the launcher; `niri-policyd` is the stand-in.
- Nothing pushes policy changes to the compositor; lookups are cached per
  UID for the compositor's lifetime.
- Per-client `wl_shm` sealing and other per-request policy.
- Any policy on what a client may do once it has bound a global.
