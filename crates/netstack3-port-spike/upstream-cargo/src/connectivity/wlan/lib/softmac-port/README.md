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

After SME-managed SAE succeeds, the same pinned client closure can now enter
association directly without sending a second authentication frame. The
request carries the negotiated RSNE using pinned `BoundClient` layout, the
response retains the existing capability intersection and typed
`WlanAssociationConfig`, and the controlled port remains closed for the later
EAPOL/traffic-key milestone.

`SaeHandshake` now packages the pinned Fuchsia SME-managed `wlan-rsn`
supplicant for a management-auth-only WPA3 stage. It emits only pinned
`SaeFrame`, timeout, authentication-status, and one non-printable PMK handoff;
association, EAPOL, traffic keys, and data remain outside this boundary. PMK
bytes are borrow-only and overwritten on drop. After association it also
accepts pinned EAPOL-Key PDUs and emits EAPOL TX, PTK/GTK/IGTK installation,
and ESS-SA-established updates. Traffic-key bytes are likewise borrow-only and
zeroized on drop; discarded source key hierarchies are overwritten during
conversion. The wrapper and its updates have no `Debug` implementation so
secret-bearing state cannot enter reports.
`build_sae_auth_frame` retains the exact pinned MLME management-frame layout.
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
| MLME client authentication/association and connect timer | MLME `client/{state,station,bound}.rs`, `auth.rs`, `device.rs` | Open-network closure plus post-SAE protected association packaged over raw RX/TX bytes and typed device programming; protected controlled-port opening and associated-state maintenance remain gated |
| SME connect/scan policy | `wlan-sme` | Already packaged and host-tested; endpoint serving is the only excluded transport edge |
| RSN, SAE/OWE, EAPOL | `wlan-rsn`, `wlan-fcg-crypto`, `eapol` | Pinned SME-managed SAE, borrow-only PMK, EAPOL RX/TX, and zeroizing PTK/GTK/IGTK handoffs are wrapped here; hardware key programming remains injected |
| Frame/IE parsing and serialization | `wlan-common`, `ieee80211`, `wlan-frame-writer` | Already packaged and tested; hardware supplies RX bytes/metadata only |
| Rate control and diagnostics | MLME `minstrel.rs`, FIDL Minstrel/stats values, Inspect facades | Values/diagnostic facades are packaged; the MLME algorithm remains in the future client-closure package |

The only MT7921-owned pieces are firmware/MCU commands, DMA/IRQ/RX transport,
calibration, reset containment, and conversion of device RX metadata into the
pinned SoftMAC value types. Authentication/association remain reachable only
through the offline fake in this milestone; the dormant MT7921 management-TX
adapter is rejected before VFIO is opened while the channel domain is `NO_IR`.

## Physical authentication gate inventory

Healthy preflight identified the authorized existing network as WPA3-Personal
only: CCMP group/pairwise, SAE AKM, and management-frame protection required.
It is not WPA2 transition mode. Consequently open-system authentication is not
valid for this target; the physical gate must use SAE authentication frames.
Credential material may enter only through a root-only ephemeral handoff and
must never be printed, persisted in reports, committed, or retained after reset.
The first physical stage stops after SAE authentication and before association,
EAPOL, key installation, or data.

Read-only no-plastic evidence found kernel regulatory domain `00: DFS-UNSET`;
its 5170--5250 MHz rule is `PASSIVE-SCAN`. The associated AP is on channel 36
(5180 MHz), which is non-DFS, and advertises Country `IN`, channels 36--48 at
30 dBm, but its environment byte is reported as invalid. Neither AP-controlled
Country information nor the absence of DFS/CAC authorizes initiating radiation.

The pinned Fuchsia authorization source is
`wlancfg/regulatory_manager.rs`: a two-byte update from the authoritative
`RegulatoryRegionWatcher` is passed to `IfaceManager::set_country`. The pinned
ordering first stops client connections and APs, then calls
`PhyManager::set_country_code`, which invokes `DeviceMonitor.SetCountry` for
every PHY; failure remains a failure rather than falling through to TX. A host
port may use user/location authority with this stop/set/recreate ordering, or
the smaller pinned cfg80211 beacon-hint transition: an error-free direct ESS
beacon on exact non-radar channel 36 while world-roaming clears `NO_IR` for
that channel. `BeaconHintAuthorizer` narrows this further to the configured
SSID+BSSID and issues only a run-scoped token invalidated by channel,
regulatory-domain, or reset transitions. Channel 36 needs no CAC, but the
token and a live-verified regulatory/SAR/rate-power submission are both required
before the dormant SAE adapter can be enabled.

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
