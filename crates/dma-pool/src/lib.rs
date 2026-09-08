#![no_std]
#![forbid(unsafe_code)]

//! Direction-typed pools of fixed-size streaming DMA segments.
//!
//! Pages are allocation batches, not exposed resources. Each page is split
//! into independently owned segments which keep their common backend mapping
//! alive. Dropping a segment returns it to its originating pool while the
//! pool's free-segment high-watermark has not been reached.

extern crate alloc;

use alloc::{
    rc::{Rc, Weak},
    vec::Vec,
};
use core::{cell::RefCell, marker::PhantomData};
use drv_hardware::{
    Backend, CpuRead, CpuWrite, Device, DeviceAddress, Direction, Error, FromDevice, Result,
    StreamingDma,
};

struct Inner<B: Backend, D: Direction> {
    device: Device<B>,
    segment_size: usize,
    page_size: usize,
    alignment: usize,
    high_watermark: usize,
    free: Vec<StreamingDma<B, D>>,
}

pub struct DmaPool<B: Backend, D: Direction> {
    inner: Rc<RefCell<Inner<B, D>>>,
}

impl<B: Backend, D: Direction> Clone for DmaPool<B, D> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<B: Backend, D: Direction> DmaPool<B, D> {
    pub fn new(
        device: Device<B>,
        segment_size: usize,
        page_size: usize,
        alignment: usize,
        high_watermark: usize,
    ) -> Result<Self> {
        if segment_size == 0
            || page_size < segment_size
            || !page_size.is_multiple_of(segment_size)
            || alignment == 0
            || !alignment.is_power_of_two()
            || !segment_size.is_multiple_of(alignment)
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            inner: Rc::new(RefCell::new(Inner {
                device,
                segment_size,
                page_size,
                alignment,
                high_watermark,
                free: Vec::new(),
            })),
        })
    }

    pub fn allocate(&self) -> Result<DmaSegment<B, D>> {
        let dma = {
            let mut inner = self.inner.borrow_mut();
            if let Some(dma) = inner.free.pop() {
                dma
            } else {
                refill(&mut inner)?
            }
        };
        Ok(DmaSegment {
            dma: Some(dma),
            pool: Rc::downgrade(&self.inner),
            _direction: PhantomData,
        })
    }

    pub fn free_segments(&self) -> usize {
        self.inner.borrow().free.len()
    }

    pub fn segment_size(&self) -> usize {
        self.inner.borrow().segment_size
    }
}

fn refill<B: Backend, D: Direction>(inner: &mut Inner<B, D>) -> Result<StreamingDma<B, D>> {
    let mut remainder = inner
        .device
        .alloc_streaming::<D>(inner.page_size, inner.alignment)?;
    let count = inner.page_size / inner.segment_size;
    for _ in 1..count {
        let (segment, rest) = remainder.split_at(inner.segment_size)?;
        if inner.free.len() < inner.high_watermark {
            inner.free.push(segment);
        }
        remainder = rest;
    }
    Ok(remainder)
}

pub struct DmaSegment<B: Backend, D: Direction> {
    dma: Option<StreamingDma<B, D>>,
    pool: Weak<RefCell<Inner<B, D>>>,
    _direction: PhantomData<D>,
}

impl<B: Backend, D: Direction> DmaSegment<B, D> {
    fn dma(&self) -> &StreamingDma<B, D> {
        self.dma.as_ref().expect("live DMA segment")
    }
    fn dma_mut(&mut self) -> &mut StreamingDma<B, D> {
        self.dma.as_mut().expect("live DMA segment")
    }
    pub fn len(&self) -> usize {
        self.dma().len()
    }
    pub fn is_empty(&self) -> bool {
        self.dma().is_empty()
    }
    pub fn device_address(&self, offset: usize) -> Result<DeviceAddress<'_, B, D>> {
        self.dma().device_address(offset)
    }
}

impl<B: Backend, D: CpuWrite> DmaSegment<B, D> {
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<()> {
        self.dma_mut().write(offset, bytes)
    }
    pub fn sync_for_device(&mut self, offset: usize, len: usize) -> Result<()> {
        self.dma_mut().sync_for_device(offset, len)
    }
}

impl<B: Backend, D: CpuRead> DmaSegment<B, D> {
    pub fn read(&self, offset: usize, bytes: &mut [u8]) -> Result<()> {
        self.dma().read(offset, bytes)
    }
    pub fn sync_for_cpu(&mut self, offset: usize, len: usize) -> Result<()> {
        self.dma_mut().sync_for_cpu(offset, len)
    }
}

impl<B: Backend> DmaSegment<B, FromDevice> {
    /// Return this RX segment to device ownership without copying its CPU
    /// shadow, ready for republishing on a receive ring.
    pub fn prepare_for_device(&mut self) -> Result<()> {
        let len = self.len();
        self.dma_mut().prepare_for_device(0, len)
    }
}

impl<B: Backend, D: Direction> Drop for DmaSegment<B, D> {
    fn drop(&mut self) {
        let Some(dma) = self.dma.take() else { return };
        let Some(pool) = self.pool.upgrade() else {
            return;
        };
        let mut inner = pool.borrow_mut();
        if inner.free.len() < inner.high_watermark {
            inner.free.push(dma);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drv_hardware::{Bidirectional, FromDevice};
    use drv_hardware_backends::DeterministicBackend;

    #[test]
    fn inherits_zero_fill_and_carves_contiguous_segments() {
        let pool = DmaPool::<_, Bidirectional>::new(DeterministicBackend::device(), 64, 256, 16, 8)
            .unwrap();
        let first = pool.allocate().unwrap();
        let second = pool.allocate().unwrap();
        let mut bytes = [0xff; 64];
        first.read(0, &mut bytes).unwrap();
        assert_eq!(bytes, [0; 64]);
        let a = first.device_address(0).unwrap().bits();
        let b = second.device_address(0).unwrap().bits();
        assert_eq!(a.abs_diff(b), 64);
    }

    #[test]
    fn dropped_segments_are_reused_up_to_high_watermark() {
        let pool = DmaPool::<_, Bidirectional>::new(DeterministicBackend::device(), 64, 128, 16, 2)
            .unwrap();
        let first = pool.allocate().unwrap();
        let address = first.device_address(0).unwrap().bits();
        drop(first);
        assert_eq!(pool.free_segments(), 2);
        let reused = pool.allocate().unwrap();
        assert_eq!(reused.device_address(0).unwrap().bits(), address);
    }

    #[test]
    fn direction_is_static_and_configuration_is_checked() {
        let device = DeterministicBackend::noncoherent_device();
        assert!(matches!(
            DmaPool::<_, FromDevice>::new(device.clone(), 60, 256, 16, 1),
            Err(Error::Invalid)
        ));
        let pool = DmaPool::<_, FromDevice>::new(device, 64, 128, 16, 2).unwrap();
        let mut rx = pool.allocate().unwrap();
        rx.sync_for_cpu(0, 64).unwrap();
        let mut bytes = [0xff; 64];
        rx.read(0, &mut bytes).unwrap();
        assert_eq!(bytes, [0; 64]);
        rx.prepare_for_device().unwrap();
    }
}
