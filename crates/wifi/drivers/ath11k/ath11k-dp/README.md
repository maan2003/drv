# ath11k data-path tests

The randomized stateful property tests exercise TX, RX, DMA-pool, and REO
accounting against independent consistency checks. Defaults are intentionally
small enough for ordinary `cargo test -p ath11k-dp` runs:

- 128 generated cases;
- command sequences of 1 through 96 steps.

`ATH11K_STATEFUL_CASES` changes the case count and
`ATH11K_STATEFUL_STEPS` changes the maximum sequence length, up to 4,096.
`PROPTEST_CASES` remains a fallback for sensitivity measurements and existing
automation. When the maximum exceeds 96, generated sequence lengths are biased
toward its upper half. Commands favor repeated work on the same peer, TID, and
ring so wrap and retry behavior occurs in practical runs.

An overnight run can use:

```sh
ATH11K_STATEFUL_CASES=5000 ATH11K_STATEFUL_STEPS=4096 \
  cargo test -p ath11k-dp -- --nocapture
```

When investigating a finding, report the accounting discrepancy, the generated
case count, the minimal command sequence, and whether a sensitivity measurement
reproduces it.
