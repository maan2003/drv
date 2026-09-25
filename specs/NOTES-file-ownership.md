# NOTES-file-ownership: Who owns the person's files

## Problem

Nothing on the desktop runs as the person ([DESIGN-multi-user-gui](DESIGN-multi-user-gui.md)):
every app, the launcher, the file manager and the portals each have their
own UID and their own `HOME` under `/var/lib/drv-apps/<uid>`. So there is
no UID that owns "my documents", and no app can be handed a file it did
not create without someone owning it first.

## Decision (2026-09, first slice built)

The first shape below: drv-files owns the tree (`services.drv.files`,
`/var/lib/drv-files`, mode 0700), is the only reader of it, shows it in
its own chooser, and serves one file per consent to one UID through the
documents mount `/run/drv/doc` (FUSE, served by drv-files, mounted by
the supervisor). Open grants are read-only, Save grants writable; a set
restart drops them all.

Second slice (2026-09): the static slice, as the manifest's `folders`.
A directory of the tree named there is bound into the app's root by the
forker as an idmapped mount (`/files/<name>`, linked from `~/<name>`):
inside, the app owns it; on disk it stays drv-files', so the chooser and
a syncthing running as an app with the same folder see the same files.
This is how a browser gets `Downloads`, a password manager `Passwords`,
a games home `Games`, and how syncthing (an app with `restart`, no
window) syncs `Personal` and `Passwords`. Not built: persistable
chooser grants, directory grants, app-published directories, the file
manager, send-to-app, what "delete" means.

## Direction (as it was)

Some UID owns the person's files and hands them out, never the whole
tree at once:

- A files service (its own UID, like drv-appd) that owns the
  tree and serves it through the document portal: an app gets a
  per-file, per-session view (a FUSE mount or an fd), chosen through the
  file chooser dialog, which is the consent. This is Flatpak's shape
  (`xdg-document-portal`) with the owner being a daemon instead of the
  user.
- A redirect at the sandbox level: drv-forker bind-mounts a per-app
  slice of the tree into the app's `HOME`, decided by the manifest
  (static grants: "this app sees `~/Music`").
- Both: static slices for media-type apps, the portal for everything
  else.

Open questions: the file manager's view (it needs the whole tree, so it
is the files service's UI, not an app), backups and sync clients, and
what "delete" means when the owner is a daemon. Nothing here is built;
the document portal's FUSE view is not exposed into the sandbox yet.
