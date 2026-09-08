#![no_std]
#![forbid(unsafe_code)]

//! Capability-shaped hardware access for native safe Rust drivers.
//!
//! This crate deliberately has no host-I/O dependencies. Backends own all raw
//! resources; drivers receive only bounded handles tied to a device generation.

extern crate alloc;

use alloc::{rc::Rc, vec, vec::Vec};
use core::{cell::RefCell, marker::PhantomData, ops::Range};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Invalid,
    OutOfBounds,
    StaleHandle,
    Limit,
    DeviceFault,
    Timeout,
}
pub type Result<T> = core::result::Result<T, Error>;

mod sealed {
    pub trait Direction {}
}
pub trait Direction: sealed::Direction + 'static {}
pub trait CpuWrite: Direction {}
pub trait CpuRead: Direction {}
pub trait DeviceRead: Direction {}
pub trait DeviceWrite: Direction {}

pub enum ToDevice {}
pub enum FromDevice {}
pub enum Bidirectional {}
impl sealed::Direction for ToDevice {}
impl Direction for ToDevice {}
impl sealed::Direction for FromDevice {}
impl Direction for FromDevice {}
impl sealed::Direction for Bidirectional {}
impl Direction for Bidirectional {}
impl CpuWrite for ToDevice {}
impl DeviceRead for ToDevice {}
impl CpuRead for FromDevice {}
impl DeviceWrite for FromDevice {}
impl CpuWrite for Bidirectional {}
impl CpuRead for Bidirectional {}
impl DeviceRead for Bidirectional {}
impl DeviceWrite for Bidirectional {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DmaDirection {
    ToDevice,
    FromDevice,
    Bidirectional,
}

/// DMA-visible allocation constraints supplied by a device driver.
///
/// The current API exposes each allocation as one contiguous device-address
/// segment. Backends must reject constraints they cannot guarantee rather than
/// silently returning an unsuitable mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DmaConstraints {
    pub alignment: usize,
    pub max_device_address: u64,
    pub max_segment_size: usize,
    pub max_segments: usize,
}
impl DmaConstraints {
    pub const fn new(alignment: usize) -> Self {
        Self {
            alignment,
            max_device_address: u64::MAX,
            max_segment_size: usize::MAX,
            max_segments: usize::MAX,
        }
    }
}

#[doc(hidden)]
pub trait Backend {
    type Region;
    type Dma;
    type Interrupt;
    fn generation(&self) -> u64;
    fn open_region(&mut self, index: u8) -> Result<Self::Region>;
    fn region_len(&self, region: &Self::Region) -> usize;
    /// MMIO load with acquire ordering: subsequent CPU reads from coherent
    /// DMA observe device writes completed before the register became visible.
    fn read_u32(&mut self, region: &Self::Region, offset: usize) -> Result<u32>;
    /// MMIO store with release ordering relative to preceding CPU writes to
    /// coherent DMA and preceding `sync_for_device` operations.
    fn write_u32(&mut self, region: &Self::Region, offset: usize, value: u32) -> Result<()>;
    /// DMA-address store with the same release ordering as `write_u32`.
    fn write_dma_address(
        &mut self,
        region: &Self::Region,
        low: usize,
        high: Option<usize>,
        dma: &Self::Dma,
        offset: usize,
    ) -> Result<()>;
    /// Return the IOMMU-visible address for a checked offset in this backend's
    /// own allocation. Callers cannot construct or register an address.
    fn dma_device_address(&self, dma: &Self::Dma, offset: usize) -> Result<u64>;
    fn alloc_dma(
        &mut self,
        size: usize,
        align: usize,
        direction: DmaDirection,
        coherent: bool,
    ) -> Result<Self::Dma>;
    fn alloc_dma_constrained(
        &mut self,
        size: usize,
        constraints: DmaConstraints,
        direction: DmaDirection,
        coherent: bool,
    ) -> Result<Self::Dma> {
        if constraints.max_device_address != u64::MAX
            || constraints.max_segments == 0
            || constraints.max_segment_size < size
        {
            return Err(Error::Limit);
        }
        self.alloc_dma(size, constraints.alignment, direction, coherent)
    }
    fn dma_read(&mut self, dma: &Self::Dma, range: Range<usize>, out: &mut [u8]) -> Result<()>;
    fn dma_write(&mut self, dma: &Self::Dma, range: Range<usize>, bytes: &[u8]) -> Result<()>;
    fn sync_for_cpu(&mut self, dma: &Self::Dma, range: Range<usize>) -> Result<()>;
    fn sync_for_device(&mut self, dma: &Self::Dma, range: Range<usize>) -> Result<()>;
    fn open_interrupt(&mut self, vector: u32) -> Result<Self::Interrupt>;
    fn wait_interrupt(
        &mut self,
        interrupt: &Self::Interrupt,
        deadline_ns: u64,
    ) -> Result<Option<IrqEvent>>;
    /// Wait for any of several interrupts with one absolute monotonic
    /// deadline. Implementations must not serialize blocking waits.
    fn wait_any(
        &mut self,
        interrupts: &[&Self::Interrupt],
        deadline_ns: u64,
    ) -> Result<Vec<IrqEvent>>;
    fn reset(&mut self) -> Result<u64>;
    fn release_region(&mut self, region: Self::Region);
    fn release_dma(&mut self, dma: Self::Dma);
    fn release_interrupt(&mut self, interrupt: Self::Interrupt);
}

struct Shared<B: Backend>(Rc<RefCell<B>>);
impl<B: Backend> Clone for Shared<B> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

pub struct Device<B: Backend> {
    shared: Shared<B>,
}
impl<B: Backend> Device<B> {
    #[doc(hidden)]
    pub fn from_backend(backend: B) -> Self {
        Self {
            shared: Shared(Rc::new(RefCell::new(backend))),
        }
    }
    pub fn generation(&self) -> u64 {
        self.shared.0.borrow().generation()
    }
    pub fn open_region(&self, index: u8) -> Result<MmioRegion<B>> {
        let mut b = self.shared.0.borrow_mut();
        let generation = b.generation();
        let token = b.open_region(index)?;
        let len = b.region_len(&token);
        drop(b);
        Ok(MmioRegion {
            shared: self.shared.clone(),
            token: Some(token),
            generation,
            len,
        })
    }
    pub fn alloc_coherent<D: Direction>(
        &self,
        size: usize,
        align: usize,
    ) -> Result<CoherentDma<B, D>> {
        self.alloc_coherent_with_constraints(size, DmaConstraints::new(align))
    }
    pub fn alloc_coherent_with_constraints<D: Direction>(
        &self,
        size: usize,
        constraints: DmaConstraints,
    ) -> Result<CoherentDma<B, D>> {
        DmaBuffer::allocate(
            self.shared.clone(),
            size,
            constraints,
            direction::<D>(),
            true,
        )
        .map(CoherentDma)
    }
    pub fn alloc_streaming<D: Direction>(
        &self,
        size: usize,
        align: usize,
    ) -> Result<StreamingDma<B, D>> {
        self.alloc_streaming_with_constraints(size, DmaConstraints::new(align))
    }
    pub fn alloc_streaming_with_constraints<D: Direction>(
        &self,
        size: usize,
        constraints: DmaConstraints,
    ) -> Result<StreamingDma<B, D>> {
        DmaBuffer::allocate(
            self.shared.clone(),
            size,
            constraints,
            direction::<D>(),
            false,
        )
        .map(StreamingDma)
    }
    pub fn open_interrupt(&self, vector: u32) -> Result<Interrupt<B>> {
        let mut b = self.shared.0.borrow_mut();
        let generation = b.generation();
        let token = b.open_interrupt(vector)?;
        drop(b);
        Ok(Interrupt {
            shared: self.shared.clone(),
            token: Some(token),
            generation,
        })
    }
    pub fn reset(&self) -> Result<u64> {
        self.shared.0.borrow_mut().reset()
    }
}

fn direction<D: Direction>() -> DmaDirection {
    if core::any::TypeId::of::<D>() == core::any::TypeId::of::<ToDevice>() {
        DmaDirection::ToDevice
    } else if core::any::TypeId::of::<D>() == core::any::TypeId::of::<FromDevice>() {
        DmaDirection::FromDevice
    } else {
        DmaDirection::Bidirectional
    }
}
fn checked_range(offset: usize, length: usize, total: usize) -> Result<Range<usize>> {
    let end = offset
        .checked_add(length)
        .filter(|x| *x <= total)
        .ok_or(Error::OutOfBounds)?;
    Ok(offset..end)
}
fn current<B: Backend>(shared: &Shared<B>, generation: u64) -> Result<()> {
    if shared.0.borrow().generation() == generation {
        Ok(())
    } else {
        Err(Error::StaleHandle)
    }
}

pub struct MmioRegion<B: Backend> {
    shared: Shared<B>,
    token: Option<B::Region>,
    generation: u64,
    len: usize,
}
impl<B: Backend> MmioRegion<B> {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn read_u32(&self, offset: usize) -> Result<u32> {
        self.check(offset, 4)?;
        self.shared
            .0
            .borrow_mut()
            .read_u32(self.token.as_ref().unwrap(), offset)
    }
    pub fn write_u32(&self, offset: usize, value: u32) -> Result<()> {
        self.check(offset, 4)?;
        self.shared
            .0
            .borrow_mut()
            .write_u32(self.token.as_ref().unwrap(), offset, value)
    }
    pub fn write_device_address<D: Direction>(
        &self,
        low: usize,
        high: Option<usize>,
        address: DeviceAddress<'_, B, D>,
    ) -> Result<()> {
        self.check(low, 4)?;
        if let Some(h) = high {
            self.check(h, 4)?;
        }
        if !Rc::ptr_eq(&self.shared.0, &address.dma.shared.0) {
            return Err(Error::Invalid);
        }
        current(&self.shared, address.dma.generation)?;
        self.shared.0.borrow_mut().write_dma_address(
            self.token.as_ref().unwrap(),
            low,
            high,
            address.dma.token.as_ref().unwrap(),
            address.offset,
        )
    }
    fn check(&self, o: usize, n: usize) -> Result<()> {
        current(&self.shared, self.generation)?;
        if !o.is_multiple_of(4) {
            return Err(Error::Invalid);
        }
        checked_range(o, n, self.len).map(|_| ())
    }
}
impl<B: Backend> Drop for MmioRegion<B> {
    fn drop(&mut self) {
        if let Some(t) = self.token.take() {
            self.shared.0.borrow_mut().release_region(t)
        }
    }
}

struct DmaBuffer<B: Backend, D: Direction> {
    shared: Shared<B>,
    token: Option<B::Dma>,
    generation: u64,
    bytes: Vec<u8>,
    _d: PhantomData<D>,
}
impl<B: Backend, D: Direction> DmaBuffer<B, D> {
    fn allocate(
        shared: Shared<B>,
        size: usize,
        constraints: DmaConstraints,
        dir: DmaDirection,
        coherent: bool,
    ) -> Result<Self> {
        if size == 0 || constraints.alignment == 0 || !constraints.alignment.is_power_of_two() {
            return Err(Error::Invalid);
        }
        let mut b = shared.0.borrow_mut();
        let generation = b.generation();
        let token = b.alloc_dma_constrained(size, constraints, dir, coherent)?;
        drop(b);
        Ok(Self {
            shared,
            token: Some(token),
            generation,
            bytes: vec![0; size],
            _d: PhantomData,
        })
    }
    fn range(&self, o: usize, n: usize) -> Result<Range<usize>> {
        current(&self.shared, self.generation)?;
        checked_range(o, n, self.bytes.len())
    }
}
impl<B: Backend, D: Direction> Drop for DmaBuffer<B, D> {
    fn drop(&mut self) {
        if let Some(t) = self.token.take() {
            self.shared.0.borrow_mut().release_dma(t)
        }
    }
}

pub struct DeviceAddress<'a, B: Backend, D: Direction> {
    dma: &'a DmaBuffer<B, D>,
    offset: usize,
    bits: u64,
}
impl<B: Backend, D: Direction> DeviceAddress<'_, B, D> {
    /// The non-forgeable allocation-derived IOMMU-visible address.
    pub fn bits(&self) -> u64 {
        self.bits
    }
    pub fn lo32(&self) -> u32 {
        self.bits as u32
    }
    pub fn hi32(&self) -> u32 {
        (self.bits >> 32) as u32
    }
}
pub struct CoherentDma<B: Backend, D: Direction>(DmaBuffer<B, D>);
impl<B: Backend, D: Direction> CoherentDma<B, D> {
    pub fn len(&self) -> usize {
        self.0.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.bytes.is_empty()
    }
    pub fn device_address_at(&self, offset: usize) -> Result<DeviceAddress<'_, B, D>> {
        if offset >= self.len() {
            return Err(Error::OutOfBounds);
        }
        let bits = self
            .0
            .shared
            .0
            .borrow()
            .dma_device_address(self.0.token.as_ref().unwrap(), offset)?;
        Ok(DeviceAddress {
            dma: &self.0,
            offset,
            bits,
        })
    }
    pub fn device_address(&self, offset: usize) -> Result<DeviceAddress<'_, B, D>> {
        self.device_address_at(offset)
    }
}
impl<B: Backend, D: CpuWrite> CoherentDma<B, D> {
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<()> {
        let r = self.0.range(offset, bytes.len())?;
        self.0.bytes[r.clone()].copy_from_slice(bytes);
        self.0
            .shared
            .0
            .borrow_mut()
            .dma_write(self.0.token.as_ref().unwrap(), r, bytes)
    }
}
impl<B: Backend, D: CpuRead> CoherentDma<B, D> {
    pub fn read(&mut self, offset: usize, out: &mut [u8]) -> Result<()> {
        let r = self.0.range(offset, out.len())?;
        self.0
            .shared
            .0
            .borrow_mut()
            .dma_read(self.0.token.as_ref().unwrap(), r.clone(), out)?;
        self.0.bytes[r].copy_from_slice(out);
        Ok(())
    }
}

pub struct StreamingDma<B: Backend, D: Direction>(DmaBuffer<B, D>);
impl<B: Backend, D: Direction> StreamingDma<B, D> {
    pub fn len(&self) -> usize {
        self.0.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.bytes.is_empty()
    }
    pub fn device_address_at(&self, offset: usize) -> Result<DeviceAddress<'_, B, D>> {
        if offset >= self.len() {
            return Err(Error::OutOfBounds);
        }
        let bits = self
            .0
            .shared
            .0
            .borrow()
            .dma_device_address(self.0.token.as_ref().unwrap(), offset)?;
        Ok(DeviceAddress {
            dma: &self.0,
            offset,
            bits,
        })
    }
    pub fn device_address(&self, offset: usize) -> Result<DeviceAddress<'_, B, D>> {
        self.device_address_at(offset)
    }
}
impl<B: Backend, D: CpuWrite> StreamingDma<B, D> {
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<()> {
        let r = self.0.range(offset, bytes.len())?;
        self.0.bytes[r].copy_from_slice(bytes);
        Ok(())
    }
    pub fn sync_for_device(&mut self, offset: usize, length: usize) -> Result<()> {
        let r = self.0.range(offset, length)?;
        let bytes = &self.0.bytes[r.clone()];
        let mut b = self.0.shared.0.borrow_mut();
        b.dma_write(self.0.token.as_ref().unwrap(), r.clone(), bytes)?;
        b.sync_for_device(self.0.token.as_ref().unwrap(), r)
    }
}
impl<B: Backend, D: CpuRead> StreamingDma<B, D> {
    pub fn sync_for_cpu(&mut self, offset: usize, length: usize) -> Result<()> {
        let r = self.0.range(offset, length)?;
        let mut b = self.0.shared.0.borrow_mut();
        b.sync_for_cpu(self.0.token.as_ref().unwrap(), r.clone())?;
        b.dma_read(
            self.0.token.as_ref().unwrap(),
            r.clone(),
            &mut self.0.bytes[r],
        )
    }
    pub fn read(&self, offset: usize, out: &mut [u8]) -> Result<()> {
        let r = self.0.range(offset, out.len())?;
        out.copy_from_slice(&self.0.bytes[r]);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqEvent {
    pub vector: u32,
    pub count: u64,
    pub at_ns: u64,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InterruptSet {
    events: Vec<IrqEvent>,
}
impl InterruptSet {
    pub fn events(&self) -> &[IrqEvent] {
        &self.events
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}
pub struct Interrupt<B: Backend> {
    shared: Shared<B>,
    token: Option<B::Interrupt>,
    generation: u64,
}
impl<B: Backend> Interrupt<B> {
    pub fn wait_until(&self, deadline_ns: u64) -> Result<Option<IrqEvent>> {
        current(&self.shared, self.generation)?;
        self.shared
            .0
            .borrow_mut()
            .wait_interrupt(self.token.as_ref().unwrap(), deadline_ns)
    }
    pub fn wait_any(interrupts: &[&Self], deadline_ns: u64) -> Result<InterruptSet> {
        let first = interrupts.first().ok_or(Error::Invalid)?;
        for interrupt in interrupts {
            if !Rc::ptr_eq(&first.shared.0, &interrupt.shared.0) {
                return Err(Error::Invalid);
            }
            current(&interrupt.shared, interrupt.generation)?;
        }
        let tokens = interrupts
            .iter()
            .map(|interrupt| interrupt.token.as_ref().unwrap())
            .collect::<Vec<_>>();
        let events = first.shared.0.borrow_mut().wait_any(&tokens, deadline_ns)?;
        Ok(InterruptSet { events })
    }
}
impl<B: Backend> Drop for Interrupt<B> {
    fn drop(&mut self) {
        if let Some(t) = self.token.take() {
            self.shared.0.borrow_mut().release_interrupt(t)
        }
    }
}
