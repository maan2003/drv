# NOTES-dbus-per-app: D-Bus for apps running as their own UIDs

## Problem

Apps launched by [ARCH-app-policy](ARCH-app-policy.md) run as UIDs other
than the session user. `dbus-daemon` and `dbus-broker` both refuse a
session-bus connection whose `SO_PEERCRED` UID differs from the bus
owner's, so those apps have no session bus at all: no portals, no
notifications, no MPRIS. Flatpak has the same problem inside its sandbox
and solves it with a filtering proxy per app; Android avoids it because
binder carries the caller UID and every service checks it.

## Candidates to fork or reuse (surveyed 2026-09)

- **xdg-dbus-proxy** (flatpak, C, GLib, LGPL). One process per app: a
  filtering proxy between the app and the real session bus with a
  SEE/TALK/OWN policy per well-known name, plus per-call and
  per-broadcast rules. Exactly the semantics we want, battle-tested
  (every Flatpak app). Small: one main file, `flatpak-proxy.c`. Downsides:
  C and GLib, an extra process per app, policy for a unique name is
  "sticky" (documented, rarely matters). Best fit if we accept C.
- **busd** (github.com/z-galaxy/busd, Rust, MIT, built on zbus, ~640
  commits). A whole broker in Rust. README says alpha, essentials only,
  no service activation. Plans peer credentials as an extra header
  field, which is the Android-style answer: one bus for everyone, the
  UID travels with each message. Best fit if we want one Rust bus per
  session with per-UID policy inside it, at the cost of finishing it.
- **dbus-broker** (bus1, C, Apache-2.0, the default on Arch and Fedora).
  Fast, has a real policy engine keyed by uid and gid at connect time.
  Its policy language is the XML busconfig one, so per-app policy means
  generating busconfig per UID. Forking it means C and a large codebase;
  configuring it might be enough without a fork.
- **dbus-daemon** (reference, C). Same policy language as dbus-broker,
  slower, no reason to pick it over dbus-broker.

## Leaning

Two viable shapes:

1. Keep the session bus as is and run `xdg-dbus-proxy` per app from the
   identity daemon, socket placed in the app's `XDG_RUNTIME_DIR`,
   policy from `identity.toml` (`talk = [...]`, `own = [...]`). Least
   code, proven, C.
2. Replace the session bus with a fork of `busd` that accepts any local
   UID and enforces per-UID policy from the identity daemon (same
   `Lookup` protocol). All Rust, one process, but busd needs finishing
   (activation, full spec) before it can carry a desktop.

Start with 1 to get portals working; 2 is the long-term shape if the
project wants everything in Rust.

## Decision (2026-09, implemented)

Neither shape above. A filtering proxy would make the bus a security
boundary, which the design forbids, and one shared bus with UID headers
means finishing a broker. Instead:

- The desktop services (compositor, notification daemon, portals, the
  bridge) share a plain `dbus-daemon` run as its own user, each service
  on its own UID; the bus config says which user may own which names and
  who may call the compositor's. Sandboxed apps never see its socket.
- An app that expects a bus gets a private `dbus-run-session` bus in its
  own UID with `drv-bridge app` on it: a shim that claims the desktop
  names (`org.freedesktop.Notifications` so far) and forwards over a Unix
  socket to `drv-bridge serve` on the services' side.
- The server is the only check: peer UID from `SO_PEERCRED`, name from
  the identity daemon, unknown UIDs dropped. What the human sees carries
  the manifest name, never the app's `app_name`.

Portals follow the same pattern: the shim also claims
`org.freedesktop.portal.Desktop`; the server forwards each app's calls on
its own bus connection, registered with the portal as `drv.app.<name>`,
rewriting handle paths so responses find their way back. File
descriptors (the PipeWire remote) pass through unchanged.
