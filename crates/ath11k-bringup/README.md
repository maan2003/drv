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

Before any staged run, execute the inert fail-closed host preflight. It checks
the VFIO cdev/sysfs identity, the 32 edge-rising SPI descriptions, `/dev/iommu`,
and the Qualcomm watchdog device, driver, and live-FDT status. It does not open
the VFIO or watchdog cdev, bind iommufd, or access device registers:

```sh
cargo run -p ath11k-bringup -- preflight \
  --vfio-device /dev/vfio/devices/vfioN
```

The resources stage performs the necessarily state-changing iommufd bind under
the armed watchdog, then fails closed unless VFIO reports the WCN6750 platform
flags, one region, and 32 single-vector edge-triggered eventfd IRQs. Successful
bind is the stock-kernel cache-coherency admission check. Redwood's
vfio-platform device has no reset handler, so that target additionally requires
`--containment remoteproc:<sysfs-name>`; the stage verifies the named
remoteproc is `running`, records its name/state/firmware, and records that
VFIO reset is unavailable. This polling-only run stops on failure and relies on
WPSS remoteproc restart plus the watchdog reboot, not a fabricated device
reset. The runner does not stop or start remoteproc.

```sh
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --stop-after resources
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --stop-after qmi --wmi-log ath11k-wmi-run.jsonl
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --stop-after scan-results --ssid example \
  --wmi-log ath11k-wmi-scan-results.jsonl
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --stop-after dp-poll --wmi-log ath11k-wmi-dp-poll.jsonl
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
of management RX and scan events through scan completion. The final diagnostic
`dp-poll` stage performs one bounded polling-first DP service pass, submits no
frames, and prints counts plus any delivered RX frames and TX completions for
capture by the operator stage log. `--ssid <name>`
reports the strongest matching BSS; without it, all observed BSSes are reported.
The diagnostic IE walker extracts SSID, DS/HT channel, and RSN/RSNXE presence;
the chip-neutral SoftMAC host will replace it with its canonical BSS conversion
when that runtime binds to ath11k. The default WMI
run record is `ath11k-wmi-run.jsonl`; override it with `--wmi-log`. It contains
one globally ordered JSONL stream with `seq`, `ts_ns`, `kind`, `id`, `len`, and
`bytes_hex` fields. A recorder write failure fails the run closed.

## Comparing a hardware capture

Compare a run against the pinned native ath11k transcript from the repository
root:

```sh
cargo run -p ath11k-wmi --bin compare-wmi -- \
  artifacts/redwood-native-ath11k/20260908T093708Z/wmi/ordered.jsonl \
  ath11k-wmi-scan-results.jsonl
```

The report is split into `boot-through-service-ready`, `vdev-create-start`,
`scan`, and `connect` phases. Each phase first summarizes commands as `exact`,
`masked`, `mismatched`, `missing`, `extra`, or `reordered`; the indented rows
give the WMI ID, both sequence numbers, the zero-based offset of the first
differing byte when applicable, and any masked host-owned fields. The following event summary
reports expected/seen/aligned totals and lists missing, extra, or reordered
event IDs.

For a run intentionally stopped after scanning, missing `connect` records are
expected. Differences caused by the runner's synthetic MAC address and by its
passive scan request versus the native capture's active-scan fields are also
expected; assess other mismatches against the phase and WMI ID that reports
them.
