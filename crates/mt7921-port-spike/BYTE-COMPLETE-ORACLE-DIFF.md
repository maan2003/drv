# MT7921 byte-complete pre-key oracle diff

## Inputs and method

The Linux input is the successful 6.18.40 oracle report
`20260812T150155Z-0000_05_00.0.log` (SHA-256
`02f920fcbccf87f59ca2c54634cddf5185219e03ace517047d071f8f2d876059`),
including all 1,836 ordered `MT76_ORACLE` records. The userspace input is
E2E87 report `20260812T160353Z-0000_05_00.0.log` and source commit
`a1014fb27aac3d8463d66d1cb4fd472aa6d37d1e`.

Payload bytes were independently reconstructed from the exact Linux 6.18.40
`skb_put_zero` structures and assignments and the userspace encoders, rather
than from the earlier field normalizer. Linux 6.18.40 through 7.1 changes in
the touched constructors do not alter these payload layouts. Every reserved
byte participates in the comparison. Only MCU sequence/envelope checksum,
addresses, association AID/RCPI, and TX PID/token/sequence/IOVA/PN plus the
EAPOL payload length/body were zeroed before hashing. The temporary raw Linux
report was mode 0600 and removed after reconstruction.

[`lab/byte-complete-oracle-diff.py`](lab/byte-complete-oracle-diff.py) compares
masked byte manifests directly and reports the first differing byte and bit;
it performs no TLV or field normalization. Manifests must also contain both
complete ordered command sequences, so a selected payload subset cannot hide
a missing or extra command. `--command-sequence LINUX USERSPACE` compares
`mcu_source` records directly.

## Full payload results

Linux and userspace have the same masked SHA-256 for every row, and every
first-difference result is `none`:

| Command/state | Bytes | Masked SHA-256 |
|---|---:|---|
| DEV_INFO active | 16 | `3944445ae0cfffa1b5aaa8b80fce43830ac5fd0b9e0edf519ec11fb8c86e4ba4` |
| initial BSS_INFO basic | 36 | `13df0bf1b8e1588d780ddf827378e59c665f7636e6853fd5fcd775991a08a310` |
| preauth STA_REC/WTBL | 128 | `d89e17e60112f3387fdf5c6e7c2216b0c35ab39ad70d2b5fca0059c91aec8c4a` |
| associated BSS_INFO basic/QBSS | 48 | `4e3c6d6ed4498351e310df2872b4d4bd915bc84d14b17c90ff4d66b2fd60440f` |
| associated peer STA_REC/WTBL | 184 | `b75da6527bf7d49cff9f7f8a4bb82dfca099167373f704465c6b89a67b945b88` |
| EDCA | 44 | `1a400d3b3bda4a4fe830d6d98d09fedfa437d44c9a13fffaada560d71d274e10` |
| post-association interface WCID | 60 | `ba59c59c0aac72ea2a5b582e6a64a79122db9d7c5257ce7428ee7aa783efd351` |
| RX-path channel command | 76 | `a1f0c20673ab62f237c781f5883687182dd3723be60d796a89493a90cd32248d` |
| Cbw80 channel switch | 76 | `635ca2ed1ceaccb8c050751679f979ec7bd38baa167853abca6baf31746321ad` |
| rate/power batch 0 | 1341 | `96328f50f653b7b77e4c2fb967a50c63f8d05f7c7af6c189715e433428dd399c` |
| rate/power batch 1 | 1017 | `094ab7dac2bf0edea55fab490798bdc169b930dfb18b747f7f9a528a5c7b70cf` |
| rate/power batch 2 | 1341 | `c21aba29e5fd0fe5d71f1ce07f0e54f7c61fd66fb3ae6320a2015697e0a9abb9` |
| rate/power batch 3 | 1341 | `24bab44b0767990db4ae1894294e0ec549a1fca6e503227587c5fc3db00a7d92` |
| rate/power batch 4 | 1341 | `4f92ea40c166240d32c5719b5ffee4619732ba7eee476ef7ce066d18527f840d` |
| rate/power batch 5 | 1341 | `965e689db266a3ad17e43f17ab107278e124c96afc5d82311eab796d8a48ac97` |
| rate/power batch 6 | 1341 | `b2f853bd7b3f6580d259d63b0ad34d97a7343337ee8fa880a13bf487d00fcea0` |
| rate/power batch 7 | 1341 | `9b0d9cc77968fb5306e3008622c35a3a7d7fe43e6e22c8cd9a2780278bb013ea` |

The selected payload subset has the same relative order: DEV_INFO, initial
BSS_INFO, channel/rate/power readiness, preauth peer, associated BSS, associated
peer, EDCA, then interface WCID. The original record-only manifest did not
encode the full command streams, so it could not support the stronger claim
that no native pre-key command was absent.

## MMIO and post-association state

The common observable writes agree: Cbw80 control 36/center 42/bandwidth 2,
antenna mask 3; ring-0 BE queue publication; global WFDMA TX enabled; peer WCID
1 and interface WCID 19 WTBL update/clear-admission operations. The userspace
post-association snapshot has `RMAC_CTRL=0x000cef1a`,
`DMASHDL_SCHED0=0x76543210`, zero admission counters, and associated/awake/
QoS peer state. Those values match the Linux source write transcript and
oracle-visible state. Registers not captured on both sides are explicitly not
claimed equal.

### M1-ingress receive eligibility audit (2026-08-14)

Pinned Linux 6.18.40 and 7.1.5 retain the same receive-filter bit assignments
and preauthentication station construction. Before the first associated
STA_REC seen in the native trace, the receive-relevant order is DEV_INFO
(active local interface/MUAR), initial BSS_INFO (BSSID, connection state,
BCNFT), channel/rate/power setup, then WCID-1 preauth STA_REC/WTBL. The preauth
command has AID 0, peer address equal to the BSSID, BW 20, QoS/HT/VHT disabled,
receive-valid/RCA1/RCA2 enabled, no cipher, no key, and a closed controlled
port. Userspace emits those commands in the same relative order and the
selected command bytes match after only the documented dynamic masks. Its
association-response path snapshots this state before emitting the associated
BSS/STA/EDCA/interface-WCID tail. Linux can receive unprotected M1 in that
preauth state; pairwise key installation is not an M1 prerequisite.

`RMAC_CTRL=0x000cef1a` has `DROP_OTHER_UC` set, as Linux does. That bit drops
unicast not addressed to the programmed local interface; it is not a
drop-all-unicast bit. BMC traffic uses a different receive/WTBL path and does
not prove local-unicast matching succeeded. No Linux-named MT792x counter can
distinguish an AP that did not send M1 from a frame rejected before RX DMA:
MIB reads are consuming, and neither source version names an RMAC per-reason
filter-drop, WTBL lookup hit/miss, or PLE/PSE RX-drop counter. Contract v6 adds
only ordinary reads of peer-WTBL DW2 (`0x820d8108`) and RMAC RFCR/RFCR1
(`0x820e5000`/`0x820e5004`) to both bounded snapshots. It logs the AID and
named address/BSSID/other-unicast filter bits but retains the ambiguous-negative
classification and the independent AP/over-air witness boundary.

## First EAPOL-Key TX

After masking only frame length/body, WCID (both are now 1), PID/token,
hardware sequence, IOVA and PN, the complete 64-byte TXD/TXP masked SHA-256 is
`d9ee2483430f6af368700ecdc17b8a310169975ef1549a75b0489282d8cfcfc6` on
both sides. All remaining TXD/TXP fields are identical. The unmasked 802.11
QoS header and LLC/SNAP context are also identical: To-DS QoS data, TID 7,
unprotected, BSSID receiver, station transmitter, PAE group destination,
EtherType `0x888e`, fixed OFDM6, qidx 3, hardware sequence/FCS ownership.

## Conclusion

There is no unmasked byte or bit divergence in the selected payloads and
TXD/TXP records. A later full command-sequence audit found that userspace
omits native CE RSSI monitor `0x400a1` and UNI ROC-abort `0x20027` records.
Source audit classifies them as optional CQM policy and cleanup of Linux's
managed JOIN-ROC transaction respectively, not as proven TX-table setup.
The comparison tool now reports these omissions instead of allowing a
record-only manifest to hide them. E2E87's peer-WCID-1 probe still ended in
firmware status 1/count 15 with no TXS.


## Firmware-owned state inventory

A source-instrumented Linux 6.18.40 run and the final guarded userspace run
(`20260812T170831Z-0000_05_00.0.log`) each yielded the same 2,944 ordered
addresses. The raw reader first validates RMAC liveness, then preserves
`0xffffffff` inside only the fixed diagnostic ranges.

WTBL peer DW2 bits 0..11 at offset `0x8` are masked as the dynamic association
ID: `mt76_connac_mcu_wtbl_generic_tlv` sources `vif->cfg.aid`, and the three
userspace captures tracked AP assignments 9, 8, and 5 in exactly those bits.
After that mask, 107 dwords differed identically in all three userspace
attempts: peer WTBL 12, interface WTBL 2, WTBLON 7, PLE 6, PSE 2, DMASHDL 2,
TMAC0 52, RMAC0 12, and MIB0 12. This is a stability inventory, not a claim
that queue/rate/counter/MIB values are semantic; a single native capture
cannot distinguish those time-varying fields.

The first semantic difference is peer WTBL DW5, offset `0x14`: Linux
`0x32000427`, userspace `0x32000c23`, XOR `0x00000804`. Linux 6.18 and 7.1
`mt76_connac_mcu.h` name DW5 bits 7..5 `CHANGE_BW_RATE`, bits 8/9/10/11
`SHORT_GI_20/40/80/160`, bits 13..12 `BW_CAP`, bits 25..23/28..26 as MPDU
failure/success counters, and bits 31..29 `RATE_IDX`. Thus named fields agree
except userspace alone has `SHORT_GI_160`; XOR bit 2 is unnamed/reserved by
both source versions. The upper counters/rate index are identical and dynamic.

One source-semantic command difference was WTBL_HT `af`. Linux's
`mt76_connac_mcu_wtbl_ht_tlv` maxes the HT A-MPDU exponent 3 with the VHT
exponent 7; the encoder now does the same and the raw golden asserts byte 206
is 7. E2E88 disproved a causal link to SGI160: DW5 changed only from
`0x32000c23` to `0x32000c27`, making unnamed bit 2 match native while named
SGI160 remained set. Remaining stable differences
are retained as follow-up evidence rather than mass-fixed: many are explicit
queue/rate/counter state, while early configuration candidates include peer
WTBL offsets `0x1c`/`0x24`, interface WTBL `0x1c`/`0x24`, DMASHDL `0x4`/`0xdc`,
and TMAC configuration ranges.

The complete three-attempt-stable offset inventory (after the AID mask) is:

| Region | Stable differing offsets |
|---|---|
| peer WTBL | `14,1c,24,28,2c,30,34,6c,74,78,7c,88` |
| interface WTBL | `1c,24` |
| WTBLON | `220,224,228,22c,230,234,238` |
| PLE | `384,404,408,40c,424,10e0` |
| PSE | `1fc,200` |
| DMASHDL | `4,dc` |
| TMAC0 | `20,24,c0,c4,e4,108,140-1e0 (every dword),27c,284,374,378,384` |
| RMAC0 | `24,4c,7c,b8,bc,180,1a4,1a8,204,208,20c,210` |
| MIB0 | `48,74,400,574,594,5b8,5e0,630,638,64c,75c,780` |

Offsets are hexadecimal. Values for the first causal dword are given above;
the comparison tool emits exact values from root-only reports without copying
raw device state into the repository.


## E2E88 and SGI160 source audit

E2E88 (`20260812T172113Z-0000_05_00.0.log`) read peer DW5 immediately before
the probe gate as `0x32000c27`. Named bits were `0x0c20`, not native
`0x0420`; reserved bit 2 was one. The gate aborted before publishing a frame,
so there is no probe TXS or TX_FREE. Cleanup completed and the watchdog reboot
returned the host to stock networking.

The capability path contains no SGI160 mismatch to fix. This PCI function is
MT7961 (`0x7961`), so Linux `mt7921_register_device` does not take its MT7922-
only branch that adds `IEEE80211_VHT_CAP_SHORT_GI_160`. Linux and our local
DeviceInfo therefore advertise VHT bytes
`b2 71 90 33 fa ff 00 00 fa ff 00 00`: bit 5 SGI80 is one and bit 6 SGI160 is
zero. The AP's beacon/association VHT capability is
`b2 79 81 33 fa ff 0c 03 fa ff 0c 23`, also with SGI160 zero and a Cbw80 VHT
operation. Fuchsia's `VhtCapabilitiesInfo::intersect` combines `sgi_cbw160`
with logical AND, and `notify_association_complete` forwards those negotiated
bytes unchanged. `encode_legacy_wme_add_wcid_command` copies the same first
four bytes into `STA_REC_VHT.vht_cap`; its WTBL_VHT contains only LDPC,
dynamic-BW, VHT-present, and TXOP-PS fields and does not synthesize SGI160.
Thus neither DeviceInfo nor negotiation nor the encoder requests SGI160. The
remaining physical bit-11 difference has no source-proven host fix yet.

## Stage-local WTBL trace (2026-08-12)

One guarded native oracle capture and one guarded userspace capture traced peer
WCID 1 DW5 without changing association behavior. Native report
`20260812T175733Z-linux-oracle-0000_05_00.0.log` and userspace report
`20260812T175915Z-0000_05_00.0.log` both observed SGI160 clear before/after the
WCID-1 admission clear, clear after the preauthentication CID-3 ACK and
association BSS ACK, and set immediately after the associated peer CID-3 ACK.
It stayed set through EDCA and the interface-WCID update on both paths.

The first SGI160 divergence is therefore the immediately-pre-data boundary,
not peer creation: native changed from `0x32000c27` after interface-WCID update
to `0x32000427` before data, while userspace remained `0x32000c27`. Between
those native boundaries Linux emitted two BSS-info updates (raw locally
retained): beacon-filter timing and power-save state. Userspace emitted no
corresponding command between its interface-WCID ACK and pre-data read. This
pair isolates the missing clear to that post-interface BSS-update interval;
it does not prove which of those updates, or firmware settling around them,
is causal. No SGI capability or inventory field was changed and no speculative
fix was made.

The admission-clear register trace selected WCID 1 explicitly. Its programmed
low index was 1 on both paths, with bit 12 set for the write and busy clear in
the observed completion; direct LMAC address `0x820d8114` was used for peer
DW5. Interface WCID 19 was not selected by either peer clear.

## BSS/RLM-to-M1 normalized transcript (2026-08-15)

This audit uses the corrected native private-frame oracle
`20260814T111237Z-linux-oracle-0000_05_00.0.log` (SHA-256
`4dbcc30d32fa59398a7d5086070483583341f1f9fecbf996bf2d6dabd392d214`),
the source-instrumented raw/state oracle
`20260812T175733Z-linux-oracle-0000_05_00.0.log`, and userspace campaign-v10
attempt 1 (`20260815T020200Z-0000_05_00.0.log`, SHA-256
`49d4de075e7cde7370ea2bc11aaff7e5fc56e3987ccc3557565b281d4d1754ec`).
The corrected oracle owns frame order/timing; the older oracle owns raw command
bytes and staged WTBL state. Where its raw logger retained only the first 64
bytes, the remainder below is reconstructed from the pinned Linux 6.18.40
TLV builders and the logger's complete TLV/decoded-field transcript. Only MCU
sequence/checksum and the ROC token are normalized. WCID 1 and BSS 0 are not
normalized because both paths allocated those exact IDs.

| Boundary from last common selected channel | Native Linux | Userspace v10 | Normalized result |
|---|---|---|---|
| Scan/channel ownership | background scan completion, ROC/channel context, then channel 36/80 MHz | scan event 13, gate, channel-switch response event 237, channel 36/80 MHz | Same semantic channel; event sequence IDs dynamic. |
| DEV/BSS preauth | CID 1 DEV active; CID 3 initial peer; CID 2 BSS BASIC+QBSS; CID 2 RLM | CID 1 DEV active; CID 2 basic BSS; legacy EDCA; no initial peer/BSS-target/RLM transition | **First presence/order divergence is the absent initial CID 3 peer allocation.** Later BSS/RLM omissions remain follow-up differences and are not changed here. |
| Initial peer CID 3 | 40-byte payload, SHA-256 `2135af4e55ab272449d675701c5dd24fc460cd7ae18e4fb2dc51421b6f0b82f6`; BASIC `state=0,new=1,aid=0,qos=0`, empty reset-and-set WTBL; ACK event 1; DW5 `0x00000000` | absent | Source-exact payload is now emitted before the existing full preauth update. |
| Preauth BSS | 44 bytes, BASIC `active=1,conn_state=1,dtim=0,qos=0`, then RLM 20 bytes | earlier generic BSS only | Present/order mismatch; deliberately not changed because it follows the initial-peer divergence. |
| Full preauth CID 3 | 128-byte payload, reconstructed SHA-256 `069e6523e65fd9525c88527735447215db979ae35e4e1b44289c620777b0b999`; BASIC/PHY/RA/STATE/WTBL, PHY TLV tag `0x0015`, PHY type `0x08`, rates `0x0015/0x3fc0`; ACK event 1; DW5 `0x32000000` | 128-byte payload inside 176-byte envelope, the same tag/type `0x0015/0x08`, rates `0x0001/0x0040`; ACK event 1; DW5 `0x32000040` | Only the decoded basic/legacy rate fields differ here. The missing initial command is earlier than these decoded-field differences. |
| SAE + ROC | acquire token, grant event 39, SAE commit/anti-clogging/confirm, abort | acquire/grant event 39, same three SAE stages, abort CID 0x27 | Presence/order equivalent; token and timing dynamic. |
| Association request | acquire/grant event 39, association request, status-0 response | same; v10 response at 71,969,741 ns | Equivalent through successful association response. |
| Associated BSS CID 2 | 44 bytes, SHA-256 `1d53ec7b42d2af141587b384ea71b900b315d95c034ac26a24949a0af84256d4`; BASIC+QBSS, `conn_state=0,dtim=2,qos=1` | exact same length/hash/decoded fields; ACK event 1 | Exact after transport normalization. |
| Associated RLM CID 2 | 20 bytes, SHA-256 `4828fa8ea2e7889bd7c2d55b03af43e05c7fc5da18a3b3e651070a06f285f188`; primary 36, center 42, BW 80, center2 0 | exact same length/hash/decoded fields; ACK event 1 | Exact and ordered before the M1 pump. |
| M1 boundary | corrected oracle: BSS at 96,985,687,548,786 ns; RLM at 96,985,688,729,990 ns; M1 at 96,985,692,244,299 ns; associated STA follows at 96,985,692,264,437 ns | pump window after BSS+RLM sees no M1; associated STA/tail then timeout | Native gaps: BSS→RLM 1.181 ms, RLM→M1 3.514 ms, M1→STA 20.138 µs. Userspace preserves BSS→RLM→pump order but receives zero RX DMA completions. |

At native M1, staged state is safely source-established as peer WTBL DW5
`0x32000000`: the raw oracle reads that value after both preauth and associated
BSS ACK, and RLM does not write WTBL. RFCR/RFCR1 are source-reconstructed as
`0x000ce70a`/`0x7fc019d0`: the corrected timeline places M1 before associated
STA and the later post-association RX-filter update, while the userspace safe
read at the homologous BSS+RLM boundary reports those values. No claim is made
for counters or unnamed fields. Userspace v10 instead carries DW5
`0x32000040` from full preauth through associated BSS and RLM; bit 6 lies in
the source-named `CHANGE_BW_RATE` field. Thus the earliest source-proven
semantic divergence that persists into BSS+RLM→M1 is Linux's initial minimal
peer transition being absent before the full preauth station update. The
implementation below restores only that command and leaves later BSS/PHY/RA
and filter differences for subsequent one-cause campaigns.

### Minimal-peer CID-3 campaign result (2026-08-16)

Commit `086dc8094380f4af138addc4a2520e4ed62f2700` was exercised exactly three
times after a root-only inert proof. All attempts published and ACKed the
source-exact initial 88-byte-enveloped/40-byte-payload CID 3 command before the
existing full preauth update. The initial ACK left peer DW5 at `0x00000000` in
all three attempts. The subsequent full preauth ACK changed it to
`0x32000040`, and associated BSS plus RLM left it at `0x32000040`. Thus the
missing initial command was a real earliest presence/order divergence, but the
campaign disproves it as the cause of the persistent named bit-6 state or the
missing M1.

All three attempts reached a status-0 association response and retained exact
associated BSS payload hash
`1d53ec7b42d2af141587b384ea71b900b315d95c034ac26a24949a0af84256d4`.
Attempts two and three first received status 30 and correctly completed the
comeback retry. No attempt observed authenticator M1 or began the four-way
handshake; attempts one and three did observe unrelated client frames at RX
DMA during the five-second window. The reports are:

- `20260816T031831Z-0000_05_00.0.log`, SHA-256
  `072eb4625e6eaa5b1c02f479cccf0d4ac5af142dce257ac2108cf6b5894c50cf`
- `20260816T032043Z-0000_05_00.0.log`, SHA-256
  `4260ff9251dcf9dc6bb973b840903f7b3a6f9c5c1153e0a7896797bce70024a6`
- `20260816T032245Z-0000_05_00.0.log`, SHA-256
  `1dafe19d8ace2d572bca1723871c6313f7f777f942d19989bbaa544190156a4d`

The earliest remaining source-proven semantic divergence is therefore inside
the full preauth CID 3 update: native and userspace both use PHY TLV tag
`0x0015` and PHY type `0x08`, while native basic/legacy rates are
`0x0015/0x3fc0` and userspace emits `0x0001/0x0040`. No follow-up behavior change is included in
this campaign result.

### Negotiated preauth-rate campaign result (2026-08-16)

Commit `cae49b18a92c294e3ec51f077d6e17580fcfb7a8` was exercised exactly three
times after the packaged inert proof. The implementation parses Supported
Rates and Extended Supported Rates from the selected AP beacon, intersects
them by rate value with the selected local 5 GHz band's supported-rate list,
preserves the AP basic markers, and translates the result through the pinned
Linux/mac80211 band table. It does not derive rates from constants. All three
preauth commands decoded as PHY TLV tag `0x0015`, basic rates `0x0015`,
unchanged OFDM PHY type `0x08`, and RA legacy rates `0x3fc0`. Their
associated updates independently retained negotiated HT/VHT and decoded as
PHY type `0x38`, the same rates, HT/VHT/AMSDU present, and six WTBL TLVs.

All three attempts reached a status-0 association response (attempt two first
received status 30 and completed its comeback retry). None observed an EAPOL
frame or authenticator M1, and none began the four-way handshake. Attempts one
and three observed unrelated client frames at RX DMA; attempt two had no RX
DMA delta during the M1 window. The full preauth rate correction also left
peer WTBL DW5 at `0x32000040` before and after associated admission clear in
all three attempts. It therefore disproves the legacy-rate bitmap mismatch as
the cause of either named bit 6 or the missing M1.

The reports are:

- `20260816T035544Z-0000_05_00.0.log`, SHA-256
  `bbde49a6fae6dfbaea508bdf6b1ab8806d81a6b3674af1fa100cedc4ba3e58c3`
- `20260816T035730Z-0000_05_00.0.log`, SHA-256
  `bc130eec65c50cb670daac3dc3258e19c0d1bb2e01441c67f6fbbdc596129b93`
- `20260816T035855Z-0000_05_00.0.log`, SHA-256
  `a6d59a16ae66b9e61db4f84c3790cde42b6acb071cdc8b0bebd6769711931ca9`

After the third and final run, recovery restored `mt7921e`, PCI D0, active
iwd, an idle/non-quarantined lab, the default route, and successful HTTPS.

### Full-preauth STA_REC/WTBL and preauth-BSS campaign (2026-08-16)

The corrected source-instrumented Linux 6.18.40 oracle
`20260812T175733Z-linux-oracle-0000_05_00.0.log` establishes the complete
full-preauth CID 3 builder transcript. After normalizing MCU sequence, peer
identity and RCPI, the corrected userspace encoder is byte-equivalent: the
128-byte payload/176-byte envelope has outer tags in order BASIC
`0/20`, PHY `21/12`, RA `1/16`, STATE `7/12`, and WTBL `13/60`.
The nested reset-and-set WTBL request has operation 1 and tags in order GENERIC
`0/20`, RX `1/12`, HDR_TRANS `6/8`, and SMPS `13/8`. Decoded values
also agree: infrastructure STA, state 2/new, QoS/AID zero, basic
`0x0015`, OFDM PHY `0x08`, RA legacy `0x3fc0`, state 0, GENERIC
MUAR/skip-TX/QoS zero, RX RCA1/RCA2/RV one, header to-DS/no-RX-transform one,
and SMPS one. There is therefore no remaining host STA_REC field or nested
WTBL TLV divergence that can directly explain the firmware result.

The source symbol for WTBL DW5 bits 7..5 is
`MT_WTBL_W5_CHANGE_BW_RATE = GENMASK(7, 5)`. Bit 6 is not an independent
boolean flag: it is the middle bit of that three-bit field. At the exact
post-preauth-ACK boundary, native DW5 is `0x32000000`
(`CHANGE_BW_RATE=0`) while all corrected userspace attempts read
`0x32000040` (`CHANGE_BW_RATE=2`). Neither pinned mt76 version exposes a
host command field named CHANGE_BW_RATE. The full commands, including
`WTBL_SMPS.smps=1`, are identical, so firmware derives the physical WTBL
field from command ordering/context rather than copying an unequal STA_REC
byte. No direct WTBL write or speculative clear was added.

The first source-proven transcript difference before that identical full
STA_REC was Linux's preauth CID-2 BSS BASIC+QBSS update. Its exact 44-byte
payload is active infrastructure-station connection state 1, target BSSID,
BMC/STA WCID 19, beacon interval 100, DTIM 0, 5-GHz local PHY mode `0xb1`,
non-HT-basic PHY `0x0078`, followed by an eight-byte QBSS TLV with QoS
disabled. Commit `f5fa4237c009dc9611e309f7a26cae5385fbc16b` adds only that
ACKed command between the already-correct initial peer command and full
preauth STA_REC. Exact payload and lifecycle ordering/rollback tests cover the
new transition.

The new root-only safe read records peer WTBL DW0..DW9 after every relevant
ACK. All three guarded attempts produced the identical post-preauth vector:

`38409356 7dc7a672 42000000 00000000 10000000 32000040 ffffffff 00000000 00000000 00000000`

DW0/DW1 carry the selected peer address in hardware layout; DW2 is the
preassociation generic control word (AID zero); DW3 is zero; DW4 is
`0x10000000`; DW5 decodes as CHANGE_BW_RATE 2, all four named SGI bits zero,
BW_CAP 0, MPDU fail/OK counters 4/4 and rate index 1; DW6 reads the documented
all-ones diagnostic value; DW7..DW9 are zero. The older native oracle safely
read DW5 at this same post-preauth boundary as `0x32000000`; it did not
publish privacy-sensitive DW0/DW1 or the other dwords, so no unsupported
native values are inferred.

The preauth-BSS campaign was run exactly three times, with no fourth. The BSS
command did not alter the resulting vector or DW5 field. Attempts one and
three reached status-0 association (attempt three after status-30 comeback);
attempt two completed preauth but failed during SAE/MLME before association.
No attempt observed EAPOL/authenticator M1 or began the four-way handshake.
The reports are:

- `20260816T042907Z-0000_05_00.0.log`, SHA-256
  `1c132925d8d23382e5786df88205fd336e0b81991092aa066ed370db4c589eb6`
- `20260816T043045Z-0000_05_00.0.log`, SHA-256
  `9589cc7efb36eb6523d01a8bc59cd6bcd9855214a28a8e3cf46755be860dd916`
- `20260816T043446Z-0000_05_00.0.log`, SHA-256
  `887ccc65833fc1a325a9429f89f4dad700d9a6f427d70e2c93a12e6965c4d0fb`

The preauth BSS difference is therefore corrected and excluded. The earliest
remaining ordered native/userspace difference is now the immediately
following preauth CID-2 RLM command, which Linux ACKs before the full
preauth STA_REC and userspace still omits at that boundary. No RLM behavior
change is included in this campaign. Final recovery restored `mt7921e`, PCI
D0, active iwd, an idle/non-quarantined lab, inactive/non-failed watchdog,
default route, and HTTPS.

### Preauth RLM campaign result (2026-08-16)

The remaining native preauth CID-2 RLM transition is now emitted after the
preauth BSS ACK and before the full preauth CID-3 STA_REC. Its exact 20-byte
payload is
`0000000002001000242a00020203010401010000` (SHA-256
`4828fa8ea2e7889bd7c2d55b03af43e05c7fc5da18a3b3e651070a06f285f188`),
identical to the corrected Linux oracle and the already-proven associated RLM
payload. Every field comes from the authorized active channel definition:
BSS 0, primary 36, center 42, center2 0, 80-MHz bandwidth code 2 and 5-GHz
band code 1; the pinned encoder supplies two TX streams, three RX streams,
short slot 1, HT operation-info byte 4 and secondary-channel offset 1 for a
primary channel below center. The success transcript is exactly CID order
`3,2,2,3` with consecutive MCU sequences for initial peer, preauth BSS,
preauth RLM and full preauth STA_REC. An injected ambiguous RLM failure proves
that the full STA_REC is not submitted and cleanup reserves later sequences
for WCID removal and BSS disable, releases WCID 1 only after both ACKs, and
otherwise retains firmware-uncertain state for teardown.

Commit `1b4e7445dfc9c11e6a5117d2abec3737853e6544` and its pinned artifact were
exercised exactly three times after package, root-entry, remote-entry,
delivery-manifest, remote-entry-test, inert-proof-root-entry-test and remote
`--plan` validation. There was no fourth active invocation. In every report
the ACK order was directly visible as `after_initial_peer_cid3_ack`,
`after_preauth_bss_ack`, `after_rlm_ack`, then the 176-byte
`e2e81_sta_rec_transcript` and `after_preauth_cid3_ack`. The first three ACKs
left the WTBL vector at

`00000000 00000000 00000000 00000000 00000000 00000000 ffffffff 00000000 00000000 00000000`

and the identical full STA_REC then produced, in all three runs,

`38409356 7dc7a672 42000000 00000000 10000000 32000040 ffffffff 00000000 00000000 00000000`.

Thus preauth RLM does not change DW5 on this firmware. The named
`CHANGE_BW_RATE` field remains 0 through its ACK and becomes 2 only at the
full preauth CID-3 ACK, versus the native oracle's 0 at that boundary. This
campaign corrects and excludes the last known missing preauth CID-2 command;
the earliest remaining state divergence is now the firmware result of the
byte-equivalent full preauth CID-3 update. It must depend on still-unidentified
prior firmware/global context or an unobserved native transition, not on an
unequal known preauth BSS, RLM, STA_REC or nested WTBL byte. No speculative
physical WTBL write or fourth campaign run was made.

Attempt one reached a status-0 association response with normalized AID 9,
completed the associated BSS/RLM/full-STA_REC/tail sequence, and changed DW5
to `0x32000827` (`CHANGE_BW_RATE=1`, SGI160 set) at the associated CID-3 ACK.
It saw three non-EAPOL client frames attributed to interface WCID 19 rather
than peer WCID 1, then timed out without authenticator M1. Attempt two sent the
association request but received no admitted association response and ended
with pinned SME/MLME connect failure. Attempt three reached status-0
association with normalized AID 8, completed the same associated sequence and
same `0x32000827` DW5 result, but saw no RX DMA activity during the M1 window.
No attempt observed EAPOL/authenticator M1 or began the four-way handshake.
The durable reports are:

- `20260816T050151Z-0000_05_00.0.log`, SHA-256
  `d07542373c3762bbf03bc57e74d9e01b1fc1f04d8a79d4973d2242dcb35cc244`
- `20260816T050308Z-0000_05_00.0.log`, SHA-256
  `a6a128b2715500a914a366c6a9c104e117379786e009ff9ee215d577a46fccda`
- `20260816T050417Z-0000_05_00.0.log`, SHA-256
  `2e1eda48d3cb4f4b92197d422513e534a13fb0fee1850b125e595e596a5fa8e5`

Each report ended with `RESTORE end failed=0`. Final recovery checks found an
idle, non-quarantined, native-ready lab; `mt7921e` bound in PCI D0; active iwd;
inactive/non-failed watchdog; a WLAN default route; and successful HTTPS.

## Chronological hidden firmware-context audit: post-CLC FWLOG (2026-08-16)

The corrected native oracle report `/tmp/native-150155.log` (SHA-256
`02f920fbc26aa54179f2d29b5d1952e4cced87621d1136c68c6dc3a6bf194387`)
contains 1,836 `MT76_ORACLE` records. Reading it chronologically, rather than
starting at preauthentication, proves the following boundary:

- the installed firmware/patch bootstrap and scatter progression reaches the
  same firmware-start, NIC-capability, EEPROM/EFUSE and first CLC operations;
  the command stream is byte/order-equal through userspace MCU sequence 13;
- native then emits CE `FWLOG_2_HOST` (`cmd=0x400c5`, payload length 4,
  no response), in a 68-byte envelope at MCU sequence 14. The Linux
  `mt7921_mcu_fw_log_2_host(dev, 1)` source supplies payload `01 00 00 00`;
- userspace formerly omitted that persistent firmware setting and made
  channel-domain its sequence 14. This is the earliest source-proven
  persistent divergence in the audited interval.

The implementation now encodes that exact 68-byte no-response command and
orders it after the first CLC response and before channel-domain. Its encoder,
golden-order and no-response classification are tested, and the source map
records the Linux ownership. The production-prefix safety gate was advanced
from 14 to 15 after the campaign exposed that mechanical expectation as stale;
the corrected package builds successfully. Nothing here establishes that
firmware logging changes peer WTBL state or M1 delivery, and in particular
nothing makes `CHANGE_BW_RATE` causal.

Later native commands were deliberately not folded into this change. The
corrected oracle next emits EEPROM buffer mode (`0x21ed`), protection
(`0x3eed`), a second CLC (`0x4005c`), then channel-domain (`0x4000f`), eight
rate-power pages (`0x4005d`), TX-power feature control (`0x400ca`), MAC enable
(`0x46ed`), another domain update, RX-path configuration (`0x4eed`) and a
second eight-page rate batch. The corresponding later DEV/MUAR, BSS,
MAC/PHY/RX, channel/RLM, RX-filter, scan, ROC, power and offload commands
remain later audit territory. The next earliest ordered difference is
therefore already source-proven: native EEPROM buffer mode immediately
follows FWLOG, whereas current userspace prematurely sends channel-domain and
has not yet reproduced native protection plus second-CLC-before-domain order.

### Exactly-three guarded campaign outcome

The pinned pre-gate-correction artifact was invoked exactly three times; no
fourth active invocation was made. All three reports directly prove successful
publication of sequence 14:

`{"firmware_bootstrap_transcript":"firmware_log_to_host","cid":"0x400c5","sequence":14,"bytes":68,"payload_raw":"01000000","wait_response":false}`

followed by
`{"firmware_bootstrap_event":"firmware_log_to_host_tx_complete","sequence":14}`.
Each then published channel-domain as sequence 15 and stopped at the
fail-closed production-prefix guard, whose old expectation was 14. Thus these
runs prove exact FWLOG transport but do **not** reach initial peer, WTBL
snapshots, association disposition, `e2e81_sta_rec_transcript`, or the M1
window; there is no WTBL/M1 result to infer from them. The durable reports are:

- `20260816T053518Z-0000_05_00.0.log`, SHA-256
  `5900f62eaa85c29e750e0b0543fe9c7e414f44f196414cbf5a9e3a183e4694a1`
- `20260816T053845Z-0000_05_00.0.log`, SHA-256
  `3dc45d8683ca88ceef489cd2e9fe21094f4804318f579cde22f0fd4e6d92ca0f`
- `20260816T053954Z-0000_05_00.0.log`, SHA-256
  `c1cd142cee573f46390d992edcf10e70e73c5fc25c19707cd0d19974ac807595`

Every report ends with `RESTORE end failed=0`. Final authoritative recovery
checks returned idle status 0, quarantined status 1 (not quarantined), and
native-ready status 0. `mt7921e` is rebound in PCI D0, iwd is active, the
watchdog is inactive/non-failed, the WLAN default route is present, and HTTPS
succeeds. The corrected, locally build-validated artifact was not subjected to
another hardware invocation because that would violate the three-run ceiling.
