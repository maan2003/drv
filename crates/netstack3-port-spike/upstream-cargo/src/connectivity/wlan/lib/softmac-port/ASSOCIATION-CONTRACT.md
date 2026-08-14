# Production association-request contract

The installed validation binary uses the
`mt7921-supported-subset-v1` association-request contract. The normalized
SHA-256 `aa0306b8149896b679356f23657c7a77b49b82bfd4d485e3009831e4473437c4`
belongs only to the fully specified canonical host fixture; normalization
clears only Sequence Control bytes 22–23. Runtime hashes are input-dependent.

The production owner is `Mt7921ClientDevice::send_wlan_frame`. Every real
`ClientMlme` association request crosses this `DeviceOps` boundary before
`effects.send_wlan_frame` submits the MPDU to the physical DMA path. The
boundary applies `AssociationRequestProfile` using selected-BSS and real
ClientMlme bytes. Its HT/VHT overrides come from the firmware NIC capability
decoded by `query_from_capabilities` into the same band-specific SoftMAC query
used to construct ClientMlme. Missing overrides are a production error: the
AP-intersected ClientMlme HT/VHT bodies are evidence, not authoritative device
capabilities. The supported runtime subset has frame length 119,
capability `0x0011`, RSN capabilities `0x0080`, and IE sequence
`0:3,1:8,48:20,45:26,191:12,244:1,221:7`.

The runtime does not implement RRM, Extended Capabilities, or FILS IP Address
Assignment behavior, so it does not advertise those IEs. Regulatory IEs also
remain disabled by default. Advertising oracle bytes without the corresponding
station behavior would be incorrect.

## Offline Linux comparison oracle

`linux-6.18.40-semantic-v1` is a separate, offline comparison contract. Its
normalized target fixture SHA-256 is
`6a80b1b8631d70447f20b1be45a35564a806bc8913848d9fdb51c3404ddf4755`.
It includes the decoded native RRM body `7000000000`, Extended Capabilities
body `04000800010000400001`, and FILS IP Address Assignment extension body
`061a`. These fields are oracle evidence only and are not runtime defaults.

SSID comes from the selected BSS; supported rates and HT/VHT bodies come from
actual device capabilities; RSN suites come from the AP/SME and RSN
capabilities are re-derived from station PMF and replay-counter policy. The old
RSN capabilities `0x00cc` copied AP policy and incorrectly advertised sixteen
PTK replay counters plus MFPR and MFPC. The station implements one replay
counter, is PMF capable, and does not independently require PMF, yielding
`0x0080`. `MFPR` without `MFPC` and replay counts other than 1/2/4/16 are
rejected.

The earlier 99b validation only post-processed a FakeDevice fixture after the
real ClientMlme path had emitted its frame. Consequently the target still sent
the old 119-byte request with capability `0x0211`, RSN `0x00cc`, and
normalized SHA-256
`8646ba36fe4d09133c784f4893e759e5d2e71de415a02642e5fa2a2adde89444`.
The installed-ELF self-test now exercises the same production DeviceOps
finalizer with two distinct valid AP-intersected base HT/VHT inputs. Both must
produce the supplied device-authoritative HT/VHT bodies; the old missing-input
preserve path and the stale hash are rejected. Active logging records SHA-256
for base, authoritative, and final HT/VHT bodies plus the normalized final
frame so a target run can compare the predicted device-query fixture with the
actual DMA readback without treating every valid device as the canonical host
fixture.
