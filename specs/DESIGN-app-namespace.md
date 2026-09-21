# DESIGN-app-namespace: What an app sees

## Status

Direction record, agreed 2026-09. Implemented so far: none of the root
described here. Today an app's namespace ([ARCH-app-policy](ARCH-app-policy.md),
"Launching") is the host root with `/run` rebuilt from the expose list, a
private `/tmp` and `/dev/shm`, and `/proc` with `hidepid`; everything else
(the whole Nix store, the host `/etc`, `/var`, `/home`, `/dev`, `/sys`) is
the host's, filtered only by DAC, and the default expose list includes
`/run/current-system`, which names the entire system closure. The build
order at the end says how the code gets from there to here; ARCH-app-policy
describes the code as it is.

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
   run. Owner: the forker (directories) and the services behind the
   sockets.

Plus `/proc` with `hidepid`, which is the kernel's view of the app's own
processes and belongs to no source.

Why sources rather than a path list: a path list grows by accretion and
nobody can say why an entry is there. A source says who is responsible for
the content and what may change it, which is what a reviewer needs.

## The four sources

### Store

`/nix/store` is bound once, read-only and nosuid. The rest of the store
is then cut away by Landlock in the trampoline (below): a read and execute
rule per path of the app's closure, which Nix computes from the manifest's
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
`urandom`, `fd`; `dev-gpu/dri/`, the render nodes. The forker binds
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

HOME is `/home/<name>`: a fresh per-app tmpfs, writable, noexec,
size-capped, owned by the UID, mounted by the forker. Nothing in it
survives the run unless the manifest's `state` list names it: each entry
(`.config/BraveSoftware`, say) is a directory under
`/var/lib/drv-apps/<uid>`, which the forker binds at a hidden fixed path
inside HOME. The trampoline, as the app UID and with no privilege, makes
the state directories, symlinks each declared entry from HOME into them,
and links the HOME defaults tree from the store.

So an app is stateless unless declared otherwise; undeclared writes
succeed and vanish; declared state persists; configuration is unwritable.
The list of what an app may keep between runs is one manifest field.

Why a writable tmpfs rather than a read-only generated HOME: apps create
undeclared dotfiles on first start (`~/.pki`, `~/.cache`, shader caches,
GTK settings) and fail badly when they cannot. Why the trampoline and not
the forker does the linking: the forker holds CAP_CHOWN and CAP_SYS_ADMIN
and must never walk a directory the app controls
([ARCH-app-policy](ARCH-app-policy.md), Invariants).

### Runtime

The app's own `XDG_RUNTIME_DIR` (`/run/drv-apps/<uid>`), its `/tmp` kept
for the boot (so a second launch finds the first's single-instance
socket), a fresh `/dev/shm`, and under `/run` exactly the expose list:
the appd socket, the apps' Wayland socket, the bridge, the documents
mount, audio and pulse for audio apps, and the pair-link directory for
linked apps. Landlock scoping keeps abstract sockets and signals inside
the app's domain.

## The trampoline

The forker execs not the app but a small unprivileged program from the
set's own package, already as the app's UID, inside the finished mount
namespace, with no capabilities: it makes the state directories and
links, applies the Landlock rules (closure, HOME, network ports, scoping),
sets MDWE and the exec securebits, then execs the manifest's `exec`.
Everything it does is with the app's own authority, so a bug in it is
worth exactly one app. The forker stays what it is: a list of prebuilt
strings applied between fork and exec.

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
node plus the sys view. `network`, `audio`, `bus`, `globals`, `grants`,
`opens` and `expose` keep their meaning.

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
3. The trampoline: HOME links, Landlock, MDWE, securebits.
4. A baseline seccomp filter for apps and the kernel sysctls.
5. The closure lint and binary wrappers.
6. The drill that asserts the four sources from inside an app.

Each step leaves the smoke and drills green in the KVM and M2 VMs.
