# AMD HDA spike provenance

The tree below `upstream-fuchsia/` is a byte-for-byte extraction from Fuchsia
commit `1e1219e3fac944c9a906aea9646939746b6062b3`, the repository pin in
`nix/fuchsia-reference.json`. It is licensed under the BSD 2-Clause license in
`LICENSE.fuchsia` and is intentionally not compiled or patched.

| Pristine upstream path | Local path |
| --- | --- |
| `src/media/audio/drivers/intel-hda/controller` | `upstream-fuchsia/src/media/audio/drivers/intel-hda/controller` |
| `src/media/audio/drivers/intel-hda/codecs/realtek` | `upstream-fuchsia/src/media/audio/drivers/intel-hda/codecs/realtek` |
| `src/media/audio/drivers/lib/intel-hda` | `upstream-fuchsia/src/media/audio/drivers/lib/intel-hda` |

`SOURCE-ITEMS.tsv` records a SHA-256 digest for every extracted file. Host Rust
code lives outside `upstream-fuchsia/`; Linux-derived AMD behavior is called
out separately in `SOURCE-MAP.md` rather than being represented as Fuchsia
code.
