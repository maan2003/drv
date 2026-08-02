//! Audited Linux boundary for the QEMU edu vertical slice.
#![cfg(target_os = "linux")]

use drv_hardware::{Backend, Device, DmaConstraints, DmaDirection, Error, IrqEvent, Result};
use drv_hardware_backends::run_edu_sequence;
use std::{
    fs::{File, OpenOptions},
    io::Read,
    ops::Range,
    os::fd::{AsRawFd, FromRawFd, RawFd},
    ptr::NonNull,
};

const VFIO_TYPE: u64 = b';' as u64;
const VFIO_BASE: u64 = 100;
const VFIO_DEVICE_GET_REGION_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 8);
const VFIO_DEVICE_SET_IRQS: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 10);
const VFIO_DEVICE_RESET: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 11);
const VFIO_DEVICE_BIND_IOMMUFD: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 18);
const VFIO_DEVICE_ATTACH_IOMMUFD_PT: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 19);
const IOMMU_DESTROY: u64 = (VFIO_TYPE << 8) | 0x80;
const IOMMU_IOAS_ALLOC: u64 = (VFIO_TYPE << 8) | 0x81;
const IOMMU_IOAS_MAP: u64 = (VFIO_TYPE << 8) | 0x85;
const IOMMU_IOAS_UNMAP: u64 = (VFIO_TYPE << 8) | 0x86;
const MAP_FIXED: u32 = 1;
const MAP_WRITEABLE: u32 = 2;
const MAP_READABLE: u32 = 4;
const VFIO_IRQ_SET_DATA_EVENTFD: u32 = 1 << 2;
const VFIO_IRQ_SET_DATA_NONE: u32 = 1;
const VFIO_IRQ_SET_ACTION_UNMASK: u32 = 1 << 4;
const VFIO_IRQ_SET_ACTION_TRIGGER: u32 = 1 << 5;
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_SHARED: i32 = 1;
const MAP_PRIVATE: i32 = 2;
const MAP_ANONYMOUS: i32 = 0x20;

#[repr(C)]
#[derive(Default)]
struct Bind {
    argsz: u32,
    flags: u32,
    iommufd: i32,
    out_devid: u32,
}
#[repr(C)]
#[derive(Default)]
struct Attach {
    argsz: u32,
    flags: u32,
    pt_id: u32,
}
#[repr(C)]
#[derive(Default)]
struct RegionInfo {
    argsz: u32,
    flags: u32,
    index: u32,
    cap_offset: u32,
    size: u64,
    offset: u64,
}
#[repr(C)]
#[derive(Default)]
struct IoasAlloc {
    size: u32,
    flags: u32,
    out_ioas_id: u32,
}
#[repr(C)]
#[derive(Default)]
struct IoasMap {
    size: u32,
    flags: u32,
    ioas_id: u32,
    _pad: u32,
    user_va: u64,
    length: u64,
    iova: u64,
}
#[repr(C)]
#[derive(Default)]
struct IoasUnmap {
    size: u32,
    ioas_id: u32,
    iova: u64,
    length: u64,
}
#[repr(C)]
#[derive(Default)]
struct Destroy {
    size: u32,
    id: u32,
}
#[repr(C)]
struct IrqSet {
    argsz: u32,
    flags: u32,
    index: u32,
    start: u32,
    count: u32,
    eventfd: i32,
}
#[repr(C)]
struct IrqHeader {
    argsz: u32,
    flags: u32,
    index: u32,
    start: u32,
    count: u32,
}

unsafe extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn eventfd(init: u32, flags: i32) -> i32;
}
fn ioctl_mut<T>(fd: RawFd, request: u64, value: &mut T) -> Result<()> {
    if unsafe { ioctl(fd, request, value) } < 0 {
        eprintln!("ioctl {request:#x}: {}", std::io::Error::last_os_error());
        Err(Error::DeviceFault)
    } else {
        Ok(())
    }
}
fn page_map(len: usize, fd: RawFd, offset: i64, flags: i32) -> Result<NonNull<u8>> {
    NonNull::new(unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            PROT_READ | PROT_WRITE,
            flags,
            fd,
            offset,
        )
    })
    .filter(|p| p.as_ptr() as isize != -1)
    .ok_or(Error::DeviceFault)
}

struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
}
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { munmap(self.ptr.as_ptr(), self.len) };
    }
}
struct Dma {
    mapping: Mapping,
    iova: u64,
    direction: DmaDirection,
}
struct LinuxVfio {
    device: Option<File>,
    iommu: File,
    ioas: u32,
    generation: u64,
    irq: Option<File>,
}

impl LinuxVfio {
    fn device_fd(&self) -> RawFd {
        self.device.as_ref().expect("live VFIO device").as_raw_fd()
    }
    fn open(path: &str) -> Result<Self> {
        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|_| Error::DeviceFault)?;
        let iommu = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/iommu")
            .map_err(|_| Error::DeviceFault)?;
        let mut bind = Bind {
            argsz: std::mem::size_of::<Bind>() as u32,
            iommufd: iommu.as_raw_fd(),
            ..Default::default()
        };
        ioctl_mut(device.as_raw_fd(), VFIO_DEVICE_BIND_IOMMUFD, &mut bind)?;
        let mut alloc = IoasAlloc {
            size: std::mem::size_of::<IoasAlloc>() as u32,
            ..Default::default()
        };
        ioctl_mut(iommu.as_raw_fd(), IOMMU_IOAS_ALLOC, &mut alloc)?;
        let mut attach = Attach {
            argsz: std::mem::size_of::<Attach>() as u32,
            pt_id: alloc.out_ioas_id,
            ..Default::default()
        };
        ioctl_mut(
            device.as_raw_fd(),
            VFIO_DEVICE_ATTACH_IOMMUFD_PT,
            &mut attach,
        )?;
        Ok(Self {
            device: Some(device),
            iommu,
            ioas: alloc.out_ioas_id,
            generation: 1,
            irq: None,
        })
    }
}
impl Drop for LinuxVfio {
    fn drop(&mut self) {
        self.irq.take();
        self.device.take();
        let mut d = Destroy {
            size: std::mem::size_of::<Destroy>() as u32,
            id: self.ioas,
        };
        let _ = ioctl_mut(self.iommu.as_raw_fd(), IOMMU_DESTROY, &mut d);
    }
}

impl Backend for LinuxVfio {
    type Region = Mapping;
    type Dma = Dma;
    type Interrupt = ();
    fn generation(&self) -> u64 {
        self.generation
    }
    fn open_region(&mut self, index: u8) -> Result<Mapping> {
        let mut info = RegionInfo {
            argsz: std::mem::size_of::<RegionInfo>() as u32,
            index: index as u32,
            ..Default::default()
        };
        ioctl_mut(self.device_fd(), VFIO_DEVICE_GET_REGION_INFO, &mut info)?;
        let ptr = page_map(
            info.size as usize,
            self.device_fd(),
            info.offset as i64,
            MAP_SHARED,
        )?;
        Ok(Mapping {
            ptr,
            len: info.size as usize,
        })
    }
    fn region_len(&self, r: &Mapping) -> usize {
        r.len
    }
    fn read_u32(&mut self, r: &Mapping, o: usize) -> Result<u32> {
        Ok(unsafe { std::ptr::read_volatile(r.ptr.as_ptr().add(o).cast()) })
    }
    fn write_u32(&mut self, r: &Mapping, o: usize, v: u32) -> Result<()> {
        unsafe { std::ptr::write_volatile(r.ptr.as_ptr().add(o).cast(), v) };
        Ok(())
    }
    fn write_dma_address(
        &mut self,
        r: &Mapping,
        low: usize,
        high: Option<usize>,
        d: &Dma,
        o: usize,
    ) -> Result<()> {
        let a = d.iova.checked_add(o as u64).ok_or(Error::OutOfBounds)?;
        self.write_u32(r, low, a as u32)?;
        if let Some(h) = high {
            self.write_u32(r, h, (a >> 32) as u32)?;
        }
        Ok(())
    }
    fn alloc_dma(
        &mut self,
        size: usize,
        _: usize,
        direction: DmaDirection,
        _: bool,
    ) -> Result<Dma> {
        let len = size.next_multiple_of(4096);
        let mapping = Mapping {
            ptr: page_map(len, -1, 0, MAP_PRIVATE | MAP_ANONYMOUS)?,
            len,
        };
        let mut map = IoasMap {
            size: std::mem::size_of::<IoasMap>() as u32,
            flags: MAP_FIXED
                | match direction {
                    DmaDirection::ToDevice => MAP_READABLE,
                    DmaDirection::FromDevice => MAP_WRITEABLE,
                    DmaDirection::Bidirectional => MAP_READABLE | MAP_WRITEABLE,
                },
            ioas_id: self.ioas,
            user_va: mapping.ptr.as_ptr() as u64,
            length: len as u64,
            iova: 0x0100_0000,
            ..Default::default()
        };
        ioctl_mut(self.iommu.as_raw_fd(), IOMMU_IOAS_MAP, &mut map)?;
        eprintln!("mapped DMA IOVA {:#x} flags {:#x}", map.iova, map.flags);
        Ok(Dma {
            mapping,
            iova: map.iova,
            direction,
        })
    }
    fn alloc_dma_constrained(
        &mut self,
        size: usize,
        constraints: DmaConstraints,
        direction: DmaDirection,
        coherent: bool,
    ) -> Result<Dma> {
        if constraints.alignment == 0 || !constraints.alignment.is_power_of_two() {
            return Err(Error::Invalid);
        }
        let mapped_len = size.next_multiple_of(4096);
        let last = 0x0100_0000_u64
            .checked_add(mapped_len as u64 - 1)
            .ok_or(Error::Limit)?;
        if constraints.max_segments == 0
            || constraints.max_segment_size < size
            || !0x0100_0000_usize.is_multiple_of(constraints.alignment)
            || last > constraints.max_device_address
        {
            return Err(Error::Limit);
        }
        self.alloc_dma(size, constraints.alignment, direction, coherent)
    }
    fn dma_read(&mut self, d: &Dma, r: Range<usize>, out: &mut [u8]) -> Result<()> {
        if matches!(d.direction, DmaDirection::ToDevice) {
            return Err(Error::Invalid);
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                d.mapping.ptr.as_ptr().add(r.start),
                out.as_mut_ptr(),
                out.len(),
            )
        };
        Ok(())
    }
    fn dma_write(&mut self, d: &Dma, r: Range<usize>, bytes: &[u8]) -> Result<()> {
        if matches!(d.direction, DmaDirection::FromDevice) {
            return Err(Error::Invalid);
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                d.mapping.ptr.as_ptr().add(r.start),
                bytes.len(),
            )
        };
        Ok(())
    }
    fn sync_for_cpu(&mut self, _: &Dma, _: Range<usize>) -> Result<()> {
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn sync_for_device(&mut self, _: &Dma, _: Range<usize>) -> Result<()> {
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn open_interrupt(&mut self, vector: u32) -> Result<()> {
        if vector != 0 {
            return Err(Error::Invalid);
        }
        let fd = unsafe { eventfd(0, 0) };
        if fd < 0 {
            return Err(Error::DeviceFault);
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let mut set = IrqSet {
            argsz: std::mem::size_of::<IrqSet>() as u32,
            flags: VFIO_IRQ_SET_DATA_EVENTFD | VFIO_IRQ_SET_ACTION_TRIGGER,
            index: 0,
            start: 0,
            count: 1,
            eventfd: fd,
        };
        ioctl_mut(self.device_fd(), VFIO_DEVICE_SET_IRQS, &mut set)?;
        self.irq = Some(file);
        Ok(())
    }
    fn wait_interrupt(&mut self, _: &(), _: u64) -> Result<Option<IrqEvent>> {
        let mut bytes = [0; 8];
        self.irq
            .as_mut()
            .ok_or(Error::Invalid)?
            .read_exact(&mut bytes)
            .map_err(|_| Error::DeviceFault)?;
        let mut unmask = IrqHeader {
            argsz: std::mem::size_of::<IrqHeader>() as u32,
            flags: VFIO_IRQ_SET_DATA_NONE | VFIO_IRQ_SET_ACTION_UNMASK,
            index: 0,
            start: 0,
            count: 1,
        };
        ioctl_mut(self.device_fd(), VFIO_DEVICE_SET_IRQS, &mut unmask)?;
        Ok(Some(IrqEvent {
            vector: 0,
            count: u64::from_ne_bytes(bytes),
            at_ns: 0,
        }))
    }
    fn reset(&mut self) -> Result<u64> {
        // QEMU edu's VFIO cdev does not advertise a function-reset method on
        // every kernel; generation revocation remains mandatory either way.
        let _ = unsafe { ioctl(self.device_fd(), VFIO_DEVICE_RESET) };
        self.generation += 1;
        Ok(self.generation)
    }
    fn release_region(&mut self, _: Mapping) {}
    fn release_dma(&mut self, d: Dma) {
        let mut u = IoasUnmap {
            size: std::mem::size_of::<IoasUnmap>() as u32,
            ioas_id: self.ioas,
            iova: d.iova,
            length: d.mapping.len as u64,
        };
        let _ = ioctl_mut(self.iommu.as_raw_fd(), IOMMU_IOAS_UNMAP, &mut u);
    }
    fn release_interrupt(&mut self, _: ()) {
        self.irq.take();
    }
}

fn run_physical_probe(path: &str) {
    let backend = LinuxVfio::open(path).expect("initialize VFIO/iommufd backend");
    let device = Device::from_backend(backend);
    let mut dma = device
        .alloc_streaming::<drv_hardware::Bidirectional>(4096, 4096)
        .expect("map private DMA arena");
    let marker = b"drv physical VFIO probe";
    dma.write(0, marker).expect("write private DMA arena");
    dma.sync_for_device(0, marker.len())
        .expect("publish private DMA arena");
    drop(dma);
    drop(device);
    println!("safe VFIO/iommufd DMA map/unmap probe passed without device MMIO");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args
        .next()
        .expect("usage: vfio_edu [--probe] /dev/vfio/devices/vfioN");
    if first == "--probe" {
        let path = args
            .next()
            .or_else(|| std::env::var("DRV_VFIO_DEVICE").ok())
            .expect("--probe requires a VFIO cdev path or DRV_VFIO_DEVICE");
        run_physical_probe(&path);
        return;
    }

    let backend = LinuxVfio::open(&first).expect("initialize VFIO/iommufd backend");
    let device = Device::from_backend(backend);
    let payload = run_edu_sequence(&device).expect("safe edu sequence");
    eprintln!("DMA payload {payload:?}");
    println!("safe VFIO edu MMIO/DMA/IRQ/reset sequence passed");
}
