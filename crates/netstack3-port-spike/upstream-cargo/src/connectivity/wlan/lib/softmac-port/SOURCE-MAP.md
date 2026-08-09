# Source map

Fuchsia pin: `1e1219e3fac944c9a906aea9646939746b6062b3`.

| Packaged behavior | Pinned source | Host disposition | License |
| --- | --- | --- | --- |
| SoftMAC/MLME/channel/capability values | `sdk/fidl/fuchsia.wlan.{ieee80211,mlme,softmac}/**` | Uses the existing path-shaped host schema crates directly | Fuchsia BSD-2-Clause |
| Passive scan validation, offload request construction, scan identity/completion handling | `src/connectivity/wlan/lib/mlme/rust/src/client/scanner.rs` | Synchronous extraction over `SoftmacHardware`; IEEE Time Unit conversion, state transitions, error outcomes, and request fields retained | Fuchsia BSD-2-Clause |
| Hardware operation boundary | `src/connectivity/wlan/lib/mlme/rust/src/device.rs` (`DeviceOps` scan/channel methods) | Narrowed to the portable scan milestone; no FIDL transport, executor, Ethernet, association, or radio implementation | Fuchsia BSD-2-Clause |
| Fake adapter and host fixture plumbing | Local test boundary only | Records typed requests and emits deterministic queued observations; cannot perform I/O | Fuchsia BSD-2-Clause |

The source headers in `src/lib.rs` preserve Fuchsia authorship and license.
The canonical license and full shared-overlay provenance are retained in
`../../../../../{LICENSE.fuchsia,PROVENANCE.md}`.
