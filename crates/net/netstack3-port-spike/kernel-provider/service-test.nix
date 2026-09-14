{
  stdenv,
  testers,
}:

let
  linkPeer = stdenv.mkDerivation {
    pname = "netstack3-deterministic-link-peer";
    version = "1";
    dontUnpack = true;
    buildPhase = ''
      $CC -std=c11 -D_POSIX_C_SOURCE=200809L -O2 -Wall -Wextra -Werror \
        ${./selftests/link_peer.c} -o netstack3-test-link-peer
      $CC -std=c11 -O2 -Wall -Wextra -Werror \
        ${./selftests/remote_client.c} -o netstack3-test-remote-client
    '';
    installPhase = ''
      install -Dm755 netstack3-test-link-peer \
        $out/bin/netstack3-test-link-peer
      install -Dm755 netstack3-test-remote-client \
        $out/bin/netstack3-test-remote-client
    '';
  };
in
testers.runNixOSTest {
  name = "netstack3-provider-service";

  nodes.machine = { ... }: {
    imports = [ ./module.nix ];
    services.netstack3Provider = {
      enable = true;
      linkPeer.command = [ "${linkPeer}/bin/netstack3-test-link-peer" ];
    };
    networking.useDHCP = false;
    virtualisation.memorySize = 1024;
  };
  nodes.static = { ... }: {
    imports = [ ./module.nix ];
    services.netstack3Provider = {
      enable = true;
      linkPeer.command = [ "${linkPeer}/bin/netstack3-test-link-peer" ];
      staticIpv4 = {
        address = "192.0.2.10";
        prefix = 24;
        dns = [ "192.0.2.2" ];
      };
    };
    networking.useDHCP = false;
    virtualisation.memorySize = 1024;
  };

  testScript = ''
    start_all()
    for node in [machine, static]:
        node.wait_for_unit("multi-user.target")
        node.wait_for_unit("netstack3-provider.service")

    with subtest("supervisor gives each child only the connected fd-3 link"):
        machine.wait_until_succeeds(
            "test $(wc -l </run/netstack3-test-link-generations) -ge 1", timeout=30
        )
        peer = machine.succeed("pgrep -x netstack3-test-").strip()
        daemon = machine.succeed("pgrep -x netstack3-provi").strip()
        machine.succeed(f"test -S /proc/{peer}/fd/3")
        machine.succeed(f"test -S /proc/{daemon}/fd/3")
        no_extra_socket = "-lname 'socket:*' -printf '%f\n' | " \
            "awk '$1 >= 4 { found=1 } END { exit found }'"
        machine.succeed(f"find /proc/{peer}/fd {no_extra_socket}")
        machine.succeed(
            f"for fd in $(find /proc/{daemon}/fd -lname 'socket:*' -printf '%f\n' "
            "| awk '$1 >= 3'); do "
            f"test $(readlink /proc/{daemon}/fd/$fd) = "
            f"$(readlink /proc/{daemon}/fd/3); done"
        )

    client = "/run/current-system/sw/bin/timeout 20s ${linkPeer}/bin/netstack3-test-remote-client"

    with subtest("offline provider owns sockets before DHCP configuration"):
        machine.wait_until_succeeds("! sh -c 'exec 9<>/dev/netstack3-provider'", timeout=30)
        machine.succeed(f"{client} offline")
        machine.succeed(f"{client} preconfig >/run/preconfig.log 2>&1 &")
        machine.wait_for_file("/run/netstack3-test-preconfig-ready", timeout=20)

    with subtest("DHCP configuration makes an existing socket and remote UDP/TCP usable"):
        machine.succeed("touch /run/netstack3-test-enable-dhcp")
        machine.succeed("touch /run/netstack3-test-use-preconfig")
        machine.wait_for_file("/run/netstack3-test-preconfig-ok", timeout=30)
        machine.succeed(f"{client} udp")
        machine.wait_until_succeeds(f"{client} tcp", timeout=30)

    with subtest("link down changes reachability without revoking sockets"):
        daemon = machine.succeed("pgrep -x netstack3-provi").strip()
        machine.succeed(f"{client} keep >/run/keep.log 2>&1 &")
        machine.wait_for_file("/run/netstack3-test-keep-ready", timeout=30)
        machine.succeed("touch /run/netstack3-test-link-down")
        machine.succeed("touch /run/netstack3-test-check-link-down")
        machine.wait_for_file("/run/netstack3-test-kept-on-link-down", timeout=30)
        machine.succeed(f"test $(pgrep -x netstack3-provi) = {daemon}")
        machine.succeed("! sh -c 'exec 9<>/dev/netstack3-provider'")
        machine.succeed("rm /run/netstack3-test-link-down")
        machine.wait_until_succeeds(f"{client} udp", timeout=30)

    with subtest("DHCP NAK removes reachability without revoking provider clients"):
        daemon = machine.succeed("pgrep -x netstack3-provi").strip()
        machine.succeed(f"{client} survive >/run/survive.log 2>&1 &")
        machine.wait_for_file("/run/netstack3-test-survive-ready", timeout=30)
        machine.succeed("rm /run/netstack3-test-enable-dhcp")
        machine.succeed("touch /run/netstack3-test-check-loss")
        machine.wait_for_file("/run/netstack3-test-survived-loss", timeout=30)
        machine.succeed(f"test $(pgrep -x netstack3-provi) = {daemon}")
        machine.succeed("! sh -c 'exec 9<>/dev/netstack3-provider'")
        machine.succeed(f"{client} offline")
        machine.succeed("touch /run/netstack3-test-enable-dhcp")
        machine.succeed("touch /run/netstack3-test-check-reacquired")
        machine.wait_until_succeeds(f"{client} udp", timeout=30)
        machine.wait_for_file("/run/netstack3-test-survived-reacquire", timeout=60)

    with subtest("static configuration works with DHCP disabled"):
        static.wait_until_succeeds("! sh -c 'exec 9<>/dev/netstack3-provider'", timeout=30)
        static.wait_until_succeeds(f"{client} udp", timeout=30)
        static.wait_until_succeeds(f"{client} tcp", timeout=30)

    with subtest("peer loss reaps the daemon and starts a fresh generation"):
        old_daemon = machine.succeed("pgrep -x netstack3-provi").strip()
        machine.succeed(
            "systemd-run --unit=netstack3-test-hold --collect --no-block sh -c "
            f"'{client} hold >/run/hold.log 2>&1 && "
            "/run/current-system/sw/bin/touch /run/hold-hup'"
        )
        machine.wait_for_file("/run/netstack3-test-live-socket", timeout=30)
        machine.succeed("kill $(tail -n1 /run/netstack3-test-link-generations)")
        machine.wait_for_file("/run/hold-hup", timeout=30)
        machine.wait_until_succeeds(
            "test $(wc -l </run/netstack3-test-link-generations) -ge 2", timeout=30
        )
        machine.wait_until_succeeds(
            f"test $(pgrep -x netstack3-provi) != {old_daemon}", timeout=30
        )
        machine.wait_until_succeeds("! sh -c 'exec 9<>/dev/netstack3-provider'", timeout=30)
        machine.wait_until_succeeds(f"{client} udp", timeout=30)
        machine.wait_until_succeeds(f"{client} tcp", timeout=30)
  '';
}
