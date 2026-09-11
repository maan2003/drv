# ARCH-redwood-wifi-target: WCN6750 userspace Wi-Fi target

## Status

Redwood is a bring-up and driver-port bug-discovery target, aiming for scan,
association, DHCP, and proved Internet connectivity before production
hardening. Physical bring-up now completes QMI, the full core lifecycle,
passive scanning, and scan-result collection. A bounded physical run delivered
management frames and four BSS summaries across channels 1, 6, and 11. The
same run proved normal scan-result cleanup with WPSS stopped before VFIO
release and no SMMU or WPSS fault. Association is not yet proved. The ath11k
SoftMAC adapter now carries association security provenance into the ported
key, peer-security-index, and REO replay effects, and a separate production
service composes it behind a fatal WCN6750-specific sandbox, but that path
still needs renewed physical association and Internet acceptance. An explicitly
labelled operator diagnostic mode bypasses only that confinement gate and sends
one exact scan and connect request directly to the existing ClientRuntime/SME
control seam; it does not make production wlancfg persistence or namespace
setup a bring-up prerequisite and does not duplicate SAE/RSN policy. The
production default remains fail-closed and sandboxed. The target network is
the India-domain (`IN`) WPA3-Personal network `ajay` on 5 GHz channel 149. The
service installs only a conservative channel subset after a fresh successful
firmware regulatory event confirms the country, band rule, active-initiation
flags, bandwidth, and power bounds. Its SME-managed SAE path advertises PMF
only with software BIP-CMAC-128/IGTK transmit, receive, and replay handling.

An exact `#9`/32-byte-DT run reached control `Ready`, completed its
first data-path poll with no delivered or malformed packets, then reset during
the first control poll. Its bounded CE trace received and refilled one ready
CE2 frame, then repeatedly found the receive rings empty and completed 10 ms
waits normally until the 256-record trace cap was exhausted. Source inspection
found that this runtime poll incorrectly reused the full synchronous control
deadline after draining ready work. Runtime event polling now uses a zero CE
deadline so it drains already-completed frames and returns immediately when
quiet; bring-up and synchronous command paths retain their full deadlines.

A subsequent exact run containing that fix durably reached the corrected
runtime poll. An np-local collector acknowledged the exact candidate identity,
service entry, control readiness, and control-loop entry before recording 70 CE
markers. The first receive consumed and refilled one ready CE2 frame. Four
following receives each reported an expired zero deadline and returned empty,
physically establishing that the changed CE path drains ready work and returns
when quiet. The separated empty-receive groups also establish that multiple
corrected control polls returned: without another routed frame, each empty
group ends its current `poll_wlan_event`, and later groups require a subsequent
poll. The phone reset after the last acknowledged empty return. The durable
stream did not distinguish that final poll's return, policy-request dispatch,
or a following WMI send, so it does not identify the remaining reset boundary
or cause. Per-marker TCP acknowledgements deliberately perturbed timing, so
this is evidence for the bounded diagnostic experiment, not production timing
behavior.

A follow-up run added checkpoints around runtime CE transmission and reproduced
the same receive prefix through marker 69 without reaching any transmit-entry
checkpoint. It excludes runtime CE ring publication in that run, but not later
processing of commands issued during startup. The next bounded discriminator
therefore checkpoints SoftMAC drive, data-path, control-return, and passive-scan
dispatch boundaries before narrowing any device mutation further.

The diagnostic ran in a phone-local transient unit with no external timeout or
transport-owned lifetime; the submitting SSH session had already exited
normally. The np-local hardware lock remained held across the reset, and the
fresh recovery boot found WPSS offline and the platform device unbound. The
recovery pstore record contained no panic or reset signature. A previous run
completed four control polls, so the failure is not deterministic. Bounded CE
runtime markers remain capped at 256 records. Association remains unproved.

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
  WPSS before DMA and accepts manual recovery. On an ordinary runner error
  returned through control flow, the
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

Before a shared-resource-conflicting mutation, verify a working control path
and hold an atomic nonblocking exclusive flock on
`np:/run/lock/drv-hardware.lock` for the critical mutation and cleanup. Release
it during local builds, nonconflicting staging, idle boot dwell, connectivity
waits, and manual recovery when no active DMA ownership remains; reacquire and
revalidate state before the next conflicting mutation. After acquisition, fail
closed if durable Wi-Fi lab state reports quarantine or an unresolved
transaction. USB SSH through `usb0` at
172.16.42.1/24 is useful but does not gate a run when the existing Tailscale
control path works. Wi-Fi loss is expected. Preserve reports locally; restoring
`wlan0` is not a prerequisite for recovery or evidence collection.

Experiments have a bounded duration and operator-attended manual recovery; no
automatic Redwood reboot watchdog is armed. At the current proved core and
passive-scan stages, the runner
retains all VFIO authority while it stops WPSS and verifies `offline`; the
wrapper then verifies the child is reaped and no cdev descriptor remains before
unbinding VFIO. A stop or verification failure holds that authority rather than
dropping live DMA mappings. Recovery remains with the operator. After normal
verified cleanup, the wrapper retains the inert candidate for another userspace
cycle.

No automatic Redwood reboot watchdog is used. The experiment deadline bounds
cooperative stalls, but an uncatchable process kill cannot run same-process
WPSS cleanup; production needs a surviving external containment owner before
claiming orderly cleanup for fatal parent death or seccomp failure. Genuine
kernel hangs may require a manual power-cycle:
the project owner explicitly accepts that residual risk. Additional machinery
to eliminate it is not required for bring-up. Tests should exercise the failure
path being relied on; a fake test that omits VFIO ownership/reset/remoteproc
cannot establish live recovery correctness.

IOMMU confinement, bounded DMA ownership, and device exclusivity remain required.
Wi-Fi experiments use **kexec-only** kernels and DTBs; they do not flash
boot/vendor_boot, modify partitions or slot metadata, or touch encryption keys.
The owner-authorized NixOS reinstall is a separate maintenance operation: it may
reformat userdata and install a validated slot-B kernel/configuration for
untethered boot. Preserve the bootloader/firmware chain and fastboot recovery. Every passed DTB retains
`qcom,board-id = <0x1000b 0>` and `xiaomi,board-id = <0xe 0>`. Keep the unchanged
known-good boot configuration for lab recovery; this is not a native-driver
runtime fallback in the production stack. Warm kexec into the embedded rescue
image is known not to work from a real boot and must not be assumed as recovery.

## Build and configuration ownership

Kernel builds run on **np over SSH, using plain `make` outside Nix**. Do not
substitute a Nix kernel build or `nix copy` workflow. Coordinate np usage with
the shared filesystem lock rather than an owner-message handshake. The NixOS configuration repository is `~/src/nixos`
(`/home/maan2003/src/nixos` in the coordinator's environment); it owns NixOS
system configuration, not the plain-make experimental kernel build. Keep exact
build/deployment commands with the runner rather than duplicating them here.
