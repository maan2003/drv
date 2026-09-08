# ARCH-redwood-wifi-target: WCN6750 userspace Wi-Fi target

## Status

Redwood is a bring-up and driver-port bug-discovery target, aiming for scan,
association, DHCP, and proved Internet connectivity before production hardening.
The QMI-only discovery run reported BAR address `0x61e00000` and size
`0x200000`, then stopped before MMIO/CE/HTC/WMI. The DT-assisted region-selection
runner is merged but not physically validated. Automatic recovery for that
no-reset VFIO experiment now uses a process timeout plus a local forced-reboot
deadline and reboots on runner exit instead of attempting a native-driver
rebind. Its stalled-runner and successful-exit paths pass a host-only test. An
ordinary pre-release runner failure physically forced reboot to the unchanged
Linux 7.2.0 #1 system, which was unlocked and reached running state over the
healthy USB control path. The QMI region-selection path remains unvalidated.

Redwood is a POCO X5 Pro 5G (`xiaomi,redwood`, Qualcomm SM7325) running the
project's Linux 7.2.0. Its WCN6750 is platform device `17a10040.wifi`,
compatible `qcom,wcn6750-wifi`, normally driven by `ath11k_ahb`, and alone in
SMMU IOMMU group 6. It is AHB, not PCIe.

## Minimal kernel substrate

The production boundary keeps generic SoC mechanisms in the kernel and moves
the device driver above them:

- ARM SMMU remains kernel-owned so an untrusted userspace driver receives only
  explicitly mapped private DMA and cannot program translation hardware.
- Qualcomm remoteproc/PIL boots WPSS and authenticates its firmware through
  TrustZone SCM/PAS SMCs. Userspace cannot execute this privileged boot path.
  Remoteproc is generic and is already shared by modem/ADSP-class processors.
- SMEM/SMP2P/GLINK/QRTR remain as shared Qualcomm IPC transport. The userspace
  QMI client uses AF_QIPCRTR; QMI policy and WLAN handshake move out.
- VFIO-platform (or a comparably narrow broker if it proves insufficient)
  exposes only fenced MMIO, DMA and interrupts. WCN6750 has no upstream
  vfio-platform reset handler; pinned ath11k AHB contains the firmware through
  the WPSS remoteproc lifecycle instead. A no-RESET cdev is admitted only for
  the polling diagnostic after the supervisor/operator externally restarts
  WPSS before DMA, names that now-running remoteproc to the read-only runner,
  and arms a watchdog to reboot on run or restart failure. The runner never
  controls remoteproc. Production still requires a proved remoteproc
  restart/reset ownership contract.

Everything ath11k-specific moves to userspace: QMI WLAN handshake, WMI, HTC/CE,
HTT, HAL descriptors/registers/SRNG, TCL/REO/WBM data path, and hardware-facing
pdev/vdev/peer operations. mac80211/cfg80211 glue does not move; the Fuchsia
MLME drives the existing WlanSoftmac seam described by
[ARCH-wlan-stack-topology](ARCH-wlan-stack-topology.md).

```text
userspace: Fuchsia MLME ─WlanSoftmac─▶ ath11k core/WMI/DP/HAL/QMI
                                             │
kernel:   VFIO platform ─▶ SMMU     AF_QIPCRTR┘
          remoteproc/PIL ─▶ WPSS ─▶ SMEM/SMP2P/GLINK/QRTR
```

## Agile bring-up and recovery

Run the smallest useful experiment, inspect the first real failure, fix it,
check proportionately, and rerun. The Redwood owner handles driver changes,
builds, deployment, and testing end to end, without advisors. Exhaustive oracle
coverage, production abstractions, and deferred interrupt/broker designs are
not prerequisites for each polling experiment. Keep stage and native transcript
evidence honest; source-derived fixtures are not physical captures.

Before a stateful experiment, verify USB SSH through `usb0` at 172.16.42.1/24
and coordinate exclusive hardware access through no-plastic (`np`). Wi-Fi loss
is expected. Preserve reports locally and retrieve them over USB; restoring
`wlan0` is not a prerequisite for recovery or evidence collection.

Each experiment has a bounded duration and automatic recovery to a reachable,
known state on ordinary runner failure, timeout, or control-session loss. Use
the smallest mechanism that covers those failures. There is no requirement for
a particular number of timers or for rebinding ath11k/iwd. For the current
no-reset VFIO experiment, reboot to the unchanged known-good system is preferred
to an unvalidated rebind sequence. A runner's successful exit is not recovery:
do not disarm protection before the intended cleanup/recovery completes.

The current initrd userspace watchdog can recover process/control-path loss,
not a hung kernel. An independently ticking runner heartbeat is not evidence
that the device operation is progressing; the experiment deadline must still
bound a stalled runner. Genuine kernel hangs may require a manual power-cycle:
the project owner explicitly accepts that residual risk. Additional machinery
to eliminate it is not required for bring-up. Tests should exercise the failure
path being relied on; a fake test that omits VFIO ownership/reset/remoteproc
cannot establish live recovery correctness.

IOMMU confinement, bounded DMA ownership, and device exclusivity remain required.
New kernels and DTBs are **kexec-only**. Never flash boot/vendor_boot, modify
partitions or slot metadata, or touch encryption keys. Every passed DTB retains
`qcom,board-id = <0x1000b 0>` and `xiaomi,board-id = <0xe 0>`. Keep the unchanged
known-good boot configuration for lab recovery; this is not a native-driver
runtime fallback in the production stack. Warm kexec into the embedded rescue
image is known not to work from a real boot and must not be assumed as recovery.

## Build and configuration ownership

Kernel builds run on **np over SSH, using plain `make` outside Nix**. Do not
substitute a Nix kernel build or `nix copy` workflow. Coordinate np usage with
the MT7921/substrate owner. The NixOS configuration repository is `~/src/nixos`
(`/home/maan2003/src/nixos` in the coordinator's environment); it owns NixOS
system configuration, not the plain-make experimental kernel build. Keep exact
build/deployment commands with the runner rather than duplicating them here.
