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

[Serial evidence](evidence/serial.log), with line endings/trailing blanks normalized,
includes independent `/proc` inspection,
refused connections and SO_ERROR clearing, TCP integrity, nonblocking connect,
epoll, half-close, dup/fork, UDP source addresses, empty datagrams, peek/truncation,
provider absence, blocked-read wake on death, replacement and no resurrection.
The suite runs before and after replacement. Three consecutive final KVM runs
passed. [Build evidence](evidence/build.txt) records artifact/source hashes and
symbol inspection; [service tests](evidence/network-tests.log) pass 37 tests.
The regression for polling an unconnected TCP socket runs in that service suite.
The larger upstream workspace test attempt was blocked by an unavailable
offline `backtrace` dev dependency; it is not counted as passing.

## Per-socket IPC, not a shared RPC queue

`protocol.h` owns ABI4. The privileged registration FD scopes a namespace and
generation. Its CLAIM ioctl returns one O_CLOEXEC anonymous-inode FD per
application socket, like accepting an endpoint. That FD is permanently bound
to one kernel socket; the header socket ID is checked, not used to select an
arbitrary destination. Netstack3 polls the endpoint FDs with epoll.

Each socket has its own lock, request queue and 256 KiB transmit bound. There
are 32 request slots per socket, eight reserved from data admission. RX is
limited to four kernel-enforced frame credits. CREDIT returns, CLOSE, and
accept-space notifications are coalesced allocation-free state, not consumers
of those request slots. Application close cannot be prevented by a full data
queue. The namespace quota is 256 socket objects, charged until the last
application/provider/request reference is gone.

OPEN is asynchronous. Connect initiation does not wait for a provider claim
or OPEN reply. Netstack3 publishes accepted children through the listener's
endpoint ioctl; applications accept from a local kernel queue. One pending
unpublished child per listener bounds userspace accept state. Closing a
listener releases its unaccepted children.

A control interruption/timeout revokes only that socket, not the namespace.
Closing an endpoint revokes only its socket. Closing the registration FD or
provider death revokes the generation; replacement never revives old sockets.
Read/write byte movement is asynchronous and copied, not an application
read/write RPC to the provider.

`endpoint-test` is a **separate synthetic kernel-boundary test**, not the
localhost transport proof. It tests cross-FD rejection, saturation while a
second socket progresses, allocation-free close, interrupted control isolation,
unclaimed nonblocking connect, and quotas across retained provider FDs. The
following `loopback-test` runs against **real sandboxed Netstack3**, before and
after provider replacement. The service suite includes fatal seccomp denial
tests for the new endpoint/registration syscall boundaries.

## Why anonymous FDs, rather than a new provider AF

I implemented and ran an actual socket-FD alternative with `sock_create_lite`,
SOCK_SEQPACKET and custom `proto_ops` over the same endpoint queues. Both passed
the real Netstack3 suite. The [comparison patch](evidence/socket-fd-comparison.patch)
reproduces that experimental variant; it is not a second deployed backend.
No new address-family number is needed just to return such a socket FD, so the
experiment compares socket-file dispatch against anonymous-file dispatch,
not two different network stacks or AF registration overhead.

Five alternating KVM runs of `endpoint-test bench` each transferred 100,000
16 KiB frames through provider write, application receive, and credit read:
- Anonymous FD median process CPU: 0.118061 seconds.
- Socket FD median process CPU: 0.124298 seconds.
- Anonymous FD used about 5% less CPU in this **cache-hot, same-process IPC
  microbenchmark**, which excludes Netstack3 and process scheduling.

[Raw microbenchmark results](evidence/ipc-microbench.txt) and
[real-stack measurements](evidence/ipc-netstack-bench.txt) are retained.
Repeated 8 MiB TCP transfers were highly variable: about 1.6–71 MB/s across
both designs; the old multiplexed frontend also had large variation.
The fast 1 MiB tests were generally around 64–73 MB/s. This is an unoptimized
Rust build, and the TCP measurement includes setup, allocation and integrity
checking. It does not establish deployment throughput or an end-to-end winner.
The long-transfer stalls remain unexplained; timer/window interactions are
a hypothesis, not a diagnosed cause.

**Decision:** use anonymous per-socket FDs: fewer kernel objects and adapter
operations, no measured advantage for socket FDs. Keep the registration
character device only as the authority/admission endpoint.

## What libkrun TSI contributed

Inspected pinned upstream sources:
- [libkrun stream proxy](https://github.com/libkrun/libkrun/blob/24d714b5dce8e8dd91afb9e0f64ebf6f3e1e846e/src/devices/src/virtio/vsock/tsi_stream/unix.rs)
- [libkrunfw TSI patch](https://github.com/libkrun/libkrunfw/blob/c2b4333eb870f307ba8f85d8ebb5a64f835aca92/patches/0011-Transparent-Socket-Impersonation-implementation.patch)

TSI reuses vsock queues and per-proxy credit state, which supports explicit
endpoint ownership rather than syscall-sized data RPC. But its Unix backend
creates native host TCP/UDP sockets. The guest patch also has native-INET
selection and synchronous control exchanges; its native connection probe
explicitly clears O_NONBLOCK. It is not a drop-in replacement for our
no-native-TCP/IP, same-kernel sandboxed-Netstack3 boundary. No libkrun code was
copied into the implementation.

## Remaining limits

This remains a localhost milestone, not deployment readiness:
- Bind/listen/name controls use synchronous completion with a 10-second
  deadline. Cancellation terminates the affected socket instead of supporting
  reusable canceled operations. Concurrent controls on one socket serialize.
- Protocol socket options are unsupported; generic Linux SOL_SOCKET state is
  not a complete mapping to Netstack3 options. UDP payloads are limited to
  16 KiB. Ancillary data, scoped IPv6 and broad flags/ioctl compatibility remain.
- Shutdown drains admitted output. Complete linger/shutdown semantics, exhaustive
  concurrent lifetime testing and hostile-provider fuzzing are not established.
- Throughput/CPU/power targets are not established. In particular, the observed
  long-transfer performance variation must not be hidden by the fast IPC result.
- No Ethernet/Wi-Fi capability is attached to this entry point. MT7921 and host
  networking were untouched. The minimal guest kernel is not a hardened deployment
  configuration. Although QEMU requests two vCPUs, these captures show only one
  guest CPU online; they are not multicore concurrency evidence.

## Reproduce

Use a disposable Linux 6.18.40 tree and native x86 build tools:

```sh
./install-kernel.sh "$LINUX"
cp guest.config "$LINUX/.config"
make -C "$LINUX" olddefconfig
make -C "$LINUX" -j4 bzImage
cc -O2 -Wall -Wextra -Werror loopback-test.c -o "$ROOT/bin/loopback-test"
cc -O2 -Wall -Wextra -Werror endpoint-test.c -o "$ROOT/bin/endpoint-test"
```

Build `netstack3-provider` from `crates/network-service` with the repository's
materialized upstream Cargo overlay and vendor setup. The changed
`port-integration/src/{lib.rs,socket_provider.rs}` overlay files must be copied
into that reference tree, as for other service builds. No Nix build is required.

Stage a static Busybox as `$ROOT/bin/busybox`, the provider as
`$ROOT/bin/netstack3-provider`, and both test clients above. For dynamically linked
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
incremental artifacts; `ipc-delivery-{1,2,3}` contain the final boot captures.
`ipc-comparison/final-anon-bzImage` is the tested kernel; the sibling socket image
is the comparison only. For microbenchmarks, insert `endpoint-test bench` before
provider startup in a copy of `guest-init`. For TCP measurements, replace the
first `loopback-test` invocation with `loopback-test bench` (8 MiB per direction).
Apply the comparison patch to a disposable installed kernel tree with `patch -p1`
and rebuild to reproduce socket-file measurements; never apply it to production.
