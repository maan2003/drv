# Encrypted DNS service

Independent full-message Quad9 DoH forwarder for the served Netstack3 network
namespace. See [upstream provenance](UPSTREAM.md). The default configuration
uses Quad9's filtered, DNSSEC-validating service at two numeric IPv4 endpoints;
TLS verifies `dns.quad9.net`. There is no bootstrap lookup, system resolver,
plaintext fallback, dynamic discovery, local DNSSEC validation, or serve-stale.

## Process boundary

A trusted launcher starts this executable as root, single-threaded, with:
- FD 3: read-only regular TOML configuration, at most 8 KiB.
- FD 4: read-only regular PEM CA bundle, at most 2 MiB.
- stdout/stderr: write-only logging pipes, not terminals or files.
- a clean, trusted dynamic-loader environment and installed ELF dependencies.

Before reading either input or accepting requests, setup binds IPv4/IPv6
loopback UDP/TCP port 53 and `/run/drv-resolver.sock`, closes other descriptors,
replaces stdin with EOF, enters a private empty-root mount namespace, drops
UID/GID to 65534 and all capabilities, sets no-new-privileges and installs a
fatal-default TSYNC seccomp filter. The served network namespace is retained.
Only then are configuration and CA parsed and two Tokio workers started.

Runtime authority is loopback listeners, accepted client sockets, ordinary
Internet stream/datagram sockets, anonymous memory, reactor descriptors and
logging pipes. No filesystem/device access, SCM_RIGHTS reception, process
creation, executable mappings, raw/netlink sockets or namespace changes.
Internet socket authority is not limited to Quad9 by seccomp: static endpoint
selection is service policy; network-namespace egress policy can further
restrict a compromised process.

The launcher owns stale NSS path removal, logging, restart and CA replacement.
Stop and reap the old daemon before unlinking its socket. The userspace provider
may need a short teardown interval before rebinding port 53. Do not enable the
provider's legacy `--resolver` concurrently.

```sh
drv-dns-service 3</etc/quad9.toml 4</etc/ssl/certs/ca-certificates.crt \
  2>&1 | tee /run/dns.log
```

`DNS_LOCKED` precedes parsing; `DNS_READY` means listeners are running, not that
an uplink exists. Connections are lazy and fail closed when offline. NSS uses
the existing `drv-dns-wire` and `libnss_drv` ABI; applications can also send
ordinary DNS queries over loopback UDP/TCP.

## Bounds and semantics

Global admission: 64 queries (NSS atomically reserves two for concurrent A/AAAA).
Each TCP/NSS listener admits at most 64 connections. Whole client requests have
four-second deadlines; endpoint attempts have at most 1.5 seconds. UDP replies
are capped at 1232 bytes, or 512 without EDNS; library encoding truncates complete
records and sets TC. TCP/DoH bodies are capped at 65535 bytes and HTTP headers at
16 KiB. The process has 256 descriptors and 512 MiB address-space limits.

H2 connections are shared, lazy, generation-scoped and owned; retiring one does
not abort another request still using it. Canceled request streams release their
resources through h2's drop implementation. Idle/partial TCP frames are bounded;
a TCP disconnect during an upstream exchange is noticed at response write or
the request deadline. NSS disconnects cancel immediately during lookup.

The conservative complete-message LRU holds at most 256 entries and 4 MiB of
wire/key data. Exact zero-ID request keys distinguish flags and EDNS semantics.
Unknown EDNS options, signatures, transient errors and truncated replies bypass
caching. TTLs include HTTP Age; negative SOA TTLs are normalized before returning
or caching. No TTL extension. AD is relayed only to AD/DO-aware clients.

## Verification

Run native Cargo on the build host (no Nix build):
```sh
cargo test --manifest-path crates/net/dns-service/Cargo.toml -- --test-threads=1
cargo build --release --manifest-path crates/net/dns-service/Cargo.toml --examples
```

`examples/check.rs` exercises real Quad9 via loopback IPv4/IPv6 UDP/TCP,
A/AAAA/MX/TXT/HTTPS, NXDOMAIN/SOA, concurrent clients and real glibc NSS.
`examples/deny.rs` is **guest-only**: it changes mount/privilege state and
deliberately triggers fatal seccomp violations. Never run it on a shared host.

For a no-native-INET KVM, prepare the existing production root with the provider,
`netstack3-virtio-lab`, daemon, release examples as `/bin/dns-check` and
`/bin/dns-deny`, their ELF libraries, CA/config and libnss_drv. Then reuse the
isolated SLIRP runner with this crate's guest init:
```sh
bash crates/net/netstack3-port-spike/kernel-provider/production/run-tailscale-kvm.sh \
  "$KERNEL" "$ROOT" "$NEW_OUTPUT" lab/dns/guest-init
```
This does not detach host hardware or change host networking. Physical MT7921
acceptance is a separate gate through the production WLAN policy/service path.
