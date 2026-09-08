# Production association-request contract

The installed validation binary uses `mt7921-supported-subset-v2`. For the
fixed no-plastic inputs its MPDU is 175 bytes, capability is `0x0111`, and IE
order is:

`0:3,1:8,33:2,36:50,48:20,45:26,191:12,244:1,221:7`

Clearing only Sequence Control bytes 22–23 gives SHA-256
`5449fa5acf5317259694bb400a04d6ba8e169f99cf555583a424b3530f8a63c4`.
Runtime hashes remain input-dependent outside that fully specified input set.

`Mt7921ClientDevice::send_wlan_frame` is the production owner. Every real
ClientMlme association request crosses that literal `DeviceOps` boundary before
physical DMA. `AssociationRequestProfile` is built from the firmware NIC
capability decoded into the band-specific SoftMAC query plus the same immutable
pinned-regdb snapshot that authorizes rate/power programming. Missing HT/VHT or
regulatory inputs fail closed. The association builder and associated STA_REC
therefore consume the same corrected SoftMAC capabilities; STA_REC tests reject
the former HT/VHT values.

## Exact native comparison

The Linux 6.18.40 native request (204 bytes, normalized SHA-256
`6a80b1b8631d70447f20b1be45a35564a806bc8913848d9fdb51c3404ddf4755`)
and the last active DMA request (119 bytes, normalized SHA-256
`aa0306b8149896b679356f23657c7a77b49b82bfd4d485e3009831e4473437c4`)
differ as follows:

| Field | Native Linux | Former DMA | Source-backed v2 |
|---|---|---|---|
| Capability | `0x1111` | `0x0011` | `0x0111` |
| HT capability word | `0x09ff` | `0x09f3` | `0x09ff` |
| HT remaining 24 bytes | identical | identical | identical |
| VHT capability word | `0x338071b2` | `0x339071b2` | `0x338071b2` |
| VHT RX/TX MCS maps | `fa ff` / `fa ff` | identical | identical |
| VHT RX highest | `00 00` | identical | identical |
| VHT TX highest | `00 20` | `00 00` | `00 20` |
| Power Capability | `00 14` | absent | `00 14` |
| Supported Channels | 28 ordered `(channel,1)` pairs | absent | 25 enabled pairs |
| RRM / Extended / extension | present | absent | absent |

Pinned mt76 initializes the device capabilities, then mac80211 narrows them at
association time: `ieee80211_add_ht_ie` writes disabled SMPS while runtime SMPS
is off, and `ieee80211_add_vht_ie` removes MU beamformee when the AP lacks the
matching MU-beamformer support. `mt76_init_stream_cap` sets
`IEEE80211_VHT_EXT_NSS_BW_CAPABLE` after mt792x advertises the corresponding
hardware flag. The firmware PHY TLV supplies the two-stream/device-mode inputs.
No capture byte is used as a runtime source.

Linux's power-capability construction uses minimum 0 and the current channel's
regulatory maximum. V2 obtains maximum 20 dBm for channel 36 from the verified
world-domain regdb snapshot. Supported Channels is derived, in snapshot/query
order, from enabled primary channels and uses Linux's exact one-channel range
encoding: 36–64, 100–144, and 149–165 (25 pairs). Channels 169, 173, and 177
remain in the physical device query but are disabled by the pinned regdb, so
the honest supported subset omits them rather than copying the native capture.

The station does not implement RRM, Extended Capabilities, or FILS IP Address
Assignment, so v2 does not set the RRM bit and emits no IE70, IE127, or IE255.
RSN suites remain AP/SME-owned; station PMF/replay policy yields RSN
capabilities `0x0080`. RSNXE remains selected-BSS H2E evidence and WMM remains
ClientMlme-owned.

## Separate exact-204 oracle

`linux-6.18.40-semantic-v1` remains a comparison oracle, not a production
profile. Its RRM body `7000000000`, Extended Capabilities body
`04000800010000400001`, and FILS extension body `061a` are preserved for exact
native comparison only. Production must not advertise them until their
userspace behavior exists.

The earlier stale request with capability `0x0211`, RSN `0x00cc`, and hash
`8646ba36fe4d09133c784f4893e759e5d2e71de415a02642e5fa2a2adde89444`,
and the now-stale 119-byte `aa0306…` request are both rejected by the installed
self-test.
