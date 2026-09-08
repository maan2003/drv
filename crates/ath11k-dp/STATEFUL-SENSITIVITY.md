# Stateful suite sensitivity measurement

Each measurement restored one historical accounting defect in a scratch change,
ran the owning randomized stateful property test with `PROPTEST_CASES=5000`, and
then abandoned the scratch change. The source and test suites were restored
before this report was updated. “Cases to first finding” counts the failing case,
so a run reporting zero preceding successes has a value of 1.

| Defect | Scratch restoration | Suite | Detected | Cases to first finding | Minimal sequence length | Runtime |
|---|---|---|---:|---:|---:|---:|
| RX refill prepare (`846b7b215a1b`) | Removed `entry.buffer.prepare_for_device()?` before dropping the completed RX buffer | `ath11k-dp` host/service | yes | 1 | 2 | 1.76 s |
| dma-pool return (`af20e71d8082`) | Removed the `!self.reusable` check from `DmaSegment::drop` | `ath11k-dp` host/service | yes | 3 | 1 | 2.27 s |
| RX cookie wrap (`e2a789c437a3`) | Restored blind masked-cursor allocation and wrap in `replenish_pool` | `ath11k-dp` host/service | yes | 1 | 5 | 2.12 s |
| TX completion source (`7f0b919af404`) | Treated unsupported release sources as TQM completions | `ath11k-dp` host/service | yes | 1 | 3 | 1.82 s |
| REO/TID same-key retained queue (`e1f52f10c403`) | Removed the setup check over uncertain setup and failed delete owners | `ath11k-dp` REO status | yes | 1 | 2 | 1.64 s |

The strengthened suites detected all 5 of 5 restored defects. The minimal
sequences exercised these consistency checks:

- RX completion followed by service found the missing device-ownership prepare.
- A generated failed prepare, drop, and replacement allocation found reuse of
  the discarded DMA address.
- Two RX replacement cycles crossed the test cursor's 18-bit boundary and found
  a duplicate live cookie.
- Submit, unsupported-source completion, and service found both the unexpected
  callback and the missing live TX owner.
- Uncertain WMI setup followed by successful same-key setup found two possibly
  device-visible owners. The generator also covers failed REO invalidation and
  a subsequent same-key setup.

The host/service command was run as:

```sh
PROPTEST_CASES=5000 cargo test -p ath11k-dp tx::tests::stateful_tests::command_sequences_keep_buffer_lifecycle_consistent -- --exact --nocapture
```

The REO command was run as:

```sh
PROPTEST_CASES=5000 cargo test -p ath11k-dp reo::tests::stateful_tests::reo_command_sequences_keep_owner_accounting_consistent -- --exact --nocapture
```

The timings include recompilation of each scratch defect restoration. Because a
finding stops and shrinks the run, these are finding-and-shrink runtimes rather
than runtimes for 5,000 successful cases.

## Changes that increased sensitivity

**dma-pool return.** A generated command now performs a failed prepare followed
by drop and replacement allocation. Its consistency check requires the new DMA
address to differ from the discarded address.

**RX cookie wrap.** The host/service setup moves the production cookie cursor to
the last 18-bit value while ordinary live cookies remain allocated. Generated
RX replacement work reaches wrap without changing the production allocation
rule.

**TX completion source.** The command model tracks expected live TX IDs
independently. Only supported completion sources update that accounting;
unsupported sources require no callback and leave their expected owner live.

**REO/TID same-key owner.** The test registers its peer before issuing commands,
generates successful, known-non-visible-failure, and uncertain WMI outcomes,
and generates failed invalidations. Its consistency check requires key
uniqueness across active, uncertain-setup, and failed-delete owners that may
remain device-visible.

Both suites accept `ATH11K_STATEFUL_CASES` and `ATH11K_STATEFUL_STEPS` for long
runs, with `PROPTEST_CASES` retained for this sensitivity measurement. The data
path README documents an overnight configuration.

## Resolved long-run finding

A two-case, 4,096-step run found a REO same-key accounting discrepancy: a new
setup could publish an active owner while an older delete was still pending,
then retain both owners if the older invalidation failed. Change `df3ae4b4`,
merged by `af333a7a`, rejects same-key setup while deletion is pending. The
documented two-case × 4,096-step run now passes, including the exact REO target;
the generator remains unconstrained so the repaired sequence stays covered.

Reproduce with:

```sh
ATH11K_STATEFUL_CASES=2 ATH11K_STATEFUL_STEPS=4096 \
  cargo test -p ath11k-dp stateful_tests -- --nocapture
```
