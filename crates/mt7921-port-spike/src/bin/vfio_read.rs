//! Strictly read-only no-plastic MT7921 VFIO inventory.
#![cfg(target_os = "linux")]

use mt7921_port_spike::{
    DisabledFirmwareStageError, DisabledFirmwareStageEvent, DisabledFirmwareStageTransport,
    DisabledFwdlError, DisabledFwdlEvent, DisabledFwdlInterruptTransport, DisabledFwdlRegister,
    DisabledFwdlRingTransport, DisabledFwdlWrite, DisabledInterruptError, DisabledInterruptEvent,
    DmaDescriptor, DynamicL1Error, DynamicL1Event, DynamicL1Transport, IrqLifecycle,
    MT_HIF_REMAP_L1_BAR_OFFSET, MT_HIF_REMAP_WINDOW_BAR_OFFSET, MT_TOP_LPCR_HOST_DRV_OWN,
    MT7921_FWDL_CHUNK_BYTES, MT7921_FWDL_RING_BYTES, OwnershipError, OwnershipEvent,
    OwnershipTransport, PCIE_LPCR_HOST_CLR_OWN, Patch, PciIrqCapability, PciIrqKind,
    ReadOnlyStatus, ReadRegister, TopOwnershipError, TopOwnershipEvent, TopOwnershipTransport,
    acquire_driver_ownership, acquire_top_driver_ownership, mask_ack_disabled_fwdl_interrupt,
    program_disabled_fwdl_ring, read_dynamic_identity_status, select_vfio_irq,
    stage_disabled_firmware_chunk,
};
use std::{
    cell::Cell,
    env,
    fs::{File, OpenOptions},
    os::fd::{AsRawFd, RawFd},
    process::Command,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

const VFIO_TYPE: u64 = b';' as u64;
const VFIO_BASE: u64 = 100;
const VFIO_DEVICE_GET_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 7);
const VFIO_DEVICE_GET_REGION_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 8);
const VFIO_DEVICE_GET_IRQ_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 9);
const VFIO_DEVICE_SET_IRQS: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 10);
const VFIO_DEVICE_RESET: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 11);
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
const BAR0_REGION: u32 = 0;
const VFIO_DEVICE_FLAGS_RESET: u32 = 1;
const PAGE: usize = 4096;
const VFIO_IRQ_SET_DATA_NONE: u32 = 1;
const VFIO_IRQ_SET_DATA_EVENTFD: u32 = 1 << 2;
const VFIO_IRQ_SET_ACTION_TRIGGER: u32 = 1 << 5;
const EFD_CLOEXEC: i32 = 0x80000;
const EFD_NONBLOCK: i32 = 0x800;
const SIGHUP: i32 = 1;
const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;
const SIG_ERR: usize = usize::MAX;
const PATCH_PATH: &str =
    "/run/current-system/firmware/mediatek/WIFI_MT7961_patch_mcu_1_2_hdr.bin.zst";

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

unsafe extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn eventfd(initval: u32, flags: i32) -> i32;
    fn read(fd: i32, buffer: *mut u8, count: usize) -> isize;
    fn close(fd: i32) -> i32;
    fn signal(number: i32, handler: usize) -> usize;
}

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn request_stop(_: i32) {
    STOP_REQUESTED.store(true, Ordering::Release);
}

#[allow(dead_code)]
struct ActiveSignalGuard {
    previous: [(i32, usize); 3],
}
#[allow(dead_code)]
impl ActiveSignalGuard {
    fn install() -> Result<Self, String> {
        STOP_REQUESTED.store(false, Ordering::Release);
        let mut previous = [(0, 0); 3];
        for (slot, number) in previous.iter_mut().zip([SIGHUP, SIGINT, SIGTERM]) {
            let handler = unsafe { signal(number, request_stop as *const () as usize) };
            if handler == SIG_ERR {
                for (installed_number, installed_handler) in
                    previous.iter().copied().take_while(|entry| entry.0 != 0)
                {
                    unsafe { signal(installed_number, installed_handler) };
                }
                return Err(format!(
                    "install active-DMA signal handler: {}",
                    std::io::Error::last_os_error()
                ));
            }
            *slot = (number, handler);
        }
        Ok(Self { previous })
    }
    fn stop_requested(&self) -> bool {
        STOP_REQUESTED.load(Ordering::Acquire)
    }
}
impl Drop for ActiveSignalGuard {
    fn drop(&mut self) {
        for (number, handler) in self.previous {
            unsafe { signal(number, handler) };
        }
        STOP_REQUESTED.store(false, Ordering::Release);
    }
}

fn main() {
    if let Err(message) = run() {
        eprintln!("mt7921-vfio-read: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let operation = match env::args().nth(1).as_deref() {
        None => Operation::ReadFixed,
        Some("--acquire-driver-ownership") => Operation::AcquireDriverOwnership,
        Some("--read-dynamic-identity") => Operation::ReadDynamicIdentity,
        Some("--acquire-top-ownership") => Operation::AcquireTopOwnership,
        Some("--program-disabled-fwdl-ring") => Operation::ProgramDisabledFwdlRing,
        Some("--mask-ack-disabled-fwdl") => Operation::MaskAckDisabledFwdl,
        Some("--stage-disabled-firmware-descriptor") => Operation::StageDisabledFirmwareDescriptor,
        Some("--inventory-vfio-irqs") => Operation::InventoryVfioIrqs,
        Some("--install-disable-vfio-irq") => Operation::InstallDisableVfioIrq,
        Some("--run-one-shot-fwdl") => {
            return Err("active firmware DMA is disabled pending global-ring ownership, VFIO IRQ, and valid PATCH_START protocol".into());
        }
        Some(argument) => return Err(format!("unknown argument {argument}")),
    };
    let acquire = operation == Operation::AcquireDriverOwnership;
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

    let wfdma = ReadPage::map(
        &device,
        &info,
        0xd4000,
        matches!(
            operation,
            Operation::ProgramDisabledFwdlRing | Operation::MaskAckDisabledFwdl
        ),
    )?;
    let conn = ReadPage::map(&device, &info, 0xe0000, acquire)?;
    if matches!(
        operation,
        Operation::InventoryVfioIrqs | Operation::InstallDisableVfioIrq
    ) {
        let capabilities = vfio_irq_capabilities(&device)?;
        for capability in &capabilities {
            println!("{{\"vfio_irq_capability\":\"{capability:?}\"}}");
        }
        let selected = select_vfio_irq(&capabilities)
            .ok_or("VFIO exposes no eventfd-capable PCI interrupt")?;
        println!("{{\"vfio_irq_selected\":\"{selected:?}\"}}");
        if operation == Operation::InstallDisableVfioIrq {
            let lifecycle = IrqLifecycle::Uninstalled
                .install(selected)
                .map_err(|error| format!("install IRQ lifecycle: {error:?}"))?;
            let mut irq = VfioIrq::install(&device, selected)?;
            println!("{{\"vfio_irq_event\":\"eventfd_installed\"}}");
            if irq.try_read()?.is_some() {
                return Err("unexpected IRQ before device source enable".into());
            }
            irq.disable()?;
            lifecycle
                .disable()
                .map_err(|error| format!("disable IRQ lifecycle: {error:?}"))?;
            println!("{{\"vfio_irq_event\":\"eventfd_empty_and_disabled\"}}");
            reset_vfio_device(&device)?;
            println!("{{\"vfio_irq_event\":\"vfio_device_reset_completed\"}}");
        }
    }
    if acquire {
        let mut transport = VfioOwnership {
            page: &conn,
            start: Instant::now(),
        };
        acquire_driver_ownership(&mut transport, log_ownership_event).map_err(
            |error| match error {
                OwnershipError::Transport(error) => error,
                OwnershipError::ClockOverflow => "ownership clock overflow".into(),
                OwnershipError::UnexpectedState(raw) => {
                    format!("unexpected ownership state {raw:#010x}")
                }
                OwnershipError::Timeout => "driver ownership timed out after 500 ms".into(),
            },
        )?;
    }
    if matches!(
        operation,
        Operation::ReadDynamicIdentity | Operation::AcquireTopOwnership
    ) {
        let selector = ReadPage::map(&device, &info, 0xfe000, true)?;
        let window = ReadPage::map(
            &device,
            &info,
            MT_HIF_REMAP_WINDOW_BAR_OFFSET,
            operation == Operation::AcquireTopOwnership,
        )?;
        if operation == Operation::ReadDynamicIdentity {
            let mut transport = VfioDynamicL1 {
                selector: &selector,
                window: &window,
                saved: Cell::new(None),
            };
            let status = read_dynamic_identity_status(&mut transport, log_dynamic_l1_event)
                .map_err(|error| match error {
                    DynamicL1Error::Transport(error) => error,
                    DynamicL1Error::SelectorMismatch { expected_base, raw } => format!(
                        "dynamic L1 selector mismatch: expected {expected_base:#06x}, read {raw:#010x}"
                    ),
                    DynamicL1Error::Restore(error) => {
                        format!("restore dynamic L1 selector: {error}")
                    }
                })?;
            if status.chip_id != 0x7961 {
                return Err(format!(
                    "dynamic chip ID is {:#x}, expected 0x7961",
                    status.chip_id
                ));
            }
            println!(
                "{{\"dynamic_identity\":{{\"chip_id\":\"{:#010x}\",\"revision\":\"{:#010x}\",\"hardware_bound\":\"{:#010x}\",\"top_low_power_control\":\"{:#010x}\"}}}}",
                status.chip_id,
                status.revision,
                status.hardware_bound,
                status.top_low_power_control
            );
        } else {
            let mut transport = VfioTopOwnership {
                selector: &selector,
                window: &window,
                start: Instant::now(),
                saved: Cell::new(None),
            };
            acquire_top_driver_ownership(&mut transport, log_top_ownership_event).map_err(
                |error| match error {
                    TopOwnershipError::Transport(error) => error,
                    TopOwnershipError::ClockOverflow => "MT_TOP ownership clock overflow".into(),
                    TopOwnershipError::SelectorMismatch(raw) => {
                        format!("MT_TOP selector mismatch {raw:#010x}")
                    }
                    TopOwnershipError::UnexpectedState(raw) => {
                        format!("unexpected MT_TOP ownership state {raw:#010x}")
                    }
                    TopOwnershipError::Timeout => "MT_TOP driver ownership timed out".into(),
                    TopOwnershipError::Restore(error) => {
                        format!("restore MT_TOP selector: {error}")
                    }
                },
            )?;
        }
    }
    if operation == Operation::ProgramDisabledFwdlRing {
        let mut arena = DmaArena::map(&iommu, ioas.id, 0x0100_0000)?;
        arena.initialize_fwdl_descriptors()?;
        println!(
            "{{\"fwdl_ring_event\":\"arena_initialized\",\"iova\":\"{:#010x}\",\"mapped_bytes\":{},\"descriptor_bytes\":{}}}",
            arena.iova, arena.len, MT7921_FWDL_RING_BYTES
        );
        let mut transport = VfioFwdlRing { page: &wfdma };
        program_disabled_fwdl_ring(&mut transport, arena.iova, log_disabled_fwdl_event)
            .map_err(|error| match error {
                DisabledFwdlError::InvalidArena => "invalid firmware ring arena".into(),
                DisabledFwdlError::DmaOrInterruptActive {
                    global_config,
                    interrupt_enable,
                } => format!(
                    "refused active WFDMA state global={global_config:#010x} interrupts={interrupt_enable:#010x}"
                ),
                DisabledFwdlError::Transport(error) => error,
                DisabledFwdlError::Readback { expected, actual } => {
                    format!("firmware ring readback mismatch expected={expected:?} actual={actual:?}")
                }
                DisabledFwdlError::Restore(error) => format!("restore firmware ring: {error}"),
            })?;
        arena.teardown()?;
        println!(
            "{{\"fwdl_ring_event\":\"arena_unmapped\",\"iova\":\"0x01000000\",\"bytes\":4096}}"
        );
        reset_vfio_device(&device)?;
        println!("{{\"fwdl_ring_event\":\"vfio_device_reset_completed\"}}");
    }
    if operation == Operation::MaskAckDisabledFwdl {
        let mut transport = VfioFwdlInterrupt { page: &wfdma };
        mask_ack_disabled_fwdl_interrupt(&mut transport, log_disabled_interrupt_event).map_err(
            |error| match error {
                DisabledInterruptError::DmaActive(raw) => {
                    format!("refused active WFDMA state {raw:#010x}")
                }
                DisabledInterruptError::InterruptsEnabled(raw) => {
                    format!("refused enabled interrupt mask {raw:#010x}")
                }
                DisabledInterruptError::Transport(error) => error,
                DisabledInterruptError::MaskReadback(raw) => {
                    format!("interrupt mask did not clear: {raw:#010x}")
                }
                DisabledInterruptError::AckDidNotClear(raw) => {
                    format!("firmware-download interrupt did not clear: {raw:#010x}")
                }
                DisabledInterruptError::Restore(error) => {
                    format!("restore interrupt mask: {error}")
                }
            },
        )?;
        reset_vfio_device(&device)?;
        println!("{{\"fwdl_interrupt_event\":\"vfio_device_reset_completed\"}}");
    }
    if operation == Operation::StageDisabledFirmwareDescriptor {
        let global = wfdma.read(ReadRegister::WfdmaGlobalConfig.bar_offset())?;
        let interrupt_enable = wfdma.read(0xd4204)?;
        if global & 0x5 != 0 || interrupt_enable != 0 {
            return Err(format!(
                "refused active WFDMA state global={global:#010x} interrupts={interrupt_enable:#010x}"
            ));
        }
        println!(
            "{{\"fwdl_stage_event\":\"disabled_state_verified\",\"global_config\":\"{global:#010x}\",\"interrupt_enable\":\"{interrupt_enable:#010x}\"}}"
        );
        let patch_bytes = decompress_patch()?;
        let patch =
            Patch::parse(&patch_bytes).map_err(|error| format!("patch format: {error:?}"))?;
        let section = patch.sections().next().ok_or("patch has no section")?;
        let chunk = section
            .payload
            .get(..MT7921_FWDL_CHUNK_BYTES)
            .ok_or("patch section is smaller than one firmware chunk")?;
        let mut ring = DmaArena::map(&iommu, ioas.id, 0x0100_0000)?;
        let mut payload = DmaArena::map(&iommu, ioas.id, 0x0100_1000)?;
        ring.write_descriptor(DmaDescriptor::reset());
        let payload_iova = payload.iova;
        let stage = {
            let mut transport = VfioDisabledFirmwareStage {
                ring: &mut ring,
                payload: &mut payload,
            };
            stage_disabled_firmware_chunk(
                &mut transport,
                payload_iova,
                chunk,
                log_disabled_firmware_stage_event,
            )
            .map_err(|error| match error {
                DisabledFirmwareStageError::InvalidPayload => "invalid firmware chunk".into(),
                DisabledFirmwareStageError::InvalidIova => "invalid firmware payload IOVA".into(),
                DisabledFirmwareStageError::Descriptor(error) => {
                    format!("firmware descriptor: {error:?}")
                }
                DisabledFirmwareStageError::Transport(error) => error,
                DisabledFirmwareStageError::DescriptorReadback { expected, actual } => {
                    format!("descriptor readback mismatch expected={expected:?} actual={actual:?}")
                }
                DisabledFirmwareStageError::Reset(error) => format!("reset staged memory: {error}"),
            })
        };
        let payload_unmap = payload.teardown();
        let ring_unmap = ring.teardown();
        let reset = reset_vfio_device(&device);
        stage?;
        payload_unmap?;
        ring_unmap?;
        reset?;
        println!("{{\"fwdl_stage_event\":\"arenas_unmapped_and_vfio_device_reset\"}}");
    }
    let read = |register: ReadRegister| -> Result<u32, String> {
        let page = match register.bar_offset() / PAGE {
            0xd4 => &wfdma,
            0xe0 => &conn,
            _ => return Err("register escaped immutable page allowlist".into()),
        };
        page.read(register.bar_offset())
    };
    if !matches!(
        operation,
        Operation::ProgramDisabledFwdlRing
            | Operation::MaskAckDisabledFwdl
            | Operation::StageDisabledFirmwareDescriptor
            | Operation::InstallDisableVfioIrq
    ) {
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
    }
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

struct DmaArena<'a> {
    iommu: &'a File,
    ioas: u32,
    ptr: NonNull<u8>,
    len: usize,
    iova: u64,
    mapped: bool,
}
impl<'a> DmaArena<'a> {
    fn map(iommu: &'a File, ioas: u32, iova: u64) -> Result<Self, String> {
        let len = PAGE;
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
        .filter(|pointer| pointer.as_ptr() as isize != -1)
        .ok_or_else(|| format!("allocate DMA arena: {}", std::io::Error::last_os_error()))?;
        let mut map = IoasMap {
            size: size::<IoasMap>(),
            flags: IOMMU_MAP_FIXED | IOMMU_MAP_READABLE | IOMMU_MAP_WRITEABLE,
            ioas_id: ioas,
            user_va: ptr.as_ptr() as u64,
            length: len as u64,
            iova,
            ..Default::default()
        };
        if let Err(error) = ioctl_mut(iommu.as_raw_fd(), IOMMU_IOAS_MAP, &mut map, "map DMA arena")
        {
            unsafe { munmap(ptr.as_ptr(), len) };
            return Err(error);
        }
        if map.iova != iova || map.iova + len as u64 - 1 > u64::from(u32::MAX) {
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
            unsafe { munmap(ptr.as_ptr(), len) };
            return Err("iommufd did not honor low-32-bit fixed IOVA".into());
        }
        Ok(Self {
            iommu,
            ioas,
            ptr,
            len,
            iova,
            mapped: true,
        })
    }
    fn initialize_fwdl_descriptors(&mut self) -> Result<(), String> {
        if MT7921_FWDL_RING_BYTES > self.len {
            return Err("firmware ring exceeds DMA arena".into());
        }
        unsafe { std::ptr::write_bytes(self.ptr.as_ptr(), 0, self.len) };
        for offset in (0..MT7921_FWDL_RING_BYTES).step_by(16) {
            unsafe {
                std::ptr::write_volatile(self.ptr.as_ptr().add(offset + 4).cast::<u32>(), 1 << 31)
            };
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        Ok(())
    }
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > self.len {
            return Err("DMA payload exceeds arena".into());
        }
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr.as_ptr(), bytes.len()) };
        Ok(())
    }
    fn zero_bytes(&mut self, length: usize) -> Result<(), String> {
        if length > self.len {
            return Err("DMA zero exceeds arena".into());
        }
        unsafe { std::ptr::write_bytes(self.ptr.as_ptr(), 0, length) };
        Ok(())
    }
    fn write_descriptor(&mut self, descriptor: DmaDescriptor) {
        for (index, word) in [
            descriptor.buf0,
            descriptor.ctrl,
            descriptor.buf1,
            descriptor.info,
        ]
        .into_iter()
        .enumerate()
        {
            unsafe { std::ptr::write_volatile(self.ptr.as_ptr().cast::<u32>().add(index), word) };
        }
    }
    fn read_descriptor(&self) -> DmaDescriptor {
        let word =
            |index| unsafe { std::ptr::read_volatile(self.ptr.as_ptr().cast::<u32>().add(index)) };
        DmaDescriptor {
            buf0: word(0),
            ctrl: word(1),
            buf1: word(2),
            info: word(3),
        }
    }
    fn teardown(&mut self) -> Result<(), String> {
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
        unsafe { munmap(self.ptr.as_ptr(), self.len) };
        Ok(())
    }
}

struct VfioDisabledFirmwareStage<'a, 'b> {
    ring: &'a mut DmaArena<'b>,
    payload: &'a mut DmaArena<'b>,
}
impl DisabledFirmwareStageTransport for VfioDisabledFirmwareStage<'_, '_> {
    type Error = String;
    fn write_payload(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.payload.write_bytes(bytes)
    }
    fn write_descriptor(&mut self, descriptor: DmaDescriptor) -> Result<(), Self::Error> {
        self.ring.write_descriptor(descriptor);
        Ok(())
    }
    fn release_fence(&mut self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release)
    }
    fn read_descriptor(&mut self) -> Result<DmaDescriptor, Self::Error> {
        Ok(self.ring.read_descriptor())
    }
    fn reset_descriptor(&mut self) -> Result<(), Self::Error> {
        self.ring.write_descriptor(DmaDescriptor::reset());
        Ok(())
    }
    fn zero_payload(&mut self, length: usize) -> Result<(), Self::Error> {
        self.payload.zero_bytes(length)
    }
}
impl Drop for DmaArena<'_> {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
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

#[allow(dead_code)]
struct VfioIrq {
    device_fd: RawFd,
    event_fd: RawFd,
    index: u32,
    installed: bool,
}
#[allow(dead_code)]
impl VfioIrq {
    fn install(device: &File, capability: PciIrqCapability) -> Result<Self, String> {
        if capability.count == 0 || !capability.eventfd {
            return Err("refused non-eventfd VFIO interrupt".into());
        }
        let index = match capability.kind {
            PciIrqKind::Intx => 0,
            PciIrqKind::Msi => 1,
            PciIrqKind::Msix => 2,
        };
        let event_fd = unsafe { eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK) };
        if event_fd < 0 {
            return Err(format!(
                "create IRQ eventfd: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut set = IrqSetEventfd {
            header: IrqSetHeader {
                argsz: size::<IrqSetEventfd>(),
                flags: VFIO_IRQ_SET_DATA_EVENTFD | VFIO_IRQ_SET_ACTION_TRIGGER,
                index,
                start: 0,
                count: 1,
            },
            eventfd: event_fd,
        };
        if let Err(error) = ioctl_mut(
            device.as_raw_fd(),
            VFIO_DEVICE_SET_IRQS,
            &mut set,
            "install VFIO IRQ eventfd",
        ) {
            unsafe { close(event_fd) };
            return Err(error);
        }
        Ok(Self {
            device_fd: device.as_raw_fd(),
            event_fd,
            index,
            installed: true,
        })
    }
    fn try_read(&self) -> Result<Option<u64>, String> {
        let mut counter = 0u64;
        let result = unsafe {
            read(
                self.event_fd,
                (&mut counter as *mut u64).cast::<u8>(),
                std::mem::size_of::<u64>(),
            )
        };
        if result == std::mem::size_of::<u64>() as isize {
            Ok(Some(counter))
        } else if result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(11) {
            Ok(None)
        } else {
            Err(format!(
                "read IRQ eventfd: {}",
                std::io::Error::last_os_error()
            ))
        }
    }
    fn disable(&mut self) -> Result<(), String> {
        if !self.installed {
            return Ok(());
        }
        let mut set = IrqSetHeader {
            argsz: size::<IrqSetHeader>(),
            flags: VFIO_IRQ_SET_DATA_NONE | VFIO_IRQ_SET_ACTION_TRIGGER,
            index: self.index,
            start: 0,
            count: 0,
        };
        ioctl_mut(
            self.device_fd,
            VFIO_DEVICE_SET_IRQS,
            &mut set,
            "disable VFIO IRQ eventfd",
        )?;
        self.installed = false;
        Ok(())
    }
}
impl Drop for VfioIrq {
    fn drop(&mut self) {
        let _ = self.disable();
        unsafe { close(self.event_fd) };
    }
}

struct ReadPage {
    ptr: NonNull<u8>,
    bar_page: usize,
}
impl ReadPage {
    fn map(
        device: &File,
        region: &RegionInfo,
        bar_page: usize,
        writable: bool,
    ) -> Result<Self, String> {
        if bar_page % PAGE != 0 || bar_page + PAGE > region.size as usize {
            return Err("allowlisted BAR page is outside BAR 0".into());
        }
        let ptr = NonNull::new(unsafe {
            mmap(
                std::ptr::null_mut(),
                PAGE,
                PROT_READ | if writable { PROT_WRITE } else { 0 },
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
    fn write_clear_own(&self) -> Result<(), String> {
        let offset = ReadRegister::ConnOnLowPowerControl.bar_offset();
        let within = offset - self.bar_page;
        if self.bar_page != 0xe0000 || within + 4 > PAGE {
            return Err("CLR_OWN write escaped immutable allowlist".into());
        }
        unsafe {
            std::ptr::write_volatile(
                self.ptr.as_ptr().add(within).cast::<u32>(),
                PCIE_LPCR_HOST_CLR_OWN,
            )
        };
        Ok(())
    }
    fn write_remap_selector(&self, value: u32) -> Result<(), String> {
        let within = MT_HIF_REMAP_L1_BAR_OFFSET - self.bar_page;
        if self.bar_page != 0xfe000 || within + 4 > PAGE {
            return Err("remap selector write escaped immutable allowlist".into());
        }
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        Ok(())
    }
    fn write_top_driver_own(&self) -> Result<(), String> {
        let offset = MT_HIF_REMAP_WINDOW_BAR_OFFSET + 0x10;
        let within = offset - self.bar_page;
        if self.bar_page != MT_HIF_REMAP_WINDOW_BAR_OFFSET || within + 4 > PAGE {
            return Err("MT_TOP driver-own write escaped immutable allowlist".into());
        }
        unsafe {
            std::ptr::write_volatile(
                self.ptr.as_ptr().add(within).cast::<u32>(),
                MT_TOP_LPCR_HOST_DRV_OWN,
            )
        };
        Ok(())
    }
    fn write_fwdl_ring(&self, register: DisabledFwdlWrite, value: u32) -> Result<(), String> {
        let offset = match register {
            DisabledFwdlWrite::DescriptorBase => 0xd4400,
            DisabledFwdlWrite::DescriptorCount => 0xd4404,
            DisabledFwdlWrite::CpuIndex => 0xd4408,
        };
        let within = offset - self.bar_page;
        if self.bar_page != 0xd4000 || within + 4 > PAGE {
            return Err("firmware ring write escaped immutable allowlist".into());
        }
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        Ok(())
    }
    fn write_fwdl_interrupt_enable(&self, value: u32) -> Result<(), String> {
        if self.bar_page != 0xd4000 || value != 0 {
            return Err("interrupt-mask write escaped zero-only allowlist".into());
        }
        let within = 0xd4204 - self.bar_page;
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        Ok(())
    }
    fn acknowledge_fwdl_interrupt(&self, value: u32) -> Result<(), String> {
        if self.bar_page != 0xd4000 || value & !(1 << 26) != 0 {
            return Err("interrupt acknowledgement escaped FWDL-only allowlist".into());
        }
        let within = 0xd4200 - self.bar_page;
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        Ok(())
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

fn vfio_irq_capabilities(device: &File) -> Result<Vec<PciIrqCapability>, String> {
    let mut capabilities = Vec::new();
    for (index, kind) in [PciIrqKind::Intx, PciIrqKind::Msi, PciIrqKind::Msix]
        .into_iter()
        .enumerate()
    {
        let mut irq = IrqInfo {
            argsz: size::<IrqInfo>(),
            index: index as u32,
            ..Default::default()
        };
        ioctl_mut(
            device.as_raw_fd(),
            VFIO_DEVICE_GET_IRQ_INFO,
            &mut irq,
            "query VFIO IRQ",
        )?;
        capabilities.push(PciIrqCapability {
            kind,
            count: irq.count,
            eventfd: irq.flags & 1 != 0,
        });
    }
    Ok(capabilities)
}

fn reset_vfio_device(device: &File) -> Result<(), String> {
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
    if unsafe { ioctl(device.as_raw_fd(), VFIO_DEVICE_RESET) } < 0 {
        return Err(format!(
            "VFIO device reset: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

struct VfioOwnership<'a> {
    page: &'a ReadPage,
    start: Instant,
}
impl OwnershipTransport for VfioOwnership<'_> {
    type Error = String;
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    fn write_clear_own(&mut self) -> Result<(), Self::Error> {
        self.page.write_clear_own()
    }
    fn read_low_power_control(&mut self) -> Result<u32, Self::Error> {
        self.page
            .read(ReadRegister::ConnOnLowPowerControl.bar_offset())
    }
    fn sleep_ms(&mut self, milliseconds: u64) {
        std::thread::sleep(std::time::Duration::from_millis(milliseconds));
    }
}

fn log_ownership_event(event: OwnershipEvent) {
    match event {
        OwnershipEvent::ClearOwnWritten { attempt, at_ms } => println!(
            "{{\"ownership_event\":\"clear_own_written\",\"attempt\":{attempt},\"at_ms\":{at_ms},\"value\":\"{PCIE_LPCR_HOST_CLR_OWN:#010x}\"}}"
        ),
        OwnershipEvent::StatusRead {
            attempt,
            at_ms,
            raw,
        } => println!(
            "{{\"ownership_event\":\"status_read\",\"attempt\":{attempt},\"at_ms\":{at_ms},\"raw\":\"{raw:#010x}\"}}"
        ),
        OwnershipEvent::AttemptExpired { attempt, at_ms } => println!(
            "{{\"ownership_event\":\"attempt_expired\",\"attempt\":{attempt},\"at_ms\":{at_ms}}}"
        ),
        OwnershipEvent::Acquired { attempt, at_ms } => println!(
            "{{\"ownership_event\":\"driver_ownership_acquired\",\"attempt\":{attempt},\"at_ms\":{at_ms}}}"
        ),
        OwnershipEvent::UnexpectedState {
            attempt,
            at_ms,
            raw,
        } => println!(
            "{{\"ownership_event\":\"unexpected_state\",\"attempt\":{attempt},\"at_ms\":{at_ms},\"raw\":\"{raw:#010x}\"}}"
        ),
        OwnershipEvent::TimedOut { at_ms } => {
            println!("{{\"ownership_event\":\"timed_out\",\"at_ms\":{at_ms}}}")
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Operation {
    ReadFixed,
    AcquireDriverOwnership,
    ReadDynamicIdentity,
    AcquireTopOwnership,
    ProgramDisabledFwdlRing,
    MaskAckDisabledFwdl,
    StageDisabledFirmwareDescriptor,
    InventoryVfioIrqs,
    InstallDisableVfioIrq,
}

struct VfioDynamicL1<'a> {
    selector: &'a ReadPage,
    window: &'a ReadPage,
    saved: Cell<Option<u32>>,
}
impl DynamicL1Transport for VfioDynamicL1<'_> {
    type Error = String;
    fn read_selector(&mut self) -> Result<u32, Self::Error> {
        let value = self.selector.read(MT_HIF_REMAP_L1_BAR_OFFSET)?;
        if self.saved.get().is_none() {
            self.saved.set(Some(value));
        }
        Ok(value)
    }
    fn write_selector(&mut self, value: u32) -> Result<(), Self::Error> {
        let low = value & 0xffff;
        if low != 0x7001 && low != 0x1806 && Some(value) != self.saved.get() {
            return Err(format!(
                "selector value {value:#010x} escaped target allowlist"
            ));
        }
        self.selector.write_remap_selector(value)
    }
    fn read_window(&mut self, offset: u16) -> Result<u32, Self::Error> {
        match (
            self.selector.read(MT_HIF_REMAP_L1_BAR_OFFSET)? & 0xffff,
            offset,
        ) {
            (0x7001, 0x0200 | 0x0204 | 0x0020) | (0x1806, 0x0010) => self
                .window
                .read(MT_HIF_REMAP_WINDOW_BAR_OFFSET + usize::from(offset)),
            (base, _) => Err(format!(
                "dynamic read base {base:#06x} offset {offset:#06x} escaped target allowlist"
            )),
        }
    }
}

fn log_dynamic_l1_event(event: DynamicL1Event) {
    match event {
        DynamicL1Event::SelectorSaved { raw } => {
            println!("{{\"dynamic_l1_event\":\"selector_saved\",\"raw\":\"{raw:#010x}\"}}")
        }
        DynamicL1Event::SelectorWritten { base, raw } => println!(
            "{{\"dynamic_l1_event\":\"selector_written\",\"base\":\"{base:#06x}\",\"raw\":\"{raw:#010x}\"}}"
        ),
        DynamicL1Event::SelectorVerified { base, raw } => println!(
            "{{\"dynamic_l1_event\":\"selector_verified\",\"base\":\"{base:#06x}\",\"raw\":\"{raw:#010x}\"}}"
        ),
        DynamicL1Event::RegisterRead {
            name,
            physical,
            value,
        } => println!(
            "{{\"dynamic_l1_event\":\"register_read\",\"name\":\"{name}\",\"physical\":\"{physical:#010x}\",\"value\":\"{value:#010x}\"}}"
        ),
        DynamicL1Event::SelectorRestored { raw } => {
            println!("{{\"dynamic_l1_event\":\"selector_restored\",\"raw\":\"{raw:#010x}\"}}")
        }
    }
}

struct VfioTopOwnership<'a> {
    selector: &'a ReadPage,
    window: &'a ReadPage,
    start: Instant,
    saved: Cell<Option<u32>>,
}
impl TopOwnershipTransport for VfioTopOwnership<'_> {
    type Error = String;
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    fn read_selector(&mut self) -> Result<u32, Self::Error> {
        let value = self.selector.read(MT_HIF_REMAP_L1_BAR_OFFSET)?;
        if self.saved.get().is_none() {
            self.saved.set(Some(value));
        }
        Ok(value)
    }
    fn write_selector(&mut self, value: u32) -> Result<(), Self::Error> {
        if value & 0xffff != 0x1806 && Some(value) != self.saved.get() {
            return Err(format!("MT_TOP selector {value:#010x} escaped allowlist"));
        }
        self.selector.write_remap_selector(value)
    }
    fn write_top_driver_own(&mut self) -> Result<(), Self::Error> {
        self.window.write_top_driver_own()
    }
    fn read_top_low_power_control(&mut self) -> Result<u32, Self::Error> {
        if self.selector.read(MT_HIF_REMAP_L1_BAR_OFFSET)? & 0xffff != 0x1806 {
            return Err("MT_TOP read attempted without 0x1806 selector".into());
        }
        self.window.read(MT_HIF_REMAP_WINDOW_BAR_OFFSET + 0x10)
    }
    fn sleep_ms(&mut self, milliseconds: u64) {
        std::thread::sleep(std::time::Duration::from_millis(milliseconds));
    }
}

fn log_top_ownership_event(event: TopOwnershipEvent) {
    match event {
        TopOwnershipEvent::SelectorSaved { raw } => {
            println!("{{\"top_ownership_event\":\"selector_saved\",\"raw\":\"{raw:#010x}\"}}")
        }
        TopOwnershipEvent::SelectorWritten { raw } => {
            println!("{{\"top_ownership_event\":\"selector_written\",\"raw\":\"{raw:#010x}\"}}")
        }
        TopOwnershipEvent::SelectorVerified { raw } => {
            println!("{{\"top_ownership_event\":\"selector_verified\",\"raw\":\"{raw:#010x}\"}}")
        }
        TopOwnershipEvent::DriverOwnWritten { at_ms } => println!(
            "{{\"top_ownership_event\":\"driver_own_written\",\"at_ms\":{at_ms},\"value\":\"{MT_TOP_LPCR_HOST_DRV_OWN:#010x}\"}}"
        ),
        TopOwnershipEvent::StatusRead { at_ms, raw } => println!(
            "{{\"top_ownership_event\":\"status_read\",\"at_ms\":{at_ms},\"raw\":\"{raw:#010x}\"}}"
        ),
        TopOwnershipEvent::Acquired { at_ms } => {
            println!("{{\"top_ownership_event\":\"driver_ownership_acquired\",\"at_ms\":{at_ms}}}")
        }
        TopOwnershipEvent::UnexpectedState { at_ms, raw } => println!(
            "{{\"top_ownership_event\":\"unexpected_state\",\"at_ms\":{at_ms},\"raw\":\"{raw:#010x}\"}}"
        ),
        TopOwnershipEvent::TimedOut { at_ms } => {
            println!("{{\"top_ownership_event\":\"timed_out\",\"at_ms\":{at_ms}}}")
        }
        TopOwnershipEvent::SelectorRestored { raw } => {
            println!("{{\"top_ownership_event\":\"selector_restored\",\"raw\":\"{raw:#010x}\"}}")
        }
    }
}

struct VfioFwdlRing<'a> {
    page: &'a ReadPage,
}
impl DisabledFwdlRingTransport for VfioFwdlRing<'_> {
    type Error = String;
    fn read(&mut self, register: DisabledFwdlRegister) -> Result<u32, Self::Error> {
        self.page.read(register.bar_offset())
    }
    fn write(&mut self, register: DisabledFwdlWrite, value: u32) -> Result<(), Self::Error> {
        self.page.write_fwdl_ring(register, value)
    }
    fn release_fence(&mut self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release)
    }
}

fn log_disabled_fwdl_event(event: DisabledFwdlEvent) {
    println!("{{\"fwdl_ring_event\":\"{event:?}\"}}")
}

struct VfioFwdlInterrupt<'a> {
    page: &'a ReadPage,
}
impl DisabledFwdlInterruptTransport for VfioFwdlInterrupt<'_> {
    type Error = String;
    fn read_global_config(&mut self) -> Result<u32, Self::Error> {
        self.page.read(0xd4208)
    }
    fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
        self.page.read(0xd4204)
    }
    fn write_interrupt_enable(&mut self, value: u32) -> Result<(), Self::Error> {
        self.page.write_fwdl_interrupt_enable(value)
    }
    fn read_interrupt_status(&mut self) -> Result<u32, Self::Error> {
        self.page.read(0xd4200)
    }
    fn acknowledge_interrupt_status(&mut self, value: u32) -> Result<(), Self::Error> {
        self.page.acknowledge_fwdl_interrupt(value)
    }
}

fn log_disabled_interrupt_event(event: DisabledInterruptEvent) {
    println!("{{\"fwdl_interrupt_event\":\"{event:?}\"}}")
}

fn log_disabled_firmware_stage_event(event: DisabledFirmwareStageEvent) {
    println!("{{\"fwdl_stage_event\":\"{event:?}\"}}")
}

fn decompress_patch() -> Result<Vec<u8>, String> {
    let output = Command::new("/run/current-system/sw/bin/zstdcat")
        .arg(PATCH_PATH)
        .output()
        .map_err(|error| format!("run zstdcat for {PATCH_PATH}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "zstdcat {PATCH_PATH}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vfio_irq_payload_matches_linux_uapi_layout() {
        assert_eq!(std::mem::size_of::<IrqSetHeader>(), 20);
        assert_eq!(std::mem::size_of::<IrqSetEventfd>(), 24);
        assert_eq!(
            VFIO_IRQ_SET_DATA_EVENTFD | VFIO_IRQ_SET_ACTION_TRIGGER,
            0x24
        );
        assert_eq!(VFIO_IRQ_SET_DATA_NONE | VFIO_IRQ_SET_ACTION_TRIGGER, 0x21);
    }

    #[test]
    fn active_signal_handler_requests_bounded_cleanup() {
        STOP_REQUESTED.store(false, Ordering::Release);
        request_stop(SIGTERM);
        assert!(STOP_REQUESTED.load(Ordering::Acquire));
        STOP_REQUESTED.store(false, Ordering::Release);
    }
}
