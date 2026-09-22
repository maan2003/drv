# DESIGN-app-namespace: What an app sees

## Status

Direction record, agreed 2026-09. Build order steps 1 and 3 are
implemented (niri `policy`: `drv_os::approot`, `drv-host-views.service`,
the per-app `/etc`, `closure` and `files` derivations in the module,
`drv-init`): an app's root is a fresh read-only tmpfs with the four
sources mounted, HOME a tmpfs with the declared state linked in, and
Landlock limits the store to the app's closure. Not yet: the views check against Mesa on both GPUs (step 2),
seccomp and the sysctls (4), the closure lint (5), the four-sources drill
(6). The build order at the end says how the code gets from there to
here; [ARCH-app-policy](ARCH-app-policy.md), "Launching", describes the
code as it is.

Refines the app side of [DESIGN-multi-user-gui](DESIGN-multi-user-gui.md);
applies [REQ-isolation](REQ-isolation.md) to the filesystem.

## Core idea

An app's root is assembled from exactly four sources, and nothing that is
not one of them exists for the app. Each source has one mechanism and one
owner. The definition is checkable: a drill run inside an app asserts the
root contains the four sources and nothing else.

1. **Store.** Code and configuration, immutable, from `/nix/store`. Owner:
   Nix at build time.
2. **Host views.** Documents the host chooses to write about itself:
   hardware facts and host configuration. Owner: Nix for configuration,
   a root unit at boot for hardware. Generated, never a window onto the
   live system.
3. **State.** What the app may keep between runs, declared per directory.
   Owner: the app's UID, on the persist volume.
4. **Runtime.** Sockets and scratch space that live for one boot or one
   run. Owner: the app's UID (its directories, made by tmpfiles at
   boot) and the services behind the sockets.

Plus `/proc` with `subset=pid,hidepid=invisible`, which is the kernel's
view of the app's own processes (its own PID namespace: nothing else is
there to see, and none of `/proc/sys`, `meminfo` or `cpuinfo`) and belongs
to no source.

Why sources rather than a path list: a path list grows by accretion and
nobody can say why an entry is there. A source says who is responsible for
the content and what may change it, which is what a reviewer needs.

## The four sources

### Store

`/nix/store` is bound once, read-only and nosuid. The rest of the store
is then cut away by Landlock (below): a read and execute rule per path
of the app's closure, which Nix computes from the manifest's
`exec`. The store directory itself is mode 0711 on the host so that even
the paths' names cannot be listed. A closure is the only executable thing
in the namespace; every other mount is noexec.

The app's configuration is store paths too: its `/etc` (a derivation built
from the manifest: a one-line passwd, nsswitch, localtime, CA certificates,
fonts, drirc and the GL and Vulkan loader directories, resolv.conf only
when the app has the network, a machine-id derived from the app's name so
no two apps share one and none has the host's) and its HOME defaults (a
tree of files linked into HOME at launch).

A closure may not contain an interpreter (bash, dash, python, perl, or
any `bin/` entry that starts with a shebang); the build fails otherwise.
Wrappers are binary wrappers. Where an app genuinely needs an interpreter,
it carries the kernel's exec check (`AT_EXECVE_CHECK` and the exec
securebits) so scripts outside the closure are refused.

Measured: Landlock costs about 2.5 us per closure path on the M2, under a
millisecond for a browser, nothing after `restrict_self`.

### Host views

Hardware views are generated once per boot by a root unit
(`drv-host-views.service`, before the supervisor; it needs CAP_MKNOD,
which the supervisor does not have) under `/run/drv-host`: `sys-gpu/`, the
render node's device directory and its bus ancestors copied from a fixed
name list (`uevent`, `subsystem`, `vendor`, `device`, and for a platform
GPU `of_node/compatible`) plus the cpu topology; `sys/`, the cpu topology
alone; `dev/`, the nodes made by mknod: `null`, `zero`, `full`, `random`,
`urandom`, `tty`, `fd`, `ptmx` (a link to `pts/ptmx`: the forker mounts a devpts
instance of the app's own at `pts`); `dev-gpu/dri/`, the render nodes. The forker binds
`sys-gpu` and `dev-gpu/dri` for gpu apps only, `sys` and `dev` for
everyone, read-only. Nothing hardware-specific is written by Nix, the forker never
reads sysfs, and the app sees a copy, so nothing it learns is live.

Why not bind the real sysfs directories: they carry every world-readable
attribute of the device, including the display's EDID, a fingerprint.
Why not none at all: Mesa identifies the device through libdrm's sysfs
walk before the driver ever sees the fd; without it GL falls silently to
llvmpipe and Vulkan loses the device (verified on the M2 host and guest).

Configuration views are store paths (above) and need no generator.

### State

HOME is `/home/app`, the same path for every app: a fresh tmpfs,
writable, noexec, size-capped, owned by the UID, mounted by the forker.
Nothing in it survives the run unless the manifest's `state` list names
it: each entry (`.config/BraveSoftware`, say) is a directory under
`/var/lib/drv-apps/<uid>`, which the forker binds at `.state` inside
HOME. The state linker (below), as the app UID and with no
privilege, makes the state directories, symlinks each declared entry from
HOME into them, and links the HOME defaults tree from the store.

So an app is stateless unless declared otherwise; undeclared writes
succeed and vanish; declared state persists; configuration is unwritable.
The list of what an app may keep between runs is one manifest field.

Why a writable tmpfs rather than a read-only generated HOME: apps create
undeclared dotfiles on first start (`~/.pki`, `~/.cache`, shader caches,
GTK settings) and fail badly when they cannot. Why the linker and not the
forker does the linking: the forker holds CAP_SYS_ADMIN and must never
walk a directory the app controls
([ARCH-app-policy](ARCH-app-policy.md), Invariants).

### Runtime

The app's own `XDG_RUNTIME_DIR` (`/run/drv-apps/<uid>`), its `/tmp` kept
for the boot (so a second launch finds the first's single-instance
socket), a fresh `/dev/shm`, and under `/run` exactly what its features
mean: the appd socket, the apps' Wayland socket and the documents mount
for every app, the bridge for `bus`, the driver link for `gpu`, audio
and pulse for `audio`, and later the pair-link directory for linked
apps. Landlock scoping keeps abstract sockets and signals inside
the app's domain.

## Who does what

The forker forks before it looks at the request. The parent is one
thread that receives a datagram, clones a child into PID, IPC and UTS
namespaces of its own, and reaps; it never decodes anything. The child
builds the root in order of what it needs, with the primitive every
process of the system gets its root from (`drv_os::root`: the supervisor
builds the trusted set's roots with it too, from a different list):

1. With no input: handles on everything of the host's it might place
   (the network namespace, the cgroup, detached clones of the store, the
   views, `resolv.conf`, the fixed `/run` entries, and the parents of
   the per-UID directories), then the securebits, an empty bounding set,
   no SETPCAP, no_new_privs, a mount and a network namespace of its own,
   and a fresh tmpfs pivoted in as its root: the host's root is stacked
   beneath, where no path lookup reaches it, only the handles. The
   fixed part goes in: the store, `/dev`, `/dev/shm` (noexec), `/proc`
   (`subset=pid`, of the new PID namespace), the `/run` entries every
   app has.
2. The request, decoded: UID (checked against the range), then what
   depends on it. `network` rejoins the host's namespace through the
   handle; `gpu` picks the views; `audio` adds its sockets; `/etc`, HOME,
   `/tmp` and the runtime directory are mounted for the UID, the last
   three cloned through the parents' handles; the `links` (`/bin/sh`
   into the store) are made. The handles are closed,
   the old root detached, the root goes read-only, the cgroup is joined
   and a cgroup namespace opened there (`/proc/self/cgroup` says `/`).
3. The switch: its own group and nothing else, `setresgid`,
   `setresuid`, every capability set emptied.
4. As the app: the Landlock ruleset by path (the closure, `/etc`, HOME
   and the rest), `landlock_restrict_self`, the seccomp denylist (below),
   MDWE unless `jit`, `Forked` to appd, exec. What it execs is PID 1 of
   the namespace, the linker.

The seccomp denylist is not the sandbox, it closes a few doors the
sandbox does not: an executable memfd (`MFD_EXEC`; with
`vm.memfd_noexec = 1` on the host and every writable mount noexec, the
store is the only place code runs from), io_uring, and unless the
manifest says `userns`, user namespaces (`unshare`, `clone` and `setns`
with CLONE_NEWUSER refused, `clone3` absent, which libc falls back
from). `userns` is a capability like `gpu`: the browser's own sandbox
needs it and nothing else does. Everything else passes.

The kernel's own account of the result is checked against this model, not
assumed: `nix/kernel-state.sh` in the niri repo dumps a running app's
mounts and flags, credentials, securebits, namespaces, MDWE and the whole
Landlock domain with its rules as paths (the last four through a dev-only
kernel module, `nix/kdump`) and diffs it with the checked-in expectation
under `nix/expect`; the smoke runs it.

So argv, env and the closure are touched only by a process that already
is the app, and while the request is being decoded the process holds
SYS_ADMIN, SETUID and SETGID with the per-UID parents in hand and
nothing else of the host's: what a bug there could reach is another app
UID's state, not the host. The request is the manifest, parsed: UID,
argv, env, the booleans `network`, `gpu`, `audio`, `jit`, `userns`, and
the closure as a list of store paths. What each boolean means on this host
is a fixed table in the forker, definition rather than policy,
reviewable in one place. The forker never reads a file or lists a
directory to decide anything, and makes or chowns nothing: every
directory it binds exists, made by tmpfiles for each configured UID.

Everything the app can do for itself is the linker's (`drv-init`),
which the module puts in front of every app's command: a small
unprivileged program from the set's own package, run as the app inside
the finished root, and PID 1 of the app's namespace for as long as the
app runs: once the links are made it forks the app and stays as its init,
reaping orphans, passing the signals it is sent on to the app, and
exiting with the app's status (128 + the signal for a signal death: PID 1
of a namespace cannot itself die of one from inside). The forker's log
line is the linker's status. It fills the empty `/etc` tmpfs the forker gave it
from the Nix-built derivation (one link per entry; `resolv.conf` links
to `/run/host/resolv.conf`, where the forker bound the host's live copy
for a networked app), makes the state directories and links, links the
HOME defaults, and execs the rest. Everything it does is with the app's
own authority, so a bug in it is worth exactly one app; the forker does
not know it exists. The line between the two: the forker places what
belongs to someone else, the linker arranges what belongs to the app.

## Not in the namespace

`/usr`, `/bin`, `/home` (other than the app's), `/var` (other than its
state), `/boot`, `/nix/var`, `/run/current-system`, the host `/etc`,
`/dev`, `/sys`, the system D-Bus, the services' bus, other apps' runtime
directories, the nix-daemon socket. Apps launch nothing and are launched
by nothing but drv-appd.

## Manifest

`services.drv.apps.<name>` gains: `etc` and `files` (attribute sets that
become store paths), `state` (paths under HOME that persist), `closure`
(derived from `exec`), and later `links` (apps sharing a runtime
directory) and a `launch:<app>` grant. `gpu` comes to mean the render
node plus the sys view, with no group: the host's view makes the node
openable by anyone, which is what render nodes are for. `network`,
`audio`, `bus`, `globals`, `grants` and `opens` keep their meaning.
`expose` and `groups` are gone: a semantic request has no place for an
escape hatch.

## Later, on the same shape

The kernel side does not change the definition, only its enforcement: a
sealed store (an LSM under which a registered store path can never be
written, renamed, unlinked or overmounted, and only sealed inodes
execute) replaces "the store is read-only by mount" with an invariant
root cannot undo. Until then the mount flags, Landlock, MDWE and
`vm.memfd_noexec` carry the same rule in userspace.

## Alternatives

- **One bind mount per closure path** instead of Landlock: measured at
  12 to 18 us per mount, so cheap enough, but it puts the closure into the
  forker's plan and the mount table. Landlock keeps the closure with the
  app's own authority. Still viable if Landlock ever proves insufficient.
- **A verity image per app** (systemd DDI shape): duplicates every
  closure on disk and needs a signing key on some machine. Rejected for a
  laptop; the sealed store gives the integrity property without images.
- **Bind the real sysfs and `/dev` entries**: no generator, but live and
  wider than the name list. Rejected for the EDID leak and because a copy
  is easier to reason about.
- **A read-only generated HOME with links out to state**: the mirror
  image of the chosen design; rejected because undeclared first-start
  writes break apps.
- **`/etc/home` as HOME**: works, but `/etc` means read-only host view in
  this record and HOME is the one thing the app owns.

## Build order

1. The root: fresh tmpfs, pivot_root, the four sources mounted; host
   `/etc` and `/run/current-system` gone; per-app `/etc` from Nix.
2. The hardware views generator, checked against Mesa on both GPUs.
3. HOME links, Landlock, MDWE, securebits.
4. The other namespaces (PID with an init, IPC, UTS, cgroup), the small
   `/proc`, the seccomp denylist and `vm.memfd_noexec`.
5. The closure lint and binary wrappers.
6. The drill that asserts the four sources from inside an app.

Each step leaves the smoke and drills green in the KVM and M2 VMs.
