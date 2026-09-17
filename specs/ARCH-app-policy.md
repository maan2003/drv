# ARCH-app-policy: Per-UID client policy

## Status

Implemented on the `gpu-process` branch of the niri fork at `/src/niri`
(crate `niri-policy`, wired into `src/niri.rs`). Static TOML source only;
the identity daemon that will replace it is not written. Implements the
"identity and policy" part of [DESIGN-multi-user-gui](DESIGN-multi-user-gui.md);
sits beside [ARCH-gpu-process-split](ARCH-gpu-process-split.md).

## Goal

Every app runs as its own UID. The compositor identifies a client by the
UID on its socket (`SO_PEERCRED`) and shows it only the Wayland globals its
policy grants. A client that never sees `zwlr_screencopy_manager_v1` cannot
bind it, so the capability check happens once, at the registry, instead of
per request in every protocol handler.

## Shape

```text
launcher / identity daemon (later)          compositor core
  allocates a UID per app                     accept() -> SO_PEERCRED -> uid
  owns the AppPolicy records      ---------> PolicyStore::lookup(uid) -> Arc<AppPolicy>
  answers rpc::Request::Lookup                ClientState.policy
                                              global filters: policy.allows(Global)
today: /etc/niri/policy.toml -> PolicyStore (same types, no daemon)
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

`PolicyStore::lookup(uid)` returns the entry covering the UID, else
`default`, cached per UID. `PolicyStore::permissive()` trusts everyone.

`rpc::{Request::Lookup { uid }, Response::Policy(AppPolicy)}` are the
postcard-ready wire types for the daemon; not wired yet.

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
- Policy file: `$NIRI_POLICY`, else `/etc/niri/policy.toml`. Missing file
  means single-user mode: everyone trusted, one warning. A file that fails
  to parse is fatal; it never degrades to permissive.

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
- Unknown UIDs get `default`, which the file may leave as the nothing-optional,
  no-GPU baseline.

## Not yet

- The identity daemon and the launcher; `rpc` types exist, no socket.
- Per-client `wl_shm` sealing and other per-request policy.
- Any policy on what a client may do once it has bound a global.
