# ABI5 kernel frontend in Rust

This is the complete ABI5 frontend, not the lifecycle prototype. It registers
AF_INET/AF_INET6 with native INET excluded and uses the existing, unchanged
sandboxed Netstack3 provider. Tested on Linux **7.3-rc2**, Rust **1.97.0** and
GCC **15.2.0** on np using native make, not a Nix build.

## Ownership and trust boundaries

- `frontend.rs` forbids unsafe code: namespace generations, socket quota,
  bounded request/TX/RX queues, credits, connect/readiness, accepted children,
  controls and producer shutdown barriers all live here.
- `linux.rs` and `rust_main.rs` isolate unsafe native references, address/iterator
  conversion, foreign Arc ownership and Linux callbacks.
- `glue.c` only supplies native registration and socket/file/refcount mechanics;
  it does not implement ABI5 queues or TCP/IP.
- Shared `../rust-lifecycle/endpoint_file.rs` owns typed file callbacks.
  Upstream `Arc`, `ARef<File>`, `FileDescriptorReservation`, `PollTable`,
  `PollCondVar`, mutexes, usercopy and iterator wrappers are reused.

Application release removes the registry owner. Endpoint FDs can retain socket
state and quota afterward; final endpoint release revokes that socket.
Requests are values, not self-owning socket references. Accepted-child
destruction happens outside listener/namespace locks. FD reservation, usercopy,
file creation and queue publication are transactional. Failed file creation
does not invoke endpoint revocation. CLOSE/CREDIT/accept-space reads allocate
nothing. Interrupted/failed copies do not consume queue state or iterator bytes.

The wire contract remains `../protocol.h`, version 5: 256 sockets per namespace,
32 request slots (eight reserved for control), 256 KiB TX admission, 16 KiB
payloads and four RX credits. SHUTDOWN/CLOSE preserve the userspace producer
handoff barrier; completion is not a remote TCP acknowledgement.

**Cancellation refinement:** mutating controls still revoke on ambiguous
interruption/timeout. GETNAME is read-only: its interrupted waiter detaches,
but the bounded request ID remains until its late completion. That completion
cannot overwrite a newer waiter or publish SO_ERROR. This fixes Go SIGURG
preemption killing name-query sockets, exposed by real Tailscale startup.
There is no signal-specific special case or disabled Go preemption.

## Build and run

From this directory, against a disposable source tree; never install on the host:

```sh
bash install-kernel.sh "$TREE"
cp ../guest.config "$TREE/.config"
"$TREE/scripts/config" --file "$TREE/.config" \
  -d INET -d NETSTACK3 -d NETSTACK3_RUST_LIFECYCLE \
  -e RUST -e NETSTACK3_RUST -e DEBUG_KERNEL -e PROVE_LOCKING \
  -e DEBUG_ATOMIC_SLEEP -e DEBUG_MUTEXES -e DEBUG_LIST
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
and their ELF dependencies. No provider rebuild or framing adapter is needed.
Use `../run-tailscale-kvm.sh` and its documented private no-login controller/peer
for OpenSSH application acceptance.

Use this installer for Rust. The parent installer and Linux 6.18 configuration
remain the historical C reference/recovery build, not a runtime fallback.
The new GETNAME cancellation regression intentionally exposes that C baseline's
old cancellation behavior. Do not enable either C or lifecycle frontend with
`NETSTACK3_RUST`; Kconfig excludes both.

## Verified outcome and limits

[Evidence](evidence.txt): three consecutive final full production KVM suites,
including endpoint failure/credit/cancellation/quota tests, real IPv4/IPv6
TCP/UDP, sendmmsg, refused SO_ERROR, shutdown backpressure, large and parallel
transfers, provider death/replacement, sandboxed Ethernet DHCP/DNS/TCP and NSS.
Real OpenSSH command, PTY and exact 8/8/64 MiB round trips pass through a private
Tailscale relay on the same lock-debug kernel with `CONFIG_INET=n`.
The shared infrastructure separately passes the lifecycle suite, including
4,000 concurrent lifetimes, namespace reclamation and FD-limit rollback.

No significant blocker to using Rust for ABI5 remains. This is not proof of
hostile-provider safety, exhaustive allocation-failure behavior, release-kernel
performance or physical Wi-Fi deployment. No KASAN run is claimed. Existing
unsupported socket options and missing production network-metadata publication
remain; the private Tailscale lab still uses its documented network-up override.
