# Provenance

The files below `upstream-cargo/` are byte-for-byte copies of
`src/media/audio/lib/timeline` from Fuchsia commit
`1e1219e3fac944c9a906aea9646939746b6062b3`. They are governed by
`LICENSE.fuchsia` (BSD 2-Clause). The same repository path and unpacked archive
hash are recorded in `nix/fuchsia-reference.json`; no second source pin exists.

The Cargo manifest, host include facades, and language bridge are host packaging
written for drv and licensed under the same BSD-2-Clause package license. They
do not replace timeline arithmetic or rounding behavior.
