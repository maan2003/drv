# ath11k-bringup

Linux-only, unsafe-free staging CLI for the Redwood WCN6750 userspace driver.
It is a deliberately narrow diagnostic runner, not a production network stack.

## Safe use

Start with the fully fake-backed path. It opens no VFIO/QRTR resources and
does not read firmware. It writes a deterministic WMI command/event fixture to
the selected `--wmi-log` path:

```sh
cargo run -p ath11k-bringup -- --dry-run
```

Real mode acquires exclusive VFIO ownership first, then AF_QIPCRTR. The default
opens the supplied VFIO cdev, binds it to `/dev/iommu`, allocates and attaches
an IOAS, and uses the coherent mapping path selected for kernel #3's first
hardware run. `--broker` explicitly selects the narrowed default-domain broker
once that kernel patch exists. Use `--stop-after` to bound execution:

```sh
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --stop-after resources
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --stop-after qmi --wmi-log ath11k-wmi-run.jsonl
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --stop-after scan-results --ssid example \
  --wmi-log ath11k-wmi-scan-results.jsonl
```

Do not run real mode while `ath11k_ahb` owns the device or without the project's
supervised transactional handoff and recovery prerequisites. This CLI never
loads WPSS firmware: remoteproc/PIL and WPSS remain kernel-owned.

The Redwood defaults are the already-selected m20in `board.bin` and `regdb.bin`
at `/run/current-system/firmware/ath11k/WCN6750/hw1.0/`. Override them with
`--board` and `--regdb`. There is intentionally no `board-2.bin` parser; a
future platform selector should resolve a board entry before this CLI's asset
loading stage.

Real execution composes VFIO, QRTR/QMI, CE/HTC, WMI, and HTT and can proceed
through creation of a client vdev, a passive 2.4 GHz scan, and ordered pumping
of management RX and scan events through scan completion. `--ssid <name>`
reports the strongest matching BSS; without it, all observed BSSes are reported.
The diagnostic IE walker extracts SSID, DS/HT channel, and RSN/RSNXE presence;
the chip-neutral SoftMAC host will replace it with its canonical BSS conversion
when that runtime binds to ath11k. The default WMI
run record is `ath11k-wmi-run.jsonl`; override it with `--wmi-log`. It contains
one globally ordered JSONL stream with `seq`, `ts_ns`, `kind`, `id`, `len`, and
`bytes_hex` fields. A recorder write failure fails the run closed.
