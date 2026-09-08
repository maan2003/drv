# ARCH-audio-runtime: Userspace audio runtime (obsolete)

## Status

**Legacy / outdated.** This record is retained for the existing experimental
artifacts, not as current production architecture or an active implementation
plan. Details below may be stale and must be checked before reuse. Current
project direction is defined by [ARCH-drv](ARCH-drv.md).

Existing speaker-protection and hardware-use constraints still apply to legacy
experiments. This label does not authorize physical audio output or bypass the
existing protection review and hardware-window requirements.

## Process and real-time boundaries

The hardware daemon exclusively owns VFIO/iommufd, HDA BAR and codec access,
DMA memory, interrupts, the real-time executor, and speaker protection. Its
non-real-time manager validates declarative graph requests and builds bounded
immutable snapshots in preallocated executor-local slots. A separate PipeWire
process owns native-protocol compatibility, client lifecycle, policy, and
client shared memory.

The real-time thread reads one atomic `{slot, generation}` publication at each
quantum boundary and acknowledges the retired generation after its final
quantum. It never allocates or frees, takes a lock, logs, accesses a filesystem,
performs control-process IPC, or waits for graph publication. Its only permitted
wait is the HDA completion/deadline primitive. At least three fixed snapshot
slots prevent a producer from reusing a snapshot while the executor can still
observe it; only non-real-time code reclaims acknowledged generations.

The process seam is a bounded, versioned `SOCK_SEQPACKET` control channel.
Declarative topology and parameters, validated formats, pool indices and
generations, setup-time shared-ring descriptors, and asynchronous health or
position reports may cross it. Per-quantum PCM messages, synchronous real-time
acknowledgements, pointers, executable DSP objects, mutable snapshots, IOVAs,
VFIO or codec authority, and changes to protection limits may not cross it.
PCM later uses fixed shared SPSC cells; the executor observes driver-owned
ready cells rather than receiving a per-quantum message.

## Phase 1 execution budget

Phase 1 fixes the hardware format at stereo S16LE, 48 kHz and deliberately has
one hardware clock and one internal preallocated source:

- one quantum and one BDL period is 480 frames, 1,920 bytes, or 10 ms;
- the continuous BDL has eight entries and therefore 80 ms of queued depth;
- target source-to-speaker latency is at most 100 ms;
- each refill and graph execution must finish within 2 ms of its completion
  notification, leaving 8 ms or 80% of each period as declared deadline margin;
- missed source data becomes an already-allocated silent period and increments
  an XRUN counter; FIFO or descriptor faults and missed HDA progress latch a
  safety fault rather than being counted as successful playback.

These are conservative bring-up numbers. Later phases may reduce queue depth
only after measured worst-case execution and scheduling latency retain an
explicit margin.

## Speaker protection and containment

Speaker protection is the final, structurally mandatory executor stage and is
not a graph node. Phase 1 applies a 20 Hz DC blocker, saturating conversion, an
absolute digital ceiling of 256/32768, and an ALC256 output-amplifier ceiling
approximately 36 dB below unity. No unprotected sample can enter a DMA cell.

The provisional no-calibration envelope assumes a deliberately pessimistic
12 V RMS full-scale sine output and a 2 ohm minimum load. For a sine, the
dimensionless peak-sample ratio maps that full-scale RMS voltage to RMS
voltage. The two ceilings therefore bound the continuous output to
approximately 1.486 mV RMS, 0.743 mA RMS, and 1.10 microwatts:

`12 V * (256 / 32768) * 10^(-36/20) / 2 ohm`.

This conservative signal-domain bound is used only for low-volume bring-up; it
is not a calibrated current, thermal, or excursion model. Real speaker output
remains disabled if review cannot accept its assumptions. RMS/thermal,
low-frequency excursion, and slew models will refine rather than relax this
initial peak envelope when platform parameters become available.

A renewable lease observes both completed executor cycles and IOC/LPIB
progress. Three consecutive 10 ms intervals without either constitute a stall.
Expiry latches the fault, mutes DAC and pin, stops RUN, disables EAPD, then
disables DMA and bus mastering. Every staged DMA cell is independently safe
under the continuous envelope so repeated hardware data remains bounded during
that shutdown interval. An independent supervisor retains recovery authority and
parks the function under VFIO or reboots when safe quiescence cannot be proven.

## Clock reconciliation after Phase 1

Real PipeWire producers introduce a clock domain separate from the HDA clock.
Before those producers enter the driver-owned pool, the control/data boundary
will estimate fill-level and timestamp drift and apply bounded asynchronous SRC
or adaptive resampling. Ratio changes are smoothed and bounded; discontinuity,
overflow, and starvation use explicit drain/drop/silence policy. Graph snapshot
updates carry clock-domain identity and resampler state ownership so a graph
swap cannot reset drift estimation or produce a discontinuity.

## Kernel PCM removal

The AMD HDA function has no kernel PCM path only when it remains VFIO-bound
from boot through shutdown, no ALSA library, PCM device or ioctl touches it,
and userspace owns every BDL, DMA buffer, interrupt, codec operation, and PCM
frame. A system-wide claim additionally requires `CONFIG_SND_PCM=n`, removal
of relevant modules and `/dev/snd/pcm*`, and cold-boot-to-shutdown tracing.
Rollback is a separate bootable system generation, never a runtime PCM fallback.
