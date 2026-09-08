# Stateful suite sensitivity check

Each measurement restored one historical bookkeeping defect, ran the owning
randomized suite with `PROPTEST_CASES=5000`, and then abandoned the scratch
change. The source and test suites were restored before this report was made.

| Defect | Mutation applied | Suite | Detected | Cases to first failure | Minimal sequence length |
|---|---|---|---:|---:|---:|
| RX refill prepare (`846b7b215a1b`) | Removed `entry.buffer.prepare_for_device()?` before dropping the completed RX buffer | `ath11k-dp` host/service | yes | 1 | 2 |
| dma-pool return (`af20e71d8082`) | Removed the `!self.reusable` guard from `DmaSegment::drop` | `ath11k-dp` host/service | no | — (5,000 passed) | — |
| RX cookie wrap (`e2a789c437a3`) | Restored blind masked-cursor allocation and wrap in `replenish_pool` | `ath11k-dp` host/service | no | — (5,000 passed) | — |
| TX completion source (`7f0b919af404`) | Treated every release source other than firmware/HTT as a TQM completion | `ath11k-dp` host/service | no | — (5,000 passed) | — |
| REO/TID same-key retained queue (`e1f52f10c403`) | Removed the setup gate over `uncertain_setup` and `failed_delete` | `ath11k-dp` REO status | no | — (5,000 passed) | — |

The detected RX refill mutation failed with zero preceding successful cases and
this two-command minimal sequence:

```text
RxCompletion { owner: 0, shape: 0, push_reason: 0, length: 0 }
Service { work: 1, receive: 1 }
```

The host/service command was run as:

```sh
PROPTEST_CASES=5000 cargo test -p ath11k-dp tx::tests::stateful_tests::command_sequences_keep_buffer_lifecycle_consistent -- --exact --nocapture
```

The REO command was run as:

```sh
PROPTEST_CASES=5000 cargo test -p ath11k-dp reo::tests::stateful_tests::reo_command_sequences_keep_owner_accounting_consistent -- --exact --nocapture
```

## Miss diagnoses and recommendations

**dma-pool return.** The host/service generator only drops completed RX
segments through the production path, and that path successfully prepares the
segment first. It does not inject a failed prepare or directly drop a
CPU-owned segment, so removing the pool's reusable-state guard stays dormant.
Add a generated prepare failure followed by drop and allocation, and check that
the returned address is not the discarded address.

**RX cookie wrap.** A sequence has at most 64 commands and starts the cookie
cursor at 1, far short of the 18-bit wrap point. The live-cookie distinctness
check is useful once a collision occurs, but the generator cannot reach one.
Add a test seam that starts the cursor at the last cookie, or generate a small
model cookie space, while retaining the same allocation rule.

**TX completion source.** The generator covers release sources 0 through 7,
but its checks derive callback counts from what the implementation did. They
check that completed IDs are not still live, not that an unsupported source
must leave its owner live and produce no callback. Track expected live TX IDs
in the model and update them only for explicitly supported sources.

**REO/TID same-key retained queue.** The accounting check sums all ownership
lists and limits duplicates only inside the active `tids` list. A second active
descriptor alongside the same key in `failed_delete` or `uncertain_setup`
therefore still satisfies the total. Check key uniqueness across every list
that may remain device-visible, and add a WMI outcome that exercises uncertain
setup as well as failed invalidation status.
