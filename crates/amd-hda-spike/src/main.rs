#![cfg(target_os = "linux")]

use amd_hda_spike::{Controller, Error as HdaError, Transport};
use drv_audio_pipewire_spike::{
    EndpointError, PcmFormat, PlaybackEndpoint, VIRTUAL_SINK_FORMAT, protocol,
};
use drv_fuchsia_audio_processing::apply_gain_s16;
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    os::unix::fs::FileExt,
    os::unix::fs::PermissionsExt,
    os::unix::net::UnixListener,
    path::PathBuf,
    ptr::NonNull,
    sync::atomic::{Ordering, fence},
    time::Instant,
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

#[derive(Default)]
struct AdrPcmPeriod {
    pcm: Vec<u8>,
}
impl PlaybackEndpoint for AdrPcmPeriod {
    fn format(&self) -> PcmFormat {
        VIRTUAL_SINK_FORMAT
    }
    fn write(&mut self, pcm: &[u8]) -> Result<(), EndpointError> {
        if !pcm.len().is_multiple_of(4) {
            return Err(EndpointError::PartialFrame);
        }
        self.pcm.extend_from_slice(pcm);
        Ok(())
    }
    fn frame_position(&self) -> u64 {
        (self.pcm.len() / 4) as u64
    }
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
        let error = io::Error::last_os_error();
        eprintln!("ioctl {request:#x} failed: {error}");
        Err(error)
    } else {
        Ok(())
    }
}
fn map(len: usize, fd: RawFd, offset: i64, flags: i32) -> io::Result<NonNull<u8>> {
    NonNull::new(unsafe { mmap(std::ptr::null_mut(), len, 3, flags, fd, offset) })
        .filter(|p| p.as_ptr() as isize != -1)
        .ok_or_else(|| {
            let error = io::Error::last_os_error();
            eprintln!(
                "mmap len={len:#x} fd={fd} offset={offset:#x} flags={flags:#x} failed: {error}"
            );
            error
        })
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
unsafe impl Send for VfioHda {}

struct PhysicalHdaEndpoint {
    controller: Controller<VfioHda>,
    frames: u64,
    periods: u64,
    ioc_irqs: u64,
    underruns: u64,
    started_at: Option<Instant>,
    last_lpib: (u32, u32),
}
impl PlaybackEndpoint for PhysicalHdaEndpoint {
    fn format(&self) -> PcmFormat {
        VIRTUAL_SINK_FORMAT
    }
    fn write(&mut self, pcm: &[u8]) -> Result<(), EndpointError> {
        if pcm.is_empty() || !pcm.len().is_multiple_of(4) {
            return Err(EndpointError::PartialFrame);
        }
        let mut samples = pcm
            .chunks_exact(2)
            .map(|sample| i16::from_le_bytes(sample.try_into().unwrap()))
            .collect::<Vec<_>>();
        apply_gain_s16(&mut samples, drv_audio_pipewire_spike::VIRTUAL_SINK_GAIN_DB);
        let processed = samples
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();
        let report = match self.controller.play_pcm_period(0, &processed) {
            Ok(report) => report,
            Err(error) => {
                self.underruns += 1;
                eprintln!("physical HDA period failed: {error:?}");
                return Err(EndpointError::PositionOverflow);
            }
        };
        self.started_at.get_or_insert_with(Instant::now);
        self.frames = self
            .frames
            .checked_add((processed.len() / 4) as u64)
            .ok_or(EndpointError::PositionOverflow)?;
        self.periods += 1;
        self.ioc_irqs += report.irq_count;
        self.last_lpib = (report.start_position, report.end_position);
        if self.periods == 1 || self.periods.is_multiple_of(25) {
            println!(
                "physical HDA {:?} stream={} elapsed_ms={} periods={} LPIB={}..{} IOC={} frames={} underruns={}",
                report.route,
                report.stream_index,
                self.started_at.unwrap().elapsed().as_millis(),
                self.periods,
                report.start_position,
                report.end_position,
                self.ioc_irqs,
                self.frames,
                self.underruns
            );
        }
        Ok(())
    }
    fn frame_position(&self) -> u64 {
        self.frames
    }
}
impl Drop for PhysicalHdaEndpoint {
    fn drop(&mut self) {
        println!(
            "physical HDA summary duration_ms={} periods={} frames={} LPIB={}..{} IOC={} underruns={}",
            self.started_at
                .map_or(0, |start| start.elapsed().as_millis()),
            self.periods,
            self.frames,
            self.last_lpib.0,
            self.last_lpib.1,
            self.ioc_irqs,
            self.underruns
        );
    }
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
        // A VFIO cdev is not operational until it is bound and attached to an
        // IOAS; capability queries before attachment fail with EINVAL.
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
            ptr: map(16 * 1024, -1, 0, 2 | 0x20)?,
            len: 16 * 1024,
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
    fn take_irq_count(&mut self) -> io::Result<u64> {
        let mut bytes = [0; 8];
        match self.irq.read_exact(&mut bytes) {
            Ok(()) => Ok(u64::from_ne_bytes(bytes)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(error) => Err(error),
        }
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
    let mut args = env::args().skip(1);
    let first = args.next();
    let playback = first.as_deref() == Some("--play-test-tone");
    let physical_daemon = first.as_deref() == Some("--physical-daemon");
    let path = (if playback || physical_daemon {
        args.next()
    } else {
        first
    })
    .or_else(|| env::var("DRV_VFIO_DEVICE").ok())
    .ok_or(
        "usage: amd-hda-enumerate [--play-test-tone|--physical-daemon] /dev/vfio/devices/vfioN",
    )?;
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
    if physical_daemon {
        let runtime_dir = env::var_os("PIPEWIRE_RUNTIME_DIR")
            .or_else(|| env::var_os("XDG_RUNTIME_DIR"))
            .map(PathBuf::from)
            .ok_or("PIPEWIRE_RUNTIME_DIR or XDG_RUNTIME_DIR must be set")?;
        fs::create_dir_all(&runtime_dir)?;
        let socket = runtime_dir.join("pipewire-0");
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o666))?;
        println!("physical ADR PipeWire daemon ready at {}", socket.display());
        let endpoint = PhysicalHdaEndpoint {
            controller,
            frames: 0,
            periods: 0,
            ioc_irqs: 0,
            underruns: 0,
            started_at: None,
            last_lpib: (0, 0),
        };
        return protocol::serve_daemon_with_physical_sink(&listener, Box::new(endpoint))
            .map_err(Into::into);
    }
    if playback {
        let mut period = AdrPcmPeriod::default();
        let mut tone = Vec::with_capacity(7680);
        for frame in 0..1920 {
            let phase = 2.0 * std::f64::consts::PI * 440.0 * frame as f64 / 48_000.0;
            let sample = (phase.sin() * 256.0) as i16;
            tone.extend_from_slice(&sample.to_le_bytes());
            tone.extend_from_slice(&sample.to_le_bytes());
        }
        period
            .write(&tone)
            .map_err(|error| io::Error::other(format!("ADR PCM period: {error:?}")))?;
        let report = controller
            .play_pcm_period(0, &period.pcm)
            .map_err(format_hda)?;
        println!(
            "playback route={:?} stream={} position={}..{} irq_count={} amp_gain_step={}",
            report.route,
            report.stream_index,
            report.start_position,
            report.end_position,
            report.irq_count,
            report.amp_gain_step
        );
    }
    controller.shutdown();
    println!("AMD HDA reset/CORB/RIRB/codec enumeration completed");
    Ok(())
}
fn format_hda(error: HdaError<io::Error>) -> io::Error {
    io::Error::other(format!("{error:?}"))
}
