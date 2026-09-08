//! Production Linux VFIO platform backend.

use crate::{PciConfigSnapshot, PciControl, PciControlError};
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
    AnonymousMapping, DeviceMapping, DmaBrokerCommand, DmaMapping, Ioas, IrqCapability,
    RegionMapping, VfioIrq, dma_broker_uapi as broker,
};

const PAGE: usize = 4096;
const FIRST_IOVA: u64 = 0x0100_0000;

#[derive(Debug)]
pub enum LinuxVfioError {
    OpenDevice(std::io::Error),
    OpenIommufd(std::io::Error),
    Setup(String),
    DmaBrokerUnavailable(String),
    PciControl(PciControlError),
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
            Self::PciControl(error) => write!(f, "PCI control: {error}"),
        }
    }
}
impl std::error::Error for LinuxVfioError {}

pub struct OpenedPciCoherent {
    backend: LinuxVfio,
    pci: PciControl,
    config: PciConfigSnapshot,
}

impl OpenedPciCoherent {
    pub fn into_parts(self) -> (LinuxVfio, PciControl, PciConfigSnapshot) {
        (self.backend, self.pci, self.config)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flavor {
    Coherent,
    PciCoherent,
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
    pci_irq: Option<IrqCapability>,
    generation: u64,
    next_id: u64,
    next_iova: u64,
    regions: HashMap<u64, RegionMapping>,
    dmas: HashMap<u64, Dma>,
    quarantined_dmas: HashMap<u64, Dma>,
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

    pub fn validate_wcn6750_resources(
        &self,
        register_region: u32,
        expected_size: usize,
    ) -> std::result::Result<userspace_vfio::PlatformDeviceInfo, LinuxVfioError> {
        let device = userspace_vfio::validate_wcn6750_platform_cdev(&self.device)
            .map_err(LinuxVfioError::Setup)?;
        if register_region >= device.num_regions {
            return Err(LinuxVfioError::Setup(format!(
                "VFIO register region {register_region} is absent (device exposes {})",
                device.num_regions
            )));
        }
        self.probe_region_mapping(register_region, expected_size)?;
        Ok(device)
    }

    /// Validate the platform/IRQ contract and enumerate regions without
    /// assuming that the hybrid-bus register BAR is present yet. WCN6750
    /// learns that BAR from QMI DeviceInfo before a later DT exposes it.
    pub fn inspect_wcn6750_resources(
        &self,
    ) -> std::result::Result<userspace_vfio::PlatformDeviceInfo, LinuxVfioError> {
        userspace_vfio::validate_wcn6750_platform_cdev(&self.device).map_err(LinuxVfioError::Setup)
    }

    /// Query one enumerated region while preserving the ioctl's exact error.
    pub fn region_info(
        &self,
        index: u32,
    ) -> std::result::Result<userspace_vfio::RegionInfo, LinuxVfioError> {
        userspace_vfio::region_info(&self.device, index).map_err(|error| {
            LinuxVfioError::Setup(format!(
                "VFIO region {index} GET_REGION_INFO failed: {error}"
            ))
        })
    }

    /// Prove that a selected VFIO register window has the caller-required
    /// exact size and can be mapped. Diagnostics preserve every returned fact.
    pub fn probe_region_mapping(
        &self,
        index: u32,
        expected_size: usize,
    ) -> std::result::Result<(), LinuxVfioError> {
        let info = self.region_info(index)?;
        let facts = format!(
            "VFIO region {index} flags={:#x} size={:#x} offset={:#x}",
            info.flags, info.size, info.offset
        );
        let len = usize::try_from(info.size)
            .map_err(|_| LinuxVfioError::Setup(format!("{facts}: size exceeds usize")))?;
        if len == 0 {
            return Err(LinuxVfioError::Setup(format!("{facts}: region is empty")));
        }
        if len != expected_size {
            return Err(LinuxVfioError::Setup(format!(
                "{facts}: expected register window size {expected_size:#x}"
            )));
        }
        RegionMapping::map(&self.device, &info, 0, len, true)
            .map_err(|error| LinuxVfioError::Setup(format!("{facts}: {error}")))?;
        Ok(())
    }

    pub fn open_pci_coherent(
        path: impl AsRef<Path>,
        pci_config_path: impl AsRef<Path>,
    ) -> std::result::Result<OpenedPciCoherent, LinuxVfioError> {
        let pci = PciControl::open(pci_config_path).map_err(LinuxVfioError::PciControl)?;
        let device = Arc::new(open_device(path)?);
        let iommu = Arc::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/iommu")
                .map_err(LinuxVfioError::OpenIommufd)?,
        );
        Self::initialize_pci_controlled(pci, device, iommu, |device, iommu| {
            userspace_vfio::bind_iommufd(device, iommu)?;
            let ioas = userspace_vfio::allocate_ioas(iommu)?;
            userspace_vfio::attach_ioas(device, ioas.id())?;
            Ok(ioas)
        })
    }

    fn initialize_pci_controlled(
        mut pci: PciControl,
        device: Arc<File>,
        iommu: Arc<File>,
        setup: impl FnOnce(&File, &Arc<File>) -> std::result::Result<Ioas, String>,
    ) -> std::result::Result<OpenedPciCoherent, LinuxVfioError> {
        pci.verify_dma_disabled()
            .map_err(LinuxVfioError::PciControl)?;
        let backend = Self::initialize_pci_coherent(device, iommu, setup)?;
        let config = pci
            .verify_dma_disabled()
            .map_err(LinuxVfioError::PciControl)?;
        Ok(OpenedPciCoherent {
            backend,
            pci,
            config,
        })
    }

    fn initialize_coherent(
        device: Arc<File>,
        iommu: Arc<File>,
        setup: impl FnOnce(&File, &Arc<File>) -> std::result::Result<Ioas, String>,
    ) -> std::result::Result<Self, LinuxVfioError> {
        let ioas = setup(&device, &iommu).map_err(LinuxVfioError::Setup)?;
        Ok(Self::new(
            device,
            Flavor::Coherent,
            Some(iommu),
            Some(ioas),
            None,
        ))
    }

    fn initialize_pci_coherent(
        device: Arc<File>,
        iommu: Arc<File>,
        setup: impl FnOnce(&File, &Arc<File>) -> std::result::Result<Ioas, String>,
    ) -> std::result::Result<Self, LinuxVfioError> {
        let ioas = setup(&device, &iommu).map_err(LinuxVfioError::Setup)?;
        userspace_vfio::pci_device_reset_supported(&device).map_err(LinuxVfioError::Setup)?;
        let msix = userspace_vfio::irq_capability(&device, 2).map_err(LinuxVfioError::Setup)?;
        let irq = if msix.eventfd && msix.count > 0 {
            msix
        } else {
            let msi = userspace_vfio::irq_capability(&device, 1).map_err(LinuxVfioError::Setup)?;
            if !msi.eventfd || msi.count == 0 {
                return Err(LinuxVfioError::Setup(
                    "VFIO PCI device provides neither eventfd MSI-X nor MSI".into(),
                ));
            }
            msi
        };
        Ok(Self::new(
            device,
            Flavor::PciCoherent,
            Some(iommu),
            Some(ioas),
            Some(irq),
        ))
    }

    fn initialize_broker(
        device: Arc<File>,
        probe: impl FnOnce(&File) -> std::result::Result<(), String>,
    ) -> std::result::Result<Self, LinuxVfioError> {
        probe(&device).map_err(LinuxVfioError::DmaBrokerUnavailable)?;
        Ok(Self::new(device, Flavor::Broker, None, None, None))
    }

    fn new(
        device: Arc<File>,
        flavor: Flavor,
        iommu: Option<Arc<File>>,
        ioas: Option<Ioas>,
        pci_irq: Option<IrqCapability>,
    ) -> Self {
        Self {
            device,
            iommu,
            ioas,
            flavor,
            pci_irq,
            generation: 1,
            next_id: 1,
            next_iova: FIRST_IOVA,
            regions: HashMap::new(),
            dmas: HashMap::new(),
            quarantined_dmas: HashMap::new(),
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

    fn release_dma_resource(&self, dma: &mut Dma) -> Result<()> {
        match &mut dma.memory {
            DmaMemory::Ioas(mapping) => {
                mapping.teardown().map_err(|_| Error::DeviceFault)?;
            }
            DmaMemory::BrokerCoherent(mapping) => {
                // FREE is required to fail while the shared mapping exists.
                mapping.teardown().map_err(|_| Error::DeviceFault)?;
                if let Some(handle) = dma.broker_handle {
                    self.broker(DmaBrokerCommand {
                        operation: broker::FREE,
                        handle,
                        ..Default::default()
                    })?;
                    dma.broker_handle = None;
                }
            }
            DmaMemory::BrokerStreaming(mapping) => {
                if let Some(handle) = dma.broker_handle {
                    self.broker(DmaBrokerCommand {
                        operation: broker::UNMAP,
                        handle,
                        ..Default::default()
                    })?;
                    dma.broker_handle = None;
                }
                mapping.teardown().map_err(|_| Error::DeviceFault)?;
            }
        }
        Ok(())
    }

    fn revoke_interrupts(&mut self) -> Result<()> {
        let mut failed = false;
        for (id, (vector, mut interrupt)) in std::mem::take(&mut self.interrupts) {
            if interrupt.disable().is_err() {
                self.interrupts.insert(id, (vector, interrupt));
                failed = true;
            }
        }
        (!failed).then_some(()).ok_or(Error::DeviceFault)
    }

    fn revoke_dmas(&mut self) -> Result<()> {
        self.quarantined_dmas.extend(std::mem::take(&mut self.dmas));
        let mut failed = HashMap::new();
        for (id, mut dma) in std::mem::take(&mut self.quarantined_dmas) {
            if self.release_dma_resource(&mut dma).is_err() {
                failed.insert(id, dma);
            }
        }
        self.quarantined_dmas = failed;
        self.quarantined_dmas
            .is_empty()
            .then_some(())
            .ok_or(Error::DeviceFault)
    }

    fn discard_reset_broker_dmas(&mut self) {
        self.quarantined_dmas.extend(std::mem::take(&mut self.dmas));
        for (_, mut dma) in std::mem::take(&mut self.quarantined_dmas) {
            dma.broker_handle = None;
            match &mut dma.memory {
                DmaMemory::BrokerCoherent(mapping) => {
                    let _ = mapping.teardown();
                }
                DmaMemory::BrokerStreaming(mapping) => {
                    let _ = mapping.teardown();
                }
                DmaMemory::Ioas(_) => unreachable!("broker backend owns no IOAS DMA"),
            }
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

    fn is_cache_coherent(&self) -> bool {
        self.flavor != Flavor::Broker
    }

    fn open_region(&mut self, index: u8) -> Result<u64> {
        if self.flavor == Flavor::PciCoherent && index > 5 {
            return Err(Error::Invalid);
        }
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
            Flavor::Coherent | Flavor::PciCoherent => {
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
        // The safe API also uses this primitive to zero-initialize fresh
        // device-to-CPU allocations; typed handles prevent later CPU writes.
        if range.len() != bytes.len() {
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

    fn dma_read_once_u32(&mut self, dma: &u64, offset: usize) -> Result<u32> {
        match &self.dma(dma)?.memory {
            DmaMemory::Ioas(memory) => memory.read_u32(offset),
            DmaMemory::BrokerCoherent(memory) => memory.read_u32(offset),
            DmaMemory::BrokerStreaming(memory) => memory.read_u32(offset),
        }
        .map_err(|_| Error::OutOfBounds)
    }

    fn dma_write_once_u32(&mut self, dma: &u64, offset: usize, value: u32) -> Result<()> {
        match &mut self.dma_mut(dma)?.memory {
            DmaMemory::Ioas(memory) => memory.write_u32(offset, value),
            DmaMemory::BrokerCoherent(memory) => memory.write_u32(offset, value),
            DmaMemory::BrokerStreaming(memory) => memory.write_u32(offset, value),
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
        let (capability, start) = if let Some(capability) = self.pci_irq {
            if vector >= capability.count {
                return Err(Error::Limit);
            }
            (capability, vector)
        } else {
            (
                userspace_vfio::irq_capability(&self.device, vector)
                    .map_err(|_| Error::DeviceFault)?,
                0,
            )
        };
        let interrupt =
            VfioIrq::install_at(&self.device, capability, start).map_err(|_| Error::DeviceFault)?;
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

    fn wait_any(&mut self, interrupts: &[&u64], deadline_ns: u64) -> Result<Vec<IrqEvent>> {
        if interrupts.is_empty() {
            return Err(Error::Invalid);
        }
        let mut fds = Vec::with_capacity(interrupts.len());
        for id in interrupts {
            let (_, interrupt) = self.interrupts.get(*id).ok_or(Error::StaleHandle)?;
            interrupt.prepare_wait().map_err(|_| Error::DeviceFault)?;
            fds.push(interrupt.event_fd());
        }
        let ready = userspace_vfio::wait_eventfds_until(&fds, deadline_ns)
            .map_err(|_| Error::DeviceFault)?;
        let at_ns = userspace_vfio::monotonic_time_ns().map_err(|_| Error::DeviceFault)?;
        let mut events = Vec::with_capacity(ready.len());
        for index in ready {
            let (vector, interrupt) = self
                .interrupts
                .get(interrupts[index])
                .ok_or(Error::StaleHandle)?;
            if let Some(count) = interrupt.try_read().map_err(|_| Error::DeviceFault)? {
                events.push(IrqEvent {
                    vector: *vector,
                    count,
                    at_ns,
                });
            }
        }
        Ok(events)
    }

    fn reset(&mut self) -> Result<u64> {
        match userspace_vfio::device_reset_supported(&self.device) {
            Ok(true) => {}
            Ok(false) => return Err(Error::Unsupported),
            Err(_) => return Err(Error::DeviceFault),
        }
        let next_generation = self.generation.checked_add(1).ok_or(Error::Limit)?;
        // From this point cleanup mutates live resources, so all issued handles
        // must become stale even if cleanup or the reset ioctl later fails.
        self.generation = next_generation;
        self.revoke_interrupts()?;
        self.regions.clear();
        if self.flavor == Flavor::Broker {
            userspace_vfio::reset_device_unchecked(&self.device).map_err(|_| Error::DeviceFault)?;
            // Successful broker reset revoked handles in-kernel; only now may
            // their userspace mappings be discarded without FREE/UNMAP.
            self.discard_reset_broker_dmas();
        } else {
            self.revoke_dmas()?;
            userspace_vfio::reset_device_unchecked(&self.device).map_err(|_| Error::DeviceFault)?;
        }
        Ok(self.generation)
    }

    fn release_region(&mut self, region: u64) {
        self.regions.remove(&region);
    }
    fn release_dma(&mut self, dma: u64) {
        if let Some(mut resource) = self.dmas.remove(&dma)
            && self.release_dma_resource(&mut resource).is_err()
        {
            self.quarantined_dmas.insert(dma, resource);
        }
    }
    fn release_interrupt(&mut self, interrupt: u64) {
        if let Some((vector, mut resource)) = self.interrupts.remove(&interrupt)
            && resource.disable().is_err()
        {
            self.interrupts.insert(interrupt, (vector, resource));
        }
    }
}

impl Drop for LinuxVfio {
    fn drop(&mut self) {
        let _ = self.revoke_interrupts();
        let _ = self.revoke_dmas();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        io::{Seek, SeekFrom, Write},
    };
    use userspace_vfio::test_support::{
        Failure, FakeIrq, Record, signal_eventfd, with_fake_automasked_io, with_fake_io,
        with_fake_io_failure, with_fake_no_reset_io, with_fake_pci_io,
    };

    fn fake_device() -> (Arc<File>, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "drv-vfio-fake-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(64 * 1024).unwrap();
        (Arc::new(file), path)
    }

    fn fake_pci_control(command: u16, power_state: u8) -> (PciControl, File, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "drv-pci-config-fake-{}-{:?}-{command:04x}-{power_state}",
            std::process::id(),
            std::thread::current().id()
        ));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let mut bytes = [0; 256];
        bytes[4..6].copy_from_slice(&command.to_le_bytes());
        bytes[6..8].copy_from_slice(&(1u16 << 4).to_le_bytes());
        bytes[0x34] = 0x40;
        bytes[0x40] = 1;
        bytes[0x44..0x46].copy_from_slice(&u16::from(power_state).to_le_bytes());
        file.write_all(&bytes).unwrap();
        file.flush().unwrap();
        let observer = file.try_clone().unwrap();
        (PciControl::from_file(file), observer, path)
    }

    fn fake_pci_backend(device: Arc<File>) -> LinuxVfio {
        let iommu = Arc::new(File::open("/dev/null").unwrap());
        LinuxVfio::initialize_pci_coherent(device, iommu, |device, iommu| {
            userspace_vfio::bind_iommufd(device, iommu)?;
            let ioas = userspace_vfio::allocate_ioas(iommu)?;
            userspace_vfio::attach_ioas(device, ioas.id())?;
            Ok(ioas)
        })
        .unwrap()
    }

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
    fn platform_cdev_validates_edge_irqs_before_attach_and_use() {
        let (device, path) = fake_device();
        let iommu = Arc::new(File::open("/dev/null").unwrap());
        let (_, records) = with_fake_io(false, || {
            let mut backend = LinuxVfio::initialize_coherent(device, iommu, |device, iommu| {
                userspace_vfio::bind_iommufd(device, iommu)?;
                let ioas = userspace_vfio::allocate_ioas(iommu)?;
                userspace_vfio::attach_ioas(device, ioas.id())?;
                Ok(ioas)
            })
            .unwrap();
            backend.validate_wcn6750_resources(0, PAGE).unwrap();
            let mismatch = backend.probe_region_mapping(1, PAGE * 2).unwrap_err();
            let mismatch = mismatch.to_string();
            assert!(mismatch.contains("VFIO region 1 flags=0x7 size=0x1000 offset=0x1000"));
            assert!(mismatch.contains("expected register window size 0x2000"));
            backend.probe_region_mapping(1, PAGE).unwrap();
            let dma = backend
                .alloc_dma(PAGE, PAGE, DmaDirection::Bidirectional, false)
                .unwrap();
            let irq = backend.open_interrupt(3).unwrap();
            signal_eventfd(backend.interrupts.get(&irq).unwrap().1.event_fd(), 1).unwrap();
            let deadline = userspace_vfio::monotonic_time_ns().unwrap() + 1_000_000_000;
            assert_eq!(
                backend
                    .wait_interrupt(&irq, deadline)
                    .unwrap()
                    .unwrap()
                    .count,
                1
            );
            backend.release_interrupt(irq);
            backend.release_dma(dma);
            drop(backend);
        });
        std::fs::remove_file(path).unwrap();
        let mut expected = vec![
            Record::Bind,
            Record::AllocateIoas,
            Record::AttachIoas(7),
            Record::QueryDevice,
        ];
        expected.extend((0..32).map(Record::QueryIrq));
        expected.extend([
            Record::QueryRegion(0),
            Record::QueryRegion(1),
            Record::QueryRegion(1),
            Record::Map {
                iova: FIRST_IOVA,
                length: PAGE as u64,
                device_reads: true,
                device_writes: true,
            },
            Record::QueryIrq(3),
            Record::InstallIrq(3),
            Record::DisableIrq(3),
            Record::Unmap {
                iova: FIRST_IOVA,
                length: PAGE as u64,
            },
            Record::DestroyIoas(7),
        ]);
        assert_eq!(records, expected);
    }

    #[test]
    fn platform_without_vfio_reset_reports_typed_unsupported() {
        let (device, path) = fake_device();
        let iommu = Arc::new(File::open("/dev/null").unwrap());
        let (result, _) = with_fake_no_reset_io(|| {
            let backend = LinuxVfio::initialize_coherent(device, iommu, |device, iommu| {
                userspace_vfio::bind_iommufd(device, iommu)?;
                let ioas = userspace_vfio::allocate_ioas(iommu)?;
                userspace_vfio::attach_ioas(device, ioas.id())?;
                Ok(ioas)
            })
            .unwrap();
            assert!(
                !backend
                    .validate_wcn6750_resources(0, PAGE)
                    .unwrap()
                    .reset_supported
            );
            drv_hardware::Device::from_backend(backend).reset()
        });
        assert_eq!(result, Err(Error::Unsupported));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn broker_flavor_probes_allocates_maps_syncs_and_releases_fake_fd() {
        let (device, path) = fake_device();
        let (_, records) = with_fake_io(true, || {
            let mut backend =
                LinuxVfio::initialize_broker(device, userspace_vfio::probe_dma_broker).unwrap();
            assert!(backend.iommu.is_none());
            assert!(backend.ioas.is_none());

            let coherent = backend
                .alloc_dma(PAGE, PAGE, DmaDirection::Bidirectional, true)
                .unwrap();
            backend.release_dma(coherent);

            let streaming = backend
                .alloc_dma(PAGE, PAGE, DmaDirection::Bidirectional, false)
                .unwrap();
            backend.sync_for_device(&streaming, 0..64).unwrap();
            backend.sync_for_cpu(&streaming, 0..64).unwrap();
            backend.release_dma(streaming);
            drop(backend);
        });
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            records,
            vec![
                Record::ProbeBroker,
                Record::Broker {
                    operation: broker::ALLOC_COHERENT,
                    handle: 11,
                    offset: 0,
                    length: 0,
                },
                Record::Broker {
                    operation: broker::FREE,
                    handle: 11,
                    offset: 0,
                    length: 0,
                },
                Record::Broker {
                    operation: broker::MAP_STREAMING,
                    handle: 12,
                    offset: 0,
                    length: 0,
                },
                Record::Broker {
                    operation: broker::SYNC_DEVICE,
                    handle: 12,
                    offset: 0,
                    length: 64,
                },
                Record::Broker {
                    operation: broker::SYNC_CPU,
                    handle: 12,
                    offset: 0,
                    length: 64,
                },
                Record::Broker {
                    operation: broker::UNMAP,
                    handle: 12,
                    offset: 0,
                    length: 0,
                },
            ]
        );
    }

    #[test]
    fn broker_probe_failure_is_a_precise_construction_error() {
        let (device, path) = fake_device();
        let error = match LinuxVfio::initialize_broker(device, |_| Err("ENOTTY".into())) {
            Err(error) => error,
            Ok(_) => panic!("missing broker feature unexpectedly succeeded"),
        };
        std::fs::remove_file(path).unwrap();
        assert!(
            matches!(error, LinuxVfioError::DmaBrokerUnavailable(message) if message == "ENOTTY")
        );
    }

    #[test]
    fn automasked_irq_is_unmasked_before_next_wait_and_balanced_on_release() {
        let (device, path) = fake_device();
        let (_, records) = with_fake_automasked_io(|| {
            let mut backend = LinuxVfio::initialize_broker(device, |_| Ok(())).unwrap();
            let irq = backend.open_interrupt(3).unwrap();
            let event_fd = backend.interrupts.get(&irq).unwrap().1.event_fd();

            signal_eventfd(event_fd, 2).unwrap();
            let deadline = userspace_vfio::monotonic_time_ns().unwrap() + 1_000_000_000;
            assert_eq!(
                backend
                    .wait_interrupt(&irq, deadline)
                    .unwrap()
                    .unwrap()
                    .count,
                2
            );

            signal_eventfd(event_fd, 1).unwrap();
            let deadline = userspace_vfio::monotonic_time_ns().unwrap() + 1_000_000_000;
            assert_eq!(
                backend
                    .wait_interrupt(&irq, deadline)
                    .unwrap()
                    .unwrap()
                    .count,
                1
            );
            backend.release_interrupt(irq);
            drop(backend);
        });
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            records,
            vec![
                Record::QueryIrq(3),
                Record::InstallIrq(3),
                Record::UnmaskIrq(3),
                Record::UnmaskIrq(3),
                Record::DisableIrq(3),
            ]
        );
    }

    #[test]
    fn wait_any_returns_every_ready_fake_eventfd_in_one_batch() {
        let (device, path) = fake_device();
        let (_, records) = with_fake_io(true, || {
            let mut backend = LinuxVfio::initialize_broker(device, |_| Ok(())).unwrap();
            let first = backend.open_interrupt(3).unwrap();
            let second = backend.open_interrupt(4).unwrap();
            signal_eventfd(backend.interrupts.get(&first).unwrap().1.event_fd(), 2).unwrap();
            signal_eventfd(backend.interrupts.get(&second).unwrap().1.event_fd(), 5).unwrap();
            let deadline = userspace_vfio::monotonic_time_ns().unwrap() + 1_000_000_000;
            let events = backend.wait_any(&[&first, &second], deadline).unwrap();
            assert_eq!(
                events
                    .iter()
                    .map(|event| (event.vector, event.count))
                    .collect::<Vec<_>>(),
                vec![(3, 2), (4, 5)]
            );
            backend.release_interrupt(first);
            backend.release_interrupt(second);
            drop(backend);
        });
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            records,
            vec![
                Record::QueryIrq(3),
                Record::InstallIrq(3),
                Record::QueryIrq(4),
                Record::InstallIrq(4),
                Record::DisableIrq(3),
                Record::DisableIrq(4),
            ]
        );
    }

    #[test]
    fn pci_prefers_msix_maps_logical_vectors_and_rejects_non_bar_regions() {
        let (device, path) = fake_device();
        let (_, records) = with_fake_pci_io(
            FakeIrq {
                count: 8,
                eventfd: true,
            },
            FakeIrq {
                count: 4,
                eventfd: true,
            },
            || {
                let mut backend = fake_pci_backend(device);
                let bar = backend.open_region(5).unwrap();
                assert_eq!(backend.open_region(6), Err(Error::Invalid));
                let interrupt = backend.open_interrupt(3).unwrap();
                assert_eq!(backend.open_interrupt(4), Err(Error::Limit));
                backend.release_interrupt(interrupt);
                backend.release_region(bar);
            },
        );
        std::fs::remove_file(path).unwrap();
        assert!(records.contains(&Record::QueryDevice));
        assert!(records.contains(&Record::QueryIrq(2)));
        assert!(!records.contains(&Record::QueryIrq(1)));
        assert!(records.contains(&Record::QueryRegion(5)));
        assert!(!records.contains(&Record::QueryRegion(6)));
        assert!(records.contains(&Record::InstallIrqAt { index: 2, start: 3 }));
        assert!(records.contains(&Record::DisableIrqAt { index: 2, start: 3 }));
    }

    #[test]
    fn pci_construction_gates_both_sides_of_attach() {
        const MSE: u16 = 1 << 1;
        const BME: u16 = 1 << 2;
        let (device, device_path) = fake_device();
        let iommu = Arc::new(File::open("/dev/null").unwrap());
        let (unsafe_pci, _, unsafe_path) = fake_pci_control(MSE | BME, 0);
        let attached = Cell::new(false);
        assert!(matches!(
            LinuxVfio::initialize_pci_controlled(
                unsafe_pci,
                Arc::clone(&device),
                Arc::clone(&iommu),
                |_, _| {
                    attached.set(true);
                    unreachable!()
                }
            ),
            Err(LinuxVfioError::PciControl(
                PciControlError::UnsafeDmaState { .. }
            ))
        ));
        assert!(!attached.get());

        let (safe_pci, mut config, safe_path) = fake_pci_control(MSE, 0);
        with_fake_pci_io(
            FakeIrq {
                count: 1,
                eventfd: true,
            },
            FakeIrq {
                count: 0,
                eventfd: true,
            },
            || {
                assert!(matches!(
                    LinuxVfio::initialize_pci_controlled(
                        safe_pci,
                        device,
                        iommu,
                        |device, iommu| {
                            userspace_vfio::bind_iommufd(device, iommu)?;
                            let ioas = userspace_vfio::allocate_ioas(iommu)?;
                            userspace_vfio::attach_ioas(device, ioas.id())?;
                            config.seek(SeekFrom::Start(4)).map_err(|e| e.to_string())?;
                            config
                                .write_all(&(MSE | BME).to_le_bytes())
                                .map_err(|e| e.to_string())?;
                            Ok(ioas)
                        }
                    ),
                    Err(LinuxVfioError::PciControl(
                        PciControlError::UnsafeDmaState { .. }
                    ))
                ));
            },
        );
        std::fs::remove_file(device_path).unwrap();
        std::fs::remove_file(unsafe_path).unwrap();
        std::fs::remove_file(safe_path).unwrap();
    }

    #[test]
    fn pci_falls_back_to_single_vector_msi() {
        let (device, path) = fake_device();
        let (_, records) = with_fake_pci_io(
            FakeIrq {
                count: 1,
                eventfd: true,
            },
            FakeIrq {
                count: 0,
                eventfd: true,
            },
            || {
                let mut backend = fake_pci_backend(device);
                let interrupt = backend.open_interrupt(0).unwrap();
                assert_eq!(backend.open_interrupt(1), Err(Error::Limit));
                backend.release_interrupt(interrupt);
            },
        );
        std::fs::remove_file(path).unwrap();
        assert!(records.contains(&Record::QueryIrq(2)));
        assert!(records.contains(&Record::QueryIrq(1)));
        assert!(records.contains(&Record::InstallIrq(1)));
        assert!(records.contains(&Record::DisableIrq(1)));
    }

    #[test]
    fn pci_dma_is_low_addressed_and_directionally_mapped() {
        let (device, path) = fake_device();
        let (_, records) = with_fake_pci_io(
            FakeIrq {
                count: 1,
                eventfd: true,
            },
            FakeIrq {
                count: 0,
                eventfd: true,
            },
            || {
                let mut backend = fake_pci_backend(device);
                let low32 = DmaConstraints {
                    alignment: PAGE,
                    max_device_address: u32::MAX.into(),
                    max_segment_size: PAGE,
                    max_segments: 1,
                };
                for direction in [
                    DmaDirection::ToDevice,
                    DmaDirection::FromDevice,
                    DmaDirection::Bidirectional,
                ] {
                    let dma = backend
                        .alloc_dma_constrained(PAGE, low32, direction, true)
                        .unwrap();
                    assert!(backend.dma_device_address(&dma, PAGE - 1).unwrap() <= u32::MAX.into());
                    backend.release_dma(dma);
                }
            },
        );
        std::fs::remove_file(path).unwrap();
        let maps = records
            .iter()
            .filter_map(|record| match record {
                Record::Map {
                    iova,
                    device_reads,
                    device_writes,
                    ..
                } => Some((*iova, *device_reads, *device_writes)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            maps,
            vec![
                (FIRST_IOVA, true, false),
                (FIRST_IOVA + PAGE as u64, false, true),
                (FIRST_IOVA + 2 * PAGE as u64, true, true),
            ]
        );
    }

    #[test]
    fn pci_reset_revokes_resources_before_device_reset() {
        let (device, path) = fake_device();
        let (_, records) = with_fake_pci_io(
            FakeIrq {
                count: 1,
                eventfd: true,
            },
            FakeIrq {
                count: 2,
                eventfd: true,
            },
            || {
                let mut backend = fake_pci_backend(device);
                let region = backend.open_region(0).unwrap();
                let dma = backend
                    .alloc_dma(PAGE, PAGE, DmaDirection::Bidirectional, true)
                    .unwrap();
                let interrupt = backend.open_interrupt(1).unwrap();
                assert_eq!(backend.reset().unwrap(), 2);
                assert_eq!(backend.region_len(&region), 0);
                assert_eq!(backend.dma_device_address(&dma, 0), Err(Error::StaleHandle));
                assert!(matches!(
                    backend.wait_interrupt(&interrupt, 0),
                    Err(Error::StaleHandle)
                ));
            },
        );
        std::fs::remove_file(path).unwrap();
        let disable = records
            .iter()
            .position(|record| *record == Record::DisableIrqAt { index: 2, start: 1 })
            .unwrap();
        let unmap = records
            .iter()
            .position(|record| matches!(record, Record::Unmap { .. }))
            .unwrap();
        let reset = records
            .iter()
            .position(|record| *record == Record::Reset)
            .unwrap();
        assert!(disable < reset && unmap < reset);
    }

    #[test]
    fn failed_broker_unmap_quarantines_arena_until_retry_succeeds() {
        let (device, path) = fake_device();
        let (_, records) = with_fake_io_failure(true, Some(Failure::Broker(broker::UNMAP)), || {
            let mut backend = LinuxVfio::initialize_broker(device, |_| Ok(())).unwrap();
            let dma = backend
                .alloc_dma(PAGE, PAGE, DmaDirection::Bidirectional, false)
                .unwrap();
            backend.release_dma(dma);
            assert_eq!(backend.quarantined_dmas.len(), 1);
            backend.revoke_dmas().unwrap();
            assert!(backend.quarantined_dmas.is_empty());
            drop(backend);
        });
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(record, Record::Broker { operation, .. } if *operation == broker::UNMAP))
                .count(),
            2
        );
    }

    #[test]
    fn address_alignment_checks_overflow() {
        assert_eq!(aligned(FIRST_IOVA + 1, 8192), Ok(FIRST_IOVA + 8192));
        assert_eq!(aligned(u64::MAX, PAGE), Err(Error::Limit));
    }
}
