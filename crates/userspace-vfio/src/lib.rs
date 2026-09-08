//! Linux VFIO cdev/iommufd resource ownership for userspace drivers.
//!
//! This crate contains no device register or firmware policy. Mappings expose
//! only bounds-checked volatile and byte operations; raw pointers never escape.

use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    ptr::NonNull,
    sync::Arc,
};

const VFIO_TYPE: u64 = b';' as u64;
const VFIO_BASE: u64 = 100;
const VFIO_DEVICE_GET_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 7);
const VFIO_DEVICE_GET_REGION_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 8);
const VFIO_DEVICE_GET_IRQ_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 9);
const VFIO_DEVICE_SET_IRQS: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 10);
const VFIO_DEVICE_RESET: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 11);
const VFIO_DEVICE_FEATURE: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 17);
const VFIO_DEVICE_BIND_IOMMUFD: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 18);
const VFIO_DEVICE_ATTACH_IOMMUFD_PT: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 19);
const IOMMU_DESTROY: u64 = (VFIO_TYPE << 8) | 0x80;
const IOMMU_IOAS_ALLOC: u64 = (VFIO_TYPE << 8) | 0x81;
const IOMMU_IOAS_MAP: u64 = (VFIO_TYPE << 8) | 0x85;
const IOMMU_IOAS_UNMAP: u64 = (VFIO_TYPE << 8) | 0x86;
const IOMMU_MAP_FIXED: u32 = 1;
const IOMMU_MAP_WRITEABLE: u32 = 2;
const IOMMU_MAP_READABLE: u32 = 4;
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_SHARED: i32 = 1;
const MAP_PRIVATE: i32 = 2;
const MAP_ANONYMOUS: i32 = 0x20;
const VFIO_REGION_INFO_FLAG_READ: u32 = 1;
const VFIO_REGION_INFO_FLAG_WRITE: u32 = 2;
const VFIO_REGION_INFO_FLAG_MMAP: u32 = 4;
const VFIO_DEVICE_FLAGS_RESET: u32 = 1;
const VFIO_IRQ_SET_DATA_NONE: u32 = 1;
const VFIO_IRQ_SET_DATA_EVENTFD: u32 = 1 << 2;
const VFIO_IRQ_SET_ACTION_TRIGGER: u32 = 1 << 5;
const EFD_CLOEXEC: i32 = 0x80000;
const EFD_NONBLOCK: i32 = 0x800;

/// Frozen out-of-tree VFIO platform DMA broker ABI constants.
pub mod dma_broker_uapi {
    pub const GET: u32 = 1 << 16;
    pub const SET: u32 = 1 << 17;
    pub const PROBE: u32 = 1 << 18;
    pub const FEATURE: u32 = 0xff00;
    pub const ALLOC_COHERENT: u32 = 1;
    pub const MAP_STREAMING: u32 = 2;
    pub const SYNC_CPU: u32 = 3;
    pub const SYNC_DEVICE: u32 = 4;
    pub const FREE: u32 = 5;
    pub const UNMAP: u32 = 6;
    pub const TO_DEVICE: u32 = 1;
    pub const FROM_DEVICE: u32 = 2;
    pub const BIDIRECTIONAL: u32 = 3;
}

unsafe extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn eventfd(initval: u32, flags: i32) -> i32;
    fn read(fd: i32, buffer: *mut u8, count: usize) -> isize;
    fn poll(fds: *mut PollFd, count: usize, timeout: i32) -> i32;
    fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}
#[repr(C)]
struct Timespec {
    seconds: i64,
    nanoseconds: i64,
}
const POLLIN: i16 = 1;
const CLOCK_MONOTONIC: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RegionInfo {
    pub argsz: u32,
    pub flags: u32,
    pub index: u32,
    pub cap_offset: u32,
    pub size: u64,
    pub offset: u64,
}

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
struct BindIommufd {
    argsz: u32,
    flags: u32,
    iommufd: i32,
    out_devid: u32,
}
#[repr(C)]
#[derive(Default)]
struct AttachIommufdPt {
    argsz: u32,
    flags: u32,
    pt_id: u32,
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
struct IrqInfo {
    argsz: u32,
    flags: u32,
    index: u32,
    count: u32,
}
#[repr(C)]
#[derive(Default)]
struct IrqSetHeader {
    argsz: u32,
    flags: u32,
    index: u32,
    start: u32,
    count: u32,
}
#[repr(C)]
#[derive(Default)]
struct IrqSetEventfd {
    header: IrqSetHeader,
    eventfd: i32,
}
#[repr(C)]
#[derive(Default)]
struct Destroy {
    size: u32,
    id: u32,
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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DmaBrokerCommand {
    pub argsz: u32,
    pub operation: u32,
    pub flags: u32,
    pub handle: u32,
    pub size: u64,
    pub alignment: u64,
    pub max_device_address: u64,
    pub user_address: u64,
    pub offset: u64,
    pub length: u64,
    pub mmap_offset: u64,
    pub iova: u64,
    pub direction: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Default)]
struct DmaBrokerFeature {
    argsz: u32,
    flags: u32,
    command: DmaBrokerCommand,
}

fn size<T>() -> u32 {
    std::mem::size_of::<T>() as u32
}
fn ioctl_mut<T>(fd: RawFd, request: u64, value: &mut T, operation: &str) -> Result<(), String> {
    #[cfg(feature = "test-support")]
    if let Some(result) = test_support::dispatch(fd, request, value as *mut T as *mut ()) {
        return result.map_err(|error| format!("{operation}: fake errno {error}"));
    }
    if unsafe { ioctl(fd, request, value) } < 0 {
        Err(format!("{operation}: {}", std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(feature = "test-support")]
pub mod test_support {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum Record {
        Bind,
        AllocateIoas,
        AttachIoas(u32),
        Map {
            iova: u64,
            length: u64,
        },
        Unmap {
            iova: u64,
            length: u64,
        },
        DestroyIoas(u32),
        ProbeBroker,
        Broker {
            operation: u32,
            handle: u32,
            offset: u64,
            length: u64,
        },
    }

    struct Fake {
        broker_supported: bool,
        records: Rc<RefCell<Vec<Record>>>,
    }
    thread_local! {
        static FAKE: RefCell<Option<Fake>> = const { RefCell::new(None) };
    }

    /// Runs one host-side VFIO test with all ioctls intercepted. Anonymous and
    /// shared mmap still use the supplied real file descriptors.
    pub fn with_fake_io<T>(broker_supported: bool, run: impl FnOnce() -> T) -> (T, Vec<Record>) {
        let records = Rc::new(RefCell::new(Vec::new()));
        FAKE.with(|fake| {
            assert!(fake.borrow().is_none(), "nested fake VFIO transport");
            *fake.borrow_mut() = Some(Fake {
                broker_supported,
                records: Rc::clone(&records),
            });
        });
        let result = run();
        FAKE.with(|fake| *fake.borrow_mut() = None);
        let recorded = records.borrow().clone();
        (result, recorded)
    }

    pub(super) fn dispatch(
        _fd: RawFd,
        request: u64,
        value: *mut (),
    ) -> Option<std::result::Result<(), i32>> {
        FAKE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let fake = slot.as_mut()?;
            let record = match request {
                VFIO_DEVICE_BIND_IOMMUFD => Record::Bind,
                IOMMU_IOAS_ALLOC => {
                    // SAFETY: ioctl_mut supplies IoasAlloc for this request.
                    unsafe { value.cast::<IoasAlloc>().as_mut().unwrap().out_ioas_id = 7 };
                    Record::AllocateIoas
                }
                VFIO_DEVICE_ATTACH_IOMMUFD_PT => {
                    // SAFETY: ioctl_mut supplies AttachIommufdPt for this request.
                    let attach = unsafe { value.cast::<AttachIommufdPt>().as_ref().unwrap() };
                    Record::AttachIoas(attach.pt_id)
                }
                IOMMU_IOAS_MAP => {
                    // SAFETY: ioctl_mut supplies IoasMap for this request.
                    let map = unsafe { value.cast::<IoasMap>().as_ref().unwrap() };
                    Record::Map {
                        iova: map.iova,
                        length: map.length,
                    }
                }
                IOMMU_IOAS_UNMAP => {
                    // SAFETY: ioctl_mut supplies IoasUnmap for this request.
                    let unmap = unsafe { value.cast::<IoasUnmap>().as_ref().unwrap() };
                    Record::Unmap {
                        iova: unmap.iova,
                        length: unmap.length,
                    }
                }
                IOMMU_DESTROY => {
                    // SAFETY: ioctl_mut supplies Destroy for this request.
                    let destroy = unsafe { value.cast::<Destroy>().as_ref().unwrap() };
                    Record::DestroyIoas(destroy.id)
                }
                VFIO_DEVICE_FEATURE => {
                    // SAFETY: both feature calls supply DmaBrokerFeature.
                    let feature = unsafe { value.cast::<DmaBrokerFeature>().as_mut().unwrap() };
                    if feature.flags & dma_broker_uapi::PROBE != 0 {
                        if !fake.broker_supported {
                            return Some(Err(25));
                        }
                        Record::ProbeBroker
                    } else {
                        let operation = feature.command.operation;
                        if operation == dma_broker_uapi::ALLOC_COHERENT {
                            feature.command.handle = 11;
                            feature.command.iova = 0x0200_0000;
                            feature.command.mmap_offset = 4096;
                        } else if operation == dma_broker_uapi::MAP_STREAMING {
                            feature.command.handle = 12;
                            feature.command.iova = 0x0300_0000;
                        }
                        Record::Broker {
                            operation,
                            handle: feature.command.handle,
                            offset: feature.command.offset,
                            length: feature.command.length,
                        }
                    }
                }
                _ => return Some(Err(25)),
            };
            fake.records.borrow_mut().push(record);
            Some(Ok(()))
        })
    }
}

pub fn bind_iommufd(device: &File, iommu: &File) -> Result<u32, String> {
    let mut bind = BindIommufd {
        argsz: size::<BindIommufd>(),
        iommufd: iommu.as_raw_fd(),
        ..Default::default()
    };
    ioctl_mut(
        device.as_raw_fd(),
        VFIO_DEVICE_BIND_IOMMUFD,
        &mut bind,
        "bind VFIO device to iommufd",
    )?;
    Ok(bind.out_devid)
}

pub fn allocate_ioas(iommu: &Arc<File>) -> Result<Ioas, String> {
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
    Ok(Ioas::from_allocated(iommu, alloc.out_ioas_id))
}

pub fn attach_ioas(device: &File, ioas: u32) -> Result<(), String> {
    let mut attach = AttachIommufdPt {
        argsz: size::<AttachIommufdPt>(),
        pt_id: ioas,
        ..Default::default()
    };
    ioctl_mut(
        device.as_raw_fd(),
        VFIO_DEVICE_ATTACH_IOMMUFD_PT,
        &mut attach,
        "attach VFIO device to IOAS",
    )
}

pub fn region_info(device: &File, index: u32) -> Result<RegionInfo, String> {
    let mut info = RegionInfo {
        argsz: size::<RegionInfo>(),
        index,
        ..Default::default()
    };
    ioctl_mut(
        device.as_raw_fd(),
        VFIO_DEVICE_GET_REGION_INFO,
        &mut info,
        "query VFIO region",
    )?;
    Ok(info)
}

pub fn probe_dma_broker(device: &File) -> Result<(), String> {
    let mut feature = DmaBrokerFeature {
        argsz: size::<DmaBrokerFeature>(),
        flags: dma_broker_uapi::FEATURE | dma_broker_uapi::PROBE,
        ..Default::default()
    };
    ioctl_mut(
        device.as_raw_fd(),
        VFIO_DEVICE_FEATURE,
        &mut feature,
        "probe VFIO DMA broker feature",
    )
}

pub fn dma_broker_command(
    device: &File,
    mut command: DmaBrokerCommand,
) -> Result<DmaBrokerCommand, String> {
    command.argsz = size::<DmaBrokerCommand>();
    let mut feature = DmaBrokerFeature {
        argsz: size::<DmaBrokerFeature>(),
        flags: dma_broker_uapi::FEATURE | dma_broker_uapi::SET,
        command,
    };
    ioctl_mut(
        device.as_raw_fd(),
        VFIO_DEVICE_FEATURE,
        &mut feature,
        "execute VFIO DMA broker operation",
    )?;
    Ok(feature.command)
}

/// An IOAS id whose destruction is owned and retried explicitly before Drop.
pub struct Ioas {
    fd: Arc<File>,
    id: u32,
    destroyed: bool,
}
impl Ioas {
    pub fn from_allocated(fd: &Arc<File>, id: u32) -> Self {
        Self {
            fd: Arc::clone(fd),
            id,
            destroyed: false,
        }
    }
    pub fn id(&self) -> u32 {
        self.id
    }
    pub fn teardown(&mut self) -> Result<(), String> {
        if self.destroyed {
            return Ok(());
        }
        let mut destroy = Destroy {
            size: size::<Destroy>(),
            id: self.id,
        };
        ioctl_mut(
            self.fd.as_raw_fd(),
            IOMMU_DESTROY,
            &mut destroy,
            "destroy IOAS",
        )?;
        self.destroyed = true;
        Ok(())
    }
}
impl Drop for Ioas {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

/// Page-aligned anonymous memory pinned into one IOAS at an exact low IOVA.
pub struct DmaMapping {
    iommu: Arc<File>,
    ioas: u32,
    ptr: NonNull<u8>,
    len: usize,
    iova: u64,
    mapped: bool,
}
impl DmaMapping {
    pub fn map(
        iommu: &Arc<File>,
        ioas: u32,
        iova: u64,
        len: usize,
        page: usize,
    ) -> Result<Self, String> {
        Self::map_with_flags(iommu, ioas, iova, len, page, true, true)
    }

    pub fn map_with_flags(
        iommu: &Arc<File>,
        ioas: u32,
        iova: u64,
        len: usize,
        page: usize,
        device_reads: bool,
        device_writes: bool,
    ) -> Result<Self, String> {
        if len == 0 || !len.is_multiple_of(page) || !iova.is_multiple_of(page as u64) {
            return Err("DMA mapping length and IOVA must be page aligned".into());
        }
        let ptr = NonNull::new(unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        })
        .filter(|p| p.as_ptr() as isize != -1)
        .ok_or_else(|| format!("allocate DMA mapping: {}", std::io::Error::last_os_error()))?;
        let mut map = IoasMap {
            size: size::<IoasMap>(),
            flags: IOMMU_MAP_FIXED
                | if device_reads { IOMMU_MAP_READABLE } else { 0 }
                | if device_writes {
                    IOMMU_MAP_WRITEABLE
                } else {
                    0
                },
            ioas_id: ioas,
            user_va: ptr.as_ptr() as u64,
            length: len as u64,
            iova,
            ..Default::default()
        };
        if let Err(error) = ioctl_mut(iommu.as_raw_fd(), IOMMU_IOAS_MAP, &mut map, "map DMA arena")
        {
            unsafe {
                munmap(ptr.as_ptr(), len);
            }
            return Err(error);
        }
        if map.iova != iova {
            let mut unmap = IoasUnmap {
                size: size::<IoasUnmap>(),
                ioas_id: ioas,
                iova: map.iova,
                length: len as u64,
            };
            let _ = ioctl_mut(
                iommu.as_raw_fd(),
                IOMMU_IOAS_UNMAP,
                &mut unmap,
                "unmap invalid arena",
            );
            unsafe {
                munmap(ptr.as_ptr(), len);
            }
            return Err("iommufd did not honor fixed IOVA".into());
        }
        Ok(Self {
            iommu: Arc::clone(iommu),
            ioas,
            ptr,
            len,
            iova,
            mapped: true,
        })
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn iova(&self) -> u64 {
        self.iova
    }
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        self.range(offset, bytes.len())?;
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.ptr.as_ptr().add(offset),
                bytes.len(),
            );
        }
        Ok(())
    }
    pub fn read(&self, offset: usize, len: usize) -> Result<Vec<u8>, String> {
        self.range(offset, len)?;
        let mut out = vec![0; len];
        unsafe {
            std::ptr::copy_nonoverlapping(self.ptr.as_ptr().add(offset), out.as_mut_ptr(), len);
        }
        Ok(out)
    }
    pub fn zero(&mut self, len: usize) -> Result<(), String> {
        self.range(0, len)?;
        unsafe {
            std::ptr::write_bytes(self.ptr.as_ptr(), 0, len);
        }
        Ok(())
    }
    pub fn secure_zero(&mut self, len: usize) -> Result<(), String> {
        self.range(0, len)?;
        for i in 0..len {
            unsafe {
                std::ptr::write_volatile(self.ptr.as_ptr().add(i), 0);
            }
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    pub fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), String> {
        self.range(offset, 4)?;
        if !offset.is_multiple_of(4) {
            return Err("unaligned DMA word".into());
        }
        unsafe {
            std::ptr::write_volatile(self.ptr.as_ptr().add(offset).cast::<u32>(), value);
        }
        Ok(())
    }
    pub fn read_u32(&self, offset: usize) -> Result<u32, String> {
        self.range(offset, 4)?;
        if !offset.is_multiple_of(4) {
            return Err("unaligned DMA word".into());
        }
        Ok(unsafe { std::ptr::read_volatile(self.ptr.as_ptr().add(offset).cast::<u32>()) })
    }
    fn range(&self, offset: usize, len: usize) -> Result<(), String> {
        offset
            .checked_add(len)
            .filter(|end| *end <= self.len)
            .map(|_| ())
            .ok_or_else(|| "DMA access escaped mapping".into())
    }
    pub fn teardown(&mut self) -> Result<(), String> {
        if !self.mapped {
            return Ok(());
        }
        let mut unmap = IoasUnmap {
            size: size::<IoasUnmap>(),
            ioas_id: self.ioas,
            iova: self.iova,
            length: self.len as u64,
        };
        ioctl_mut(
            self.iommu.as_raw_fd(),
            IOMMU_IOAS_UNMAP,
            &mut unmap,
            "unmap DMA arena",
        )?;
        if unmap.length != self.len as u64 {
            return Err(format!(
                "iommufd unmapped {} of {} bytes",
                unmap.length, self.len
            ));
        }
        self.mapped = false;
        if unsafe { munmap(self.ptr.as_ptr(), self.len) } != 0 {
            return Err(format!(
                "unmap DMA memory: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

/// Page-backed anonymous memory used only as input to a device-bound DMA broker.
pub struct AnonymousMapping {
    ptr: NonNull<u8>,
    len: usize,
    mapped: bool,
}
impl AnonymousMapping {
    pub fn new(len: usize) -> Result<Self, String> {
        if len == 0 || !len.is_multiple_of(4096) {
            return Err("anonymous mapping length must be page aligned".into());
        }
        let ptr = NonNull::new(unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        })
        .filter(|p| p.as_ptr() as isize != -1)
        .ok_or_else(|| {
            format!(
                "allocate anonymous mapping: {}",
                std::io::Error::last_os_error()
            )
        })?;
        Ok(Self {
            ptr,
            len,
            mapped: true,
        })
    }
    pub fn user_address(&self) -> u64 {
        self.ptr.as_ptr() as u64
    }
    pub fn read(&self, offset: usize, len: usize) -> Result<Vec<u8>, String> {
        checked_memory_range(offset, len, self.len)?;
        let mut bytes = vec![0; len];
        unsafe {
            std::ptr::copy_nonoverlapping(self.ptr.as_ptr().add(offset), bytes.as_mut_ptr(), len)
        };
        Ok(bytes)
    }
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        checked_memory_range(offset, bytes.len(), self.len)?;
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.ptr.as_ptr().add(offset),
                bytes.len(),
            )
        };
        Ok(())
    }
    pub fn teardown(&mut self) -> Result<(), String> {
        if self.mapped && unsafe { munmap(self.ptr.as_ptr(), self.len) } != 0 {
            return Err(format!(
                "unmap anonymous memory: {}",
                std::io::Error::last_os_error()
            ));
        }
        self.mapped = false;
        Ok(())
    }
}
impl Drop for AnonymousMapping {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

/// One shared mapping returned by a device-bound coherent DMA allocation.
pub struct DeviceMapping {
    ptr: NonNull<u8>,
    len: usize,
    mapped: bool,
}
impl DeviceMapping {
    pub fn map(device: &File, offset: u64, len: usize) -> Result<Self, String> {
        if len == 0 || !offset.is_multiple_of(4096) {
            return Err("device mapping offset must be page aligned and length nonzero".into());
        }
        let offset = i64::try_from(offset).map_err(|_| "device mapping offset is too large")?;
        let ptr = NonNull::new(unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                device.as_raw_fd(),
                offset,
            )
        })
        .filter(|p| p.as_ptr() as isize != -1)
        .ok_or_else(|| {
            format!(
                "map coherent DMA memory: {}",
                std::io::Error::last_os_error()
            )
        })?;
        Ok(Self {
            ptr,
            len,
            mapped: true,
        })
    }
    pub fn read(&self, offset: usize, len: usize) -> Result<Vec<u8>, String> {
        checked_memory_range(offset, len, self.len)?;
        let mut bytes = vec![0; len];
        unsafe {
            std::ptr::copy_nonoverlapping(self.ptr.as_ptr().add(offset), bytes.as_mut_ptr(), len)
        };
        Ok(bytes)
    }
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        checked_memory_range(offset, bytes.len(), self.len)?;
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.ptr.as_ptr().add(offset),
                bytes.len(),
            )
        };
        Ok(())
    }
    pub fn teardown(&mut self) -> Result<(), String> {
        if self.mapped && unsafe { munmap(self.ptr.as_ptr(), self.len) } != 0 {
            return Err(format!(
                "unmap coherent DMA memory: {}",
                std::io::Error::last_os_error()
            ));
        }
        self.mapped = false;
        Ok(())
    }
}
impl Drop for DeviceMapping {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

fn checked_memory_range(offset: usize, len: usize, total: usize) -> Result<(), String> {
    offset
        .checked_add(len)
        .filter(|end| *end <= total)
        .map(|_| ())
        .ok_or_else(|| "memory access escaped mapping".into())
}
impl Drop for DmaMapping {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

/// One bounded VFIO region mapping. Device policy chooses the region/offset.
pub struct RegionMapping {
    ptr: NonNull<u8>,
    len: usize,
    offset: u64,
    mapped: bool,
}
impl RegionMapping {
    pub fn map(
        device: &File,
        region: &RegionInfo,
        offset: usize,
        len: usize,
        writable: bool,
    ) -> Result<Self, String> {
        if !offset.is_multiple_of(4096)
            || offset
                .checked_add(len)
                .is_none_or(|end| end > region.size as usize)
        {
            return Err("region mapping is outside VFIO region".into());
        }
        let required = VFIO_REGION_INFO_FLAG_READ
            | VFIO_REGION_INFO_FLAG_MMAP
            | if writable {
                VFIO_REGION_INFO_FLAG_WRITE
            } else {
                0
            };
        if region.flags & required != required {
            return Err(format!(
                "VFIO region flags {:#x} lack mapping permissions",
                region.flags
            ));
        }
        let device_offset = region
            .offset
            .checked_add(offset as u64)
            .and_then(|offset| i64::try_from(offset).ok())
            .ok_or_else(|| "VFIO region offset is too large".to_string())?;
        let ptr = NonNull::new(unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ | if writable { PROT_WRITE } else { 0 },
                MAP_SHARED,
                device.as_raw_fd(),
                device_offset,
            )
        })
        .filter(|p| p.as_ptr() as isize != -1)
        .ok_or_else(|| format!("map VFIO region: {}", std::io::Error::last_os_error()))?;
        Ok(Self {
            ptr,
            len,
            offset: offset as u64,
            mapped: true,
        })
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn read_u32(&self, offset: usize) -> Result<u32, String> {
        if !offset.is_multiple_of(4) || offset.checked_add(4).is_none_or(|end| end > self.len) {
            return Err("region read escaped mapping".into());
        }
        Ok(unsafe { std::ptr::read_volatile(self.ptr.as_ptr().add(offset).cast::<u32>()) })
    }
    pub fn write_u32(&self, offset: usize, value: u32) -> Result<(), String> {
        if !offset.is_multiple_of(4) || offset.checked_add(4).is_none_or(|end| end > self.len) {
            return Err("region write escaped mapping".into());
        }
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(offset).cast::<u32>(), value) };
        Ok(())
    }
    pub fn teardown(&mut self) -> Result<(), String> {
        if !self.mapped {
            return Ok(());
        }
        if unsafe { munmap(self.ptr.as_ptr(), self.len) } != 0 {
            return Err(format!(
                "unmap VFIO region: {}",
                std::io::Error::last_os_error()
            ));
        }
        self.mapped = false;
        Ok(())
    }
}
impl Drop for RegionMapping {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

pub struct IrqCapability {
    pub index: u32,
    pub count: u32,
    pub eventfd: bool,
}
pub fn irq_capability(device: &File, index: u32) -> Result<IrqCapability, String> {
    let mut info = IrqInfo {
        argsz: size::<IrqInfo>(),
        index,
        ..Default::default()
    };
    ioctl_mut(
        device.as_raw_fd(),
        VFIO_DEVICE_GET_IRQ_INFO,
        &mut info,
        "query VFIO IRQ",
    )?;
    Ok(IrqCapability {
        index,
        count: info.count,
        eventfd: info.flags & 1 != 0,
    })
}
pub struct VfioIrq {
    device: Arc<File>,
    event_fd: OwnedFd,
    index: u32,
    installed: bool,
}
impl VfioIrq {
    pub fn install(device: &Arc<File>, capability: IrqCapability) -> Result<Self, String> {
        if capability.count == 0 || !capability.eventfd {
            return Err("refused non-eventfd VFIO interrupt".into());
        }
        let raw = unsafe { eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK) };
        if raw < 0 {
            return Err(format!(
                "create IRQ eventfd: {}",
                std::io::Error::last_os_error()
            ));
        }
        let event_fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut set = IrqSetEventfd {
            header: IrqSetHeader {
                argsz: size::<IrqSetEventfd>(),
                flags: VFIO_IRQ_SET_DATA_EVENTFD | VFIO_IRQ_SET_ACTION_TRIGGER,
                index: capability.index,
                start: 0,
                count: 1,
            },
            eventfd: event_fd.as_raw_fd(),
        };
        ioctl_mut(
            device.as_raw_fd(),
            VFIO_DEVICE_SET_IRQS,
            &mut set,
            "install VFIO IRQ eventfd",
        )?;
        Ok(Self {
            device: Arc::clone(device),
            event_fd,
            index: capability.index,
            installed: true,
        })
    }
    pub fn try_read(&self) -> Result<Option<u64>, String> {
        let mut count = 0u64;
        let result = unsafe {
            read(
                self.event_fd.as_raw_fd(),
                (&mut count as *mut u64).cast(),
                8,
            )
        };
        if result == 8 {
            Ok(Some(count))
        } else if result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(11) {
            Ok(None)
        } else {
            Err(format!(
                "read IRQ eventfd: {}",
                std::io::Error::last_os_error()
            ))
        }
    }
    pub fn wait_until(&self, deadline_ns: u64) -> Result<Option<u64>, String> {
        let now = monotonic_time_ns()?;
        let timeout = if deadline_ns <= now {
            0
        } else {
            let remaining = deadline_ns - now;
            i32::try_from(remaining.div_ceil(1_000_000)).unwrap_or(i32::MAX)
        };
        let mut fd = PollFd {
            fd: self.event_fd.as_raw_fd(),
            events: POLLIN,
            revents: 0,
        };
        let result = unsafe { poll(&mut fd, 1, timeout) };
        if result < 0 {
            Err(format!(
                "wait for IRQ eventfd: {}",
                std::io::Error::last_os_error()
            ))
        } else if result == 0 {
            Ok(None)
        } else {
            self.try_read()
        }
    }
    pub fn disable(&mut self) -> Result<(), String> {
        if !self.installed {
            return Ok(());
        }
        disable_irq(&self.device, self.index)?;
        self.installed = false;
        Ok(())
    }
}

pub fn monotonic_time_ns() -> Result<u64, String> {
    let mut time = Timespec {
        seconds: 0,
        nanoseconds: 0,
    };
    if unsafe { clock_gettime(CLOCK_MONOTONIC, &mut time) } < 0 {
        return Err(format!(
            "read monotonic clock: {}",
            std::io::Error::last_os_error()
        ));
    }
    u64::try_from(time.seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
        .and_then(|nanos| nanos.checked_add(time.nanoseconds as u64))
        .ok_or_else(|| "invalid monotonic clock value".into())
}
impl Drop for VfioIrq {
    fn drop(&mut self) {
        let _ = self.disable();
    }
}
pub fn disable_irq(device: &File, index: u32) -> Result<(), String> {
    let mut set = IrqSetHeader {
        argsz: size::<IrqSetHeader>(),
        flags: VFIO_IRQ_SET_DATA_NONE | VFIO_IRQ_SET_ACTION_TRIGGER,
        index,
        start: 0,
        count: 0,
    };
    ioctl_mut(
        device.as_raw_fd(),
        VFIO_DEVICE_SET_IRQS,
        &mut set,
        "disable VFIO IRQ",
    )
}
pub fn reset_device_supported(device: &File) -> Result<(), String> {
    let mut info = DeviceInfo {
        argsz: size::<DeviceInfo>(),
        ..Default::default()
    };
    ioctl_mut(
        device.as_raw_fd(),
        VFIO_DEVICE_GET_INFO,
        &mut info,
        "query VFIO reset capability",
    )?;
    if info.flags & VFIO_DEVICE_FLAGS_RESET == 0 {
        return Err("VFIO device does not advertise reset support".into());
    }
    Ok(())
}
pub fn reset_device(device: &File) -> Result<(), String> {
    reset_device_supported(device)?;
    if unsafe { ioctl(device.as_raw_fd(), VFIO_DEVICE_RESET) } < 0 {
        return Err(format!(
            "VFIO device reset: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn abi_layouts_are_linux_uapi_exact() {
        assert_eq!(size::<RegionInfo>(), 32);
        assert_eq!(size::<IoasMap>(), 40);
        assert_eq!(size::<IoasUnmap>(), 24);
        assert_eq!(size::<IrqSetEventfd>(), 24);
        assert_eq!(size::<DmaBrokerCommand>(), 88);
        assert_eq!(size::<DmaBrokerFeature>(), 96);
        assert_eq!(dma_broker_uapi::FEATURE, 0xff00);
        assert_eq!(dma_broker_uapi::GET, 1 << 16);
        assert_eq!(dma_broker_uapi::SET, 1 << 17);
        assert_eq!(dma_broker_uapi::PROBE, 1 << 18);
    }
}
