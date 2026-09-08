#![no_std]
#![forbid(unsafe_code)]

//! Capability-shaped hardware access for native safe Rust drivers.
//!
//! This crate deliberately has no host-I/O dependencies. Backends own all raw
//! resources; drivers receive only bounded handles tied to a device generation.

extern crate alloc;

use alloc::{rc::Rc, vec, vec::Vec};
use core::{cell::RefCell, marker::PhantomData, mem, ops::Range};
use zerocopy::{FromBytes, IntoBytes};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Invalid,
    OutOfBounds,
    StaleHandle,
    Limit,
    DeviceFault,
    Timeout,
    Unsupported,
}
pub type Result<T> = core::result::Result<T, Error>;

mod sealed {
    pub trait Direction {}
    pub trait PodOnce: Sized {
        fn from_u32(value: u32) -> Self;
        fn into_u32(self) -> u32;
    }
}
pub trait Direction: sealed::Direction + 'static {}
pub trait CpuWrite: Direction {}
pub trait CpuRead: Direction {}
pub trait DeviceRead: Direction {}
pub trait DeviceWrite: Direction {}
/// A DMA value supported by one naturally aligned, non-tearing backend access.
///
/// This is sealed to the descriptor word width backends currently guarantee.
pub trait PodOnce: sealed::PodOnce + FromBytes + IntoBytes + Copy {}
impl sealed::PodOnce for u32 {
    fn from_u32(value: u32) -> Self {
        value
    }
    fn into_u32(self) -> u32 {
        self
    }
}
impl PodOnce for u32 {}

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
    /// Whether streaming DMA mappings are cache coherent for this device.
    fn is_cache_coherent(&self) -> bool {
        true
    }
    fn open_region(&mut self, index: u8) -> Result<Self::Region>;
    fn region_len(&self, region: &Self::Region) -> usize;
    /// MMIO load with acquire ordering: subsequent CPU reads from coherent
    /// DMA observe device writes completed before the register became visible.
    fn read_u32(&mut self, region: &Self::Region, offset: usize) -> Result<u32>;
    /// MMIO store with release ordering relative to preceding CPU writes to
    /// coherent DMA and preceding `sync_for_device` operations on the same
    /// thread. In particular, a descriptor write must become visible before a
    /// later doorbell store performed through this method. Returning `Err`
    /// guarantees that the device did not observe the requested store.
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
    /// Notify a backend that a streaming CPU shadow has been modified but not
    /// yet synchronized for the device. Production backends may ignore this;
    /// deterministic backends use it to reject missing ownership transitions.
    fn streaming_cpu_dirty(&mut self, _dma: &Self::Dma, _range: Range<usize>) -> Result<()> {
        Ok(())
    }
    /// Validate a CPU read from a streaming shadow. Deterministic
    /// non-coherent backends use this to catch a missing `sync_for_cpu`.
    fn streaming_cpu_read(&mut self, _dma: &Self::Dma, _range: Range<usize>) -> Result<()> {
        Ok(())
    }
    /// Perform one naturally aligned, non-tearing 32-bit DMA-memory load.
    fn dma_read_once_u32(&mut self, _dma: &Self::Dma, _offset: usize) -> Result<u32> {
        Err(Error::Invalid)
    }
    /// Perform one naturally aligned, non-tearing 32-bit DMA-memory store.
    fn dma_write_once_u32(&mut self, _dma: &Self::Dma, _offset: usize, _value: u32) -> Result<()> {
        Err(Error::Invalid)
    }
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
impl<B: Backend> Clone for Device<B> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
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
    pub fn is_cache_coherent(&self) -> bool {
        self.shared.0.borrow().is_cache_coherent()
    }
    pub fn open_region(&self, index: u8) -> Result<MmioRegion<B>> {
        let mut b = self.shared.0.borrow_mut();
        let generation = b.generation();
        let token = b.open_region(index)?;
        let len = b.region_len(&token);
        drop(b);
        Ok(MmioRegion {
            allocation: Rc::new(RegionAllocation {
                shared: self.shared.clone(),
                token: Some(token),
            }),
            generation,
            offset: 0,
            len,
            offset_mapper: None,
        })
    }

    /// Open a caller-selected register window only when its complete exposed
    /// size matches the hardware contract.
    pub fn open_region_sized(&self, index: u8, expected_size: usize) -> Result<MmioRegion<B>> {
        if expected_size == 0 {
            return Err(Error::Invalid);
        }
        let region = self.open_region(index)?;
        if region.len() != expected_size {
            return Err(Error::OutOfBounds);
        }
        Ok(region)
    }
    /// Allocate coherent DMA storage whose entire device-visible contents are
    /// zero before this function returns.
    pub fn alloc_coherent<D: Direction>(
        &self,
        size: usize,
        align: usize,
    ) -> Result<CoherentDma<B, D>> {
        self.alloc_coherent_with_constraints(size, DmaConstraints::new(align))
    }
    /// Allocate constrained coherent DMA storage, zero-filled as for
    /// [`Device::alloc_coherent`].
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
    /// Allocate streaming DMA storage whose entire device-visible contents and
    /// CPU shadow are zero before this function returns.
    pub fn alloc_streaming<D: Direction>(
        &self,
        size: usize,
        align: usize,
    ) -> Result<StreamingDma<B, D>> {
        self.alloc_streaming_with_constraints(size, DmaConstraints::new(align))
    }
    /// Allocate constrained streaming DMA storage, zero-filled as for
    /// [`Device::alloc_streaming`].
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

struct RegionAllocation<B: Backend> {
    shared: Shared<B>,
    token: Option<B::Region>,
}
impl<B: Backend> Drop for RegionAllocation<B> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.shared.0.borrow_mut().release_region(token);
        }
    }
}

pub struct MmioRegion<B: Backend> {
    allocation: Rc<RegionAllocation<B>>,
    generation: u64,
    offset: usize,
    len: usize,
    offset_mapper: Option<fn(usize) -> Option<usize>>,
}
impl<B: Backend> MmioRegion<B> {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Return an independently owned view bounded to a sub-window of this
    /// mapping. The backend mapping is released after the last view is dropped.
    pub fn slice(&self, offset: usize, len: usize) -> Result<Self> {
        if self.offset_mapper.is_some() {
            return Err(Error::Invalid);
        }
        let range = checked_range(offset, len, self.len)?;
        current(&self.allocation.shared, self.generation)?;
        Ok(Self {
            allocation: self.allocation.clone(),
            generation: self.generation,
            offset: self
                .offset
                .checked_add(range.start)
                .ok_or(Error::OutOfBounds)?,
            len: range.len(),
            offset_mapper: None,
        })
    }
    /// Apply a device register-address translation to this complete physical
    /// mapping. Slice first when multiple independently owned views are
    /// required; slicing a translated view fails closed.
    pub fn map_offsets(mut self, mapper: fn(usize) -> Option<usize>) -> Result<Self> {
        if self.offset_mapper.is_some() {
            return Err(Error::Invalid);
        }
        self.offset_mapper = Some(mapper);
        Ok(self)
    }
    pub fn read_u32(&self, offset: usize) -> Result<u32> {
        let offset = self.mapped_offset(offset, 4)?;
        self.allocation.shared.0.borrow_mut().read_u32(
            self.allocation.token.as_ref().unwrap(),
            self.offset + offset,
        )
    }
    /// Perform one checked MMIO store. `Err` means no store occurred.
    pub fn write_u32(&self, offset: usize, value: u32) -> Result<()> {
        let offset = self.mapped_offset(offset, 4)?;
        self.allocation.shared.0.borrow_mut().write_u32(
            self.allocation.token.as_ref().unwrap(),
            self.offset + offset,
            value,
        )
    }
    pub fn write_device_address<D: Direction>(
        &self,
        low: usize,
        high: Option<usize>,
        address: DeviceAddress<'_, B, D>,
    ) -> Result<()> {
        let low = self.mapped_offset(low, 4)?;
        let high = high
            .map(|offset| self.mapped_offset(offset, 4))
            .transpose()?;
        if !Rc::ptr_eq(&self.allocation.shared.0, &address.dma.allocation.shared.0) {
            return Err(Error::Invalid);
        }
        current(&self.allocation.shared, address.dma.generation)?;
        self.allocation.shared.0.borrow_mut().write_dma_address(
            self.allocation.token.as_ref().unwrap(),
            self.offset + low,
            high.map(|offset| self.offset + offset),
            address.dma.allocation.token.as_ref().unwrap(),
            address.offset,
        )
    }
    fn mapped_offset(&self, offset: usize, width: usize) -> Result<usize> {
        current(&self.allocation.shared, self.generation)?;
        if !offset.is_multiple_of(4) {
            return Err(Error::Invalid);
        }
        let mapped = match self.offset_mapper {
            Some(mapper) => mapper(offset).ok_or(Error::OutOfBounds)?,
            None => offset,
        };
        if !mapped.is_multiple_of(4) {
            return Err(Error::Invalid);
        }
        checked_range(mapped, width, self.len)?;
        Ok(mapped)
    }
}
struct DmaAllocation<B: Backend> {
    shared: Shared<B>,
    token: Option<B::Dma>,
}
impl<B: Backend> Drop for DmaAllocation<B> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.shared.0.borrow_mut().release_dma(token);
        }
    }
}

struct DmaBuffer<B: Backend, D: Direction> {
    allocation: Rc<DmaAllocation<B>>,
    generation: u64,
    offset: usize,
    alignment: usize,
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
        // Allocation is the safe API's zero-initializing operation. Do this
        // through the backend rather than relying on allocator-specific
        // behavior so the device-visible storage and CPU shadow agree.
        let bytes = vec![0; size];
        if let Err(error) = b.dma_write(&token, 0..size, &bytes) {
            b.release_dma(token);
            return Err(error);
        }
        if !coherent
            && !b.is_cache_coherent()
            && let Err(error) = b.sync_for_device(&token, 0..size)
        {
            b.release_dma(token);
            return Err(error);
        }
        drop(b);
        Ok(Self {
            allocation: Rc::new(DmaAllocation {
                shared,
                token: Some(token),
            }),
            generation,
            offset: 0,
            alignment: constraints.alignment,
            bytes,
            _d: PhantomData,
        })
    }
    fn range(&self, o: usize, n: usize) -> Result<Range<usize>> {
        current(&self.allocation.shared, self.generation)?;
        checked_range(o, n, self.bytes.len())
    }
    fn backend_range(&self, range: Range<usize>) -> Range<usize> {
        self.offset + range.start..self.offset + range.end
    }
    fn split_at(mut self, offset: usize) -> Result<(Self, Self)> {
        current(&self.allocation.shared, self.generation)?;
        if offset == 0 || offset >= self.bytes.len() || !offset.is_multiple_of(self.alignment) {
            return Err(Error::Invalid);
        }
        let right_bytes = self.bytes.split_off(offset);
        let right = Self {
            allocation: self.allocation.clone(),
            generation: self.generation,
            offset: self.offset + offset,
            alignment: self.alignment,
            bytes: right_bytes,
            _d: PhantomData,
        };
        Ok((self, right))
    }
    fn pod_range<T>(&self, offset: usize) -> Result<Range<usize>> {
        let size = mem::size_of::<T>();
        if size == 0 || !(self.offset + offset).is_multiple_of(mem::align_of::<T>()) {
            return Err(Error::Invalid);
        }
        self.range(offset, size)
    }
    fn read_pod<T: FromBytes + IntoBytes>(&mut self, offset: usize) -> Result<T> {
        let range = self.pod_range::<T>(offset)?;
        let backend_range = self.backend_range(range.clone());
        self.allocation.shared.0.borrow_mut().dma_read(
            self.allocation.token.as_ref().unwrap(),
            backend_range,
            &mut self.bytes[range.clone()],
        )?;
        T::read_from_bytes(&self.bytes[range]).map_err(|_| Error::Invalid)
    }
    fn write_pod<T: FromBytes + IntoBytes>(&mut self, offset: usize, mut value: T) -> Result<()> {
        let range = self.pod_range::<T>(offset)?;
        let bytes = value.as_mut_bytes();
        self.bytes[range.clone()].copy_from_slice(bytes);
        let backend_range = self.backend_range(range);
        self.allocation.shared.0.borrow_mut().dma_write(
            self.allocation.token.as_ref().unwrap(),
            backend_range,
            bytes,
        )
    }
    fn read_once<T: PodOnce>(&mut self, offset: usize) -> Result<T> {
        let range = self.pod_range::<T>(offset)?;
        let backend_offset = self.backend_range(range.clone()).start;
        let value = self
            .allocation
            .shared
            .0
            .borrow_mut()
            .dma_read_once_u32(self.allocation.token.as_ref().unwrap(), backend_offset)?;
        self.bytes[range].copy_from_slice(&value.to_ne_bytes());
        Ok(<T as sealed::PodOnce>::from_u32(value))
    }
    fn write_once<T: PodOnce>(&mut self, offset: usize, value: T) -> Result<()> {
        let range = self.pod_range::<T>(offset)?;
        let backend_offset = self.backend_range(range.clone()).start;
        let value = <T as sealed::PodOnce>::into_u32(value);
        self.allocation.shared.0.borrow_mut().dma_write_once_u32(
            self.allocation.token.as_ref().unwrap(),
            backend_offset,
            value,
        )?;
        self.bytes[range].copy_from_slice(&value.to_ne_bytes());
        Ok(())
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
    /// Split one backend allocation into two independently owned contiguous
    /// views. The split must preserve the allocation's requested alignment.
    pub fn split_at(self, offset: usize) -> Result<(Self, Self)> {
        let (left, right) = self.0.split_at(offset)?;
        Ok((Self(left), Self(right)))
    }
    pub fn device_address_at(&self, offset: usize) -> Result<DeviceAddress<'_, B, D>> {
        if offset >= self.len() {
            return Err(Error::OutOfBounds);
        }
        let bits = self.0.allocation.shared.0.borrow().dma_device_address(
            self.0.allocation.token.as_ref().unwrap(),
            self.0.offset + offset,
        )?;
        Ok(DeviceAddress {
            dma: &self.0,
            offset: self.0.offset + offset,
            bits,
        })
    }
    pub fn device_address(&self, offset: usize) -> Result<DeviceAddress<'_, B, D>> {
        self.device_address_at(offset)
    }
}
impl<B: Backend, D: CpuWrite> CoherentDma<B, D> {
    /// Write DMA memory. A later [`MmioRegion::write_u32`] on the same thread
    /// is release-ordered after this write by the backend contract.
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<()> {
        let r = self.0.range(offset, bytes.len())?;
        self.0.bytes[r.clone()].copy_from_slice(bytes);
        let backend_range = self.0.backend_range(r);
        self.0.allocation.shared.0.borrow_mut().dma_write(
            self.0.allocation.token.as_ref().unwrap(),
            backend_range,
            bytes,
        )
    }
    pub fn write_pod<T: FromBytes + IntoBytes>(&mut self, offset: usize, value: T) -> Result<()> {
        self.0.write_pod(offset, value)
    }
    pub fn write_once<T: PodOnce>(&mut self, offset: usize, value: T) -> Result<()> {
        self.0.write_once(offset, value)
    }
}
impl<B: Backend, D: CpuRead> CoherentDma<B, D> {
    pub fn read(&mut self, offset: usize, out: &mut [u8]) -> Result<()> {
        let r = self.0.range(offset, out.len())?;
        let backend_range = self.0.backend_range(r.clone());
        self.0.allocation.shared.0.borrow_mut().dma_read(
            self.0.allocation.token.as_ref().unwrap(),
            backend_range,
            out,
        )?;
        self.0.bytes[r].copy_from_slice(out);
        Ok(())
    }
    pub fn read_pod<T: FromBytes + IntoBytes>(&mut self, offset: usize) -> Result<T> {
        self.0.read_pod(offset)
    }
    pub fn read_once<T: PodOnce>(&mut self, offset: usize) -> Result<T> {
        self.0.read_once(offset)
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
    /// Split one backend allocation into two independently owned contiguous
    /// views. The split must preserve the allocation's requested alignment.
    pub fn split_at(self, offset: usize) -> Result<(Self, Self)> {
        let (left, right) = self.0.split_at(offset)?;
        Ok((Self(left), Self(right)))
    }
    pub fn device_address_at(&self, offset: usize) -> Result<DeviceAddress<'_, B, D>> {
        if offset >= self.len() {
            return Err(Error::OutOfBounds);
        }
        let bits = self.0.allocation.shared.0.borrow().dma_device_address(
            self.0.allocation.token.as_ref().unwrap(),
            self.0.offset + offset,
        )?;
        Ok(DeviceAddress {
            dma: &self.0,
            offset: self.0.offset + offset,
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
        self.0.bytes[r.clone()].copy_from_slice(bytes);
        let backend_range = self.0.backend_range(r);
        self.0
            .allocation
            .shared
            .0
            .borrow_mut()
            .streaming_cpu_dirty(self.0.allocation.token.as_ref().unwrap(), backend_range)
    }
    pub fn write_pod<T: FromBytes + IntoBytes>(
        &mut self,
        offset: usize,
        mut value: T,
    ) -> Result<()> {
        let range = self.0.pod_range::<T>(offset)?;
        self.0.bytes[range.clone()].copy_from_slice(value.as_mut_bytes());
        let backend_range = self.0.backend_range(range);
        self.0
            .allocation
            .shared
            .0
            .borrow_mut()
            .streaming_cpu_dirty(self.0.allocation.token.as_ref().unwrap(), backend_range)
    }
    pub fn write_once<T: PodOnce>(&mut self, offset: usize, value: T) -> Result<()> {
        let range = self.0.pod_range::<T>(offset)?;
        let backend_range = self.0.backend_range(range.clone());
        let value = <T as sealed::PodOnce>::into_u32(value);
        let mut backend = self.0.allocation.shared.0.borrow_mut();
        backend.dma_write_once_u32(
            self.0.allocation.token.as_ref().unwrap(),
            backend_range.start,
            value,
        )?;
        if !backend.is_cache_coherent() {
            backend.sync_for_device(self.0.allocation.token.as_ref().unwrap(), backend_range)?;
        }
        self.0.bytes[range].copy_from_slice(&value.to_ne_bytes());
        Ok(())
    }
    pub fn sync_for_device(&mut self, offset: usize, length: usize) -> Result<()> {
        let r = self.0.range(offset, length)?;
        let bytes = &self.0.bytes[r.clone()];
        let backend_range = self.0.backend_range(r);
        let mut b = self.0.allocation.shared.0.borrow_mut();
        b.dma_write(
            self.0.allocation.token.as_ref().unwrap(),
            backend_range.clone(),
            bytes,
        )?;
        if b.is_cache_coherent() {
            Ok(())
        } else {
            b.sync_for_device(self.0.allocation.token.as_ref().unwrap(), backend_range)
        }
    }
}
impl<B: Backend, D: CpuRead> StreamingDma<B, D> {
    /// Transfer a streaming range to CPU ownership without copying it into the
    /// CPU shadow. Callers that retry a later shadow refresh must not repeat
    /// this transition until the range is returned to the device.
    pub fn acquire_for_cpu(&mut self, offset: usize, length: usize) -> Result<()> {
        let r = self.0.range(offset, length)?;
        let backend_range = self.0.backend_range(r);
        let mut b = self.0.allocation.shared.0.borrow_mut();
        if !b.is_cache_coherent() {
            b.sync_for_cpu(self.0.allocation.token.as_ref().unwrap(), backend_range)?;
        }
        Ok(())
    }
    /// Refresh the CPU shadow for a range already acquired by the CPU.
    pub fn refresh_for_cpu(&mut self, offset: usize, length: usize) -> Result<()> {
        let r = self.0.range(offset, length)?;
        let backend_range = self.0.backend_range(r.clone());
        let mut b = self.0.allocation.shared.0.borrow_mut();
        b.dma_read(
            self.0.allocation.token.as_ref().unwrap(),
            backend_range,
            &mut self.0.bytes[r],
        )
    }
    pub fn sync_for_cpu(&mut self, offset: usize, length: usize) -> Result<()> {
        self.acquire_for_cpu(offset, length)?;
        self.refresh_for_cpu(offset, length)
    }
    pub fn read(&self, offset: usize, out: &mut [u8]) -> Result<()> {
        let r = self.0.range(offset, out.len())?;
        let backend_range = self.0.backend_range(r.clone());
        self.0
            .allocation
            .shared
            .0
            .borrow_mut()
            .streaming_cpu_read(self.0.allocation.token.as_ref().unwrap(), backend_range)?;
        out.copy_from_slice(&self.0.bytes[r]);
        Ok(())
    }
    pub fn read_pod<T: FromBytes + IntoBytes>(&self, offset: usize) -> Result<T> {
        let range = self.0.pod_range::<T>(offset)?;
        let backend_range = self.0.backend_range(range.clone());
        self.0
            .allocation
            .shared
            .0
            .borrow_mut()
            .streaming_cpu_read(self.0.allocation.token.as_ref().unwrap(), backend_range)?;
        T::read_from_bytes(&self.0.bytes[range]).map_err(|_| Error::Invalid)
    }
    pub fn read_once<T: PodOnce>(&mut self, offset: usize) -> Result<T> {
        let range = self.0.pod_range::<T>(offset)?;
        let backend_range = self.0.backend_range(range.clone());
        let mut backend = self.0.allocation.shared.0.borrow_mut();
        if !backend.is_cache_coherent() {
            backend.sync_for_cpu(
                self.0.allocation.token.as_ref().unwrap(),
                backend_range.clone(),
            )?;
        }
        let value = backend.dma_read_once_u32(
            self.0.allocation.token.as_ref().unwrap(),
            backend_range.start,
        )?;
        self.0.bytes[range].copy_from_slice(&value.to_ne_bytes());
        Ok(<T as sealed::PodOnce>::from_u32(value))
    }
}

impl<B: Backend> StreamingDma<B, FromDevice> {
    /// Return a device-to-CPU mapping to device ownership after the CPU has
    /// consumed it. Unlike the `CpuWrite` synchronization path, this performs
    /// no CPU-shadow copy.
    pub fn prepare_for_device(&mut self, offset: usize, length: usize) -> Result<()> {
        let range = self.0.range(offset, length)?;
        let backend_range = self.0.backend_range(range);
        let mut backend = self.0.allocation.shared.0.borrow_mut();
        if backend.is_cache_coherent() {
            Ok(())
        } else {
            backend.sync_for_device(self.0.allocation.token.as_ref().unwrap(), backend_range)
        }
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
