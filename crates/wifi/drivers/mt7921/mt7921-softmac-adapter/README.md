# MT7921 pinned-Fuchsia SoftMAC adapter

This GPL-2.0-only crate is the offline-testable seam between the typed MT7921
NIC capability/channel inventory in `mt7921-core` and the BSD-2-Clause
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

This crate itself has no device-opening implementation. The separate GPL
`mt7921-passive-scan` workspace implements its physical mechanics trait by
reusing the bounded VFIO firmware-loader backend; keeping that authority out of
the adapter preserves this crate's testable policy/mechanics boundary. Neither
crate uses mac80211, cfg80211, or the temporary `iw` scan binary.

`client_device::Mt7921ClientDevice` is the one-way mechanical `DeviceOps`
adapter for the pinned production `ClientMlme`. It forwards immutable
query/support values and only set-channel, join, exact WLAN frame bytes/flags,
key installation, association notification/clear, controlled-port link, MLME
event, and frame/RX-status queue effects. Injected effects return already-mapped
Zircon statuses; the adapter neither retries nor reinterprets them. Every other
host-retained `DeviceOps` method returns `ZX_ERR_NOT_SUPPORTED` rather than fake
success. Frames and key-bearing values are never included in adapter Debug or
error output; tests use only a documented synthetic key pattern.

`production_effects::LiveClientEffects` owns the production client effect
state behind an opaque API. Its separate, non-cloneable authorization handle is
fixed to the constructor's target and can only publish matching rate-power
readiness and SAE authorization. The synchronous diagnostic observer is
infallible and panic-contained, runs only after internal locks are released,
and receives bounded redacted events rather than frame or key bytes. TIM
telemetry publishes `partial_virtual_bitmap_sha256`, not the bitmap contents.

`Mt7921AssociationState` is the hardware-side ordering guard for those
effects. Association publishes one bounded WCID, traffic-key effects retain
only non-secret readiness bits, the controlled port cannot open before
PTK/GTK (and IGTK when MFP is required), and clear synchronously revokes
readiness before returning close-port/remove-keys/remove-WCID teardown
metadata.

The internal live constructor requires `LiveBeaconPowerAuthorization`.
Acquiring that capability is explicitly **UNIMPLEMENTED** and returns
`ZX_ERR_NOT_SUPPORTED`. The beacon and rate-power authorizers reject tokens
from other authorizer instances, but those separate owner identities prove only
which authorizer minted each token. They do not prove a shared device,
transport, reset epoch, or physical run and therefore cannot be composed into
the live prerequisite.

Before that prerequisite can be implemented, the actual VFIO device and its
scan, power, reset, regulatory-domain, and channel lifecycle must be extracted
into one authoritative owner. Only that owner can mint a shared revocable live
lease after checking both existing authorizations, and the lease must be
revalidated at the final management-frame publish point. Until then, the
offline fake constructor exists only under this crate's unit-test configuration
and the adapter cannot enable physical TX, VFIO, MMIO, DMA doorbells, or any
other transport.

The offline mechanics gate is complete: tests pass exact query, channel, join,
frame/flag, synthetic-key, association, link, MLME-event, and RX-status values
through the pinned `DeviceOps` contract; injected failures are returned once
without later effects. The live effects API exposes only production
construction/authorization and the pure frame helpers required by the physical
consumer; mutable state and diagnostic controls remain private. The pristine
production SME/MLME/RSN gate remains green. This is not evidence for
management-frame transmission or a production backend.

## Netstack3 Ethernet boundary

`Mt7921ClientDevice::new_with_ethernet` attaches the existing
`netstack3-port-spike::EthernetDevice` contract at the pinned Fuchsia client
MLME's native Ethernet seam. There is no second packet stack or Wi-Fi data
converter:

```text
MT7921 RX (802.11, firmware-decrypted) -> Fuchsia client MLME
  -> Ethernet II without FCS -> bounded Mt7921EthernetDevice -> Netstack3

Netstack3 -> Ethernet II without FCS -> bounded Mt7921EthernetTx
  -> Fuchsia client MLME -> protected 802.11 data -> MT7921 TX
```

The MLME continues to own LLC/SNAP conversion, address mapping, EAPOL routing,
the RSN controlled port, 802.11 sequence/QoS fields, and the Protected bit.
Firmware/device effects continue to own installed-key slots and actual
encryption/decryption. The adapter accepts only the existing 14--1514 byte
Ethernet-II frame contract (1500-byte IP MTU), takes the interface MAC directly
from the SoftMAC query response, reports link-up only after the MLME's
controlled-port effect succeeds, and bounds both directions. Reset/stop closes
the port, revokes the address, drains both queues, and overwrites queued frame
storage before release.

The run loop retains the `Mt7921ScanRunner` after moving the device into the
client MLME. It calls `runner.pump_client_rx(&mut mlme)` to deliver at most one
descriptor-validated raw 802.11 RX frame, and
`ethernet_tx.pump_one(&mut mlme)` to pass at most one Netstack3 Ethernet frame
through the MLME's native encapsulation path. Both calls are bounded and
preserve the single run-scoped effects owner.

Offline tests cover MLME RX delivery into the Netstack3 device contract,
outbound ARP, IPv4 (including DHCP/data), and IPv6 frames through the SoftMAC TX
facade, backpressure/retry, MTU/MAC validation, link transitions, and teardown.
An associated-link simulation additionally drives this exact adapter and a
real userspace Netstack3 `Runtime` through DHCPv4 lease/route/DNS acquisition,
Trust-DNS resolution, and a TCP HTTP request. Link loss atomically revokes the
address, routes, resolver configuration, and retained frames; link return
restarts DHCP and reacquires the lease. The existing Netstack3 offline suite
separately proves ARP/NDP, IPv4, IPv6, DNS over UDP/TCP, TCP, and UDP through
the same `EthernetDevice` shape.

The remaining physical backend interface is deliberately small. After
association it must make the existing `Mt7921ClientEffects` methods real:

1. `install_key` programs the MLME-provided key configuration into firmware and
   does not retain an extra plaintext copy;
2. `notify_association_complete` and `clear_association` publish/revoke the
   firmware station/WCID association state;
3. `send_wlan_frame` accepts the exact MLME-produced 802.11 frame and flags with
   bounded backpressure, using the installed firmware key for protected data;
4. `next_rx` yields descriptor-validated, firmware-decrypted raw 802.11 frames
   plus `WlanRxInfo`; and
5. `set_link_up`, `reset`, and `stop` preserve ordering and revocation. Link-up
   must occur only after association, key installation, and controlled-port
   opening; reset/stop must make later TX impossible.

No VFIO, firmware boot, SAE, association, or physical execution is added by
this host-side integration.

## Physical transport contract

Any physical transport must supply these exact MT7921 mechanics before this
adapter can operate hardware. `mt7921-passive-scan` implements the bounded
one-channel subset; wider channel-list operation remains gated on physical
evidence:

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
./crates/net/netstack3-port-spike/prepare-upstream
cargo test --locked --manifest-path crates/wifi/drivers/mt7921/mt7921-softmac-adapter/Cargo.toml
```

The bounded DHCP/DNS/TCP/disconnect/reconnect proof can also be run alone:

```sh
cargo test --locked --offline --manifest-path crates/wifi/drivers/mt7921/mt7921-softmac-adapter/Cargo.toml ethernet::associated_runtime_test::associated_link_acquires_dhcp_resolves_dns_transfers_tcp_and_reconnects -- --exact
```
