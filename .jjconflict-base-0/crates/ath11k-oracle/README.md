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
