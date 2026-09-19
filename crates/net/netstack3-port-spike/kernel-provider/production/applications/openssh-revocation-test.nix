{ runCommand, callPackage, coreutils, gcc, glibc, gnugrep, openssh, python3 }:

let
  patchedOpenSSH = callPackage ./openssh-revocation.nix { };
in
runCommand "openssh-listener-revocation-test"
  {
    nativeBuildInputs = [
      coreutils
      gcc
      glibc
      gnugrep
      patchedOpenSSH
      python3
    ];
  }
  ''
    set -euxo pipefail

    cat > revoke-listener.c <<'EOF'
    #define _GNU_SOURCE
    #include <dlfcn.h>
    #include <errno.h>
    #include <poll.h>
    #include <stdlib.h>
    #include <string.h>
    #include <sys/socket.h>

    int
    ppoll(struct pollfd *fds, nfds_t nfds, const struct timespec *timeout,
        const sigset_t *sigmask)
    {
        static int injected;
        static int (*real_ppoll)(struct pollfd *, nfds_t,
            const struct timespec *, const sigset_t *);

        if (!injected && getenv("DRV_REVOKE_LISTENER") != NULL && nfds > 0) {
            nfds_t i;

            injected = 1;
            for (i = 0; i < nfds; i++)
                fds[i].revents = 0;
            /*
             * Match the production socket provider's terminal readiness:
             * operation-ready plus unconditional error and hangup.
             */
            if (strcmp(getenv("DRV_REVOKE_LISTENER"), "transient") == 0)
                fds[0].revents = POLLIN | POLLERR;
            else
                fds[0].revents = POLLIN | POLLERR | POLLHUP;
            return 1;
        }
        if (real_ppoll == NULL)
            real_ppoll = dlsym(RTLD_NEXT, "ppoll");
        return real_ppoll(fds, nfds, timeout, sigmask);
    }

    int
    accept(int fd, struct sockaddr *addr, socklen_t *addrlen)
    {
        static int injected;
        static int (*real_accept)(int, struct sockaddr *, socklen_t *);

        if (!injected && getenv("DRV_REVOKE_LISTENER") != NULL &&
            strcmp(getenv("DRV_REVOKE_LISTENER"), "transient") == 0) {
            injected = 1;
            errno = ENETDOWN;
            return -1;
        }
        if (real_accept == NULL)
            real_accept = dlsym(RTLD_NEXT, "accept");
        return real_accept(fd, addr, addrlen);
    }
    EOF
    gcc -shared -fPIC -Wall -Wextra -Werror revoke-listener.c \
      -o revoke-listener.so -ldl

    ssh-keygen -q -t ed25519 -N "" -f host_key
    port()
    {
      python3 - <<'PY'
    import socket
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        print(sock.getsockname()[1])
    PY
    }
    first=$(port)
    second=$(port)
    while test "$second" = "$first"; do second=$(port); done

    cat > two-listeners.conf <<EOF
    HostKey $PWD/host_key
    PidFile $PWD/two-listeners.pid
    ListenAddress 127.0.0.1:$first
    ListenAddress 127.0.0.1:$second
    PasswordAuthentication no
    KbdInteractiveAuthentication no
    UsePAM no
    LogLevel VERBOSE
    EOF

    DRV_REVOKE_LISTENER=1 LD_PRELOAD=$PWD/revoke-listener.so \
      ${patchedOpenSSH}/bin/sshd -D -e -f "$PWD/two-listeners.conf" \
      >two-listeners.log 2>&1 &
    server=$!
    trap 'kill "$server" 2>/dev/null || true' EXIT

    # One listener is terminal, while the unaffected listener must still
    # complete an actual SSH transport handshake.
    for attempt in $(seq 1 50); do
      if ssh-keyscan -T 1 -p "$first" 127.0.0.1 >host-key 2>/dev/null ||
         ssh-keyscan -T 1 -p "$second" 127.0.0.1 >host-key 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
    test -s host-key
    grep -q 'Listener on fd .* terminated: poll revents' two-listeners.log
    test "$(grep -c 'Listener on fd .* terminated:' two-listeners.log)" = 1
    ! grep -q 'accept: Network is down' two-listeners.log
    kill "$server"
    wait "$server" || test "$?" = 143
    trap - EXIT

    transient=$(port)
    cat > transient-listener.conf <<EOF
    HostKey $PWD/host_key
    PidFile $PWD/transient-listener.pid
    ListenAddress 127.0.0.1:$transient
    PasswordAuthentication no
    KbdInteractiveAuthentication no
    UsePAM no
    LogLevel VERBOSE
    EOF

    DRV_REVOKE_LISTENER=transient LD_PRELOAD=$PWD/revoke-listener.so       ${patchedOpenSSH}/bin/sshd -D -e -f "$PWD/transient-listener.conf"       >transient-listener.log 2>&1 &
    server=$!
    trap 'kill "$server" 2>/dev/null || true' EXIT
    for attempt in $(seq 1 50); do
      if ssh-keyscan -T 1 -p "$transient" 127.0.0.1           >transient-host-key 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
    test -s transient-host-key
    test "$(grep -c 'accept: Network is down' transient-listener.log)" = 1
    ! grep -q 'Listener on fd .* terminated:' transient-listener.log
    kill "$server"
    wait "$server" || test "$?" = 143
    trap - EXIT

    only=$(port)
    cat > only-listener.conf <<EOF
    HostKey $PWD/host_key
    PidFile $PWD/only-listener.pid
    ListenAddress 127.0.0.1:$only
    PasswordAuthentication no
    KbdInteractiveAuthentication no
    UsePAM no
    LogLevel VERBOSE
    EOF

    set +e
    DRV_REVOKE_LISTENER=1 LD_PRELOAD=$PWD/revoke-listener.so \
      timeout 10 ${patchedOpenSSH}/bin/sshd -D -e -f "$PWD/only-listener.conf" \
      >only-listener.log 2>&1
    status=$?
    set -e
    test "$status" -eq 255
    test "$(grep -c 'Listener on fd .* terminated:' only-listener.log)" = 1
    grep -q 'No listening sockets remain' only-listener.log
    ! grep -q 'accept: Network is down' only-listener.log

    mkdir -p "$out"
    cp two-listeners.log transient-listener.log only-listener.log       host-key transient-host-key "$out/"
  ''
