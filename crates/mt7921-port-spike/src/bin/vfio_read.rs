//! Strictly read-only no-plastic MT7921 VFIO inventory.
#![cfg(target_os = "linux")]

use mt7921_port_spike::{ReadOnlyStatus, ReadRegister};
use std::{
    env,
    fs::{File, OpenOptions},
    os::fd::{AsRawFd, RawFd},
    ptr::NonNull,
};

const VFIO_TYPE: u64 = b';' as u64;
const VFIO_BASE: u64 = 100;
const VFIO_DEVICE_GET_REGION_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 8);
const VFIO_DEVICE_BIND_IOMMUFD: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 18);
const VFIO_DEVICE_ATTACH_IOMMUFD_PT: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 19);
const IOMMU_DESTROY: u64 = (VFIO_TYPE << 8) | 0x80;
const IOMMU_IOAS_ALLOC: u64 = (VFIO_TYPE << 8) | 0x81;
const PROT_READ: i32 = 1;
const MAP_SHARED: i32 = 1;
const BAR0_REGION: u32 = 0;
const PAGE: usize = 4096;

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
struct Destroy {
    size: u32,
    id: u32,
}

unsafe extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
}

fn main() {
    if let Err(message) = run() {
        eprintln!("mt7921-vfio-read: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let bdf = env::var("DRV_PCI_BDF").map_err(|_| "DRV_PCI_BDF is required")?;
    let vfio = env::var("DRV_VFIO_DEVICE").map_err(|_| "DRV_VFIO_DEVICE is required")?;
    verify_pci_identity(&bdf)?;

    let device = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&vfio)
        .map_err(|error| format!("open {vfio}: {error}"))?;
    let iommu = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/iommu")
        .map_err(|error| format!("open /dev/iommu: {error}"))?;
    let mut bind = Bind {
        argsz: size::<Bind>(),
        iommufd: iommu.as_raw_fd(),
        ..Default::default()
    };
    ioctl_mut(
        device.as_raw_fd(),
        VFIO_DEVICE_BIND_IOMMUFD,
        &mut bind,
        "bind iommufd",
    )?;
    let mut alloc = IoasAlloc {
        size: size::<IoasAlloc>(),
        ..Default::default()
    };
    ioctl_mut(
        iommu.as_raw_fd(),
        IOMMU_IOAS_ALLOC,
        &mut alloc,
        "allocate IOAS",
    )?;
    let ioas = Ioas {
        fd: &iommu,
        id: alloc.out_ioas_id,
    };
    let mut attach = Attach {
        argsz: size::<Attach>(),
        pt_id: ioas.id,
        ..Default::default()
    };
    ioctl_mut(
        device.as_raw_fd(),
        VFIO_DEVICE_ATTACH_IOMMUFD_PT,
        &mut attach,
        "attach IOAS",
    )?;

    let mut info = RegionInfo {
        argsz: size::<RegionInfo>(),
        index: BAR0_REGION,
        ..Default::default()
    };
    ioctl_mut(
        device.as_raw_fd(),
        VFIO_DEVICE_GET_REGION_INFO,
        &mut info,
        "query BAR 0",
    )?;

    let wfdma = ReadPage::map(&device, &info, 0xd4000)?;
    let conn = ReadPage::map(&device, &info, 0xe0000)?;
    let read = |register: ReadRegister| -> Result<u32, String> {
        let page = match register.bar_offset() / PAGE {
            0xd4 => &wfdma,
            0xe0 => &conn,
            _ => return Err("register escaped immutable page allowlist".into()),
        };
        page.read(register.bar_offset())
    };
    let mcu = read(ReadRegister::McuCommand)?;
    let interrupt = read(ReadRegister::HostInterruptStatus)?;
    let wfdma_config = read(ReadRegister::WfdmaGlobalConfig)?;
    let low_power = read(ReadRegister::ConnOnLowPowerControl)?;
    let conn_misc = read(ReadRegister::ConnOnMisc)?;
    let status = ReadOnlyStatus::decode(conn_misc, low_power, wfdma_config);
    println!(
        "{{\"pci_bdf\":\"{bdf}\",\"vendor_device\":\"14c3:7961\",\"subsystem\":\"1a3b:4680\",\"registers\":{{\"{}\":\"{mcu:#010x}\",\"{}\":\"{interrupt:#010x}\",\"{}\":\"{wfdma_config:#010x}\",\"{}\":\"{low_power:#010x}\",\"{}\":\"{conn_misc:#010x}\"}},\"status\":{{\"firmware_powered\":{},\"firmware_n9_ready\":{},\"firmware_owns_device\":{},\"tx_dma_enabled\":{},\"tx_dma_busy\":{},\"rx_dma_enabled\":{},\"rx_dma_busy\":{}}}}}",
        ReadRegister::McuCommand.name(),
        ReadRegister::HostInterruptStatus.name(),
        ReadRegister::WfdmaGlobalConfig.name(),
        ReadRegister::ConnOnLowPowerControl.name(),
        ReadRegister::ConnOnMisc.name(),
        status.firmware_powered,
        status.firmware_n9_ready,
        status.firmware_owns_device,
        status.tx_dma_enabled,
        status.tx_dma_busy,
        status.rx_dma_enabled,
        status.rx_dma_busy,
    );
    drop(wfdma);
    drop(conn);
    drop(device);
    drop(ioas);
    Ok(())
}

fn verify_pci_identity(bdf: &str) -> Result<(), String> {
    let root = format!("/sys/bus/pci/devices/{bdf}");
    let fields = [
        ("vendor", "0x14c3"),
        ("device", "0x7961"),
        ("subsystem_vendor", "0x1a3b"),
        ("subsystem_device", "0x4680"),
    ];
    for (name, expected) in fields {
        let value = std::fs::read_to_string(format!("{root}/{name}"))
            .map_err(|error| format!("read PCI {name}: {error}"))?;
        if value.trim() != expected {
            return Err(format!(
                "PCI {name} is {}, expected {expected}",
                value.trim()
            ));
        }
    }
    Ok(())
}

struct Ioas<'a> {
    fd: &'a File,
    id: u32,
}
impl Drop for Ioas<'_> {
    fn drop(&mut self) {
        let mut destroy = Destroy {
            size: size::<Destroy>(),
            id: self.id,
        };
        let _ = ioctl_mut(
            self.fd.as_raw_fd(),
            IOMMU_DESTROY,
            &mut destroy,
            "destroy IOAS",
        );
    }
}

struct ReadPage {
    ptr: NonNull<u8>,
    bar_page: usize,
}
impl ReadPage {
    fn map(device: &File, region: &RegionInfo, bar_page: usize) -> Result<Self, String> {
        if bar_page % PAGE != 0 || bar_page + PAGE > region.size as usize {
            return Err("allowlisted BAR page is outside BAR 0".into());
        }
        let ptr = NonNull::new(unsafe {
            mmap(
                std::ptr::null_mut(),
                PAGE,
                PROT_READ,
                MAP_SHARED,
                device.as_raw_fd(),
                (region.offset + bar_page as u64) as i64,
            )
        })
        .filter(|pointer| pointer.as_ptr() as isize != -1)
        .ok_or_else(|| {
            format!(
                "map BAR page {bar_page:#x}: {}",
                std::io::Error::last_os_error()
            )
        })?;
        Ok(Self { ptr, bar_page })
    }
    fn read(&self, offset: usize) -> Result<u32, String> {
        let within = offset
            .checked_sub(self.bar_page)
            .ok_or("register below mapped page")?;
        if within % 4 != 0 || within + 4 > PAGE {
            return Err("register outside mapped page".into());
        }
        Ok(unsafe { std::ptr::read_volatile(self.ptr.as_ptr().add(within).cast::<u32>()) })
    }
}
impl Drop for ReadPage {
    fn drop(&mut self) {
        unsafe { munmap(self.ptr.as_ptr(), PAGE) };
    }
}

fn size<T>() -> u32 {
    std::mem::size_of::<T>() as u32
}

fn ioctl_mut<T>(fd: RawFd, request: u64, value: &mut T, operation: &str) -> Result<(), String> {
    if unsafe { ioctl(fd, request, value) } < 0 {
        Err(format!("{operation}: {}", std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}
