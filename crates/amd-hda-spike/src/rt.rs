//! Fixed Phase 1 real-time audio execution contract.
//!
//! `ProtectedQuantum` is the only value accepted by the DMA ring, making the
//! final protection stage structurally unavoidable. Hardware activation is
//! deliberately outside this module until the Phase 1B safety gate opens.

use drv_fuchsia_audio_processing::{apply_gain_scale_s16, gain_db_to_scale};
use drv_fuchsia_audio_timeline::TimelineFunction;

pub const SAMPLE_RATE: usize = 48_000;
pub const CHANNELS: usize = 2;
pub const QUANTUM_FRAMES: usize = 480;
pub const QUANTUM_SAMPLES: usize = QUANTUM_FRAMES * CHANNELS;
pub const QUANTUM_BYTES: usize = QUANTUM_SAMPLES * size_of::<i16>();
pub const PERIOD_MICROS: u64 = 10_000;
pub const BDL_ENTRIES: usize = 8;
pub const QUEUED_MILLIS: u64 = 80;
pub const TARGET_LATENCY_MILLIS: u64 = 100;
pub const EXECUTION_BUDGET_MICROS: u64 = 2_000;
pub const DEADLINE_MARGIN_PERCENT: u8 = 80;
pub const DIGITAL_PEAK_CEILING: i16 = 256;
pub const STALL_INTERVALS: u8 = 3;

const FIXED_GAIN_DB: f32 = -6.020_600_3;
// exp(-2*pi*20/48000), fixing the Phase 1 DC blocker at 20 Hz.
const DC_BLOCKER_R: f32 = 0.997_385_43;

#[derive(Clone)]
pub struct PcmQuantum {
    samples: [i16; QUANTUM_SAMPLES],
}

impl Default for PcmQuantum {
    fn default() -> Self {
        Self {
            samples: [0; QUANTUM_SAMPLES],
        }
    }
}

impl PcmQuantum {
    pub fn from_fn(mut sample: impl FnMut(usize) -> i16) -> Self {
        let mut value = Self::default();
        for (index, output) in value.samples.iter_mut().enumerate() {
            *output = sample(index);
        }
        value
    }

    pub fn samples(&self) -> &[i16; QUANTUM_SAMPLES] {
        &self.samples
    }
}

#[derive(Clone)]
pub struct ProtectedQuantum {
    samples: [i16; QUANTUM_SAMPLES],
}

impl Default for ProtectedQuantum {
    fn default() -> Self {
        Self {
            samples: [0; QUANTUM_SAMPLES],
        }
    }
}

impl ProtectedQuantum {
    pub fn peak(&self) -> i16 {
        self.samples
            .iter()
            .map(|sample| sample.saturating_abs())
            .max()
            .unwrap_or(0)
    }

    pub fn checksum(&self) -> i64 {
        self.samples.iter().map(|sample| i64::from(*sample)).sum()
    }
}

#[derive(Default)]
struct SpeakerProtection {
    previous_input: [f32; CHANNELS],
    previous_output: [f32; CHANNELS],
}

impl SpeakerProtection {
    fn process(&mut self, input: &[i16; QUANTUM_SAMPLES], output: &mut ProtectedQuantum) {
        for (frame, samples) in input.chunks_exact(CHANNELS).enumerate() {
            for (channel, sample) in samples.iter().enumerate() {
                let current = f32::from(*sample);
                let blocked = current - self.previous_input[channel]
                    + DC_BLOCKER_R * self.previous_output[channel];
                self.previous_input[channel] = current;
                self.previous_output[channel] = blocked;
                let rounded = (blocked + if blocked >= 0.0 { 0.5 } else { -0.5 }) as i32;
                output.samples[frame * CHANNELS + channel] = rounded.clamp(
                    -i32::from(DIGITAL_PEAK_CEILING),
                    i32::from(DIGITAL_PEAK_CEILING),
                ) as i16;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuantumReport {
    pub frame_position: u64,
    pub dsp_checksum: i64,
    pub protected_checksum: i64,
    pub protected_peak: i16,
}

pub struct FixedRtExecutor {
    dsp: [i16; QUANTUM_SAMPLES],
    protected: ProtectedQuantum,
    protection: SpeakerProtection,
    gain_scale: f32,
    frames: i64,
    frame_timeline: TimelineFunction,
}

impl Default for FixedRtExecutor {
    fn default() -> Self {
        Self {
            dsp: [0; QUANTUM_SAMPLES],
            protected: ProtectedQuantum::default(),
            protection: SpeakerProtection::default(),
            // Graph construction is non-RT; DbToScale and its libm call never
            // occur inside `execute`.
            gain_scale: gain_db_to_scale(FIXED_GAIN_DB),
            frames: 0,
            frame_timeline: TimelineFunction::new(0, 0, 1, 1).unwrap(),
        }
    }
}

impl FixedRtExecutor {
    pub fn execute(
        &mut self,
        input: &PcmQuantum,
        sink: &mut ContinuousDmaRing,
    ) -> Result<QuantumReport, DmaRingError> {
        self.dsp.copy_from_slice(input.samples());
        apply_gain_scale_s16(&mut self.dsp, self.gain_scale);
        let dsp_checksum = self.dsp.iter().map(|sample| i64::from(*sample)).sum();
        self.protection.process(&self.dsp, &mut self.protected);
        sink.submit(&self.protected)?;
        self.frames += QUANTUM_FRAMES as i64;
        Ok(QuantumReport {
            frame_position: self.frame_timeline.apply(self.frames) as u64,
            dsp_checksum,
            protected_checksum: self.protected.checksum(),
            protected_peak: self.protected.peak(),
        })
    }
}

pub struct ContinuousDmaRing {
    entries: [ProtectedQuantum; BDL_ENTRIES],
    queued: [bool; BDL_ENTRIES],
    next: usize,
    submitted_periods: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DmaRingError {
    SlotStillOwnedByHardware,
    UnexpectedCompletion,
    CompletionCountExceedsRing,
    Progress(DmaProgressError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DmaProgressError {
    MissingInterrupt,
    StalledPosition,
    StreamFault,
}

#[derive(Default)]
pub struct DmaProgress {
    last_lpib: u32,
    completions: u64,
    xruns: u64,
    fault: Option<DmaProgressError>,
}

impl DmaProgress {
    pub fn complete(
        &mut self,
        ioc_count: u64,
        lpib: u32,
        stream_fault: bool,
    ) -> Result<(), DmaProgressError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if stream_fault {
            self.fault = Some(DmaProgressError::StreamFault);
            return Err(DmaProgressError::StreamFault);
        }
        if ioc_count == 0 {
            return Err(DmaProgressError::MissingInterrupt);
        }
        if lpib == self.last_lpib {
            return Err(DmaProgressError::StalledPosition);
        }
        self.last_lpib = lpib;
        self.completions += ioc_count;
        Ok(())
    }

    fn record_starvation(&mut self) {
        self.xruns += 1;
    }

    pub fn completions(&self) -> u64 {
        self.completions
    }

    pub fn xruns(&self) -> u64 {
        self.xruns
    }

    pub fn fault(&self) -> Option<DmaProgressError> {
        self.fault
    }
}

impl Default for ContinuousDmaRing {
    fn default() -> Self {
        Self {
            entries: std::array::from_fn(|_| ProtectedQuantum::default()),
            queued: [false; BDL_ENTRIES],
            next: 0,
            submitted_periods: 0,
        }
    }
}

impl ContinuousDmaRing {
    pub fn submitted_periods(&self) -> u64 {
        self.submitted_periods
    }

    pub fn queued_frames(&self) -> usize {
        BDL_ENTRIES * QUANTUM_FRAMES
    }

    pub fn entry_peak(&self, index: usize) -> i16 {
        self.entries[index].peak()
    }

    pub fn complete(
        &mut self,
        index: usize,
        progress: &mut DmaProgress,
        ioc_count: u64,
        lpib: u32,
        stream_fault: bool,
    ) -> Result<(), DmaRingError> {
        let count = usize::try_from(ioc_count)
            .ok()
            .filter(|count| *count <= BDL_ENTRIES)
            .ok_or(DmaRingError::CompletionCountExceedsRing)?;
        if index != self.next
            || count == 0
            || (0..count).any(|offset| !self.queued[(index + offset) % BDL_ENTRIES])
        {
            return Err(DmaRingError::UnexpectedCompletion);
        }
        progress
            .complete(ioc_count, lpib, stream_fault)
            .map_err(DmaRingError::Progress)?;
        for offset in 0..count {
            self.queued[(index + offset) % BDL_ENTRIES] = false;
        }
        Ok(())
    }

    pub fn refill_silence(&mut self, progress: &mut DmaProgress) -> Result<(), DmaRingError> {
        if self.queued[self.next] {
            return Err(DmaRingError::SlotStillOwnedByHardware);
        }
        self.entries[self.next] = ProtectedQuantum::default();
        self.queued[self.next] = true;
        self.next = (self.next + 1) % BDL_ENTRIES;
        self.submitted_periods += 1;
        progress.record_starvation();
        Ok(())
    }

    fn submit(&mut self, period: &ProtectedQuantum) -> Result<(), DmaRingError> {
        if self.queued[self.next] {
            return Err(DmaRingError::SlotStillOwnedByHardware);
        }
        self.entries[self.next].clone_from(period);
        self.queued[self.next] = true;
        self.next = (self.next + 1) % BDL_ENTRIES;
        self.submitted_periods += 1;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseAction {
    Continue,
    MuteAndPark,
}

#[derive(Default)]
pub struct ProgressLease {
    last_rt_cycle: u64,
    last_lpib: u32,
    stalled: u8,
    latched: bool,
}

impl ProgressLease {
    pub fn observe(&mut self, rt_cycle: u64, lpib: u32) -> LeaseAction {
        if self.latched {
            return LeaseAction::MuteAndPark;
        }
        let both_progressed = rt_cycle != self.last_rt_cycle && lpib != self.last_lpib;
        self.last_rt_cycle = rt_cycle;
        self.last_lpib = lpib;
        if !both_progressed {
            self.stalled = self.stalled.saturating_add(1);
        } else {
            self.stalled = 0;
        }
        if self.stalled >= STALL_INTERVALS {
            self.latched = true;
            LeaseAction::MuteAndPark
        } else {
            LeaseAction::Continue
        }
    }

    pub fn is_latched(&self) -> bool {
        self.latched
    }
}

pub trait EmergencyQuiesce {
    type Error;
    fn mute_dac_and_pin(&mut self) -> Result<(), Self::Error>;
    fn stop_run(&mut self) -> Result<(), Self::Error>;
    fn disable_eapd(&mut self) -> Result<(), Self::Error>;
    fn disable_dma(&mut self) -> Result<(), Self::Error>;
    fn disable_bus_mastering(&mut self) -> Result<(), Self::Error>;
}

#[derive(Default)]
pub struct StallWatchdog {
    lease: ProgressLease,
    quiesce_attempted: bool,
}

impl StallWatchdog {
    pub fn tick<Q: EmergencyQuiesce>(
        &mut self,
        rt_cycle: u64,
        lpib: u32,
        device: &mut Q,
    ) -> Result<LeaseAction, Q::Error> {
        let action = self.lease.observe(rt_cycle, lpib);
        if action == LeaseAction::MuteAndPark {
            self.quiesce(device)?;
        }
        Ok(action)
    }

    pub fn fault<Q: EmergencyQuiesce>(&mut self, device: &mut Q) -> Result<LeaseAction, Q::Error> {
        self.lease.latched = true;
        self.quiesce(device)?;
        Ok(LeaseAction::MuteAndPark)
    }

    fn quiesce<Q: EmergencyQuiesce>(&mut self, device: &mut Q) -> Result<(), Q::Error> {
        if !self.quiesce_attempted {
            let mut first_error = None;
            for result in [
                device.mute_dac_and_pin(),
                device.stop_run(),
                device.disable_eapd(),
                device.disable_dma(),
                device.disable_bus_mastering(),
            ] {
                if first_error.is_none() {
                    first_error = result.err();
                }
            }
            if let Some(error) = first_error {
                return Err(error);
            }
            self.quiesce_attempted = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drv_audio_pipewire_spike::{PlaybackEndpoint, VirtualPcmEndpoint};

    #[test]
    fn declared_phase_one_buffer_and_deadline_contract_is_consistent() {
        assert_eq!(QUANTUM_BYTES, 1_920);
        assert_eq!(PERIOD_MICROS, 10_000);
        assert_eq!(BDL_ENTRIES * QUANTUM_FRAMES, 3_840);
        assert_eq!(QUEUED_MILLIS, 80);
        assert_eq!(EXECUTION_BUDGET_MICROS * 100 / PERIOD_MICROS, 20);
        assert_eq!(DEADLINE_MARGIN_PERCENT, 80);
        let queued_millis = QUEUED_MILLIS;
        let target_latency_millis = TARGET_LATENCY_MILLIS;
        assert!(queued_millis <= target_latency_millis);
    }

    #[test]
    fn fuchsia_gain_checksum_is_preserved_before_mandatory_protection() {
        let input = PcmQuantum::from_fn(|index| if index % 2 == 0 { 10_000 } else { 2_000 });
        let mut executor = FixedRtExecutor::default();
        let mut ring = ContinuousDmaRing::default();
        let report = executor.execute(&input, &mut ring).unwrap();

        let mut bytes = [0; QUANTUM_BYTES];
        for (sample, output) in input.samples().iter().zip(bytes.chunks_exact_mut(2)) {
            output.copy_from_slice(&sample.to_le_bytes());
        }
        let mut baseline = VirtualPcmEndpoint::default();
        baseline.write(&bytes).unwrap();
        assert_eq!(report.dsp_checksum, 2_880_000);
        assert_eq!(
            report.dsp_checksum,
            baseline.processed_sample_checksum(),
            "pre-protection RT DSP must remain equivalent to the virtual baseline"
        );
        assert_eq!(report.frame_position, 480);
        assert_eq!(report.protected_peak, DIGITAL_PEAK_CEILING);
        assert_eq!(ring.submitted_periods(), 1);
    }

    #[test]
    fn every_dma_entry_is_preallocated_silence_or_peak_limited() {
        let mut executor = FixedRtExecutor::default();
        let mut ring = ContinuousDmaRing::default();
        assert_eq!(ring.queued_frames(), 3_840);
        for index in 0..BDL_ENTRIES {
            assert_eq!(ring.entry_peak(index), 0);
            let input =
                PcmQuantum::from_fn(|sample| if sample % 2 == 0 { i16::MAX } else { i16::MIN });
            executor.execute(&input, &mut ring).unwrap();
        }
        for index in 0..BDL_ENTRIES {
            assert!(ring.entry_peak(index) <= DIGITAL_PEAK_CEILING);
        }
        let input = PcmQuantum::default();
        assert_eq!(
            executor.execute(&input, &mut ring),
            Err(DmaRingError::SlotStillOwnedByHardware)
        );

        let mut progress = DmaProgress::default();
        ring.complete(0, &mut progress, 1, QUANTUM_BYTES as u32, false)
            .unwrap();
        executor.execute(&input, &mut ring).unwrap();
        assert_eq!(ring.submitted_periods(), (BDL_ENTRIES + 1) as u64);
    }

    #[test]
    fn dc_blocker_rejects_a_constant_offset() {
        let input = PcmQuantum::from_fn(|_| 200);
        let mut executor = FixedRtExecutor::default();
        let mut ring = ContinuousDmaRing::default();
        let first = executor.execute(&input, &mut ring).unwrap();
        let mut last = first;
        for _ in 0..399 {
            let mut ring = ContinuousDmaRing::default();
            last = executor.execute(&input, &mut ring).unwrap();
        }
        assert!(last.protected_checksum.abs() < first.protected_checksum.abs() / 2);
    }

    #[test]
    fn lease_requires_both_rt_and_hardware_progress_and_latches() {
        let mut lease = ProgressLease::default();
        assert_eq!(lease.observe(1, 1_920), LeaseAction::Continue);
        assert_eq!(lease.observe(2, 1_920), LeaseAction::Continue);
        assert_eq!(lease.observe(3, 1_920), LeaseAction::Continue);
        assert_eq!(lease.observe(4, 1_920), LeaseAction::MuteAndPark);
        assert!(lease.is_latched());
        assert_eq!(lease.observe(5, 3_840), LeaseAction::MuteAndPark);
    }

    #[test]
    fn healthy_wraparound_never_expires_lease() {
        let mut lease = ProgressLease::default();
        let ring_bytes = (BDL_ENTRIES * QUANTUM_BYTES) as u32;
        for cycle in 1..=10_000_u64 {
            let lpib = ((cycle as usize * QUANTUM_BYTES) % ring_bytes as usize) as u32;
            assert_eq!(lease.observe(cycle, lpib), LeaseAction::Continue);
        }
    }

    #[test]
    fn alternating_rt_and_lpib_stalls_cannot_evade_lease() {
        let mut lease = ProgressLease::default();
        assert_eq!(lease.observe(1, 1_920), LeaseAction::Continue);
        assert_eq!(lease.observe(2, 1_920), LeaseAction::Continue);
        assert_eq!(lease.observe(2, 3_840), LeaseAction::Continue);
        assert_eq!(lease.observe(3, 3_840), LeaseAction::MuteAndPark);
    }

    #[test]
    fn one_or_two_progress_misses_recover_before_threshold() {
        let mut lease = ProgressLease::default();
        assert_eq!(lease.observe(1, 1_920), LeaseAction::Continue);
        assert_eq!(lease.observe(2, 1_920), LeaseAction::Continue);
        assert_eq!(lease.observe(3, 1_920), LeaseAction::Continue);
        assert_eq!(lease.observe(4, 3_840), LeaseAction::Continue);
        assert!(!lease.is_latched());
    }

    #[test]
    fn completion_accounting_rejects_lost_irq_stall_and_stream_fault() {
        let mut progress = DmaProgress::default();
        assert_eq!(progress.complete(1, 1_920, false), Ok(()));
        assert_eq!(progress.completions(), 1);

        let mut missing = DmaProgress::default();
        assert_eq!(
            missing.complete(0, 3_840, false),
            Err(DmaProgressError::MissingInterrupt)
        );
        assert_eq!(missing.complete(1, 3_840, false), Ok(()));

        let mut stalled = DmaProgress::default();
        stalled.complete(1, 1_920, false).unwrap();
        assert_eq!(
            stalled.complete(1, 1_920, false),
            Err(DmaProgressError::StalledPosition)
        );
        assert_eq!(stalled.complete(1, 3_840, false), Ok(()));

        let mut stream_fault = DmaProgress::default();
        assert_eq!(
            stream_fault.complete(1, 3_840, true),
            Err(DmaProgressError::StreamFault)
        );
        assert_eq!(
            stream_fault.complete(1, 3_840, false),
            Err(DmaProgressError::StreamFault)
        );
        assert_eq!(stream_fault.fault(), Some(DmaProgressError::StreamFault));
    }

    #[test]
    fn completed_slot_is_refilled_with_preallocated_silence_on_starvation() {
        let input = PcmQuantum::from_fn(|_| i16::MAX);
        let mut executor = FixedRtExecutor::default();
        let mut ring = ContinuousDmaRing::default();
        for _ in 0..BDL_ENTRIES {
            executor.execute(&input, &mut ring).unwrap();
        }
        assert_eq!(ring.entry_peak(0), DIGITAL_PEAK_CEILING);

        let mut progress = DmaProgress::default();
        ring.complete(0, &mut progress, 1, QUANTUM_BYTES as u32, false)
            .unwrap();
        ring.refill_silence(&mut progress).unwrap();
        assert_eq!(ring.entry_peak(0), 0);
        assert_eq!(progress.xruns(), 1);
        assert_eq!(ring.submitted_periods(), (BDL_ENTRIES + 1) as u64);
    }

    #[test]
    fn coalesced_completions_release_every_slot_across_wrap() {
        let input = PcmQuantum::default();
        let mut executor = FixedRtExecutor::default();
        let mut ring = ContinuousDmaRing::default();
        let mut progress = DmaProgress::default();
        for _ in 0..BDL_ENTRIES {
            executor.execute(&input, &mut ring).unwrap();
        }

        ring.complete(0, &mut progress, 7, 7 * QUANTUM_BYTES as u32, false)
            .unwrap();
        for _ in 0..7 {
            executor.execute(&input, &mut ring).unwrap();
        }
        ring.complete(7, &mut progress, 2, QUANTUM_BYTES as u32, false)
            .unwrap();
        executor.execute(&input, &mut ring).unwrap();
        executor.execute(&input, &mut ring).unwrap();
        assert_eq!(progress.completions(), 9);
    }

    #[test]
    fn watchdog_mutes_and_parks_once_in_safe_order() {
        #[derive(Default)]
        struct Device {
            steps: [u8; 5],
            count: usize,
        }
        impl Device {
            fn step(&mut self, value: u8) {
                self.steps[self.count] = value;
                self.count += 1;
            }
        }
        impl EmergencyQuiesce for Device {
            type Error = core::convert::Infallible;
            fn mute_dac_and_pin(&mut self) -> Result<(), Self::Error> {
                self.step(1);
                Ok(())
            }
            fn stop_run(&mut self) -> Result<(), Self::Error> {
                self.step(2);
                Ok(())
            }
            fn disable_eapd(&mut self) -> Result<(), Self::Error> {
                self.step(3);
                Ok(())
            }
            fn disable_dma(&mut self) -> Result<(), Self::Error> {
                self.step(4);
                Ok(())
            }
            fn disable_bus_mastering(&mut self) -> Result<(), Self::Error> {
                self.step(5);
                Ok(())
            }
        }

        let mut watchdog = StallWatchdog::default();
        let mut device = Device::default();
        for _ in 0..STALL_INTERVALS {
            watchdog.tick(0, 0, &mut device).unwrap();
        }
        assert_eq!(device.steps, [1, 2, 3, 4, 5]);
        watchdog.tick(1, 1_920, &mut device).unwrap();
        assert_eq!(device.count, 5);

        let mut fault_watchdog = StallWatchdog::default();
        let mut fault_device = Device::default();
        assert_eq!(
            fault_watchdog.fault(&mut fault_device),
            Ok(LeaseAction::MuteAndPark)
        );
        assert_eq!(fault_device.steps, [1, 2, 3, 4, 5]);
        assert_eq!(
            fault_watchdog.tick(1, 1_920, &mut fault_device),
            Ok(LeaseAction::MuteAndPark)
        );
        assert_eq!(fault_device.count, 5);
    }

    #[test]
    fn failed_mute_still_attempts_every_containment_step_and_retries() {
        #[derive(Default)]
        struct Device {
            attempts: [u8; 5],
            fail_mute: bool,
        }
        impl EmergencyQuiesce for Device {
            type Error = u8;
            fn mute_dac_and_pin(&mut self) -> Result<(), Self::Error> {
                self.attempts[0] += 1;
                if self.fail_mute { Err(1) } else { Ok(()) }
            }
            fn stop_run(&mut self) -> Result<(), Self::Error> {
                self.attempts[1] += 1;
                Ok(())
            }
            fn disable_eapd(&mut self) -> Result<(), Self::Error> {
                self.attempts[2] += 1;
                Ok(())
            }
            fn disable_dma(&mut self) -> Result<(), Self::Error> {
                self.attempts[3] += 1;
                Ok(())
            }
            fn disable_bus_mastering(&mut self) -> Result<(), Self::Error> {
                self.attempts[4] += 1;
                Ok(())
            }
        }

        let mut watchdog = StallWatchdog::default();
        let mut device = Device {
            fail_mute: true,
            ..Device::default()
        };
        for _ in 0..STALL_INTERVALS - 1 {
            watchdog.tick(0, 0, &mut device).unwrap();
        }
        assert_eq!(watchdog.tick(0, 0, &mut device), Err(1));
        assert_eq!(device.attempts, [1, 1, 1, 1, 1]);

        device.fail_mute = false;
        assert_eq!(
            watchdog.tick(0, 0, &mut device),
            Ok(LeaseAction::MuteAndPark)
        );
        assert_eq!(device.attempts, [2, 2, 2, 2, 2]);
    }
}
