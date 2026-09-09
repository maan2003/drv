# ARCH-redwood-wifi-target: WCN6750 userspace Wi-Fi target

## Status

Redwood is a bring-up and driver-port bug-discovery target, aiming for scan,
association, DHCP, and proved Internet connectivity before production
hardening. Physical bring-up now completes QMI, the full core lifecycle,
passive scanning, and scan-result collection. A bounded physical run delivered
management frames and four BSS summaries across channels 1, 6, and 11. The
same run proved normal scan-result cleanup with WPSS stopped before VFIO
release and no SMMU or WPSS fault. Association is not yet proved: observed
BSSes require RSN, while the ath11k SoftMAC adapter still blocks key
installation until the device's REO packet-number replay and peer
security-index effects are ported.

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
  WPSS before DMA and arms independent reboot fallbacks. On a runner error, the
  runner requests and verifies a synchronous WPSS stop through remoteproc while
  it still owns every VFIO mapping. The privileged authentication and stop
  implementation remain kernel-owned in remoteproc/PIL; userspace owns the
  experiment's lifecycle policy. Production still requires a proved
  remoteproc restart/reset ownership contract beyond the currently proved
  terminal paths.

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

Experiments normally have a bounded duration and automatic recovery to a
reachable, known state on ordinary runner failure, timeout, or control-session
loss. Operator-attended diagnostics may instead select explicit manual recovery
when automatic reboot is itself under investigation and the operator accepts a
manual reboot. At the current proved core and passive-scan stages, the runner
retains all VFIO authority while it stops WPSS and verifies `offline`; the
wrapper then verifies the child is reaped and no cdev descriptor remains before
unbinding VFIO. A stop or verification failure holds that authority rather than
dropping live DMA mappings. Automatic mode leaves its independent reboot
fallbacks armed; manual mode leaves recovery to the operator. After normal
verified cleanup, the wrapper retains the inert candidate for another userspace
cycle. A session-lifetime sleep inhibitor keeps USB control available while
that candidate is idle.

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
