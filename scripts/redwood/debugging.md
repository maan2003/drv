# Redwood reset and panic debugging

Follow [ARCH-redwood-wifi-target](../../specs/ARCH-redwood-wifi-target.md):
kexec only, no partition/slot/key changes, no automatic reboot watchdog.
Build on np with plain `make`. Hold np's nonblocking exclusive
`/run/lock/drv-hardware.lock` across hardware mutation, DMA and verified
WPSS-offline cleanup; release while building or after a confirmed fresh boot.
Identify remoteprocs by `name`, not index: `remoteproc0` is the modem;
WPSS was `remoteproc2` in these experiments.

## Make ramoops survive the reset being investigated

**An empty/stale pstore did not mean there was no panic.** The experimental
Redwood DT used `mem-type = <2>` for ramoops. In this kernel,
`fs/pstore/ram_core.c:persistent_ram_vmap` maps that as cached `PAGE_KERNEL`;
`persistent_ram_write` does not explicitly clean those cache lines.
A snapshot survived a controlled kexec byte-for-byte but stayed stale across
the failing Wi-Fi reset. With the same kernel and userspace binary, changing
only the ramoops mapping to write-combined captured an asynchronous SError
panic on the next reproduction.

On np, modify a **fresh copy of the current candidate's** `/sys/firmware/fdt`:

```sh
fdtput -t x candidate.dtb /reserved-memory/ramoops@a9000000 mem-type 0
```

`0` is normal write-combined memory; do not substitute `1` (device/noncached
mapping can break the atomics used by persistent RAM). Keep reserved address
and sizes unchanged. The tested layout was 2 MiB at `0xa9000000`,
record-size `0x40000`, console-size `0x180000`, pmsg-size `0x40000`.
Compare decoded DTs and allow only the intended edits. Preserve board IDs.
The Wi-Fi register-window amendment is separate from this logging change.

Pass that DT using the existing candidate's legacy `kexec -c -l ...
--dtb=candidate.dtb` path; normal installed-kernel entry first uses the
established file-load hop. Do not assume the normal kernel accepts a custom
DT through `kexec -s`. No kernel rebuild is needed for `mem-type`.

Verify the **running** DT, not just the input file:

```sh
od -An -tx1 /sys/firmware/devicetree/base/reserved-memory/ramoops@a9000000/mem-type
dmesg | grep -iE 'ramoops|pstore'
printf '<0>REDWOOD_RAMOOPS_READY boot=%s\n' "$(cat /proc/sys/kernel/random/boot_id)" > /dev/kmsg
```

Expected bytes: `00 00 00 00`. Save boot ID, image/service hashes, DT diff,
and marker before starting the reproducer. Keep phone-local journal and
driver logging too; they establish boundaries but may miss the final panic.

## Retrieve evidence before it is erased or confused with a later boot

After an observed reset, reconnect to the normal recovery initrd and copy
`/sys/fs/pstore/*` **directly to np**, before unlocking/switching root where
possible. Otherwise inspect `/var/lib/systemd/pstore/` and the
`systemd-pstore` journal. Its archival moves/deletes records and can reuse
filenames; store each run under a distinct np evidence directory.

For controlled persistence tests, append
`systemd.mask=systemd-pstore.service` to the **experimental kexec command line**.
This is temporary; do not change the installed system. Copy records; do not
delete unrelated pstore evidence. Confirm the archived kernel version, unique
marker, and timestamps belong to the failed boot. A boot-time self-test can
produce a new snapshot that looks like a recovered old one. Validate survival
by copying/hashing before and after a hop into a kernel without that self-test.

A passed kexec persistence test alone is insufficient: kexec can flush cached
state that an abrupt reset loses. Also verify the marker and actual crash
survive the failure under investigation.

## Small incremental reset-path instrumentation

[kernel-reset-probe.patch](kernel-reset-probe.patch) is the optional test-only
patch used on the existing 7.2.0+ candidate tree (base
`509ce3d952d550f93b544c8d94c99e798f09a9b4` plus its existing Redwood changes).
It logs a stack and calls `kmsg_dump_desc(KMSG_DUMP_EMERG, ...)` at Linux
restart/power-off entry and immediately before PS_HOLD/PSCI reset. It also
raises ramoops' default accepted reason from OOPS to EMERG; an explicit DT
`max-reason` or `no-dump-oops` can still override it. EMERG uses pstore's
nonblocking lock path. The dump call is not exported to modules in this tree;
do not assume a kprobe module can simply call it.

Preserve the original Image, configuration, symbols, source diff and toolchain.
Apply/check the patch against the actual tree, then reuse its matching build
environment and run **`make -j4 Image`**, not a clean kernel/modules/DT build.
The first build compiled four changed objects plus the version object and
relinked. Configuration and `Module.symvers` remained identical.
The patch does not block resets, arm a watchdog, or enable ramoops by itself.
Its usefulness still depends on a durable ramoops mapping.

The patch also prints the current task's saved EL0 registers on fatal SError
when that task has a userspace frame. This helps when the exception arrives
during kernel syscall entry: the original dump only shows the kernel PC.
It reads no user memory and does not suppress the panic. Treat the saved EL0 PC
as context, not proof of the faulting access. At early syscall entry,
`syscallno` can still be stale; the saved `x8` contains the AArch64 syscall
number. Capture `/proc/$pid/maps` while the service is alive and retain the
exact executable for address attribution.

With the #13 incremental kernel, saved EL0 `x8 = 0x1d` identified `ioctl`;
`x1 = 0x3b86` and LR disassembly identified `IOMMU_IOAS_UNMAP`. The SError
arrived before its handler ran, so this is a DMA-unmap **entry boundary**, not
proof that unmapping caused the error. Inspect the immediately preceding
operations and ownership/release path rather than blaming the ioctl itself.

## Interpret the evidence narrowly

The captured failure was `Asynchronous SError Interrupt`, ESR `0xbfed17ff`,
CPU7, task `ath11k-wifi-ser`, PC `el0_svc+0x38/0x220`, after Connect and two
CE2 receive/refills. ESR's IDS bit is set: the syndrome is implementation
defined. An asynchronous exception's PC is **not proof of the originating
access**. The faulty access remains to be isolated; neither association nor a
fix is proved. Negotiating the native shadow-register table and redirecting CE/DP
publications did not remove the failure: the same boundary reproduced with
syndrome `0xbff517fd`. Both syndromes have IDS set; do not decode the changed ISS
as a standard architectural fault status.

PMIC GEN3 retained history separately showed PS_HOLD warm reset. Historical
FIFO entries have no timestamps: attribute a reset only from a fresh
before/after pointer and record delta. PS_HOLD identifies the reset trigger,
not whether Linux or secure firmware requested it.

Evidence on np:
`/var/lib/poco-linux/redwood/work/driver-takeover-20260912/`:
- `reset-kernel-instrumentation/`: source patches, baseline images, incremental
  build logs, byte-identical self-test snapshots, and experiment notes.
- `evidence/reset-investigation/candidate-db970e2c/`: actual SError
  `dmesg-ramoops-0`, `console-ramoops-0`, phone logs and PMIC before/after.

## Bisect with driver-side checkpoints, not process destruction

In `--diagnostic-unsandboxed` mode only, set `REDWOOD_DIAGNOSTIC_PAUSE`
when invoking `redwood-wpa3-diagnostic --start`. The launcher forwards it.
Supported checkpoints are `vfio_active`, `qmi_bar_ready`,
`client_runtime_ready`, `control_ready`, `runtime_stop_enter`,
`remoteproc_stop_enter`, `event:<CE/SoftMAC sequence>`, and
`mmio:<hex offset>` (for example `mmio:0xb81fc`). Event sequence numbers
are shared by the existing CE and SoftMAC trace callbacks.

The matching checkpoint logs `ath11k_diagnostic=PAUSED` and raises SIGSTOP
once. It keeps the complete stack and all DMA owners intact; it does **not**
stop firmware. Verify the PID's `/proc/<pid>/status` shows state `T`, retain
the np hardware lock, and observe pstore/kernel logs before resuming with
`kill -CONT <pid>`. Never SIGKILL or unbind the stopped live service. Resume
and let cooperative containment finish, or recover to a verified fresh boot.
The diagnostic client's existing deadline continues to run during a pause;
a long hold can lead straight into cleanup after resume, not the original
scan/connect sequence. A stable hold only bounds where an error was observed;
it does not prove earlier asynchronous accesses harmless.

Set `REDWOOD_DIAGNOSTIC_VFIO_TRACE=1` for runtime-only VFIO tracing; an
`mmio:` pause enables it automatically. Otherwise it is disabled to preserve
bring-up timing. It records accesses before executing them, including
DMA-unmap entry, without logging thousands of startup allocations. Tracing
and pauses perturb timing: compare a resumed failing boundary against an
otherwise identical run, not against a different kernel or service.

A full-startup trace on #13 (boot `c06f1858-0002-4d5c-8ecf-6d0561647938`)
exceeded the control deadline without reaching scan and then panicked during
cleanup. Its final logged access was a zero write to `0xb81fc`, REO destination
ring MISC, from SRNG teardown. Native `ath11k_dp_srng_cleanup` frees DMA
without this register write. This is a concrete boundary to bisect, not yet
proof that it explains the earlier Connect failures. Evidence is under
`evidence/reset-investigation/candidate-c06f1858/` on np.

The checkpoint run `4219e44d-c0a0-4e6b-9e49-89f23364599c` reproduced
scan/Connect/event 176, reached that same write, and remained SSH-reachable
with the service in state `T` for a measured 30-second observation.
SIGCONT was issued at uptime 281.41 s; ramoops recorded SError `0xbfe7d879`
at 281.623 s. This strongly implicates the resumed teardown boundary.
Retain the stopped-state snapshots and exact binary alongside the panic;
an asynchronous exception still does not identify a unique instruction.

Removing the UMAC register write (SRNG teardown no longer accepts MMIO)
allowed the same scan/Connect/event-176 path to complete cleanup on boot
`6aa20f7d-4cdd-4940-b592-d9e3bddecd82`: WPSS offline, VFIO released, wrapper
inert, same boot retained. Association still failed; heavy tracing delayed
cleanup past the diagnostic deadline. This is physical evidence for the
cleanup crash fix, not association acceptance. The cleanup regression test
rejects MMIO writes and requires complete DP ring teardown to succeed.

A second run on the same boot, with VFIO access tracing disabled, again
reached scan/Connect/event 176 and completed containment without a reset.
The diagnostic reported `DriverFault` during association, then WPSS offline
and VFIO released. Evidence is in `candidate-6aa20f7d-quiet/`; service SHA-256
`4f7eab63955a3b7cecf51e789086fb67bf8a7f2373a426ab5e2dadc7d0d16685`.
This separates the remaining association failure from the fixed cleanup
SError without the full-access tracing overhead.

## Association progress exposes a separate firmware crash

On retained boot `bd83e235-5700-43e5-b2ff-94d263c4a281`, userspace/WPSS
restarts (without further kexec) isolated and fixed the non-HT channel-width
mismatch, legacy 12-byte HTT peer-map decoding, missing live CE TX reaping,
and WMI credit/completion handling. The AP accepted SAE and association
(AID 4); driver-side association configuration completed in one run, but
no EAPOL receive/usable connection or Internet acceptance was established.

**A stable Linux boot concealed WPSS crashes.** The kernel log records
`cmnos_thread.c:4645:Asserted in whal_recv_recovery.c:whalCheckRingBkPressure:935`
and automatic remoteproc recovery. In the final instrumented run, the
firmware assertion at uptime 3432.956218 preceded the WMI peer-association
command at 3433.099562362. Subsequent missing completions therefore do not
prove that command caused the crash. Capture the kernel log for every
userspace cycle and correlate firmware lifetimes as well as Linux boot IDs.
Do not keep retrying after a firmware crash as though the previous
QMI/CE/DP generation were still valid. Investigate RX ring provisioning,
consumption/backpressure, and remoteproc recovery containment before
renewing live acceptance attempts.

That investigation’s evidence on np is under the existing work directory:
`evidence/reset-investigation/candidate-bd83e235-wmi-events/`.
Its service SHA-256 was
`f12f0e5df247441317d40c9eacc4640e3047e407da15b90688c7da6eee62977b`.
Cleanup verified WPSS offline and VFIO unbound before releasing the
hardware lock. The later Internet run below resolves that RX backpressure finding.

## Internet acceptance after receive and key-boundary fixes

On the same Linux boot `bd83e235-5700-43e5-b2ff-94d263c4a281`, populating
the previously empty WBM idle-link ring eliminated the observed RX
backpressure failure and delivered EAPOL through REO. Compiling the actual
native `rx_desc.h` confirmed the QCN9074 descriptor is **384**, not 388,
bytes; header status starts at 264 and MPDU-start at 132. The old handwritten
C oracle duplicated the Rust offset error. It now derives offsets with
`offsetof` from the pinned header.

The adapter now decodes RX channel metadata, submits data/EAPOL through TCL,
admits only EAPOL while the controlled port is closed, dispatches software
IGTK before the data-cipher switch, and converts SME's wire-order GTK/IPN
counters to the little-endian packet numbers used at the driver boundary.
Native suspend uses the firmware MAC/PHY pdev ID, not the host radio index.

`candidate-bd83e235-internet-netstack/` under the existing np evidence
directory records WPA3 association, a sandboxed Netstack3 process in its own
network namespace, and HTTPS to `https://example.com/` through its SOCKS5
endpoint: HTTP 200, TLS verification 0, and 559 downloaded bytes containing
“Example Domain”. The Wi-Fi diagnostic remains explicitly unsandboxed;
this is Internet bring-up acceptance, not production confinement acceptance.
The 180-second window completed, but PdevSuspend during cleanup triggered
`wlan_dev.c:dispatch_wlan_pdev_cmds:7696`. Orderly teardown is not proved.
Attended recovery verified WPSS offline and VFIO unbound before releasing the
hardware lock; Linux retained the same boot ID.

`redwood-wpa3-diagnostic` now defaults to the staged `drv-network-service`
and retains the Ethernet generation for a bounded 180-second SOCKS5 window
on phone loopback port 1080. Use `REDWOOD_NETWORK_SERVICE=` for the earlier
association-only diagnostic. Proof clients must explicitly use
`socks5h://127.0.0.1:1080` with no proxy bypass, so DNS and TCP traverse
Netstack3 rather than the phone's Linux routing.

With automatic remoteproc recovery disabled, a crashed WPSS can reject normal
`stop`. In the attended recovery used here, the retained service was stopped
with SIGSTOP while all mappings remained live; an explicit debugfs `recover`
reset WPSS and copied its coredump, then normal `stop` verified `offline`.
Only then was the stuck service killed and VFIO unbound. A coredump set to
`enabled` is generated during recovery, not merely on entering `crashed`.

## Use direct incremental Cargo for userspace iterations

Do not rebuild a Nix Rust package for every edit. On np, the retained
`driver-takeover-20260912/cargo-cross/` workspace has a writable pinned
reference tree, offline vendored dependencies, and `build-service`. The Internet diagnostic
also links the existing network supervisor; its additional dependencies are
available through the retained `vendor-union/` of existing vendor artifacts.
That script invokes Cargo directly with the existing ARM64 musl toolchain,
a persistent `target/`, `CARGO_INCREMENTAL=1`, and two jobs; it does not
invoke a Nix build. Sync only changed project files into `crates/`; apply
upstream port-patch changes to the writable `reference/` as well.
The first cache population took 1m 50s, a no-change repeat took 0.23s,
and subsequent source-edit builds typically took 2–6s. Keep exact build
and test logs with the hardware evidence. Kernel work still uses plain
incremental `make` on np, never Nix.
