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

## Interpret the evidence narrowly

The captured failure was `Asynchronous SError Interrupt`, ESR `0xbfed17ff`,
CPU7, task `ath11k-wifi-ser`, PC `el0_svc+0x38/0x220`, after Connect and two
CE2 receive/refills. ESR's IDS bit is set: the syndrome is implementation
defined. An asynchronous exception's PC is **not proof of the originating
access**. The faulty access remains to be isolated; neither association nor a
fix is proved.

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
