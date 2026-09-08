# ath11k-bringup

Linux-only, unsafe-free staging CLI for the Redwood WCN6750 userspace driver.
It is a deliberately narrow diagnostic runner, not a production network stack.

## Safe use

Start with the fully fake-backed path. It opens no VFIO/QRTR resources and
does not read firmware or create the WMI log:

```sh
cargo run -p ath11k-bringup -- --dry-run
```

Real mode acquires exclusive VFIO ownership first, then AF_QIPCRTR. The default
is the non-coherent DMA broker required for unproven Redwood coherency.
`--coherent` is an explicit opt-in and must only be used after coherency has
been proved. Use `--stop-after` to bound execution:

```sh
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --stop-after resources
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --stop-after qmi --wmi-log ath11k-wmi-run.jsonl
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
through creation of a client vdev and a passive 2.4 GHz scan. The default WMI
run record is `ath11k-wmi-run.jsonl`; override it with `--wmi-log`. It contains
one globally ordered JSONL stream with `seq`, `ts_ns`, `kind`, `id`, `len`, and
`bytes_hex` fields. A recorder write failure fails the run closed.
