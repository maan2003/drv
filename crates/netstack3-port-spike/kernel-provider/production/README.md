# Native Linux socket frontend: localhost and Ethernet integration

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
These are the original localhost captures; the Ethernet follow-up below records
the current acceptance suite. The larger upstream workspace test attempt was blocked by an unavailable
offline `backtrace` dev dependency; it is not counted as passing.

## Inherited Ethernet capability

`netstack3-provider --ethernet-mac XX:XX:XX:XX:XX:XX` additionally accepts a
trusted launcher's nonblocking AF_UNIX SOCK_SEQPACKET frame capability on FD4.
FD3 still owns the application namespace. FD4 cannot be used for endpoint
ioctls or raw reads/writes; the sandbox permits only the frame transport's
nonblocking datagram operations. No device/DMA authority crosses this boundary.

The same runtime now runs the existing Fuchsia `DhcpService`, not a second
stack or DHCP implementation. It reports address/DNS acquisition. Link loss
revokes external configuration and queued frames without killing the provider
or its localhost sockets. Replacing a revoked Ethernet capability, publishing
DNS configuration to applications, and the MT launch path remain integration
work; this is not yet physical Wi-Fi acceptance.

The maintained guest fixture uses the service tests' simulated associated AP.
It proves DHCP, application TCP/HTTP and UDP/DNS through the kernel frontend
and sandboxed provider, followed by localhost operation after the frame peer
closes. [Ethernet serial evidence](evidence/ethernet-serial.log) includes this
proof and the complete localhost/concurrent/lifetime suite. The host service
suite reports 43 passing tests; the guest-only fixture is gated off on the host
and is explicitly executed inside KVM.

The guest now boots Q35 with virtual Intel IOMMU, IRQ remapping and strict DMA
invalidation. These initialization checks prepare for VFIO testing; without an
assigned device they do not prove physical DMA confinement.

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
A subsequent controlled experiment diagnosed buffer-induced TCP timer pacing;
see below. The comparison above is retained as historical evidence.

**Decision:** use anonymous per-socket FDs: fewer kernel objects and adapter
operations, no measured advantage for socket FDs. Keep the registration
character device only as the authority/admission endpoint.

## TCP buffer ownership follows Fuchsia's binding contract

The native binding uses bounded `VecDeque` ring storage. Consumption advances
the head instead of shifting queued bytes. Packet-builder payloads retain the
storage guard and borrow fragmented ring slices through Netstack3's
`FragmentedPayload`; slicing does not clone the readable suffix.
Capacity shrink requests remain pending until buffered data (including
out-of-order bytes) drains; growth can take effect immediately.
The service tests exercise wraparound, payload slicing and deferred shrink.

This adapts Fuchsia's ring/fragmented-payload design to our synchronous embedding,
not its Zircon executor. IPC uses level-triggered epoll readiness with bounded endpoint batches.
Loopback work uses a coalesced runnable flag independent of diagnostic queue
capacity; a bounded pump preserves reentrant wakeups from packet processing.
The suite covers that invariant and 16 concurrent IPv4/IPv6 TCP connections.
Idle allocation reclamation and per-socket core readiness callbacks still need
implementation; ring storage alone does not establish production readiness.

## Bulk TCP timer pacing fixed

The port's 64 KiB default send/receive buffers left insufficient pipeline
headroom for the 65,536-byte loopback MTU. Instrumentation found repeated
40 ms timer waits with queued transmit data and available receive credits.
Core inspection implicates Nagle plus delayed ACK after initial quick ACKs;
the negotiated MSS was not directly captured. TCP algorithms remain unchanged.

The default is now 256 KiB per send/receive buffer (allocated on use; the
4 MiB maximum is unchanged). A 128 KiB experiment still stalled on IPv4.
This increases default payload capacity fourfold, up to 128 MiB across the
256-socket quota, excluding kernel queues and other overhead.

With identical optimized Rust builds, the old 64 KiB default delivered
1.6–3.1 MB/s on repeated 8 MiB transfers; 256 KiB delivered 326–363 MB/s.
Three larger 64 MiB-per-direction runs delivered **307–337 MB/s** across
IPv4 and IPv6. These are sequential request/echo payload bytes divided by
wall time, including setup, allocation and integrity checking, not simultaneous
full-duplex throughput. Optimization alone did not fix the old configuration.

[Raw before/after results](evidence/throughput.txt),
[final maintained KVM suite](evidence/throughput-serial.log), and
[37 service tests](evidence/throughput-tests.log) retain delivery evidence.
The maintained suite includes `bench-long` (64 MiB per direction) to exhaust
quick ACKs and catch timer-paced regressions within its existing 50-second
whole-VM timeout. The old 64 KiB optimized configuration times out (exit 124);
the fixed configuration passes. This is a hardware-dependent integration
regression, not a portable 100 MB/s assertion. A virtual-time unit-test
experiment did not reproduce the actual provider's scheduling and was discarded.

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

This remains an integration milestone, not deployment readiness:
- Bind/listen/name controls use synchronous completion with a 10-second
  deadline. Cancellation terminates the affected socket instead of supporting
  reusable canceled operations. Concurrent controls on one socket serialize.
- Protocol socket options are unsupported; generic Linux SOL_SOCKET state is
  not a complete mapping to Netstack3 options. UDP payloads are limited to
  16 KiB. Ancillary data, scoped IPv6 and broad flags/ioctl compatibility remain.
- Shutdown drains admitted output. Complete linger/shutdown semantics, exhaustive
  concurrent lifetime testing and hostile-provider fuzzing are not established.
- Localhost throughput exceeds 100 MB/s in the retained optimized KVM tests.
  Deployment throughput, CPU/power and physical-link performance remain unproved.
- Ethernet capability integration is tested against both a simulated AP and
  MT7921 passthrough. The guest kernel is not a hardened deployment configuration. Earlier captures
  had only one online CPU; the Q35/ACPI guest enables both requested vCPUs.

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
into that reference tree, as for other service builds. Use `cargo build --release`
for throughput measurements. No Nix build is required.

Stage a static Busybox as `$ROOT/bin/busybox`, the provider as
`$ROOT/bin/netstack3-provider`, and both test clients above. Build service tests
with `cargo test --no-run --lib` and stage the reported library test executable
as `$ROOT/bin/network-service-tests` (strip debug symbols to keep the initrd small).
For dynamically linked
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
`wifi-guest-bzImage` and `ethernet-q35-1` are the current kernel and capture.
`ipc-comparison/final-anon-bzImage` is the earlier non-PCI localhost kernel;
the sibling socket image is the comparison only. For microbenchmarks, insert `endpoint-test bench` before
provider startup in a copy of `guest-init`. For TCP measurements, replace the
first `loopback-test` invocation with `loopback-test bench` (8 MiB per direction).
The maintained suite also runs `bench-long` (64 MiB per direction).
Apply the comparison patch to a disposable installed kernel tree with `patch -p1`
and rebuild to reproduce socket-file measurements; never apply it to production.

## MT7921 Wi-Fi guest

`wifi-guest-init` and `run-wifi-kvm.sh` exercise the same provider over a physical
frame capability: host VFIO → Q35/Intel virtual IOMMU → guest VFIO cdev →
userspace MT7921 → sandboxed Netstack3 → native application socket queues.
There is no virtual NIC, host NAT, SOCKS listener, or native guest INET stack.
The MT launcher selects this frontend with `DRV_NETSTACK_KERNEL_PROVIDER=1`;
registration/frame/bootstrap capabilities are FD3/FD4/FD5. `--bootstrap` on
the provider reuses READY/GO and NETWORK_READY/SERVE so the driver can audit
descriptors and sandbox state before serving. Device authority never enters
Netstack3.

The maintained guest is deliberately the existing **np/ajay/channel-149** lab
fixture, using the production driver's compiled native client identity. Supply
the current scanned BSSID, not an old launcher value. It checks the guest's sole
MT IOMMU-group member, DHCP, IPv4/IPv6 localhost with the external link up, an
application DNS query, certificate-verified HTTPS, a second origin's 1 MiB
download, clean driver exit and hardware-safe teardown. Physical throughput is
variable; this is connectivity evidence, not everyday-service acceptance.
See [retained results and limitations](evidence/wifi.txt).

### Stage and invoke under the existing lab safety owner

In addition to the ordinary guest root, stage:
- `mt7921-passive-scan`, built natively with `--no-default-features --features
  fuchsia-passive,full-firmware-production`, as `/bin/mt7921-passive-scan`;
- curl, Busybox with `nslookup`, their transitive ELF dependencies, and
  `/etc/ssl/certs/ca-certificates.crt`;
- the driver's pinned compressed MT7961 patch/RAM files under
  `/run/current-system/firmware/mediatek`, plus `zstdcat` and `sha256sum` under
  `/run/current-system/sw/bin`.

Use the checked-in kernel configuration, including `CONFIG_FW_CFG_SYSFS=y`.
The runner is a **command of `wifi-driver-lab`**, not a replacement for that
host safety/recovery owner. Its root launcher supplies the existing unlinked,
root-owned mode-0600 credential FD3 and bounded regulatory-snapshot FD4.
No credential is placed in the initrd, repository, command line or log.
QEMU fw_cfg carries these bytes into the trusted guest launcher; the sandboxed
network process cannot access that filesystem.

Inside the lab-owned command, with native QEMU/cpio/gzip and core tools in PATH:

```sh
export DRV_SAE_BSSID=... # current native scan, channel 149, SSID ajay
./run-wifi-kvm.sh "$BZIMAGE" "$WIFI_ROOT" "$NEW_PRIVATE_OUTPUT_DIRECTORY"
```

Keep the actual host reboot watchdog armed. The runner requires over 80 seconds
of lease remaining and limits QEMU to 75 seconds. The lab's command deadline must
cover that interval. It exports the *actual* watchdog status snapshot through
fw_cfg and marks the host safety ledger MUTATED before QEMU starts. Only normal
QEMU exit plus the guest's post-driver SAFE marker permits native rebind; failed
containment leaves the lab ledger quarantined and watchdog armed. Keep reports
root-private. After lab restoration, wait for asynchronous native netdev creation,
restore its original name if necessary, wait for the iwd scan/connect operation,
and verify native connectivity plus lab idleness before disarming. Do not stop
iwd between observing and renaming its managed netdev: that deletes/recreates it.

Control SSH must remain independent of the assigned MT7921. The tested control
path was Redwood's own Wi-Fi → restricted reverse SSH on devbox →
Redwood USB → np. Verify the np SSH connection is from 172.16.42.1 to
172.16.42.2 before handing off the device. No native runtime fallback is added
to the guest by the host's lab recovery.

### Resolver compatibility is not faked

Unsupported protocol options return `ENOPROTOOPT`. Current glibc nonetheless
requires successful `IP_RECVERR` setup and aborts resolution without it.
Netstack3's pending datagram-error notification is not a Linux extended-error
queue; the binding does not pretend otherwise. The acceptance test therefore
uses Busybox's actual UDP DNS query and passes the returned A address to curl's
`--resolve`; TLS still validates the original hostname and CA chain. This is
not evidence that arbitrary glibc-resolving applications work unchanged.
Durable DNS configuration publication and the owned userspace resolver boundary
remain integration work; parsing the provider's DHCP diagnostic is test-fixture
plumbing, not the production configuration API.

### Core-owned names and per-socket workers

The socket binding now queries core TCP/UDP `get_info` for local and peer names
and delegates ephemeral port selection to core. It no longer maintains a
second local-address cache or a sequential port allocator. Unbound
`getsockname` opens the provider endpoint without binding it; regression tests
cover unspecified names, routed localhost names, and UDP sendto-then-connect
for IPv4 and IPv6.

The kernel frontend's service uses one worker owning each endpoint FD, core
socket, pending accepted child, send queue and close response. Dropping a worker
closes its owned core sockets. Admitted sends still drain before normal close.
This removes the separate FD/socket registries and the readiness-change
linear handle lookup. Data/readiness still scan workers: this is **not** a
complete event-driven Fuchsia binding port.

Provenance: Fuchsia `1e1219e3fac944c9a906aea9646939746b6062b3`,
`src/connectivity/network/netstack3/src/bindings/socket/worker.rs`
(`SocketWorker::handle_stream`, `SocketWorkerHandler` request/close lifetime),
`socket/stream.rs` and `socket/datagram.rs` (`get_sock_name`, `get_peer_name`).
These are native Linux adaptations, not unchanged source imports: bounded epoll
batches replace FIDL streams; Linux already combines dup/fork users into one
endpoint; addresses use the embedding ABI without scoped IPv6. Upstream notices
are retained with the [BSD license](../../upstream-cargo/LICENSE.fuchsia).
Our regression tests exercise the adapted kernel contract; upstream FIDL tests
were not ported.

Three alternating full KVM suites compared the pre-worker core-name build
against the worker build, using identical kernel, clients and guest init.
With 128 idle bound UDP sockets, 64 MiB-per-direction TCP transfers improved
from 210–217 MB/s to 230–234 MB/s (pooled IPv4/IPv6 median 214.79 → 230.95 MB/s,
7.5%). These are payload bytes over wall time including setup and integrity
checks, not physical Wi-Fi throughput. The maintained suite now includes this
idle-socket workload without a hardware-dependent throughput threshold.
[All six serial logs](evidence/worker-comparison.txt) retain results.
All 43 service tests passed initially and on final recheck. One intervening
run concurrent with KVM failed the existing resource-admission wait-count
assertion (exit 125); the focused rerun passed with nine blocking waits.
The failure is retained in the evidence; scheduling sensitivity is suspected,
not established as the cause. Rust 1.97 incremental compilation hit an
unstable-fingerprint compiler panic; the final release and tests passed with
incremental compilation disabled.

At the same Fuchsia pin, Starnix
`src/starnix/kernel/core/vfs/socket/socket_backed_by_zxio.rs` stubs
`SOL_IP/IP_RECVERR` with success, and its stream `MSG_ERRQUEUE` path returns
`EAGAIN`. That is not a complete extended-error queue to port. This change
deliberately does not copy fake option success, implement NSS, or claim to
resolve the glibc compatibility gap above.

A redundant delivery rebuild/KVM repeat after whitespace-only cleanup lost
np SSH connectivity; its completion was not observed. The preceding final KVM
run passed. No physical device was assigned during these runs.

### Rust NSS and the Hickory upgrade

The native DNS runtime now pins Hickory 0.26.3 rather than Fuchsia's retained
Trust-DNS 0.22.0 fork. It still uses the custom Netstack3 runtime adapter,
without Tokio, host resolver configuration or runtime host-file lookup.
UDP replies now retain their actual source address; TCP writes acknowledge
actual core consumption and retain writes when its buffer is full.

`netstack3-provider --resolver` binds a local Unix listener before entering the
existing empty-root sandbox. The filter gains only accept4 on that listener;
DNS parsing/retries/caching remain in Hickory within the network process.
[`nss-drv`](../../../nss-drv/README.md) is a separately loaded Rust cdylib, not
a copy of the resolver/network stack in each application. One common FFI
adapter handles glibc pointers; lookup, layout and I/O live in a separate module
that forbids unsafe code. Server and wire modules also forbid unsafe code.

To stage the maintained KVM image in addition to existing artifacts:
- Build `crates/nss-drv` natively as a release cdylib; copy `libnss_drv.so` to
  `ROOT/lib/libnss_drv.so.2`.
- Compile `nss-test.c` with `-O2 -Wall -Wextra -Werror -ldl -pthread`; install it
  as `ROOT/bin/nss-test`, with its ELF dependencies.
- Create `ROOT/etc/nsswitch.conf` containing `hosts: files drv`.
- Rebuild/stage the service and service-test executable with the upgraded
  dependency lockfiles. `guest-init` supplies `/lib` in the fixture's loader
  search path; the child process still enters its empty root.
No host NSS files or host networking are changed.

The simulated Ethernet fixture checks dynamic NSS loading, real glibc A/AAAA
lookup, NXDOMAIN, UDP truncation → TCP fallback, short and misaligned buffers,
full pointer terminators, threads/fork, and fail-closed lookup after link loss
and provider death. This replaces `--resolve` only for the glibc/NSS path;
it does not implement Linux extended-error queues or non-NSS resolvers.

[Delivery evidence](evidence/dns-nss.txt): 43 service tests, 26 integration-crate
unit tests plus nine integration tests, two NSS tests and one wire test passed;
MT7921 adapter/passive-scan and ath11k-service downstream Cargo checks passed.
The final complete no-INET KVM suite passed, including offline NSS checks.
