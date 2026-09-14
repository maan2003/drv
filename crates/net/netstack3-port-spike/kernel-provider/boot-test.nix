{
  stdenv,
  testers,
}:

let
  providerSelftest = stdenv.mkDerivation {
    pname = "netstack3-kernel-provider-selftest";
    version = "2";
    dontUnpack = true;
    buildPhase = ''
      $CC -std=c11 -O2 -g -Wall -Wextra -Werror -pthread \
        ${./selftests/provider.c} -o provider
    '';
    installPhase = ''
      install -Dm755 provider $out/bin/netstack3-provider-selftest
    '';
  };
in
testers.runNixOSTest {
  name = "netstack3-kernel-provider-boot";

  nodes.machine = { ... }: {
    imports = [ ./module.nix ];
    hardware.netstack3KernelProvider.enable = true;
    environment.systemPackages = [ providerSelftest ];
    virtualisation.memorySize = 1024;
  };

  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")

    with subtest("patched Linux 6.18 kernel exposes the provider device"):
        machine.succeed("uname -r | grep -F '6.18.40'")
        machine.succeed("test -c /dev/netstack3-provider")

    with subtest("booted provider ABI passes its complete kernel selftest"):
        output = machine.succeed("netstack3-provider-selftest")
        assert "1..12" in output, output
        assert "not ok" not in output, output
        assert "ok 12 - directional shutdown reached provider" in output, output
  '';
}
