# Rust NSS client for the native resolver

`libnss_drv.so.2` supplies glibc forward hostname lookup. Build this crate as a
`cdylib`, install the resulting `libnss_drv.so` under the SONAME filename
`libnss_drv.so.2` in the target loader's library path, and configure the target:
```
hosts: files drv
```
Do not add `dns` as a fallback when fail-closed native-resolver ownership is
required. Nothing here modifies the build host's NSS configuration.

Start `netstack3-provider --resolver` to bind `/run/drv-resolver.sock` before
sandboxing. Its parent must provide a root-owned `/run` and own removal of the
socket path after the old provider exits; startup never unlinks an existing
socket. Socket permissions permit local clients to query. A sandboxed client
must be granted access to this path. DNS servers come from service-owned DHCP
configuration, never NSS environment variables or diagnostic-log parsing.

## Safety boundary

`ffi.rs` is the single common foreign-pointer adapter plus three ABI aliases.
It validates representable arguments, catches unwinding panics, converts the
caller storage to `MaybeUninit` slices, and writes typed C pointer tables only
after safe layout checks pass. Caller pointers still require glibc's ordinary
validity/nonaliasing contract; Rust cannot validate arbitrary addresses.
Returned names, address bytes and pointer tables live in caller storage.

`safe.rs` forbids unsafe code. It owns lookup policy, bounds/alignment
calculation, fixed-protocol validation, and deadline-bounded I/O using
`std`/`rustix`. No DNS engine, Netstack3, executor, background threads or shared
connection mutexes are loaded into applications. The DNS wire crate and
resolver server also forbid unsafe code. Allocation failure may still abort
the calling process; memory safety does not imply process isolation.

## Supported scope

IPv4/IPv6 forward lookup, `gethostbyname[_r]`/`getaddrinfo` via glibc's name2/name3
fallback, ERANGE retry, concurrent calls and lookup after fork. Returned TTL
is zero; the returned name is the requested name, not a promised CNAME target.
Reverse lookup, enumeration and NSS-independent/static/musl resolvers are not
implemented. Upstream DNS transport currently uses IPv4 servers; AAAA results
are supported. Missing/unavailable service fails without creating INET sockets
inside NSS. The service bounds clients to 64 and request lifetime to four
seconds; NSS has a five-second overall I/O deadline.

The maintained no-INET KVM fixture dynamically loads the shared library and
checks real glibc A/AAAA lookup, NXDOMAIN, UDP truncation → TCP fallback,
misaligned/short buffers, pointer terminators, threads and fork. It configures
only the disposable guest, not np's libc resolver.
