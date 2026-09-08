# Source and provenance map

| Adapter responsibility | Source ownership | License/disposition |
| --- | --- | --- |
| NIC capabilities and physical candidate channels | `mt7921-port-spike::{NicCapability,CandidateChannel,candidate_channels}`, adapted from pinned Linux mt76/MT7921 | Consumed by path; combined adapter crate is GPL-2.0-only |
| SoftMAC values, hardware trait, scan events | `fuchsia-softmac-port`, Fuchsia `1e1219e3fac944c9a906aea9646939746b6062b3` | Consumed by path; upstream remains BSD-2-Clause |
| Beacon/probe conversion | pinned Fuchsia MLME `client/convert_beacon.rs::construct_bss_description` | Called directly through `fuchsia-softmac-port`; no local parser; BSD-2-Clause source remains at the pin |
| Production client device effect seam | pinned Fuchsia MLME `device.rs::DeviceOps` | Implemented directly by `client_device::Mt7921ClientDevice`; retained operations are mechanical and offline-tested; live beacon+power authorization is explicitly unimplemented |
| WPA3 association and RSN orchestration | pinned `fuchsia-softmac-port::{OpenClientMlme,SaeHandshake}` over Fuchsia client MLME/RSN | Called directly by `one_shot`; local code only sequences bounded, generation-scoped effects |
| Authorized DHCP/DNS/TCP/HTTP proof | pinned `netstack3-port-integration::{DhcpService,Runtime}` and `netstack3-port-spike::{EthernetRunner,RemoteSocketProvider}` | Called directly by `ethernet::BoundedNetstackProof`; no local network protocol implementation |
| SoftMAC lifecycle and data-path shape | pinned Fuchsia iwlwifi `platform/wlansoftmac-device.cc`: Start/Stop 107-130, QueueTx 132-187, SetChannel 213-235, passive scan/cancel 411-452, RX/scan completion 467-504; tests in `test/wlan-softmac-device-test.cc`: Start/RX/teardown 311-413, Stop fixture teardown 443-460, SetChannel 812-873, passive scan 1149-1175, QueueTx 1407-1469 | Used as the structural model for the corresponding device boundary and its direct causal tests; no iwlwifi firmware or hardware mechanics are copied |
| Stop ordering | pinned Fuchsia `wlansoftmac/rust_driver/src/lib.rs`: MLME-before-dependent-server shutdown contract 203-215 and ordering loop 304-375; ordering test 1167-1220 | Lifecycle revocation is write-ahead and sticky until the backend is reconstructed |
| MT7921 transport trait and adapter state machine | Project-local integration in this crate | GPL-2.0-only |
| Scripted transport and failure injection | `src/lib.rs` unit-test module | Test-only, in-memory, no I/O; GPL-2.0-only |

Pinned Fuchsia closure provenance and license are retained in
`../netstack3-port-spike/upstream-cargo/{PROVENANCE.md,LICENSE.fuchsia}`. The
MT7921 port's Linux source inventory and per-item dispositions remain in
`../mt7921-port-spike/{SOURCE-MAP.md,SOURCE-ITEMS.tsv}`.
