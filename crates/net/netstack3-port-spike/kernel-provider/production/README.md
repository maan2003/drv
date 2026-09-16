# Native Linux socket frontend: localhost and Ethernet integration

Implements the direction in [ARCH-network-service](../../../../../specs/ARCH-network-service.md).
This is separate from the earlier `patches/` experiment: do **not** install both.

## Current frontend: owned sockets and ABI6

The [Rust frontend](rust-abi5/README.md) registers AF_INET/AF_INET6 with native
INET excluded. Linux and SOCKS are thin consumers of concrete owned sockets in
port-integration; Runtime is the sole core-ID registry. Legacy framed daemons
construct their own tool-only handle tables and do not shape production.

`install-kernel.sh` selects this Linux 7.3 frontend. The old C production
implementation has been retired; its sources and captures remain in Git history.
The directory name `rust-abi5` is historical, not a compatibility promise.
Kernel and userspace must be deployed together with the current `protocol.h`.
Current [ABI6 acceptance](rust-abi5/evidence.txt) records native tests, lock-debug
no-INET KVM and private-overlay OpenSSH. The captures below are historical
baselines, not ABI6 acceptance evidence.

## Delegated route metadata

Linux retains native AF_NETLINK socket transport, port binding, subscriptions,
credentials, receive limits and unrelated protocols. A separate
`/dev/netstack3-netlink` registration delegates subsequently created
NETLINK_ROUTE sockets in the launcher's namespace. Claimed FDs identify single
sockets; publication is restricted to that generation's current subscribers.
Provider loss fails closed, including after replacement. The wire contract and
limits are in [netlink-protocol.h](netlink-protocol.h).

The supervisor passes registration FD10 with `--netlink` into the existing
sandbox. The userspace adapter renders read-only link/address dumps and change
notifications from actual Netstack3 observations, including loopback, DHCP,
IPv6 address state and link loss. It does not synthesize configured IP addresses.
Route queries and mutations return explicit unsupported errors. Per-application
metadata privacy remains deferred; this is a namespace-wide compatibility view.

The standard guest fixture now also needs these clients, built from this
directory (no Go module or external Go dependency is required):

```sh
gcc -O2 -Wall -Wextra -Werror -pthread netlink-test.c -o "$ROOT/bin/netlink-test"
gcc -O2 -Wall -Wextra -Werror netlink-client-test.c -o "$ROOT/bin/netlink-client-test"
CGO_ENABLED=0 go build -p 16 -o "$ROOT/bin/netlink-go-test" netlink-go-test.go
```

The boundary fixture checks endpoint isolation, subscriptions, bounded queues,
overrun reporting, sender credentials, namespace isolation, native Generic
Netlink, revocation and replacement. The real Ethernet fixture checks glibc
`getifaddrs`/`AI_ADDRCONFIG`, pure-Go interface discovery and address-removal
notifications before/after link loss, alongside existing TCP/UDP and NSS tests.
These are no-INET KVM checks, not physical deployment or full rtnetlink coverage.

## Original C baseline proof

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
FD3 still owns the application namespace. These numbers are startup conventions:
seccomp checks syscall/command/flag constraints, not descriptor slots. Native
file operations reject endpoint ioctls on a frame socket; its socket capability
authorizes frame I/O. Replacement frames retain their received descriptors, with
no reserved offline slot. No device/DMA authority crosses this boundary.

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

## Historical ABI5 per-socket IPC

The former `protocol.h` owned ABI5 (current ABI6 is described above). The privileged registration FD scopes a namespace and
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

A mutating control interruption/timeout revokes only that socket, not the
namespace. In Rust, interrupted read-only GETNAME queries detach their waiter
without revocation; late replies are drained by ID, never applied to a new call.
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

## Historical Fuchsia-based task and shutdown redesign (ABI5)

The binding adapts Fuchsia revision
`1e1219e3fac944c9a906aea9646939746b6062b3`, under
`src/connectivity/network/netstack3/src/bindings/socket/`:

- `worker.rs`: `SocketWorker::handle_stream` and `SocketWorkerHandler::close`
  inform the single endpoint/core owner and final-close response.
- `stream.rs`: `TaskControl::shutdown_send` informs the producer barrier.
- `stream/buffer.rs`: `send_task`, `send_task_shutdown`,
  `CoreSendBufferInner::ShuttingDown`, and `receive_task` inform separate
  bounded send/receive tasks and terminal core ownership.
- Its `send_task_shutdown` test is adapted to vary existing core occupancy
  and admitted remainder without any ACKs/network progress. Local tests add
  partial-write completion, failed handoff, credits, child ownership and ABI
  rejection. These are adaptations, not verbatim ports or the full Fuchsia
  test suite. Copyright notices accompany the code; BSD-2-Clause terms are in
  `../../upstream-cargo/LICENSE.fuchsia`.

Linux dup/fork already shares one endpoint, replacing FIDL clone streams.
Kernel messages replace Zircon socket buffers; four receive credits replace
Zircon writable waits. A safe `VecDeque` holds both normal and terminal send
bytes, instead of Fuchsia's ring plus overflow vector and initialized-slice
unsafe conversions. Each socket has one worker; send/receive are independently
polled state machines on the existing executor, not additional OS threads.

Ownership proceeds from kernel-admitted SEND → send task → core send buffer.
Write shutdown stops kernel admission under the transmit lock and queues an
ordered barrier. The worker transfers any remaining admitted bytes into a
terminal core buffer, completes the SENDs, and invokes core shutdown. It never
waits for peer ACKs or receive-window space. The extra storage is bounded by the
endpoint's existing 256 KiB outstanding-byte limit; ordinary TCP buffer limits
remain in force before terminal handoff. Core retains TCP delivery/retransmission
responsibility after application close. Receive pumping and credit returns
remain independent; EOF is emitted only after buffered data.

A listener owns its one unpublished accepted child. Publication transfers it
directly into a child worker before epoll registration, so failed registration
drops the child rather than leaking a raw core handle. Endpoint revocation
drops the tasks and closes owned handles; it does not promise delivery after
cancellation or provider death.

ABI5 deliberately rejects ABI4 peers: the old worker paired with the new
kernel failed the shutdown byte-count test. This requires coordinated rollout,
not compatibility negotiation. No kernel Rust rewrite, FIDL/Zircon emulation,
Fuchsia datagram MessageQueue port, or TCP algorithm rewrite is claimed.
The Linux queue/capability boundary remains C; the worker forbids unsafe Rust.

See [redesign acceptance evidence](evidence/fuchsia-task-redesign.txt) for
host tests, no-reader-progress KVM regressions and private-tailnet SSH transfer.

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

Build `netstack3-provider` from `crates/net/network-service` with the repository's
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
- `wlan-stack-kvm`, `wlancfg-service`, `wlanctl`, and `netstack3-provider`;
- `drv-dns-service`, `dns-check`, the current `loopback-test`, and
  `/lib/libnss_drv.so.2`, with `/etc/quad9.toml`;
- curl, Busybox, the matching ELF loader and transitive runtime libraries, and
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
export DRV_SAE_CHANNEL=149
./run-wifi-kvm-guarded.sh 0000:05:00.0 140 -- \
  ./run-wifi-kvm.sh "$BZIMAGE" "$WIFI_ROOT" "$NEW_PRIVATE_OUTPUT_DIRECTORY"
```

Do not invoke the guarded launcher on a host until both an independent recovery
access path and a reset-before-native-bind reboot/power-cycle contract have been
proved for that host. Independent USB control is verified on np, but continuous
containment across automatic reboot is not; physical trials remain blocked.

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
path is Redwood's own Wi-Fi/Tailscale → SSH forwarding on Redwood →
Redwood USB → np. The older reverse-SSH relay had stale-listener recovery gaps. Verify the np SSH connection is from 172.16.42.1 to
172.16.42.2 before handing off the device. No native runtime fallback is added
to the guest by the host's lab recovery.

### Resolver compatibility is not faked

Unsupported protocol options still return `ENOPROTOOPT`; the binding does not
pretend that Netstack3 datagram errors are a Linux extended-error queue.
Instead, the guest runs the separate sandboxed `drv-dns-service`, receiving
read-only configuration and CA capabilities on FD3/FD4. Its Quad9 DoT/DoH
upstreams have no plaintext or native-resolver fallback. The NSS module
uses its Unix socket through `hosts: files drv`. The fixture scopes
`LD_LIBRARY_PATH=/lib` to `dns-check` and curl so the staged loader finds the
matching NSS module. Curl uses ordinary hostnames and verifies TLS certificates,
without `--resolve`. DHCP DNS is logged as diagnostic evidence, not used as a
fallback or production configuration API.

The long-lived launcher owns separate driver, policy and network processes;
`wlanctl` drives scan, connect and status through the bounded control API.
The guest uses a private tmpfs policy-state directory. After application gates,
it immediately requests orderly launcher shutdown and requires both a clean
exit and the hardware SAFE marker. The earlier physical run 44 passed
association, DHCP, NSS DNS and both HTTPS gates but hit the VM deadline before
safe shutdown: it is functional evidence, **not** lifecycle acceptance of this
updated launcher.

### Laptop power lifecycle contract

The production launcher accepts `SIGUSR1` as a **suspend-preparation request**.
It first closes policy admission, then waits up to ten seconds for the Wi-Fi
service's normal `ClientRuntime::shutdown` path to revoke its Ethernet endpoint
and contain the MT7921. It then terminates the network-service generation.
This order keeps external peer EOF from racing the driver's protocol stop;
network and application capabilities are still revoked before successful common
child cleanup emits:

```
wlan_stack_suspend_ready=true hardware_stopped=true network_revoked=true
```

A failed or timed-out containment exits nonzero and never emits readiness. A
platform power coordinator may suspend only after observing the launcher's
successful exit and this marker. After resume it must start a new launcher; the
new random Wi-Fi generation, new driver construction, and wlancfg's persisted
desired network prevent reuse of old operation/key/DMA authority and permit
automatic selection/reconnect.

This is a real quiescence boundary, not a complete system-suspend integration.
The repository does not yet contain the platform inhibitor/coordinator that
orders all devices into an ACPI sleep state, nor an MT7921 WoWLAN wake contract,
PCI D-state transition, or runtime-autosuspend owner. Those remain unsupported
and must not be inferred from launcher restart. Connected-idle power saving is
separate. `wlanctl power-save performance` requests the acknowledged awake
state (0). `wlanctl power-save balanced` currently returns `unsupported`
without changing firmware state. Pinned Linux's dynamic state (2) is coupled
to a TX gate that queues traffic behind `mt792x_mcu_drv_pmctrl` whenever
firmware owns the HIF; the native driver does not yet implement or track that
wake/ownership boundary. Exposing state 2 alone caused post-idle TX to expire,
so it fails closed rather than silently mapping Balanced to Performance. The
driver starts each association awake and a Performance command succeeds only
after firmware ACK and DMA reclaim. Physical idle-current, traffic wake, and
repeated suspend/re-entry validation remain required. The
Wi-Fi control process still has a 1 ms fallback service tick, so this is not a
claim that the full process graph is tickless or that battery savings have been
measured.

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
- Build `crates/net/nss-drv` natively as a release cdylib; copy `libnss_drv.so` to
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

### Tailscale userspace compatibility lab

`run-tailscale-kvm.sh` runs a disposable no-INET guest against an unprivileged
QEMU/SLIRP Ethernet gateway. This is an external lab uplink, not an alternate
TCP/IP backend inside the guest and not an MT7921 hardware proof. No host
interfaces, routes, firewall rules or Tailscale configuration change.

Enable `VIRTIO_MENU`, `VIRTIO_PCI` and `VIRTIO_CONSOLE` in the lab kernel.
Stage the normal provider/NSS root plus `netstack3-virtio-lab`, an actual
Tailscale daemon binary (not a distro wrapper), and CA certificates.
The daemon may be a multicall binary linked as both `tailscale` and
`tailscaled`. `netstack3-virtio-lab` is a safe-Rust test transport for QEMU's
length-prefixed Ethernet stream, not a production driver.

The guest starts `tailscaled --tun=userspace-networking`, with local SOCKS5
port 1055 and HTTP-proxy port 1056. It advertises no routes and disables
acceptance of Tailscale DNS/routes. DNS servers for Go's independent resolver
are published from the actual DHCP result in this fixture only.

**Known metadata gap:** Tailscale 1.98.10 observes no configured Linux IP
interface and pauses its control client. The lab explicitly sets
`TS_ASSUME_NETWORK_UP_FOR_TEST=true`, the upstream development override checked
by `ipn/ipnlocal/local.go` and `wgengine/magicsock/magicsock.go`. No certificate,
authentication or transport validation is disabled. Publishing service-owned
link/address/route state to Linux applications remains needed to remove this
override. Userspace mode does not by itself fix that integration gap.

The run reached the real control plane through Netstack3 and subsequently
completed user authorization. Direct UDP discovery pings and encrypted TSMP
pings passed in both directions between the guest and lab host. The guest HTTP
and SOCKS5 proxies successfully connected to the host's SSH service and received
its banner. These are same-host virtual-gateway tests, not physical Wi-Fi or
remote-NAT proof. Login URLs, machine state and
tailnet membership belong only in the private output directory, never checked-in
evidence. The guest's state is volatile. The runner bounds its guest and
gateway lifetime to one hour. `control.sock` is a root shell confined to that
private VM; its containing host directory is mode 0700.

[Run evidence](evidence/tailscale-prelogin.txt) records pre-login and authenticated
checks. `tailscale netcheck` reported UDP false and no IPv4 address despite
successful direct UDP/TSMP traffic; the discrepancy and UDP rebind errors remain
unexplained. The daemon established a DERP connection, but payload transport
through DERP has not been isolated and verified.

### OpenSSH follow-up

The live lab also ran ordinary OpenSSH with a loopback-only listener,
public-key-only authentication, and `tailscale serve --bg --tcp=22
tcp://127.0.0.1:2222`. This is not Tailscale SSH or transparent TUN routing.
Guest-local SSH command execution and PTY allocation passed with native
INET still excluded. After the owner updated network access policy, lab-host
tailnet SSH key authentication, commands, and PTY allocation also passed.
The lab itself does not modify shared policy.

A 64 KiB SSH download passed its hash check, but two 1 MiB download attempts
timed out (the repeat received 826817 bytes in 45 seconds). This is not a
successful bulk-transfer proof. A separate devbox connection also timed out.
See the run evidence for scope, unsupported socket-option warnings, and a
transient restart/rebind failure. These unresolved behaviors need isolation
before treating this as dependable remote management.

### Reproducible private-tailnet SSH test (no interactive login)

`tailscale-control/` directly uses upstream Tailscale v1.98.10's testcontrol
server and DERP server, not production accounts. Build with native Go 1.26.5:
`cd tailscale-control && go build -o test-control .`.
The helper's control listener is **127.0.0.1:18766 only**. Its TLS relay listens
on an ephemeral port of the host's selected IPv4 address and rejects HTTP
requests from other source addresses. The DERP map pins the certificate.
No public control server, shared ACL, host route, or real host daemon is changed.
The helper is intentionally an automatic-registration test fixture, never a
production coordination service. Keep all test state in a mode-0700 lab directory.

For the existing runner, stage:
- Actual `tailscaled`/`tailscale`, provider and virtio frame adapter plus CA/ELF files.
- Actual `/bin/sshd` and `/bin/ssh-keygen` plus OpenSSH's matching libexec helpers
  and ELF dependencies (distro wrappers alone are insufficient).
- `sshd-lab.conf` at `/etc/sshd-lab.conf`, and only the intended test public key
  at `/root/.ssh/authorized_keys`.
- `/etc/tailscale-lab-login-server` containing `http://10.0.2.2:18766`.
- Optionally a private prior test identity at `/run/tailscaled.state`. Otherwise
  the local test server automatically admits a fresh guest, with no browser login.

Run `test-control`, then start a **separate userspace** host tailscaled with
private `--state` and `--socket` paths, and `TS_LOGS_DIR` set to that private
directory. Point its `tailscale up` to
`http://127.0.0.1:18766` with `--accept-dns=false --accept-routes=false`.
Do not use the real host daemon's socket/state. Run `run-tailscale-kvm.sh`
normally; guest init starts key-only sshd and exposes tailnet TCP/22 through
`tailscale serve`. Determine the guest's test-tailnet IP from the private
daemon's status rather than assuming registration order.

Use ordinary OpenSSH with a console-pinned host key and:
`ProxyCommand=tailscale --socket=PRIVATE_PEER_SOCKET nc %h %p`.
The guest remains `CONFIG_INET=n`; this checks both outer encrypted transport
and the forwarded loopback TCP application path. Stop the private host peer
and controller after tests; the QEMU runner bounds its guest/gateway to one hour.
SSH transport cancellation alone may leave remote daemons alive: track their
actual PIDs and validate command identity before cleanup.

[Redesign checkpoint evidence](evidence/socket-worker-ssh.txt) records the
old-kernel sendmmsg regression, the fixed-kernel test, and SSH bulk comparisons.
Unsupported flags must be distinguished from Linux-internal scheduling hints:
`MSG_BATCH` is added by `__sys_sendmmsg`, even when userspace supplies flags=0.

## Kernel-Rust feasibility experiment

The separate [Rust lifecycle slice](rust-lifecycle/README.md) builds on Linux
7.3-rc2 and exercises real AF_INET/AF_INET6 creation, polling, final release and
provider-generation death in no-INET KVM. It reuses upstream Rust polling and
RCU-teardown wrappers. It has no TCP/UDP data path. Its shared typed file infrastructure is now used
by the [complete Rust ABI5 frontend](rust-abi5/README.md). [Evidence and limits](rust-lifecycle/evidence.txt).
