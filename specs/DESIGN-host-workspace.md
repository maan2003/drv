# DESIGN-host-workspace: The person's own terminal next to the apps

## Status

Agreed and implemented 2026-09 (niri `policy`: `drv-supervisor` `--host-*`
and the compositor's `match app=`; the nixos repo's module
`services.drv.host`). Refines [DESIGN-multi-user-gui](DESIGN-multi-user-gui.md)
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

The host workspace is a member of the supervisor's set, not an app. The
supervisor forks one terminal as the person's own account (`--host-user`),
on the host's own root: no namespaces, no root of its own, the real home, the
real system bus, so `run0` in it is plain `run0`. drv-appd knows its UID by
name and can do nothing with it; it has no drv agent unless the record says so. It is a
"better VT switch": the person's session is a workspace instead of a console.

Its Wayland connection is the apps' socket. drv-appd's manifest has a record
for the person's UID named `host`, with `gpu` and no exec, emitted by the
module from `services.drv.host`: the compositor learns the name the way it
learns every client's, and every door keys on the same record, so
notifications and the chooser work from the terminal. Nothing can launch it:
the record has no exec, and the forker refuses any UID outside the app range.
Window rules can match on the policy
name (`match app="host"`, never client-supplied), and the module pins those
windows to a named workspace `host`. A configured bind on Ctrl+Alt+F1 wins
over the hardcoded VT switch and focuses that workspace; nothing else can
launch or focus it. On lock it is hidden with everything else.

The terminal is outside the set's group: the set's cgroup kill does not reach
it, so a compositor restart does not kill a `nixos-rebuild` in progress.
When the compositor goes its Wayland connection dies and the terminal exits
as any terminal would; the supervisor starts a fresh one with the next set,
and restarts it alone when the person closes it (five starts a minute at
most).

## Non-goals

No second host account, no menu entry, no per-app "run on the host" grant.
The host workspace is the whole of the host's presence on the desktop.
