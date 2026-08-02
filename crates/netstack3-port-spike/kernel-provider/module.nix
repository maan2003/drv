{ lib, pkgs, ... }:

{
  boot.kernelPackages = lib.mkDefault pkgs.linuxPackages_6_18;
  boot.kernelPatches = [
    {
      name = "netstack3-userspace-socket-provider";
      patch = ./patches/0001-net-add-netstack3-userspace-socket-provider.patch;
      structuredExtraConfig.NETSTACK3_PROVIDER = lib.kernel.yes;
    }
  ];
}
