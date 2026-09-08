#![forbid(unsafe_code)]

use drv_hardware::{Backend, Device, DmaConstraints, DmaDirection, Error, IrqEvent, Result};
use std::{cell::RefCell, collections::HashMap, ops::Range, rc::Rc};

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

const FIRST_IOVA: u64 = 0x1000_0000;
struct Dma {
    bytes: Vec<u8>,
    iova: u64,
    direction: DmaDirection,
    coherent: bool,
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
        }
    }
}
impl DeterministicBackend {
    pub fn device() -> Device<Self> {
        Device::from_backend(Self::default())
    }
    pub fn recording_device() -> (Device<Self>, OperationLog) {
        let operations = Rc::new(RefCell::new(Vec::new()));
        let backend = Self {
            operations: Some(operations.clone()),
            ..Self::default()
        };
        (Device::from_backend(backend), operations)
    }
    fn dma(&self, id: &u64) -> Result<&Dma> {
        self.dmas.get(id).ok_or(Error::StaleHandle)
    }
    fn dma_mut(&mut self, id: &u64) -> Result<&mut Dma> {
        self.dmas.get_mut(id).ok_or(Error::StaleHandle)
    }
}

impl Backend for DeterministicBackend {
    type Region = u8;
    type Dma = u64;
    type Interrupt = u32;
    fn generation(&self) -> u64 {
        self.generation
    }
    fn open_region(&mut self, index: u8) -> Result<u8> {
        if index != 0 {
            return Err(Error::Invalid);
        }
        self.live_regions += 1;
        Ok(index)
    }
    fn region_len(&self, _: &u8) -> usize {
        0x10_0000
    }
    fn read_u32(&mut self, region: &u8, offset: usize) -> Result<u32> {
        let value = match offset {
            0x24 => self.pending.into(),
            _ => 0,
        };
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
        if let Some(log) = &self.operations {
            log.borrow_mut().push(Operation::WriteU32 {
                region: *region,
                offset,
                value,
            });
        }
        match (offset, value) {
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
                    self.dmas
                        .get_mut(&destination.0)
                        .ok_or(Error::StaleHandle)?
                        .bytes[destination.1..end]
                        .copy_from_slice(&self.edu_buffer[..self.count]);
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
                    self.edu_buffer[..self.count].copy_from_slice(
                        &self.dmas.get(&source.0).ok_or(Error::StaleHandle)?.bytes[source.1..end],
                    );
                }
                self.pending = value & 4 != 0;
                Ok(())
            }
            _ => Ok(()),
        }
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
        if size > 4096 {
            return Err(Error::Limit);
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
                bytes: vec![0; size],
                iova: self.next,
                direction,
                coherent,
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
        let d = self.dma(dma)?;
        if matches!(d.direction, DmaDirection::ToDevice) {
            return Err(Error::Invalid);
        }
        out.copy_from_slice(&d.bytes[r]);
        Ok(())
    }
    fn dma_write(&mut self, dma: &u64, r: Range<usize>, bytes: &[u8]) -> Result<()> {
        let d = self.dma_mut(dma)?;
        if matches!(d.direction, DmaDirection::FromDevice) {
            return Err(Error::Invalid);
        }
        d.bytes[r].copy_from_slice(bytes);
        Ok(())
    }
    fn sync_for_cpu(&mut self, dma: &u64, range: Range<usize>) -> Result<()> {
        let d = self.dma(dma)?;
        if d.coherent || matches!(d.direction, DmaDirection::ToDevice) {
            Err(Error::Invalid)
        } else {
            if let Some(log) = &self.operations {
                log.borrow_mut()
                    .push(Operation::SyncForCpu { dma: *dma, range });
            }
            Ok(())
        }
    }
    fn sync_for_device(&mut self, dma: &u64, range: Range<usize>) -> Result<()> {
        let d = self.dma(dma)?;
        if d.coherent || matches!(d.direction, DmaDirection::FromDevice) {
            Err(Error::Invalid)
        } else {
            if let Some(log) = &self.operations {
                log.borrow_mut()
                    .push(Operation::SyncForDevice { dma: *dma, range });
            }
            Ok(())
        }
    }
    fn open_interrupt(&mut self, vector: u32) -> Result<u32> {
        if vector != 0 {
            return Err(Error::Invalid);
        }
        self.live_irqs += 1;
        Ok(vector)
    }
    fn wait_interrupt(&mut self, i: &u32, deadline: u64) -> Result<Option<IrqEvent>> {
        self.now = self.now.max(deadline);
        if self.pending {
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
    fn reset(&mut self) -> Result<u64> {
        self.generation += 1;
        self.dmas.clear();
        self.pending = false;
        Ok(self.generation)
    }
    fn release_region(&mut self, _: u8) {
        self.live_regions -= 1
    }
    fn release_dma(&mut self, id: u64) {
        self.dmas.remove(&id);
    }
    fn release_interrupt(&mut self, _: u32) {
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
    fn safe_sequence_covers_dma_mmio_irq_reset_and_bounds() {
        let dev = DeterministicBackend::device();
        assert_eq!(run_edu_sequence(&dev).unwrap(), [1, 2, 3, 4]);
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
}
