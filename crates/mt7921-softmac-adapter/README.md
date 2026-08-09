# MT7921 pinned-Fuchsia SoftMAC adapter

This GPL-2.0-only crate is the offline-testable seam between the typed MT7921
NIC capability/channel inventory in `mt7921-port-spike` and the BSD-2-Clause
`fuchsia-softmac-port` at Fuchsia commit
`1e1219e3fac944c9a906aea9646939746b6062b3`. Keeping the adapter separate makes
the combined Linux-derived/Fuchsia-derived distribution and its provenance
explicit; neither dependency is relicensed.

The crate lives in the root `crates/` tree as a small standalone workspace. The
root workspace excludes it, while its path dependency targets the exact
materialized Fuchsia commit prepared by `netstack3-port-spike`. This avoids
copying the BSD sources or adding a GPL package to the Fuchsia workspace.

`Mt7921PassiveTransport` is deliberately limited to set-channel, passive scan
start/cancel, and event receive mechanics. `Mt7921SoftmacAdapter` owns scan IDs,
validates an already-authorized conservative channel list against typed NIC
candidates, rejects active or unauthorized operations, enforces monotonic RX
timestamps, and waits for matching completion after cancellation. Transport or
untrusted-input failures poison the state machine. Beacon/probe IEs are passed
to pinned Fuchsia `construct_bss_description`; this crate contains no IE parser.

The crate has no transport implementation outside its scripted unit test. It
cannot open a device, send an MCU command, tune a radio, or interact with Linux
network policy. In particular it does not use `vfio_read`, mac80211, cfg80211,
or the temporary `iw` scan binary.

## Physical transport roadmap (not implemented)

A future, separately authorized physical transport must supply these exact
MT7921 mechanics before this adapter can operate hardware:

1. encode and submit the mt76/MT7921 MCU channel-context command for a 20 MHz
   primary channel and match its sequence/status completion;
2. encode the Connac hardware passive-scan request with this adapter's scan ID,
   channel list, and nanosecond dwell bounds converted with checked units;
3. encode the matching hardware-scan cancellation command and distinguish its
   acknowledged completion from the asynchronous scan-done event;
4. drain MCU scan events and data RX descriptors, correlate scan IDs, and expose
   beacon/probe BSSID, fixed fields, untouched IE bytes, primary channel, RSSI,
   and a broker-supplied monotonic timestamp; and
5. bound queues/frames, validate descriptor lengths and generations, route
   timeout/reset failures into transport errors, and discard late events after
   cancellation or reset.

Regulatory authorization, CLC interpretation, Linux/mac80211/cfg80211 policy,
DMA/IRQ ownership, firmware boot, reset, and physical enablement remain outside
this crate and this offline milestone.

## Verification

After materializing the repository's pinned Fuchsia source closure:

```sh
./crates/netstack3-port-spike/prepare-upstream
cargo test --locked --manifest-path crates/mt7921-softmac-adapter/Cargo.toml
```
