#![cfg(target_os = "linux")]

use amd_hda_spike::{Controller, Error as HdaError, Transport};
use std::{
    env,
    fs::{File, OpenOptions},
    io,
    os::fd::{AsRawFd, FromRawFd, RawFd},
    os::unix::fs::FileExt,
    ptr::NonNull,
    sync::atomic::{Ordering, fence},
};

const VFIO_TYPE: u64 = b';' as u64;
const VFIO_BASE: u64 = 100;
const VFIO_DEVICE_GET_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 7);
const VFIO_DEVICE_GET_REGION_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 8);
const VFIO_DEVICE_GET_IRQ_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 9);
const VFIO_DEVICE_SET_IRQS: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 10);
const VFIO_DEVICE_BIND_IOMMUFD: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 18);
const VFIO_DEVICE_ATTACH_IOMMUFD_PT: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 19);
const IOMMU_IOAS_ALLOC: u64 = (VFIO_TYPE << 8) | 0x81;
const IOMMU_IOAS_MAP: u64 = (VFIO_TYPE << 8) | 0x85;
const IOMMU_IOAS_UNMAP: u64 = (VFIO_TYPE << 8) | 0x86;
const VFIO_REGION_INFO_FLAG_MMAP: u32 = 1 << 2;
const VFIO_IRQ_INFO_EVENTFD: u32 = 1 << 0;
const VFIO_IRQ_SET_DATA_NONE: u32 = 1;
const VFIO_IRQ_SET_DATA_EVENTFD: u32 = 1 << 2;
const VFIO_IRQ_SET_ACTION_TRIGGER: u32 = 1 << 5;
const VFIO_PCI_BAR0_REGION_INDEX: u32 = 0;
const VFIO_PCI_CONFIG_REGION_INDEX: u32 = 7;
const VFIO_PCI_MSI_IRQ_INDEX: u32 = 1;
const DMA_IOVA: u64 = 0x0100_0000;

#[repr(C)]
#[derive(Default)]
struct DeviceInfo {
    argsz: u32,
    flags: u32,
    num_regions: u32,
    num_irqs: u32,
    cap_offset: u32,
    pad: u32,
}
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
struct IrqInfo {
    argsz: u32,
    flags: u32,
    index: u32,
    count: u32,
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
    pad: u32,
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
fn ioctl_mut<T>(fd: RawFd, request: u64, value: &mut T) -> io::Result<()> {
    if unsafe { ioctl(fd, request, value) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn map(len: usize, fd: RawFd, offset: i64, flags: i32) -> io::Result<NonNull<u8>> {
    NonNull::new(unsafe { mmap(std::ptr::null_mut(), len, 3, flags, fd, offset) })
        .filter(|p| p.as_ptr() as isize != -1)
        .ok_or_else(io::Error::last_os_error)
}
struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
}
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            munmap(self.ptr.as_ptr(), self.len);
        }
    }
}

struct VfioHda {
    device: File,
    iommu: File,
    ioas: u32,
    bar: Mapping,
    dma: Mapping,
    irq: File,
    original_command: u16,
    original_pmcsr: Option<(u64, u16)>,
    config_offset: u64,
}
impl VfioHda {
    fn region(device: &File, index: u32) -> io::Result<RegionInfo> {
        let mut r = RegionInfo {
            argsz: size::<RegionInfo>(),
            index,
            ..Default::default()
        };
        ioctl_mut(device.as_raw_fd(), VFIO_DEVICE_GET_REGION_INFO, &mut r)?;
        Ok(r)
    }
    fn open(path: &str) -> io::Result<Self> {
        let device = OpenOptions::new().read(true).write(true).open(path)?;
        let iommu = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/iommu")?;
        let mut info = DeviceInfo {
            argsz: size::<DeviceInfo>(),
            ..Default::default()
        };
        ioctl_mut(device.as_raw_fd(), VFIO_DEVICE_GET_INFO, &mut info)?;
        if info.num_regions <= VFIO_PCI_CONFIG_REGION_INDEX
            || info.num_irqs <= VFIO_PCI_MSI_IRQ_INDEX
        {
            return Err(io::Error::other("incomplete VFIO PCI regions/IRQs"));
        }
        let mut bind = Bind {
            argsz: size::<Bind>(),
            iommufd: iommu.as_raw_fd(),
            ..Default::default()
        };
        ioctl_mut(device.as_raw_fd(), VFIO_DEVICE_BIND_IOMMUFD, &mut bind)?;
        let mut alloc = IoasAlloc {
            size: size::<IoasAlloc>(),
            ..Default::default()
        };
        ioctl_mut(iommu.as_raw_fd(), IOMMU_IOAS_ALLOC, &mut alloc)?;
        let mut attach = Attach {
            argsz: size::<Attach>(),
            pt_id: alloc.out_ioas_id,
            ..Default::default()
        };
        ioctl_mut(
            device.as_raw_fd(),
            VFIO_DEVICE_ATTACH_IOMMUFD_PT,
            &mut attach,
        )?;

        let config = Self::region(&device, VFIO_PCI_CONFIG_REGION_INDEX)?;
        let read_cfg16 = |at: u64| -> io::Result<u16> {
            let mut b = [0; 2];
            device.read_exact_at(&mut b, config.offset + at)?;
            Ok(u16::from_le_bytes(b))
        };
        let vendor = read_cfg16(0)?;
        let device_id = read_cfg16(2)?;
        let subsystem_vendor = read_cfg16(0x2c)?;
        let subsystem = read_cfg16(0x2e)?;
        if (vendor, device_id, subsystem_vendor, subsystem) != (0x1022, 0x15e3, 0x1043, 0x1513) {
            return Err(io::Error::other(format!(
                "identity mismatch {vendor:04x}:{device_id:04x} subsystem {subsystem_vendor:04x}:{subsystem:04x}"
            )));
        }
        let original_command = read_cfg16(4)?;
        let mut original_pmcsr = None;
        if read_cfg16(6)? & 0x10 != 0 {
            let mut pointer = {
                let mut b = [0];
                device.read_exact_at(&mut b, config.offset + 0x34)?;
                b[0] & !3
            };
            for _ in 0..48 {
                if pointer < 0x40 {
                    break;
                }
                let mut header = [0; 2];
                device.read_exact_at(&mut header, config.offset + pointer as u64)?;
                if header[0] == 1 {
                    let at = pointer as u64 + 4;
                    let pmcsr = read_cfg16(at)?;
                    device.write_all_at(&(pmcsr & !3).to_le_bytes(), config.offset + at)?;
                    original_pmcsr = Some((at, pmcsr));
                    break;
                }
                pointer = header[1] & !3;
            }
        }
        device.write_all_at(
            &(original_command | 0x0006).to_le_bytes(),
            config.offset + 4,
        )?;

        let bar_info = Self::region(&device, VFIO_PCI_BAR0_REGION_INDEX)?;
        if bar_info.size != 32 * 1024 || bar_info.flags & VFIO_REGION_INFO_FLAG_MMAP == 0 {
            return Err(io::Error::other(format!(
                "BAR0 is not the expected mmapable 32 KiB region: size={:#x} flags={:#x}",
                bar_info.size, bar_info.flags
            )));
        }
        let bar = Mapping {
            ptr: map(
                bar_info.size as usize,
                device.as_raw_fd(),
                bar_info.offset as i64,
                1,
            )?,
            len: bar_info.size as usize,
        };
        let dma = Mapping {
            ptr: map(4096, -1, 0, 2 | 0x20)?,
            len: 4096,
        };
        unsafe {
            std::ptr::write_bytes(dma.ptr.as_ptr(), 0, dma.len);
        }
        let mut dma_map = IoasMap {
            size: size::<IoasMap>(),
            flags: 1 | 2 | 4,
            ioas_id: alloc.out_ioas_id,
            user_va: dma.ptr.as_ptr() as u64,
            length: dma.len as u64,
            iova: DMA_IOVA,
            ..Default::default()
        };
        ioctl_mut(iommu.as_raw_fd(), IOMMU_IOAS_MAP, &mut dma_map)?;

        let mut irq_info = IrqInfo {
            argsz: size::<IrqInfo>(),
            index: VFIO_PCI_MSI_IRQ_INDEX,
            ..Default::default()
        };
        ioctl_mut(device.as_raw_fd(), VFIO_DEVICE_GET_IRQ_INFO, &mut irq_info)?;
        if irq_info.count != 1 || irq_info.flags & VFIO_IRQ_INFO_EVENTFD == 0 {
            return Err(io::Error::other(
                "expected exactly one eventfd-capable MSI vector",
            ));
        }
        let irq_fd = unsafe { eventfd(0, 0x800 | 0x80000) };
        if irq_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let irq = unsafe { File::from_raw_fd(irq_fd) };
        let mut set = IrqSet {
            argsz: size::<IrqSet>(),
            flags: VFIO_IRQ_SET_DATA_EVENTFD | VFIO_IRQ_SET_ACTION_TRIGGER,
            index: VFIO_PCI_MSI_IRQ_INDEX,
            start: 0,
            count: 1,
            eventfd: irq_fd,
        };
        ioctl_mut(device.as_raw_fd(), VFIO_DEVICE_SET_IRQS, &mut set)?;
        Ok(Self {
            device,
            iommu,
            ioas: alloc.out_ioas_id,
            bar,
            dma,
            irq,
            original_command,
            original_pmcsr,
            config_offset: config.offset,
        })
    }
}
fn size<T>() -> u32 {
    std::mem::size_of::<T>() as u32
}
impl Transport for VfioHda {
    type Error = io::Error;
    fn read8(&self, o: usize) -> io::Result<u8> {
        if o >= self.bar.len {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        Ok(unsafe { std::ptr::read_volatile(self.bar.ptr.as_ptr().add(o)) })
    }
    fn read16(&self, o: usize) -> io::Result<u16> {
        if o + 2 > self.bar.len || !o.is_multiple_of(2) {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        Ok(unsafe { std::ptr::read_volatile(self.bar.ptr.as_ptr().add(o).cast()) })
    }
    fn read32(&self, o: usize) -> io::Result<u32> {
        if o + 4 > self.bar.len || !o.is_multiple_of(4) {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        Ok(unsafe { std::ptr::read_volatile(self.bar.ptr.as_ptr().add(o).cast()) })
    }
    fn write8(&mut self, o: usize, v: u8) -> io::Result<()> {
        if o >= self.bar.len {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        unsafe { std::ptr::write_volatile(self.bar.ptr.as_ptr().add(o), v) };
        Ok(())
    }
    fn write16(&mut self, o: usize, v: u16) -> io::Result<()> {
        if o + 2 > self.bar.len || !o.is_multiple_of(2) {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        unsafe { std::ptr::write_volatile(self.bar.ptr.as_ptr().add(o).cast(), v) };
        Ok(())
    }
    fn write32(&mut self, o: usize, v: u32) -> io::Result<()> {
        if o + 4 > self.bar.len || !o.is_multiple_of(4) {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        unsafe { std::ptr::write_volatile(self.bar.ptr.as_ptr().add(o).cast(), v) };
        Ok(())
    }
    fn dma_iova(&self) -> u64 {
        DMA_IOVA
    }
    fn dma_write32(&mut self, o: usize, v: u32) {
        assert!(o + 4 <= self.dma.len && o.is_multiple_of(4));
        unsafe { std::ptr::write_volatile(self.dma.ptr.as_ptr().add(o).cast(), v) }
    }
    fn dma_read32(&self, o: usize) -> u32 {
        assert!(o + 4 <= self.dma.len && o.is_multiple_of(4));
        unsafe { std::ptr::read_volatile(self.dma.ptr.as_ptr().add(o).cast()) }
    }
    fn fence(&self) {
        fence(Ordering::SeqCst)
    }
}
impl Drop for VfioHda {
    fn drop(&mut self) {
        let mut disable = IrqHeader {
            argsz: size::<IrqHeader>(),
            flags: VFIO_IRQ_SET_DATA_NONE | VFIO_IRQ_SET_ACTION_TRIGGER,
            index: VFIO_PCI_MSI_IRQ_INDEX,
            start: 0,
            count: 0,
        };
        let _ = ioctl_mut(self.device.as_raw_fd(), VFIO_DEVICE_SET_IRQS, &mut disable);
        let _ = self
            .device
            .write_all_at(&self.original_command.to_le_bytes(), self.config_offset + 4);
        if let Some((offset, value)) = self.original_pmcsr {
            let _ = self
                .device
                .write_all_at(&value.to_le_bytes(), self.config_offset + offset);
        }
        let mut unmap = IoasUnmap {
            size: size::<IoasUnmap>(),
            ioas_id: self.ioas,
            iova: DMA_IOVA,
            length: self.dma.len as u64,
        };
        let _ = ioctl_mut(self.iommu.as_raw_fd(), IOMMU_IOAS_UNMAP, &mut unmap);
        let _ = &self.irq;
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args()
        .nth(1)
        .or_else(|| env::var("DRV_VFIO_DEVICE").ok())
        .ok_or("usage: amd-hda-enumerate /dev/vfio/devices/vfioN")?;
    let backend = VfioHda::open(&path)?;
    let mut controller = Controller::new(backend);
    let state = controller.reset().map_err(format_hda)?;
    if state != 1 {
        return Err(
            format!("expected only codec address 0 after reset; STATESTS={state:#x}").into(),
        );
    }
    controller.start_command_rings().map_err(format_hda)?;
    let codec = controller.enumerate_codec(0).map_err(format_hda)?;
    println!(
        "codec address={} vendor_device={:08x} revision={:08x}",
        codec.address, codec.vendor_device, codec.revision
    );
    for widget in codec.widgets {
        println!(
            "widget node={:#04x} capabilities={:#010x}",
            widget.node, widget.capabilities
        );
    }
    controller.shutdown();
    println!("AMD HDA reset/CORB/RIRB/codec enumeration completed");
    Ok(())
}
fn format_hda(error: HdaError<io::Error>) -> io::Error {
    io::Error::other(format!("{error:?}"))
}
