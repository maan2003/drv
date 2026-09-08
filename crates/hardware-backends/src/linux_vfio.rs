//! Production Linux VFIO platform backend.

use drv_hardware::{Backend, DmaConstraints, DmaDirection, Error, IrqEvent, Result};
use std::{
    collections::HashMap,
    fmt,
    fs::{File, OpenOptions},
    ops::Range,
    path::Path,
    sync::{
        Arc,
        atomic::{Ordering, fence},
    },
};
use userspace_vfio::{
    AnonymousMapping, DeviceMapping, DmaBrokerCommand, DmaMapping, Ioas, RegionMapping, VfioIrq,
    dma_broker_uapi as broker,
};

const PAGE: usize = 4096;
const FIRST_IOVA: u64 = 0x0100_0000;

#[derive(Debug)]
pub enum LinuxVfioError {
    OpenDevice(std::io::Error),
    OpenIommufd(std::io::Error),
    Setup(String),
    DmaBrokerUnavailable(String),
}
impl fmt::Display for LinuxVfioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenDevice(error) => write!(f, "open VFIO device: {error}"),
            Self::OpenIommufd(error) => write!(f, "open /dev/iommu: {error}"),
            Self::Setup(error) => write!(f, "initialize coherent VFIO/iommufd device: {error}"),
            Self::DmaBrokerUnavailable(error) => {
                write!(
                    f,
                    "VFIO device does not provide DMA broker feature: {error}"
                )
            }
        }
    }
}
impl std::error::Error for LinuxVfioError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flavor {
    Coherent,
    Broker,
}

enum DmaMemory {
    Ioas(DmaMapping),
    BrokerCoherent(DeviceMapping),
    BrokerStreaming(AnonymousMapping),
}
struct Dma {
    memory: DmaMemory,
    iova: u64,
    len: usize,
    direction: DmaDirection,
    broker_handle: Option<u32>,
}

/// A VFIO cdev backend. Use `open_coherent` only after the platform has proved
/// DMA cache coherency; use `open_broker` for a non-coherent platform device.
pub struct LinuxVfio {
    device: Arc<File>,
    // Kept alive until every mapping is revoked. Broker flavor must never own one.
    iommu: Option<Arc<File>>,
    ioas: Option<Ioas>,
    flavor: Flavor,
    generation: u64,
    next_id: u64,
    next_iova: u64,
    regions: HashMap<u64, RegionMapping>,
    dmas: HashMap<u64, Dma>,
    interrupts: HashMap<u64, (u32, VfioIrq)>,
}

impl LinuxVfio {
    pub fn open_coherent(path: impl AsRef<Path>) -> std::result::Result<Self, LinuxVfioError> {
        let device = Arc::new(open_device(path)?);
        let iommu = Arc::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/iommu")
                .map_err(LinuxVfioError::OpenIommufd)?,
        );
        Self::initialize_coherent(device, iommu, |device, iommu| {
            userspace_vfio::bind_iommufd(device, iommu)?;
            let ioas = userspace_vfio::allocate_ioas(iommu)?;
            userspace_vfio::attach_ioas(device, ioas.id())?;
            Ok(ioas)
        })
    }

    pub fn open_broker(path: impl AsRef<Path>) -> std::result::Result<Self, LinuxVfioError> {
        let device = Arc::new(open_device(path)?);
        Self::initialize_broker(device, userspace_vfio::probe_dma_broker)
    }

    fn initialize_coherent(
        device: Arc<File>,
        iommu: Arc<File>,
        setup: impl FnOnce(&File, &Arc<File>) -> std::result::Result<Ioas, String>,
    ) -> std::result::Result<Self, LinuxVfioError> {
        let ioas = setup(&device, &iommu).map_err(LinuxVfioError::Setup)?;
        Ok(Self::new(device, Flavor::Coherent, Some(iommu), Some(ioas)))
    }

    fn initialize_broker(
        device: Arc<File>,
        probe: impl FnOnce(&File) -> std::result::Result<(), String>,
    ) -> std::result::Result<Self, LinuxVfioError> {
        probe(&device).map_err(LinuxVfioError::DmaBrokerUnavailable)?;
        Ok(Self::new(device, Flavor::Broker, None, None))
    }

    fn new(
        device: Arc<File>,
        flavor: Flavor,
        iommu: Option<Arc<File>>,
        ioas: Option<Ioas>,
    ) -> Self {
        Self {
            device,
            iommu,
            ioas,
            flavor,
            generation: 1,
            next_id: 1,
            next_iova: FIRST_IOVA,
            regions: HashMap::new(),
            dmas: HashMap::new(),
            interrupts: HashMap::new(),
        }
    }

    fn id(&mut self) -> Result<u64> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or(Error::Limit)?;
        Ok(id)
    }

    fn dma(&self, id: &u64) -> Result<&Dma> {
        self.dmas.get(id).ok_or(Error::StaleHandle)
    }
    fn dma_mut(&mut self, id: &u64) -> Result<&mut Dma> {
        self.dmas.get_mut(id).ok_or(Error::StaleHandle)
    }

    fn broker(&self, command: DmaBrokerCommand) -> Result<DmaBrokerCommand> {
        userspace_vfio::dma_broker_command(&self.device, command).map_err(|_| Error::DeviceFault)
    }

    fn release_dma_resource(&self, mut dma: Dma) {
        match &mut dma.memory {
            DmaMemory::Ioas(mapping) => {
                let _ = mapping.teardown();
            }
            DmaMemory::BrokerCoherent(mapping) => {
                // FREE is required to fail while the shared mapping exists.
                let _ = mapping.teardown();
                if let Some(handle) = dma.broker_handle {
                    let _ = self.broker(DmaBrokerCommand {
                        operation: broker::FREE,
                        handle,
                        ..Default::default()
                    });
                }
            }
            DmaMemory::BrokerStreaming(mapping) => {
                if let Some(handle) = dma.broker_handle {
                    let _ = self.broker(DmaBrokerCommand {
                        operation: broker::UNMAP,
                        handle,
                        ..Default::default()
                    });
                }
                let _ = mapping.teardown();
            }
        }
    }

    fn revoke_all(&mut self) {
        let interrupts = std::mem::take(&mut self.interrupts);
        drop(interrupts);
        for (_, dma) in std::mem::take(&mut self.dmas) {
            self.release_dma_resource(dma);
        }
    }
}

fn open_device(path: impl AsRef<Path>) -> std::result::Result<File, LinuxVfioError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(LinuxVfioError::OpenDevice)
}

fn checked_allocation(size: usize, constraints: DmaConstraints) -> Result<usize> {
    if size == 0 || constraints.alignment == 0 || !constraints.alignment.is_power_of_two() {
        return Err(Error::Invalid);
    }
    let mapped_len = size.checked_next_multiple_of(PAGE).ok_or(Error::Limit)?;
    if constraints.max_segments == 0 || constraints.max_segment_size < size {
        return Err(Error::Limit);
    }
    Ok(mapped_len)
}

fn aligned(value: u64, alignment: usize) -> Result<u64> {
    let alignment = u64::try_from(alignment.max(PAGE)).map_err(|_| Error::Limit)?;
    let mask = alignment - 1;
    value
        .checked_add(mask)
        .map(|v| v & !mask)
        .ok_or(Error::Limit)
}

fn direction_number(direction: DmaDirection) -> u32 {
    match direction {
        DmaDirection::ToDevice => 1,
        DmaDirection::FromDevice => 2,
        DmaDirection::Bidirectional => 3,
    }
}

impl Backend for LinuxVfio {
    type Region = u64;
    type Dma = u64;
    type Interrupt = u64;

    fn generation(&self) -> u64 {
        self.generation
    }

    fn open_region(&mut self, index: u8) -> Result<u64> {
        let info = userspace_vfio::region_info(&self.device, u32::from(index))
            .map_err(|_| Error::DeviceFault)?;
        let len = usize::try_from(info.size).map_err(|_| Error::Limit)?;
        if len == 0 {
            return Err(Error::Invalid);
        }
        let mapping = RegionMapping::map(&self.device, &info, 0, len, true)
            .map_err(|_| Error::DeviceFault)?;
        let id = self.id()?;
        self.regions.insert(id, mapping);
        Ok(id)
    }

    fn region_len(&self, region: &u64) -> usize {
        self.regions.get(region).map_or(0, RegionMapping::len)
    }

    fn read_u32(&mut self, region: &u64, offset: usize) -> Result<u32> {
        let value = self
            .regions
            .get(region)
            .ok_or(Error::StaleHandle)?
            .read_u32(offset)
            .map_err(|_| Error::OutOfBounds)?;
        fence(Ordering::Acquire);
        Ok(value)
    }

    fn write_u32(&mut self, region: &u64, offset: usize, value: u32) -> Result<()> {
        fence(Ordering::Release);
        self.regions
            .get(region)
            .ok_or(Error::StaleHandle)?
            .write_u32(offset, value)
            .map_err(|_| Error::OutOfBounds)
    }

    fn write_dma_address(
        &mut self,
        region: &u64,
        low: usize,
        high: Option<usize>,
        dma: &u64,
        offset: usize,
    ) -> Result<()> {
        let address = self.dma_device_address(dma, offset)?;
        if high.is_none() && address > u64::from(u32::MAX) {
            return Err(Error::Limit);
        }
        self.write_u32(region, low, address as u32)?;
        if let Some(high) = high {
            self.write_u32(region, high, (address >> 32) as u32)?;
        }
        Ok(())
    }

    fn dma_device_address(&self, dma: &u64, offset: usize) -> Result<u64> {
        let dma = self.dma(dma)?;
        if offset >= dma.len {
            return Err(Error::OutOfBounds);
        }
        dma.iova
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
        self.alloc_dma_constrained(size, DmaConstraints::new(align), direction, coherent)
    }

    fn alloc_dma_constrained(
        &mut self,
        size: usize,
        constraints: DmaConstraints,
        direction: DmaDirection,
        coherent: bool,
    ) -> Result<u64> {
        let mapped_len = checked_allocation(size, constraints)?;
        let id = self.id()?;
        let dma = match self.flavor {
            Flavor::Coherent => {
                let iova = aligned(self.next_iova, constraints.alignment)?;
                let last = iova
                    .checked_add(mapped_len as u64 - 1)
                    .ok_or(Error::Limit)?;
                if last > constraints.max_device_address {
                    return Err(Error::Limit);
                }
                let iommu = self.iommu.as_ref().ok_or(Error::DeviceFault)?;
                let ioas = self.ioas.as_ref().ok_or(Error::DeviceFault)?.id();
                let (device_reads, device_writes) = match direction {
                    DmaDirection::ToDevice => (true, false),
                    DmaDirection::FromDevice => (false, true),
                    DmaDirection::Bidirectional => (true, true),
                };
                let mapping = DmaMapping::map_with_flags(
                    iommu,
                    ioas,
                    iova,
                    mapped_len,
                    PAGE,
                    device_reads,
                    device_writes,
                )
                .map_err(|_| Error::DeviceFault)?;
                self.next_iova = last.checked_add(1).ok_or(Error::Limit)?;
                Dma {
                    memory: DmaMemory::Ioas(mapping),
                    iova,
                    len: size,
                    direction,
                    broker_handle: None,
                }
            }
            Flavor::Broker => {
                let operation = if coherent {
                    broker::ALLOC_COHERENT
                } else {
                    broker::MAP_STREAMING
                };
                let mut arena = if coherent {
                    None
                } else {
                    Some(AnonymousMapping::new(mapped_len).map_err(|_| Error::DeviceFault)?)
                };
                let result = self.broker(DmaBrokerCommand {
                    operation,
                    size: size as u64,
                    alignment: constraints.alignment as u64,
                    max_device_address: constraints.max_device_address,
                    user_address: arena.as_ref().map_or(0, AnonymousMapping::user_address),
                    direction: if coherent {
                        0
                    } else {
                        direction_number(direction)
                    },
                    ..Default::default()
                })?;
                if result.handle == 0
                    || result
                        .iova
                        .checked_add(size as u64 - 1)
                        .is_none_or(|last| last > constraints.max_device_address)
                    || !result.iova.is_multiple_of(constraints.alignment as u64)
                {
                    let _ = self.broker(DmaBrokerCommand {
                        operation: if coherent {
                            broker::FREE
                        } else {
                            broker::UNMAP
                        },
                        handle: result.handle,
                        ..Default::default()
                    });
                    return Err(Error::DeviceFault);
                }
                let memory = if coherent {
                    match DeviceMapping::map(&self.device, result.mmap_offset, size) {
                        Ok(mapping) => DmaMemory::BrokerCoherent(mapping),
                        Err(_) => {
                            let _ = self.broker(DmaBrokerCommand {
                                operation: broker::FREE,
                                handle: result.handle,
                                ..Default::default()
                            });
                            return Err(Error::DeviceFault);
                        }
                    }
                } else {
                    DmaMemory::BrokerStreaming(arena.take().expect("streaming arena"))
                };
                Dma {
                    memory,
                    iova: result.iova,
                    len: size,
                    direction,
                    broker_handle: Some(result.handle),
                }
            }
        };
        self.dmas.insert(id, dma);
        Ok(id)
    }

    fn dma_read(&mut self, dma: &u64, range: Range<usize>, out: &mut [u8]) -> Result<()> {
        let dma = self.dma(dma)?;
        if dma.direction == DmaDirection::ToDevice || range.len() != out.len() {
            return Err(Error::Invalid);
        }
        if range.start > range.end || range.end > dma.len {
            return Err(Error::OutOfBounds);
        }
        let bytes = match &dma.memory {
            DmaMemory::Ioas(memory) => memory.read(range.start, range.len()),
            DmaMemory::BrokerCoherent(memory) => memory.read(range.start, range.len()),
            DmaMemory::BrokerStreaming(memory) => memory.read(range.start, range.len()),
        }
        .map_err(|_| Error::OutOfBounds)?;
        out.copy_from_slice(&bytes);
        Ok(())
    }

    fn dma_write(&mut self, dma: &u64, range: Range<usize>, bytes: &[u8]) -> Result<()> {
        let dma = self.dma_mut(dma)?;
        if dma.direction == DmaDirection::FromDevice || range.len() != bytes.len() {
            return Err(Error::Invalid);
        }
        if range.start > range.end || range.end > dma.len {
            return Err(Error::OutOfBounds);
        }
        match &mut dma.memory {
            DmaMemory::Ioas(memory) => memory.write(range.start, bytes),
            DmaMemory::BrokerCoherent(memory) => memory.write(range.start, bytes),
            DmaMemory::BrokerStreaming(memory) => memory.write(range.start, bytes),
        }
        .map_err(|_| Error::OutOfBounds)
    }

    fn sync_for_cpu(&mut self, dma: &u64, range: Range<usize>) -> Result<()> {
        let dma = self.dma(dma)?;
        if dma.direction == DmaDirection::ToDevice {
            return Err(Error::Invalid);
        }
        if range.start > range.end || range.end > dma.len {
            return Err(Error::OutOfBounds);
        }
        if self.flavor == Flavor::Broker {
            self.broker(DmaBrokerCommand {
                operation: broker::SYNC_CPU,
                handle: dma.broker_handle.ok_or(Error::DeviceFault)?,
                offset: range.start as u64,
                length: range.len() as u64,
                ..Default::default()
            })?;
        } else {
            fence(Ordering::Acquire);
        }
        Ok(())
    }

    fn sync_for_device(&mut self, dma: &u64, range: Range<usize>) -> Result<()> {
        let dma = self.dma(dma)?;
        if dma.direction == DmaDirection::FromDevice {
            return Err(Error::Invalid);
        }
        if range.start > range.end || range.end > dma.len {
            return Err(Error::OutOfBounds);
        }
        if self.flavor == Flavor::Broker {
            self.broker(DmaBrokerCommand {
                operation: broker::SYNC_DEVICE,
                handle: dma.broker_handle.ok_or(Error::DeviceFault)?,
                offset: range.start as u64,
                length: range.len() as u64,
                ..Default::default()
            })?;
        } else {
            fence(Ordering::Release);
        }
        Ok(())
    }

    fn open_interrupt(&mut self, vector: u32) -> Result<u64> {
        let capability =
            userspace_vfio::irq_capability(&self.device, vector).map_err(|_| Error::DeviceFault)?;
        let interrupt =
            VfioIrq::install(&self.device, capability).map_err(|_| Error::DeviceFault)?;
        let id = self.id()?;
        self.interrupts.insert(id, (vector, interrupt));
        Ok(id)
    }

    fn wait_interrupt(&mut self, interrupt: &u64, deadline_ns: u64) -> Result<Option<IrqEvent>> {
        let (vector, interrupt) = self.interrupts.get(interrupt).ok_or(Error::StaleHandle)?;
        let count = interrupt
            .wait_until(deadline_ns)
            .map_err(|_| Error::DeviceFault)?;
        let at_ns = userspace_vfio::monotonic_time_ns().map_err(|_| Error::DeviceFault)?;
        Ok(count.map(|count| IrqEvent {
            vector: *vector,
            count,
            at_ns,
        }))
    }

    fn reset(&mut self) -> Result<u64> {
        self.revoke_all();
        userspace_vfio::reset_device(&self.device).map_err(|_| Error::DeviceFault)?;
        self.generation = self.generation.checked_add(1).ok_or(Error::Limit)?;
        Ok(self.generation)
    }

    fn release_region(&mut self, region: u64) {
        self.regions.remove(&region);
    }
    fn release_dma(&mut self, dma: u64) {
        if let Some(dma) = self.dmas.remove(&dma) {
            self.release_dma_resource(dma);
        }
    }
    fn release_interrupt(&mut self, interrupt: u64) {
        self.interrupts.remove(&interrupt);
    }
}

impl Drop for LinuxVfio {
    fn drop(&mut self) {
        self.revoke_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_validation_is_fail_closed() {
        assert_eq!(
            checked_allocation(0, DmaConstraints::new(PAGE)),
            Err(Error::Invalid)
        );
        let mut constraints = DmaConstraints::new(PAGE);
        constraints.max_segment_size = PAGE - 1;
        assert_eq!(checked_allocation(PAGE, constraints), Err(Error::Limit));
        constraints.max_segment_size = PAGE;
        constraints.max_segments = 0;
        assert_eq!(checked_allocation(PAGE, constraints), Err(Error::Limit));
    }

    #[test]
    fn coherent_and_broker_flavors_keep_domain_ownership_separate() {
        let device = Arc::new(File::open("/dev/null").unwrap());
        let iommu = Arc::new(File::open("/dev/null").unwrap());
        let coherent =
            LinuxVfio::initialize_coherent(Arc::clone(&device), Arc::clone(&iommu), |_, iommu| {
                Ok(Ioas::from_allocated(iommu, 7))
            })
            .unwrap();
        assert_eq!(coherent.flavor, Flavor::Coherent);
        assert!(coherent.iommu.is_some());
        assert_eq!(coherent.ioas.as_ref().unwrap().id(), 7);

        let broker = LinuxVfio::initialize_broker(device, |_| Ok(())).unwrap();
        assert_eq!(broker.flavor, Flavor::Broker);
        assert!(broker.iommu.is_none());
        assert!(broker.ioas.is_none());
    }

    #[test]
    fn broker_probe_failure_is_a_precise_construction_error() {
        let device = Arc::new(File::open("/dev/null").unwrap());
        let error = match LinuxVfio::initialize_broker(device, |_| Err("ENOTTY".into())) {
            Err(error) => error,
            Ok(_) => panic!("missing broker feature unexpectedly succeeded"),
        };
        assert!(
            matches!(error, LinuxVfioError::DmaBrokerUnavailable(message) if message == "ENOTTY")
        );
    }

    #[test]
    fn address_alignment_checks_overflow() {
        assert_eq!(aligned(FIRST_IOVA + 1, 8192), Ok(FIRST_IOVA + 8192));
        assert_eq!(aligned(u64::MAX, PAGE), Err(Error::Limit));
    }
}
