# NOTES-selinux: SELinux under drv

## Problem

drv's isolation is namespaces, UIDs, Landlock, seccomp and the doors
([DESIGN-app-namespace](DESIGN-app-namespace.md), [REQ-isolation](REQ-isolation.md)).
None of it constrains root, and a namespace escape lands in the host's
DAC world. SELinux is the one upstream mechanism that labels every object
and checks root too; the question was whether NixOS and drv can carry it
at all.

## Spike (2026-10, dev VM; niri `nix/selinux.nix`, `nix/dev-guest.nix`)

What NixOS gives: every nixpkgs kernel has SELinux built in
(`CONFIG_SECURITY_SELINUX=y`); `security.lsm = [ "selinux" ]` is the
whole kernel side. systemd must be rebuilt (`withSelinux = true`); it
dlopens libselinux, mounts selinuxfs and loads `/etc/selinux/<type>/policy/policy.N`
itself before anything else starts, so `environment.etc` is enough to
ship a policy. nixpkgs has no NixOS module, no secilc, and its coreutils
know nothing of labels (`ls -Z`, `chcon` do not work); libselinux's and
policycoreutils' tools do (`getfilecon`, `setfilecon`, `setfiles`, `sestatus`).

The policy: the kernel's own dummy-policy generator (`scripts/selinux/mdp`,
built against the kernel's headers) emits every class, permission, initial
sid, `fs_use` and `genfscon` for exactly that kernel with one type,
`base_t`, allowed everything; drv appends its types and checkpolicy
compiles it. No refpolicy, nothing to maintain per distribution. Boots
enforcing; the smoke run passes with zero denials, since everything is
`base_t`.

The app domain: the forker writes `user_u:base_r:drv_app_t` to its
thread's `attr/exec` after the uid switch (what setexeccon does, no
libselinux), and the exec transitions although no_new_privs is set
(`nnp_nosuid_transition` policy capability, `process2 nnp_transition`).
A door labelled `door_t` (`setfilecon` on `/run/drv/wayland`) refuses the
app at the socket (`avc: denied { write } ... tclass=sock_file`) while the
set, in `base_t`, is untouched; relabelled back, the app connects. The
label is part of `nix/kernel-state.sh`'s account of a process now.

## Where it leads

The Android shape from the discussion, not built: one `store_t` for the
whole store (xattrs set once; Nix keeps `security.selinux` on copies, a
`type_transition` labels builds), one `drv_app` domain with an MCS
category per uid so apps cannot see each other's objects even as root,
the doors and the set's members typed, and constraints on what root in
`base_t` may touch. Open: m2sh's Asahi kernel config; bind mounts and
btrfs subvolumes carry one label per superblock, so they cannot be
relabelled per path; CIL (secilc, to build) would replace policy.conf as
the policy grows.
