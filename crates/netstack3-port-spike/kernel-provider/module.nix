{ config, lib, pkgs, utils, ... }:

let
  kernelCfg = config.hardware.netstack3KernelProvider;
  serviceCfg = config.services.netstack3Provider;
  supervisor = "${serviceCfg.package}/bin/netstack3-link-supervisor";
  daemon = "${serviceCfg.package}/bin/netstack3-provider-daemon";
  serviceCommand = [ supervisor daemon serviceCfg.device ] ++ serviceCfg.linkPeer.command;
in
{
  options = {
    hardware.netstack3KernelProvider.enable =
      lib.mkEnableOption "the Netstack3 userspace socket-provider kernel ABI";

    services.netstack3Provider = {
      enable = lib.mkEnableOption "the native Netstack3 provider service";

      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.callPackage ../provider-package.nix { };
        defaultText = lib.literalExpression "pkgs.callPackage ./provider-package.nix { }";
        description = "Package containing the provider daemon and link supervisor.";
      };

      device = lib.mkOption {
        type = lib.types.str;
        default = "/dev/netstack3-provider";
        description = "Kernel provider character device.";
      };

      linkPeer.command = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = ''
          Shell-free Ethernet-owner command and arguments. The supervisor gives
          this process one connected AF_UNIX/SOCK_SEQPACKET endpoint as fd 3 and
          sets NETSTACK3_ETHERNET_FD=3. The command must complete the versioned
          attach/link protocol and own the physical or deterministic link.
        '';
      };
    };
  };

  config = lib.mkMerge [
    (lib.mkIf kernelCfg.enable {
      boot.kernelPackages = lib.mkDefault pkgs.linuxPackages_6_18;
      boot.kernelPatches = [
        {
          name = "netstack3-userspace-socket-provider";
          patch = ./patches/0001-net-add-netstack3-userspace-socket-provider.patch;
          structuredExtraConfig.NETSTACK3_PROVIDER = lib.kernel.yes;
        }
      ];
    })

    (lib.mkIf serviceCfg.enable {
      hardware.netstack3KernelProvider.enable = true;
      assertions = [
        {
          assertion = serviceCfg.linkPeer.command != [ ];
          message = "services.netstack3Provider.linkPeer.command must name a link peer";
        }
      ];

      systemd.services.netstack3-provider = {
        description = "Native Netstack3 kernel provider";
        wantedBy = [ "multi-user.target" ];
        after = [ "systemd-udev-settle.service" ];
        serviceConfig = {
          ExecStart = utils.escapeSystemdExecArgs serviceCommand;
          Restart = "on-failure";
          RestartSec = "1s";
          KillMode = "control-group";
          TimeoutStopSec = "5s";
          User = "root";
          Group = "root";
          UMask = "0077";
          DevicePolicy = "closed";
          DeviceAllow = [ "${serviceCfg.device} rw" ];
          NoNewPrivileges = true;
          PrivateTmp = true;
          ProtectSystem = "strict";
          ProtectHome = true;
          ProtectKernelTunables = true;
          ProtectKernelModules = true;
          ProtectControlGroups = true;
          RestrictAddressFamilies = [ "AF_UNIX" ];
          RestrictNamespaces = true;
          LockPersonality = true;
          RestrictRealtime = true;
          RestrictSUIDSGID = true;
          SystemCallArchitectures = "native";
        };
      };
    })
  ];
}
