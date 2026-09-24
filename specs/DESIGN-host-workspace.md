# DESIGN-host-workspace: The person's own terminal next to the apps

## Status

Agreed and implemented 2026-09 (niri `policy`: the compositor's
`HostOverlay` with the `toggle-host` action; the nixos repo's module
`services.drv.host`, which makes `drv-host.service`). Refines [DESIGN-multi-user-gui](DESIGN-multi-user-gui.md)
for the one thing that is not an app: administering the host from the desktop.
Details of the set are in [ARCH-app-policy](ARCH-app-policy.md).

## Problem

Apps are sandboxed accounts with no way to the host: no `run0`, no home, no
nixos-rebuild. Until now that meant switching to a VT and a separate session
of the host's own. That session is going away; the desktop needs a place to do
what a VT did, with the clipboard and the screen it already has.

The alternatives were rejected for widening what an app can be: a manifest
flag that lets an app skip the sandbox makes drv-appd able to launch
unsandboxed things; ssh to localhost from an app adds a network path and a
credential; polkit cannot tell the host user apart from an app user for
`run0 --user`, so "an app allowed to run0" is not a smaller privilege.

## Core idea

The host workspace is a service of the host's own, not an app and not a
member of the supervisor's set. systemd starts one terminal as the person's
own account with `PAMName=`, so pam_systemd registers a logind session for it:
the real home, the real system bus, and a session polkit can find, so `run0`
in it is plain `run0` (its agent registers with polkit, which refuses a
process it cannot map to a session; the supervisor's children are in none,
which is why the first version, a terminal forked by the supervisor, could
not run0). No new privileges all the same: run0 asks polkit, it does not
setuid; sudo is not a goal. drv-appd knows its UID by
name and can do nothing with it; the record grants it the drv agent, so ssh
from it uses the same keys as the apps that have `agent`. It is a
"better VT switch": the person's session is an overlay instead of a console.

Its Wayland connection is the apps' socket. drv-appd's manifest has a record
for the person's UID named `host`, with `gpu` and no exec, emitted by the
module from `services.drv.host`: the compositor learns the name the way it
learns every client's, and every door keys on the same record, so
notifications and the chooser work from the terminal. Nothing can launch it:
the record has no exec, and the forker refuses any UID outside the app range.
The record says `agent`: `SSH_AUTH_SOCK` in the terminal is drv-agent's door.
The compositor keeps the `host`
client's toplevels out of the layout altogether: they live in the host
overlay, drawn fullscreen over the workspaces and under the lock, the way
the lock screen is a layer and not a window. The `toggle-host` action shows
and hides it; while shown it holds the keyboard and the pointer, so the
shell's menu and notifications wait. A bind (Mod+Grave, the drop-down
terminal key; Ctrl-Alt-Fn stay VT switches) shows it; nothing else can launch
or show it. Locking
hides it. (A workspace was tried first: named workspaces sort first in niri,
and a workspace is one more thing to scroll past.)

The terminal is outside the set's group: the set's cgroup kill does not reach
it, so a compositor restart does not kill a `nixos-rebuild` in progress.
When the compositor goes its Wayland connection dies and the terminal exits
as any terminal would; the unit restarts it (five starts a minute at most).

## Non-goals

No second host account, no menu entry, no per-app "run on the host" grant.
The host workspace is the whole of the host's presence on the desktop.
