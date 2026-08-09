# Pinned `wlan-mlme` host packaging inventory

This directory contains host build metadata and an external integration
harness. The adjacent `src/**` tree is materialized unchanged from Fuchsia commit
`1e1219e3fac944c9a906aea9646939746b6062b3` by `prepare-upstream`. The manifest
transcribes the pin's `BUILD.gn` client production dependencies and is a member
of the shared pinned-source workspace.

## Reviewable host selection

`wlan-mlme-host.patch` is applied only after the pristine pin is materialized.
It makes no SAE, association, RSN, EAPOL, key, timeout, cancellation, frame
parse/build, Minstrel, `MlmeImpl`, `ClientMlme`, or client-state change.

| Patch hunk | Host rationale |
| --- | --- |
| `lib.rs`: gate `ap` | AP runtime is unrelated to the client gate and retains Fuchsia endpoint dependencies. |
| `lib.rs`: gate `DriverEvent*`, `mlme_main_loop`, `main_loop_impl`, and their imports | These are concrete driver/FFI service plumbing; the harness pumps the retained `MlmeImpl` methods directly. |
| `lib.rs`: publish the existing `MinstrelWrapper` alias | External `DeviceOps` implementations must name the trait's existing Minstrel parameter; ownership and implementation are unchanged. |
| `device.rs`: gate concrete `Device`, its `start` trait method/implementation, FFI imports, and upstream endpoint-backed test fake | The external harness implements every retained effect seam without FIDL endpoints or driver handles. |
| `error.rs`: map host FIDL errors to `ZX_ERR_IO` | Host FIDL exposes only transport-unavailable values; status extraction remains Fuchsia-only. |
| `client/bound.rs`: convert frame-writer vectors into owned host arena frames and transfer the data-frame suffix | Fuchsia's arena pointer recovery has no host meaning. The host allocation owns and preserves the exact bytes passed to `DeviceOps`. |
| `wlan-sme-host.patch`: gate scheduled-scan VMO serialization | The existing host SME has no FIDL VMO transport; ordinary connect/MLME policy remains compiled and unchanged. |

The host `fdf` crate provides owned allocation, mutable frame construction, and
explicit ownership transfer. The host `fuchsia-async` crate reuses the
monotonic Zircon values used by WLAN common. Neither facade supplies I/O,
endpoints, driver callbacks, or no-op success.

The excluded runtime closure remains available in the pristine reference but
is deliberately not recreated on the host:

| Excluded closure | Representative pinned uses |
| --- | --- |
| Driver runtime arena ownership | `fdf::{Arena,ArenaBox,ArenaStaticBox}`, raw arena recovery and transfer lifetime |
| Endpoint/runtime transport | `fidl::endpoints`, channel/proxy/request/responder types, transport errors, bridge bootstrap |
| WLAN FFI transport | Ethernet/WLAN RX/TX transfer ownership, borrowed-operation completion, event sender callbacks |
| Async/runtime behavior | `fuchsia_async` timer construction and channel-switch wakeups |
| Full generated schema behavior | flexible-enum constants/methods, event extraction helpers, SoftMAC bridge protocols |
| Host primitive parity | monotonic-duration arithmetic and trace enablement |

The reference fetch closure now retains the exact pinned `fuchsia-async` and
driver-runtime Rust sources as well as the already retained complete WLAN tree.
Those sources are evidence and future packaging inputs, not host facades.

Implementing those APIs as inert fakes would create fake transport semantics
inside the production crate and would not prove the requested client gate.
Selecting only client source files from an external crate would instead create
a second, extracted MLME closure. Both are rejected.

## Offline gate

`host-client-gate` sets `sme_handler_supported = true` and
`driver_handler_supported = false`. It pumps only production `MlmeRequest`,
`MlmeEvent`, peer-frame, and timer seams against a pinned `wlan-rsn`
Authenticator. It covers SAE frame/timer production, authentication and
association RX, EAPOL, PTK/GTK/IGTK installation, confirmations, controlled
port ordering, cancellation, and stale SME/MLME timers. No MT7921 `DeviceOps`,
VFIO, management-TX enablement, or `open_client` extension is present.

The integration gate was repeated from a newly fetched, repo-local copy of the
exact pin. Before building, every patch in `upstream-cargo/patches` was applied
with GNU `patch --fuzz=0`; output containing `fuzz` or `offset` was rejected.
The complete shared patch set applied exactly. From that clean root,
`cargo test --locked --manifest-path <root>/src/connectivity/network/netstack3/Cargo.toml -p wlan-mlme --test host-client-gate`
passed both tests, and the corresponding `-p fdf` arena test passed. Evidence
from a previously materialized reference is not accepted for this gate.
`prepare-upstream` enforces the same rule: it either applies each patch exactly
with zero fuzz/offset or proves by an exact reverse dry-run that it is already
applied. A content hash stamps the complete patch set, so a changed patch
requires a fresh reference root instead of reusing contaminated material.
