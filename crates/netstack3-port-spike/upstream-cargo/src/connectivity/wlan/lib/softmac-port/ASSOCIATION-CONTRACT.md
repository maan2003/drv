# Linux-equivalent association request contract

`AssociationRequestProfile` is the source of truth for association IEs that are
not already owned by `ClientCapabilities` or the selected BSS. The contract is
`linux-6.18.40-semantic-v1`; its normalized target fixture SHA-256 is
`6a80b1b8631d70447f20b1be45a35564a806bc8913848d9fdb51c3404ddf4755`.
Normalization clears only Sequence Control bytes 22–23.

The decoded non-secret native authority is
`mt7921-port-spike/lab/fixtures/association-native.log`. SSID comes from the
selected BSS; supported rates and HT/VHT bodies come from actual device
capabilities; Power Capability and Supported Channels come from the immutable
regdb snapshot; RSN suites come from the AP/SME and RSN capabilities are
re-derived from station PMF and replay-counter policy; RRM, Extended
Capabilities, and Extension IEs are supplied only by implemented station
features; H2E RSNXE remains selected-BSS evidence; WMM remains ClientMlme-owned.

The old RSN capabilities `0x00cc` copied AP policy: it advertised sixteen PTK
replay counters plus MFPR and MFPC. The station implements one replay counter,
is PMF capable, and does not independently require peers to use PMF, producing
`0x0080`. `MFPR` without `MFPC` and replay counts other than 1/2/4/16 are
rejected.

The native RRM body `7000000000`, Extended Capabilities body
`04000800010000400001`, and FILS IP Address Assignment extension body `061a`
are retained as an oracle comparison profile but are not runtime defaults.
They must be omitted until their corresponding action/state behavior exists;
copying the bytes would over-advertise station behavior. The generic profile
therefore defaults to omitting them and regulatory IEs. The target oracle
fixture opts into all decoded fields solely to verify byte-for-byte parity.
