# ath11k-ce

The crate's randomized stateful property testing exercises HTC credit accounting,
endpoint sequencing, frame consistency checks, and DMA buffer ownership across
generated command sequences.

The normal test run uses 128 cases with at most 64 commands per case. These
defaults are intentionally fast. `ATH11K_STATEFUL_CASES` changes the case count,
and `ATH11K_STATEFUL_STEPS` changes the maximum sequence length. Sequence length
is limited to 4,096 commands. The generator favors longer sequences,
repeated operations, and repeated traffic for the same endpoint so extended
runs spend more time in accumulated state.

For an overnight run:

```sh
ATH11K_STATEFUL_CASES=5000 ATH11K_STATEFUL_STEPS=4096 \
  cargo test -p ath11k-ce \
  tests::stateful_tests::htc_and_dma_command_sequences_keep_accounting_consistent \
  -- --exact --nocapture
```

When investigating a finding, keep the saved proptest regression seed and
rerun the exact test first. Record the command settings and minimized sequence
with any discrepancy. Comparing how quickly known accounting defects are found
at different settings is a useful sensitivity measurement; case count alone
does not describe sequence depth.
