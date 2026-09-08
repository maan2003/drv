# ath11k-bringup

Linux-only, unsafe-free staging CLI for the Redwood WCN6750 userspace driver.
It is a deliberately narrow diagnostic runner, not a production network stack.

## Bring-up workflow and build locations

Optimize for the next useful real-device result: a small fix, targeted checks,
and a bounded run. Redwood uses no advisors. Production hardening and exhaustive
oracle coverage are not prerequisites for each experiment; IOMMU/DMA containment
and the recovery contract in
[ARCH-redwood-wifi-target](../../specs/ARCH-redwood-wifi-target.md) still apply.

Build experimental kernels on **np over SSH with plain `make`, outside Nix**.
NixOS system configuration lives in `~/src/nixos` (locally
`/home/maan2003/src/nixos`), separate from that kernel build workflow. Coordinate
np access with the MT7921/substrate owner. Kernel/DTB experiments are kexec-only:
no flashing, partition/slot changes, or encryption-key changes.

### Run B starting-kernel precondition

The 32-byte region-1 DT has only booted through a two-hop sequence. A direct
flashed `7.2.0 #1` to forced-legacy candidate hop is **not validated** and most
recently failed to return USB; do not retry it. This is a validation gap, not a
proved kernel root cause. The observed sequence used these exact inputs:

- `stage3/Image`, SHA-256
  `97efd9fa53e252512dcf5f8572a06db150b31a79c1f9dddfb3934bb0d9c9b885`
- `stage7/initrd-watchdog`, SHA-256
  `654f1c6ffbf8baa85dad2bcf24a58db70f7133e32c8280a9d88c22edcb01652f`
- `stage10/runB-region1.fdt`, SHA-256
  `8a5d019f7c258b654dffa180215f5d17cb5d95d04561a59a470d27cf33707b67`
- `stage7/kexec-transaction/command-line`, exactly 1540 bytes.

From unlocked, running flashed `#1`, hop first in auto mode:

```sh
set -e
test "$(systemctl is-system-running)" = running
test ! -e /run/redwood-lab-watchdog/armed
kexec -l /var/lib/ath11k-redwood-lab/stage3/Image \
  --initrd=/var/lib/ath11k-redwood-lab/stage7/initrd-watchdog \
  --dtb=/var/lib/ath11k-redwood-lab/stage10/runB-region1.fdt \
  --command-line="$(cat /var/lib/ath11k-redwood-lab/stage7/kexec-transaction/command-line)"
sync
kexec -e
```

The one successful observation reached `7.2.0+ #9` but ignored the DTB
argument. A later attempt from a `degraded`, rather than `running`, flashed
userspace reset back to flashed `#1`; no retained evidence distinguishes a
reset before candidate entry from a candidate failure before USB. Do not treat
the first hop as reproducible or proceed from a degraded system. A diagnostic
retry from `running` userspace also failed to return USB and required a manual
power-cycle; its exact inputs and successful `kexec_loaded` transition do not
show whether the candidate kernel began executing. Require a 16-byte Wi-Fi
`reg` and live-FDT SHA-256
`d97685d12ed5033abeeec478e9ed5a409e327a86f0384305de0d275062815f35`.
After the approved stdin-only unlock and return to `#9` userspace, make the
second hop with the legacy syscall forced:

```sh
set -e
test ! -e /run/redwood-lab-watchdog/armed
kexec -u || true
kexec -c -l /var/lib/ath11k-redwood-lab/stage3/Image \
  --initrd=/var/lib/ath11k-redwood-lab/stage7/initrd-watchdog \
  --dtb=/var/lib/ath11k-redwood-lab/stage10/runB-region1.fdt \
  --command-line="$(cat /var/lib/ath11k-redwood-lab/stage7/kexec-transaction/command-line)"
sync
kexec -e
```

Before unlocking the second hop, require `#9`, a 146776-byte live FDT, and the
exact 32-byte Wi-Fi `reg` ending in `61 e0 00 00 ... 00 20 00 00`. The staged
candidate proof is on np at
`/var/lib/poco-linux/redwood/work/artifacts/runB-dtb-20260908T221230Z`.
The first hop's ignored DTB and the successful second hop are observed facts;
neither hop accessed VFIO or QMI. Do not infer that another starting kernel or
memory context is safe.

## Safe use

Verify USB control before a stateful run. The complete experiment must have a
deadline and automatic recovery for runner failure, timeout, or session loss;
Wi-Fi restoration is not required to collect logs or recover USB access. Manual
power-cycle is an accepted exception for a genuine kernel hang, not ordinary
runner failure. Use the smallest applicable recovery procedure, not a mandated
number of timers. The existing rebind-only
`scripts/redwood/redwood-wifi-transaction` is not valid unchanged for the
no-reset VFIO experiment; its fake restore test does not model device ownership.
The updated automatic recovery procedure must be established before that run.

`scripts/redwood/redwood-wifi-transaction` supplies that procedure for the
no-reset VFIO run. Launch it as the detached runner unit; it installs a local
deadline reboot before starting the command, applies a process timeout even
while the runner's independent heartbeat is advancing, and forces reboot on
both success and failure. Keep the initrd watchdog armed throughout. For
example, after setup has recorded `RUN` and discovered `CDEV`:

```sh
systemd-run --unit=redwood-ath11k-runB --collect \
  /var/lib/ath11k-redwood-lab/stage10/redwood-wifi-transaction --run \
  /bin/sh -c 'exec "$1" --vfio-device "$2" \
    --containment remoteproc:remoteproc2 --register-region 1 --stop-after qmi \
    --wmi-log "$3/runB.wmi.jsonl" >"$3/runB.log" 2>&1' \
  sh /var/lib/ath11k-redwood-lab/stage10/ath11k-bringup-runB "$CDEV" "$RUN"
```

Do not append `redwood-lab-watchdog stop`: this no-reset transaction ends in
automatic reboot, and reboot is the cleanup that releases VFIO/WPSS state.
Run the host-only recovery check with
`scripts/redwood/redwood-wifi-transaction --self-test`.

Start with the fully fake-backed path. It opens no VFIO/QRTR resources and
does not read firmware. It writes a deterministic WMI command/event fixture to
the selected `--wmi-log` path:

```sh
cargo run -p ath11k-bringup -- --dry-run
```

Real mode acquires exclusive VFIO ownership first, then AF_QIPCRTR. The default
opens the supplied VFIO cdev, binds it to `/dev/iommu`, allocates and attaches
an IOAS, and uses the coherent mapping path selected for kernel #3's first
hardware run. `--broker` explicitly selects the experimental DMA-broker backend
and requires compatible kernel support; the admitted coherent polling path does
not need it. Use `--stop-after` to bound execution:

Before any staged run, arm the initrd userspace watchdog, then execute the inert
fail-closed host preflight. It checks the VFIO cdev/sysfs identity, the 32
edge-rising SPI descriptions, `/dev/iommu`, and the watchdog's armed marker. It
does not open the VFIO cdev, bind iommufd, or access device registers:

```sh
cargo run -p ath11k-bringup -- preflight \
  --vfio-device /dev/vfio/devices/vfioN
```

The real runner updates `/run/redwood-lab-watchdog/heartbeat` from an
independent thread and leaves the watchdog armed when it exits. This heartbeat
does not establish runner progress: a deadline must still terminate a stalled
experiment. Runner success alone must not stop the watchdog; the experiment
controller completes cleanup/recovery before disarming protection. For the
current no-reset experiment, prefer automatic reboot to the known-good system
over an unvalidated rebind. Do not require a manual watchdog-stop step for
ordinary recovery.

The resources stage performs the necessarily state-changing iommufd bind under
the armed watchdog, then fails closed unless VFIO reports the WCN6750 platform
flags and 32 single-vector edge-triggered eventfd IRQs. It records every
enumerated region's index, flags, size, and offset without opening region 0:
on the stock DT that zero-length resource is the GIC doorbell address, not the
hybrid-bus register aperture. Successful bind is the stock-kernel
cache-coherency admission check. Redwood's
vfio-platform device has no reset handler, so that target additionally requires
`--containment remoteproc:<sysfs-name>`; the stage verifies the named
remoteproc is `running`, records its name/state/firmware, and records that
VFIO reset is unavailable. Before DMA, the supervisor/operator must externally
restart WPSS remoteproc; the runner only reads its resulting state and firmware
identity and never stops or starts remoteproc. This polling-only run stops on
failure, with the armed watchdog rebooting the phone if the run or external
restart fails, rather than fabricating a device reset.

The first physical `qmi` run is deliberately BAR-discovery-only. It waits for
the QRTR server, completes indication registration, host capability, target
capability and QMI DeviceInfo in pinned source order, then prints `bar_addr`
and `bar_size`. It stops before the fixed-memory BDF download. If no enumerated
VFIO region has that exact size, it exits successfully before MMIO, CE, HTC,
or WMI. A later
DT-assisted run must expose the QMI-selected 2 MiB aperture as a distinct VFIO
region and select it explicitly with `--register-region <index>`; DeviceInfo
then opens that region at the exact reported size. Region 0 is never a fallback.

```sh
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --containment remoteproc:remoteprocN \
  --stop-after resources
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --containment remoteproc:remoteprocN \
  --stop-after qmi --wmi-log ath11k-wmi-run.jsonl
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --containment remoteproc:remoteprocN \
  --stop-after scan-results --ssid example \
  --wmi-log ath11k-wmi-scan-results.jsonl
cargo run -p ath11k-bringup -- --vfio-device /dev/vfio/devices/vfioN \
  --containment remoteproc:remoteprocN \
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
