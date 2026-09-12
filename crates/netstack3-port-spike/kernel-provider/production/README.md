# Native Linux socket frontend: localhost milestone

Implements the direction in [ARCH-network-service](../../../../specs/ARCH-network-service.md).
This is separate from the earlier `patches/` experiment: do **not** install both.

## What is proved

Linux 6.18.40 in KVM on np registers our implementation for AF_INET/AF_INET6,
with `CONFIG_INET=n`. There is no native Linux TCP/UDP implementation or guest
network device. The ordinary C application exchanges 1,048,713 checked bytes
in each direction over TCP, for both IP versions. Only the application echoes;
the provider calls the actual Netstack3 TCP socket APIs and pumps its actual
loopback device receive queue.

`drv-network-service`'s `netstack3-provider` binary inherits the privileged
provider endpoint as FD3, then enters private mount/network namespaces, an
empty root, UID/GID 65534, no capabilities, no-new-privileges and default-kill
seccomp. Its own namespace cannot create application Internet sockets.
The retained endpoint is authority for exactly the original namespace/session.

[Serial evidence](evidence/serial.log) includes independent `/proc` inspection,
refused connections and SO_ERROR clearing, TCP integrity, nonblocking connect,
epoll, half-close, dup/fork, UDP source addresses, empty datagrams, peek/truncation,
provider absence, blocked-read wake on death, replacement and no resurrection.
The suite runs before and after replacement. Three consecutive final KVM runs
passed. [Build evidence](evidence/build.txt) records artifact/source hashes and
symbol inspection; [service tests](evidence/network-tests.log) pass 35 tests.
The regression for polling an unconnected TCP socket runs in that service suite.
The larger upstream workspace test attempt was blocked by an unavailable
offline `backtrace` dev dependency; it is not counted as passing.

## Mechanics and limits

`protocol.h` owns the framed provider ABI. Kernel-assigned socket/request IDs,
namespace generations, bounded request counts and copied payloads avoid exposing
kernel pointers. The provider device is namespace-scoped, not a global switch.
Kernel send admission and receive queues are local; 16 KiB data frames move
asynchronously, with four receive credits per endpoint and 256 KiB kernel
per-socket buffering. Credits return only after consuming a receive frame.
Netstack3 retains transport state, TCP buffering and all packet processing.

This is a production-shaped **localhost milestone**, not deployment readiness:
- Control operations are synchronous with a 10-second deadline; interruption
  revokes the session rather than allowing ambiguous late completion.
  Lazy first-open and accept still need fully nonblocking control admission.
- Protocol socket options are explicitly unsupported. UDP payloads are limited
  to 16 KiB; ancillary data, scoped IPv6, full Linux flags/ioctls/options and
  broad application compatibility are not established.
- Shutdown serializes with writes and drains admitted output. Complete Linux
  shutdown/linger semantics, concurrent lifecycle stress and hostile-provider
  fuzzing remain unverified.
- No throughput/CPU/power target is claimed. The test payload exceeds local
  buffers, but this is not a throughput benchmark or exhaustive backpressure test.
- There is no Ethernet/Wi-Fi capability in this provider entry point yet.
  MT7921 and host networking were not touched. The guest configuration is a
  minimal test configuration, not a security-hardened deployment kernel.

## Reproduce

Use a disposable Linux 6.18.40 tree and native x86 build tools:

```sh
./install-kernel.sh "$LINUX"
cp guest.config "$LINUX/.config"
make -C "$LINUX" olddefconfig
make -C "$LINUX" -j4 bzImage
cc -O2 -Wall -Wextra -Werror loopback-test.c -o "$ROOT/bin/loopback-test"
```

Build `netstack3-provider` from `crates/network-service` with the repository's
materialized upstream Cargo overlay and vendor setup. The changed
`port-integration/src/{lib.rs,socket_provider.rs}` overlay files must be copied
into that reference tree, as for other service builds. No Nix build is required.

Stage a static Busybox as `$ROOT/bin/busybox`, the provider as
`$ROOT/bin/netstack3-provider`, and the client above. For dynamically linked
binaries, preserve their ELF interpreter and transitive library paths under
`$ROOT` (the interpreter's `--list BINARY` reports dependencies). Create
`$ROOT/{dev,proc,sys,run,tmp}`. Then, with cpio/gzip/QEMU in PATH:

```sh
./run-kvm.sh "$LINUX/arch/x86/boot/bzImage" "$ROOT" "$OUTPUT"
```

The runner requires KVM, never attaches a host device or network interface,
captures serial output, and fails on timeout, missing final marker or failure
diagnostics. `guest-init` opens FD3 only in the provider child; the parent must
not retain a duplicate, or provider death would not revoke the session.

The retained np build area is
`/var/lib/poco-linux/redwood/work/socket-provider-kvm`.
`make-kernel` and `build-network` there use existing native toolchains and
incremental artifacts; `final-{1,2,3}` contain the final boot captures.
