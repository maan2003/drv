# Linux kernel provider integration

Target provenance is the deployed `new-plastic` NixOS configuration (formerly
`no-plastic`), not the unrelated MT7921 reference tree:

- NixOS declaration: `/home/maan2003/src/nixos/hosts/new-plastic.nix`,
  `boot.kernelPackages = pkgs.linuxPackages_6_18`;
- locked nixpkgs: `21ea275a7c46aef9d4d6ddc962e6d562e9d94183`, lock narHash
  `sha256-sPS3CaXH8RAT3FZRuy4VcV47iuYIWMMfa0GbyJKC3o4=`;
- evaluated kernel: Linux `6.18.40`;
- source: `mirror://kernel/linux/kernel/v6.x/linux-6.18.40.tar.xz`;
- flat source hash: `sha256-NxL8Hsg55NqsmBF2yFGJEuj0UmUKrt/kOB2kQZYTpDE=`
  (Nix base32 `0cd42fb4390x73jdzbhacm9g9s0ji58whxhik2ndmr1rr0ggq4ip`).

[ABI_V2.md](ABI_V2.md) is the contract the kernel patch will implement. No
patched kernel is deployed or switched by this directory.
