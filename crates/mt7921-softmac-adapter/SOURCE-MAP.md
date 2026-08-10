# Source and provenance map

| Adapter responsibility | Source ownership | License/disposition |
| --- | --- | --- |
| NIC capabilities and physical candidate channels | `mt7921-port-spike::{NicCapability,CandidateChannel,candidate_channels}`, adapted from pinned Linux mt76/MT7921 | Consumed by path; combined adapter crate is GPL-2.0-only |
| SoftMAC values, hardware trait, scan events | `fuchsia-softmac-port`, Fuchsia `1e1219e3fac944c9a906aea9646939746b6062b3` | Consumed by path; upstream remains BSD-2-Clause |
| Beacon/probe conversion | pinned Fuchsia MLME `client/convert_beacon.rs::construct_bss_description` | Called directly through `fuchsia-softmac-port`; no local parser; BSD-2-Clause source remains at the pin |
| Production client device effect seam | pinned Fuchsia MLME `device.rs::DeviceOps` | Implemented directly by `client_device::Mt7921ClientDevice`; retained operations are mechanical and offline-tested; live beacon+power authorization is explicitly unimplemented |
| MT7921 transport trait and adapter state machine | Project-local integration in this crate | GPL-2.0-only |
| Scripted transport and failure injection | `src/lib.rs` unit-test module | Test-only, in-memory, no I/O; GPL-2.0-only |

Pinned Fuchsia closure provenance and license are retained in
`../netstack3-port-spike/upstream-cargo/{PROVENANCE.md,LICENSE.fuchsia}`. The
MT7921 port's Linux source inventory and per-item dispositions remain in
`../mt7921-port-spike/{SOURCE-MAP.md,SOURCE-ITEMS.tsv}`.
