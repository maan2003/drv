# Portable pinned-Fuchsia SoftMAC scan boundary

This crate is the first host-portable SoftMAC milestone. It packages the
passive offload-scan portion of Fuchsia's Rust MLME scanner at commit
`1e1219e3fac944c9a906aea9646939746b6062b3` behind a synchronous, capability-
shaped hardware trait. The trait uses the pinned Fuchsia schema values directly:
`WlanSoftmacQueryResponse`, `DiscoverySupport`, `ChannelNumber`, SoftMAC scan
and set-channel requests, and MLME `BssDescription`/scan results. It introduces
no second channel, capability, scan-result, or regulatory model.

`FakeMt7921Adapter` is deliberately the only adapter here. It records typed
requests and returns queued beacon/probe observations; it has no register, DMA,
firmware, transport, or radio access. Consequently this milestone cannot tune
or receive from physical hardware by construction. A future physical adapter
must implement the same boundary and remains responsible for converting
received beacon/probe frames to Fuchsia's `BssDescription` with the pinned WLAN
common/MLME conversion code.

The scanner keeps Fuchsia's rejection and dwell-time conversion behavior: one
scan at a time, nonempty channel list, maximum dwell not below minimum dwell,
scan-offload support required, and IEEE 802.11 Time Units converted at 1024 us.
Focused fixtures derived from the upstream MLME scanner run against the fake:

```sh
cargo test -p fuchsia-softmac-port
```

## Source and license

See [`SOURCE-MAP.md`](SOURCE-MAP.md). Fuchsia-derived code is BSD-2-Clause and
the exact license is retained as
[`../../../../../LICENSE.fuchsia`](../../../../../LICENSE.fuchsia).
The shared pin and fetch/overlay process remain documented in
[`../../../../../PROVENANCE.md`](../../../../../PROVENANCE.md).

This boundary supports deterministic development without hardware as required
by [REQ-hardware-independent-testing](../../../../../../../../specs/REQ-hardware-independent-testing.md)
and exposes no host plumbing in accordance with
[REQ-host-portability](../../../../../../../../specs/REQ-host-portability.md).
