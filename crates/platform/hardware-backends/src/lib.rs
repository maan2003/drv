#![forbid(unsafe_code)]

use drv_hardware::{Backend, Device, DmaConstraints, DmaDirection, Error, IrqEvent, Result};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    rc::Rc,
};

#[cfg(target_os = "linux")]
mod linux_vfio;
#[cfg(target_os = "linux")]
pub use linux_vfio::unconfined as unconfined_vfio;
#[cfg(target_os = "linux")]
pub use linux_vfio::{
    LinuxVfio, LinuxVfioError, LinuxVfioPciCapabilities, LinuxVfioPlatformCapabilities,
    LinuxVfioPlatformFdIdentities, LockedLinuxVfioPciCapabilities, LockedVfioEduReport,
    OpenedPciCoherent, run_locked_vfio_edu_mechanics,
};
#[cfg(target_os = "linux")]
pub fn monotonic_time_ns() -> Result<u64> {
    userspace_vfio::monotonic_time_ns().map_err(|_| Error::DeviceFault)
}
#[cfg(target_os = "linux")]
mod pci_control;
#[cfg(target_os = "linux")]
pub use pci_control::{PciConfigSnapshot, PciControl, PciControlError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    ReadU32 {
        region: u8,
        offset: usize,
        value: u32,
    },
    WriteU32 {
        region: u8,
        offset: usize,
        value: u32,
    },
    WriteDeviceAddress {
        region: u8,
        low: usize,
        high: Option<usize>,
        value: u64,
    },
    SyncForCpu {
        dma: u64,
        range: Range<usize>,
    },
    SyncForDevice {
        dma: u64,
        range: Range<usize>,
    },
}
pub type OperationLog = Rc<RefCell<Vec<Operation>>>;

/// All externally chosen response data consumed by a deterministic device
/// model. Tests normally start with a known handshake input and vary these
/// fields with proptest.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceResponseInput {
    pub register_reads: Vec<u32>,
    pub response_bytes: Vec<u8>,
    pub completions_per_poll: Vec<usize>,
    pub completion_order: Vec<usize>,
    pub stays_silent: bool,
}

#[derive(Clone)]
pub struct DeviceModel(Rc<RefCell<DeviceModelState>>);

struct DeviceModelState {
    dma_writes: VecDeque<(u64, Vec<u8>)>,
    register_reads: VecDeque<u32>,
    response_bytes: Vec<u8>,
    completions_per_poll: VecDeque<usize>,
    completion_order: Vec<usize>,
    stays_silent: bool,
}

impl DeviceModel {
    fn new(input: DeviceResponseInput) -> Self {
        Self(Rc::new(RefCell::new(DeviceModelState {
            dma_writes: VecDeque::new(),
            register_reads: input.register_reads.into(),
            response_bytes: input.response_bytes,
            completions_per_poll: input.completions_per_poll.into(),
            completion_order: input.completion_order,
            stays_silent: input.stays_silent,
        })))
    }

    /// Queue a device-side DMA write, applied before the next CPU DMA read.
    /// Mapping, direction and streaming ownership are checked by the backend.
    pub fn write_dma(&self, address: u64, bytes: Vec<u8>) {
        self.0.borrow_mut().dma_writes.push_back((address, bytes));
    }

    /// Number of response decisions not yet consumed by the driver.
    pub fn remaining_decisions(&self) -> usize {
        let state = self.0.borrow();
        state.register_reads.len() + state.completions_per_poll.len() + state.dma_writes.len()
    }
}

#[derive(Default)]
struct DoorbellDependency {
    descriptor: Range<u64>,
    doorbell_offset: usize,
    satisfied: bool,
}

/// Assertions attached to a recording backend for descriptor/doorbell tests.
#[derive(Clone)]
pub struct OrderingAssertions(Rc<RefCell<Vec<DoorbellDependency>>>);
impl OrderingAssertions {
    /// Require a fresh DMA write covering `descriptor` before each write to
    /// `doorbell_offset`. A violation makes the doorbell write fail closed.
    pub fn expect_descriptor_before_doorbell(
        &self,
        descriptor: Range<u64>,
        doorbell_offset: usize,
    ) {
        self.0.borrow_mut().push(DoorbellDependency {
            descriptor,
            doorbell_offset,
            satisfied: false,
        });
    }
}

const FIRST_IOVA: u64 = 0x1000_0000;
const DETERMINISTIC_MAX_DMA_ALLOCATION: usize = 1024 * 1024;
struct Dma {
    bytes: Vec<u8>,
    iova: u64,
    direction: DmaDirection,
    coherent: bool,
    ownership: Vec<DmaOwnership>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DmaOwnership {
    Cpu,
    CpuDirty,
    Device,
    DeviceDirty,
}
pub struct DeterministicBackend {
    generation: u64,
    next: u64,
    next_id: u64,
    dmas: HashMap<u64, Dma>,
    pending: bool,
    now: u64,
    live_regions: usize,
    live_irqs: usize,
    source: Option<(u64, usize)>,
    destination: Option<(u64, usize)>,
    count: usize,
    edu_buffer: Vec<u8>,
    operations: Option<OperationLog>,
    ordering: Option<OrderingAssertions>,
    cache_coherent: bool,
    failures: Option<FailureInjection>,
    region_len: usize,
    device_model: Option<DeviceModel>,
    resource_probe: Option<DeterministicResourceProbe>,
    registers: HashMap<(u8, usize), u32>,
    mt7921_activation_model: bool,
    ambiguous_interrupt_vectors: HashSet<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeterministicRelease {
    Interrupt,
    Dma,
    Region,
}

#[derive(Default)]
struct DeterministicResourceState {
    attempts: usize,
    fail_at: Option<usize>,
    live_regions: usize,
    live_dmas: usize,
    live_interrupts: usize,
    releases: Vec<DeterministicRelease>,
}

/// Observer and acquisition-failure control for deterministic ownership tests.
#[derive(Clone, Default)]
pub struct DeterministicResourceProbe(Rc<RefCell<DeterministicResourceState>>);

impl DeterministicResourceProbe {
    pub fn attempts(&self) -> usize {
        self.0.borrow().attempts
    }

    pub fn live_regions(&self) -> usize {
        self.0.borrow().live_regions
    }

    pub fn live_dmas(&self) -> usize {
        self.0.borrow().live_dmas
    }

    pub fn live_interrupts(&self) -> usize {
        self.0.borrow().live_interrupts
    }

    pub fn releases(&self) -> Vec<DeterministicRelease> {
        self.0.borrow().releases.clone()
    }

    fn acquire(&self) -> Result<()> {
        let mut state = self.0.borrow_mut();
        state.attempts += 1;
        if state.fail_at == Some(state.attempts) {
            Err(Error::DeviceFault)
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
struct FailureState {
    mmio_write: Option<(usize, u32)>,
    sync_for_device: bool,
    interrupt_open: bool,
    interrupt_disable: usize,
}
#[derive(Clone, Default)]
pub struct FailureInjection(Rc<RefCell<FailureState>>);
impl FailureInjection {
    /// Fail the next write matching this register and value, without applying it.
    pub fn fail_next_matching_mmio_write(&self, offset: usize, value: u32) {
        self.0.borrow_mut().mmio_write = Some((offset, value));
    }
    pub fn fail_next_sync_for_device(&self) {
        self.0.borrow_mut().sync_for_device = true;
    }
    pub fn fail_next_interrupt_disable(&self) {
        self.fail_interrupt_disable_attempts(1);
    }
    pub fn fail_interrupt_disable_attempts(&self, attempts: usize) {
        self.0.borrow_mut().interrupt_disable = attempts;
    }
    pub fn fail_next_interrupt_open(&self) {
        self.0.borrow_mut().interrupt_open = true;
    }
}
impl Default for DeterministicBackend {
    fn default() -> Self {
        Self {
            generation: 1,
            next: FIRST_IOVA,
            next_id: 1,
            dmas: HashMap::new(),
            pending: false,
            now: 0,
            live_regions: 0,
            live_irqs: 0,
            source: None,
            destination: None,
            count: 0,
            edu_buffer: vec![0; 4096],
            operations: None,
            ordering: None,
            cache_coherent: true,
            failures: None,
            region_len: 0x10_0000,
            device_model: None,
            resource_probe: None,
            registers: HashMap::new(),
            mt7921_activation_model: false,
            ambiguous_interrupt_vectors: HashSet::new(),
        }
    }
}
impl DeterministicBackend {
    pub fn device() -> Device<Self> {
        Device::from_backend(Self::default())
    }
    pub fn device_with_resource_probe(
        fail_at: Option<usize>,
    ) -> (Device<Self>, DeterministicResourceProbe) {
        let probe = DeterministicResourceProbe::default();
        probe.0.borrow_mut().fail_at = fail_at;
        let backend = Self {
            resource_probe: Some(probe.clone()),
            ..Self::default()
        };
        (Device::from_backend(backend), probe)
    }
    pub fn noncoherent_device() -> Device<Self> {
        Device::from_backend(Self {
            cache_coherent: false,
            ..Self::default()
        })
    }
    pub fn noncoherent_device_with_failures() -> (Device<Self>, FailureInjection) {
        let failures = FailureInjection::default();
        let backend = Self {
            cache_coherent: false,
            failures: Some(failures.clone()),
            ..Self::default()
        };
        (Device::from_backend(backend), failures)
    }
    pub fn recording_device() -> (Device<Self>, OperationLog) {
        let operations = Rc::new(RefCell::new(Vec::new()));
        let backend = Self {
            operations: Some(operations.clone()),
            ..Self::default()
        };
        (Device::from_backend(backend), operations)
    }
    /// Recording device whose small register model supplies MT7921 activation
    /// readbacks while retaining the exact operation trace.
    pub fn recording_mt7921_activation_device() -> (Device<Self>, OperationLog) {
        let operations = Rc::new(RefCell::new(Vec::new()));
        let backend = Self {
            operations: Some(operations.clone()),
            mt7921_activation_model: true,
            ..Self::default()
        };
        (Device::from_backend(backend), operations)
    }
    pub fn recording_mt7921_device_with_model(
        input: DeviceResponseInput,
    ) -> (Device<Self>, OperationLog, DeviceModel) {
        let operations = Rc::new(RefCell::new(Vec::new()));
        let model = DeviceModel::new(input);
        let backend = Self {
            operations: Some(operations.clone()),
            device_model: Some(model.clone()),
            mt7921_activation_model: true,
            ..Self::default()
        };
        (Device::from_backend(backend), operations, model)
    }

    pub fn recording_mt7921_activation_device_with_failures()
    -> (Device<Self>, OperationLog, FailureInjection) {
        let operations = Rc::new(RefCell::new(Vec::new()));
        let failures = FailureInjection::default();
        let backend = Self {
            operations: Some(operations.clone()),
            failures: Some(failures.clone()),
            mt7921_activation_model: true,
            ..Self::default()
        };
        (Device::from_backend(backend), operations, failures)
    }
    /// Recording device with a caller-sized BAR for drivers whose register
    /// windows exceed the compact default test region.
    pub fn recording_device_with_region_len(region_len: usize) -> (Device<Self>, OperationLog) {
        let operations = Rc::new(RefCell::new(Vec::new()));
        let backend = Self {
            operations: Some(operations.clone()),
            region_len,
            ..Self::default()
        };
        (Device::from_backend(backend), operations)
    }
    pub fn recording_noncoherent_device() -> (Device<Self>, OperationLog) {
        let operations = Rc::new(RefCell::new(Vec::new()));
        let backend = Self {
            operations: Some(operations.clone()),
            cache_coherent: false,
            ..Self::default()
        };
        (Device::from_backend(backend), operations)
    }
    /// Recording noncoherent backend whose register, DMA-response, and
    /// interrupt decisions come only from `input`.
    pub fn recording_noncoherent_device_with_model(
        input: DeviceResponseInput,
    ) -> (Device<Self>, OperationLog, DeviceModel) {
        let operations = Rc::new(RefCell::new(Vec::new()));
        let model = DeviceModel::new(input);
        let backend = Self {
            operations: Some(operations.clone()),
            cache_coherent: false,
            device_model: Some(model.clone()),
            ..Self::default()
        };
        (Device::from_backend(backend), operations, model)
    }
    pub fn recording_device_with_ordering_checks()
    -> (Device<Self>, OperationLog, OrderingAssertions) {
        let operations = Rc::new(RefCell::new(Vec::new()));
        let ordering = OrderingAssertions(Rc::new(RefCell::new(Vec::new())));
        let backend = Self {
            operations: Some(operations.clone()),
            ordering: Some(ordering.clone()),
            ..Self::default()
        };
        (Device::from_backend(backend), operations, ordering)
    }
    fn dma(&self, id: &u64) -> Result<&Dma> {
        self.dmas.get(id).ok_or(Error::StaleHandle)
    }
    fn dma_mut(&mut self, id: &u64) -> Result<&mut Dma> {
        self.dmas.get_mut(id).ok_or(Error::StaleHandle)
    }
    fn record_dma_write(&mut self, dma: &u64, range: Range<usize>) -> Result<()> {
        let iova = self.dma(dma)?.iova;
        if let Some(ordering) = &self.ordering {
            let written = iova + range.start as u64..iova + range.end as u64;
            for dependency in ordering.0.borrow_mut().iter_mut() {
                if written.start <= dependency.descriptor.start
                    && written.end >= dependency.descriptor.end
                {
                    dependency.satisfied = true;
                }
            }
        }
        Ok(())
    }
    fn apply_device_writes(&mut self) -> Result<()> {
        let Some(model) = self.device_model.clone() else {
            return Ok(());
        };
        while let Some((address, bytes)) = model.0.borrow_mut().dma_writes.pop_front() {
            let (id, range) = self
                .dmas
                .iter()
                .find_map(|(id, dma)| {
                    let offset = usize::try_from(address.checked_sub(dma.iova)?).ok()?;
                    let end = offset.checked_add(bytes.len())?;
                    (end <= dma.bytes.len()).then_some((*id, offset..end))
                })
                .ok_or(Error::OutOfBounds)?;
            self.device_accessible(id, range.clone())?;
            let dma = self.dma_mut(&id)?;
            if matches!(dma.direction, DmaDirection::ToDevice) {
                return Err(Error::Invalid);
            }
            dma.bytes[range].copy_from_slice(&bytes);
        }
        Ok(())
    }

    fn device_accessible(&self, dma: u64, range: Range<usize>) -> Result<()> {
        let dma = self.dmas.get(&dma).ok_or(Error::StaleHandle)?;
        if self.cache_coherent
            || dma.coherent
            || dma.ownership[range]
                .iter()
                .all(|owner| *owner == DmaOwnership::Device)
        {
            Ok(())
        } else {
            Err(Error::DeviceFault)
        }
    }
}

impl Backend for DeterministicBackend {
    type Region = u8;
    type Dma = u64;
    type Interrupt = u32;
    fn generation(&self) -> u64 {
        self.generation
    }
    fn is_cache_coherent(&self) -> bool {
        self.cache_coherent
    }
    fn open_region(&mut self, index: u8) -> Result<u8> {
        if index != 0 {
            return Err(Error::Invalid);
        }
        if let Some(probe) = &self.resource_probe {
            probe.acquire()?;
            probe.0.borrow_mut().live_regions += 1;
        }
        self.live_regions += 1;
        Ok(index)
    }
    fn region_len(&self, _: &u8) -> usize {
        self.region_len
    }
    fn read_u32(&mut self, region: &u8, offset: usize) -> Result<u32> {
        let value = self
            .device_model
            .as_ref()
            .and_then(|model| {
                let mut state = model.0.borrow_mut();
                (!state.stays_silent)
                    .then(|| state.register_reads.pop_front())
                    .flatten()
            })
            .unwrap_or_else(|| {
                if self.mt7921_activation_model {
                    match offset {
                        0x40140 => {
                            self.registers.get(&(*region, offset)).copied().unwrap_or(1) | (1 << 4)
                        }
                        0x40010 => 0,
                        _ => self.registers.get(&(*region, offset)).copied().unwrap_or(0),
                    }
                } else {
                    match offset {
                        0x24 => self.pending.into(),
                        _ => 0,
                    }
                }
            });
        if let Some(log) = &self.operations {
            log.borrow_mut().push(Operation::ReadU32 {
                region: *region,
                offset,
                value,
            });
        }
        Ok(value)
    }
    fn write_u32(&mut self, region: &u8, offset: usize, value: u32) -> Result<()> {
        if let Some(failures) = &self.failures {
            let mut failure = failures.0.borrow_mut();
            if failure.mmio_write == Some((offset, value)) {
                failure.mmio_write = None;
                return Err(Error::DeviceFault);
            }
        }
        if let Some(ordering) = &self.ordering
            && ordering
                .0
                .borrow()
                .iter()
                .any(|dependency| dependency.doorbell_offset == offset && !dependency.satisfied)
        {
            return Err(Error::DeviceFault);
        }
        let result = (|| match (offset, value) {
            (0x40000, value) => {
                self.edu_buffer[..4].copy_from_slice(&value.to_le_bytes());
                Ok(())
            }
            (0x80, _) => {
                self.source = None;
                Ok(())
            }
            (0x88, _) => {
                self.destination = None;
                Ok(())
            }
            (0x90, value) => {
                self.count = value as usize;
                Ok(())
            }
            (0x98, value) if value & 1 != 0 => {
                if value & 2 != 0 {
                    let destination = self.destination.ok_or(Error::Invalid)?;
                    let end = destination
                        .1
                        .checked_add(self.count)
                        .ok_or(Error::OutOfBounds)?;
                    if end
                        > self
                            .dmas
                            .get(&destination.0)
                            .ok_or(Error::StaleHandle)?
                            .bytes
                            .len()
                        || self.count > self.edu_buffer.len()
                    {
                        return Err(Error::OutOfBounds);
                    }
                    self.device_accessible(destination.0, destination.1..end)?;
                    let mut response = self.edu_buffer[..self.count].to_vec();
                    if let Some(model) = &self.device_model {
                        let state = model.0.borrow();
                        if !state.stays_silent && !state.response_bytes.is_empty() {
                            for (index, byte) in response.iter_mut().enumerate() {
                                let choice = state
                                    .completion_order
                                    .get(index % state.completion_order.len().max(1))
                                    .copied()
                                    .unwrap_or(index);
                                *byte = state.response_bytes[choice % state.response_bytes.len()];
                            }
                        }
                    }
                    self.dmas
                        .get_mut(&destination.0)
                        .ok_or(Error::StaleHandle)?
                        .bytes[destination.1..end]
                        .copy_from_slice(&response);
                    if !self.cache_coherent {
                        let dma = self
                            .dmas
                            .get_mut(&destination.0)
                            .ok_or(Error::StaleHandle)?;
                        if !dma.coherent {
                            dma.ownership[destination.1..end].fill(DmaOwnership::DeviceDirty);
                        }
                    }
                } else {
                    let source = self.source.ok_or(Error::Invalid)?;
                    let end = source.1.checked_add(self.count).ok_or(Error::OutOfBounds)?;
                    if end
                        > self
                            .dmas
                            .get(&source.0)
                            .ok_or(Error::StaleHandle)?
                            .bytes
                            .len()
                        || self.count > self.edu_buffer.len()
                    {
                        return Err(Error::OutOfBounds);
                    }
                    self.device_accessible(source.0, source.1..end)?;
                    self.edu_buffer[..self.count].copy_from_slice(
                        &self.dmas.get(&source.0).ok_or(Error::StaleHandle)?.bytes[source.1..end],
                    );
                }
                self.pending = value & 4 != 0;
                Ok(())
            }
            _ => Ok(()),
        })();
        if result.is_ok() {
            if self.mt7921_activation_model {
                let stored = if offset == 0xd4200 { 0 } else { value };
                self.registers.insert((*region, offset), stored);
            }
            if let Some(ordering) = &self.ordering {
                for dependency in ordering.0.borrow_mut().iter_mut() {
                    if dependency.doorbell_offset == offset {
                        dependency.satisfied = false;
                    }
                }
            }
            if let Some(log) = &self.operations {
                log.borrow_mut().push(Operation::WriteU32 {
                    region: *region,
                    offset,
                    value,
                });
            }
        }
        result
    }
    fn write_dma_address(
        &mut self,
        region: &u8,
        low: usize,
        high: Option<usize>,
        dma: &u64,
        offset: usize,
    ) -> Result<()> {
        let d = self.dma(dma)?;
        let value = d
            .iova
            .checked_add(offset as u64)
            .ok_or(Error::OutOfBounds)?;
        if let Some(log) = &self.operations {
            log.borrow_mut().push(Operation::WriteDeviceAddress {
                region: *region,
                low,
                high,
                value,
            });
        }
        match low {
            0x80 => self.source = Some((*dma, offset)),
            0x88 => self.destination = Some((*dma, offset)),
            _ => {}
        }
        Ok(())
    }
    fn dma_device_address(&self, dma: &u64, offset: usize) -> Result<u64> {
        self.dma(dma)?
            .iova
            .checked_add(offset as u64)
            .ok_or(Error::OutOfBounds)
    }
    fn alloc_dma(
        &mut self,
        size: usize,
        align: usize,
        direction: DmaDirection,
        coherent: bool,
    ) -> Result<u64> {
        if size > DETERMINISTIC_MAX_DMA_ALLOCATION {
            return Err(Error::Limit);
        }
        if let Some(probe) = &self.resource_probe {
            probe.acquire()?;
            probe.0.borrow_mut().live_dmas += 1;
        }
        let mask = (align as u64) - 1;
        self.next = self
            .next
            .checked_add(mask)
            .map(|x| x & !mask)
            .ok_or(Error::Limit)?;
        let id = self.next_id;
        self.next_id += 1;
        self.dmas.insert(
            id,
            Dma {
                // Make tests prove that hardware-api performs its documented
                // zero-initialization instead of inheriting it from a backend.
                bytes: vec![0xa5; size],
                iova: self.next,
                direction,
                coherent,
                ownership: vec![DmaOwnership::Cpu; size],
            },
        );
        self.next += size as u64;
        Ok(id)
    }
    fn alloc_dma_constrained(
        &mut self,
        size: usize,
        constraints: DmaConstraints,
        direction: DmaDirection,
        coherent: bool,
    ) -> Result<u64> {
        if constraints.max_segments == 0 || constraints.max_segment_size < size {
            return Err(Error::Limit);
        }
        let mask = constraints.alignment.checked_sub(1).ok_or(Error::Invalid)? as u64;
        let start = self
            .next
            .checked_add(mask)
            .map(|value| value & !mask)
            .ok_or(Error::Limit)?;
        let last = start.checked_add(size as u64 - 1).ok_or(Error::Limit)?;
        if last > constraints.max_device_address {
            return Err(Error::Limit);
        }
        self.alloc_dma(size, constraints.alignment, direction, coherent)
    }
    fn dma_read(&mut self, dma: &u64, r: Range<usize>, out: &mut [u8]) -> Result<()> {
        self.apply_device_writes()?;
        let d = self.dma(dma)?;
        if matches!(d.direction, DmaDirection::ToDevice) {
            return Err(Error::Invalid);
        }
        if !self.cache_coherent
            && !d.coherent
            && !d.ownership[r.clone()]
                .iter()
                .all(|owner| matches!(owner, DmaOwnership::Cpu | DmaOwnership::CpuDirty))
        {
            return Err(Error::DeviceFault);
        }
        out.copy_from_slice(&d.bytes[r]);
        Ok(())
    }
    fn dma_write(&mut self, dma: &u64, r: Range<usize>, bytes: &[u8]) -> Result<()> {
        self.record_dma_write(dma, r.clone())?;
        let d = self.dma_mut(dma)?;
        // hardware-api uses this backend primitive to initialize every fresh
        // allocation, including device-to-CPU buffers. Directional access is
        // enforced by the public typed DMA handles.
        d.bytes[r].copy_from_slice(bytes);
        Ok(())
    }
    fn streaming_cpu_dirty(&mut self, dma: &u64, range: Range<usize>) -> Result<()> {
        if !self.cache_coherent {
            let dma = self.dma_mut(dma)?;
            if !dma.coherent {
                dma.ownership[range].fill(DmaOwnership::CpuDirty);
            }
        }
        Ok(())
    }
    fn streaming_cpu_read(&mut self, dma: &u64, range: Range<usize>) -> Result<()> {
        let dma = self.dma(dma)?;
        if !self.cache_coherent
            && !dma.coherent
            && !dma.ownership[range]
                .iter()
                .all(|owner| matches!(owner, DmaOwnership::Cpu | DmaOwnership::CpuDirty))
        {
            return Err(Error::DeviceFault);
        }
        Ok(())
    }
    fn dma_read_once_u32(&mut self, dma: &u64, offset: usize) -> Result<u32> {
        self.apply_device_writes()?;
        let bytes: [u8; 4] = self.dma(dma)?.bytes[offset..offset + 4]
            .try_into()
            .map_err(|_| Error::OutOfBounds)?;
        Ok(u32::from_ne_bytes(bytes))
    }
    fn dma_write_once_u32(&mut self, dma: &u64, offset: usize, value: u32) -> Result<()> {
        self.record_dma_write(dma, offset..offset + 4)?;
        self.dma_mut(dma)?.bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
        Ok(())
    }
    fn sync_for_cpu(&mut self, dma: &u64, range: Range<usize>) -> Result<()> {
        let d = self.dma_mut(dma)?;
        if d.coherent || matches!(d.direction, DmaDirection::ToDevice) {
            Err(Error::Invalid)
        } else {
            if !d.ownership[range.clone()]
                .iter()
                .all(|owner| matches!(owner, DmaOwnership::Device | DmaOwnership::DeviceDirty))
            {
                return Err(Error::DeviceFault);
            }
            d.ownership[range.clone()].fill(DmaOwnership::Cpu);
            if let Some(log) = &self.operations {
                log.borrow_mut()
                    .push(Operation::SyncForCpu { dma: *dma, range });
            }
            Ok(())
        }
    }
    fn sync_for_device(&mut self, dma: &u64, range: Range<usize>) -> Result<()> {
        if let Some(failures) = &self.failures {
            let mut fail = failures.0.borrow_mut();
            if fail.sync_for_device {
                fail.sync_for_device = false;
                return Err(Error::DeviceFault);
            }
        }
        let d = self.dma_mut(dma)?;
        if d.coherent {
            Err(Error::Invalid)
        } else {
            if !d.ownership[range.clone()]
                .iter()
                .all(|owner| matches!(owner, DmaOwnership::Cpu | DmaOwnership::CpuDirty))
            {
                return Err(Error::DeviceFault);
            }
            d.ownership[range.clone()].fill(DmaOwnership::Device);
            if let Some(log) = &self.operations {
                log.borrow_mut()
                    .push(Operation::SyncForDevice { dma: *dma, range });
            }
            Ok(())
        }
    }
    fn open_interrupt(&mut self, vector: u32) -> Result<u32> {
        self.ambiguous_interrupt_vectors.insert(vector);
        if let Some(failures) = &self.failures {
            let mut fail = failures.0.borrow_mut();
            if fail.interrupt_open {
                fail.interrupt_open = false;
                return Err(Error::DeviceFault);
            }
        }
        if let Some(probe) = &self.resource_probe {
            probe.acquire()?;
            probe.0.borrow_mut().live_interrupts += 1;
        }
        self.live_irqs += 1;
        self.ambiguous_interrupt_vectors.remove(&vector);
        Ok(vector)
    }
    fn wait_interrupt(&mut self, i: &u32, deadline: u64) -> Result<Option<IrqEvent>> {
        self.now = self.now.max(deadline);
        if let Some(model) = &self.device_model {
            let mut state = model.0.borrow_mut();
            if state.stays_silent {
                return Ok(None);
            }
            let count = state.completions_per_poll.pop_front().unwrap_or(0);
            if count != 0 && *i == 0 {
                self.pending = false;
                return Ok(Some(IrqEvent {
                    vector: *i,
                    count: u64::try_from(count).unwrap_or(u64::MAX),
                    at_ns: self.now,
                }));
            }
            return Ok(None);
        }
        if self.pending && *i == 0 {
            self.pending = false;
            Ok(Some(IrqEvent {
                vector: *i,
                count: 1,
                at_ns: self.now,
            }))
        } else {
            Ok(None)
        }
    }
    fn wait_any(&mut self, interrupts: &[&u32], deadline: u64) -> Result<Vec<IrqEvent>> {
        if interrupts.is_empty() {
            return Err(Error::Invalid);
        }
        let Some(interrupt) = interrupts.iter().find(|interrupt| ***interrupt == 0) else {
            return Ok(Vec::new());
        };
        Ok(self
            .wait_interrupt(interrupt, deadline)?
            .into_iter()
            .collect())
    }
    fn disable_interrupt(&mut self, _: &u32) -> Result<()> {
        if let Some(failures) = &self.failures {
            let mut fail = failures.0.borrow_mut();
            if fail.interrupt_disable != 0 {
                fail.interrupt_disable -= 1;
                return Err(Error::DeviceFault);
            }
        }
        Ok(())
    }
    fn disable_interrupt_vector(&mut self, vector: u32) -> Result<()> {
        self.ambiguous_interrupt_vectors.insert(vector);
        if let Some(failures) = &self.failures {
            let mut fail = failures.0.borrow_mut();
            if fail.interrupt_disable != 0 {
                fail.interrupt_disable -= 1;
                return Err(Error::DeviceFault);
            }
        }
        self.ambiguous_interrupt_vectors.remove(&vector);
        Ok(())
    }
    fn reset(&mut self) -> Result<u64> {
        for vector in self
            .ambiguous_interrupt_vectors
            .iter()
            .copied()
            .collect::<Vec<_>>()
        {
            self.disable_interrupt_vector(vector)?;
        }
        self.generation += 1;
        self.dmas.clear();
        self.pending = false;
        Ok(self.generation)
    }
    fn release_region(&mut self, _: u8) {
        if let Some(probe) = &self.resource_probe {
            let mut state = probe.0.borrow_mut();
            state.live_regions -= 1;
            state.releases.push(DeterministicRelease::Region);
        }
        self.live_regions -= 1
    }
    fn release_dma(&mut self, id: u64) {
        if let Some(probe) = &self.resource_probe {
            let mut state = probe.0.borrow_mut();
            state.live_dmas -= 1;
            state.releases.push(DeterministicRelease::Dma);
        }
        self.dmas.remove(&id);
    }
    fn release_interrupt(&mut self, _: u32) {
        if let Some(probe) = &self.resource_probe {
            let mut state = probe.0.borrow_mut();
            state.live_interrupts -= 1;
            state.releases.push(DeterministicRelease::Interrupt);
        }
        self.live_irqs -= 1
    }
}

/// Exercises the QEMU `edu` DMA engine through only safe driver capabilities.
pub fn run_edu_sequence<B: Backend>(device: &Device<B>) -> Result<[u8; 4]> {
    let bar = device.open_region(0)?;
    let mut dma = device.alloc_streaming::<drv_hardware::Bidirectional>(4096, 4096)?;
    let source = [1, 2, 3, 4];
    dma.write(0, &source)?;
    dma.sync_for_device(0, source.len())?;
    bar.write_device_address(0x80, Some(0x84), dma.device_address(0)?)?;
    bar.write_u32(0x88, 0x40000)?;
    bar.write_u32(0x8c, 0)?;
    bar.write_u32(0x90, source.len() as u32)?;
    let interrupt = device.open_interrupt(0)?;
    bar.write_u32(0x98, 1 | 4)?;
    interrupt.wait_until(u64::MAX)?.ok_or(Error::Timeout)?;
    bar.write_u32(0x64, 0x100)?;
    bar.write_u32(0x80, 0x40000)?;
    bar.write_u32(0x84, 0)?;
    bar.write_device_address(0x88, Some(0x8c), dma.device_address(2048)?)?;
    bar.write_u32(0x98, 1 | 2 | 4)?;
    interrupt.wait_until(u64::MAX)?.ok_or(Error::Timeout)?;
    dma.sync_for_cpu(2048, source.len())?;
    let mut destination = [0; 4];
    dma.read(2048, &mut destination)?;
    // Deterministic tests assert the data result. The QEMU VFIO path also
    // executes both DMA directions; its backend verification is completed by
    // the VM suite's independent mapping/unmapping probe.
    if bar.read_u32(bar.len()) != Err(Error::OutOfBounds) {
        return Err(Error::DeviceFault);
    }
    device.reset()?;
    if bar.read_u32(0) != Err(Error::StaleHandle) {
        return Err(Error::DeviceFault);
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modeled_device_dma_checks_mapping_direction_and_streaming_ownership() {
        use drv_hardware::{FromDevice, ToDevice};
        let (device, _, model) =
            DeterministicBackend::recording_noncoherent_device_with_model(Default::default());
        let mut rx = device.alloc_coherent::<FromDevice>(4096, 4096).unwrap();
        let address = rx.device_address(0).unwrap().bits();
        model.write_dma(address + 3, vec![7, 8]);
        let mut bytes = [0; 2];
        rx.read(3, &mut bytes).unwrap();
        assert_eq!(bytes, [7, 8]);
        assert_eq!(model.remaining_decisions(), 0);
        model.write_dma(address + 4095, vec![1, 2]);
        assert_eq!(rx.read(0, &mut bytes), Err(Error::OutOfBounds));
        let tx = device.alloc_coherent::<ToDevice>(4096, 4096).unwrap();
        model.write_dma(tx.device_address(0).unwrap().bits(), vec![1]);
        assert_eq!(rx.read(0, &mut bytes), Err(Error::Invalid));
        let mut stream = device.alloc_streaming::<FromDevice>(4096, 4096).unwrap();
        stream.sync_for_cpu(0, 4096).unwrap();
        model.write_dma(stream.device_address(0).unwrap().bits(), vec![1]);
        assert_eq!(rx.read(0, &mut bytes), Err(Error::DeviceFault));
    }

    #[test]
    fn safe_sequence_covers_dma_mmio_irq_reset_and_bounds() {
        let dev = DeterministicBackend::device();
        assert_eq!(run_edu_sequence(&dev).unwrap(), [1, 2, 3, 4]);
    }

    #[test]
    fn hardware_api_zeroes_fresh_coherent_and_streaming_allocations() {
        let device = DeterministicBackend::device();
        let mut coherent = device
            .alloc_coherent::<drv_hardware::Bidirectional>(32, 8)
            .unwrap();
        let mut observed = [0xff; 32];
        coherent.read(0, &mut observed).unwrap();
        assert_eq!(observed, [0; 32]);

        let mut streaming = device
            .alloc_streaming::<drv_hardware::Bidirectional>(32, 8)
            .unwrap();
        streaming.sync_for_cpu(0, 32).unwrap();
        streaming.read(0, &mut observed).unwrap();
        assert_eq!(observed, [0; 32]);

        let mut from_device = device
            .alloc_coherent::<drv_hardware::FromDevice>(32, 8)
            .unwrap();
        from_device.read(0, &mut observed).unwrap();
        assert_eq!(observed, [0; 32]);

        let mut from_device = device
            .alloc_streaming::<drv_hardware::FromDevice>(32, 8)
            .unwrap();
        from_device.sync_for_cpu(0, 32).unwrap();
        from_device.read(0, &mut observed).unwrap();
        assert_eq!(observed, [0; 32]);
    }

    #[test]
    fn rejects_misaligned_mmio_and_foreign_device_addresses() {
        let first = DeterministicBackend::device();
        let second = DeterministicBackend::device();
        let bar = first.open_region(0).unwrap();
        assert_eq!(bar.read_u32(1), Err(Error::Invalid));
        assert_eq!(bar.write_u32(2, 0), Err(Error::Invalid));
        let own_dma = first
            .alloc_coherent::<drv_hardware::Bidirectional>(16, 4)
            .unwrap();
        assert_eq!(
            bar.write_device_address(0x81, Some(0x84), own_dma.device_address(0).unwrap()),
            Err(Error::Invalid)
        );
        let dma = second
            .alloc_coherent::<drv_hardware::Bidirectional>(16, 4)
            .unwrap();
        assert_eq!(
            bar.write_device_address(0x80, Some(0x84), dma.device_address(0).unwrap()),
            Err(Error::Invalid)
        );
    }

    #[test]
    fn malformed_dma_ranges_return_errors_instead_of_panicking() {
        let device = DeterministicBackend::device();
        let bar = device.open_region(0).unwrap();
        let dma = device
            .alloc_coherent::<drv_hardware::Bidirectional>(16, 4)
            .unwrap();
        bar.write_device_address(0x80, Some(0x84), dma.device_address(8).unwrap())
            .unwrap();
        bar.write_u32(0x88, 0x40000).unwrap();
        bar.write_u32(0x90, u32::MAX).unwrap();
        assert_eq!(bar.write_u32(0x98, 1), Err(Error::OutOfBounds));
        bar.write_u32(0x80, 0x40000).unwrap();
        bar.write_device_address(0x88, Some(0x8c), dma.device_address(8).unwrap())
            .unwrap();
        assert_eq!(bar.write_u32(0x98, 1 | 2), Err(Error::OutOfBounds));
    }

    #[test]
    fn recording_mode_accepts_and_orders_generic_register_operations() {
        let (device, operations) = DeterministicBackend::recording_device();
        let bar = device.open_region(0).unwrap();
        let dma = device
            .alloc_coherent::<drv_hardware::Bidirectional>(64, 64)
            .unwrap();
        bar.write_u32(0x100, 7).unwrap();
        let address = dma.device_address_at(8).unwrap();
        assert_eq!(address.bits(), FIRST_IOVA + 8);
        assert_eq!(address.lo32(), (FIRST_IOVA + 8) as u32);
        assert_eq!(address.hi32(), 0);
        assert!(matches!(
            dma.device_address_at(dma.len()),
            Err(Error::OutOfBounds)
        ));
        bar.write_device_address(0x120, Some(0x124), address)
            .unwrap();
        assert_eq!(
            operations.borrow().as_slice(),
            &[
                Operation::WriteU32 {
                    region: 0,
                    offset: 0x100,
                    value: 7
                },
                Operation::WriteDeviceAddress {
                    region: 0,
                    low: 0x120,
                    high: Some(0x124),
                    value: FIRST_IOVA + 8,
                },
            ]
        );
    }

    #[test]
    fn mmio_slices_share_mapping_and_enforce_nested_bounds() {
        let (device, operations) = DeterministicBackend::recording_device();
        let bar = device.open_region(0).unwrap();
        let block = bar.slice(0x200, 0x40).unwrap();
        let register = block.slice(0x10, 4).unwrap();

        register.write_u32(0, 7).unwrap();
        assert_eq!(register.write_u32(4, 8), Err(Error::OutOfBounds));
        assert!(matches!(bar.slice(bar.len(), 1), Err(Error::OutOfBounds)));
        assert_eq!(
            operations.borrow().as_slice(),
            &[Operation::WriteU32 {
                region: 0,
                offset: 0x210,
                value: 7,
            }]
        );

        drop(bar);
        register.write_u32(0, 9).unwrap();
        device.reset().unwrap();
        assert_eq!(register.write_u32(0, 10), Err(Error::StaleHandle));
    }

    #[test]
    fn translated_mmio_maps_every_operation_and_fails_closed() {
        fn translate(offset: usize) -> Option<usize> {
            match offset {
                0x100 => Some(0x100),
                0x1000 => Some(0x200),
                0x2000 => Some(0x400),
                0x3000 => None,
                0x4000 => Some(1),
                0x5000 => Some(0x2000),
                _ => Some(offset),
            }
        }

        let (device, operations) = DeterministicBackend::recording_device_with_region_len(0x2000);
        let raw = device.open_region(0).unwrap();
        assert!(matches!(
            raw.slice(0, raw.len())
                .unwrap()
                .map_offsets(translate)
                .unwrap()
                .map_offsets(translate),
            Err(Error::Invalid)
        ));
        let translated = raw
            .slice(0, raw.len())
            .unwrap()
            .map_offsets(translate)
            .unwrap();
        let dma = device
            .alloc_coherent::<drv_hardware::Bidirectional>(64, 8)
            .unwrap();

        translated.write_u32(0x100, 1).unwrap();
        translated.write_u32(0x1000, 2).unwrap();
        translated.write_u32(0x2000, 3).unwrap();
        translated
            .write_device_address(0x1000, Some(0x2000), dma.device_address(0).unwrap())
            .unwrap();
        assert_eq!(translated.write_u32(0x3000, 4), Err(Error::OutOfBounds));
        assert_eq!(translated.write_u32(0x4000, 4), Err(Error::Invalid));
        assert_eq!(translated.write_u32(0x5000, 4), Err(Error::OutOfBounds));
        assert_eq!(translated.slice(0, 4).err(), Some(Error::Invalid));
        assert_eq!(
            operations.borrow().as_slice(),
            &[
                Operation::WriteU32 {
                    region: 0,
                    offset: 0x100,
                    value: 1,
                },
                Operation::WriteU32 {
                    region: 0,
                    offset: 0x200,
                    value: 2,
                },
                Operation::WriteU32 {
                    region: 0,
                    offset: 0x400,
                    value: 3,
                },
                Operation::WriteDeviceAddress {
                    region: 0,
                    low: 0x200,
                    high: Some(0x400),
                    value: FIRST_IOVA,
                },
            ]
        );

        device.reset().unwrap();
        assert_eq!(translated.write_u32(0x100, 5), Err(Error::StaleHandle));
    }

    #[test]
    fn dma_splits_preserve_one_backend_allocation_and_contiguous_addresses() {
        let device = DeterministicBackend::device();
        let coherent = device
            .alloc_coherent::<drv_hardware::Bidirectional>(64, 16)
            .unwrap();
        let (mut left, mut right) = coherent.split_at(32).unwrap();
        assert_eq!(left.len(), 32);
        assert_eq!(right.len(), 32);
        assert_eq!(left.device_address(0).unwrap().bits(), FIRST_IOVA);
        assert_eq!(right.device_address(0).unwrap().bits(), FIRST_IOVA + 32);
        left.write(31, &[1]).unwrap();
        right.write(0, &[2]).unwrap();
        drop(left);
        let mut observed = [0; 1];
        right.read(0, &mut observed).unwrap();
        assert_eq!(observed, [2]);

        let streaming = device
            .alloc_streaming::<drv_hardware::Bidirectional>(64, 16)
            .unwrap();
        let (mut left, mut right) = streaming.split_at(16).unwrap();
        left.write(0, &[3]).unwrap();
        right.write(0, &[4]).unwrap();
        left.sync_for_device(0, 1).unwrap();
        right.sync_for_device(0, 1).unwrap();
        right.sync_for_cpu(0, 1).unwrap();
        right.read(0, &mut observed).unwrap();
        assert_eq!(observed, [4]);

        let unaligned = device
            .alloc_coherent::<drv_hardware::Bidirectional>(64, 16)
            .unwrap();
        assert!(matches!(unaligned.split_at(8), Err(Error::Invalid)));
    }

    #[test]
    fn typed_and_once_dma_accesses_check_alignment_and_preserve_values() {
        let device = DeterministicBackend::device();
        let mut coherent = device
            .alloc_coherent::<drv_hardware::Bidirectional>(16, 4)
            .unwrap();
        coherent.write_pod(4, 0x1122_3344_u32).unwrap();
        assert_eq!(coherent.read_pod::<u32>(4).unwrap(), 0x1122_3344);
        coherent.write_once(8, 0xaabb_ccdd_u32).unwrap();
        assert_eq!(coherent.read_once::<u32>(8).unwrap(), 0xaabb_ccdd);
        assert_eq!(coherent.read_pod::<u32>(2), Err(Error::Invalid));
        assert_eq!(coherent.write_once(14, 1_u32), Err(Error::Invalid));

        let mut streaming = device
            .alloc_streaming::<drv_hardware::Bidirectional>(16, 4)
            .unwrap();
        streaming.write_pod(0, 0x1234_5678_u32).unwrap();
        assert_eq!(streaming.read_pod::<u32>(0).unwrap(), 0x1234_5678);
        streaming.sync_for_device(0, 4).unwrap();
        streaming.write_once(4, 0x8765_4321_u32).unwrap();
        assert_eq!(streaming.read_once::<u32>(4).unwrap(), 0x8765_4321);
    }

    #[test]
    fn recording_backend_rejects_doorbell_before_declared_descriptor_write() {
        let (device, operations, ordering) =
            DeterministicBackend::recording_device_with_ordering_checks();
        let bar = device.open_region(0).unwrap();
        let mut descriptors = device
            .alloc_coherent::<drv_hardware::ToDevice>(16, 4)
            .unwrap();
        let descriptor = descriptors.device_address(4).unwrap().bits();
        ordering.expect_descriptor_before_doorbell(descriptor..descriptor + 4, 0x100);

        assert_eq!(bar.write_u32(0x100, 1), Err(Error::DeviceFault));
        assert!(operations.borrow().is_empty());
        descriptors.write(4, &[1, 2, 3, 4]).unwrap();
        bar.write_u32(0x100, 1).unwrap();
        assert_eq!(operations.borrow().len(), 1);
        assert_eq!(bar.write_u32(0x100, 2), Err(Error::DeviceFault));
        assert_eq!(operations.borrow().len(), 1);
    }

    #[test]
    fn coherent_streaming_syncs_are_noops_but_noncoherent_syncs_transition_ownership() {
        let (coherent, operations) = DeterministicBackend::recording_device();
        assert!(coherent.is_cache_coherent());
        let mut dma = coherent
            .alloc_streaming::<drv_hardware::Bidirectional>(16, 4)
            .unwrap();
        dma.write(0, &[1]).unwrap();
        dma.sync_for_device(0, 1).unwrap();
        dma.sync_for_cpu(0, 1).unwrap();
        assert!(operations.borrow().is_empty());

        let (noncoherent, operations) = DeterministicBackend::recording_noncoherent_device();
        assert!(!noncoherent.is_cache_coherent());
        let mut dma = noncoherent
            .alloc_streaming::<drv_hardware::Bidirectional>(16, 4)
            .unwrap();
        operations.borrow_mut().clear();
        dma.write(0, &[1]).unwrap();
        dma.sync_for_device(0, 1).unwrap();
        dma.sync_for_cpu(0, 1).unwrap();
        assert_eq!(
            operations.borrow().as_slice(),
            &[
                Operation::SyncForDevice {
                    dma: 1,
                    range: 0..1
                },
                Operation::SyncForCpu {
                    dma: 1,
                    range: 0..1
                },
            ]
        );
    }

    #[test]
    fn noncoherent_backend_rejects_unsynchronized_device_and_cpu_access() {
        let (device, operations) = DeterministicBackend::recording_noncoherent_device();
        let bar = device.open_region(0).unwrap();
        let mut source = device
            .alloc_streaming::<drv_hardware::Bidirectional>(16, 4)
            .unwrap();
        bar.write_device_address(0x80, Some(0x84), source.device_address(0).unwrap())
            .unwrap();
        bar.write_u32(0x90, 1).unwrap();
        source.write(0, &[7]).unwrap();
        assert_eq!(bar.write_u32(0x98, 1), Err(Error::DeviceFault));
        source.sync_for_device(0, 1).unwrap();
        bar.write_u32(0x98, 1).unwrap();

        let mut destination = device
            .alloc_streaming::<drv_hardware::FromDevice>(16, 4)
            .unwrap();
        bar.write_u32(0x80, 0x40000).unwrap();
        bar.write_device_address(0x88, Some(0x8c), destination.device_address(0).unwrap())
            .unwrap();
        bar.write_u32(0x98, 1 | 2).unwrap();
        let mut byte = [0];
        assert_eq!(destination.read(0, &mut byte), Err(Error::DeviceFault));
        destination.sync_for_cpu(0, 1).unwrap();
        destination.read(0, &mut byte).unwrap();
        assert_eq!(bar.write_u32(0x98, 1 | 2), Err(Error::DeviceFault));
        destination.prepare_for_device(0, 1).unwrap();
        bar.write_u32(0x98, 1 | 2).unwrap();
        let operations = operations.borrow();
        let cpu = operations
            .iter()
            .rposition(|operation| matches!(operation, Operation::SyncForCpu { dma: 2, range } if range == &(0..1)))
            .unwrap();
        let device = operations
            .iter()
            .rposition(|operation| matches!(operation, Operation::SyncForDevice { dma: 2, range } if range == &(0..1)))
            .unwrap();
        assert!(cpu < device);
    }

    #[test]
    fn constrained_dma_enforces_address_alignment_and_segment_limits() {
        let device = DeterministicBackend::device();
        let low32 = DmaConstraints {
            alignment: 4096,
            max_device_address: u32::MAX.into(),
            max_segment_size: 4096,
            max_segments: 1,
        };
        let dma = device
            .alloc_coherent_with_constraints::<drv_hardware::Bidirectional>(4096, low32)
            .unwrap();
        assert!(dma.device_address(0).is_ok());

        let below_backend_iova = DmaConstraints {
            max_device_address: 0x0fff_ffff,
            ..low32
        };
        assert!(matches!(
            device.alloc_coherent_with_constraints::<drv_hardware::Bidirectional>(
                4096,
                below_backend_iova
            ),
            Err(Error::Limit)
        ));
        let short_segment = DmaConstraints {
            max_segment_size: 2048,
            ..low32
        };
        assert!(matches!(
            device.alloc_coherent_with_constraints::<drv_hardware::Bidirectional>(
                4096,
                short_segment
            ),
            Err(Error::Limit)
        ));
    }

    #[test]
    fn wait_any_identifies_ready_vector_and_rejects_foreign_sets() {
        let device = DeterministicBackend::device();
        let other = DeterministicBackend::device();
        let unrelated = device.open_interrupt(4).unwrap();
        let ready = device.open_interrupt(0).unwrap();
        let foreign = other.open_interrupt(0).unwrap();
        let bar = device.open_region(0).unwrap();
        let mut dma = device
            .alloc_streaming::<drv_hardware::ToDevice>(4, 4)
            .unwrap();
        dma.write(0, &[1]).unwrap();
        dma.sync_for_device(0, 1).unwrap();
        bar.write_device_address(0x80, Some(0x84), dma.device_address(0).unwrap())
            .unwrap();
        bar.write_u32(0x90, 1).unwrap();
        bar.write_u32(0x98, 1 | 4).unwrap();
        let set = drv_hardware::Interrupt::wait_any(&[&unrelated, &ready], 10).unwrap();
        assert_eq!(set.events()[0].vector, 0);
        assert_eq!(
            drv_hardware::Interrupt::wait_any(&[&ready, &foreign], 10),
            Err(Error::Invalid)
        );
    }
}
