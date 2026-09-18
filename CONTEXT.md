# Names

The words we use for the GUI stack, so a message means the same thing to
everyone. "Today" is what runs now; the agreed name is what we are moving
to and what any discussion means.

- **drv-supervisor** (crate `drv-supervisor`, root). Spawns the trusted
  set below, creates every socketpair between them at startup and pushes
  the fds (`Attach`). Takes input from nobody. Restarts what dies.
- **drv-appd** (crate `drv-appd`, uid drv-appd). The launcher for
  untrusted things. Owns the manifest (`appd.toml`), the public socket
  (`/run/drv/appd.sock`), policy lookups, autostart. Android's
  PackageManager plus ActivityManager.
- **drv-forker** (crate `drv-forker`, root today). drv-appd's privileged
  helper, a separate minimal binary: one socketpair from the supervisor,
  one fixed-shape request per app, builds the sandbox, forks, reports the
  pid. Android's Zygote. An implementation detail of drv-appd, not a
  peer. drv-appd and drv-forker are one group: if either dies the
  supervisor replaces both; the apps stay up and the new forker finds
  them through their cgroups.
- **seatd** (`drv-seatd`, uid drv-seat with groups video, input, tty and
  `CAP_SYS_TTY_CONFIG` as its whole bounding set). libseat, udev, opens
  devices, on its own VT (7). Pushes device fds at runtime to the
  compositor (cards go on to compositor-gpu through the compositor).
- **authd** (`drv-authd`). PIN store and verifier. The only source of
  unlock. No socket.
- **compositor** (niri core, uid drv-compositor). Never forks, never
  holds a device.
- **compositor-gpu** (`niri gpu-process --mode drm`, uid drv-gpu, a
  supervisor service). DRM and Mesa. Wired to the compositor by the
  supervisor at startup; the compositor hands it the devices it opened
  through seatd, then it seals itself.
- **locker** (`drv-lock`, uid drv-lock, a supervisor service). Draws the
  lock screen, collects the PIN. Its Wayland connection and its authd
  verifier both come down its wire from the supervisor; it finds no
  socket and none finds it. Always running: after every `finished` it
  asks to lock again and the compositor holds the request for the next
  locking.
- **compositor group**: compositor, compositor-gpu and locker. If any
  member dies the supervisor stops the rest and starts the whole group
  again.
- **apps** (uid 100001 and up). Everything untrusted, forked by
  drv-forker on drv-appd's say.
- **launcher**: an app that may start other apps. Launch authority will
  be an fd handed to it, not a grant.

Retired words: "spawner" (meant supervisor plus forker), "identityd".
