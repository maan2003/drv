# Names

The words we use for the GUI stack, so a message means the same thing to
everyone. "Today" is what runs now; the agreed name is what we are moving
to and what any discussion means.

- **drv-supervisor** (today: the root half of `drv-spawnd`). Spawns the
  trusted set below, creates every socketpair between them at startup and
  pushes the fds (`Attach`). Takes input from nobody. Restarts what dies.
- **drv-appd** (today: `drv-spawnd identityd`, uid drv-identity). The
  launcher for untrusted things. Owns the manifest, the public socket,
  policy lookups, autostart. Android's PackageManager plus ActivityManager.
- **drv-forker** (today: the app-forking code inside `drv-spawnd`'s root
  half). drv-appd's privileged helper, a separate minimal binary: one
  socketpair from the supervisor, one fixed-shape request per app, builds
  the sandbox, forks, reports the pid. Android's Zygote. An implementation
  detail of drv-appd, not a peer.
- **seatd** (`drv-seatd`). libseat, udev, opens devices. Pushes device fds
  at runtime: cards to compositor-gpu, evdev to the compositor. To run as
  its own user with `CAP_SYS_TTY_CONFIG`, no setuid.
- **authd** (`drv-authd`). PIN store and verifier. The only source of
  unlock. No socket.
- **compositor** (niri core, uid drv-compositor). Never forks, never
  holds a device.
- **compositor-gpu** (today: `niri gpu-process`, seatd's child). DRM and
  Mesa. To become a supervisor service, wired to the compositor and
  seatd at startup.
- **locker** (today: `drv-lock`, an app with `auth = true` launched
  through identityd). Draws the lock screen, collects the PIN. To become
  a supervisor service: Wayland connection and authd verifier both
  handed by the supervisor.
- **compositor group**: compositor, compositor-gpu, locker. Restart
  semantics of the group are still to be decided.
- **apps** (uid 100001 and up). Everything untrusted, forked by
  drv-forker on drv-appd's say.
- **launcher**: an app that may start other apps. Launch authority will
  be an fd handed to it, not a grant.

Retired words: "spawner" (meant supervisor plus forker), "identityd".
