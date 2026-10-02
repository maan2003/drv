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

## Built (2026-10): the module, `u:r:<type>:<range>`

`services.drv.selinux` (nixos `config/system/drv/selinux.nix`) is the
spike as a module, on for the dev VM (enforcing) and m2sh (permissive
first; its Asahi kernel lacks SELinux, so the module's `kernel` adds the
config and that switch is a kernel build). The context is Android's
shape: one user `u` and one role `r`, since they carry nothing; the type
and the range do the work. The policy is MLS with one sensitivity and
1024 categories (MCS): `base_t` subjects hold the whole range
`s0-s0:c0.c1023`, objects `s0`; the forker gives an app
`u:r:drv_app_t:s0:c<uid%256>,c<256+uid/256>`, so what it makes carries
its pair and no other app dominates it (`mlsconstrain ... (h1 dom l2)` on
files, directories, processes, unix sockets). Verified enforcing in the
VM: a file made at `c1,c2` is refused to `c3,c4` and read by `c1,c2`, by
`c1,c2,c3,c4` and by `base_t`; the smoke run has no denials.

The person's tree (`services.drv.files`) is `files_t`, an
`mlstrustedobject`: the categories are recorded but not checked there,
because sharing is by grant (the forker's idmapped bind of a folder,
drv-files' documents mount), the way Android keeps MCS to app-private
data and puts shared storage behind one label and a runtime daemon.
idmapping is invisible to SELinux (labels are xattrs, not uids).
systemd-tmpfiles labels the directories it makes from `file_contexts`;
what is made inside takes its directory's type.

The set's members have their own domains, `drv_<member>_t`, the
supervisor's from its unit (`SELinuxContext=`), the members' from the
supervisor (setexeccon before each exec, named after the member).
With that, `files_t` is reachable by drv-files, the forker (the binds)
and the apps; `base_t`, which is root and everything untyped, may make
and relabel its directories and write files (tmpfiles) and read nothing,
and no domain may turn enforcement off or load a policy (`security
{ setenforce load_policy setbool }`); the kernel's `enforcing=0` is the
way back in. Verified in the VM: root's `cat` and `ls` of the tree and
`setenforce 0` are refused; the smoke run passes.

## Where it leads

Not built: one `store_t` for the whole store (xattrs set once; Nix keeps
`security.selinux` on copies, a `type_transition` labels builds), the
doors typed, the services systemd starts typed (`SELinuxContext=`), and
the members' and apps' allow-everything on `base_t` narrowed. Open: m2sh's Asahi kernel config; bind mounts and
btrfs subvolumes carry one label per superblock, so they cannot be
relabelled per path; CIL (secilc, to build) would replace policy.conf as
the policy grows.
