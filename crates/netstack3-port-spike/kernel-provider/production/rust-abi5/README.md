# Rust kernel frontend: owned socket binding, ABI6

Registers AF_INET/AF_INET6 with native INET excluded. The historical directory
name is retained; ABI5 peers are not supported. The current private transport is
[`../protocol.h`](../protocol.h). Linux application syscalls remain unchanged.

## Ownership follows actual storage

Linux queues own admitted TX and published RX. `read(endpoint)` transfers TCP
bytes or one atomic UDP record; `NS3_READ_CONTROL` independently dequeues typed
control. No SEND replies, mirrored receive credits, GETNAME RPC or redundant
socket identities occur in ordinary records. Queue occupancy drives readiness.
Provider staging is one record normally, bounded terminal remainder at a seal.

Linux and SOCKS use concrete `TcpSocket`, `TcpListener`, `UdpSocket` owners in
port-integration. Runtime alone owns strong core IDs and invokes buffer hooks.
Connect attempt results survive SO_ERROR consumption. UDP readiness never
dequeues data. Listener publication transfers an owned child transactionally.

Runtime storage reservations cover two fixed 256KiB core buffers plus a 272KiB
terminal allowance per unit. Active/accepted sockets reserve one unit; listeners
also reserve backlog units before listening. Accepted children get their own
reservation before core dequeue; conservative backlog reservations are retained
until core listener teardown returns. A shared lease lives in actual send/receive
storage, so closing a Runtime handle cannot free a still-live core charge.
The shared pool has twice the configured socket capacity (active and passive
populations); retained terminal storage competes with future admission.

Kernel TX and RX each bound 256KiB and 32 records per endpoint; namespace quota
is 256 including endpoint-held closed sockets and unpublished children. The
binding stages at most one 16KiB record normally; terminal staging is at most
272KiB. Temporary terminal accumulation/core-copy overlap is bounded by two
additional remainder copies. VecDeque allocator growth may reserve more than
logical length; these are payload/accounting bounds, not exact RSS claims.
No peer-ACK wait occurs at terminal handoff. Runtime owns the terminal core-hook
precondition: empty termination of an unconnected owner is valid, nonempty
unconnected writes/handoffs are rejected, and only connection variants invoke
core `do_send`. Repeated empty sealing is idempotent.

`EndpointFault` retires one endpoint, not the provider generation. Listener
resource pressure is an `AcceptState` with a scheduler-visible retry deadline.
`RxRecord` admits bounded payloads before publication; oversize UDP produces
a local EMSGSIZE and drops that datagram without ending receive pumping.

## Ancillary send admission

Control messages are validated before implicit socket activation or payload
consumption. Malformed native headers return EINVAL; unsupported semantics
return EOPNOTSUPP. A well-formed nonzero UDP_SEGMENT request returns EIO because
GSO cannot be executed, allowing applications such as curl 8.21.0 to resend
individual datagrams. This is explicit rejection, not GSO support. Unknown
metadata is never silently discarded, including when mixed with UDP_SEGMENT.

The current native x86-64 regression reproduces silent acceptance on kernel
#18 and passes on #19: exact curl and unpadded layouts, malformed headers,
mixed controls, TCP rejection, sendmmsg partial success and datagram-preserving
fallback. See [acceptance evidence](ancillary-evidence.txt). The full socket
suite, Firefox/WebSocket and SSH/Git transfers also pass. End-to-end HTTP/3
acceptance is separate from the ancillary contract.

## Waiting and the hostile-provider boundary

Safe `frontend.rs` owns queue/state transitions, not TCP/IP. There are no
syscall-long transmit, receive or control mutex guards. Control slot contenders
and protocol waiters use interruptible waits and one immutable absolute deadline.
Undispatched cancellation withdraws, except a sealed shutdown revokes rather
than undoing its producer barrier; dispatched mutation ambiguity revokes.
Background UDP activation survives a nonblocking caller's EAGAIN.

First nonblocking UDP send can return EAGAIN with zero payload consumed while
autobind metadata is prepared. Initially poll allows an attempt; activation then
suppresses writable until completion/error. GETNAME reads committed metadata.
Explicit bind/connect share the logical operation gate with activation.

Completion opcode, correlation, shape and address family are validated before
metadata, result, request-retirement or wake publication. A TCP connection
outcome is correlated separately from its acknowledgement and retained separately
from consume-on-read errors. `ConnectAttempt` owns acknowledgement/outcome and
waiter attachment transitions; a completed but unclaimed attempt cannot be
replaced by another caller. Failed attempts retain poll readiness after SO_ERROR
consumption; interrupted waits claim already-committed outcomes before detaching.
TX seals count TCP bytes or UDP records including
empty datagrams. Datagram destinations are fixed at app admission.

`linux.rs`/`rust_main.rs` isolate native references and iterator callbacks.
`glue.c` owns registration/native object mechanics only. Shared typed endpoint
files reuse upstream Arc/ARef/FD reservation and pollfree/RCU lifetime helpers.
The installer applies `positionless-poll.patch`: poll callbacks borrow a live
file without asserting the stronger File/fdget_pos exclusion invariant.
Native shutdown serializes its own shared safe callers.

## Build and run

From this directory, against a disposable source tree; never install on the host:

```sh
bash install-kernel.sh "$TREE"
cp ../guest.config "$TREE/.config"
"$TREE/scripts/config" --file "$TREE/.config" \
  -d INET -d NETSTACK3 -d NETSTACK3_RUST_LIFECYCLE \
  -e RUST -e NETSTACK3_RUST -e DEBUG_KERNEL -e PROVE_LOCKING \
  -e DEBUG_ATOMIC_SLEEP -e DEBUG_MUTEXES -e DEBUG_LIST -e KUNIT
# Export RUST_LIB_SRC (matching compiler library sources) and LIBCLANG_PATH.
# RUSTC/HOSTRUSTC/BINDGEN must be make arguments, not just environment variables.
make -C "$TREE" RUSTC="$RUSTC" HOSTRUSTC="$RUSTC" BINDGEN="$BINDGEN" olddefconfig
make -C "$TREE" RUSTC="$RUSTC" HOSTRUSTC="$RUSTC" BINDGEN="$BINDGEN" rustavailable
make -C "$TREE" -j4 RUSTC="$RUSTC" HOSTRUSTC="$RUSTC" BINDGEN="$BINDGEN" bzImage
gcc -O2 -Wall -Wextra -Werror ../endpoint-test.c -o "$ROOT/bin/endpoint-test"
bash ../run-kvm.sh "$TREE/arch/x86/boot/bzImage" "$ROOT" "$NEW_OUTPUT"
```

ROOT is the existing [production fixture](../README.md), with BusyBox,
sandboxed provider, loopback/concurrency/lifetime clients, NSS and service tests
and their ELF dependencies. Rebuild the provider and service tests for the matching ABI. No compatibility adapter is used.
Use `../run-tailscale-kvm.sh` and its documented private no-login controller/peer
for OpenSSH application acceptance.


## Evidence status

[evidence.txt](evidence.txt) records ABI6 native and KVM acceptance before the
historical ABI5 capture. Core tests: 36 passed. Service tests: 49 passed serially.
KVM covers four connection-transition KUnit tests, endpoint ownership/quotas,
shared-FD waits and wake-flood deadlines, TCP/UDP v4/v6, DHCP/DNS/NSS,
provider replacement and absence. Private no-login overlay OpenSSH passes PTY
and exact random 8MiB, 8MiB and 64MiB roundtrips.

The same architecture advisor inspected the structural boundaries and recommended
no further layer; verification was run by the primary agent on np, not by the
advisor. A parallel host service resource-admission fixture has an intermittent
timing failure; serial and guest acceptance pass. This is not physical Wi-Fi
acceptance, complete Linux socket-option compatibility, or a security proof.
