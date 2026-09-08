# ARCH-redwood-wifi-target: WCN6750 userspace Wi-Fi target

## Status

The parallel-port interfaces and pinned source oracle exist. The native byte
transcript is blocked because the running kernel has
`CONFIG_ATH11K_TRACING` unset. No Wi-Fi handoff or kexec experiment has been
run. VFIO-platform feasibility therefore remains unproven.

Redwood is a POCO X5 Pro 5G (`xiaomi,redwood`, Qualcomm SM7325) running the
project's Linux 7.2.0. Its WCN6750 is platform device `17a10040.wifi`,
compatible `qcom,wcn6750-wifi`, currently bound through `ath11k_ahb`, and
alone in SMMU IOMMU group 6. It is AHB, not PCIe.

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
  exposes only fenced MMIO, DMA and interrupts. A real WCN6750 reset contract
  is required for production; `reset_required=0` is development-only and
  unsafe because assignment does not establish reset isolation.

Everything ath11k-specific moves to userspace: QMI WLAN handshake, WMI, HTC/CE,
HTT, HAL descriptors/registers/SRNG, TCL/REO/WBM data path, and hardware-facing
pdev/vdev/peer operations. mac80211/cfg80211 glue does not move; the Fuchsia
MLME drives the existing WlanSoftmac seam described by
[ARCH-wlan-stack-topology](ARCH-wlan-stack-topology.md).

At the pinned source, the in-kernel ath11k directory is 82,878 lines (roughly
the stated 100k-line driver surface once adjacent integration is included).
The retained shared substrate measures about 5,899 SMMU, 9,581 Qualcomm
remoteproc, 2,319 SMEM/SMP2P, 3,387 GLINK, and 2,722 QRTR lines, plus 1,780
generic VFIO-platform lines. These are physical line counts, not trusted-code
equivalence. The Wi-Fi-specific kernel target is zero lines if generic
vfio-platform can express reset; otherwise only the smallest WCN6750 reset
adapter remains. Shared SoC infrastructure must not be charged as a new
Wi-Fi driver, but it remains privileged attack surface.

```text
userspace: Fuchsia MLME ─WlanSoftmac─▶ ath11k core/WMI/DP/HAL/QMI
                                             │
kernel:   VFIO platform ─▶ SMMU     AF_QIPCRTR┘
          remoteproc/PIL ─▶ WPSS ─▶ SMEM/SMP2P/GLINK/QRTR
```

## Supervised transactional handoff

Redwood's `wlan0` is its only live management uplink. `usb0` is configured
as SSH-only ECM at 172.16.42.1/24, including initrd recovery, but the phone is
not physically USB-connected, so it is not currently an out-of-band channel.
No serial console is available. The current kernel has a Qualcomm watchdog
module configured, but no `/dev/watchdog` was present during inventory; the
hardware-watchdog reboot lease and post-reboot Wi-Fi return must be explicitly
proven before any unbind or kexec.

A handoff must run under two independent bounds: a local systemd restore timer
that unconditionally rebinds ath11k and restarts iwd, and the SoC hardware
watchdog, whose expiry reboots the unchanged flashed known-good slot-B system.
Reports are persisted under a root-owned local directory and uploaded only
after wlan0 returns. Session loss follows the same restore path. The checked-in
transaction script has a fake-backend failure test; this does not substitute
for the required physical watchdog proof.

New kernels are **kexec-only**. Never flash boot/vendor_boot, alter slot
metadata, or touch LUKS keys. Every passed DTB must retain
`qcom,board-id = <0x1000b 0>` and `xiaomi,board-id = <0xe 0>`. A hung or
unlock-failing kexec kernel is recovered only by watchdog reboot into the
unchanged flashed kernel; warm kexec into the embedded rescue image is known
not to work from a real boot. Kernel builds occur directly on no-plastic, never
via `nix copy`, and must be coordinated with its MT7921 hardware owner.

## Measurement

For WCN6750 record: per-crate source and Rust lines; reused portable hardware
API, WlanSoftmac/MLME/SME/RSN, harness and transcript tooling; wall-clock time
from foundation approval to first scan, association, DHCP and Internet; and
every invented guard rejected at a seam. Compare byte-complete QMI/WMI/HTT and
descriptor fixtures with native Linux. Track non-observable ring state
separately rather than presenting source-derived fixtures as a captured oracle.
