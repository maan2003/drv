//! Device-lifetime MAC preparation. Operation-specific radio commands are
//! separate: initialization does not authorize channel changes or scanning.

use drv_hardware::{Backend, MmioRegion};
use mt7921_core::{
    PassiveMacMmioOperation, passive_mac_bar_offset, passive_mac_mmio_plan,
    passive_mac_source_rmw_value, validate_passive_mac_bar_read,
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub(super) struct MacPreparation {
    remaining: VecDeque<PassiveMacMmioOperation>,
    waiting: Option<(u32, u32, Instant)>,
    failed: bool,
}

impl MacPreparation {
    pub fn new() -> Self {
        Self {
            remaining: passive_mac_mmio_plan().into(),
            waiting: None,
            failed: false,
        }
    }

    /// One source RMW, WTBL publication, or busy observation per owner turn.
    /// A failed/partially executed plan cannot be restarted without containment.
    pub fn drive<B: Backend>(
        &mut self,
        bar: &MmioRegion<B>,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        if self.failed {
            return Err(zx::Status::BAD_STATE);
        }
        let result = self.step(bar, now);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn step<B: Backend>(&mut self, bar: &MmioRegion<B>, now: Instant) -> Result<bool, zx::Status> {
        if let Some((address, busy_mask, deadline)) = self.waiting {
            if now >= deadline {
                return Err(zx::Status::TIMED_OUT);
            }
            let offset = passive_mac_bar_offset(address).map_err(|_| zx::Status::INTERNAL)?;
            let value = bar.read_u32(offset).map_err(|_| zx::Status::IO)?;
            validate_passive_mac_bar_read(address, value)
                .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
            if value & busy_mask != 0 {
                return Ok(false);
            }
            self.waiting = None;
            return Ok(true);
        }
        let Some(operation) = self.remaining.pop_front() else {
            return Ok(false);
        };
        match operation {
            PassiveMacMmioOperation::Rmw {
                address,
                mask,
                value,
            } => {
                let offset = passive_mac_bar_offset(address).map_err(|_| zx::Status::INTERNAL)?;
                let initial = bar.read_u32(offset).map_err(|_| zx::Status::IO)?;
                validate_passive_mac_bar_read(address, initial)
                    .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
                // Source mt76_mmio_rmw has no immediate equality readback.
                bar.write_u32(
                    offset,
                    passive_mac_source_rmw_value(initial, mask, value & mask),
                )
                .map_err(|_| zx::Status::IO)?;
            }
            PassiveMacMmioOperation::WtblClear {
                address,
                value,
                busy_mask,
                timeout_us,
                ..
            } => {
                let offset = passive_mac_bar_offset(address).map_err(|_| zx::Status::INTERNAL)?;
                self.waiting = Some((
                    address,
                    busy_mask,
                    now + Duration::from_micros(u64::from(timeout_us)),
                ));
                bar.write_u32(offset, value).map_err(|_| zx::Status::IO)?;
            }
        }
        Ok(true)
    }

    pub fn complete(&self) -> bool {
        !self.failed && self.remaining.is_empty() && self.waiting.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OwnedHardwareResources;
    use drv_hardware_backends::{DeterministicBackend, Operation};

    #[test]
    fn plan_advances_in_bounded_turns_without_repeating_completed_writes() {
        let (device, log) = DeterministicBackend::recording_mt7921_activation_device();
        let (resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        let mut prep = MacPreparation::new();
        let now = Instant::now();
        let writes = passive_mac_mmio_plan().len();
        for _ in 0..writes + 20 {
            let before = log.borrow().len();
            assert!(prep.drive(&resources.bar0, now).unwrap());
            assert!(log.borrow().len() - before <= 2);
        }
        assert!(prep.complete());
        let before = log.borrow().len();
        assert!(!prep.drive(&resources.bar0, now).unwrap());
        assert_eq!(log.borrow().len(), before);
        assert_eq!(
            log.borrow()
                .iter()
                .filter(|op| matches!(op, Operation::WriteU32 { .. }))
                .count(),
            writes
        );
    }

    #[test]
    fn busy_wait_keeps_original_deadline_and_fault_is_terminal() {
        let (device, log) = DeterministicBackend::recording_mt7921_activation_device();
        let (resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        let mut prep = MacPreparation::new();
        let now = Instant::now();
        for _ in 0..4 {
            prep.drive(&resources.bar0, now).unwrap();
        }
        let offset = passive_mac_bar_offset(0x820d_4230).unwrap();
        resources.bar0.write_u32(offset, 1 << 31).unwrap();
        assert!(
            !prep
                .drive(&resources.bar0, now + Duration::from_millis(4))
                .unwrap()
        );
        assert_eq!(
            prep.drive(&resources.bar0, now + Duration::from_millis(5)),
            Err(zx::Status::TIMED_OUT)
        );
        let before = log.borrow().len();
        assert_eq!(prep.drive(&resources.bar0, now), Err(zx::Status::BAD_STATE));
        assert_eq!(log.borrow().len(), before);
        assert!(!prep.complete());
    }
}
