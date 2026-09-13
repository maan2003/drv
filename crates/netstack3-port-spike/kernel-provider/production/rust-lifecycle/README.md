# Kernel-Rust lifetime feasibility slice

This is an **experimental alternative**, not the production ABI5 frontend.
It registers AF_INET/AF_INET6 with native INET disabled, but has no TCP/UDP
data path, accept support or per-socket provider-FD transport. Unsupported
data operations fail; there is no native TCP fallback.

## Why Linux 7.3-rc2

Pinned mainline release: Linux **7.3-rc2**, the latest mainline listed by
kernel.org when inspected on 2026-09-13 (latest stable was 7.2.5).
The initial 6.18.40 prototype retained a C wait queue. Moving to 7.3-rc2
allows direct use of upstream `rust/kernel/sync/poll.rs`:

- `PollTable::register_wait` registers before sampling readiness.
- `PollCondVar` wraps condition-variable wakeups.
- Its destructor performs pollfree notification and waits for an RCU grace
  period, so our code does not implement wait-queue teardown.

This is actual reuse of upstream kernel abstractions, not copied wrappers.
`rust/kernel/net/mod.rs` still exposes PHY and netlink, not a socket-family
or `proto_ops` abstraction. The Rust misc-device API lacks a poll callback.
Moving to latest mainline does **not** eliminate the remaining C adapter.

The production Linux 6.18.40 tree and ABI5 implementation remain untouched.
A release candidate is appropriate for this API experiment, not automatically
a production-kernel recommendation.

## Ownership and the unsafe boundary

`lifecycle.rs` forbids unsafe code. Kernel `Arc`, `KBox`, pinned `Mutex` and
`PollCondVar` own namespace state, provider generation, admission (256 sockets),
readiness and teardown. Namespace state keeps values, not back-references to
socket owners, avoiding an ownership cycle.

`rust_adapter.rs` is the narrow unsafe FFI boundary. Each foreign allocation
has one owner; callback borrows never escape. Linux VFS keeps file callbacks
alive and excludes final release. C never interprets a Rust object layout.

`linux_adapter.c` handles family/per-net/misc-device registration, capability
checks, socket allocation, net references, FD callbacks and user-copy. It
contains no generation policy, readiness state, socket registry or wait queue.
Linux dup/fork shares the file owner, rather than creating more Rust owners.
Final provider-file release revokes a generation; replacement cannot revive
old sockets. Final socket-file release drops its Rust owner and returns quota.

**Sleepability matters:** Rust destructors take a mutex; namespace wait
destruction may wait for RCU. Socket state is dropped from sleepable final
file release, not `sk_destruct`, which can run in atomic/RCU context.
The prototype owns no independent queued `struct sock` references. Adding
provider endpoint/request owners must revisit this boundary before reuse in
the production frontend.

One wait queue and mutex per namespace intentionally simplify the feasibility
test. The production frontend needs per-socket queues/credits, resource charging
through outstanding requests, control cancellation, send shutdown barriers and
accepted-child ownership. This experiment does not establish those contracts.
Atomic owner counters are test diagnostics only, not lifetime decisions.

## Build without changing the host kernel

Use a **disposable Linux 7.3-rc2 source tree**, native make, a supported Rust
compiler with exactly matching Rust library sources, bindgen and libclang:

```sh
bash install-kernel.sh "$TREE"
cp ../guest.config "$TREE/.config"
"$TREE/scripts/config" --file "$TREE/.config" \
  -d NETSTACK3 -e RUST -e NETSTACK3_RUST_LIFECYCLE \
  -e DEBUG_KERNEL -e PROVE_LOCKING -e DEBUG_ATOMIC_SLEEP \
  -e DEBUG_MUTEXES -e DEBUG_LIST
# Set RUST_LIB_SRC and LIBCLANG_PATH in the environment.
# Pass RUSTC, HOSTRUSTC and BINDGEN as make arguments (not just environment).
make -C "$TREE" RUSTC="$RUSTC" HOSTRUSTC="$RUSTC" BINDGEN="$BINDGEN" olddefconfig
make -C "$TREE" RUSTC="$RUSTC" HOSTRUSTC="$RUSTC" BINDGEN="$BINDGEN" rustavailable
make -C "$TREE" -j4 RUSTC="$RUSTC" HOSTRUSTC="$RUSTC" BINDGEN="$BINDGEN" bzImage
gcc -O2 -Wall -Wextra -Werror lifecycle-test.c -o "$ROOT/bin/lifecycle-test"
bash run-kvm.sh "$TREE/arch/x86/boot/bzImage" "$ROOT" "$NEW_OUTPUT"
```

ROOT must contain BusyBox and the test binary's ELF dependencies. The runner
boots a private no-network KVM guest, rejects test failures and kernel warnings,
and exits within 60 seconds. The test protocol in `protocol.h` is only a
readiness injector for this slice, not ABI5. Nothing installs a host kernel.

Tests cover IPv4/IPv6 create/poll, app/provider dup/fork, provider SIGKILL,
generation replacement, interrupted `epoll_pwait`, quota recovery, invalid
user pointers/fields, namespace isolation/reclamation, 100 wake races and
eight concurrent workers doing 500 lifetimes each. No SSH/data-path claim
applies to this experimental kernel.

The next stage is now implemented and verified in
[full Rust ABI5](../rust-abi5/README.md); the historical checkpoint below
describes what this standalone lifecycle fixture establishes.

## Historical result and next decision

**Feasible, and latest Linux improves the boundary.** The complete 7.3-rc2
suite passed four times with lockdep, debug mutexes and atomic-sleep checks:
16,000 concurrent lifetime cycles in total. The 6.18.40 prototype also passed
its debug suite; 7.3-rc2 additionally removes local C wait-queue ownership.
See [toolchain, configuration and raw-result excerpts](evidence.txt).

Proceed incrementally: next prove scoped provider endpoint and queued-request
owners, cancellation and quota retention in Rust. Do not replace the working
ABI5 frontend until that slice and the real Netstack3 data path pass the
existing KVM/SSH acceptance tests. This experiment is not evidence that all
remaining C glue can disappear or that the complete rewrite is already safer.

## Typed endpoint-file infrastructure

`endpoint_file.rs` supplies read/write/poll/ioctl callbacks with typed `Arc<T>`
private ownership. It reuses upstream `FileDescriptorReservation` and file
references; a small C allocation helper calls `anon_inode_getfile`. Socket state
can now outlive its application file through an endpoint owner. KVM additionally
checks endpoint dup, quota retention, scoped reads, provider death and FD-limit
rollback. This is the first infrastructure patch toward full ABI5, not its data
path implementation. Earlier artifact hashes in evidence.txt describe the
initial prototype; the endpoint patch is identified in its Git commit.
