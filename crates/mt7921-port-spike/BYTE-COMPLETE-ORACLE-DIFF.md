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
it performs no TLV or field normalization.

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

The report order also agrees byte-command-for-byte-command: DEV_INFO, initial
BSS_INFO, channel/rate/power readiness, preauth peer, associated BSS, associated
peer, EDCA, then interface WCID. No extra Linux pre-key command is absent from
the userspace readiness path.

## MMIO and post-association state

The common observable writes agree: Cbw80 control 36/center 42/bandwidth 2,
antenna mask 3; ring-0 BE queue publication; global WFDMA TX enabled; peer WCID
1 and interface WCID 19 WTBL update/clear-admission operations. The userspace
post-association snapshot has `RMAC_CTRL=0x000cef1a`,
`DMASHDL_SCHED0=0x76543210`, zero admission counters, and associated/awake/
QoS peer state. Those values match the Linux source write transcript and
oracle-visible state. Registers not captured on both sides are explicitly not
claimed equal.

## First EAPOL-Key TX

After masking only frame length/body, WCID (both are now 1), PID/token,
hardware sequence, IOVA and PN, the complete 64-byte TXD/TXP masked SHA-256 is
`d9ee2483430f6af368700ecdc17b8a310169975ef1549a75b0489282d8cfcfc6` on
both sides. All remaining TXD/TXP fields are identical. The unmasked 802.11
QoS header and LLC/SNAP context are also identical: To-DS QoS data, TID 7,
unprotected, BSSID receiver, station transmitter, PAE group destination,
EtherType `0x888e`, fixed OFDM6, qidx 3, hardware sequence/FCS ownership.

## Conclusion

There is no unmasked byte, bit, command-order, MMIO, register-state, TXD/TXP,
802.11-header, or LLC divergence available to fix. E2E87's peer-WCID-1 probe
still ended in firmware status 1/count 15 with no TXS. Host-visible internal
evidence is exhausted; the next useful discriminator requires external RF or
firmware visibility. No E2E88 is justified.


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

The causal command-construction difference is WTBL_HT `af`, not an actual
negotiated SGI-160 capability. Linux's `mt76_connac_mcu_wtbl_ht_tlv` starts
from `sta->ht_cap.ampdu_factor`, obtains the VHT maximum A-MPDU exponent with
`FIELD_GET(IEEE80211_VHT_CAP_MAX_A_MPDU_LENGTH_EXPONENT_MASK, ...)`, and stores
the maximum. Our encoder sent HT exponent 3 despite the negotiated VHT
exponent 7. The firmware consequently produced the differing DW5 state. The
encoder now performs the same max before emitting WTBL_HT; the independent
raw command golden asserts `af=7` at byte 206. Remaining stable differences
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
