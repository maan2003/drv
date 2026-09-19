# Names

The words we use for the GUI stack, so a message means the same thing to
everyone. "Today" is what runs now; the agreed name is what we are moving
to and what any discussion means.

- **drv-supervisor** (crate `drv-supervisor`, uid drv-supervisor; its
  unit grants it CAP_SETUID, SETGID, SETPCAP, CHOWN, KILL, SYS_ADMIN and
  SYS_TTY_CONFIG as its whole bounding set: what its children keep plus
  what switching and stopping them takes). Spawns the trusted set below,
  makes every socketpair between them before the first fork and hands
  each member its ends as named startup fds (`drv_os::fds`, the systemd
  LISTEN_FDS convention), each in the same sandbox an app gets
  (`drv_os::sandbox`: private mount namespace, `/run` holding only what
  is listed for it, no network). Takes input from nobody. If any member
  dies it kills the apps and restarts the whole set. Nothing in the tree
  runs as root.
- **drv-appd** (crate `drv-appd`, uid drv-appd). The launcher for
  untrusted things. Owns the manifest (`appd.toml`), the public socket
  (`/run/drv/appd.sock`, lookups only), the launch channels, autostart. Android's
  PackageManager plus ActivityManager. Seccomp-sealed once its fds are in
  place.
- **drv-forker** (crate `drv-forker`, uid drv-forker with CAP_SETUID,
  SETGID, SETPCAP, SYS_ADMIN and CHOWN as its whole bounding set; owns
  the supervisor's cgroup subtree and the apps' directory parents).
  drv-appd's privileged helper, a separate minimal binary: one socketpair
  from the supervisor, one fixed-shape request per app, builds the
  sandbox, forks, reports the pid. Its children leave with every
  capability set empty, bounding set included. Android's Zygote. An implementation detail of drv-appd, not a
  peer. Apps die with the set: a restart of any member kills them
  through `apps/cgroup.kill` and autostart brings them back.
- **seatd** (`drv-seatd`, uid drv-seat with groups video, input, tty and
  `CAP_SYS_TTY_CONFIG` as its whole bounding set). libseat, udev, opens
  devices, on its own VT (7). Pushes device fds at runtime to the
  compositor (cards go on to compositor-gpu through the compositor).
- **authd** (`drv-authd`). PIN store and verifier. The only source of
  unlock. No socket. Seccomp-sealed after startup (its state directory
  stays writable).
- **compositor** (niri core, uid drv-compositor). Never forks, never
  holds a device.
- **compositor-gpu** (`niri gpu-process --mode drm`, uid drv-gpu, a
  supervisor service). DRM and Mesa. Wired to the compositor by the
  supervisor at startup; the compositor hands it the devices it opened
  through seatd, then it seals itself.
- **locker** (`drv-lock`, uid drv-lock, a supervisor service). Draws the
  lock screen, collects the PIN. Its Wayland connection and its authd
  verifier are its startup fds `compositor` and `auth`; it finds no
  socket and none finds it. Always running: after every `finished` it
  asks to lock again and the compositor holds the request for the next
  locking. Seccomp-sealed after its first render (fonts stay readable).
- **menu** (`drv-menu`, uid drv-menu, a supervisor service). Holds a
  launch channel (fd `appd`), a poke line from the compositor (fd
  `compositor`, a byte per `show-launcher` bind) and its own Wayland
  connection (fd `wayland`, inserted by the compositor as a layer-shell
  client). Draws the list itself with `drv-ui`, launches the pick by
  name. One process, sealed like the locker.
- **portal** (`drv-portal`, uid drv-portal, a supervisor service). The
  person's side of the portals: the file chooser with its documents
  mount, screen sharing consent, and the microphone and camera
  consents (it tells the compositor who holds them, for the indicator). Owns the person's files
  (`--files`, `/var/lib/drv-files`), shows them on its own Wayland
  connection (fd `wayland`) when the bridge asks (fd `bridge`), and
  serves each pick to the asking UID alone at `/run/drv-doc/<id>/<name>`
  over FUSE (fd `fuse`, mounted by the supervisor). For a screen it
  lists the outputs and starts the cast at the compositor over its own
  cast line (fd `compositor`).
- **bridge** (`drv-bridge serve`, uid drv-bridge, a supervisor service).
  The apps' desktop services, keyed on the peer UID: notifications to
  the services' bus, the file chooser and the screencast to the portal,
  settings answered in place, the PipeWire remotes for a cast and for
  cameras (connections restricted to those nodes before the app gets
  them), the microphone and camera questions WirePlumber's gate raises
  through the bridge's `drv-access` metadata, OpenURI (drv-appd starts
  the scheme's manifest handler over the bridge's launch channel, fd
  `appd`), and nothing else (no xdg-desktop-portal).
  `drv-bridge app` is the shim on an app's private bus: it terminates
  all of the app's D-Bus in the app's UID and speaks `drv_bridge::wire`
  (postcard over SEQPACKET) to the server, which never reads D-Bus from
  an app.
- **notifier** (`services.drv.notifier`, mako by default; uid drv-notifier, a
  supervisor service). The notification daemon: a layer-shell client on fd
  `wayland` (`WAYLAND_SOCKET=3`), the one owner of
  `org.freedesktop.Notifications` on the services' bus. It sees every
  notification, so it is a member of the set, never an app.
- **documents mount** (`/run/drv-doc`). Where an app finds the files it
  was given. A grant is one file for one UID; the listing shows a UID
  only its own.
- **drv-ui** (crate). What the set's windows share: connection on a
  supervisor fd, toolkit boilerplate, Pango text in shm buffers, the
  seccomp seal after the fonts are warm. The locker, the menu and the
  portal are on it.
- **the set**: the ten above (seatd, authd, compositor-gpu, compositor,
  locker, menu, portal, forker, appd, bridge). If any member dies the
  supervisor kills the apps, stops the rest and starts everything again
  with fresh sockets. There are no smaller groups.
- **apps** (uid 100001 and up). Everything untrusted, forked by
  drv-forker on drv-appd's say.
- **launcher**: whoever holds a launch channel, a supervisor socketpair
  to drv-appd. Only a member of the set can (the compositor for key
  binds and the menu, each on its fd `appd`); no app is trusted, and
  `drv` only looks up.

Retired words: "spawner" (meant supervisor plus forker), "identityd".
