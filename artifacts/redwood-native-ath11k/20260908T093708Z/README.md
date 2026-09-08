# Redwood native ath11k WMI transcript

Captured on the POCO X5 Pro (`redwood`) from Linux 7.2.0+ with
`CONFIG_ATH11K_TRACING=y`. Tracing began after loading the ath11k core and
before loading ath11k AHB, so it covers WCN6750 firmware boot and cold
calibration, followed by an iwd connection to the lab AP and a passive scan.

`wmi/ordered.jsonl` contains 1,897 length-checked records in trace order: 411
commands and 1,486 events. Each record has `seq`, `ts_ns`, `kind`, `id`, `len`,
and the complete dynamic tracepoint byte array as lowercase `bytes_hex`.

The raw `native-trace.dat` and 7.9 MB `trace-cmd report -R` output remain on
no-plastic at
`/var/lib/poco-linux/redwood/work/artifacts/redwood-native-ath11k-20260908T093708Z/native-transcript/`.
Their SHA-256 values are respectively
`7a1f694bd884ec8b19ab16612b6264c9d410594084392fcc06f5c8f395a70b5f`
and
`f77c259606886005f48381a5aabf21f147995785c4e69156eef91e1229ccf352`.
