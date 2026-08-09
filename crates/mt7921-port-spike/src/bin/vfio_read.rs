//! Strictly read-only no-plastic MT7921 VFIO inventory.
#![cfg(target_os = "linux")]

use mt7921_port_spike::{
    ChannelDomainCommand, ClcSetCommand, ClcSetResponse, DisabledFirmwareStageError,
    DisabledFirmwareStageEvent, DisabledFirmwareStageTransport, DisabledFwdlError,
    DisabledFwdlEvent, DisabledFwdlInterruptTransport, DisabledFwdlRegister,
    DisabledFwdlRingTransport, DisabledFwdlWrite, DisabledInterruptError, DisabledInterruptEvent,
    DisabledMcuRxEvent, DisabledMcuRxTransport, DmaDescriptor, DmaSegment, DownloadCommand,
    DynamicL1Error, DynamicL1Event, DynamicL1Transport, Firmware, FirmwareCommandCompletion,
    FirmwareImagePart, FirmwareLoaderState, FirmwareLoaderTransport, GlobalTxRingError,
    GlobalTxRingEvent, GlobalTxRingTransport, IrqLifecycle, MT_HIF_REMAP_L1_BAR_OFFSET,
    MT_HIF_REMAP_WINDOW_BAR_OFFSET, MT_TOP_LPCR_HOST_DRV_OWN, MT7921_FWDL_CHUNK_BYTES,
    MT7921_FWDL_RING_BYTES, McuRxRegisters, OwnershipError, OwnershipEvent, OwnershipTransport,
    PCIE_LPCR_HOST_CLR_OWN, Patch, PciIrqCapability, PciIrqKind, ReadOnlyStatus, ReadRegister,
    TopOwnershipError, TopOwnershipEvent, TopOwnershipTransport, TxRingState, WfsysResetEvent,
    WfsysResetTransport, acquire_driver_ownership, acquire_top_driver_ownership,
    encode_download_command, load_mt7921_firmware, load_mt7921_firmware_through_channel_domain,
    mask_ack_disabled_fwdl_interrupt, parse_clc_set_response, parse_download_response,
    parse_eeprom_block, parse_nic_capability, prepare_global_rx_rings, prepare_global_tx_rings,
    prepare_mcu_rx_ring, program_disabled_fwdl_ring, read_dynamic_identity_status, reset_wfsys,
    select_vfio_irq, stage_disabled_firmware_chunk,
};
use std::{
    cell::Cell,
    env,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::fd::{AsRawFd, RawFd},
    process::{Command, Stdio},
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
const VFIO_REGION_INFO_FLAG_READ: u32 = 1 << 0;
const VFIO_REGION_INFO_FLAG_WRITE: u32 = 1 << 1;
const VFIO_REGION_INFO_FLAG_MMAP: u32 = 1 << 2;
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
const RAM_PATH: &str = "/run/current-system/firmware/mediatek/WIFI_RAM_CODE_MT7961_1.bin.zst";
const PATCH_SHA256: &str = "a276c06c2b772adb50b86639d33c82824ff4c21d617feb78caea74c040b873f6";
const RAM_SHA256: &str = "b94217a951518a9c14095765f367bc5dd7698f2dc033941d6f18fc2ebd6a2ab9";
const PATCH_IMAGE_BYTES: usize = 92_192;
const RAM_IMAGE_BYTES: usize = 792_036;

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
        Some("--prepare-owned-global-tx-rings") => Operation::PrepareOwnedGlobalTxRings,
        Some("--query-patch-semaphore") => Operation::QueryPatchSemaphore,
        Some("--run-one-shot-fwdl") => Operation::RunOneShotFirmware,
        Some("--run-one-shot-channel-domain") => Operation::RunOneShotChannelDomain,
        Some(argument) => return Err(format!("unknown argument {argument}")),
    };
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

    let wfdma = ReadPage::map(&device, &info, 0xd4000, operation.wfdma_writable())?;
    let pcie_mac = if matches!(
        operation,
        Operation::PrepareOwnedGlobalTxRings
            | Operation::QueryPatchSemaphore
            | Operation::RunOneShotFirmware
            | Operation::RunOneShotChannelDomain
    ) {
        Some(ReadPage::map(&device, &info, 0x10000, true)?)
    } else {
        None
    };
    let conn = ReadPage::map(&device, &info, 0xe0000, operation.conn_writable())?;
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
    if operation == Operation::AcquireDriverOwnership {
        verify_pci_dma_disabled(&bdf)?;
        set_lab_safety("MUTATED")?;
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
        let response = conn.read(ReadRegister::ConnOnLowPowerControl.bar_offset())?;
        println!(
            "{{\"ownership_event\":\"device_response_verified\",\"raw\":\"{response:#010x}\"}}"
        );
        reset_vfio_device(&device)?;
        verify_pci_dma_disabled(&bdf)?;
        set_lab_safety("SAFE")?;
        println!("{{\"ownership_event\":\"vfio_device_reset_completed\"}}");
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
    if operation == Operation::PrepareOwnedGlobalTxRings {
        verify_pci_dma_disabled(&bdf)?;
        if info.size < 0x100000 {
            return Err(format!("BAR0 is too small: {:#x}", info.size));
        }
        let pcie_mac = pcie_mac.as_ref().expect("operation mapped PCIe MAC page");
        let mac_irq = pcie_mac.read(0x10188)?;
        if mac_irq == u32::MAX {
            return Err("PCIe MAC interrupt gate returned all ones".into());
        }
        set_lab_safety("MUTATED")?;
        disable_pci_intx(&bdf)?;
        pcie_mac.write_pcie_mac_interrupt_enable_zero()?;
        if pcie_mac.read(0x10188)? != 0 {
            return Err(format!(
                "PCIe MAC interrupt gate did not clear from {mac_irq:#010x}"
            ));
        }
        let mut guard = DmaArena::map(&iommu, ioas.id, 0x0100_0000)?;
        let mut fwdl = DmaArena::map(&iommu, ioas.id, 0x0100_1000)?;
        let mut mcu = DmaArena::map(&iommu, ioas.id, 0x0100_2000)?;
        guard.initialize_descriptor_page()?;
        fwdl.initialize_descriptor_page()?;
        mcu.initialize_descriptor_page()?;
        let operation = {
            let mut transport = VfioGlobalTxRings { page: &wfdma };
            prepare_global_tx_rings(
                &mut transport,
                guard.iova,
                fwdl.iova,
                mcu.iova,
                log_global_tx_ring_event,
            )
            .map_err(|error| match error {
                GlobalTxRingError::InvalidArena => "invalid owned TX arena".into(),
                GlobalTxRingError::ActiveState {
                    global_config,
                    interrupt_enable,
                } => format!(
                    "refused active state global={global_config:#010x} interrupts={interrupt_enable:#010x}"
                ),
                GlobalTxRingError::InvalidMmio => "invalid all-ones TX ring MMIO".into(),
                GlobalTxRingError::DirtyRing { index, state } => {
                    format!("TX ring {index} is not idle: {state:?}")
                }
                GlobalTxRingError::Transport(error) => error,
                GlobalTxRingError::Readback { index, state } => {
                    format!("TX ring {index} ownership readback mismatch: {state:?}")
                }
            })
        };
        verify_pci_dma_disabled(&bdf)?;
        let global = wfdma.read(0xd4208)?;
        let host_irq = wfdma.read(0xd4204)?;
        let mac_irq = pcie_mac.read(0x10188)?;
        if global & 0xf != 0 || host_irq != 0 || mac_irq != 0 {
            return Err(format!(
                "post-program gates unsafe global={global:#010x} host_irq={host_irq:#010x} mac_irq={mac_irq:#010x}"
            ));
        }
        reset_vfio_device(&device)?;
        println!("{{\"global_tx_ring_event\":\"vfio_device_reset_while_pinned\"}}");
        verify_pci_dma_disabled(&bdf)?;
        let reset_global = wfdma.read(0xd4208)?;
        let reset_host_irq = wfdma.read(0xd4204)?;
        let reset_mac_irq = pcie_mac.read(0x10188)?;
        if reset_global & 0xf != 0 || reset_host_irq != 0 || reset_mac_irq != 0 {
            return Err(format!(
                "post-reset gates unsafe global={reset_global:#010x} host_irq={reset_host_irq:#010x} mac_irq={reset_mac_irq:#010x}"
            ));
        }
        set_lab_safety("SAFE")?;
        let guard_unmap = guard.teardown();
        let fwdl_unmap = fwdl.teardown();
        let mcu_unmap = mcu.teardown();
        guard_unmap?;
        fwdl_unmap?;
        mcu_unmap?;
        println!("{{\"global_tx_ring_event\":\"owned_arenas_unmapped_after_reset\"}}");
        operation?;
    }
    if matches!(
        operation,
        Operation::QueryPatchSemaphore
            | Operation::RunOneShotFirmware
            | Operation::RunOneShotChannelDomain
    ) {
        verify_pci_dma_disabled(&bdf)?;
        let pcie_mac = pcie_mac.as_ref().expect("operation mapped PCIe MAC page");
        let selected = select_vfio_irq(&vfio_irq_capabilities(&device)?)
            .ok_or("VFIO exposes no eventfd-capable PCI interrupt")?;
        if selected.kind == PciIrqKind::Intx {
            return Err("active MCU transaction requires MSI or MSI-X, not level INTx".into());
        }
        verify_vfio_reset_supported(&device)?;
        println!("{{\"vfio_irq_selected\":\"{selected:?}\"}}");
        let firmware_images = if matches!(
            operation,
            Operation::RunOneShotFirmware | Operation::RunOneShotChannelDomain
        ) {
            let patch = decompress_patch()?;
            let ram = decompress_ram()?;
            Patch::parse(&patch).map_err(|error| format!("parse verified patch: {error:?}"))?;
            Firmware::parse(&ram).map_err(|error| format!("parse verified RAM: {error:?}"))?;
            Some((patch, ram))
        } else {
            None
        };
        let selector_page = ReadPage::map(&device, &info, 0xfe000, true)?;
        let dynamic_window = ReadPage::map(&device, &info, MT_HIF_REMAP_WINDOW_BAR_OFFSET, true)?;
        let swdef = ReadPage::map(&device, &info, 0x9f000, true)?;
        let dmashdl = ReadPage::map(&device, &info, 0xd6000, true)?;

        let mut tx_guard = DmaArena::map(&iommu, ioas.id, 0x0100_0000)?;
        let mut fwdl_ring = DmaArena::map(&iommu, ioas.id, 0x0100_1000)?;
        let mut mcu_tx_ring = DmaArena::map(&iommu, ioas.id, 0x0100_2000)?;
        let mut rx_guard = DmaArena::map(&iommu, ioas.id, 0x0100_3000)?;
        let mut mcu_rx_ring = DmaArena::map(&iommu, ioas.id, 0x0100_4000)?;
        let mut mcu_rx_buffers = DmaArena::map_len(&iommu, ioas.id, 0x0100_5000, 4 * PAGE)?;
        let mut command_payload = DmaArena::map(&iommu, ioas.id, 0x0100_9000)?;
        let mut fwdl_payload = DmaArena::map(&iommu, ioas.id, 0x0100_a000)?;
        let mut mcu_wa_rx_ring = DmaArena::map(&iommu, ioas.id, 0x0100_b000)?;
        let mut mcu_wa_rx_buffers = DmaArena::map_len(&iommu, ioas.id, 0x0100_c000, 4 * PAGE)?;
        tx_guard.initialize_descriptor_page()?;
        fwdl_ring.initialize_descriptor_page()?;
        mcu_tx_ring.initialize_descriptor_page()?;
        rx_guard.initialize_descriptor_page()?;
        mcu_rx_ring.initialize_descriptor_page()?;
        mcu_rx_buffers.zero_bytes(4 * PAGE)?;
        command_payload.zero_bytes(PAGE)?;
        fwdl_payload.zero_bytes(PAGE)?;
        mcu_wa_rx_ring.initialize_descriptor_page()?;
        mcu_wa_rx_buffers.zero_bytes(4 * PAGE)?;
        let prepared_rx = prepare_mcu_rx_ring(mcu_rx_ring.iova, mcu_rx_buffers.iova)
            .map_err(|error| format!("prepare MCU RX descriptors: {error:?}"))?;
        for (index, descriptor) in prepared_rx.descriptors.into_iter().enumerate() {
            mcu_rx_ring.write_descriptor_at(index, descriptor);
        }
        let prepared_wa_rx = prepare_mcu_rx_ring(mcu_wa_rx_ring.iova, mcu_wa_rx_buffers.iova)
            .map_err(|error| format!("prepare post-N9 MCU RX descriptors: {error:?}"))?;
        for (index, descriptor) in prepared_wa_rx.descriptors.into_iter().enumerate() {
            mcu_wa_rx_ring.write_descriptor_at(index, descriptor);
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);

        let signal = ActiveSignalGuard::install()?;
        set_lab_safety("MUTATED")?;
        let mut irq = None;
        let active = (|| -> Result<(), String> {
            disable_pci_intx(&bdf)?;
            pcie_mac.write_pcie_mac_interrupt_enable_zero()?;
            let mut ownership = VfioOwnership {
                page: &conn,
                start: Instant::now(),
            };
            acquire_driver_ownership(&mut ownership, log_ownership_event)
                .map_err(|error| format!("acquire ownership for MCU transaction: {error:?}"))?;
            let saved_selector = selector_page.read(MT_HIF_REMAP_L1_BAR_OFFSET)?;
            let mut wfsys = VfioWfsysReset {
                selector: &selector_page,
                window: &dynamic_window,
                start: Instant::now(),
                saved: saved_selector,
            };
            let wfsys_result = reset_wfsys(&mut wfsys, log_wfsys_reset_event)
                .map_err(|error| format!("reset WFSYS: {error:?}"));
            let restore_result = wfsys.restore();
            wfsys_result?;
            restore_result?;

            let initial_global = wfdma.read(0xd4208)?;
            if initial_global == u32::MAX {
                return Err("WFDMA global configuration returned all ones".into());
            }
            let disabled = initial_global
                & !((1 << 0) | (1 << 2) | (1 << 15) | (1 << 21) | (1 << 27) | (1 << 28));
            wfdma.write_active_wfdma(0xd4208, disabled)?;
            let disable_deadline = Instant::now() + std::time::Duration::from_millis(100);
            while wfdma.read(0xd4208)? & ((1 << 1) | (1 << 3)) != 0 {
                if Instant::now() >= disable_deadline {
                    return Err("WFDMA did not quiesce before ring ownership".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let global_ext = wfdma.read(0xd42b0)?;
            if global_ext == u32::MAX {
                return Err("WFDMA extended configuration returned all ones".into());
            }
            wfdma.write_active_wfdma(0xd42b0, global_ext & !(1 << 6))?;
            dmashdl.enable_dmashdl_bypass()?;
            let reset = wfdma.read(0xd4100)?;
            if reset == u32::MAX {
                return Err("WFDMA reset control returned all ones".into());
            }
            wfdma.write_active_wfdma(0xd4100, reset & !0x30)?;
            wfdma.write_active_wfdma(0xd4100, reset | 0x30)?;
            {
                let mut transport = VfioGlobalTxRings { page: &wfdma };
                prepare_global_tx_rings(
                    &mut transport,
                    tx_guard.iova,
                    fwdl_ring.iova,
                    mcu_tx_ring.iova,
                    log_global_tx_ring_event,
                )
                .map_err(|error| format!("own global TX rings: {error:?}"))?;
            }
            {
                let mut transport = VfioGlobalRxRings { page: &wfdma };
                prepare_global_rx_rings(
                    &mut transport,
                    rx_guard.iova,
                    mcu_rx_ring.iova,
                    log_global_rx_ring_event,
                )
                .map_err(|error| format!("own global RX rings: {error:?}"))?;
                wfdma.write_rx_ring_slot(4, mcu_wa_rx_ring.iova as u32, 8, 7, 0)?;
            }
            let installed = VfioIrq::install(&device, selected)?;
            if installed.try_read()?.is_some() {
                return Err("unexpected IRQ before device source enable".into());
            }
            irq = Some(installed);
            println!("{{\"active_mcu_event\":\"vfio_irq_installed\"}}");
            if wfdma.read(0xd4200)? != 0 {
                return Err(format!(
                    "refused nonzero interrupt status before activation: {:#010x}",
                    wfdma.read(0xd4200)?
                ));
            }
            wfdma.write_active_wfdma(0xd42f0, 0)?;
            wfdma.write_active_wfdma(0xd4680, 4)?;
            wfdma.write_active_wfdma(0xd4690, 0x00c0_0004)?;
            wfdma.write_active_wfdma(0xd4640, 0x0340_0004)?;
            wfdma.write_active_wfdma(0xd4644, 0x0380_0004)?;
            set_pci_bus_master(&bdf, true)?;
            let global = wfdma.read(0xd4208)?
                | (1 << 0)
                | (1 << 2)
                | (3 << 4)
                | (1 << 6)
                | (1 << 11)
                | (1 << 12)
                | (1 << 13)
                | (1 << 15)
                | (1 << 21)
                | (1 << 28)
                | (1 << 30);
            pcie_mac.write_pcie_mac_interrupt_enable(0xff)?;
            wfdma.write_active_wfdma(0xd4208, global)?;
            let response_irq_mask = if matches!(
                operation,
                Operation::RunOneShotFirmware | Operation::RunOneShotChannelDomain
            ) {
                WM_RX_IRQ_BIT | WM2_RX_IRQ_BIT
            } else {
                1 << 0
            };
            wfdma.write_active_wfdma(0xd4204, response_irq_mask)?;
            println!(
                "{{\"active_mcu_event\":\"dma_and_response_irq_enabled\",\"global\":\"{global:#010x}\",\"irq_mask\":\"{response_irq_mask:#010x}\"}}"
            );
            let mut top = VfioTopOwnership {
                selector: &selector_page,
                window: &dynamic_window,
                start: Instant::now(),
                saved: Cell::new(None),
            };
            acquire_top_driver_ownership(&mut top, log_top_ownership_event)
                .map_err(|error| format!("acquire MT_TOP ownership: {error:?}"))?;
            pcie_mac.disable_pcie_l0s()?;
            swdef.write_swdef_normal()?;
            if matches!(
                operation,
                Operation::RunOneShotFirmware | Operation::RunOneShotChannelDomain
            ) {
                let (patch_bytes, ram_bytes) = firmware_images
                    .as_ref()
                    .expect("one-shot operation validated firmware artifacts");
                let mcu = ActiveMcuIo {
                    wfdma: &wfdma,
                    irq: irq.as_mut().expect("IRQ installed"),
                    signal: &signal,
                    tx_ring: &mut mcu_tx_ring,
                    payload: &mut command_payload,
                    wm: ActiveMcuRx {
                        rx_ring: &mut mcu_rx_ring,
                        rx_buffers: &mcu_rx_buffers,
                        rx_tail: 0,
                        rx_head: 7,
                        rx_ring_index: 0,
                        rx_count: 8,
                        irq_bit: WM_RX_IRQ_BIT,
                    },
                    wm2: Some(ActiveMcuRx {
                        rx_ring: &mut mcu_wa_rx_ring,
                        rx_buffers: &mcu_wa_rx_buffers,
                        rx_tail: 0,
                        rx_head: 7,
                        rx_ring_index: 4,
                        rx_count: 8,
                        irq_bit: WM2_RX_IRQ_BIT,
                    }),
                };
                let mut loader = VfioFirmwareLoader {
                    mcu,
                    conn: &conn,
                    pcie_mac,
                    device: &device,
                    bdf: &bdf,
                    fwdl_ring: &mut fwdl_ring,
                    fwdl_payload: &mut fwdl_payload,
                    sequence: 0,
                    command_index: 0,
                    fwdl_index: 0,
                    pending_scatter: None,
                    start: Instant::now(),
                };
                let patch = Patch::parse(patch_bytes)
                    .map_err(|error| format!("parse patch for loader: {error:?}"))?;
                let firmware = Firmware::parse(ram_bytes)
                    .map_err(|error| format!("parse RAM for loader: {error:?}"))?;
                let result = if operation == Operation::RunOneShotChannelDomain {
                    load_mt7921_firmware_through_channel_domain(&mut loader, patch, firmware)
                } else {
                    load_mt7921_firmware(&mut loader, patch, firmware)
                };
                let report =
                    result.map_err(|error| format!("one-shot firmware loader: {error:?}"))?;
                println!("{{\"active_fwdl_report\":\"{report:?}\"}}");
                return Ok(());
            }
            let mut mcu_io = ActiveMcuIo {
                wfdma: &wfdma,
                irq: irq.as_mut().expect("IRQ installed"),
                signal: &signal,
                tx_ring: &mut mcu_tx_ring,
                payload: &mut command_payload,
                wm: ActiveMcuRx {
                    rx_ring: &mut mcu_rx_ring,
                    rx_buffers: &mcu_rx_buffers,
                    rx_tail: 0,
                    rx_head: 7,
                    rx_ring_index: 0,
                    rx_count: 8,
                    irq_bit: WM_RX_IRQ_BIT,
                },
                wm2: None,
            };
            mcu_io.cancelled()?;
            publish_mcu_command(
                mcu_io.wfdma,
                mcu_io.tx_ring,
                mcu_io.payload,
                DownloadCommand::NicPowerControl,
                1,
                0,
            )?;
            mcu_io.wait_tx_consumed(1)?;
            let ready_deadline = Instant::now() + std::time::Duration::from_millis(1000);
            loop {
                mcu_io.cancelled()?;
                let _ = mcu_io.handle_irq(None)?;
                let firmware_state = conn.read(0xe00f0)? & 0x7;
                if firmware_state == 1 {
                    println!("{{\"active_mcu_event\":\"firmware_download_ready\"}}");
                    break;
                }
                if Instant::now() >= ready_deadline {
                    return Err(format!(
                        "boot firmware did not enter download-ready state: {firmware_state}"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let get_result =
                mcu_io.send_patch_semaphore(DownloadCommand::PatchSemaphoreGet, 2, 1)?;
            match get_result {
                1 => println!("{{\"patch_semaphore\":\"patch_already_downloaded\"}}"),
                2 => {
                    let release_result = mcu_io.send_patch_semaphore(
                        DownloadCommand::PatchSemaphoreRelease,
                        3,
                        2,
                    )?;
                    if release_result != 3 {
                        return Err(format!(
                            "patch semaphore release returned {release_result}, expected 3"
                        ));
                    }
                    println!("{{\"patch_semaphore\":\"acquired_and_released\"}}");
                }
                result => {
                    return Err(format!("patch semaphore GET returned failure {result}"));
                }
            }
            Ok(())
        })();

        let mut cleanup_errors = Vec::new();
        if let Err(error) = pcie_mac.write_pcie_mac_interrupt_enable_zero() {
            cleanup_errors.push(error);
        }
        if let Err(error) = wfdma.write_active_wfdma(0xd4204, 0) {
            cleanup_errors.push(error);
        }
        match wfdma.read(0xd4208) {
            Ok(u32::MAX) => cleanup_errors
                .push("WFDMA global configuration returned all ones during cleanup".into()),
            Ok(global) => {
                let disabled =
                    global & !((1 << 0) | (1 << 2) | (1 << 15) | (1 << 21) | (1 << 27) | (1 << 28));
                if let Err(error) = wfdma.write_active_wfdma(0xd4208, disabled) {
                    cleanup_errors.push(error);
                }
            }
            Err(error) => cleanup_errors.push(error),
        }
        let deadline = Instant::now() + std::time::Duration::from_millis(100);
        loop {
            match wfdma.read(0xd4208) {
                Ok(global) if global & 0xa == 0 => break,
                Ok(global) if Instant::now() >= deadline => {
                    cleanup_errors.push(format!("DMA busy during teardown: {global:#010x}"));
                    break;
                }
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(error) => {
                    cleanup_errors.push(error);
                    break;
                }
            }
        }
        if let Err(error) = set_pci_bus_master(&bdf, false) {
            cleanup_errors.push(error);
        }
        if let Some(mut installed) = irq
            && let Err(error) = installed.disable()
        {
            cleanup_errors.push(error);
        }
        let reset = reset_vfio_device(&device);
        if let Err(error) = reset {
            retain_mappings_for_watchdog(&format!(
                "reset while pinned failed: active={active:?} cleanup={cleanup_errors:?} reset={error}"
            ));
        }
        println!("{{\"active_mcu_event\":\"vfio_device_reset_while_pinned\"}}");
        if let Err(error) = verify_pci_dma_disabled(&bdf).and_then(|()| set_lab_safety("SAFE")) {
            retain_mappings_for_watchdog(&format!(
                "post-reset containment verification failed: active={active:?} cleanup={cleanup_errors:?} error={error}"
            ));
        }
        cleanup_errors.extend(attempt_all_cleanup(
            [
                (ActiveArenaKind::Wm2Buffers, &mut mcu_wa_rx_buffers),
                (ActiveArenaKind::Wm2Ring, &mut mcu_wa_rx_ring),
                (ActiveArenaKind::FwdlPayload, &mut fwdl_payload),
                (ActiveArenaKind::CommandPayload, &mut command_payload),
                (ActiveArenaKind::WmBuffers, &mut mcu_rx_buffers),
                (ActiveArenaKind::WmRing, &mut mcu_rx_ring),
                (ActiveArenaKind::RxGuard, &mut rx_guard),
                (ActiveArenaKind::McuTxRing, &mut mcu_tx_ring),
                (ActiveArenaKind::FwdlRing, &mut fwdl_ring),
                (ActiveArenaKind::TxGuard, &mut tx_guard),
            ],
            |(kind, arena)| {
                arena
                    .teardown()
                    .map_err(|error| format!("teardown {kind:?}: {error}"))
            },
        ));
        println!("{{\"active_mcu_event\":\"all_dma_mappings_released_after_reset\"}}");
        active?;
        if !cleanup_errors.is_empty() {
            return Err(format!("active MCU cleanup failed: {cleanup_errors:?}"));
        }
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
            | Operation::PrepareOwnedGlobalTxRings
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

fn verify_pci_dma_disabled(bdf: &str) -> Result<(), String> {
    let path = format!("/sys/bus/pci/devices/{bdf}/config");
    let mut file = File::open(&path).map_err(|error| format!("open PCI config: {error}"))?;
    let mut config = [0u8; 256];
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut config))
        .map_err(|error| format!("read PCI config: {error}"))?;
    let command = u16::from_le_bytes(config[4..6].try_into().expect("fixed field"));
    if command & (1 << 1) == 0 || command & (1 << 2) != 0 {
        return Err(format!(
            "PCI command requires MSE=1 BME=0, read {command:#06x}"
        ));
    }
    let mut capability = usize::from(config[0x34] & !3);
    let mut power_state = None;
    for _ in 0..48 {
        if capability < 0x40 || capability + 6 > config.len() {
            break;
        }
        if config[capability] == 1 {
            power_state = Some(
                u16::from_le_bytes(
                    config[capability + 4..capability + 6]
                        .try_into()
                        .expect("fixed field"),
                ) & 3,
            );
            break;
        }
        capability = usize::from(config[capability + 1] & !3);
    }
    if power_state != Some(0) {
        return Err(format!("PCI device is not in D0: {power_state:?}"));
    }
    Ok(())
}

fn disable_pci_intx(bdf: &str) -> Result<(), String> {
    let path = format!("/sys/bus/pci/devices/{bdf}/config");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("open PCI config for INTx disable: {error}"))?;
    let mut raw = [0u8; 2];
    file.seek(SeekFrom::Start(4))
        .and_then(|_| file.read_exact(&mut raw))
        .map_err(|error| format!("read PCI command for INTx disable: {error}"))?;
    let command = u16::from_le_bytes(raw) | (1 << 10);
    file.seek(SeekFrom::Start(4))
        .and_then(|_| file.write_all(&command.to_le_bytes()))
        .and_then(|_| file.seek(SeekFrom::Start(4)))
        .and_then(|_| file.read_exact(&mut raw))
        .map_err(|error| format!("write PCI INTx disable: {error}"))?;
    let readback = u16::from_le_bytes(raw);
    if readback & (1 << 10) == 0 {
        return Err(format!("PCI INTx disable did not latch: {readback:#06x}"));
    }
    Ok(())
}

fn set_pci_bus_master(bdf: &str, enabled: bool) -> Result<(), String> {
    let path = format!("/sys/bus/pci/devices/{bdf}/config");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("open PCI config for bus mastering: {error}"))?;
    let mut raw = [0u8; 2];
    file.seek(SeekFrom::Start(4))
        .and_then(|_| file.read_exact(&mut raw))
        .map_err(|error| format!("read PCI command for bus mastering: {error}"))?;
    let mut command = u16::from_le_bytes(raw) | (1 << 1) | (1 << 10);
    if enabled {
        command |= 1 << 2;
    } else {
        command &= !(1 << 2);
    }
    file.seek(SeekFrom::Start(4))
        .and_then(|_| file.write_all(&command.to_le_bytes()))
        .and_then(|_| file.seek(SeekFrom::Start(4)))
        .and_then(|_| file.read_exact(&mut raw))
        .map_err(|error| format!("write PCI bus mastering: {error}"))?;
    let readback = u16::from_le_bytes(raw);
    let expected = (1 << 1) | (u16::from(enabled) << 2) | (1 << 10);
    if readback & ((1 << 1) | (1 << 2) | (1 << 10)) != expected {
        return Err(format!(
            "PCI command bus-master transition did not latch: {readback:#06x}"
        ));
    }
    println!(
        "{{\"active_mcu_event\":\"pci_bus_master_{}\",\"command\":\"{readback:#06x}\"}}",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

fn set_lab_safety(value: &str) -> Result<(), String> {
    if !matches!(value, "SAFE" | "MUTATED") {
        return Err("invalid lab safety state".into());
    }
    let path = env::var("DRV_LAB_SAFETY_STATE")
        .map_err(|_| "DRV_LAB_SAFETY_STATE is required for mutating operations")?;
    std::fs::write(&path, format!("{value}\n"))
        .map_err(|error| format!("write lab safety state {path}: {error}"))
}

fn retain_mappings_for_watchdog(message: &str) -> ! {
    eprintln!("mt7921-vfio-read: {message}; retaining device and every IOVA for watchdog reboot");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

fn publish_mcu_command(
    wfdma: &ReadPage,
    tx_ring: &mut DmaArena<'_>,
    payload: &mut DmaArena<'_>,
    command: DownloadCommand,
    sequence: u8,
    descriptor_index: usize,
) -> Result<(), String> {
    let bytes = encode_download_command(command, sequence)
        .map_err(|error| format!("encode MCU command: {error:?}"))?;
    publish_mcu_bytes(wfdma, tx_ring, payload, &bytes, sequence, descriptor_index)
}

fn publish_mcu_bytes(
    wfdma: &ReadPage,
    tx_ring: &mut DmaArena<'_>,
    payload: &mut DmaArena<'_>,
    bytes: &[u8],
    sequence: u8,
    descriptor_index: usize,
) -> Result<(), String> {
    let payload_offset = descriptor_index * 256;
    if payload_offset + bytes.len() > payload.len {
        return Err("MCU command payload arena exhausted".into());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            payload.ptr.as_ptr().add(payload_offset),
            bytes.len(),
        )
    };
    let descriptor = DmaDescriptor::tx(
        DmaSegment {
            iova: payload.iova + payload_offset as u64,
            len: bytes.len() as u16,
        },
        None,
        0,
    )
    .map_err(|error| format!("encode MCU command DMA descriptor: {error:?}"))?;
    tx_ring.write_descriptor_at(descriptor_index, descriptor);
    std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
    wfdma.write_active_wfdma(0xd4418, next_dma_index(descriptor_index, 256) as u32)?;
    println!(
        "{{\"active_mcu_event\":\"command_published\",\"sequence\":{sequence},\"tx_descriptor\":{descriptor_index}}}"
    );
    Ok(())
}

struct ActiveMcuRx<'a, 'b> {
    rx_ring: &'a mut DmaArena<'b>,
    rx_buffers: &'a DmaArena<'b>,
    rx_tail: usize,
    rx_head: usize,
    rx_ring_index: usize,
    rx_count: usize,
    irq_bit: u32,
}

struct ActiveMcuIo<'a, 'b> {
    wfdma: &'a ReadPage,
    irq: &'a mut VfioIrq,
    signal: &'a ActiveSignalGuard,
    tx_ring: &'a mut DmaArena<'b>,
    payload: &'a mut DmaArena<'b>,
    wm: ActiveMcuRx<'a, 'b>,
    wm2: Option<ActiveMcuRx<'a, 'b>>,
}

struct VfioFirmwareLoader<'a, 'b> {
    mcu: ActiveMcuIo<'a, 'b>,
    conn: &'a ReadPage,
    pcie_mac: &'a ReadPage,
    device: &'a File,
    bdf: &'a str,
    fwdl_ring: &'a mut DmaArena<'b>,
    fwdl_payload: &'a mut DmaArena<'b>,
    sequence: u8,
    command_index: usize,
    fwdl_index: usize,
    pending_scatter: Option<(FirmwareImagePart, u8, usize, u32)>,
    start: Instant,
}

struct ReceivedMcuResponse {
    event_id: u8,
    option: u8,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveArenaKind {
    Wm2Buffers,
    Wm2Ring,
    FwdlPayload,
    CommandPayload,
    WmBuffers,
    WmRing,
    RxGuard,
    McuTxRing,
    FwdlRing,
    TxGuard,
}

fn attempt_all_cleanup<I, F>(items: I, mut cleanup: F) -> Vec<String>
where
    I: IntoIterator,
    F: FnMut(I::Item) -> Result<(), String>,
{
    let mut errors = Vec::new();
    for item in items {
        if let Err(error) = cleanup(item) {
            errors.push(error);
        }
    }
    errors
}

const WM_RX_IRQ_BIT: u32 = 1 << 0;
const WM2_RX_IRQ_BIT: u32 = 1 << 22;

fn merge_matching_response(
    matched: &mut Option<ReceivedMcuResponse>,
    candidate: Option<ReceivedMcuResponse>,
) -> Result<(), String> {
    if candidate.is_some() && matched.is_some() {
        return Err("matching MCU sequence appeared on both receive rings".into());
    }
    if matched.is_none() {
        *matched = candidate;
    }
    Ok(())
}

fn response_for_sequence(
    expected_sequence: Option<u8>,
    parsed: mt7921_port_spike::DownloadResponse,
    bytes: Vec<u8>,
) -> Option<ReceivedMcuResponse> {
    (Some(parsed.sequence) == expected_sequence).then_some(ReceivedMcuResponse {
        event_id: parsed.event_id,
        option: parsed.option,
        bytes,
    })
}

const fn rx_irq_acknowledge(status: u32, mask: u32) -> u32 {
    status & mask
}

const fn active_wfdma_write_allowed(offset: usize, value: u32) -> bool {
    match offset {
        0xd4200 => value & !(WM_RX_IRQ_BIT | WM2_RX_IRQ_BIT) == 0,
        0xd4204 => {
            value == 0
                || value == WM_RX_IRQ_BIT
                || value == WM2_RX_IRQ_BIT
                || value == (WM_RX_IRQ_BIT | WM2_RX_IRQ_BIT)
        }
        0xd4208 | 0xd4100 | 0xd42b0 => true,
        0xd42f0 => value == 0,
        0xd4680 => value == 4,
        0xd4690 => value == 0x00c0_0004,
        0xd4640 => value == 0x0340_0004,
        0xd4644 => value == 0x0380_0004,
        0xd4408 => value < 128,
        0xd4418 => value < 256,
        _ => false,
    }
}

fn classify_mcu_completion(
    command: DownloadCommand,
    response: &ReceivedMcuResponse,
) -> Result<FirmwareCommandCompletion, String> {
    match command {
        DownloadCommand::PatchSemaphoreGet | DownloadCommand::PatchSemaphoreRelease => {
            if response.event_id != 0x04 {
                return Err(format!(
                    "patch semaphore response event was {:#04x}, expected 0x04",
                    response.event_id
                ));
            }
            let result = response
                .bytes
                .get(32)
                .copied()
                .ok_or("patch semaphore response omitted result")?;
            Ok(FirmwareCommandCompletion::PatchSemaphore(result.into()))
        }
        DownloadCommand::PatchFinish => {
            let status = response
                .bytes
                .get(32)
                .copied()
                .ok_or("patch finish response omitted status")?;
            Ok(FirmwareCommandCompletion::PatchFinish(status))
        }
        DownloadCommand::PatchStart { .. }
        | DownloadCommand::TargetAddressLength { .. }
        | DownloadCommand::FirmwareStart { .. } => Ok(FirmwareCommandCompletion::Ack),
        DownloadCommand::GetNicCapability => {
            let body = response
                .bytes
                .get(36..)
                .ok_or("NIC capability response omitted MCU header")?;
            let capability = parse_nic_capability(body)
                .map_err(|error| format!("parse NIC capability response: {error:?}"))?;
            Ok(FirmwareCommandCompletion::NicCapability(capability))
        }
        DownloadCommand::ReadEepromBlock { address } => {
            let body = response
                .bytes
                .get(36..)
                .ok_or("EEPROM response omitted MCU header")?;
            let block = parse_eeprom_block(body, address)
                .map_err(|error| format!("parse EEPROM response: {error:?}"))?;
            Ok(FirmwareCommandCompletion::EepromBlock(block))
        }
        DownloadCommand::NicPowerControl => {
            Err("NIC power command unexpectedly requested RX classification".into())
        }
    }
}

fn classify_clc_response(response: &ReceivedMcuResponse) -> Result<ClcSetResponse, String> {
    if response.event_id != 0x80 {
        return Err(format!(
            "SET_CLC response event was {:#04x}, expected 0x80",
            response.event_id
        ));
    }
    if response.option & (1 << 2) != 0 {
        return Err("SET_CLC response was marked as an unsolicited event".into());
    }
    let body = response
        .bytes
        .get(36..)
        .ok_or("SET_CLC response omitted MCU header")?;
    parse_clc_set_response(body).map_err(|error| format!("parse SET_CLC response: {error:?}"))
}

const fn next_dma_index(index: usize, count: usize) -> usize {
    (index + 1) % count
}

const fn dma_index_completed(actual: u32, expected: u32) -> bool {
    actual == expected
}

fn response_wait_timed_out(now: Instant, deadline: Instant) -> bool {
    now >= deadline
}

fn drain_rx_queue(
    wfdma: &ReadPage,
    queue: &mut ActiveMcuRx<'_, '_>,
    expected_sequence: Option<u8>,
) -> Result<Option<ReceivedMcuResponse>, String> {
    let mut matched = None;
    loop {
        let descriptor = queue.rx_ring.read_descriptor_at(queue.rx_tail);
        if !descriptor.is_dma_done() {
            break;
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
        let completed_index = queue.rx_tail;
        let response_len = ((descriptor.ctrl >> 16) & 0x3fff) as usize;
        let parsed = if descriptor.ctrl & (1 << 30) == 0 {
            Err("fragmented MCU RX descriptor is unsupported".into())
        } else if !(36..=2048).contains(&response_len) {
            Err(format!(
                "invalid MCU response descriptor length {response_len}"
            ))
        } else {
            let response = queue
                .rx_buffers
                .read_bytes(completed_index * 2048, response_len)?;
            let actual_sequence = response[29];
            parse_download_response(&response, actual_sequence)
                .map(|parsed| (parsed, response))
                .map_err(|error| format!("parse MCU response: {error:?}"))
        };

        let refill_index = queue.rx_head;
        let refill = DmaDescriptor::rx(DmaSegment {
            iova: queue.rx_buffers.iova + (refill_index * 2048) as u64,
            len: 2048,
        })
        .map_err(|error| format!("rearm MCU RX descriptor: {error:?}"))?;
        queue.rx_ring.write_descriptor_at(refill_index, refill);
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        queue.rx_head = next_dma_index(queue.rx_head, queue.rx_count);
        wfdma.write_rx_cpu_index(queue.rx_ring_index, queue.rx_head as u32)?;
        queue.rx_tail = next_dma_index(queue.rx_tail, queue.rx_count);

        let (parsed, response) = parsed?;
        let candidate = response_for_sequence(expected_sequence, parsed, response);
        if candidate.is_some() {
            if matched.is_some() {
                return Err(format!(
                    "duplicate MCU sequence {} on RX ring {}",
                    parsed.sequence, queue.rx_ring_index
                ));
            }
            println!(
                "{{\"active_mcu_response\":{{\"sequence\":{},\"event_id\":{},\"length\":{},\"rx_ring\":{},\"rx_descriptor\":{completed_index}}}}}",
                parsed.sequence, parsed.event_id, parsed.length, queue.rx_ring_index
            );
            matched = candidate;
        } else {
            println!(
                "{{\"active_mcu_event\":\"unrelated_rx_drained\",\"sequence\":{},\"event_id\":{},\"rx_ring\":{},\"rx_descriptor\":{completed_index}}}",
                parsed.sequence, parsed.event_id, queue.rx_ring_index
            );
        }
    }
    Ok(matched)
}

impl ActiveMcuIo<'_, '_> {
    fn rx_irq_mask(&self) -> u32 {
        self.wm.irq_bit | self.wm2.as_ref().map_or(0, |queue| queue.irq_bit)
    }

    fn cancelled(&self) -> Result<(), String> {
        if self.signal.stop_requested() {
            Err("active MCU transaction cancelled by signal".into())
        } else {
            Ok(())
        }
    }

    fn verify_post_n9_dual_rx(&self) -> Result<(), String> {
        let Some(wm2) = self.wm2.as_ref() else {
            return Err("post-N9 MCU receive requires WM2 ring 4".into());
        };
        if self.wm.rx_ring_index != 0
            || self.wm.irq_bit != WM_RX_IRQ_BIT
            || wm2.rx_ring_index != 4
            || wm2.irq_bit != WM2_RX_IRQ_BIT
            || self.wm.rx_count != 8
            || wm2.rx_count != 8
        {
            return Err("post-N9 MCU dual-ring identity mismatch".into());
        }
        Ok(())
    }

    fn handle_irq(
        &mut self,
        expected_sequence: Option<u8>,
    ) -> Result<Option<ReceivedMcuResponse>, String> {
        let Some(count) = self.irq.try_read()? else {
            return Ok(None);
        };
        self.wfdma.write_active_wfdma(0xd4204, 0)?;
        let interrupt_status = self.wfdma.read(0xd4200)?;
        let irq_mask = self.rx_irq_mask();
        let acknowledged = rx_irq_acknowledge(interrupt_status, irq_mask);
        if acknowledged != 0 {
            self.wfdma.write_active_wfdma(0xd4200, acknowledged)?;
        }
        println!(
            "{{\"active_mcu_event\":\"irq_observed\",\"count\":{count},\"interrupt_status\":\"{interrupt_status:#010x}\"}}"
        );
        let mut matched = drain_rx_queue(self.wfdma, &mut self.wm, expected_sequence)?;
        if let Some(wm2) = self.wm2.as_mut() {
            let wm2_match = drain_rx_queue(self.wfdma, wm2, expected_sequence)?;
            merge_matching_response(&mut matched, wm2_match)?;
        }
        self.wfdma.write_active_wfdma(0xd4204, irq_mask)?;
        Ok(matched)
    }

    fn wait_tx_consumed(&mut self, expected_dma_index: u32) -> Result<(), String> {
        let deadline = Instant::now() + std::time::Duration::from_millis(1000);
        loop {
            self.cancelled()?;
            if self.handle_irq(None)?.is_some() {
                return Err("unexpected patch response while waiting for NIC power".into());
            }
            if self.wfdma.read(0xd441c)? == expected_dma_index {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "MCU TX descriptor was not consumed: expected DIDX {expected_dma_index}"
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn wait_response(
        &mut self,
        sequence: u8,
        deadline: Instant,
    ) -> Result<ReceivedMcuResponse, String> {
        loop {
            self.cancelled()?;
            if let Some(response) = self.handle_irq(Some(sequence))? {
                return Ok(response);
            }
            if response_wait_timed_out(Instant::now(), deadline) {
                return Err(format!("MCU response timed out for sequence {sequence}"));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn send_patch_semaphore(
        &mut self,
        command: DownloadCommand,
        sequence: u8,
        tx_descriptor_index: usize,
    ) -> Result<u8, String> {
        self.cancelled()?;
        self.wfdma.write_active_wfdma(0xd4204, self.rx_irq_mask())?;
        publish_mcu_command(
            self.wfdma,
            self.tx_ring,
            self.payload,
            command,
            sequence,
            tx_descriptor_index,
        )?;
        let deadline = Instant::now() + std::time::Duration::from_millis(3000);
        loop {
            self.cancelled()?;
            if let Some(response) = self.handle_irq(Some(sequence))? {
                if response.event_id != 0x04 {
                    return Err(format!(
                        "unexpected patch semaphore event id {:#04x}",
                        response.event_id
                    ));
                }
                return response
                    .bytes
                    .get(32)
                    .copied()
                    .ok_or("patch semaphore response omitted result".into());
            }
            if Instant::now() >= deadline {
                return Err(format!("patch response timed out for sequence {sequence}"));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

impl FirmwareLoaderTransport for VfioFirmwareLoader<'_, '_> {
    type Error = String;

    fn next_sequence(&mut self) -> u8 {
        self.sequence = (self.sequence + 1) & 0x0f;
        if self.sequence == 0 {
            self.sequence = 1;
        }
        self.sequence
    }

    fn command(
        &mut self,
        command: DownloadCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<FirmwareCommandCompletion, Self::Error> {
        self.mcu.cancelled()?;
        if command == DownloadCommand::GetNicCapability {
            self.mcu.verify_post_n9_dual_rx()?;
            println!(
                "{{\"active_mcu_event\":\"post_n9_dual_rx_verified\",\"rings\":[0,4],\"irq_mask\":\"{:#010x}\"}}",
                self.mcu.rx_irq_mask()
            );
        }
        let descriptor_index = self.command_index;
        let next = next_dma_index(descriptor_index, 256);
        let expects_response = command != DownloadCommand::NicPowerControl;
        if expects_response {
            self.mcu
                .wfdma
                .write_active_wfdma(0xd4204, self.mcu.rx_irq_mask())?;
        }
        publish_mcu_bytes(
            self.mcu.wfdma,
            self.mcu.tx_ring,
            self.mcu.payload,
            encoded,
            sequence,
            descriptor_index,
        )?;
        self.command_index = next;

        let completion = if expects_response {
            let response = self
                .mcu
                .wait_response(sequence, Instant::now() + std::time::Duration::from_secs(3))?;
            classify_mcu_completion(command, &response)?
        } else {
            let deadline = Instant::now() + std::time::Duration::from_secs(1);
            loop {
                self.mcu.cancelled()?;
                if dma_index_completed(self.mcu.wfdma.read(0xd441c)?, next as u32) {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "MCU command TX completion timed out at descriptor {descriptor_index}"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            FirmwareCommandCompletion::NoResponse
        };
        self.mcu
            .tx_ring
            .write_descriptor_at(descriptor_index, DmaDescriptor::reset());
        self.mcu.payload.zero_bytes(PAGE)?;
        Ok(completion)
    }

    fn set_clc(
        &mut self,
        command: &ClcSetCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<Option<ClcSetResponse>, Self::Error> {
        self.mcu.cancelled()?;
        if command.alpha2 != *b"00" || command.environment != 1 || command.index > 1 {
            return Err("SET_CLC escaped the world/indoor allowlist".into());
        }
        if command.expects_response() {
            self.mcu.verify_post_n9_dual_rx()?;
        }
        let descriptor_index = self.command_index;
        let next = next_dma_index(descriptor_index, 256);
        self.mcu
            .wfdma
            .write_active_wfdma(0xd4204, self.mcu.rx_irq_mask())?;
        publish_mcu_bytes(
            self.mcu.wfdma,
            self.mcu.tx_ring,
            self.mcu.payload,
            encoded,
            sequence,
            descriptor_index,
        )?;
        self.command_index = next;
        let response = if command.expects_response() {
            let response = self
                .mcu
                .wait_response(sequence, Instant::now() + std::time::Duration::from_secs(3))?;
            Some(classify_clc_response(&response)?)
        } else {
            let deadline = Instant::now() + std::time::Duration::from_secs(1);
            loop {
                self.mcu.cancelled()?;
                if dma_index_completed(self.mcu.wfdma.read(0xd441c)?, next as u32) {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "SET_CLC TX completion timed out at descriptor {descriptor_index}"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            None
        };
        self.mcu
            .tx_ring
            .write_descriptor_at(descriptor_index, DmaDescriptor::reset());
        self.mcu.payload.zero_bytes(PAGE)?;
        Ok(response)
    }

    fn set_channel_domain(
        &mut self,
        command: &ChannelDomainCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<(), Self::Error> {
        self.mcu.cancelled()?;
        if command.alpha2 != *b"00"
            || !command.indoor
            || command.special_unii_mask != 0
            || command.channels.is_empty()
        {
            return Err("SET_CHAN_DOMAIN escaped the world/indoor mask-zero allowlist".into());
        }
        let descriptor_index = self.command_index;
        let next = next_dma_index(descriptor_index, 256);
        self.mcu
            .wfdma
            .write_active_wfdma(0xd4204, self.mcu.rx_irq_mask())?;
        publish_mcu_bytes(
            self.mcu.wfdma,
            self.mcu.tx_ring,
            self.mcu.payload,
            encoded,
            sequence,
            descriptor_index,
        )?;
        self.command_index = next;
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        loop {
            self.mcu.cancelled()?;
            if dma_index_completed(self.mcu.wfdma.read(0xd441c)?, next as u32) {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "SET_CHAN_DOMAIN TX completion timed out at descriptor {descriptor_index}"
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        self.mcu
            .tx_ring
            .write_descriptor_at(descriptor_index, DmaDescriptor::reset());
        self.mcu.payload.zero_bytes(PAGE)?;
        println!(
            "{{\"active_mcu_event\":\"set_channel_domain_tx_complete\",\"sequence\":{sequence},\"channels\":{}}}",
            command.channels.len()
        );
        Ok(())
    }

    fn publish_scatter(
        &mut self,
        part: FirmwareImagePart,
        sequence: u8,
        chunk: &[u8],
    ) -> Result<(), Self::Error> {
        self.mcu.cancelled()?;
        if self.pending_scatter.is_some() {
            return Err("scatter publication attempted before prior completion".into());
        }
        if chunk.is_empty() || chunk.len() > MT7921_FWDL_CHUNK_BYTES {
            return Err(format!("invalid firmware scatter length {}", chunk.len()));
        }
        self.fwdl_payload.write_bytes(chunk)?;
        let descriptor_index = self.fwdl_index;
        let next = next_dma_index(descriptor_index, 128);
        let descriptor = DmaDescriptor::tx(
            DmaSegment {
                iova: self.fwdl_payload.iova,
                len: chunk.len() as u16,
            },
            None,
            0,
        )
        .map_err(|error| format!("encode FWDL descriptor: {error:?}"))?;
        self.fwdl_ring
            .write_descriptor_at(descriptor_index, descriptor);
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        self.mcu.wfdma.write_active_wfdma(0xd4408, next as u32)?;
        self.fwdl_index = next;
        self.pending_scatter = Some((part, sequence, descriptor_index, next as u32));
        println!(
            r#"{{"active_fwdl_event":"scatter_published","part":"{part:?}","sequence":{sequence},"descriptor":{descriptor_index},"bytes":{}}}"#,
            chunk.len()
        );
        Ok(())
    }

    fn wait_scatter_completion(
        &mut self,
        part: FirmwareImagePart,
        sequence: u8,
        deadline_ms: u64,
    ) -> Result<(), Self::Error> {
        let Some((pending_part, pending_sequence, descriptor_index, expected_didx)) =
            self.pending_scatter
        else {
            return Err("scatter completion requested without publication".into());
        };
        if (pending_part, pending_sequence) != (part, sequence) {
            return Err("scatter completion did not match pending publication".into());
        }
        loop {
            self.mcu.cancelled()?;
            if dma_index_completed(self.mcu.wfdma.read(0xd440c)?, expected_didx) {
                break;
            }
            if self.now_ms() >= deadline_ms {
                return Err(format!(
                    "FWDL completion timed out for {part:?} sequence {sequence}"
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
        self.fwdl_ring
            .write_descriptor_at(descriptor_index, DmaDescriptor::reset());
        self.fwdl_payload.zero_bytes(PAGE)?;
        self.pending_scatter = None;
        println!(
            r#"{{"active_fwdl_event":"scatter_completed","part":"{part:?}","sequence":{sequence},"descriptor":{descriptor_index}}}"#
        );
        Ok(())
    }

    fn firmware_download_state(&mut self) -> Result<u8, Self::Error> {
        self.mcu.cancelled()?;
        Ok((self.conn.read(0xe00f0)? & 0x7) as u8)
    }

    fn firmware_n9_ready(&mut self) -> Result<bool, Self::Error> {
        self.mcu.cancelled()?;
        Ok(self.conn.read(0xe00f0)? & 3 == 3)
    }

    fn now_ms(&self) -> u64 {
        Instant::now().duration_since(self.start).as_millis() as u64
    }

    fn sleep_ms(&mut self, duration_ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(duration_ms));
    }

    fn fail_closed_cleanup(&mut self, state: FirmwareLoaderState) -> Result<(), Self::Error> {
        println!(r#"{{"active_fwdl_event":"cleanup_started","state":"{state:?}"}}"#);
        let mut errors = Vec::new();
        if let Err(error) = self.pcie_mac.write_pcie_mac_interrupt_enable_zero() {
            errors.push(error);
        }
        if let Err(error) = self.mcu.wfdma.write_active_wfdma(0xd4204, 0) {
            errors.push(error);
        }
        match self.mcu.wfdma.read(0xd4208) {
            Ok(global) if global != u32::MAX => {
                let disabled =
                    global & !((1 << 0) | (1 << 2) | (1 << 15) | (1 << 21) | (1 << 27) | (1 << 28));
                if let Err(error) = self.mcu.wfdma.write_active_wfdma(0xd4208, disabled) {
                    errors.push(error);
                }
            }
            Ok(_) => errors.push("WFDMA global config returned all ones during cleanup".into()),
            Err(error) => errors.push(error),
        }
        let deadline = Instant::now() + std::time::Duration::from_millis(100);
        loop {
            match self.mcu.wfdma.read(0xd4208) {
                Ok(global) if global & 0xa == 0 => break,
                Ok(global) if Instant::now() >= deadline => {
                    errors.push(format!("DMA busy during loader cleanup: {global:#010x}"));
                    break;
                }
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(error) => {
                    errors.push(error);
                    break;
                }
            }
        }
        if let Err(error) = set_pci_bus_master(self.bdf, false) {
            errors.push(error);
        }
        if let Err(error) = self.mcu.irq.disable() {
            errors.push(error);
        }
        if errors.is_empty() {
            if let Err(error) = reset_vfio_device(self.device) {
                retain_mappings_for_watchdog(&format!("loader reset while pinned failed: {error}"));
            }
            if let Err(error) =
                verify_pci_dma_disabled(self.bdf).and_then(|()| set_lab_safety("SAFE"))
            {
                retain_mappings_for_watchdog(&format!(
                    "loader post-reset containment verification failed: {error}"
                ));
            }
            println!(r#"{{"active_fwdl_event":"reset_while_pinned"}}"#);
            Ok(())
        } else {
            Err(format!("loader cleanup failed before reset: {errors:?}"))
        }
    }
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
        Self::map_len(iommu, ioas, iova, PAGE)
    }
    fn map_len(iommu: &'a File, ioas: u32, iova: u64, len: usize) -> Result<Self, String> {
        if len == 0 || !len.is_multiple_of(PAGE) || !iova.is_multiple_of(PAGE as u64) {
            return Err("DMA arena length and IOVA must be page aligned".into());
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
    fn initialize_descriptor_page(&mut self) -> Result<(), String> {
        unsafe { std::ptr::write_bytes(self.ptr.as_ptr(), 0, self.len) };
        for offset in (0..self.len).step_by(16) {
            unsafe {
                std::ptr::write_volatile(self.ptr.as_ptr().add(offset + 4).cast::<u32>(), 1 << 31)
            };
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        Ok(())
    }
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.write_bytes_at(0, bytes)
    }
    fn write_bytes_at(&mut self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        if offset
            .checked_add(bytes.len())
            .is_none_or(|end| end > self.len)
        {
            return Err("DMA payload exceeds arena".into());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.ptr.as_ptr().add(offset),
                bytes.len(),
            )
        };
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
        self.write_descriptor_at(0, descriptor)
    }
    fn write_descriptor_at(&mut self, index: usize, descriptor: DmaDescriptor) {
        let offset = index * 16;
        assert!(offset + 16 <= self.len);
        for (index, word) in [
            descriptor.buf0,
            descriptor.ctrl,
            descriptor.buf1,
            descriptor.info,
        ]
        .into_iter()
        .enumerate()
        {
            unsafe {
                std::ptr::write_volatile(
                    self.ptr.as_ptr().add(offset).cast::<u32>().add(index),
                    word,
                )
            };
        }
    }
    fn read_descriptor(&self) -> DmaDescriptor {
        self.read_descriptor_at(0)
    }
    fn read_descriptor_at(&self, descriptor_index: usize) -> DmaDescriptor {
        let offset = descriptor_index * 16;
        assert!(offset + 16 <= self.len);
        let word = |index| unsafe {
            std::ptr::read_volatile(self.ptr.as_ptr().add(offset).cast::<u32>().add(index))
        };
        DmaDescriptor {
            buf0: word(0),
            ctrl: word(1),
            buf1: word(2),
            info: word(3),
        }
    }
    fn read_bytes(&self, offset: usize, length: usize) -> Result<Vec<u8>, String> {
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= self.len)
            .ok_or("DMA read escaped arena")?;
        let mut bytes = vec![0; length];
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.ptr.as_ptr().add(offset),
                bytes.as_mut_ptr(),
                end - offset,
            )
        };
        Ok(bytes)
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
        if !bar_page.is_multiple_of(PAGE) || bar_page + PAGE > region.size as usize {
            return Err("allowlisted BAR page is outside BAR 0".into());
        }
        validate_region_mapping(region, writable)?;
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
    fn read_dynamic_window(&self, offset: usize) -> Result<u32, String> {
        if self.bar_page != MT_HIF_REMAP_WINDOW_BAR_OFFSET
            || offset < self.bar_page
            || offset + 4 > self.bar_page + PAGE
        {
            return Err("dynamic window read escaped immutable page".into());
        }
        self.read(offset)
    }
    fn write_dynamic_window(&self, offset: usize, value: u32) -> Result<(), String> {
        if self.bar_page != MT_HIF_REMAP_WINDOW_BAR_OFFSET
            || offset != MT_HIF_REMAP_WINDOW_BAR_OFFSET + 0x140
        {
            return Err("WFSYS reset write escaped immutable target".into());
        }
        let within = offset - self.bar_page;
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        Ok(())
    }
    fn write_pcie_mac_interrupt_enable_zero(&self) -> Result<(), String> {
        self.write_pcie_mac_interrupt_enable(0)
    }
    fn write_pcie_mac_interrupt_enable(&self, value: u32) -> Result<(), String> {
        if self.bar_page != 0x10000 {
            return Err("PCIe MAC interrupt write escaped immutable allowlist".into());
        }
        if value != 0 && value != 0xff {
            return Err("PCIe MAC interrupt value escaped allowlist".into());
        }
        let within = 0x10188 - self.bar_page;
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        Ok(())
    }
    fn disable_pcie_l0s(&self) -> Result<(), String> {
        if self.bar_page != 0x10000 {
            return Err("PCIe PM write escaped immutable allowlist".into());
        }
        let offset = 0x10194;
        let raw = self.read(offset)?;
        if raw == u32::MAX {
            return Err("PCIe PM returned all ones".into());
        }
        let value = raw | (1 << 8);
        let within = offset - self.bar_page;
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        if self.read(offset)? & (1 << 8) == 0 {
            return Err("PCIe L0s disable did not latch".into());
        }
        Ok(())
    }
    fn write_swdef_normal(&self) -> Result<(), String> {
        if self.bar_page != 0x9f000 {
            return Err("SWDEF write escaped immutable allowlist".into());
        }
        let within = 0x9f23c - self.bar_page;
        if self.read(0x9f23c)? == u32::MAX {
            return Err("SWDEF mode returned all ones".into());
        }
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), 0) };
        if self.read(0x9f23c)? != 0 {
            return Err("SWDEF normal mode did not latch".into());
        }
        Ok(())
    }
    fn enable_dmashdl_bypass(&self) -> Result<(), String> {
        if self.bar_page != 0xd6000 {
            return Err("DMASHDL write escaped immutable allowlist".into());
        }
        let offset = 0xd6004;
        let raw = self.read(offset)?;
        if raw == u32::MAX {
            return Err("DMASHDL control returned all ones".into());
        }
        let value = raw | (1 << 28);
        let within = offset - self.bar_page;
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
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
    fn write_tx_ring_slot(
        &self,
        index: usize,
        descriptor_base: u32,
        descriptor_count: u32,
        cpu_index: u32,
    ) -> Result<(), String> {
        if self.bar_page != 0xd4000 || index >= 18 {
            return Err("global TX ring write escaped slot allowlist".into());
        }
        for (word, value) in [descriptor_base, descriptor_count, cpu_index]
            .into_iter()
            .enumerate()
        {
            let within = 0x300 + index * 0x10 + word * 4;
            unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        }
        Ok(())
    }
    fn reset_all_tx_indices(&self, value: u32) -> Result<(), String> {
        if self.bar_page != 0xd4000 || value != u32::MAX {
            return Err("DTX reset escaped all-rings-only allowlist".into());
        }
        let within = 0xd420c - self.bar_page;
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
    fn write_rx_ring_slot(
        &self,
        index: usize,
        descriptor_base: u32,
        descriptor_count: u32,
        cpu_index: u32,
        dma_index: u32,
    ) -> Result<(), String> {
        if self.bar_page != 0xd4000 || index >= 8 {
            return Err("global RX ring write escaped slot allowlist".into());
        }
        for (word, value) in [descriptor_base, descriptor_count, cpu_index, dma_index]
            .into_iter()
            .enumerate()
        {
            let within = 0x500 + index * 0x10 + word * 4;
            unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        }
        Ok(())
    }
    fn write_rx_cpu_index(&self, index: usize, value: u32) -> Result<(), String> {
        if self.bar_page != 0xd4000 || index >= 8 || value >= 8 {
            return Err("RX producer write escaped slot allowlist".into());
        }
        let within = 0x500 + index * 0x10 + 8;
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        Ok(())
    }
    fn write_active_wfdma(&self, offset: usize, value: u32) -> Result<(), String> {
        if self.bar_page != 0xd4000 {
            return Err("active WFDMA write escaped BAR page".into());
        }
        if !active_wfdma_write_allowed(offset, value) {
            return Err(format!(
                "active WFDMA write {offset:#x}={value:#x} escaped allowlist"
            ));
        }
        let within = offset - self.bar_page;
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        Ok(())
    }
}

fn validate_region_mapping(region: &RegionInfo, writable: bool) -> Result<(), String> {
    let required = VFIO_REGION_INFO_FLAG_READ
        | VFIO_REGION_INFO_FLAG_MMAP
        | if writable {
            VFIO_REGION_INFO_FLAG_WRITE
        } else {
            0
        };
    if region.flags & required != required {
        return Err(format!(
            "BAR region flags {:#010x} do not permit {} mmap (required {required:#010x})",
            region.flags,
            if writable { "writable" } else { "read-only" }
        ));
    }
    Ok(())
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

fn verify_vfio_reset_supported(device: &File) -> Result<(), String> {
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

fn reset_vfio_device(device: &File) -> Result<(), String> {
    verify_vfio_reset_supported(device)?;
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
    PrepareOwnedGlobalTxRings,
    QueryPatchSemaphore,
    RunOneShotFirmware,
    RunOneShotChannelDomain,
}

impl Operation {
    fn wfdma_writable(self) -> bool {
        matches!(
            self,
            Self::ProgramDisabledFwdlRing
                | Self::MaskAckDisabledFwdl
                | Self::PrepareOwnedGlobalTxRings
                | Self::QueryPatchSemaphore
                | Self::RunOneShotFirmware
                | Self::RunOneShotChannelDomain
        )
    }

    fn conn_writable(self) -> bool {
        matches!(
            self,
            Self::AcquireDriverOwnership
                | Self::QueryPatchSemaphore
                | Self::RunOneShotFirmware
                | Self::RunOneShotChannelDomain
        )
    }
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
        if low != 0x7001 && low != 0x1800 && low != 0x1806 && Some(value) != self.saved.get() {
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

struct VfioWfsysReset<'a> {
    selector: &'a ReadPage,
    window: &'a ReadPage,
    start: Instant,
    saved: u32,
}
impl VfioWfsysReset<'_> {
    fn select(&self) -> Result<(), String> {
        self.selector
            .write_remap_selector((self.saved & !0xffff) | 0x1800)?;
        let raw = self.selector.read(MT_HIF_REMAP_L1_BAR_OFFSET)?;
        if raw & 0xffff != 0x1800 {
            return Err(format!("WFSYS selector did not latch: {raw:#010x}"));
        }
        Ok(())
    }
    fn restore(&self) -> Result<(), String> {
        self.selector.write_remap_selector(self.saved)
    }
}
impl WfsysResetTransport for VfioWfsysReset<'_> {
    type Error = String;
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    fn read_reset_control(&mut self) -> Result<u32, Self::Error> {
        self.select()?;
        let raw = self
            .window
            .read_dynamic_window(MT_HIF_REMAP_WINDOW_BAR_OFFSET + 0x140)?;
        if raw == u32::MAX {
            return Err("WFSYS reset control returned all ones".into());
        }
        Ok(raw)
    }
    fn write_reset_control(&mut self, value: u32) -> Result<(), Self::Error> {
        self.select()?;
        self.window
            .write_dynamic_window(MT_HIF_REMAP_WINDOW_BAR_OFFSET + 0x140, value)
    }
    fn sleep_ms(&mut self, milliseconds: u64) {
        std::thread::sleep(std::time::Duration::from_millis(milliseconds));
    }
}

fn log_wfsys_reset_event(event: WfsysResetEvent) {
    println!("{{\"wfsys_reset_event\":\"{event:?}\"}}")
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

struct VfioGlobalTxRings<'a> {
    page: &'a ReadPage,
}
impl GlobalTxRingTransport for VfioGlobalTxRings<'_> {
    type Error = String;
    fn read_global_config(&mut self) -> Result<u32, Self::Error> {
        self.page.read(0xd4208)
    }
    fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
        self.page.read(0xd4204)
    }
    fn read_tx_ring(&mut self, index: usize) -> Result<TxRingState, Self::Error> {
        if index >= 18 {
            return Err("TX ring read escaped slot allowlist".into());
        }
        let base = 0xd4300 + index * 0x10;
        Ok(TxRingState {
            descriptor_base: self.page.read(base)?,
            descriptor_count: self.page.read(base + 4)?,
            cpu_index: self.page.read(base + 8)?,
            dma_index: self.page.read(base + 12)?,
        })
    }
    fn write_tx_ring(
        &mut self,
        index: usize,
        descriptor_base: u32,
        descriptor_count: u32,
        cpu_index: u32,
    ) -> Result<(), Self::Error> {
        self.page
            .write_tx_ring_slot(index, descriptor_base, descriptor_count, cpu_index)
    }
    fn reset_tx_indices(&mut self, value: u32) -> Result<(), Self::Error> {
        self.page.reset_all_tx_indices(value)
    }
}

fn log_global_tx_ring_event(event: GlobalTxRingEvent) {
    println!("{{\"global_tx_ring_event\":\"{event:?}\"}}")
}

struct VfioGlobalRxRings<'a> {
    page: &'a ReadPage,
}
impl DisabledMcuRxTransport for VfioGlobalRxRings<'_> {
    type Error = String;
    fn read_global_config(&mut self) -> Result<u32, Self::Error> {
        self.page.read(0xd4208)
    }
    fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
        self.page.read(0xd4204)
    }
    fn read_registers(&mut self) -> Result<McuRxRegisters, Self::Error> {
        self.read_registers_at(0)
    }
    fn read_registers_at(&mut self, index: usize) -> Result<McuRxRegisters, Self::Error> {
        if index >= 8 {
            return Err("RX ring read escaped slot allowlist".into());
        }
        let base = 0xd4500 + index * 0x10;
        Ok(McuRxRegisters {
            descriptor_base: self.page.read(base)?,
            descriptor_count: self.page.read(base + 4)?,
            cpu_index: self.page.read(base + 8)?,
            dma_index: self.page.read(base + 12)?,
        })
    }
    fn write_initial(
        &mut self,
        descriptor_base: u32,
        descriptor_count: u32,
    ) -> Result<(), Self::Error> {
        self.write_ring_initial(0, descriptor_base, descriptor_count)
    }
    fn publish_cpu_index(&mut self, cpu_index: u32) -> Result<(), Self::Error> {
        self.publish_ring_cpu_index(0, cpu_index)
    }
    fn write_ring_initial(
        &mut self,
        index: usize,
        descriptor_base: u32,
        descriptor_count: u32,
    ) -> Result<(), Self::Error> {
        self.page
            .write_rx_ring_slot(index, descriptor_base, descriptor_count, 0, 0)
    }
    fn publish_ring_cpu_index(&mut self, index: usize, cpu_index: u32) -> Result<(), Self::Error> {
        let state = self.read_registers_at(index)?;
        self.page.write_rx_ring_slot(
            index,
            state.descriptor_base,
            state.descriptor_count,
            cpu_index,
            state.dma_index,
        )
    }
    fn release_fence(&mut self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release)
    }
}

fn log_global_rx_ring_event(event: DisabledMcuRxEvent) {
    println!("{{\"global_rx_ring_event\":\"{event:?}\"}}")
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
    decompress_verified_image(PATCH_PATH, PATCH_SHA256, PATCH_IMAGE_BYTES)
}

fn decompress_ram() -> Result<Vec<u8>, String> {
    decompress_verified_image(RAM_PATH, RAM_SHA256, RAM_IMAGE_BYTES)
}

fn decompress_verified_image(
    path: &str,
    expected_sha256: &str,
    expected_len: usize,
) -> Result<Vec<u8>, String> {
    let output = Command::new("/run/current-system/sw/bin/zstdcat")
        .arg(path)
        .output()
        .map_err(|error| format!("run zstdcat for {path}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "zstdcat {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if output.stdout.len() != expected_len {
        return Err(format!(
            "decompressed {path} is {} bytes, expected {expected_len}",
            output.stdout.len()
        ));
    }
    let mut hash = Command::new("/run/current-system/sw/bin/sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("start sha256sum for {path}: {error}"))?;
    hash.stdin
        .take()
        .ok_or("sha256sum stdin unavailable")?
        .write_all(&output.stdout)
        .map_err(|error| format!("hash {path}: {error}"))?;
    let hash = hash
        .wait_with_output()
        .map_err(|error| format!("wait for sha256sum {path}: {error}"))?;
    if !hash.status.success() {
        return Err(format!("sha256sum failed for {path}"));
    }
    let actual = String::from_utf8_lossy(&hash.stdout);
    if actual.split_whitespace().next() != Some(expected_sha256) {
        return Err(format!(
            "decompressed {path} SHA-256 mismatch: {}",
            actual.trim()
        ));
    }
    println!(
        "{{\"firmware_image_verified\":{{\"path\":\"{path}\",\"bytes\":{expected_len},\"sha256\":\"{expected_sha256}\"}}}}"
    );
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

    #[test]
    fn dual_rx_correlates_interleaved_responses_and_wraps_independently() {
        let envelope = |sequence, event_id| mt7921_port_spike::DownloadResponse {
            length: 12,
            packet_type: 0xe000,
            event_id,
            sequence,
            option: 0,
            extended_event_id: 0,
        };
        let unrelated_wm = response_for_sequence(Some(7), envelope(3, 1), vec![3]);
        let matching_wm2 = response_for_sequence(Some(7), envelope(7, 0x80), vec![7]);
        assert!(unrelated_wm.is_none());
        let mut matched = None;
        merge_matching_response(&mut matched, unrelated_wm).unwrap();
        merge_matching_response(&mut matched, matching_wm2).unwrap();
        assert_eq!(matched.unwrap().event_id, 0x80);

        let mut duplicate = Some(ReceivedMcuResponse {
            event_id: 1,
            option: 0,
            bytes: vec![],
        });
        assert!(
            merge_matching_response(
                &mut duplicate,
                Some(ReceivedMcuResponse {
                    event_id: 0x80,
                    option: 0,
                    bytes: vec![],
                })
            )
            .is_err()
        );

        let mut wm_head = 7;
        let mut wm_tail = 6;
        let mut wm2_head = 3;
        let mut wm2_tail = 7;
        wm_head = next_dma_index(wm_head, 8);
        wm_tail = next_dma_index(wm_tail, 8);
        wm2_head = next_dma_index(wm2_head, 8);
        wm2_tail = next_dma_index(wm2_tail, 8);
        assert_eq!((wm_head, wm_tail), (0, 7));
        assert_eq!((wm2_head, wm2_tail), (4, 0));
    }

    #[test]
    fn dual_rx_irq_ack_and_wait_deadline_are_bounded() {
        let mask = WM_RX_IRQ_BIT | WM2_RX_IRQ_BIT;
        assert_eq!(rx_irq_acknowledge(mask | (1 << 27), mask), mask);
        assert_eq!(rx_irq_acknowledge(1 << 27, mask), 0);
        assert!(active_wfdma_write_allowed(0xd4200, mask));
        assert!(active_wfdma_write_allowed(0xd4204, mask));
        assert!(active_wfdma_write_allowed(0xd4204, WM2_RX_IRQ_BIT));
        assert!(!active_wfdma_write_allowed(0xd4200, 1 << 27));
        assert!(!active_wfdma_write_allowed(0xd4204, 1 << 27));
        let deadline = Instant::now() + std::time::Duration::from_millis(10);
        assert!(!response_wait_timed_out(
            deadline - std::time::Duration::from_nanos(1),
            deadline
        ));
        assert!(response_wait_timed_out(deadline, deadline));
    }

    #[test]
    fn dual_rx_cleanup_attempts_both_rings_after_injected_failures() {
        let items = [
            ActiveArenaKind::Wm2Buffers,
            ActiveArenaKind::Wm2Ring,
            ActiveArenaKind::WmBuffers,
            ActiveArenaKind::WmRing,
        ];
        let mut attempted = Vec::new();
        let errors = attempt_all_cleanup(items, |kind| {
            attempted.push(kind);
            if matches!(kind, ActiveArenaKind::Wm2Buffers | ActiveArenaKind::WmRing) {
                Err(format!("injected {kind:?}"))
            } else {
                Ok(())
            }
        });
        assert_eq!(attempted, items);
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn every_wfdma_writer_requires_a_writable_mapping() {
        assert!(Operation::ProgramDisabledFwdlRing.wfdma_writable());
        assert!(Operation::MaskAckDisabledFwdl.wfdma_writable());
        assert!(Operation::PrepareOwnedGlobalTxRings.wfdma_writable());
        assert!(Operation::QueryPatchSemaphore.wfdma_writable());
        assert!(Operation::RunOneShotFirmware.wfdma_writable());
        assert!(Operation::RunOneShotChannelDomain.wfdma_writable());
        assert!(!Operation::ReadFixed.wfdma_writable());
        assert!(!Operation::AcquireDriverOwnership.wfdma_writable());
        assert!(!Operation::InventoryVfioIrqs.wfdma_writable());
        assert!(Operation::AcquireDriverOwnership.conn_writable());
        assert!(Operation::QueryPatchSemaphore.conn_writable());
        assert!(Operation::RunOneShotFirmware.conn_writable());
        assert!(Operation::RunOneShotChannelDomain.conn_writable());
        assert!(!Operation::ReadFixed.conn_writable());
        assert!(!Operation::PrepareOwnedGlobalTxRings.conn_writable());
    }

    #[test]
    fn active_backend_classifies_responses_and_modular_completion_fail_closed() {
        let mut bytes = vec![0; 33];
        bytes[32] = 2;
        let response = ReceivedMcuResponse {
            event_id: 0x04,
            option: 0,
            bytes,
        };
        assert_eq!(
            classify_mcu_completion(DownloadCommand::PatchSemaphoreGet, &response),
            Ok(FirmwareCommandCompletion::PatchSemaphore(
                mt7921_port_spike::PatchSemaphoreStatus::Acquired
            ))
        );
        let wrong_event = ReceivedMcuResponse {
            event_id: 3,
            option: 0,
            bytes: response.bytes.clone(),
        };
        assert!(classify_mcu_completion(DownloadCommand::PatchSemaphoreGet, &wrong_event).is_err());
        let truncated = ReceivedMcuResponse {
            event_id: 4,
            option: 0,
            bytes: vec![0; 32],
        };
        assert!(classify_mcu_completion(DownloadCommand::PatchSemaphoreGet, &truncated).is_err());
        assert_eq!(next_dma_index(127, 128), 0);
        assert!(dma_index_completed(0, 0));
        assert!(!dma_index_completed(127, 0));

        let capability_response = ReceivedMcuResponse {
            event_id: 1,
            option: 0,
            bytes: vec![0; 40],
        };
        assert_eq!(
            classify_mcu_completion(DownloadCommand::GetNicCapability, &capability_response),
            Ok(FirmwareCommandCompletion::NicCapability(
                mt7921_port_spike::NicCapability {
                    element_count: 0,
                    mac_address: None,
                    phy: None,
                    has_6ghz: None,
                    chip_capability: None,
                    unknown_elements: 0,
                }
            ))
        );
        assert!(
            classify_mcu_completion(
                DownloadCommand::GetNicCapability,
                &ReceivedMcuResponse {
                    event_id: 1,
                    option: 0,
                    bytes: vec![0; 39],
                }
            )
            .is_err()
        );

        let mut eeprom_bytes = vec![0; 60];
        eeprom_bytes[36..40].copy_from_slice(&0x550u32.to_le_bytes());
        eeprom_bytes[40..44].copy_from_slice(&1u32.to_le_bytes());
        eeprom_bytes[55] = 1;
        assert_eq!(
            classify_mcu_completion(
                DownloadCommand::ReadEepromBlock { address: 0x550 },
                &ReceivedMcuResponse {
                    event_id: 1,
                    option: 0,
                    bytes: eeprom_bytes,
                }
            ),
            Ok(FirmwareCommandCompletion::EepromBlock(
                mt7921_port_spike::EepromBlock {
                    address: 0x550,
                    valid: 1,
                    data: {
                        let mut data = [0; 16];
                        data[11] = 1;
                        data
                    },
                }
            ))
        );

        let mut clc_bytes = vec![0; 108];
        clc_bytes[42..44].copy_from_slice(&68u16.to_le_bytes());
        clc_bytes[44] = 0x1f;
        let clc = ReceivedMcuResponse {
            event_id: 0x80,
            option: 0,
            bytes: clc_bytes.clone(),
        };
        assert_eq!(
            classify_clc_response(&clc),
            Ok(ClcSetResponse {
                tag: 0,
                length: 68,
                special_unii_mask: 0x1f,
            })
        );
        assert!(
            classify_clc_response(&ReceivedMcuResponse {
                event_id: 0x80,
                option: 1 << 2,
                bytes: clc_bytes,
            })
            .is_err()
        );
    }

    #[test]
    fn vfio_region_capabilities_fail_closed_before_mmap() {
        let mut region = RegionInfo {
            flags: VFIO_REGION_INFO_FLAG_READ | VFIO_REGION_INFO_FLAG_MMAP,
            ..Default::default()
        };
        assert!(validate_region_mapping(&region, false).is_ok());
        assert!(validate_region_mapping(&region, true).is_err());

        region.flags |= VFIO_REGION_INFO_FLAG_WRITE;
        assert!(validate_region_mapping(&region, true).is_ok());

        region.flags &= !VFIO_REGION_INFO_FLAG_MMAP;
        assert!(validate_region_mapping(&region, false).is_err());
        assert!(validate_region_mapping(&region, true).is_err());
    }
}
