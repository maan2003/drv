# ath11k WMI maintenance hand-off

The wire reference is Linux commit
`509ce3d952d550f93b544c8d94c99e798f09a9b4`. `PORT-MAP.md` is the coverage
inventory. This crate owns checked WMI envelopes, typed command encoders, and
event decoders; host policy and request values belong in `ath11k-core` and the
SoftMAC adapter.

## Sources of truth and verification gates

- Command encoders live in `src/cmd/`; `src/cmd/golden.rs` reverse-maps the
  checked-in native transcript and applies the documented host-state masks.
- Event decoders and lifecycle buffering live in `src/event/`.
- `src/cmd/comparison.rs` implements phase-aware comparison and
  `src/bin/compare-wmi.rs` is its CLI. The native input is
  `artifacts/redwood-native-ath11k/20260908T093708Z/wmi/ordered.jsonl`.
- `tests/golden_transcript.rs` gates native command-family and event coverage;
  `tests/connect_roundtrip.rs` gates semantic reverse-map round trips.
- `ath11k-oracle` runs generated C differentials against the pinned source.
  Run `cargo test -p ath11k-wmi`, `cargo test -p ath11k-wmi --features
  proptest`, and `cargo test -p ath11k-oracle` after wire changes.

Compare a phone trace from the repository root with:

```sh
cargo run -p ath11k-wmi --bin compare-wmi -- \
  artifacts/redwood-native-ath11k/20260908T093708Z/wmi/ordered.jsonl \
  /path/to/phone.wmi.jsonl
```

## Accepted differences

Only host-owned bytes are masked: INIT host-memory addresses, management-TX
DMA address and downloaded frame, and peer reorder-queue DMA address. A run
stopped before association legitimately lacks the native connect tail; the
diagnostic runner also uses a synthetic MAC and passive rather than native
active-scan fields. These are review context, not blanket approval of missing,
extra, reordered, or mismatched records. `crates/ath11k-oracle/MISMATCHES.md` records
accepted source divergences; it currently records no WMI divergence.

The scan-channel-list array-length quirk in `golden.rs` is an exact reproduction
of the observable pinned ABI, not an accepted mismatch.

## Open items

- Compare the first non-empty real runner transcript. The first phone attempt
  failed during QMI `Device::probe()` and produced an empty WMI file, so no
  `SERVICE_READY`, `READY`, command, or wire comparison was available.
- Complete generated-C differential coverage for `PeerAssoc`,
  `VdevInstallKey`, and `MgmtSend`; do not replace it with hand-written packing.

The cross-layer conformance purpose is documented in
`specs/ARCH-wlan-stack-topology.md`; Redwood capture, containment, and kexec
safety constraints live in `specs/ARCH-redwood-wifi-target.md`.
