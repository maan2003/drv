# Netstack3 host-provider ABI v2

Version 1 is an experimental incompatible spike. It cannot represent an empty
UDP datagram, peer addresses, UDP connect, ephemeral bind, directional
shutdown, EOF, connect completion, socket errors, or listener readiness. A v1
endpoint and v2 endpoint reject one another; there is no negotiated downgrade.

V2 retains the 40-byte little-endian `NS3P` header, immutable namespace/client
identity, 64 KiB payload limit, and bounded transport. The header version is 2.
Every response repeats request ID, opcode, namespace, and client. Response
payload is `[0, result...]` or `[1, stable-error-u8]`. A malformed, duplicate,
unsolicited, wrong-identity, or wrong-opcode response is a provider-channel
protocol failure, not an operation error.

## Primitive encodings

- `handle`, event sequence: `u64le`; lengths/flags/backlog/quota: `u32le`;
  port: `u16le`, including zero.
- address: tag `4`, 4 address bytes, port; or tag `6`, 16 address bytes, port.
  Optional address additionally permits tag `0`.
- socket kind: `1=UDP`, `2=TCP`; family: `4` or `6`.
- shutdown: `1=read`, `2=write`, `3=read+write`.
- name selector: `1=local`, `2=peer`.
- readiness mask: readable, writable, incoming, read-closed/EOF,
  write-closed, error, connected, and connect-failed are bits 0 through 7.
  `ReadinessChanged` is `sequence:u64, handle:u64, mask:u16, error:u8`.
  Sequence must increase per handle, making stale/reordered events detectable.
- receive result: `eof:u8, msg_flags:u32, original_len:u32, optional-source,
  data...`. No queued message is the `WouldBlock` error, while a zero-length
  datagram is a successful result with source and `original_len=0`.
- options: `1=SO_REUSEADDR`, `2=SO_REUSEPORT`, `3=SO_BROADCAST`,
  `4=SO_KEEPALIVE`, `5=SO_RCVBUF`, `6=SO_SNDBUF`, `7=SO_LINGER`,
  `8=TCP_NODELAY`, `9=IPV6_V6ONLY`, `10=TCP_KEEPIDLE`,
  `11=TCP_KEEPINTVL`, `12=TCP_KEEPCNT`. Values are length-prefixed bytes;
  unsupported options return `NotSupported`. `SO_ERROR` uses its consuming
  operation rather than a generic option.

## Complete operation and state table

| # | Operation | Request | Success | Allowed state → state |
|---:|---|---|---|---|
| 1 | OpenClient | quota | empty | identity absent → open |
| 2 | CloseClient | empty | empty | open → closed; revokes handles |
| 3 | OpenSocket | kind, family | handle | client open → unbound |
| 4 | Bind | handle, optional address, port | actual local address | unbound → bound |
| 5 | Connect | handle, peer | empty | UDP unbound/bound → connected; TCP → connecting |
| 6 | Disconnect | handle | actual local address | connected UDP → bound |
| 7 | Listen | handle, backlog (zero allowed) | actual local address | TCP unbound/bound → listening |
| 8 | Accept | listener handle | child handle, local, peer | listening → listening; child connected |
| 9 | SendMsg | handle, flags, optional peer, bytes | sent length | UDP unbound/bound/connected; TCP connected/read-closed |
| 10 | RecvMsg | handle, max length, flags | receive result above | bound/connected/write-closed |
| 11 | Shutdown | handle, direction | empty | connected → read/write/both closed |
| 12 | GetName | handle, selector | address | every live socket; peer requires connected |
| 13 | GetSocketError | handle | error code (`0=none`) | every live socket; consumes pending error |
| 14 | SetOption | handle, option, value length, value | empty | live socket, subject to option timing |
| 15 | GetOption | handle, option | value length, value | every live socket |
| 16 | Readiness | handle | sequence, mask, error | every live socket |
| 17 | Close | handle | empty | every live socket → closed |
| 18 | ReadinessChanged | event payload above | no response | provider event for a live handle |

A TCP connect response may be `InProgress`; completion is then represented by
exactly one of connected/connect-failed readiness plus the pending socket error.
Read-closed remains readable so `recvmsg` can return EOF. Listener incoming is
level state, not an edge counter.

## Loopback and wildcard policy

The initial Linux adapter uses **conservative explicit-address policy**. All of
IPv4 `127/8` and IPv6 `::1` use a hidden Linux kernel socket. Explicit other
addresses use the provider. An unspecified-address bind (`0.0.0.0` or `::`) is
rejected with `EOPNOTSUPP` rather than falsely omitting loopback or leaking
remote traffic to Linux. A future dual-backed wildcard socket must coordinate
both backends, return one coherent ephemeral port, merge listener/datagram
readiness, and preserve datagram peer identity before this policy can change.
Mixed-destination unconnected UDP is classified per `SendMsg`; it therefore
requires both hidden handles after the first destination and must never migrate
an existing flow.

## Pinned Fuchsia semantic source map

Baseline: Fuchsia `1e1219e3fac944c9a906aea9646939746b6062b3`.

- `src/connectivity/network/netstack3/src/bindings/socket/datagram.rs`:
  `TransportState::{connect,bind,disconnect,shutdown,get_socket_info}` defines
  the corresponding stateful operations; bind uses `Option` for address and
  local identifier, preserving wildcard and ephemeral values.
- The same file's synchronous `connect`, `bind`, `get_sock_name`,
  `get_peer_name`, `recv_msg`, and `shutdown` handlers preserve POSIX results.
  `recv_msg` returns `EAGAIN` for no message, returns source metadata for a
  message, and separately returns empty data for receive shutdown.
- `src/connectivity/network/netstack3/src/bindings/socket/stream.rs` signals
  connection establishment and changes the incoming signal from the accept
  queue count; its request handling includes accept, get-error, both names,
  and directional shutdown.
- `src/connectivity/network/netstack3/src/bindings/socket/queue.rs` maintains
  level-readable and pending-error notifications rather than lossy edges.

The project-owned wire format transports those semantics; it does not expose
FIDL, Zircon handles/signals, or Linux types.
