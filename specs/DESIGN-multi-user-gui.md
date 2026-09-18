# DESIGN-multi-user-gui: Multi-user Wayland desktop

## Status

Direction record. Implemented so far: the compositor core / GPU process
split ([ARCH-gpu-process-split](ARCH-gpu-process-split.md)) and the
identity daemon forked by a root spawner, per-UID compositor policy and
group-gated PipeWire, per-app network namespaces and a NixOS module
([ARCH-app-policy](ARCH-app-policy.md)), verified in a KVM dev VM with
every process on its own UID: Chromium started from a launcher that is
itself just an app, notifications and portals (screen share with
per-session consent) through a UID-keyed bridge. Not yet: the GPU
process on its own UID, the lock lease, sub-UID ranges, file ownership
([NOTES-file-ownership](NOTES-file-ownership.md)). This records the agreed direction
and the reasoning behind each choice so later work can check itself against
intent. Details (exact protocols, ioctls, daemon splits) belong in ARCH
specs.

Part of the desktop layer of [ARCH-drv](ARCH-drv.md); applies
[REQ-isolation](REQ-isolation.md) to the GUI.

## Core idea

Every application runs as its own Unix user, and the display stack is
redesigned around that instead of retrofitted (Flatpak) or virtualised
(Qubes, Spectrum OS). The mental model is Chromium's process architecture
applied to the whole desktop: a small trusted core, per-app untrusted
processes, and every cross-boundary interaction going through an explicit,
checked channel.

Why UIDs rather than VMs: VMs cost memory and GPU access. Why UIDs rather
than Flatpak-style same-user sandboxing: the kernel already enforces UID
separation for files, `/proc`, ptrace and signals, and every existing daemon
can learn a caller's UID from `SO_PEERCRED` with no new protocol. Identity
is always the UID. PIDs are never used for identity because they are reused.

## Components and their trust

**Compositor core.** Owns client connections, protocol parsing, input
routing, focus, and policy. Holds no GPU, no network, no home, no ability to
spawn processes. Receives its DRM master and input fds from a seat daemon.
Reason: it is the browser-process equivalent; the less it holds, the less an
exploit gets.

**GPU process.** Separate process and UID. Holds the render node and DRM
master, imports client buffers and composites. Talks only to the core over
one socket. Reason: Mesa parsing attacker-controlled dmabuf parameters is
the single largest surface in a compositor; isolating it means a Mesa bug
yields pixels, not input or policy. The protocol stays in the core, not in
the GPU process, because whoever owns a client connection can inject input
events into that client. Rendering is not slowed by this: per-commit
messages are tiny, the heavy work was already GPU-side.

**Seat daemon.** Opens `/dev/dri` and `/dev/input`, owns udev and hotplug,
hands fds to the core and GPU process. Respawns the compositor with the same
fds on crash. Reason: keeps the compositor unprivileged and gives fast
recovery to the lock screen instead of a TTY. Status: `drv-seatd`
opens the devices and hands the fds to the core, which forwards the DRM
ones to the GPU process; udev, hotplug and respawn are still the
compositor's.

**Identity daemon.** Answers `uid -> app, manifest, globals, grants`.
The only parser of manifests. Compositor and bridge ask it and cache per
connection; anyone may ask it to launch an app by name, so launching is
not a privilege and does not pass through the compositor. Reason: one parser, one source of truth,
no config-format bugs inside the compositor. Identities are static: the app
list and each app's UID come from the system configuration (Nix), so
installing an app is a configuration change and there are no runtime
identities, no scratch slots, no UID allocation. A launch names an app and
nothing else; its arguments come from the manifest, never from the caller.
Dynamic grants are a separate append-only store it also reads.

**Spawn daemon.** The only thing that can start a process as another UID.
Root, forks the identity daemon over a socketpair and takes orders from
nothing else. Builds the environment from scratch (never inherits),
passes only the fds the caller is allowed to pass. Apps may spawn only into a UID range they
own (Android `isolated_app` idea), so the terminal can start shells in
sub-UIDs without any path to privilege escalation.

**Auth daemon.** Verifies PIN, fingerprint, or FIDO and is the only thing
that can unlock. Has no display and no network.

**Desktop clients.** Bar, launcher, notifications, lock UI, portals, file
manager, settings, IME, accessibility. Ordinary Wayland clients, each
its own UID, whose identity record lists exactly the globals and grants
it needs (layer shell for the bar, the screencast grant for the portal
backend). There is no `trusted` flag and no shared "human" UID: nothing
on the desktop runs as the person. Reason: keeps the compositor core
small, lets these crash independently, and keeps a compromised launcher
from being a compromised portal. Accessibility (AT-SPI) needs the most:
it sees all text and injects actions.

**Consent, not capability.** Screen sharing is a per-session lease: the
portal dialog is the consent, the portal session is the lease, and only
the portal backend may open one. No app has a static screencast right.

## Compositor policy

Wayland already stops clients seeing each other's input and output; the
compositor adds per-UID policy at one choke point, the globals it advertises
to a connection. Untrusted apps never see screencopy, layer-shell,
data-control, foreign-toplevel, virtual input, session-lock, or output
control. GPU (`linux-dmabuf`) is granted per manifest; software-rendered
`wl_shm` clients need no GPU at all. Pointer lock and idle-inhibit stay
allowed but gated, with a compositor-side escape and a visible indicator.

Server-side decorations are forced on untrusted windows and show the
identity-daemon name in a fixed slot, so a window cannot pretend to be
another app. Fullscreen for untrusted apps requires a compositor-owned
gesture and keeps a visible banner. New toplevels get focus only via an
`xdg-activation` token derived from a user gesture, closing the "pop a
window while the user types a password" keylogger.

Clipboard: a `receive` is served only to the focused client and only shortly
after the compositor delivered a key or click to it. This kills background
clipboard sniffing without prompts. Clipboard managers are trusted clients.
Drag-and-drop is out of scope.

`wl_shm` pools are sealed against shrinking by the compositor itself
(`F_SEAL_SHRINK`) so a hostile client cannot SIGBUS the compositor; pools
that cannot be sealed are rejected. The core never maps client memory; fds
are validated and forwarded to the GPU process.

X11 and Xwayland are not supported.

## Trusted UI and the lock screen

An owned app can draw anything inside its window, including a fake lock
screen. Defences, layered:

- A compositor-owned screen region that no client can cover shows whether
  trusted, secret-entry UI is currently on screen. Scoped to secret entry
  only, so the signal stays meaningful.
- A per-install secret image shown by trusted UI and never exposed to apps.
  Defeats untargeted phishing if the user checks; screen share and
  screenshots must blank while trusted UI is visible so the image never
  leaks.
- A hardware attention key (power button tap) always summons real trusted
  UI, so the user invokes it rather than reacting to it.
- The unlock secret is a local-only PIN or biometric, never a reused
  password, so a phished secret is worthless remotely.

**Locked is the default state; unlocked is a lease.** The auth daemon grants
"unlocked until `CLOCK_BOOTTIME` T" and user input extends it. Idle lock,
suspend lock, auth-daemon failure and compositor restart all reduce to the
lease expiring or never existing. Login is the first unlock, so there is no
separate greeter. Idle-inhibit becomes "extend the lease while visible".

The kernel replays the last framebuffer on resume before userspace runs.
This is a design gap we intend to fix in the kernel (blank planes on resume)
rather than work around with pre-suspend hooks.

## Storage lock

Per-app homes are separately encrypted so that keys can be evicted while
the screen is locked, approaching Android's before-first-unlock state.
Two tiers: after a short lock, freeze app cgroups and evict keys; after a
long lock, kill apps and evict fully. Open files keep their derived keys in
current fscrypt, so tier one needs a kernel change to force-evict keys of
frozen processes' open files; this is safe only because nothing can perform
I/O while frozen. Whole-disk dm-crypt with a TPM-bound key sits underneath
to hide sizes, names and structure from a pulled drive. Swap is off or
encrypted with a boot-random key. Trusted components live on the
unencrypted system partition.

ZFS offers authenticated per-dataset encryption but is out of tree and
cannot evict keys while mounted. ext4 or f2fs with fscrypt is the default;
ZFS remains an option if integrity is judged worth the module surface.

## IPC and services

Apps never see the system D-Bus. Each app that needs one gets its own
session bus; a bridge in the app's UID claims the names browsers expect
(notifications, portals, secrets, MPRIS) and translates to project
protocols. Same-UID bridges are compatibility only, never security; every
check lives in the receiving service keyed on peer UID. A D-Bus broker is
therefore never a security boundary here, which is why there is no shared
bus and no filtering proxy. The document portal's FUSE view of granted files
is per app UID. Secrets are a separate daemon with per-app
namespaces, which gives per-app keyrings for free.

Portals are mandatory: file chooser, screen share, camera, open-URL. They
resolve paths with no symlink following, and apps supply filenames, never
paths. Downloads is the one shared directory: append-only via a small FUSE
layer, finalised files become read-only.

Browser permission prompts are taken over by a single system prompt naming
both website and app. The origin is trusted from an honest browser; an
owned browser gains nothing it did not already have.

Notifications render plain text and show the identity name, never
app-supplied markup or titles.

Network isolation (per-app netns, firewall) and the developer environment
("dev" users, ssh agent, editors) are designed separately.

## Threat model summary

An owned untrusted app reaches: its own data and sessions, the network it
was granted, the GPU driver surface if granted, the compositor protocol
surface, and whatever the user hands it through portals. It cannot reach
other apps' data, the screen, other apps' input, the clipboard without user
action, secrets, or any privileged daemon. Persistence inside its own home
is the app's problem; the system offers reset.

Denial of service is not a security goal. Modest reservations keep the
compositor alive under memory pressure; nothing more.

## Build order

Two workstreams sharing one early contract, the per-UID policy record
(allowed globals, grants, GPU, display name, icon):

1. Compositor: seat daemon, core, GPU process, static policy file. First
   milestone is one `wl_shm` client shown with no Mesa in the core; then
   two UIDs with a denied global; then the lock lease; then a hostile-client
   test in CI.
2. Identity: UID allocation, NSS, manifests, spawn daemon, launcher.
   Replaces the static policy file when ready.

Fork niri (Rust, smithay) as the starting point, expecting the core/GPU
split to be the main divergence.
