# Source map

Fuchsia pin: `1e1219e3fac944c9a906aea9646939746b6062b3`.

| Packaged behavior | Pinned source | Host disposition | License |
| --- | --- | --- | --- |
| SoftMAC/MLME/channel/capability values | `sdk/fidl/fuchsia.wlan.{ieee80211,mlme,softmac}/**` | Uses the existing path-shaped host schema crates directly | Fuchsia BSD-2-Clause |
| Passive scan validation, offload request construction, scan identity/completion handling | `src/connectivity/wlan/lib/mlme/rust/src/client/scanner.rs` | Synchronous extraction over `SoftmacHardware`; IEEE Time Unit conversion, state transitions, error outcomes, and request fields retained | Fuchsia BSD-2-Clause |
| Passive channel candidate intersection | `src/connectivity/wlan/lib/sme/src/client/scan.rs` (`get_primary_channels_for_scan`, `CANDIDATE_PRIMARY_CHANNELS`) | `allowed_passive_channels`; fixed world/indoor input, hardware intersection, no AP Country-IE expansion | Fuchsia BSD-2-Clause |
| Beacon/probe conversion | `src/connectivity/wlan/lib/mlme/rust/src/client/convert_beacon.rs` | Exact pinned module included by path and `construct_bss_description` re-exported; upstream unit fixtures retained | Fuchsia BSD-2-Clause |
| Open authentication validation | `src/connectivity/wlan/lib/mlme/rust/src/auth.rs` | Exact pinned module compiled by path; open request and AP-response validation used directly | Fuchsia BSD-2-Clause |
| SME-managed SAE and EAPOL state/crypto/timers | `src/connectivity/wlan/lib/{sme/src/client/{protection,rsn,state},rsn/src/**,fcg-crypto/src/**}` | `SaeHandshake` is a narrow wrapper around the unchanged pinned `wlan-rsn::Supplicant`; it converts SAE/EAPOL updates plus PMK/PTK/GTK/IGTK into non-printable borrow-only zeroizing handoffs, while hardware programming remains excluded | Fuchsia BSD-2-Clause |
| SAE authentication frame construction | `src/connectivity/wlan/lib/mlme/rust/src/{akm_algorithm.rs,client/bound.rs}` | `build_sae_auth_frame` retains the pinned SME `SaeFrame` fields and exact `BoundClient::send_auth_frame` management layout | Fuchsia BSD-2-Clause |
| Client connect states, timer, failures, and SME event | `src/connectivity/wlan/lib/mlme/rust/src/client/state.rs` (`Joined`, `Authenticating`, `Associating`, `States::{start_connecting,on_mgmt_frame,on_timed_event}`) | Synchronous open-network extraction plus direct post-SAE entry into protected association; state transitions, single beacon-relative connect timeout, status outcomes, and `ConnectConf` retained; EAPOL and post-connect maintenance remain excluded | Fuchsia BSD-2-Clause |
| Authentication/association frame construction and RX filtering | `src/connectivity/wlan/lib/mlme/rust/src/client/{bound,station}.rs` | Uses pinned frame writer, management helpers, sequence manager, MAC parser, address filter, SSID/rates/HT/VHT IE layout, and listen interval | Fuchsia BSD-2-Clause |
| Association response parsing and device configuration | `src/connectivity/wlan/lib/mlme/rust/src/client/{mod,state}.rs` (`ParsedAssociateResp`, `Associating::on_assoc_resp_frame`) | Retains AP/client capability intersection, HT/VHT operation parsing, typed association config, controlled-port open, and success/failure events | Fuchsia BSD-2-Clause |
| Hardware operation boundary | `src/connectivity/wlan/lib/mlme/rust/src/device.rs` | Scan edge plus a separate client edge narrowed to management-frame transport, association complete/clear, and Ethernet-up notification; no radio implementation | Fuchsia BSD-2-Clause |
| Fake adapter and host fixture plumbing | Local test boundary only | Records typed scan/connect requests and emits deterministic queued observations/frames; cannot perform I/O | Fuchsia BSD-2-Clause |

The source headers in `src/lib.rs` preserve Fuchsia authorship and license.
The canonical license and full shared-overlay provenance are retained in
`../../../../../{LICENSE.fuchsia,PROVENANCE.md}`.
