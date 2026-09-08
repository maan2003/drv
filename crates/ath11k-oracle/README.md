# ath11k C equivalence oracle

This crate compares the Rust ath11k protocol codecs with the corresponding
code in pinned Linux commit `509ce3d952d550f93b544c8d94c99e798f09a9b4`.
It generates valid typed messages only and never requires Wi-Fi hardware.

The port does not reproduce defects in the reference C. Faithful-port means
faithful to documented hardware and protocol behaviour, not to C behaviour
that the governing documentation does not sanction. When a comparison exposes
such a behaviour difference, `MISMATCHES.md` records the minimal input and the
Rust behaviour remains the expected result; the owning crate also records the
exception in its `PORT-MAP.md`.

Build the pinned source with `nix build .#ath11k-reference-source --out-link
result-ath11k`, or set `ATH11K_REFERENCE_DIR` to its
`reference/linux-509ce3d952d550f93b544c8d94c99e798f09a9b4` directory, then run
`cargo test -p ath11k-oracle`.

The HAL differential suite uses the descriptor layouts and masks from the same
pinned `hal_desc.h`, `hal_rx.h`, `hal_rx.c`, `hal_tx.c`, and `hal.c`. It covers
the WCN6750 client-path TCL command (including the QCN9074 mesh bit), RX buffer
address setup/get, REO entrance and destination parsing, WBM release and MSDU
link setup, all three source-supported REO commands and every REO status tag, CE
source/destination/status descriptors, and the WCN6750 monitor
MPDU and PPDU-duration fields. All generated values are within the hardware
field widths. `ath11k-hal` has no descriptor `TraceSink`; where a Rust builder
is not public, the suite compares a normalized sequence of parsed fields from
the valid C-built descriptor instead.
