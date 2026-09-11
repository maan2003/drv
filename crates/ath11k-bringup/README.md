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

### Known-good Redwood operation

The validated route to the 32-byte region-1 DT is two orderly kexec hops. Run
the phone-side blocks directly in a root shell; do not wrap them in another
remote `sh -c`. A reachable recovery initrd may be the original flashed image,
whose 120-second boot deadline is still active. Before collecting evidence or
unlocking, immediately stop and runtime-mask both watchdog units, then verify
that neither can run:

```sh
systemctl stop boot-watchdog.service redwood-lab-watchdog.service || true
systemctl mask --runtime boot-watchdog.service redwood-lab-watchdog.service
test "$(systemctl show -P LoadState boot-watchdog.service)" = masked
test "$(systemctl show -P ActiveState boot-watchdog.service)" = inactive
test "$(systemctl show -P LoadState redwood-lab-watchdog.service)" = masked
test "$(systemctl show -P ActiveState redwood-lab-watchdog.service)" = inactive
test ! -e /run/redwood-lab-watchdog/armed
```

Do this on every reachable recovery-initrd boot, before any slower inspection;
do not race the deadline by unlocking first. This recovery action does not
replace the command-line mask gates below for either candidate hop. Then
identify the recovered flashed kernel:

```sh
cat /proc/version
test "$(wc -c </proc/device-tree/soc@0/wifi@17a10040/reg)" = 16
test ! -e /run/redwood-lab-watchdog/armed
```

For an operator-attended manual-recovery run, do not use the canonical staged
command line unchanged. The staged watchdog initrd enables
`boot-watchdog.service`, whose 120-second initrd deadline kexecs the rescue
kernel unless switch-root has completed, and it also enables the normally
disarmed `redwood-lab-watchdog.service`. Create and manifest a distinct,
single-line command-line input which appends both initrd-only masks:

```sh
rd.systemd.mask=boot-watchdog.service \
rd.systemd.mask=redwood-lab-watchdog.service
```

Use that exact input for **both** hops. Merely observing no armed lab-watchdog
marker does not disable the boot watchdog.

The first line must identify flashed `7.2.0 #1`. Unlock only by streaming the
protected key from np to the phone process's stdin. Resolve the phone's trusted
`systemd-cryptsetup` path first, substitute it for `SYSTEMD_CRYPTSETUP`, and do
not enable shell tracing, log or `tee` the pipe, or put key bytes or the np key
path in the phone command line:

```sh
ssh PHONE 'command -v systemd-cryptsetup'
ssh PHONE \
  'systemctl stop systemd-cryptsetup@redwood\\x2droot.service; \
   systemctl reset-failed systemd-cryptsetup@redwood\\x2droot.service || true; \
   udevadm settle --timeout=10; sleep 2; \
   test ! -e /dev/mapper/redwood-root'
ssh NP 'exec sudo flock -n /run/lock/drv-hardware.lock \
  cat "$PROTECTED_KEY"' |
  ssh PHONE 'exec SYSTEMD_CRYPTSETUP attach redwood-root /dev/sda33 /dev/stdin'
ssh PHONE \
  'test -e /dev/mapper/redwood-root; touch /run/host-ack; \
   systemctl default'
```

This stop/attach/reset/start/default sequence is the unlock; repeat all of it
after each hop. A bare attach can race the generated unit and does not switch
to the real root. Wait until `test "$(systemctl is-system-running)" = running`
succeeds on flashed `#1`. Then load hop 1 through
`kexec_file_load` with **no explicit DTB**, then let PID 1 shut down userspace:

```sh
LAB=/var/lib/ath11k-redwood-lab
test "$(sha256sum "$LAB/stage3/Image" | cut -d' ' -f1)" = \
  97efd9fa53e252512dcf5f8572a06db150b31a79c1f9dddfb3934bb0d9c9b885
test "$(grep -ao -m1 -E '#[0-9]+ SMP PREEMPT [^[:cntrl:]]+2026' \
  "$LAB/stage3/Image")" = '#9 SMP PREEMPT Tue Sep  8 15:07:10 IST 2026'
test "$(sha256sum "$LAB/stage7/initrd-watchdog" | cut -d' ' -f1)" = \
  654f1c6ffbf8baa85dad2bcf24a58db70f7133e32c8280a9d88c22edcb01652f
test "$(sha256sum "$LAB/stage12/kexec-command-line-manual-no-reboot" | cut -d' ' -f1)" = \
  74ef9ba7e5abfc6131f595a3db921fdad63fac8e1ea73764b437e01af1115212
kexec -u || true
kexec -s -l "$LAB/stage3/Image" \
  --initrd="$LAB/stage7/initrd-watchdog" \
  --command-line="$(cat "$LAB/stage12/kexec-command-line-manual-no-reboot")"
sync
systemd-inhibit --what=sleep --mode=block --why='Redwood orderly kexec hop 1' \
  systemctl kexec
```

Before unlocking, verify the masks in the initrd manager and keep the initrd
reachable past the former 120-second deadline:

```sh
grep -qw 'rd.systemd.mask=boot-watchdog.service' /proc/cmdline
grep -qw 'rd.systemd.mask=redwood-lab-watchdog.service' /proc/cmdline
test "$(systemctl show -P LoadState boot-watchdog.service)" = masked
test "$(systemctl show -P ActiveState boot-watchdog.service)" = inactive
test "$(systemctl show -P LoadState redwood-lab-watchdog.service)" = masked
test "$(systemctl show -P ActiveState redwood-lab-watchdog.service)" = inactive
test ! -e /run/redwood-lab-watchdog/armed
sleep 125
test "$(systemctl show -P LoadState boot-watchdog.service)" = masked
```

After stdin-only unlock returns, require `7.2.0+ #9` and a 16-byte Wi-Fi
`reg`. A whole-blob live-FDT digest is not by itself a topology identity:
`/chosen/bootargs` contains the required manual-recovery masks and
bootloader-supplied values can change across cold boots. Decode the live FDT
and the manifested candidate with `dtc`; require their complete topology to
be identical except for `/chosen/bootargs` and the expected Wi-Fi `reg`
widening below. Validate the command line separately by its staged SHA-256 and
require both watchdog-mask tokens. Then force the legacy loader for hop 2,
this time passing the candidate DTB, and again use the orderly PID-1 path:

```sh
LAB=/var/lib/ath11k-redwood-lab
cat /proc/version
systemctl reset-failed unl0kr-agent.path unl0kr-agent.service \
  unl0kr.service unl0kr-stop.service nftables.service 2>/dev/null || true
test "$(systemctl is-system-running)" = running
test -z "$(systemctl --failed --no-legend --plain --no-pager | \
  awk '$1 ~ /\.(service|path)$/ { print $1 }')"
test "$(wc -c </proc/device-tree/soc@0/wifi@17a10040/reg)" = 16
test "$(sha256sum "$LAB/stage3/Image" | cut -d' ' -f1)" = \
  97efd9fa53e252512dcf5f8572a06db150b31a79c1f9dddfb3934bb0d9c9b885
test "$(grep -ao -m1 -E '#[0-9]+ SMP PREEMPT [^[:cntrl:]]+2026' \
  "$LAB/stage3/Image")" = '#9 SMP PREEMPT Tue Sep  8 15:07:10 IST 2026'
test "$(sha256sum "$LAB/stage7/initrd-watchdog" | cut -d' ' -f1)" = \
  654f1c6ffbf8baa85dad2bcf24a58db70f7133e32c8280a9d88c22edcb01652f
test "$(sha256sum "$LAB/stage10/runB-region1.fdt" | cut -d' ' -f1)" = \
  8a5d019f7c258b654dffa180215f5d17cb5d95d04561a59a470d27cf33707b67
test "$(sha256sum "$LAB/stage12/kexec-command-line-manual-no-reboot" | cut -d' ' -f1)" = \
  74ef9ba7e5abfc6131f595a3db921fdad63fac8e1ea73764b437e01af1115212
kexec -u || true
kexec -c -l "$LAB/stage3/Image" \
  --initrd="$LAB/stage7/initrd-watchdog" \
  --dtb="$LAB/stage10/runB-region1.fdt" \
  --command-line="$(cat "$LAB/stage12/kexec-command-line-manual-no-reboot")"
sync
systemd-inhibit --what=sleep --mode=block --why='Redwood orderly kexec hop 2' \
  systemctl kexec
```

Repeat the initrd mask checks and 125-second dwell before unlocking once more,
then require the last-tested candidate state explicitly:

```sh
cat /proc/version                         # must identify 7.2.0+ #9
grep -qw 'rd.systemd.mask=boot-watchdog.service' /proc/cmdline
grep -qw 'rd.systemd.mask=redwood-lab-watchdog.service' /proc/cmdline
test ! -e /run/redwood-lab-watchdog/armed
systemctl reset-failed unl0kr-agent.path unl0kr-agent.service \
  unl0kr.service unl0kr-stop.service nftables.service 2>/dev/null || true
test "$(systemctl is-system-running)" = running
test -z "$(systemctl --failed --no-legend --plain --no-pager | \
  awk '$1 ~ /\.(service|path)$/ { print $1 }')"
test "$(wc -c </proc/device-tree/soc@0/wifi@17a10040/reg)" = 32
od -An -tx1 -v /proc/device-tree/soc@0/wifi@17a10040/reg
test "$(wc -c </sys/firmware/fdt)" = 146860
test "$(sha256sum /sys/firmware/fdt | cut -d' ' -f1)" = \
  23245887d50adaaa02586778567444ad24c111d6314ce0782324aa1949671b4f
```

The `reg` dump must end `61 e0 00 00 ... 00 20 00 00`. The candidate
live-FDT hash and size identify the last-tested hop-2 artifact; a hop-1
whole-blob hash is provenance rather than a topology gate. Before either hop,
stage the checked-in payload under
its executable name, then generate and verify a per-stage SHA-256 manifest with
absolute staged paths. The payload verifies this exact file and copies it into
the durable run directory before device mutation:

```sh
# From the repository checkout on np:
scp scripts/redwood/redwood-runB-core-once PHONE:/tmp/redwood-runB-core-once

# In the root phone shell:
LAB=/var/lib/ath11k-redwood-lab
install -m 0755 /tmp/redwood-runB-core-once \
  "$LAB/stage12/redwood-runB-core-once"
rm /tmp/redwood-runB-core-once
sha256sum \
  "$LAB/stage12/redwood-runB-core-once" \
  "$LAB/stage10/ath11k-bringup-runB" \
  "$LAB/stage3/Image" \
  "$LAB/stage7/initrd-watchdog" \
  "$LAB/stage10/runB-region1.fdt" \
  "$LAB/stage7/kexec-transaction/command-line" \
  "$LAB/stage12/kexec-command-line-manual-no-reboot" \
  "$LAB/stage7/modules/vfio-platform-base.ko" \
  "$LAB/stage7/modules/vfio-platform.ko" \
  "$LAB/stage10/redwood-wifi-transaction" \
  "$LAB/watchdog-acceptance/redwood-lab-watchdog" \
  >"$LAB/stage12/SHA256SUMS"
sha256sum -c "$LAB/stage12/SHA256SUMS"
```

Archive the manifest with the run. Do not silently reuse it after replacing an
input. The last physically tested Image, initrd, command line, and staged DTB
digests begin `97efd9fa`, `654f1c6f`, `578c`, and `8a5d019f`, respectively;
the full values belong in that validated manifest.

The transaction payload's source of truth is
[`scripts/redwood/redwood-runB-core-once`](../../scripts/redwood/redwood-runB-core-once).
Persist that exact executable on the phone and pass it as the wrapper's sole
payload argument--never inline it, detach a temporary heredoc, or add a nested
remote `sh -c`:

```sh
systemd-run --unit=redwood-ath11k-runB --collect \
  /var/lib/ath11k-redwood-lab/stage10/redwood-wifi-transaction --run \
  /var/lib/ath11k-redwood-lab/stage12/redwood-runB-core-once
```

The wrapper's self-test checks that boundary and the payload rejects positional
arguments. The wrapper installs a deadline, inhibits sleep, and forces reboot
on timeout or any payload exit. The payload validates the manifest and
candidate live FDT before mutation, asks kernel remoteproc/PIL to load and start
WPSS, waits for `remoteproc2` to be `running`, loads and binds VFIO only
afterward, arms and preflights the watchdog, and runs through `core` with region
1. Do not append a watchdog stop: forced reboot is the no-reset VFIO recovery
path. Use `REDWOOD_*` overrides only when their values and replacement inputs
are captured in the run's manifest.

After recovery and stdin-only unlock, retrieve the durable payload logs, the
live pstore view, and systemd's archive before another run (run these on np):

```sh
OUT=artifacts/runB-recovered-$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p "$OUT"
scp -rp PHONE:/var/lib/ath11k-redwood-lab/runs/runB-core-region1-orderly \
  "$OUT/run"
scp -rp PHONE:/sys/fs/pstore "$OUT/live-pstore"
scp -rp PHONE:/var/lib/systemd/pstore "$OUT/archived-pstore"
```

### Fast repeated Run B failures

After one validated two-hop candidate boot, stage
`scripts/redwood/redwood-runB-fast-cycle` and include its absolute destination
in `stage12/SHA256SUMS`. Invoke it as the sole executable of a transient unit.
It retains all VFIO mappings while `qcom_q6v5_pas` stops WPSS and verifies
`remoteproc2` is `offline`; only then may the failed child return, the cdev be
unbound, and WPSS be started for a fresh process. An independent systemd reboot
deadline and the hardware watchdog remain armed throughout. If remoteproc stop
or verification fails, the runner deliberately holds its VFIO owners until that
fallback reboots the phone.

While those fallbacks are armed, the wrapper records a synced flight-recorder
line every two seconds with the runner PID/heartbeat timestamp, remoteproc
state, USB carrier, and watchdog control-peer reachability. It also syncs the
runner and WMI logs. This is diagnostic evidence only: it does not renew,
disarm, or replace either recovery deadline.

Set `REDWOOD_MANUAL_RECOVERY=1` only for an operator-attended diagnostic where
manual reboot is explicitly accepted. That mode verifies no lab watchdog is
armed, starts no automatic reboot deadline, and passes `--manual-recovery` to
the runner and its preflight. The runner still stops WPSS before releasing any
VFIO mapping; if quiescence cannot be proved it retains those owners and the
operator must reboot manually. The session sleep inhibitor remains active.

This is evidence for repeated failure at the currently tested
`DpHttConnect` boundary, not a universal reset proof for arbitrary later DMA
states. The retained physical run
`runB-fast-cycle-20260909T-current` completed two fresh-process cycles in
20,209 ms and 20,155 ms from cycle start through verified WPSS offline, before
successful VFIO unbind; the second full QMI handshake proves QRTR/WPSS service
reappearance. After the final verified offline/reaped/unbound boundary, the
wrapper disarms its emergency fallbacks and retains the inert candidate kernel
for the next userspace cycle. A separate systemd sleep inhibitor remains active
for that retained-candidate session so idle suspend cannot remove USB control;
it ends when the candidate reboots. An earlier failure leaves the reboot
fallbacks armed.

The current physical proof is retained on np at
`/var/lib/poco-linux/redwood/work/artifacts/runB-core-region1-qmi-match-20260909T043223Z`:
WPSS reached `running`, VFIO region 1 was 2 MiB, QMI DeviceInfo returned BAR
`0x61e00000`/`0x200000`, and region 1 matched and mapped. That pre-fix run then
stopped at the software QMI-to-core seam before any MMIO or CE. Revision
`0c71becf` (now in master) fixes and tests that continuation in software, but it
has not been physically rerun; there is still no MMIO/CE proof. The preceding
WPSS-offline discriminator is retained at
`/var/lib/poco-linux/redwood/work/artifacts/runB-core-region1-remoteproc-offline-20260909T042108Z`.

### Failed variants are not the procedure

Do not retry direct flashed-`#1` to forced-legacy candidate, direct `kexec -e`,
explicit-DTB hop 1, detached delayed execution, or nested-shell transaction
variants. They variously failed to return USB, bypassed the proved orderly
shutdown, had their DTB ignored, or lost the executable/argv boundary. Relevant
retained evidence on np is at
`/var/lib/poco-linux/redwood/work/artifacts/redwood-kexec-orderly-20260908T215500Z`,
`/var/lib/poco-linux/redwood/work/artifacts/redwood-runB-hop2-20260908T220000Z`,
and
`/var/lib/poco-linux/redwood/work/artifacts/redwood-runB-hop2-recovery-20260909T035711Z`.
An empty `/sys/fs/pstore` does not prove ramoops was absent: `systemd-pstore`
may already have moved records to `/var/lib/systemd/pstore`.

## Safe use

Verify USB control before a stateful run. The complete experiment must have a
deadline and automatic recovery for runner failure, timeout, or session loss;
Wi-Fi restoration is not required to collect logs or recover USB access. Manual
power-cycle is an accepted exception for a genuine kernel hang, not ordinary
runner failure. Use the smallest applicable recovery procedure, not a mandated
number of timers.

`scripts/redwood/redwood-wifi-transaction` supplies the deadline and forced
reboot used by the persisted payload above. Run its host-only recovery and argv
boundary checks with `scripts/redwood/redwood-wifi-transaction --self-test`.

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
