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
- **shell** (`drv-shell`, uid drv-shell, a supervisor service). The layer
  above the compositor: the lock screen (collects the PIN; its authd
  verifier is fd `auth`; after every `finished` it asks to lock again
  and the compositor holds the request for the next locking), the app
  menu (a launch channel on fd `appd`, a poke line from the compositor
  on fd `poke`, a byte per `show-launcher` bind), the person's prompts
  for drv-cast and drv-agent (fds `cast` and `agent`, `drv_shell::ask`)
  and the notifications (`/run/drv-shell/notify.sock`, keyed on the
  peer UID, the manifest name as the sender). Its Wayland connection is
  fd `wayland`, inserted by the compositor with session-lock and
  layer-shell. The lock itself is the compositor's, so the shell may
  render app text. Seccomp-sealed after its first render.
- **files** (`drv-files`, uid drv-files, a supervisor service). Owns the
  person's files (`--files`, `/var/lib/drv-files`), shows them in its
  chooser (fd `wayland`, a layer-shell client) when an app's shim asks
  on `/run/drv-files/files.sock` (keyed on the peer UID), and serves
  each pick to the asking UID alone at `/run/drv-doc/<id>/<name>` over
  FUSE (fd `fuse`, mounted by the supervisor). Sealed like the shell,
  file writes allowed.
- **cast** (`drv-cast`, uid drv-cast, group pipewire, a supervisor
  service). Screen sharing, cameras and microphones for apps, on
  `/run/drv-cast/cast.sock` (keyed on the peer UID): asks the person at
  the shell (fd `shell`), starts and stops casts at the compositor over
  its cast line (fd `compositor`) and tells it who holds the devices for
  the indicator, answers WirePlumber's gate through its `drv-access`
  metadata, hands apps PipeWire remotes restricted to the one node or
  the cameras. Not sealed (PipeWire's plugins).
- **shim** (`drv-dbus-shim`, in the app's UID on its private bus, for
  apps with `bus`). Claims `org.freedesktop.Notifications` and
  `org.freedesktop.portal.Desktop`, terminates all of the app's D-Bus
  and speaks the services' wires (postcard over SEQPACKET) to
  drv-files, drv-cast and drv-shell, and `Open` to drv-appd's public
  socket for OpenURI; settings answered in place. Compatibility, never a
  boundary: every service checks the UID itself.
- **documents mount** (`/run/drv-doc`). Where an app finds the files it
  was given. A grant is one file for one UID; the listing shows a UID
  only its own.
- **drv-ui** (crate). What the set's windows share: connection on a
  supervisor fd, toolkit boilerplate, Pango text in shm buffers, the
  seccomp seal after the fonts are warm. The shell and drv-files are on
  it.
- **the set**: the eleven above (seatd, authd, compositor-gpu, compositor,
  forker, appd, shell, files, cast, agent, keys). If any member dies the
  supervisor kills the apps, stops the rest and starts everything again
  with fresh sockets. There are no smaller groups.
- **apps** (uid 100001 and up). Everything untrusted, forked by
  drv-forker on drv-appd's say.
- **launcher**: whoever holds a launch channel, a supervisor socketpair
  to drv-appd. Only a member of the set can (the compositor for key
  binds and the shell for the menu, each on its fd `appd`); no app is trusted, and
  `drv` only looks up.

Retired words: "spawner" (meant supervisor plus forker), "identityd".
