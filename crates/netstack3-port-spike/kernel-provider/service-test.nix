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
      cat > peer.c <<'C'
      #include <errno.h>
      #include <fcntl.h>
      #include <stdint.h>
      #include <stdio.h>
      #include <stdlib.h>
      #include <string.h>
      #include <sys/socket.h>
      #include <unistd.h>

      static void send_message(int fd, uint8_t opcode, const uint8_t *payload,
                               uint16_t length) {
          uint8_t packet[16] = { 'N', 'S', '3', 'E', 1, opcode,
                                 length & 0xff, length >> 8 };
          uint8_t ack[9];
          memcpy(packet + 8, payload, length);
          if (send(fd, packet, 8 + length, 0) != 8 + length ||
              recv(fd, ack, sizeof(ack), 0) != sizeof(ack) ||
              memcmp(ack, "NS3E\1\5\1\0", 8) || ack[8] != opcode) {
              perror("link handshake");
              exit(1);
          }
      }

      int main(void) {
          const char *value = getenv("NETSTACK3_ETHERNET_FD");
          uint8_t attach[] = { 2, 0, 0, 0, 0, 1, 0xdc, 0x05 };
          uint8_t link_up = 1;
          uint8_t discard[2048];
          int generations;
          if (!value || strcmp(value, "3")) return 2;
          send_message(3, 1, attach, sizeof(attach));
          send_message(3, 2, &link_up, sizeof(link_up));
          generations = open("/run/netstack3-test-link-generations",
                             O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
          if (generations < 0) return 1;
          dprintf(generations, "%ld\n", (long)getpid());
          close(generations);
          while (recv(3, discard, sizeof(discard), 0) > 0) {}
          return errno == 0 ? 0 : 1;
      }
      C
      $CC -std=c11 -D_POSIX_C_SOURCE=200809L -O2 -Wall -Wextra -Werror \
        peer.c -o netstack3-test-link-peer
    '';
    installPhase = ''
      install -Dm755 netstack3-test-link-peer \
        $out/bin/netstack3-test-link-peer
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
    virtualisation.memorySize = 1024;
  };

  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("netstack3-provider.service")

    with subtest("supervisor gives each child only the connected fd-3 link"):
        machine.wait_until_succeeds(
            "test $(wc -l </run/netstack3-test-link-generations) -ge 1"
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

    with subtest("link-up without DHCP never captures application sockets"):
        machine.succeed("sh -c 'exec 9<>/dev/netstack3-provider'")
        machine.sleep(1)
        machine.succeed("sh -c 'exec 9<>/dev/netstack3-provider'")

    with subtest("peer loss reaps the daemon and starts a fresh generation"):
        old_daemon = machine.succeed("pgrep -x netstack3-provi").strip()
        machine.succeed("kill $(pgrep -x netstack3-test-)")
        machine.wait_until_succeeds(
            "test $(wc -l </run/netstack3-test-link-generations) -ge 2"
        )
        machine.wait_until_succeeds(
            f"test $(pgrep -x netstack3-provi) != {old_daemon}"
        )
        machine.succeed("sh -c 'exec 9<>/dev/netstack3-provider'")
  '';
}
