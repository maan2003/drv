# Portable pinned-Fuchsia SoftMAC client boundary

This crate packages host-portable portions of Fuchsia's Rust client MLME at commit
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
Cancellation retains the device scan ID and the scanner remains busy until the
matching hardware completion arrives.

`OpenClientMlme` adds the pinned open-system client closure without moving WLAN
policy into MT7921: `Joined -> Authenticating -> Associating -> Associated`, the
single beacon-relative `Connecting` timeout, exact authentication and association
management frames, RX filtering/parsing, capability intersection, typed
`WlanAssociationConfig`, controlled-port opening, and `MlmeEvent::ConnectConf`.
The exact pinned `auth.rs` is compiled by path. Frame construction/parsing and
capability negotiation use the pinned `wlan-frame-writer` and `wlan-common`
implementations. Its hardware trait is restricted to management-frame transport,
association programming/clear, and the existing Ethernet-up notification.

The deterministic fake closure covers successful open association, wrong-BSSID
and invalid-auth input, AP rejection, capability mismatch, authentication and
association timeout, management transport failure, and device-programming
failure. It has no physical adapter and therefore cannot transmit.
Focused fixtures derived from the upstream MLME scanner run against the fake:

```sh
cargo test -p fuchsia-softmac-port
```

The temporary regulatory boundary is fixed to `alpha2="00"` and indoor use.
`allowed_passive_channels` applies the pinned SME passive-scan intersection to
hardware-reported primary channels and accepts the firmware five-bit special-
UNII mask only as a restriction input. The pinned policy omits UNII-4 and has
no 6-GHz band type, so neither a permissive CLC response nor an AP Country IE
can expand this milestone's channel set.

## Pinned WLAN closure roadmap

| Responsibility | Pinned ownership | Current disposition |
| --- | --- | --- |
| Passive scan request/state and regulatory candidate intersection | MLME `client/scanner.rs`, SME `client/scan.rs` | Packaged here over Fuchsia value types; hardware execution remains gated |
| Beacon/probe IE and channel conversion | MLME `client/convert_beacon.rs`, `wlan-common` | The exact pinned `construct_bss_description` is re-exported directly and its upstream fixtures run on host |
| MLME client open authentication/association and connect timer | MLME `client/{state,station,bound}.rs`, `auth.rs`, `device.rs` | Open-network closure packaged here over raw RX/TX bytes and typed device programming; protected networks and associated-state maintenance remain gated |
| SME connect/scan policy | `wlan-sme` | Already packaged and host-tested; endpoint serving is the only excluded transport edge |
| RSN, SAE/OWE, EAPOL | `wlan-rsn`, `wlan-fcg-crypto`, `eapol` | Already packaged unchanged with pinned crypto and host tests |
| Frame/IE parsing and serialization | `wlan-common`, `ieee80211`, `wlan-frame-writer` | Already packaged and tested; hardware supplies RX bytes/metadata only |
| Rate control and diagnostics | MLME `minstrel.rs`, FIDL Minstrel/stats values, Inspect facades | Values/diagnostic facades are packaged; the MLME algorithm remains in the future client-closure package |

The only MT7921-owned pieces are firmware/MCU commands, DMA/IRQ/RX transport,
calibration, reset containment, and conversion of device RX metadata into the
pinned SoftMAC value types. Authentication/association are reachable only through
the offline fake in this milestone; no MT7921 management-TX adapter exists.

## Physical association gate inventory

No controlled AP is currently available, so physical management transmit remains
blocked. The repository contains no hostapd configuration or controlled-AP
harness. Inventory on `no-plastic` found only the MT7921-backed `wlan0`, a down
Ethernet interface, no second USB WLAN adapter, and no active hostapd service.
Nearby BSS observations from passive gates are not evidence of authorization and
must not be used as association targets. A later physical run requires a separately
identified controlled open AP, fixed channel/regulatory authorization, the existing
watchdog containment, and an MT7921 adapter that preserves this trait boundary.

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
