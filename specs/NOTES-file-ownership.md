# NOTES-file-ownership: Who owns the person's files

## Problem

Nothing on the desktop runs as the person ([DESIGN-multi-user-gui](DESIGN-multi-user-gui.md)):
every app, the launcher, the file manager and the portals each have their
own UID and their own `HOME` under `/var/lib/drv-apps/<uid>`. So there is
no UID that owns "my documents", and no app can be handed a file it did
not create without someone owning it first.

## Direction (not decided)

Some UID owns the person's files and hands them out, never the whole
tree at once:

- A files service (its own UID, like the identity daemon) that owns the
  tree and serves it through the document portal: an app gets a
  per-file, per-session view (a FUSE mount or an fd), chosen through the
  file chooser dialog, which is the consent. This is Flatpak's shape
  (`xdg-document-portal`) with the owner being a daemon instead of the
  user.
- A redirect at the sandbox level: the spawner bind-mounts a per-app
  slice of the tree into the app's `HOME`, decided by the manifest
  (static grants: "this app sees `~/Music`").
- Both: static slices for media-type apps, the portal for everything
  else.

Open questions: the file manager's view (it needs the whole tree, so it
is the files service's UI, not an app), backups and sync clients, and
what "delete" means when the owner is a daemon. Nothing here is built;
the document portal's FUSE view is not exposed into the sandbox yet.
