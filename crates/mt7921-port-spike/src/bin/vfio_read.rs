//! Strictly read-only no-plastic MT7921 VFIO inventory.
#![cfg(target_os = "linux")]
#![allow(unexpected_cfgs)]

#[cfg(feature = "fuchsia-passive")]
use fidl_fuchsia_wlan_common as fidl_common;
#[cfg(feature = "fuchsia-passive")]
use fidl_fuchsia_wlan_driver as fidl_driver;
#[cfg(feature = "fuchsia-passive")]
use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
#[cfg(feature = "fuchsia-passive")]
use fidl_fuchsia_wlan_softmac as fidl_softmac;
#[cfg(feature = "fuchsia-passive")]
use fuchsia_softmac_port::{
    BeaconHintAuthorizer, ChannelBandwidth, ChannelNumber, ConservativeRegulatoryPolicy,
    HardwareScanEvent, MlmeScanEvent, PassiveScanner, SaeHandshake, SaeHandshakeUpdate,
    ScanRequest, ScanResultCode, ScanTypes, SoftmacHardware, WlanBand,
    WlanSoftmacBaseSetChannelRequest, WlanSoftmacBaseStartPassiveScanRequest,
    allowed_passive_channels, build_sae_auth_frame,
};
#[cfg(feature = "fuchsia-passive")]
use ieee80211::MacAddrBytes as _;
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
    MT7921_FWDL_RING_BYTES, McuRxRegisters, Mt7921TxFree, Mt7921TxStatus, OwnershipError,
    OwnershipEvent, OwnershipTransport, PCIE_LPCR_HOST_CLR_OWN, Patch, PciIrqCapability,
    PciIrqKind, ReadOnlyStatus, ReadRegister, TopOwnershipError, TopOwnershipEvent,
    TopOwnershipTransport, TxRingState, WfsysResetEvent, WfsysResetTransport,
    acquire_driver_ownership, acquire_top_driver_ownership, encode_download_command,
    encode_mt7921_5ghz_auth_tx, load_mt7921_firmware, load_mt7921_firmware_through_channel_domain,
    mask_ack_disabled_fwdl_interrupt, mt7921_packet_type, parse_clc_set_response,
    parse_download_response, parse_eeprom_block, parse_mt7921_tx_free, parse_mt7921_tx_status,
    parse_nic_capability, prepare_global_rx_rings, prepare_global_tx_rings, prepare_mcu_rx_ring,
    program_disabled_fwdl_ring, read_dynamic_identity_status, reset_wfsys, select_vfio_irq,
    stage_disabled_firmware_chunk,
};
#[cfg(feature = "fuchsia-passive")]
use mt7921_port_spike::{
    ConservativePowerLimits, PassiveMacMmioOperation, PassiveMcuCommand, RateTxPowerAuthorizer,
    RateTxPowerTransport, candidate_channels, encode_pse_reg_read_command,
    load_mt7921_firmware_with_passive_boundary, parse_passive_advertisement,
    parse_passive_scan_done, parse_pse_reg_read_response, passive_mac_bar_offset,
    passive_mac_mmio_plan, passive_mac_source_rmw_value, validate_passive_mac_bar_read,
};
#[cfg(feature = "fuchsia-passive")]
use mt7921_softmac_adapter::client_device::{
    ClientRxFrame, ClientSupport, Mt7921ClientDevice, Mt7921ClientEffects,
};
#[cfg(feature = "fuchsia-passive")]
use mt7921_softmac_adapter::{
    Mt7921SoftmacAdapter, PassiveMechanicsEvent, PassivePrerequisites, SourceExactPassiveMechanics,
    SourceExactPassiveTransport, query_from_capabilities,
};
use std::{
    cell::Cell,
    env,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    num::NonZeroU64,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    process::{Command, Stdio},
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
#[cfg(feature = "fuchsia-passive")]
use wlan_mlme::device::DeviceOps;

const VFIO_TYPE: u64 = b';' as u64;
const VFIO_BASE: u64 = 100;
const VFIO_DEVICE_GET_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 7);
const VFIO_DEVICE_GET_REGION_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 8);
const VFIO_DEVICE_GET_IRQ_INFO: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 9);
const VFIO_DEVICE_SET_IRQS: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 10);
const VFIO_DEVICE_RESET: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 11);
const VFIO_DEVICE_BIND_IOMMUFD: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 18);
const VFIO_DEVICE_ATTACH_IOMMUFD_PT: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 19);
const VFIO_DEVICE_DETACH_IOMMUFD_PT: u64 = (VFIO_TYPE << 8) | (VFIO_BASE + 20);
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
const MCU_TX_RING_COUNT: usize = 256;
const MCU_COMMAND_SLOT_BYTES: usize = 256;
const MCU_COMMAND_PAYLOAD_BYTES: usize = MCU_TX_RING_COUNT * MCU_COMMAND_SLOT_BYTES;
const MCU_COMMAND_PAYLOAD_IOVA: u64 = 0x0102_0000;
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
const WATCHDOG_STATUS_PATH: &str = "/run/current-system/sw/bin/wifi-lab-watchdog";
const SAE_STAGE_PATH: &str = "/var/lib/wifi-driver-lab/sae-stage";

#[cfg(feature = "fuchsia-passive")]
fn record_sae_stage(event: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(SAE_STAGE_PATH)
    {
        let _ = writeln!(file, "{event}");
        let _ = file.sync_all();
    }
    eprintln!(r#"{{"sae_auth_event":"{event}"}}"#);
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
struct Detach {
    argsz: u32,
    flags: u32,
    pasid: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
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
    #[link_name = "write"]
    fn write_fd(fd: i32, buffer: *const u8, count: usize) -> isize;
    fn pause() -> i32;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AcquisitionIntent {
    BindIommu,
    AllocateIoas,
    AttachIoas,
    MapBar(usize),
    MapDma { iova: u64, len: usize },
    InstallIrq,
}

struct AcquisitionLedger {
    intents: [Option<AcquisitionIntent>; 64],
    len: usize,
}

impl Default for AcquisitionLedger {
    fn default() -> Self {
        Self {
            intents: [None; 64],
            len: 0,
        }
    }
}

impl AcquisitionLedger {
    fn record(&mut self, intent: AcquisitionIntent) -> Result<(), String> {
        let slot = self
            .intents
            .get_mut(self.len)
            .ok_or_else(|| "active acquisition ledger capacity exhausted".to_string())?;
        *slot = Some(intent);
        self.len += 1;
        Ok(())
    }

    fn recorded(&self) -> impl Iterator<Item = AcquisitionIntent> + '_ {
        self.intents[..self.len].iter().copied().flatten()
    }
}

#[derive(Default)]
struct ActiveVfioResources {
    selector_page: Option<ReadPage>,
    dynamic_window: Option<ReadPage>,
    #[cfg(feature = "fuchsia-passive")]
    passive_window_pages: [Option<ReadPage>; PASSIVE_MAC_BAR_PAGES.len()],
    swdef: Option<ReadPage>,
    dmashdl: Option<ReadPage>,
    tx_guard: Option<DmaArena>,
    fwdl_ring: Option<DmaArena>,
    mcu_tx_ring: Option<DmaArena>,
    rx_guard: Option<DmaArena>,
    mcu_rx_ring: Option<DmaArena>,
    mcu_rx_buffers: Option<DmaArena>,
    command_payload: Option<DmaArena>,
    fwdl_payload: Option<DmaArena>,
    mcu_wa_rx_ring: Option<DmaArena>,
    mcu_wa_rx_buffers: Option<DmaArena>,
    #[cfg(feature = "fuchsia-passive")]
    data_rx_ring: Option<DmaArena>,
    #[cfg(feature = "fuchsia-passive")]
    data_rx_buffers: Option<DmaArena>,
    #[cfg(feature = "fuchsia-passive")]
    mgmt_txwi: Option<DmaArena>,
    #[cfg(feature = "fuchsia-passive")]
    mgmt_frame: Option<DmaArena>,
    #[cfg(feature = "fuchsia-passive")]
    mgmt_tx_ring: Option<DmaArena>,
    irq: Option<VfioIrq>,
}

struct ActiveVfioCapsule {
    device: Arc<File>,
    iommu: Arc<File>,
    ioas: Option<Ioas>,
    ioas_attached: bool,
    wfdma: Option<ReadPage>,
    pcie_mac: Option<ReadPage>,
    conn: Option<ReadPage>,
    active: Option<ActiveVfioResources>,
    containment: Option<ContainmentLedger>,
    acquisition: AcquisitionLedger,
    #[cfg(test)]
    drop_probe: Option<CapsuleDropProbe>,
}

#[cfg(test)]
struct CapsuleDropProbe(std::path::PathBuf);

#[cfg(test)]
impl Drop for CapsuleDropProbe {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"dropped");
    }
}

impl ActiveVfioCapsule {
    fn new(device: Arc<File>, iommu: Arc<File>, containment: Option<ContainmentLedger>) -> Self {
        Self {
            device,
            iommu,
            ioas: None,
            ioas_attached: false,
            wfdma: None,
            pcie_mac: None,
            conn: None,
            active: None,
            containment,
            acquisition: AcquisitionLedger::default(),
            #[cfg(test)]
            drop_probe: None,
        }
    }

    fn release_observable(&mut self) -> Vec<ReleaseFailure> {
        fn release_dma(slot: &mut Option<DmaArena>, failures: &mut Vec<ReleaseFailure>) {
            if let Some(arena) = slot.as_mut()
                && let Err(error) = arena.teardown()
            {
                failures.push(ReleaseFailure {
                    action: ObservableRelease::DmaUnmap,
                    error,
                });
            }
        }
        fn release_bar(slot: &mut Option<ReadPage>, failures: &mut Vec<ReleaseFailure>) {
            if let Some(page) = slot.as_mut()
                && let Err(error) = page.teardown()
            {
                failures.push(ReleaseFailure {
                    action: ObservableRelease::BarMunmap,
                    error,
                });
            }
        }

        let mut failures = Vec::new();
        if let Some(active) = self.active.as_mut() {
            // OwnedFd/File close is normal RAII, not proof, and is never
            // retried as a raw close operation.
            drop(active.irq.take());
            #[cfg(feature = "fuchsia-passive")]
            release_dma(&mut active.data_rx_buffers, &mut failures);
            #[cfg(feature = "fuchsia-passive")]
            release_dma(&mut active.data_rx_ring, &mut failures);
            #[cfg(feature = "fuchsia-passive")]
            release_dma(&mut active.mgmt_frame, &mut failures);
            #[cfg(feature = "fuchsia-passive")]
            release_dma(&mut active.mgmt_txwi, &mut failures);
            #[cfg(feature = "fuchsia-passive")]
            release_dma(&mut active.mgmt_tx_ring, &mut failures);
            for slot in [
                &mut active.mcu_wa_rx_buffers,
                &mut active.mcu_wa_rx_ring,
                &mut active.fwdl_payload,
                &mut active.command_payload,
                &mut active.mcu_rx_buffers,
                &mut active.mcu_rx_ring,
                &mut active.rx_guard,
                &mut active.mcu_tx_ring,
                &mut active.fwdl_ring,
                &mut active.tx_guard,
            ] {
                release_dma(slot, &mut failures);
            }
            #[cfg(feature = "fuchsia-passive")]
            for page in active.passive_window_pages.iter_mut().flatten() {
                if let Err(error) = page.teardown() {
                    failures.push(ReleaseFailure {
                        action: ObservableRelease::BarMunmap,
                        error,
                    });
                }
            }
            for slot in [
                &mut active.dmashdl,
                &mut active.swdef,
                &mut active.dynamic_window,
                &mut active.selector_page,
            ] {
                release_bar(slot, &mut failures);
            }
        }
        for slot in [&mut self.conn, &mut self.pcie_mac, &mut self.wfdma] {
            release_bar(slot, &mut failures);
        }
        if self.ioas_attached {
            let mut detach = Detach {
                argsz: size::<Detach>(),
                ..Default::default()
            };
            if let Err(error) = ioctl_mut(
                self.device.as_raw_fd(),
                VFIO_DEVICE_DETACH_IOMMUFD_PT,
                &mut detach,
                "detach VFIO device from IOAS",
            ) {
                failures.push(ReleaseFailure {
                    action: ObservableRelease::IoasDetach,
                    error,
                });
            } else {
                self.ioas_attached = false;
            }
        }
        if !self.ioas_attached
            && let Some(ioas) = self.ioas.as_mut()
            && let Err(error) = ioas.teardown()
        {
            failures.push(ReleaseFailure {
                action: ObservableRelease::IoasDestroy,
                error,
            });
        }
        failures
    }

    fn retain_forever(self) -> ! {
        park_retention_capsule(self)
    }
}

fn report_acquisition_failure(capsule: &mut ActiveVfioCapsule, primary: String) -> String {
    if capsule
        .containment
        .as_ref()
        .is_some_and(ContainmentLedger::hardware_may_be_active)
    {
        park_retention_capsule_ref(capsule);
    }
    let release_errors = capsule.release_observable();
    if let Some(ledger) = capsule.containment.as_mut() {
        ledger.phase = if release_errors.is_empty() {
            RunPhase::Contained
        } else {
            RunPhase::SafeReleaseError
        };
    }
    if release_errors.is_empty() {
        primary
    } else {
        format!("{primary}; SAFE acquisition release errors: {release_errors:?}")
    }
}

macro_rules! finish_owned_acquisition {
    ($capsule:expr, $result:expr) => {
        match $result {
            Ok(value) => value,
            Err(primary) => return Err(report_acquisition_failure($capsule, primary)),
        }
    };
}

impl Drop for ActiveVfioResources {
    fn drop(&mut self) {
        // The IRQ contains the device raw fd, so release it before mappings and handles.
        drop(self.irq.take());
    }
}

impl Drop for ActiveVfioCapsule {
    fn drop(&mut self) {
        if self
            .containment
            .as_ref()
            .is_some_and(ContainmentLedger::hardware_may_be_active)
        {
            park_retention_capsule_ref(self);
        }
        // Preserve dependency order even on partial acquisition failures.
        drop(self.active.take());
        drop(self.conn.take());
        drop(self.pcie_mac.take());
        drop(self.wfdma.take());
        drop(self.ioas.take());
    }
}

fn acquire_active_vfio_resources(
    resources: &mut ActiveVfioResources,
    device: &Arc<File>,
    iommu: &Arc<File>,
    ioas: u32,
    info: &RegionInfo,
    operation: Operation,
    ledger: &mut AcquisitionLedger,
    containment: &mut ContainmentLedger,
) -> Result<(), String> {
    #[cfg(not(feature = "fuchsia-passive"))]
    let _ = operation;
    macro_rules! map_bar {
        ($field:ident, $offset:expr) => {{
            containment.mark_possibly_active(Hazard::BarMapping);
            ledger.record(AcquisitionIntent::MapBar($offset))?;
            resources.$field = Some(ReadPage::map(device, info, $offset, true)?);
        }};
    }
    macro_rules! map_dma {
        ($field:ident, $iova:expr, $len:expr) => {{
            containment.mark_possibly_active(Hazard::DmaMapping);
            ledger.record(AcquisitionIntent::MapDma {
                iova: $iova,
                len: $len,
            })?;
            resources.$field = Some(DmaArena::map_len(iommu, ioas, $iova, $len)?);
        }};
    }
    map_bar!(selector_page, 0xfe000);
    map_bar!(dynamic_window, MT_HIF_REMAP_WINDOW_BAR_OFFSET);
    #[cfg(feature = "fuchsia-passive")]
    if operation.is_passive() {
        for (slot, bar_page) in resources
            .passive_window_pages
            .iter_mut()
            .zip(PASSIVE_MAC_BAR_PAGES)
        {
            containment.mark_possibly_active(Hazard::BarMapping);
            ledger.record(AcquisitionIntent::MapBar(bar_page))?;
            *slot = Some(ReadPage::map(device, info, bar_page, true)?);
        }
    }
    map_bar!(swdef, 0x9f000);
    map_bar!(dmashdl, 0xd6000);
    map_dma!(tx_guard, 0x0100_0000, PAGE);
    map_dma!(fwdl_ring, 0x0100_1000, PAGE);
    map_dma!(mcu_tx_ring, 0x0100_2000, PAGE);
    map_dma!(rx_guard, 0x0100_3000, PAGE);
    map_dma!(mcu_rx_ring, 0x0100_4000, PAGE);
    map_dma!(mcu_rx_buffers, 0x0100_5000, 4 * PAGE);
    map_dma!(
        command_payload,
        MCU_COMMAND_PAYLOAD_IOVA,
        MCU_COMMAND_PAYLOAD_BYTES
    );
    map_dma!(fwdl_payload, 0x0100_a000, PAGE);
    map_dma!(mcu_wa_rx_ring, 0x0100_b000, PAGE);
    map_dma!(mcu_wa_rx_buffers, 0x0100_c000, 4 * PAGE);
    #[cfg(feature = "fuchsia-passive")]
    {
        map_dma!(data_rx_ring, 0x0101_0000, PAGE);
        map_dma!(data_rx_buffers, 0x0101_1000, 4 * PAGE);
        if operation == Operation::RunOneShotSaeAuth {
            map_dma!(mgmt_txwi, 0x0103_0000, PAGE);
            map_dma!(mgmt_frame, 0x0103_1000, PAGE);
            map_dma!(mgmt_tx_ring, 0x0103_2000, PAGE);
        }
    }

    let tx_guard = resources.tx_guard.as_mut().expect("mapped");
    let fwdl_ring = resources.fwdl_ring.as_mut().expect("mapped");
    let mcu_tx_ring = resources.mcu_tx_ring.as_mut().expect("mapped");
    let rx_guard = resources.rx_guard.as_mut().expect("mapped");
    let mcu_rx_ring = resources.mcu_rx_ring.as_mut().expect("mapped");
    let mcu_rx_buffers = resources.mcu_rx_buffers.as_mut().expect("mapped");
    let command_payload = resources.command_payload.as_mut().expect("mapped");
    let fwdl_payload = resources.fwdl_payload.as_mut().expect("mapped");
    let mcu_wa_rx_ring = resources.mcu_wa_rx_ring.as_mut().expect("mapped");
    let mcu_wa_rx_buffers = resources.mcu_wa_rx_buffers.as_mut().expect("mapped");
    tx_guard.initialize_descriptor_page()?;
    fwdl_ring.initialize_descriptor_page()?;
    mcu_tx_ring.initialize_descriptor_page()?;
    rx_guard.initialize_descriptor_page()?;
    mcu_rx_ring.initialize_descriptor_page()?;
    mcu_rx_buffers.zero_bytes(4 * PAGE)?;
    command_payload.zero_bytes(MCU_COMMAND_PAYLOAD_BYTES)?;
    fwdl_payload.zero_bytes(PAGE)?;
    mcu_wa_rx_ring.initialize_descriptor_page()?;
    mcu_wa_rx_buffers.zero_bytes(4 * PAGE)?;
    #[cfg(feature = "fuchsia-passive")]
    {
        resources
            .data_rx_ring
            .as_mut()
            .expect("mapped")
            .initialize_descriptor_page()?;
        resources
            .data_rx_buffers
            .as_mut()
            .expect("mapped")
            .zero_bytes(4 * PAGE)?;
        if let Some(arena) = resources.mgmt_txwi.as_mut() {
            arena.zero_bytes(PAGE)?;
        }
        if let Some(arena) = resources.mgmt_frame.as_mut() {
            arena.zero_bytes(PAGE)?;
        }
        if let Some(arena) = resources.mgmt_tx_ring.as_mut() {
            arena.initialize_descriptor_page()?;
        }
    }
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
    #[cfg(feature = "fuchsia-passive")]
    {
        let data_rx_ring = resources.data_rx_ring.as_mut().expect("mapped");
        let data_rx_buffers = resources.data_rx_buffers.as_ref().expect("mapped");
        let prepared_data = prepare_mcu_rx_ring(data_rx_ring.iova, data_rx_buffers.iova)
            .map_err(|error| format!("prepare data RX descriptors: {error:?}"))?;
        for (index, descriptor) in prepared_data.descriptors.into_iter().enumerate() {
            data_rx_ring.write_descriptor_at(index, descriptor);
        }
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
    Ok(())
}

pub fn main() {
    if let Err(message) = run() {
        eprintln!("mt7921-vfio-read: {message}");
        std::process::exit(1);
    }
}

#[cfg(feature = "fuchsia-passive")]
struct SaeCredential(Vec<u8>);

#[cfg(feature = "fuchsia-passive")]
impl Drop for SaeCredential {
    fn drop(&mut self) {
        self.0.fill(0);
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}

#[cfg(feature = "fuchsia-passive")]
fn read_sae_credential() -> Result<SaeCredential, String> {
    let raw_fd = env::var("DRV_SAE_CREDENTIAL_FD")
        .map_err(|_| "DRV_SAE_CREDENTIAL_FD is required")?
        .parse::<RawFd>()
        .map_err(|_| "DRV_SAE_CREDENTIAL_FD is invalid")?;
    if raw_fd <= 2 {
        return Err("DRV_SAE_CREDENTIAL_FD is invalid".into());
    }
    let mut credential = Vec::with_capacity(64);
    // SAFETY: the root launcher transfers this inherited descriptor exactly
    // once to this one-shot process; taking ownership also closes it promptly.
    let mut file = unsafe { File::from_raw_fd(raw_fd) };
    Read::by_ref(&mut file)
        .take(64)
        .read_to_end(&mut credential)
        .map_err(|_| "read SAE credential FD failed".to_string())?;
    if !(8..=63).contains(&credential.len()) {
        credential.fill(0);
        return Err("SAE credential length is invalid".into());
    }
    Ok(SaeCredential(credential))
}

#[cfg(feature = "fuchsia-passive")]
fn find_ie(ies: &[u8], wanted: u8) -> Option<&[u8]> {
    let mut offset = 0;
    while offset + 2 <= ies.len() {
        let length = usize::from(ies[offset + 1]);
        let end = offset.checked_add(length + 2)?;
        let ie = ies.get(offset..end)?;
        if ie[0] == wanted {
            return Some(ie);
        }
        offset = end;
    }
    None
}

#[cfg(feature = "fuchsia-passive")]
fn live_client_support(query: fidl_softmac::WlanSoftmacQueryResponse) -> ClientSupport {
    ClientSupport {
        query,
        discovery: fidl_softmac::DiscoverySupport {
            scan_offload: Some(fidl_softmac::ScanOffloadExtension {
                supported: Some(true),
                scan_cancel_supported: Some(true),
            }),
            ..Default::default()
        },
        mac_sublayer: fidl_common::MacSublayerSupport {
            device: Some(fidl_common::DeviceExtension {
                mac_implementation_type: Some(fidl_common::MacImplementationType::Softmac),
                ..Default::default()
            }),
            ..Default::default()
        },
        security: fidl_common::SecuritySupport {
            mfp: Some(fidl_common::MfpFeature {
                supported: Some(true),
            }),
            sae: Some(fidl_common::SaeFeature {
                driver_handler_supported: Some(false),
                sme_handler_supported: Some(true),
                hash_to_element_supported: Some(false),
            }),
            ..Default::default()
        },
        spectrum_management: Default::default(),
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
        #[cfg(feature = "fuchsia-passive")]
        Some("--run-one-shot-passive-prepare") => Operation::RunOneShotPassivePrepare,
        #[cfg(feature = "fuchsia-passive")]
        Some("--run-one-shot-passive-channel-1") => Operation::RunOneShotPassiveChannel1,
        #[cfg(feature = "fuchsia-passive")]
        Some("--run-one-shot-passive-channels-1-6") => Operation::RunOneShotPassiveChannels1And6,
        #[cfg(feature = "fuchsia-passive")]
        Some("--run-one-shot-passive-2ghz") => Operation::RunOneShotPassive2Ghz,
        #[cfg(feature = "fuchsia-passive")]
        Some("--run-one-shot-passive-5ghz-non-dfs") => Operation::RunOneShotPassive5GhzNonDfs,
        #[cfg(feature = "fuchsia-passive")]
        Some("--run-one-shot-passive-5ghz-dfs-low") => Operation::RunOneShotPassive5GhzDfsLow,
        #[cfg(feature = "fuchsia-passive")]
        Some("--run-one-shot-passive-5ghz-dfs-high") => Operation::RunOneShotPassive5GhzDfsHigh,
        #[cfg(feature = "fuchsia-passive")]
        Some("--run-one-shot-passive-sme-full") => Operation::RunOneShotPassiveSmeFull,
        #[cfg(feature = "fuchsia-passive")]
        Some("--run-one-shot-power-setup") => Operation::RunOneShotPowerSetup,
        #[cfg(feature = "fuchsia-passive")]
        Some("--run-one-shot-sae-auth") => Operation::RunOneShotSaeAuth,
        #[cfg(not(feature = "fuchsia-passive"))]
        Some("--run-one-shot-sae-auth") => return Err("SAE TX is disabled; connect orchestration must come from the full pinned Fuchsia client MLME".into()),
        Some(argument) => return Err(format!("unknown argument {argument}")),
    };
    #[cfg(feature = "fuchsia-passive")]
    if operation == Operation::RunOneShotSaeAuth {
        record_sae_stage("process_enter");
    }
    #[cfg(feature = "fuchsia-passive")]
    let power_target = if matches!(
        operation,
        Operation::RunOneShotPowerSetup | Operation::RunOneShotSaeAuth
    ) {
        let bssid = parse_mac(
            &env::var("DRV_SAE_BSSID").map_err(|_| "DRV_SAE_BSSID is required for power setup")?,
        )?;
        let ssid = env::var("DRV_SAE_SSID")
            .map_err(|_| "DRV_SAE_SSID is required for power setup")?
            .into_bytes();
        if ssid.is_empty() || ssid.len() > 32 {
            return Err("power-setup SSID length is invalid".into());
        }
        verify_no_usable_mt792x_acpi_sar()?;
        Some((bssid, ssid))
    } else {
        None
    };
    #[cfg(feature = "fuchsia-passive")]
    let mut sae_credential = (operation == Operation::RunOneShotSaeAuth)
        .then(read_sae_credential)
        .transpose()?;
    #[cfg(feature = "fuchsia-passive")]
    if operation == Operation::RunOneShotSaeAuth {
        record_sae_stage("credential_read");
    }
    let bdf = env::var("DRV_PCI_BDF").map_err(|_| "DRV_PCI_BDF is required")?;
    let vfio = env::var("DRV_VFIO_DEVICE").map_err(|_| "DRV_VFIO_DEVICE is required")?;
    verify_pci_identity(&bdf)?;
    let watchdog = operation
        .is_active_mcu()
        .then(verify_external_watchdog_armed)
        .transpose()?;
    #[cfg(feature = "fuchsia-passive")]
    if operation == Operation::RunOneShotSaeAuth {
        record_sae_stage("watchdog_verified");
    }
    let containment = operation
        .is_active_mcu()
        .then(|| ContainmentLedger::acquire(watchdog))
        .transpose()?;

    if operation == Operation::RunOneShotSaeAuth {
        record_sae_stage("vfio_cdev_open_before");
    }
    let device = Arc::new(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&vfio)
            .map_err(|error| format!("open {vfio}: {error}"))?,
    );
    if operation == Operation::RunOneShotSaeAuth {
        record_sae_stage("vfio_cdev_open_after");
        record_sae_stage("iommufd_open_before");
    }
    let iommu = Arc::new(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/iommu")
            .map_err(|error| format!("open /dev/iommu: {error}"))?,
    );
    if operation == Operation::RunOneShotSaeAuth {
        record_sae_stage("iommufd_open_after");
    }
    let mut capsule = ActiveVfioCapsule::new(device, iommu, containment);
    // Advisory preflight facts are re-read with the complete resource owner
    // installed, before the first stateful VFIO operation is attempted.
    if operation == Operation::RunOneShotSaeAuth {
        record_sae_stage("second_pci_identity_before");
    }
    verify_pci_identity(&bdf)?;
    if operation == Operation::RunOneShotSaeAuth {
        record_sae_stage("second_pci_identity_after");
    }
    if operation != Operation::RunOneShotSaeAuth {
        verify_pci_dma_disabled(&bdf)?;
    }

    let base_acquisition = (|| -> Result<Option<RegionInfo>, String> {
        capsule.acquisition.record(AcquisitionIntent::BindIommu)?;
        let mut bind = Bind {
            argsz: size::<Bind>(),
            iommufd: capsule.iommu.as_raw_fd(),
            ..Default::default()
        };
        if let Some(ledger) = capsule.containment.as_mut() {
            ledger.mark_possibly_active(Hazard::VfioBound);
        }
        if operation == Operation::RunOneShotSaeAuth {
            record_sae_stage("vfio_bind_iommufd_before");
        }
        ioctl_mut(
            capsule.device.as_raw_fd(),
            VFIO_DEVICE_BIND_IOMMUFD,
            &mut bind,
            "bind iommufd",
        )?;
        if operation == Operation::RunOneShotSaeAuth {
            record_sae_stage("vfio_bind_iommufd_after");
        }
        capsule
            .acquisition
            .record(AcquisitionIntent::AllocateIoas)?;
        let mut alloc = IoasAlloc {
            size: size::<IoasAlloc>(),
            ..Default::default()
        };
        if let Some(ledger) = capsule.containment.as_mut() {
            ledger.mark_possibly_active(Hazard::IoasAllocated);
        }
        if operation == Operation::RunOneShotSaeAuth {
            record_sae_stage("ioas_allocate_before");
        }
        ioctl_mut(
            capsule.iommu.as_raw_fd(),
            IOMMU_IOAS_ALLOC,
            &mut alloc,
            "allocate IOAS",
        )?;
        if operation == Operation::RunOneShotSaeAuth {
            record_sae_stage("ioas_allocate_after");
        }
        capsule.ioas = Some(Ioas {
            fd: Arc::clone(&capsule.iommu),
            id: alloc.out_ioas_id,
            destroyed: false,
        });
        capsule.acquisition.record(AcquisitionIntent::AttachIoas)?;
        let mut attach = Attach {
            argsz: size::<Attach>(),
            pt_id: capsule.ioas.as_ref().expect("IOAS acquired").id,
            ..Default::default()
        };
        if let Some(ledger) = capsule.containment.as_mut() {
            ledger.mark_possibly_active(Hazard::IoasAttached);
        }
        if operation == Operation::RunOneShotSaeAuth {
            record_sae_stage("vfio_attach_iommufd_pt_before");
        }
        ioctl_mut(
            capsule.device.as_raw_fd(),
            VFIO_DEVICE_ATTACH_IOMMUFD_PT,
            &mut attach,
            "attach IOAS",
        )?;
        capsule.ioas_attached = true;
        if operation == Operation::RunOneShotSaeAuth {
            record_sae_stage("vfio_attach_iommufd_pt_after");
        }

        if operation == Operation::RunOneShotSaeAuth {
            verify_pci_dma_disabled(&bdf).map_err(|error| {
                format!("vfio_attached_d0_preflight_not_ready; refusing reset: {error}")
            })?;
            record_sae_stage("vfio_attached_d0_preflight_already_ready");

            let mut device_info = DeviceInfo {
                argsz: size::<DeviceInfo>(),
                ..Default::default()
            };
            record_sae_stage(&format!(
                "vfio_device_get_info_before argsz={}",
                device_info.argsz
            ));
            if let Err(error) = ioctl_mut(
                capsule.device.as_raw_fd(),
                VFIO_DEVICE_GET_INFO,
                &mut device_info,
                "query VFIO device info",
            ) {
                record_sae_stage(&format!(
                    "vfio_device_get_info_error argsz={} error={error}",
                    device_info.argsz
                ));
                return Err(error);
            }
            record_sae_stage(&format!(
                "vfio_device_get_info_after argsz={} flags={:#x} num_regions={} num_irqs={}",
                device_info.argsz,
                device_info.flags,
                device_info.num_regions,
                device_info.num_irqs
            ));

            let mut bar0 = None;
            for index in 0..device_info.num_regions {
                let mut region = RegionInfo {
                    argsz: size::<RegionInfo>(),
                    index,
                    ..Default::default()
                };
                record_sae_stage(&format!(
                    "vfio_device_get_region_info_before index={index} argsz={}",
                    region.argsz
                ));
                if unsafe {
                    ioctl(
                        capsule.device.as_raw_fd(),
                        VFIO_DEVICE_GET_REGION_INFO,
                        &mut region,
                    )
                } < 0
                {
                    let io_error = std::io::Error::last_os_error();
                    if io_error.raw_os_error() == Some(22) && index != 0 && index != 7 {
                        record_sae_stage(&format!(
                            "vfio_device_get_region_info_absent index={index} argsz={} errno=22",
                            region.argsz
                        ));
                        continue;
                    }
                    let error = format!("query VFIO region: {io_error}");
                    record_sae_stage(&format!(
                        "vfio_device_get_region_info_error index={index} argsz={} error={error}",
                        region.argsz
                    ));
                    return Err(error);
                }
                record_sae_stage(&format!(
                    "vfio_device_get_region_info_after index={index} argsz={} flags={:#x} cap_offset={} size={} offset={}",
                    region.argsz, region.flags, region.cap_offset, region.size, region.offset
                ));
                if index == BAR0_REGION {
                    bar0 = Some(region);
                }
            }
            record_sae_stage("vfio_region_discovery_complete");
            let bar0 = bar0.ok_or("required BAR0 region was not discovered")?;
            let selector_page = MT_HIF_REMAP_L1_BAR_OFFSET & !(PAGE - 1);
            record_sae_stage(&format!(
                "vfio_bar0_mmap_before page={selector_page:#x} length={} prot=read_write flags=shared region_size={} region_offset={}",
                PAGE, bar0.size, bar0.offset
            ));
            let mut page = match ReadPage::map(&capsule.device, &bar0, selector_page, true) {
                Ok(page) => page,
                Err(error) => {
                    record_sae_stage(&format!(
                        "vfio_bar0_mmap_error page={selector_page:#x} error={error}"
                    ));
                    return Err(error);
                }
            };
            record_sae_stage(&format!(
                "vfio_bar0_mmap_after page={selector_page:#x} length=4096"
            ));
            record_sae_stage(&format!(
                "vfio_remap_selector_read_before offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x}"
            ));
            let saved_selector = match page.read(MT_HIF_REMAP_L1_BAR_OFFSET) {
                Ok(value) => value,
                Err(error) => {
                    record_sae_stage(&format!(
                        "vfio_remap_selector_read_error offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} error={error}"
                    ));
                    return Err(error);
                }
            };
            record_sae_stage(&format!(
                "vfio_remap_selector_saved offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} value={saved_selector:#010x}"
            ));
            record_sae_stage(&format!(
                "vfio_remap_selector_identity_write_before offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} value={saved_selector:#010x}"
            ));
            if let Err(error) = page.write_remap_selector(saved_selector) {
                record_sae_stage(&format!(
                    "vfio_remap_selector_identity_write_error offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} error={error}"
                ));
                return Err(error);
            }
            record_sae_stage(&format!(
                "vfio_remap_selector_identity_write_after offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} value={saved_selector:#010x}"
            ));
            record_sae_stage(&format!(
                "vfio_remap_selector_readback_before offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x}"
            ));
            let readback = match page.read(MT_HIF_REMAP_L1_BAR_OFFSET) {
                Ok(value) => value,
                Err(error) => {
                    record_sae_stage(&format!(
                        "vfio_remap_selector_readback_error offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} error={error}"
                    ));
                    return Err(error);
                }
            };
            record_sae_stage(&format!(
                "vfio_remap_selector_readback_after offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} value={readback:#010x} equal={}",
                readback == saved_selector
            ));
            if readback != saved_selector {
                return Err(format!(
                    "L1 selector identity-write mismatch: saved={saved_selector:#010x} readback={readback:#010x}"
                ));
            }
            record_sae_stage(&format!(
                "vfio_bar0_munmap_before page={selector_page:#x} length=4096"
            ));
            if let Err(error) = page.teardown() {
                record_sae_stage(&format!(
                    "vfio_bar0_munmap_error page={selector_page:#x} error={error}"
                ));
                return Err(error);
            }
            record_sae_stage(&format!(
                "vfio_bar0_munmap_after page={selector_page:#x} length=4096"
            ));
            return Ok(None);
        }

        let mut info = RegionInfo {
            argsz: size::<RegionInfo>(),
            index: BAR0_REGION,
            ..Default::default()
        };
        ioctl_mut(
            capsule.device.as_raw_fd(),
            VFIO_DEVICE_GET_REGION_INFO,
            &mut info,
            "query BAR 0",
        )?;

        if let Some(ledger) = capsule.containment.as_mut() {
            ledger.mark_possibly_active(Hazard::BarMapping);
        }
        capsule
            .acquisition
            .record(AcquisitionIntent::MapBar(0xd4000))?;
        capsule.wfdma = Some(ReadPage::map(
            &capsule.device,
            &info,
            0xd4000,
            operation.wfdma_writable(),
        )?);
        if operation.needs_pcie_mac() {
            capsule
                .acquisition
                .record(AcquisitionIntent::MapBar(0x10000))?;
            capsule.pcie_mac = Some(ReadPage::map(&capsule.device, &info, 0x10000, true)?);
        }
        capsule
            .acquisition
            .record(AcquisitionIntent::MapBar(0xe0000))?;
        capsule.conn = Some(ReadPage::map(
            &capsule.device,
            &info,
            0xe0000,
            operation.conn_writable(),
        )?);
        Ok(Some(info))
    })();
    let info = finish_owned_acquisition!(&mut capsule, base_acquisition);
    if info.is_none() {
        let release_errors = capsule.release_observable();
        if !release_errors.is_empty() {
            record_sae_stage(&format!("vfio_region_discovery_release_error errors={release_errors:?}"));
            return Err(format!("VFIO discovery release failed: {release_errors:?}"));
        }
        if let Some(ledger) = capsule.containment.as_mut() {
            ledger.phase = RunPhase::Contained;
        }
        record_sae_stage("vfio_region_discovery_released_safe");
        return Ok(());
    }
    let info = info.expect("non-discovery operation queried BAR 0");

    let device = &capsule.device;
    let iommu = &capsule.iommu;
    let ioas = capsule.ioas.as_ref().expect("IOAS acquired");
    let wfdma = capsule.wfdma.as_ref().expect("WFDMA page acquired");
    let pcie_mac = capsule.pcie_mac.as_ref();
    let conn = capsule.conn.as_ref().expect("CONN page acquired");

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
    let mut active_terminal_error = None;
    if operation.is_active_mcu() {
        let active_preflight = (|| -> Result<_, String> {
            verify_pci_dma_disabled(&bdf)?;
            let selected = select_vfio_irq(&vfio_irq_capabilities(&device)?)
                .ok_or("VFIO exposes no eventfd-capable PCI interrupt")?;
            if selected.kind == PciIrqKind::Intx {
                return Err("active MCU transaction requires MSI or MSI-X, not level INTx".into());
            }
            verify_vfio_reset_supported(&device)?;
            println!("{{\"vfio_irq_selected\":\"{selected:?}\"}}");
            let firmware_images = if operation.loads_firmware() {
                let patch = decompress_patch()?;
                let ram = decompress_ram()?;
                Patch::parse(&patch).map_err(|error| format!("parse verified patch: {error:?}"))?;
                Firmware::parse(&ram).map_err(|error| format!("parse verified RAM: {error:?}"))?;
                Some((patch, ram))
            } else {
                None
            };
            Ok((selected, firmware_images))
        })();
        let (selected, firmware_images) = finish_owned_acquisition!(&mut capsule, active_preflight);
        let pcie_mac = pcie_mac.as_ref().expect("operation mapped PCIe MAC page");
        let acquisition_ledger = &mut capsule.acquisition;
        capsule
            .containment
            .as_mut()
            .expect("active MCU operation has containment ledger")
            .mark_possibly_active(Hazard::DmaMapping);
        capsule.active = Some(ActiveVfioResources::default());
        let active_acquisition = acquire_active_vfio_resources(
            capsule.active.as_mut().expect("active slots installed"),
            &capsule.device,
            &capsule.iommu,
            capsule.ioas.as_ref().expect("IOAS acquired").id,
            &info,
            operation,
            acquisition_ledger,
            capsule
                .containment
                .as_mut()
                .expect("active MCU operation has containment ledger"),
        );
        finish_owned_acquisition!(&mut capsule, active_acquisition);
        let mapped_transition = capsule
            .containment
            .as_mut()
            .expect("active MCU operation has containment ledger")
            .transition(RunPhase::Acquiring, RunPhase::MappedDmaDisabled);
        finish_owned_acquisition!(&mut capsule, mapped_transition);
        let host_control_transition = capsule
            .containment
            .as_mut()
            .expect("active MCU operation has containment ledger")
            .transition(RunPhase::MappedDmaDisabled, RunPhase::AcquiringHostControl);
        finish_owned_acquisition!(&mut capsule, host_control_transition);
        let signal = finish_owned_acquisition!(&mut capsule, ActiveSignalGuard::install());
        let resources = capsule
            .active
            .as_mut()
            .expect("active resources acquired before mutation");
        let ActiveVfioResources {
            selector_page,
            dynamic_window,
            #[cfg(feature = "fuchsia-passive")]
            passive_window_pages,
            swdef,
            dmashdl,
            tx_guard,
            fwdl_ring,
            mcu_tx_ring,
            rx_guard,
            mcu_rx_ring,
            mcu_rx_buffers,
            command_payload,
            fwdl_payload,
            mcu_wa_rx_ring,
            mcu_wa_rx_buffers,
            #[cfg(feature = "fuchsia-passive")]
            data_rx_ring,
            #[cfg(feature = "fuchsia-passive")]
            data_rx_buffers,
            #[cfg(feature = "fuchsia-passive")]
            mgmt_txwi,
            #[cfg(feature = "fuchsia-passive")]
            mgmt_frame,
            #[cfg(feature = "fuchsia-passive")]
            mgmt_tx_ring,
            irq,
        } = resources;
        let selector_page = selector_page.as_ref().expect("acquired");
        let dynamic_window = dynamic_window.as_ref().expect("acquired");
        #[cfg(feature = "fuchsia-passive")]
        let passive_window_pages = passive_window_pages;
        let swdef = swdef.as_ref().expect("acquired");
        let dmashdl = dmashdl.as_ref().expect("acquired");
        let tx_guard = tx_guard.as_mut().expect("acquired");
        let fwdl_ring = fwdl_ring.as_mut().expect("acquired");
        let mcu_tx_ring = mcu_tx_ring.as_mut().expect("acquired");
        let rx_guard = rx_guard.as_mut().expect("acquired");
        let mcu_rx_ring = mcu_rx_ring.as_mut().expect("acquired");
        let mcu_rx_buffers = mcu_rx_buffers.as_mut().expect("acquired");
        let command_payload = command_payload.as_mut().expect("acquired");
        let fwdl_payload = fwdl_payload.as_mut().expect("acquired");
        let mcu_wa_rx_ring = mcu_wa_rx_ring.as_mut().expect("acquired");
        let mcu_wa_rx_buffers = mcu_wa_rx_buffers.as_mut().expect("acquired");
        #[cfg(feature = "fuchsia-passive")]
        let data_rx_ring = data_rx_ring.as_mut().expect("acquired");
        #[cfg(feature = "fuchsia-passive")]
        let data_rx_buffers = data_rx_buffers.as_mut().expect("acquired");
        #[cfg(feature = "fuchsia-passive")]
        let mut mgmt_txwi = mgmt_txwi.as_mut();
        #[cfg(feature = "fuchsia-passive")]
        let mut mgmt_frame = mgmt_frame.as_mut();
        #[cfg(feature = "fuchsia-passive")]
        let mut mgmt_tx_ring = mgmt_tx_ring.as_mut();
        let ledger = capsule
            .containment
            .as_mut()
            .expect("active MCU operation has containment ledger");
        ledger.mark_possibly_active(Hazard::LabMutated);
        ledger.mark_possibly_active(Hazard::HostControl);
        let active = (|| -> Result<(), String> {
            set_lab_safety("MUTATED")?;
            disable_pci_intx(&bdf)?;
            pcie_mac.write_pcie_mac_interrupt_enable_zero()?;
            let mut ownership = VfioOwnership {
                page: &conn,
                start: Instant::now(),
            };
            acquire_driver_ownership(&mut ownership, log_ownership_event)
                .map_err(|error| format!("acquire ownership for MCU transaction: {error:?}"))?;
            capsule
                .containment
                .as_mut()
                .expect("active MCU operation has containment ledger")
                .transition(RunPhase::AcquiringHostControl, RunPhase::HostDriverOwned)?;
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
            capsule
                .containment
                .as_mut()
                .expect("active MCU operation has containment ledger")
                .transition(
                    RunPhase::HostDriverOwned,
                    RunPhase::WfsysResetAndSelectorRestored,
                )?;

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
            capsule
                .containment
                .as_mut()
                .expect("active MCU operation has containment ledger")
                .mark_possibly_active(Hazard::DeviceIrq);
            acquisition_ledger.record(AcquisitionIntent::InstallIrq)?;
            *irq = Some(VfioIrq::install(&device, selected)?);
            if irq.as_ref().expect("IRQ installed").try_read()?.is_some() {
                return Err("unexpected IRQ before device source enable".into());
            }
            println!("{{\"active_mcu_event\":\"vfio_irq_installed\"}}");
            if wfdma.read(0xd4200)? != 0 {
                return Err(format!(
                    "refused nonzero interrupt status before activation: {:#010x}",
                    wfdma.read(0xd4200)?
                ));
            }
            capsule
                .containment
                .as_mut()
                .expect("active MCU operation has containment ledger")
                .transition(
                    RunPhase::WfsysResetAndSelectorRestored,
                    RunPhase::RingsPreparedIrqSourceDisabled,
                )?;
            capsule
                .containment
                .as_mut()
                .expect("active MCU operation has containment ledger")
                .mark_possibly_active(Hazard::Wfdma);
            wfdma.write_active_wfdma(0xd42f0, 0)?;
            wfdma.write_active_wfdma(0xd4680, 4)?;
            wfdma.write_active_wfdma(0xd4690, 0x00c0_0004)?;
            wfdma.write_active_wfdma(0xd4640, 0x0340_0004)?;
            wfdma.write_active_wfdma(0xd4644, 0x0380_0004)?;
            capsule
                .containment
                .as_mut()
                .expect("active MCU operation has containment ledger")
                .mark_possibly_active(Hazard::BusMaster);
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
            let response_irq_mask = if operation.loads_firmware() {
                firmware_bootstrap_rx_irq_mask()
            } else {
                1 << 0
            };
            wfdma.write_active_wfdma(0xd4204, response_irq_mask)?;
            capsule
                .containment
                .as_mut()
                .expect("active MCU operation has containment ledger")
                .transition(
                    RunPhase::RingsPreparedIrqSourceDisabled,
                    RunPhase::DmaAndResponseIrqEnabled,
                )?;
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
            if operation.loads_firmware() {
                let (patch_bytes, ram_bytes) = firmware_images
                    .as_ref()
                    .expect("one-shot operation validated firmware artifacts");
                let mcu = ActiveMcuIo {
                    wfdma: &wfdma,
                    irq: irq.as_mut().expect("IRQ installed"),
                    signal: &signal,
                    tx_ring: &mut *mcu_tx_ring,
                    payload: &mut *command_payload,
                    wm: ActiveMcuRx {
                        rx_ring: &mut *mcu_rx_ring,
                        rx_buffers: &mcu_rx_buffers,
                        rx_tail: 0,
                        rx_head: 7,
                        rx_ring_index: 0,
                        rx_count: 8,
                        irq_bit: WM_RX_IRQ_BIT,
                    },
                    wm2: Some(ActiveMcuRx {
                        rx_ring: &mut *mcu_wa_rx_ring,
                        rx_buffers: &mcu_wa_rx_buffers,
                        rx_tail: 0,
                        rx_head: 7,
                        rx_ring_index: 4,
                        rx_count: 8,
                        irq_bit: WM2_RX_IRQ_BIT,
                    }),
                    extra_irq_mask: 0,
                    unsolicited: Vec::new(),
                    normal_rx_frames: Vec::new(),
                    descriptor_provenance: DescriptorProvenance::new(),
                };
                let mut loader = VfioFirmwareLoader {
                    mcu,
                    conn: &conn,
                    pcie_mac,
                    device: &device,
                    bdf: &bdf,
                    fwdl_ring: &mut *fwdl_ring,
                    fwdl_payload: &mut *fwdl_payload,
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
                #[cfg(feature = "fuchsia-passive")]
                let result = if operation == Operation::RunOneShotPassivePrepare {
                    load_mt7921_firmware_with_passive_boundary(
                        &mut loader,
                        patch,
                        firmware,
                        |loader, report| {
                            capsule
                                .containment
                                .as_mut()
                                .expect("active MCU operation has containment ledger")
                                .transition(
                                    RunPhase::DmaAndResponseIrqEnabled,
                                    RunPhase::FirmwareReady,
                                )?;
                            let mechanics = VfioPassiveMechanics {
                                loader,
                                ledger: capsule
                                    .containment
                                    .as_mut()
                                    .expect("active MCU operation has containment ledger"),
                                data: ActiveMcuRx {
                                    rx_ring: &mut *data_rx_ring,
                                    rx_buffers: &data_rx_buffers,
                                    rx_tail: 0,
                                    rx_head: 7,
                                    rx_ring_index: 2,
                                    rx_count: 8,
                                    irq_bit: DATA_RX_IRQ_BIT,
                                },
                                mac_pages: &*passive_window_pages,
                                scan_started: None,
                                pending_scan_done: None,
                                advertisements: Vec::new(),
                                tx_completions: Vec::new(),
                            };
                            let mut transport =
                                SourceExactPassiveTransport::new(mechanics, report.nic_capability)
                                    .map_err(|error| error.to_string())?;
                            let prerequisites = transport
                                .prepare_receive_only()
                                .map_err(|error| error.to_string())?;
                            if prerequisites
                                != (PassivePrerequisites {
                                    channel_domain_mask_zero: true,
                                    mac_mmio_initialized: true,
                                    data_rx_owned: true,
                                })
                            {
                                return Err(format!(
                                    "passive prepare prerequisites mismatch: {prerequisites:?}"
                                ));
                            }
                            println!(r#"{{"passive_prepare_event":"gate_passed"}}"#);
                            Ok(())
                        },
                    )
                } else if matches!(
                    operation,
                    Operation::RunOneShotPassiveChannel1
                        | Operation::RunOneShotPassiveChannels1And6
                        | Operation::RunOneShotPassive2Ghz
                        | Operation::RunOneShotPassive5GhzNonDfs
                        | Operation::RunOneShotPassive5GhzDfsLow
                        | Operation::RunOneShotPassive5GhzDfsHigh
                        | Operation::RunOneShotPassiveSmeFull
                        | Operation::RunOneShotPowerSetup
                        | Operation::RunOneShotSaeAuth
                ) {
                    load_mt7921_firmware_with_passive_boundary(
                        &mut loader,
                        patch,
                        firmware,
                        |loader, report| {
                            capsule
                                .containment
                                .as_mut()
                                .expect("active MCU operation has containment ledger")
                                .transition(
                                    RunPhase::DmaAndResponseIrqEnabled,
                                    RunPhase::FirmwareReady,
                                )?;
                            let mechanics = VfioPassiveMechanics {
                                loader,
                                ledger: capsule
                                    .containment
                                    .as_mut()
                                    .expect("active MCU operation has containment ledger"),
                                data: ActiveMcuRx {
                                    rx_ring: &mut *data_rx_ring,
                                    rx_buffers: &data_rx_buffers,
                                    rx_tail: 0,
                                    rx_head: 7,
                                    rx_ring_index: 2,
                                    rx_count: 8,
                                    irq_bit: DATA_RX_IRQ_BIT,
                                },
                                mac_pages: &*passive_window_pages,
                                scan_started: None,
                                pending_scan_done: None,
                                advertisements: Vec::new(),
                                tx_completions: Vec::new(),
                            };
                            let transport =
                                SourceExactPassiveTransport::new(mechanics, report.nic_capability)
                                    .map_err(|error| error.to_string())?;
                            let candidates = candidate_channels(report.nic_capability);
                            let channels_for = |band, numbers: &[u8]| {
                                numbers
                                    .iter()
                                    .copied()
                                    .map(|number| ChannelNumber { band, number })
                                    .collect::<Vec<_>>()
                            };
                            let channels = match operation {
                                Operation::RunOneShotPassiveChannel1 => {
                                    channels_for(WlanBand::TwoGhz, &[1])
                                }
                                Operation::RunOneShotPassiveChannels1And6 => {
                                    channels_for(WlanBand::TwoGhz, &[1, 6])
                                }
                                Operation::RunOneShotPassive2Ghz => {
                                    let numbers = (1..=14).collect::<Vec<_>>();
                                    channels_for(WlanBand::TwoGhz, &numbers)
                                }
                                Operation::RunOneShotPassive5GhzNonDfs => channels_for(
                                    WlanBand::FiveGhz,
                                    &[36, 40, 44, 48, 149, 153, 157, 161, 165],
                                ),
                                Operation::RunOneShotPassive5GhzDfsLow => {
                                    channels_for(WlanBand::FiveGhz, &[52, 56, 60, 64])
                                }
                                Operation::RunOneShotPassive5GhzDfsHigh => channels_for(
                                    WlanBand::FiveGhz,
                                    &[100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144],
                                ),
                                Operation::RunOneShotPassiveSmeFull => allowed_passive_channels(
                                    &query_from_capabilities(report.nic_capability, &candidates),
                                    ConservativeRegulatoryPolicy {
                                        alpha2: *b"00",
                                        indoor: true,
                                        special_unii_mask: report.special_unii_mask,
                                    },
                                )
                                .map_err(|error| {
                                    format!("derive pinned SME channels: {error:?}")
                                })?,
                                Operation::RunOneShotPowerSetup => {
                                    channels_for(WlanBand::FiveGhz, &[36])
                                }
                                Operation::RunOneShotSaeAuth => {
                                    channels_for(WlanBand::FiveGhz, &[36])
                                }
                                _ => unreachable!("passive scan operation matched above"),
                            };
                            let mut adapter = Mt7921SoftmacAdapter::new(
                                transport,
                                report.nic_capability,
                                candidates.clone(),
                                channels.clone(),
                            )
                            .map_err(|error| error.to_string())?;
                            if operation == Operation::RunOneShotPassiveSmeFull {
                                let mut scanner = PassiveScanner::default();
                                scanner
                                    .start(
                                        &mut adapter,
                                        ScanRequest {
                                            txn_id: 1,
                                            scan_type: ScanTypes::Passive,
                                            channel_list: channels,
                                            ssid_list: vec![],
                                            probe_delay: 0,
                                            min_channel_time: 50,
                                            max_channel_time: 120,
                                        },
                                    )
                                    .map_err(|error| error.to_string())?;
                                let mut results = 0usize;
                                loop {
                                    match scanner
                                        .poll(&mut adapter)
                                        .map_err(|error| error.to_string())?
                                    {
                                        Some(MlmeScanEvent::Result { result, .. }) => {
                                            results += 1;
                                            println!(
                                                r#"{{"passive_sme_result":{{"txn_id":{},"bss":"{:?}"}}}}"#,
                                                result.txn_id, result.bss
                                            );
                                        }
                                        Some(MlmeScanEvent::End(end)) => {
                                            if end.txn_id != 1
                                                || end.code != ScanResultCode::Success
                                                || results == 0
                                            {
                                                return Err(format!(
                                                    "SME full scan failed: end={end:?} results={results}"
                                                ));
                                            }
                                            println!(
                                                r#"{{"passive_scan_event":"sme_full_gate_passed","txn_id":1,"results":{results}}}"#
                                            );
                                            return Ok(());
                                        }
                                        None => {
                                            std::thread::sleep(std::time::Duration::from_millis(1))
                                        }
                                    }
                                }
                            }
                            let mut beacon_authorizer =
                                power_target.as_ref().map(|(bssid, ssid)| {
                                    BeaconHintAuthorizer::new(*bssid, ssid.clone())
                                });
                            let mut beacon_authorization = None;
                            let mut target_bss = None;
                            let mut total_observations = 0usize;
                            for channel in &channels {
                                adapter
                                    .set_channel(WlanSoftmacBaseSetChannelRequest {
                                        primary: Some(*channel),
                                        bandwidth: Some(ChannelBandwidth::Cbw20),
                                        vht_secondary_80_channel: None,
                                    })
                                    .map_err(|error| error.to_string())?;
                                for attempt in 1..=operation.passive_scan_attempt_limit() {
                                    let response = adapter
                                        .start_passive_scan(
                                            WlanSoftmacBaseStartPassiveScanRequest {
                                                channels: Some(vec![*channel]),
                                                min_channel_time: Some(50_000_000),
                                                max_channel_time: Some(120_000_000),
                                                min_home_time: Some(0),
                                            },
                                        )
                                        .map_err(|error| error.to_string())?;
                                    let scan_id =
                                        response.scan_id.ok_or("passive scan omitted id")?;
                                    if let Some(authorizer) = beacon_authorizer.as_mut() {
                                        authorizer.begin_passive_scan(scan_id, *channel);
                                    }
                                    let mut observations = 0usize;
                                    let success = loop {
                                        match adapter
                                            .next_scan_event()
                                            .map_err(|error| error.to_string())?
                                        {
                                            Some(HardwareScanEvent::Observation(observation)) => {
                                                observations += 1;
                                                if let Some(authorizer) = beacon_authorizer.as_mut()
                                                    && let Some(authorization) =
                                                        authorizer.observe(scan_id, &observation)
                                                {
                                                    beacon_authorization = Some(authorization);
                                                    target_bss = Some(observation.bss.clone());
                                                }
                                                println!(
                                                    r#"{{"passive_scan_observation":{{"scan_id":{scan_id},"value":"{observation:?}"}}}}"#
                                                );
                                            }
                                            Some(HardwareScanEvent::Complete {
                                                scan_id: completed,
                                                success,
                                            }) if completed == scan_id => break success,
                                            Some(HardwareScanEvent::Complete {
                                                scan_id: completed,
                                                ..
                                            }) => {
                                                return Err(format!(
                                                    "passive completion id {completed} did not match {scan_id}"
                                                ));
                                            }
                                            None => std::thread::sleep(
                                                std::time::Duration::from_millis(1),
                                            ),
                                        }
                                    };
                                    if !success {
                                        return Err(format!(
                                            "passive channel {} failed completion",
                                            channel.number
                                        ));
                                    }
                                    total_observations += observations;
                                    println!(
                                        r#"{{"passive_scan_event":"channel_gate_passed","scan_id":{scan_id},"channel":{},"attempt":{attempt},"observations":{observations}}}"#,
                                        channel.number
                                    );
                                    if !operation.should_continue_passive_scans(
                                        attempt,
                                        beacon_authorization.is_some(),
                                    ) {
                                        break;
                                    }
                                }
                            }
                            let completion_only_group = matches!(
                                operation,
                                Operation::RunOneShotPassive5GhzDfsLow
                                    | Operation::RunOneShotPassive5GhzDfsHigh
                            );
                            if total_observations == 0 && !completion_only_group {
                                return Err(format!(
                                    "passive scan gate observed no BSS across {} channels",
                                    channels.len()
                                ));
                            }
                            if matches!(
                                operation,
                                Operation::RunOneShotPowerSetup | Operation::RunOneShotSaeAuth
                            ) {
                                let beacon_authorization = beacon_authorization
                                    .as_ref()
                                    .ok_or("target beacon did not authorize channel 36")?;
                                let beacon_authorizer = beacon_authorizer
                                    .as_ref()
                                    .expect("power setup created beacon authorizer");
                                if !beacon_authorizer.permits(beacon_authorization) {
                                    return Err(
                                        "target beacon authorization is no longer live".into()
                                    );
                                }
                                if operation == Operation::RunOneShotSaeAuth {
                                    let shared = Arc::new(Mutex::new(LiveClientState::default()));
                                    let effects = LiveClientEffects {
                                        state: shared.clone(),
                                        target: power_target.as_ref().expect("SAE target").0,
                                    };
                                    let support = live_client_support(query_from_capabilities(
                                        report.nic_capability,
                                        &candidates,
                                    ));
                                    let (mut device, runner) =
                                        Mt7921ClientDevice::new(effects, adapter, support);
                                    futures::executor::block_on(DeviceOps::set_channel(
                                        &mut device,
                                        channels[0],
                                        fidl_ieee80211::ChannelBandwidth::Cbw20,
                                        ChannelNumber {
                                            band: WlanBand::TwoGhz,
                                            number: 0,
                                        },
                                    ))
                                    .map_err(|status| {
                                        format!("DeviceOps set channel failed: {status}")
                                    })?;
                                    let response = futures::executor::block_on(
                                        DeviceOps::start_passive_scan(
                                            &mut device,
                                            &WlanSoftmacBaseStartPassiveScanRequest {
                                                channels: Some(vec![channels[0]]),
                                                min_channel_time: Some(50_000_000),
                                                max_channel_time: Some(120_000_000),
                                                min_home_time: Some(0),
                                            },
                                        ),
                                    )
                                    .map_err(|status| {
                                        format!("DeviceOps passive scan failed: {status}")
                                    })?;
                                    let scan_id = response
                                        .scan_id
                                        .ok_or("DeviceOps passive scan omitted id")?;
                                    loop {
                                        match runner.poll().map_err(|status| {
                                            format!("DeviceOps runner failed: {status}")
                                        })? {
                                            Some(HardwareScanEvent::Complete {
                                                scan_id: completed,
                                                success,
                                            }) if completed == scan_id && success => break,
                                            Some(HardwareScanEvent::Complete { .. }) => {
                                                return Err(
                                                    "DeviceOps scan completion failed".into()
                                                );
                                            }
                                            _ => std::thread::sleep(
                                                std::time::Duration::from_millis(1),
                                            ),
                                        }
                                    }
                                    runner.with_physical(|adapter| {
                                        adapter.with_transport_mut(|transport| {
                                            let mechanics = transport.mechanics_mut();
                                            mechanics.ledger.transition(
                                                RunPhase::PassiveReady,
                                                RunPhase::BeaconAuthorized,
                                            )?;
                                            program_live_rate_power(
                                                mechanics,
                                                report.nic_capability,
                                            )
                                        })
                                    })?;
                                    shared.lock().unwrap().power_rate_authorized = true;
                                    let bss =
                                        target_bss.as_ref().ok_or("target BSS was not retained")?;
                                    let rsne =
                                        find_ie(&bss.ies, 48).ok_or("target BSS omitted RSNE")?;
                                    let client = report
                                        .nic_capability
                                        .mac_address
                                        .ok_or("NIC omitted MAC address")?;
                                    let credential = sae_credential
                                        .as_mut()
                                        .ok_or("SAE credential unavailable")?;
                                    let mut handshake = SaeHandshake::new(
                                        power_target.as_ref().unwrap().1.clone(),
                                        credential.0.clone(),
                                        client.into(),
                                        power_target.as_ref().unwrap().0.into(),
                                        rsne,
                                        false,
                                    )
                                    .map_err(|_| {
                                        "initialize pinned SAE supplicant failed".to_string()
                                    })?;
                                    credential.0.fill(0);
                                    std::sync::atomic::compiler_fence(Ordering::SeqCst);
                                    let frames = handshake
                                        .start()
                                        .map_err(|_| {
                                            "start pinned SAE supplicant failed".to_string()
                                        })?
                                        .into_iter()
                                        .filter_map(|u| match u {
                                            SaeHandshakeUpdate::TxFrame(f) => Some(f),
                                            _ => None,
                                        })
                                        .collect::<Vec<_>>();
                                    if frames.len() != 1 {
                                        return Err(
                                            "pinned SAE startup did not produce exactly one frame"
                                                .into(),
                                        );
                                    }
                                    let frame = build_sae_auth_frame(
                                        client.into(),
                                        power_target.as_ref().unwrap().0.into(),
                                        0,
                                        &frames[0],
                                    )
                                    .map_err(|_| {
                                        "build pinned SAE authentication frame failed".to_string()
                                    })?;
                                    DeviceOps::send_wlan_frame(
                                        &mut device,
                                        frame.into(),
                                        fidl_softmac::WlanTxInfoFlags::empty(),
                                        None,
                                    )
                                    .map_err(|status| {
                                        format!("DeviceOps SAE TX rejected: {status}")
                                    })?;
                                    let frame = shared
                                        .lock()
                                        .unwrap()
                                        .frame
                                        .take()
                                        .ok_or("DeviceOps did not retain SAE frame")?;
                                    runner.with_physical(|adapter| {
                                        adapter.with_transport_mut(|transport| {
                                            let mechanics = transport.mechanics_mut();
                                            let ring = mgmt_tx_ring
                                                .as_deref_mut()
                                                .ok_or("SAE TX ring arena missing")?;
                                            println!(r#"{{"sae_auth_event":"spike_only_not_production_safe","failure_recovery":"reboot_required"}}"#);
                                            mechanics.transmit_one_sae_auth(
                                                ring,
                                                mgmt_txwi
                                                    .as_deref_mut()
                                                    .ok_or("SAE TXWI arena missing")?,
                                                mgmt_frame
                                                    .as_deref_mut()
                                                    .ok_or("SAE frame arena missing")?,
                                                &frame,
                                            )
                                        })
                                    })?;
                                    println!(r#"{{"sae_auth_event":"one_management_mpdu_acked"}}"#);
                                    return Ok(());
                                }
                                let transport = adapter.into_transport();
                                let mut mechanics = transport.into_mechanics();
                                mechanics.ledger.transition(
                                    RunPhase::PassiveReady,
                                    RunPhase::BeaconAuthorized,
                                )?;
                                let _ = drain_data_rx_queue(
                                    mechanics.loader.mcu.wfdma,
                                    &mut mechanics.data,
                                    &mut mechanics.loader.mcu.descriptor_provenance,
                                    &mut mechanics.tx_completions,
                                )?;
                                mechanics.loader.mcu.extra_irq_mask = 0;
                                mechanics.loader.mcu.wfdma.write_active_wfdma(
                                    0xd4204,
                                    mechanics.loader.mcu.rx_irq_mask(),
                                )?;
                                let mut power_transport = VfioRateTxPower {
                                    loader: &mut *mechanics.loader,
                                };
                                let mut power_authorizer = RateTxPowerAuthorizer::new();
                                let authorization = power_authorizer
                                    .submit(
                                        &mut power_transport,
                                        report.nic_capability,
                                        ConservativePowerLimits {
                                            alpha2: *b"00",
                                            max_reg_power_dbm: 20,
                                            // Read-only ACPI evidence for this exact host has MTDS/MTGS
                                            // but no MTCL, so pinned initialization rejects the tables
                                            // and applies no narrower ACPI SAR range.
                                            sar_limit_half_dbm: Some(40),
                                            // Keep this setup-only boundary at 0 dBm for every rate.
                                            external_safety_cap_half_dbm: Some(0),
                                        },
                                        1,
                                    )
                                    .map_err(|error| {
                                        format!("submit rate-power setup: {error:?}")
                                    })?;
                                if !power_authorizer.permits(&authorization) {
                                    return Err("rate-power authorization is not live".into());
                                }
                                mechanics.ledger.transition(
                                    RunPhase::BeaconAuthorized,
                                    RunPhase::PowerConfiguredNoFrame,
                                )?;
                                println!(
                                    "{}",
                                    r#"{"power_setup_event":"no_frame_gate_passed","beacon_authorized":true,"rate_power_consumed":true,"management_frame_publish_reachable":false}"#
                                );
                                power_authorizer.reset();
                                return Ok(());
                            }
                            println!(
                                r#"{{"passive_scan_event":"sequential_gate_passed","channels":{},"observations":{total_observations}}}"#,
                                channels.len()
                            );
                            Ok(())
                        },
                    )
                } else if operation == Operation::RunOneShotChannelDomain {
                    load_mt7921_firmware_through_channel_domain(&mut loader, patch, firmware)
                } else {
                    load_mt7921_firmware(&mut loader, patch, firmware)
                };
                #[cfg(not(feature = "fuchsia-passive"))]
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
                tx_ring: &mut *mcu_tx_ring,
                payload: &mut *command_payload,
                wm: ActiveMcuRx {
                    rx_ring: &mut *mcu_rx_ring,
                    rx_buffers: &mcu_rx_buffers,
                    rx_tail: 0,
                    rx_head: 7,
                    rx_ring_index: 0,
                    rx_count: 8,
                    irq_bit: WM_RX_IRQ_BIT,
                },
                wm2: None,
                extra_irq_mask: 0,
                unsolicited: Vec::new(),
                normal_rx_frames: Vec::new(),
                descriptor_provenance: DescriptorProvenance::new(),
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

        let ledger = capsule
            .containment
            .as_mut()
            .expect("active MCU operation has containment ledger");
        if active.is_err() {
            ledger.phase = RunPhase::Faulted;
        }
        ledger.phase = RunPhase::Containing;
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
        if let Some(installed) = irq.as_mut()
            && let Err(error) = installed.disable()
        {
            cleanup_errors.push(error);
        }
        let reset = reset_vfio_device(&device);
        if reset.is_err() {
            retain_mappings_for_watchdog("reset while pinned failed");
        }
        println!("{{\"active_mcu_event\":\"vfio_device_reset_while_pinned\"}}");
        if verify_pci_dma_disabled(&bdf)
            .and_then(|()| verify_active_reset_containment(wfdma, pcie_mac))
            .and_then(|()| set_lab_safety("SAFE"))
            .is_err()
        {
            retain_mappings_for_watchdog("post-reset containment verification failed");
        }
        for hazard in [
            Hazard::HostControl,
            Hazard::DeviceIrq,
            Hazard::Wfdma,
            Hazard::BusMaster,
            Hazard::LabMutated,
        ] {
            ledger.confirm_inactive(hazard);
        }
        let release_errors = attempt_all_cleanup(
            [
                #[cfg(feature = "fuchsia-passive")]
                (ActiveArenaKind::DataBuffers, data_rx_buffers),
                #[cfg(feature = "fuchsia-passive")]
                (ActiveArenaKind::DataRing, data_rx_ring),
                (ActiveArenaKind::Wm2Buffers, mcu_wa_rx_buffers),
                (ActiveArenaKind::Wm2Ring, mcu_wa_rx_ring),
                (ActiveArenaKind::FwdlPayload, fwdl_payload),
                (ActiveArenaKind::CommandPayload, command_payload),
                (ActiveArenaKind::WmBuffers, mcu_rx_buffers),
                (ActiveArenaKind::WmRing, mcu_rx_ring),
                (ActiveArenaKind::RxGuard, rx_guard),
                (ActiveArenaKind::McuTxRing, mcu_tx_ring),
                (ActiveArenaKind::FwdlRing, fwdl_ring),
                (ActiveArenaKind::TxGuard, tx_guard),
            ],
            |(kind, arena)| {
                arena
                    .teardown()
                    .map_err(|error| format!("teardown {kind:?}: {error}"))
            },
        );
        ledger.confirm_inactive(Hazard::DmaMapping);
        println!("{{\"active_mcu_event\":\"all_dma_mappings_released_after_reset\"}}");
        if !release_errors.is_empty() {
            ledger.phase = RunPhase::SafeReleaseError;
            active_terminal_error = Some(match active {
                Err(primary) => format!(
                    "active MCU operation failed: {primary}; cleanup errors: {cleanup_errors:?}; SAFE resource release errors: {release_errors:?}"
                ),
                Ok(()) => format!(
                    "active MCU hardware is SAFE but resource release failed: {release_errors:?}; cleanup errors: {cleanup_errors:?}"
                ),
            });
        } else {
            match (active, cleanup_errors.is_empty()) {
                (Err(primary), true) => active_terminal_error = Some(primary),
                (Err(primary), false) => {
                    active_terminal_error = Some(format!(
                        "active MCU operation failed: {primary}; cleanup errors: {cleanup_errors:?}"
                    ));
                }
                (Ok(()), false) => {
                    active_terminal_error =
                        Some(format!("active MCU cleanup failed: {cleanup_errors:?}"));
                }
                (Ok(()), true) => {}
            }
        }
    }
    if let Some(primary) = active_terminal_error {
        let release_errors = capsule.release_observable();
        let ledger = capsule
            .containment
            .as_mut()
            .expect("active MCU operation has containment ledger");
        if release_errors.is_empty() {
            ledger.phase = RunPhase::Contained;
            return Err(primary);
        }
        ledger.phase = RunPhase::SafeReleaseError;
        return Err(format!(
            "{primary}; SAFE resource release errors: {release_errors:?}"
        ));
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
    let release_errors = capsule.release_observable();
    if let Some(ledger) = capsule.containment.as_mut() {
        ledger.phase = if release_errors.is_empty() {
            RunPhase::Contained
        } else {
            RunPhase::SafeReleaseError
        };
    }
    if !release_errors.is_empty() {
        return Err(format!(
            "hardware is SAFE but resource release failed: {release_errors:?}"
        ));
    }
    drop(capsule);
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

fn verify_active_reset_containment(wfdma: &ReadPage, pcie_mac: &ReadPage) -> Result<(), String> {
    let global = wfdma.read(0xd4208)?;
    let host_irq = wfdma.read(0xd4204)?;
    let mac_irq = pcie_mac.read(0x10188)?;
    if global == u32::MAX || host_irq == u32::MAX || mac_irq == u32::MAX {
        return Err("post-reset containment readback returned all ones".into());
    }
    if global & 0xf != 0 || host_irq != 0 || mac_irq != 0 {
        return Err(format!(
            "post-reset containment unsafe global={global:#010x} host_irq={host_irq:#010x} mac_irq={mac_irq:#010x}"
        ));
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ArmedWatchdog {
    deadline: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunPhase {
    Acquiring,
    MappedDmaDisabled,
    AcquiringHostControl,
    HostDriverOwned,
    WfsysResetAndSelectorRestored,
    RingsPreparedIrqSourceDisabled,
    DmaAndResponseIrqEnabled,
    FirmwareReady,
    PassivePreparing,
    PassiveReady,
    Scanning,
    BeaconAuthorized,
    PowerConfiguredNoFrame,
    Faulted,
    Containing,
    Contained,
    SafeReleaseError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Hazard {
    VfioBound,
    IoasAllocated,
    IoasAttached,
    BarMapping,
    DmaMapping,
    HostControl,
    DeviceIrq,
    Wfdma,
    BusMaster,
    LabMutated,
}

impl Hazard {
    const COUNT: usize = 10;

    const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EffectState {
    Inactive,
    PossiblyActive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContainmentLedger {
    phase: RunPhase,
    effects: [EffectState; Hazard::COUNT],
}

impl ContainmentLedger {
    fn acquire(watchdog: Option<ArmedWatchdog>) -> Result<Self, String> {
        watchdog.ok_or_else(|| {
            "external reboot watchdog was not verified before acquisition".to_string()
        })?;
        Ok(Self {
            phase: RunPhase::Acquiring,
            effects: [EffectState::Inactive; Hazard::COUNT],
        })
    }

    fn mark_possibly_active(&mut self, hazard: Hazard) {
        self.effects[hazard.index()] = EffectState::PossiblyActive;
    }

    fn confirm_inactive(&mut self, hazard: Hazard) {
        self.effects[hazard.index()] = EffectState::Inactive;
    }

    fn must_disable(&self, hazard: Hazard, resource_present: bool) -> bool {
        resource_present || self.effects[hazard.index()] == EffectState::PossiblyActive
    }

    fn hardware_may_be_active(&self) -> bool {
        [
            Hazard::HostControl,
            Hazard::DeviceIrq,
            Hazard::Wfdma,
            Hazard::BusMaster,
            Hazard::LabMutated,
        ]
        .into_iter()
        .any(|hazard| self.effects[hazard.index()] == EffectState::PossiblyActive)
    }

    fn transition(&mut self, expected: RunPhase, next: RunPhase) -> Result<(), String> {
        if self.phase != expected {
            return Err(format!(
                "active VFIO phase mismatch: expected {expected:?}, found {:?}",
                self.phase
            ));
        }
        self.phase = next;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObservableRelease {
    DmaUnmap,
    BarMunmap,
    IoasDetach,
    IoasDestroy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReleaseFailure {
    action: ObservableRelease,
    error: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ContainmentOutcome {
    Contained {
        primary: Option<String>,
        cleanup_errors: Vec<String>,
    },
    SafeReleaseError {
        primary: Option<String>,
        cleanup_errors: Vec<String>,
        release_errors: Vec<ReleaseFailure>,
    },
    RetainUnsafe {
        primary: Option<String>,
        cleanup_errors: Vec<String>,
    },
}

impl ContainmentOutcome {
    fn classify(
        hardware_safe: bool,
        primary: Option<String>,
        cleanup_errors: Vec<String>,
        release_errors: Vec<ReleaseFailure>,
    ) -> Self {
        if !hardware_safe {
            Self::RetainUnsafe {
                primary,
                cleanup_errors,
            }
        } else if release_errors.is_empty() {
            Self::Contained {
                primary,
                cleanup_errors,
            }
        } else {
            Self::SafeReleaseError {
                primary,
                cleanup_errors,
                release_errors,
            }
        }
    }

    const fn must_park(&self) -> bool {
        matches!(self, Self::RetainUnsafe { .. })
    }
}

fn verify_external_watchdog_armed() -> Result<ArmedWatchdog, String> {
    let output = Command::new(WATCHDOG_STATUS_PATH)
        .arg("status")
        .output()
        .map_err(|error| format!("query external reboot watchdog: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "external reboot watchdog status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let status = String::from_utf8(output.stdout)
        .map_err(|_| "external reboot watchdog status was not UTF-8".to_string())?;
    verify_watchdog_status(&status, unix_time_seconds()?)
}

fn unix_time_seconds() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| format!("read wall clock for reboot watchdog: {error}"))
}

fn verify_watchdog_status(status: &str, now: u64) -> Result<ArmedWatchdog, String> {
    let mut lines = status.lines();
    let first = lines.next().unwrap_or_default();
    let deadline = first
        .strip_prefix("armed deadline=")
        .ok_or_else(|| "external reboot watchdog is not armed".to_string())?
        .parse::<u64>()
        .map_err(|_| "external reboot watchdog deadline is invalid".to_string())?;
    if deadline <= now {
        return Err("external reboot watchdog deadline has expired".into());
    }
    let mut active = false;
    let mut waiting = false;
    for line in lines {
        active |= line == "ActiveState=active";
        waiting |= line == "SubState=waiting";
    }
    if !active || !waiting {
        return Err("external reboot watchdog timer is not active and waiting".into());
    }
    Ok(ArmedWatchdog { deadline })
}

fn retain_mappings_for_watchdog(message: &str) -> ! {
    const MESSAGE: &[u8] =
        b"mt7921-vfio-read: containment failed; resources pinned for reboot watchdog\n";
    unsafe {
        write_fd(2, MESSAGE.as_ptr(), MESSAGE.len());
    }
    loop {
        std::hint::black_box(message);
        unsafe {
            pause();
        }
    }
}

fn park_retention_capsule<T>(capsule: T) -> ! {
    park_retention_capsule_ref(&capsule)
}

fn park_retention_capsule_ref<T: ?Sized>(capsule: &T) -> ! {
    const MESSAGE: &[u8] =
        b"mt7921-vfio-read: hardware SAFE is unproven; resources pinned for reboot watchdog\n";
    unsafe {
        write_fd(2, MESSAGE.as_ptr(), MESSAGE.len());
    }
    loop {
        std::hint::black_box(capsule);
        unsafe {
            pause();
        }
    }
}

fn publish_mcu_command(
    wfdma: &ReadPage,
    tx_ring: &mut DmaArena,
    payload: &mut DmaArena,
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
    tx_ring: &mut DmaArena,
    payload: &mut DmaArena,
    bytes: &[u8],
    sequence: u8,
    descriptor_index: usize,
) -> Result<(), String> {
    let payload_offset = descriptor_index * MCU_COMMAND_SLOT_BYTES;
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
    wfdma.write_active_wfdma(
        0xd4418,
        next_dma_index(descriptor_index, MCU_TX_RING_COUNT) as u32,
    )?;
    println!(
        "{{\"active_mcu_event\":\"command_published\",\"sequence\":{sequence},\"tx_descriptor\":{descriptor_index}}}"
    );
    Ok(())
}

struct ActiveMcuRx<'a> {
    rx_ring: &'a mut DmaArena,
    rx_buffers: &'a DmaArena,
    rx_tail: usize,
    rx_head: usize,
    rx_ring_index: usize,
    rx_count: usize,
    irq_bit: u32,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DescriptorOccurrenceRoute {
    DataRx,
    McuNormalRx,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DescriptorInvalidation {
    Cancellation,
    Teardown,
    Interface,
    Run,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DescriptorSlotState {
    Vacant(u64),
    Armed(u64),
    Consumed(u64),
}

struct DescriptorRingProvenance {
    route: DescriptorOccurrenceRoute,
    ring: usize,
    slots: Vec<DescriptorSlotState>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct DescriptorOccurrenceIdentity {
    owner: NonZeroU64,
    interface_epoch: u64,
    device_epoch: u64,
    reset_epoch: u64,
    ownership_epoch: u64,
    run_epoch: u64,
    scan_id: u64,
    scan_epoch: u64,
    route: DescriptorOccurrenceRoute,
    ring: usize,
    slot: usize,
    slot_epoch: u64,
    occurrence: u64,
}

struct DescriptorOccurrenceLease {
    owner: NonZeroU64,
    current: AtomicBool,
}

struct DescriptorOccurrence {
    identity: DescriptorOccurrenceIdentity,
    lease: Arc<DescriptorOccurrenceLease>,
}

#[cfg(all(test, feature = "fuchsia-passive"))]
struct ProvenanceHandle {
    session: NonZeroU64,
    generation: u64,
    index: usize,
    drop_order_probe: Option<(Arc<DescriptorOccurrenceLease>, Arc<AtomicBool>)>,
}

#[cfg(all(test, feature = "fuchsia-passive"))]
impl Drop for ProvenanceHandle {
    fn drop(&mut self) {
        if let Some((lease, released_after_invalidation)) = &self.drop_order_probe {
            released_after_invalidation
                .store(!lease.current.load(Ordering::Acquire), Ordering::Release);
        }
    }
}

#[cfg(all(test, feature = "fuchsia-passive"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegistrationDisposition {
    Pending,
    Produced,
    Ignored(wlan_mlme::ScanResultIgnore),
    ConversionDrop,
}

#[cfg(all(test, feature = "fuchsia-passive"))]
struct ProvenanceRegistration {
    generation: u64,
    occurrence: DescriptorOccurrence,
    disposition: RegistrationDisposition,
    produced_txn_id: Option<u64>,
    produced_timestamp_nanos: Option<i64>,
}

#[cfg(all(test, feature = "fuchsia-passive"))]
struct CarriedScanResult {
    provenance: ProvenanceHandle,
    result: fidl_fuchsia_wlan_mlme::ScanResult,
}

#[cfg(all(test, feature = "fuchsia-passive"))]
struct ValidatedSmeAggregate {
    terminal: wlan_sme::client::TrustedScanTerminal<ProvenanceHandle>,
}

/// Binary-private, compile/test-only B2a arena. It owns the actual B1
/// occurrences and the only sink allowed to validate and export a scan result.
#[cfg(all(test, feature = "fuchsia-passive"))]
struct ProvenanceSession {
    source: DescriptorProvenance,
    id: NonZeroU64,
    generation: u64,
    ingress: Vec<PrivateRawFrameCarrier>,
    registrations: Vec<ProvenanceRegistration>,
    quarantine: Vec<PrivateRawFrameCarrier>,
    observed_order: Vec<u64>,
    result_tx: Option<futures::channel::mpsc::UnboundedSender<CarriedScanResult>>,
    result_rx: futures::channel::mpsc::UnboundedReceiver<CarriedScanResult>,
    carried_results: Vec<CarriedScanResult>,
    outstanding_results: usize,
    sme: wlan_sme::client::ClientSme,
    sme_requests: futures::channel::mpsc::UnboundedReceiver<wlan_sme::MlmeRequest>,
    sme_state: wlan_sme::client::TrustedScanState<ProvenanceHandle>,
    sme_output: Option<ValidatedSmeAggregate>,
    reject_next_sme_output: bool,
    panic_next_sme_validation: bool,
    panic_next_sme_result_aggregation: bool,
    sme_started: bool,
    sme_terminal_accepted: bool,
    expected_sme_txn_id: Option<u64>,
    next_observe_index: usize,
    last_admitted_occurrence: Option<u64>,
    next_handle_drop_order_probe: Option<Arc<AtomicBool>>,
    closed: bool,
    invalidated: bool,
    poisoned: bool,
}

#[cfg(all(test, feature = "fuchsia-passive"))]
impl ProvenanceSession {
    const CAPACITY: usize = 4096;

    fn new() -> Self {
        let source = DescriptorProvenance::new();
        let id = source.owner;
        let (result_tx, result_rx) = futures::channel::mpsc::unbounded();
        let inspector = fuchsia_inspect::Inspector::default();
        let inspect_node = inspector.root().create_child("private-provenance-sme");
        let (sme, _sme_sink, sme_requests, _sme_time) = wlan_sme::client::ClientSme::new(
            wlan_sme::client::ClientConfig::default(),
            fidl_fuchsia_wlan_mlme::DeviceInfo {
                sta_addr: [2, 0, 0, 0, 0, 1],
                factory_addr: [2, 0, 0, 0, 0, 1],
                role: fidl_fuchsia_wlan_common::WlanMacRole::Client,
                bands: vec![fidl_fuchsia_wlan_mlme::BandCapability {
                    band: fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz,
                    basic_rates: vec![2, 4, 11, 22],
                    ht_cap: None,
                    vht_cap: None,
                    primary_channels: vec![fidl_fuchsia_wlan_ieee80211::ChannelNumber {
                        band: fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz,
                        number: 1,
                    }],
                }],
                softmac_hardware_capability: 0,
                qos_capable: false,
            },
            inspector,
            inspect_node,
            Default::default(),
            Default::default(),
        );
        Self {
            source,
            id,
            generation: 1,
            ingress: Vec::new(),
            registrations: Vec::new(),
            quarantine: Vec::new(),
            observed_order: Vec::new(),
            result_tx: Some(result_tx),
            result_rx,
            carried_results: Vec::new(),
            outstanding_results: 0,
            sme,
            sme_requests,
            sme_state: wlan_sme::client::TrustedScanState::new(1),
            sme_output: None,
            reject_next_sme_output: false,
            panic_next_sme_validation: false,
            panic_next_sme_result_aggregation: false,
            sme_started: false,
            sme_terminal_accepted: false,
            expected_sme_txn_id: None,
            next_observe_index: 0,
            last_admitted_occurrence: None,
            next_handle_drop_order_probe: None,
            closed: false,
            invalidated: false,
            poisoned: false,
        }
    }

    fn enqueue(&mut self, carrier: PrivateRawFrameCarrier) -> Result<(), String> {
        if self.closed || self.poisoned {
            return Err("provenance session is closed or poisoned".into());
        }
        let Some(owned) = self.registrations.len().checked_add(self.ingress.len()) else {
            self.quarantine.push(carrier);
            self.fail_close();
            return Err("provenance session arena exhausted".into());
        };
        if owned >= Self::CAPACITY {
            self.quarantine.push(carrier);
            self.fail_close();
            return Err("provenance session arena exhausted".into());
        }
        self.ingress.push(carrier);
        Ok(())
    }

    fn flush_ingress(
        &mut self,
    ) -> Result<Vec<mt7921_softmac_adapter::PinnedClientRx<ProvenanceHandle>>, String> {
        self.ingress.sort_by_key(|carrier| {
            carrier
                .occurrence
                .as_ref()
                .map_or(u64::MAX, |occurrence| occurrence.identity.occurrence)
        });
        let ingress = std::mem::take(&mut self.ingress);
        ingress
            .into_iter()
            .map(|carrier| self.admit_carrier(carrier))
            .collect()
    }

    fn admit(&mut self, occurrence: DescriptorOccurrence) -> Result<ProvenanceHandle, String> {
        if self.closed || self.poisoned {
            return Err("provenance session is closed or poisoned".into());
        }
        if !self.ingress.is_empty() {
            self.quarantine.push(PrivateRawFrameCarrier {
                bytes: Vec::new(),
                occurrence: Some(occurrence),
            });
            self.fail_close();
            return Err("direct admission cannot bypass queued ingress".into());
        }
        let Some(owned) = self.registrations.len().checked_add(self.ingress.len()) else {
            self.quarantine.push(PrivateRawFrameCarrier {
                bytes: Vec::new(),
                occurrence: Some(occurrence),
            });
            self.fail_close();
            return Err("provenance session arena exhausted".into());
        };
        if owned >= Self::CAPACITY {
            self.quarantine.push(PrivateRawFrameCarrier {
                bytes: Vec::new(),
                occurrence: Some(occurrence),
            });
            self.fail_close();
            return Err("provenance session arena exhausted".into());
        }
        if self
            .last_admitted_occurrence
            .is_some_and(|previous| occurrence.identity.occurrence <= previous)
        {
            self.quarantine.push(PrivateRawFrameCarrier {
                bytes: Vec::new(),
                occurrence: Some(occurrence),
            });
            self.fail_close();
            return Err("provenance ingress occurrence order regressed".into());
        }
        let index = self.registrations.len();
        self.last_admitted_occurrence = Some(occurrence.identity.occurrence);
        let drop_order_probe =
            self.next_handle_drop_order_probe
                .take()
                .map(|released_after_invalidation| {
                    (Arc::clone(&occurrence.lease), released_after_invalidation)
                });
        self.registrations.push(ProvenanceRegistration {
            generation: self.generation,
            occurrence,
            disposition: RegistrationDisposition::Pending,
            produced_txn_id: None,
            produced_timestamp_nanos: None,
        });
        Ok(ProvenanceHandle {
            session: self.id,
            generation: self.generation,
            index,
            drop_order_probe,
        })
    }

    fn admit_carrier(
        &mut self,
        carrier: PrivateRawFrameCarrier,
    ) -> Result<mt7921_softmac_adapter::PinnedClientRx<ProvenanceHandle>, String> {
        if carrier.occurrence.is_none() {
            self.quarantine.push(carrier);
            self.fail_close();
            return Err("B1 carrier has no occurrence".into());
        }
        let Some(owned) = self.registrations.len().checked_add(self.ingress.len()) else {
            self.quarantine.push(carrier);
            self.fail_close();
            return Err("provenance session arena exhausted".into());
        };
        if owned >= Self::CAPACITY {
            self.quarantine.push(carrier);
            self.fail_close();
            return Err("provenance session arena exhausted".into());
        }
        let PrivateRawFrameCarrier { bytes, occurrence } = carrier;
        let occurrence = occurrence.expect("checked above");
        let handle = self.admit(occurrence)?;
        match mt7921_softmac_adapter::pinned_client_rx_from_connac2(&bytes, handle) {
            Ok(rx) => Ok(rx),
            Err(error) => {
                self.fail_close();
                Err(format!("reject B1 Connac2 carrier: {error:?}"))
            }
        }
    }

    fn validate_index(&self, handle: &ProvenanceHandle) -> Result<usize, String> {
        if self.closed
            || self.poisoned
            || handle.session != self.id
            || handle.generation != self.generation
        {
            return Err("provenance session handle mismatch".into());
        }
        let registration = self
            .registrations
            .get(handle.index)
            .ok_or_else(|| "provenance session index mismatch".to_string())?;
        if registration.generation != self.generation
            || registration.disposition != RegistrationDisposition::Pending
        {
            return Err("provenance registration is stale".into());
        }
        self.source.validate(&registration.occurrence)?;
        Ok(handle.index)
    }

    fn validate_produced(&self, handle: &ProvenanceHandle) -> Result<usize, String> {
        if self.closed
            || self.poisoned
            || handle.session != self.id
            || handle.generation != self.generation
        {
            return Err("provenance session handle mismatch".into());
        }
        let registration = self
            .registrations
            .get(handle.index)
            .ok_or_else(|| "provenance session index mismatch".to_string())?;
        if registration.generation != self.generation
            || registration.disposition != RegistrationDisposition::Produced
        {
            return Err("provenance registration was not produced".into());
        }
        self.source.validate(&registration.occurrence)?;
        Ok(handle.index)
    }

    fn start_sme_scan(
        &mut self,
        request: fidl_fuchsia_wlan_sme::ScanRequest,
    ) -> Result<fidl_fuchsia_wlan_mlme::ScanRequest, String> {
        if self.closed
            || self.poisoned
            || self.outstanding_results != 0
            || self.sme_started
            || self.sme_terminal_accepted
            || self.expected_sme_txn_id.is_some()
        {
            self.fail_close();
            return Err("private SME scan start has nonempty owner state".into());
        }
        self.sme
            .start_trusted_scan(&mut self.sme_state, request)
            .map_err(|error| {
                self.fail_close();
                format!("private SME scan start rejected: {error:?}")
            })?;
        match self.sme_requests.try_recv() {
            Ok(wlan_sme::MlmeRequest::Scan(request)) if self.sme_requests.try_recv().is_err() => {
                self.sme_started = true;
                self.expected_sme_txn_id = Some(request.txn_id);
                Ok(request)
            }
            _ => {
                self.fail_close();
                Err("private SME scan did not emit exactly one scan request".into())
            }
        }
    }

    fn route_next_sme_result(&mut self) -> Result<bool, String> {
        let carried = match self.result_rx.try_recv() {
            Ok(carried) => carried,
            Err(_) => return Ok(false),
        };
        self.outstanding_results = self.outstanding_results.checked_sub(1).ok_or_else(|| {
            self.fail_close();
            "private SME result ledger underflow".to_string()
        })?;
        if self.validate_produced(&carried.provenance).is_err() {
            // `carried` remains in this stack frame: fail-close invalidates its
            // source before the exact result and affine handle are released.
            self.fail_close();
            drop(carried);
            return Err("private SME result owner validation failed".into());
        }
        if std::mem::take(&mut self.panic_next_sme_result_aggregation) {
            self.sme_state.inject_result_aggregation_panic_for_test();
        }
        let routed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.sme.on_trusted_mlme_scan_result(
                carried.result,
                carried.provenance,
                &mut self.sme_state,
            )
        }));
        match routed {
            Ok(Ok(())) => Ok(true),
            Ok(Err(rejected)) => {
                let error = rejected.error();
                // No allocating owner transfer is allowed on rejection. The
                // returned exact pair stays local through synchronous invalidation.
                self.fail_close();
                drop(rejected);
                Err(format!("private SME result routing failed: {error:?}"))
            }
            Err(_) => {
                self.fail_close();
                Err("private SME result routing unwound".into())
            }
        }
    }

    fn finish_sme_scan(&mut self, end: fidl_fuchsia_wlan_mlme::ScanEnd) -> Result<(), String> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.finish_sme_scan_inner(end)
        })) {
            Ok(result) => result,
            Err(_) => {
                self.fail_close();
                Err("private SME terminal validation unwound".into())
            }
        }
    }

    fn finish_sme_scan_inner(
        &mut self,
        end: fidl_fuchsia_wlan_mlme::ScanEnd,
    ) -> Result<(), String> {
        if self.closed
            || self.poisoned
            || self.outstanding_results != 0
            || self.sme_output.is_some()
            || !self.sme_started
            || self.sme_terminal_accepted
            || self.expected_sme_txn_id != Some(end.txn_id)
        {
            self.fail_close();
            return Err("private SME terminal has undrained input or prior output".into());
        }
        let expected_txn_id = end.txn_id;
        let registrations = &self.registrations;
        let source = &self.source;
        let id = self.id;
        let generation = self.generation;
        let reject = std::mem::take(&mut self.reject_next_sme_output);
        let panic_validation = std::mem::take(&mut self.panic_next_sme_validation);
        let terminal = match self.sme.on_trusted_mlme_scan_end(
            end,
            &mut self.sme_state,
            |inputs, aggregates| {
                if panic_validation {
                    panic!("injected private SME terminal validation panic");
                }
                !reject
                    && Self::validate_sme_parts(
                        registrations,
                        source,
                        id,
                        generation,
                        expected_txn_id,
                        inputs,
                        aggregates,
                    )
            },
        ) {
            Ok(terminal) => terminal,
            Err(error) => {
                self.fail_close();
                return Err(format!("private SME terminal rejected: {error:?}"));
            }
        };
        // The owner accepted the borrowed structural view before SME finalized.
        self.sme_output = Some(ValidatedSmeAggregate { terminal });
        self.sme_terminal_accepted = true;
        Ok(())
    }

    fn validate_sme_parts(
        registrations: &[ProvenanceRegistration],
        source: &DescriptorProvenance,
        id: NonZeroU64,
        generation: u64,
        expected_txn_id: u64,
        inputs: &[wlan_sme::client::TrustedScanInput<ProvenanceHandle>],
        aggregates: &[wlan_sme::client::TrustedScanAggregate],
    ) -> bool {
        let produced_indices = registrations
            .iter()
            .enumerate()
            .filter_map(|(index, registration)| {
                (registration.disposition == RegistrationDisposition::Produced).then_some(index)
            })
            .collect::<Vec<_>>();
        if inputs.len() != produced_indices.len() {
            return false;
        }

        // First prove the Produced subsequence is densely bound. Only after
        // this proof may the table index and global Produced ordinal coincide.
        for (ordinal, input) in inputs.iter().enumerate() {
            let provenance = input.provenance();
            if input.table_index() != ordinal
                || input.encounter_index() != ordinal
                || provenance.session != id
                || provenance.generation != generation
                || produced_indices.get(ordinal).copied() != Some(provenance.index)
                || input.original_result().txn_id != expected_txn_id
                || registrations
                    .get(provenance.index)
                    .is_none_or(|registration| {
                        registration.generation != generation
                            || registration.disposition != RegistrationDisposition::Produced
                            || registration.produced_txn_id != Some(input.original_result().txn_id)
                            || registration.produced_timestamp_nanos
                                != Some(input.original_result().timestamp_nanos)
                            || source.validate(&registration.occurrence).is_err()
                    })
            {
                return false;
            }
        }

        let mut aggregate_bssids = std::collections::HashSet::with_capacity(aggregates.len());
        for aggregate in aggregates {
            let bssid = aggregate.lineage().bssid().to_array();
            if aggregate.fixed_bss().bssid != bssid || !aggregate_bssids.insert(bssid) {
                return false;
            }
        }

        let mut roles = vec![0u8; inputs.len()];
        for aggregate in aggregates {
            let lineage = aggregate.lineage();
            let bssid = lineage.bssid().to_array();
            let expected_bssid_subsequence = inputs
                .iter()
                .enumerate()
                .filter_map(|(index, input)| {
                    (input.original_result().bss.bssid == bssid).then_some(index)
                })
                .collect::<Vec<_>>();
            let mut actual_bssid_subsequence = lineage
                .merger_input_indices()
                .iter()
                .chain(lineage.dropped_input_indices())
                .copied()
                .collect::<Vec<_>>();
            if actual_bssid_subsequence
                .iter()
                .any(|&index| index >= inputs.len())
            {
                return false;
            }
            actual_bssid_subsequence.sort_by_key(|&index| inputs[index].encounter_index());
            if actual_bssid_subsequence != expected_bssid_subsequence {
                return false;
            }
            let representative = lineage.representative_index();
            let Some(representative_input) = inputs.get(representative) else {
                return false;
            };
            if !Self::same_fixed_bss(
                aggregate.fixed_bss(),
                &representative_input.original_result().bss,
            ) {
                return false;
            }
            let mut last = None;
            for &index in lineage.merger_input_indices() {
                let Some(input) = inputs.get(index) else {
                    return false;
                };
                if input.original_result().bss.bssid != lineage.bssid().to_array()
                    || last.is_some_and(|previous| previous >= input.encounter_index())
                {
                    return false;
                }
                last = Some(input.encounter_index());
                roles[index] = match roles[index].checked_add(1) {
                    Some(count) => count,
                    None => return false,
                };
            }
            last = None;
            for &index in lineage.dropped_input_indices() {
                let Some(input) = inputs.get(index) else {
                    return false;
                };
                if input.original_result().bss.bssid != lineage.bssid().to_array()
                    || last.is_some_and(|previous| previous >= input.encounter_index())
                {
                    return false;
                }
                last = Some(input.encounter_index());
                roles[index] = match roles[index].checked_add(1) {
                    Some(count) => count,
                    None => return false,
                };
            }
            if lineage.merger_input_indices().last().copied() != Some(representative) {
                return false;
            }
            let Some(occupied_inputs) = lineage
                .merger_input_indices()
                .len()
                .checked_add(lineage.dropped_input_indices().len())
                .and_then(|count| count.checked_sub(1))
            else {
                return false;
            };
            if lineage.occupied_predicate_evaluations() != occupied_inputs {
                return false;
            }
        }
        roles.into_iter().all(|count| count == 1)
    }

    fn same_fixed_bss(
        actual: &fidl_fuchsia_wlan_ieee80211::BssDescription,
        representative: &fidl_fuchsia_wlan_ieee80211::BssDescription,
    ) -> bool {
        actual.bssid == representative.bssid
            && actual.bss_type == representative.bss_type
            && actual.beacon_period == representative.beacon_period
            && actual.capability_info == representative.capability_info
            && actual.primary == representative.primary
            && actual.bandwidth == representative.bandwidth
            && actual.vht_secondary_80_channel == representative.vht_secondary_80_channel
            && actual.rssi_dbm == representative.rssi_dbm
            && actual.snr_db == representative.snr_db
    }

    fn inspect_sme_output(&self, inspect: impl FnOnce(&ValidatedSmeAggregate)) -> bool {
        if !self.sme_terminal_accepted {
            return false;
        }
        let Some(output) = self.sme_output.as_ref() else {
            return false;
        };
        inspect(output);
        true
    }

    fn inspect_next_result(&mut self, inspect: impl FnOnce(&CarriedScanResult)) -> bool {
        let Ok(carried) = self.result_rx.try_recv() else {
            return false;
        };
        self.carried_results.push(carried);
        self.outstanding_results -= 1;
        inspect(self.carried_results.last().expect("just pushed"));
        true
    }

    fn advance_generation(&mut self) {
        self.generation = self.generation.checked_add(1).unwrap_or_else(|| {
            self.poisoned = true;
            u64::MAX
        });
    }

    fn fail_close(&mut self) {
        if self.closed {
            return;
        }
        self.poisoned = true;
        // Invalidate while ingress, canonical registrations, quarantine, and
        // carried results are all still owned by this session.
        self.source.invalidate(DescriptorInvalidation::Run).ok();
        self.invalidated = true;
        self.result_tx.take();
        self.sme_output.take();
        self.sme_state = wlan_sme::client::TrustedScanState::new(self.generation);
        self.sme_started = false;
        self.sme_terminal_accepted = false;
        self.expected_sme_txn_id = None;
        while self.result_rx.try_recv().is_ok() {}
        self.outstanding_results = 0;
        self.next_observe_index = 0;
        self.last_admitted_occurrence = None;
        self.carried_results.clear();
        self.ingress.clear();
        self.registrations.clear();
        self.quarantine.clear();
        self.observed_order.clear();
        self.closed = true;
        self.advance_generation();
    }

    fn finish(&mut self) -> Result<(), String> {
        if self.closed || self.poisoned {
            return Err("provenance session cannot finish".into());
        }
        if !self.ingress.is_empty()
            || !self.quarantine.is_empty()
            || self.outstanding_results != 0
            || self.next_observe_index != self.registrations.len()
            || self.sme_started != self.sme_terminal_accepted
            || self
                .registrations
                .iter()
                .any(|registration| registration.disposition == RegistrationDisposition::Pending)
        {
            self.fail_close();
            return Err("provenance session has unclassified or undrained state".into());
        }
        // Exclusive &mut access proves no observer can race this retirement.
        // Every never-reused arena index is retired exactly once here.
        for registration in &self.registrations {
            self.source.retire(&registration.occurrence);
        }
        // Carried results remain session-owned until after their registrations
        // have been retired under this exclusive access.
        self.carried_results.clear();
        self.sme_output.take();
        self.registrations.clear();
        self.observed_order.clear();
        self.last_admitted_occurrence = None;
        self.result_tx.take();
        self.closed = true;
        self.advance_generation();
        Ok(())
    }
}

#[cfg(all(test, feature = "fuchsia-passive"))]
impl wlan_mlme::ScanResultObserver<ProvenanceHandle> for ProvenanceSession {
    fn observe(
        &mut self,
        disposition: &wlan_mlme::ScanResultDisposition<'_>,
        provenance: ProvenanceHandle,
    ) -> wlan_mlme::ScanResultObserverControl {
        let Ok(index) = self.validate_index(&provenance) else {
            self.fail_close();
            return wlan_mlme::ScanResultObserverControl::Suppress;
        };
        if index != self.next_observe_index {
            self.fail_close();
            return wlan_mlme::ScanResultObserverControl::Suppress;
        }
        let Some(next_observe_index) = self.next_observe_index.checked_add(1) else {
            self.fail_close();
            return wlan_mlme::ScanResultObserverControl::Suppress;
        };
        self.next_observe_index = next_observe_index;
        self.observed_order
            .push(self.registrations[index].occurrence.identity.occurrence);
        let produced_metadata = match disposition {
            wlan_mlme::ScanResultDisposition::Produced(result) => {
                Some((result.txn_id, result.timestamp_nanos))
            }
            _ => None,
        };
        let classification = match disposition {
            wlan_mlme::ScanResultDisposition::Produced(result) => {
                let carried = CarriedScanResult {
                    provenance,
                    result: (**result).clone(),
                };
                let Some(tx) = self.result_tx.as_ref() else {
                    self.fail_close();
                    drop(carried);
                    return wlan_mlme::ScanResultObserverControl::Suppress;
                };
                if let Err(rejected) = tx.unbounded_send(carried) {
                    // The send error owns the affine handle. Retain it across
                    // fail-close so source invalidation precedes its release.
                    self.fail_close();
                    drop(rejected);
                    return wlan_mlme::ScanResultObserverControl::Suppress;
                }
                self.outstanding_results += 1;
                RegistrationDisposition::Produced
            }
            wlan_mlme::ScanResultDisposition::Ignored(reason) => {
                RegistrationDisposition::Ignored(*reason)
            }
            wlan_mlme::ScanResultDisposition::ConversionDrop => {
                RegistrationDisposition::ConversionDrop
            }
        };
        self.registrations[index].disposition = classification;
        if let Some((txn_id, timestamp_nanos)) = produced_metadata {
            self.registrations[index].produced_txn_id = Some(txn_id);
            self.registrations[index].produced_timestamp_nanos = Some(timestamp_nanos);
        }
        if classification == RegistrationDisposition::Produced {
            wlan_mlme::ScanResultObserverControl::Suppress
        } else {
            wlan_mlme::ScanResultObserverControl::Continue
        }
    }

    fn observe_transport_failure(&mut self) {
        self.fail_close();
    }
}

#[cfg(all(test, feature = "fuchsia-passive"))]
impl Drop for ProvenanceSession {
    fn drop(&mut self) {
        // Drop bodies run before fields: invalidate every B1 occurrence before
        // the queue, arena registrations, or source provenance are released.
        if !self.closed {
            self.fail_close();
        }
    }
}

impl DescriptorOccurrence {
    fn is_current(&self) -> bool {
        self.lease.owner == self.identity.owner && self.lease.current.load(Ordering::Acquire)
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum DescriptorProvenanceEffect {
    Mint(DescriptorOccurrenceIdentity),
    Rearm {
        route: DescriptorOccurrenceRoute,
        ring: usize,
        slot: usize,
        slot_epoch: u64,
    },
    Invalidate(DescriptorInvalidation),
    Poison,
    DropUncovered,
    DescriptorWrite,
    ReleaseFence,
    IndexPublish,
}

struct DescriptorProvenance {
    owner: NonZeroU64,
    interface_epoch: u64,
    device_epoch: u64,
    reset_epoch: u64,
    ownership_epoch: u64,
    run_epoch: u64,
    scan_id: u64,
    scan_epoch: u64,
    next_occurrence: u64,
    rings: Vec<DescriptorRingProvenance>,
    sealed: Vec<DescriptorOccurrenceIdentity>,
    revoked: bool,
    poisoned: bool,
    lease: Arc<DescriptorOccurrenceLease>,
    #[cfg(test)]
    effects: Vec<DescriptorProvenanceEffect>,
}

struct PrivateRawFrameCarrier {
    bytes: Vec<u8>,
    occurrence: Option<DescriptorOccurrence>,
}

#[cfg(feature = "fuchsia-passive")]
struct PrivateRawAdvertisementCarrier {
    advertisement: mt7921_port_spike::PassiveAdvertisement,
    frame_bytes: Vec<u8>,
    occurrence: Option<DescriptorOccurrence>,
}

enum PrivateFrameSeal {
    Carried(PrivateRawFrameCarrier),
    Uncovered(Vec<u8>),
}

static NEXT_DESCRIPTOR_PROVENANCE_OWNER: AtomicU64 = AtomicU64::new(1);

fn next_descriptor_provenance_owner() -> Result<NonZeroU64, String> {
    let owner = NEXT_DESCRIPTOR_PROVENANCE_OWNER
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |owner| {
            owner.checked_add(1)
        })
        .map_err(|_| "descriptor provenance owner identity exhausted")?;
    NonZeroU64::new(owner).ok_or_else(|| "descriptor provenance owner identity wrapped".into())
}

impl DescriptorProvenance {
    fn new() -> Self {
        Self::from_owner_allocation(next_descriptor_provenance_owner())
    }

    fn from_owner_allocation(owner: Result<NonZeroU64, String>) -> Self {
        let exhausted = owner.is_err();
        // MAX is never returned by the allocator: reaching it makes
        // `fetch_update` fail. It is only a non-minting poisoned sentinel.
        let owner = owner.unwrap_or(NonZeroU64::MAX);
        let ring = |route, ring| DescriptorRingProvenance {
            route,
            ring,
            slots: (0..8)
                .map(|slot| {
                    if slot < 7 {
                        DescriptorSlotState::Armed(1)
                    } else {
                        DescriptorSlotState::Vacant(0)
                    }
                })
                .collect(),
        };
        let lease = Arc::new(DescriptorOccurrenceLease {
            owner,
            current: AtomicBool::new(!exhausted),
        });
        Self {
            owner,
            interface_epoch: 1,
            device_epoch: 1,
            reset_epoch: 1,
            ownership_epoch: 1,
            run_epoch: 1,
            scan_id: 0,
            scan_epoch: 1,
            next_occurrence: 0,
            rings: vec![
                ring(DescriptorOccurrenceRoute::McuNormalRx, 0),
                ring(DescriptorOccurrenceRoute::McuNormalRx, 4),
                ring(DescriptorOccurrenceRoute::DataRx, 2),
            ],
            sealed: Vec::new(),
            revoked: exhausted,
            poisoned: exhausted,
            lease,
            #[cfg(test)]
            effects: exhausted
                .then_some(DescriptorProvenanceEffect::Poison)
                .into_iter()
                .collect(),
        }
    }

    fn ring_mut(
        &mut self,
        route: DescriptorOccurrenceRoute,
        ring: usize,
    ) -> Option<&mut DescriptorRingProvenance> {
        self.rings
            .iter_mut()
            .find(|candidate| candidate.route == route && candidate.ring == ring)
    }

    fn poison<T>(&mut self) -> Result<T, String> {
        self.sealed.clear();
        self.lease.current.store(false, Ordering::Release);
        self.poisoned = true;
        self.revoked = true;
        #[cfg(test)]
        self.effects.push(DescriptorProvenanceEffect::Poison);
        Err("descriptor provenance exhausted and was poisoned".into())
    }

    fn seal_frame(
        &mut self,
        route: DescriptorOccurrenceRoute,
        ring: usize,
        slot: usize,
        bytes: Vec<u8>,
    ) -> Result<PrivateFrameSeal, String> {
        if self.poisoned || self.revoked {
            #[cfg(test)]
            self.effects.push(DescriptorProvenanceEffect::DropUncovered);
            return Ok(PrivateFrameSeal::Uncovered(bytes));
        }
        if self.ring_mut(route, ring).is_none() {
            #[cfg(test)]
            self.effects.push(DescriptorProvenanceEffect::DropUncovered);
            return Ok(PrivateFrameSeal::Uncovered(bytes));
        }
        let slot_epoch = match self
            .ring_mut(route, ring)
            .and_then(|ring| ring.slots.get(slot).copied())
        {
            Some(DescriptorSlotState::Armed(epoch)) => epoch,
            Some(DescriptorSlotState::Vacant(_) | DescriptorSlotState::Consumed(_)) => {
                #[cfg(test)]
                self.effects.push(DescriptorProvenanceEffect::DropUncovered);
                return Ok(PrivateFrameSeal::Uncovered(bytes));
            }
            None => {
                #[cfg(test)]
                self.effects.push(DescriptorProvenanceEffect::DropUncovered);
                return Ok(PrivateFrameSeal::Uncovered(bytes));
            }
        };
        let Some(occurrence) = self.next_occurrence.checked_add(1) else {
            let _ = self.poison::<()>();
            return Ok(PrivateFrameSeal::Uncovered(bytes));
        };
        self.next_occurrence = occurrence;
        self.ring_mut(route, ring)
            .expect("ring checked above")
            .slots[slot] = DescriptorSlotState::Consumed(slot_epoch);
        let identity = DescriptorOccurrenceIdentity {
            owner: self.owner,
            interface_epoch: self.interface_epoch,
            device_epoch: self.device_epoch,
            reset_epoch: self.reset_epoch,
            ownership_epoch: self.ownership_epoch,
            run_epoch: self.run_epoch,
            scan_id: self.scan_id,
            scan_epoch: self.scan_epoch,
            route,
            ring,
            slot,
            slot_epoch,
            occurrence,
        };
        #[cfg(test)]
        self.effects
            .push(DescriptorProvenanceEffect::Mint(identity));
        self.sealed.push(identity);
        Ok(PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            bytes,
            occurrence: Some(DescriptorOccurrence {
                identity,
                lease: Arc::clone(&self.lease),
            }),
        }))
    }

    fn rearm(
        &mut self,
        route: DescriptorOccurrenceRoute,
        ring: usize,
        slot: usize,
    ) -> Result<(), String> {
        if self.poisoned || self.revoked {
            return Ok(());
        }
        let previous = match self
            .ring_mut(route, ring)
            .and_then(|ring| ring.slots.get(slot).copied())
        {
            Some(DescriptorSlotState::Vacant(epoch) | DescriptorSlotState::Consumed(epoch)) => {
                epoch
            }
            Some(DescriptorSlotState::Armed(_)) => {
                let _ = self.poison::<()>();
                return Ok(());
            }
            None => return Ok(()),
        };
        let Some(slot_epoch) = previous.checked_add(1) else {
            let _ = self.poison::<()>();
            return Ok(());
        };
        self.ring_mut(route, ring)
            .expect("ring checked above")
            .slots[slot] = DescriptorSlotState::Armed(slot_epoch);
        #[cfg(test)]
        self.effects.push(DescriptorProvenanceEffect::Rearm {
            route,
            ring,
            slot,
            slot_epoch,
        });
        Ok(())
    }

    fn consume_without_mint(&mut self, route: DescriptorOccurrenceRoute, ring: usize, slot: usize) {
        if self.poisoned || self.revoked {
            return;
        }
        let Some(state) = self
            .ring_mut(route, ring)
            .and_then(|ring| ring.slots.get(slot).copied())
        else {
            return;
        };
        match state {
            DescriptorSlotState::Armed(epoch) => {
                self.ring_mut(route, ring).expect("ring found above").slots[slot] =
                    DescriptorSlotState::Consumed(epoch);
            }
            DescriptorSlotState::Vacant(_) | DescriptorSlotState::Consumed(_) => {
                let _ = self.poison::<()>();
            }
        }
    }

    fn validate(&self, occurrence: &DescriptorOccurrence) -> Result<(), String> {
        let identity = &occurrence.identity;
        if self.poisoned
            || self.revoked
            || !occurrence.is_current()
            || identity.owner != self.owner
            || identity.interface_epoch != self.interface_epoch
            || identity.device_epoch != self.device_epoch
            || identity.reset_epoch != self.reset_epoch
            || identity.ownership_epoch != self.ownership_epoch
            || identity.run_epoch != self.run_epoch
        {
            return Err("descriptor occurrence was revoked".into());
        }
        if self.sealed.contains(identity) {
            Ok(())
        } else {
            Err("descriptor occurrence is stale".into())
        }
    }

    fn retire(&mut self, occurrence: &DescriptorOccurrence) {
        if occurrence.identity.owner != self.owner {
            return;
        }
        if let Some(index) = self
            .sealed
            .iter()
            .position(|identity| *identity == occurrence.identity)
        {
            self.sealed.swap_remove(index);
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    fn begin_scan(&mut self, scan_id: u64) {
        if self.poisoned || self.revoked {
            return;
        }
        let Some(scan_epoch) = self.scan_epoch.checked_add(1) else {
            let _ = self.poison::<()>();
            return;
        };
        self.scan_id = scan_id;
        self.scan_epoch = scan_epoch;
    }

    fn invalidate(&mut self, reason: DescriptorInvalidation) -> Result<(), String> {
        if self.poisoned || self.revoked {
            return Ok(());
        }
        let next = match reason {
            DescriptorInvalidation::Interface => self.interface_epoch.checked_add(1),
            DescriptorInvalidation::Cancellation => self.scan_epoch.checked_add(1),
            DescriptorInvalidation::Teardown | DescriptorInvalidation::Run => {
                self.run_epoch.checked_add(1)
            }
        };
        let Some(next) = next else {
            let _ = self.poison::<()>();
            return Ok(());
        };
        match reason {
            DescriptorInvalidation::Interface => self.interface_epoch = next,
            DescriptorInvalidation::Cancellation => self.scan_epoch = next,
            DescriptorInvalidation::Teardown | DescriptorInvalidation::Run => self.run_epoch = next,
        }
        self.sealed.clear();
        self.lease.current.store(false, Ordering::Release);
        self.revoked = true;
        #[cfg(test)]
        self.effects
            .push(DescriptorProvenanceEffect::Invalidate(reason));
        Ok(())
    }
}

impl Drop for DescriptorProvenance {
    fn drop(&mut self) {
        revoke_descriptor_provenance_before_release(self);
    }
}

fn revoke_descriptor_provenance_before_release(provenance: &mut DescriptorProvenance) {
    let _ = provenance.invalidate(DescriptorInvalidation::Teardown);
}

fn revoke_before_local_carrier_release<T>(
    provenance: &mut DescriptorProvenance,
    queued: &mut Vec<T>,
) -> Result<(), String> {
    provenance.invalidate(DescriptorInvalidation::Run)?;
    queued.clear();
    Ok(())
}

fn observe_signal_cancellation(
    provenance: &mut DescriptorProvenance,
    stop_requested: bool,
) -> Result<(), String> {
    if stop_requested {
        provenance.invalidate(DescriptorInvalidation::Cancellation)?;
    }
    Ok(())
}

#[cfg(feature = "fuchsia-passive")]
fn observe_passive_command_provenance(
    provenance: &mut DescriptorProvenance,
    command: &PassiveMcuCommand,
) -> Result<(), String> {
    match command {
        PassiveMcuCommand::CancelScan { .. } => {
            provenance.invalidate(DescriptorInvalidation::Cancellation)
        }
        PassiveMcuCommand::StartScan { scan_sequence, .. } => {
            provenance.begin_scan(u64::from(*scan_sequence));
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(feature = "fuchsia-passive")]
impl PrivateRawFrameCarrier {
    fn parse(self) -> Result<PrivateRawAdvertisementCarrier, (Self, String)> {
        let advertisement = match parse_passive_advertisement(&self.bytes) {
            Ok(advertisement) => advertisement,
            Err(error) => {
                return Err((self, format!("reject routed passive RX frame: {error:?}")));
            }
        };
        Ok(PrivateRawAdvertisementCarrier {
            advertisement,
            frame_bytes: self.bytes,
            occurrence: self.occurrence,
        })
    }
}

#[cfg(feature = "fuchsia-passive")]
impl PrivateRawAdvertisementCarrier {
    fn into_unprovenanced(
        self,
        provenance: &mut DescriptorProvenance,
    ) -> mt7921_port_spike::PassiveAdvertisement {
        let Self {
            advertisement,
            frame_bytes: _,
            occurrence,
        } = self;
        if let Some(occurrence) = occurrence.as_ref() {
            provenance.retire(occurrence);
        }
        advertisement
    }
}

struct ActiveMcuIo<'a> {
    wfdma: &'a ReadPage,
    irq: &'a mut VfioIrq,
    signal: &'a ActiveSignalGuard,
    tx_ring: &'a mut DmaArena,
    payload: &'a mut DmaArena,
    wm: ActiveMcuRx<'a>,
    wm2: Option<ActiveMcuRx<'a>>,
    extra_irq_mask: u32,
    unsolicited: Vec<ReceivedMcuResponse>,
    normal_rx_frames: Vec<PrivateRawFrameCarrier>,
    descriptor_provenance: DescriptorProvenance,
}

impl Drop for ActiveMcuIo<'_> {
    fn drop(&mut self) {
        // Drop bodies run before fields. Revoke sealed identities before
        // `normal_rx_frames` or any DMA/resource field can be released.
        let _ = self
            .descriptor_provenance
            .invalidate(DescriptorInvalidation::Interface);
    }
}

struct VfioFirmwareLoader<'a> {
    mcu: ActiveMcuIo<'a>,
    conn: &'a ReadPage,
    pcie_mac: &'a ReadPage,
    device: &'a File,
    bdf: &'a str,
    fwdl_ring: &'a mut DmaArena,
    fwdl_payload: &'a mut DmaArena,
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
    #[cfg(feature = "fuchsia-passive")]
    DataBuffers,
    #[cfg(feature = "fuchsia-passive")]
    DataRing,
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
const DATA_RX_IRQ_BIT: u32 = 1 << 2;
const WM2_RX_IRQ_BIT: u32 = 1 << 22;
#[cfg(feature = "fuchsia-passive")]
const PASSIVE_MAC_BAR_PAGES: [usize; 8] = [
    0x0f000, 0x21000, 0x23000, 0x24000, 0x34000, 0xa1000, 0xa3000, 0xa4000,
];

const fn firmware_bootstrap_rx_irq_mask() -> u32 {
    WM_RX_IRQ_BIT | WM2_RX_IRQ_BIT
}

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

const fn active_wfdma_write_allowed(offset: usize, value: u32, rx_irq_mask: u32) -> bool {
    match offset {
        0xd4200 => value & !rx_irq_mask == 0,
        0xd4204 => {
            value == 0
                || value == WM_RX_IRQ_BIT
                || value == WM2_RX_IRQ_BIT
                || value == (WM_RX_IRQ_BIT | WM2_RX_IRQ_BIT)
                || (rx_irq_mask & DATA_RX_IRQ_BIT != 0 && value == rx_irq_mask)
        }
        0xd4208 | 0xd4100 | 0xd42b0 => true,
        0xd42f0 => value == 0 || value == 4,
        0xd4680 => value == 4,
        0xd4690 => value == 0x00c0_0004,
        0xd4640 => value == 0x0340_0004,
        0xd4644 => value == 0x0380_0004,
        0xd4600 => value == 0x0140_0004,
        0xd4308 | 0xd4408 => value < 128,
        0xd4418 => value < 256,
        _ => false,
    }
}

#[cfg(feature = "fuchsia-passive")]
fn passive_mac_address_allowed(address: u32) -> bool {
    passive_mac_mmio_plan()
        .iter()
        .any(|operation| match operation {
            PassiveMacMmioOperation::Rmw {
                address: expected, ..
            }
            | PassiveMacMmioOperation::WtblClear {
                address: expected, ..
            } => *expected == address,
        })
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

fn publish_descriptor_rearm(
    provenance: &mut DescriptorProvenance,
    route: DescriptorOccurrenceRoute,
    ring: usize,
    slot: usize,
    publish: impl FnOnce(&mut DescriptorProvenance) -> Result<(), String>,
) -> Result<(), String> {
    provenance.rearm(route, ring, slot)?;
    publish(provenance)
}

fn publish_current_rearm_or_revoke<T>(
    provenance: &mut DescriptorProvenance,
    route: DescriptorOccurrenceRoute,
    ring: usize,
    slot: usize,
    current: T,
    refill: Result<DmaDescriptor, String>,
    publish: impl FnOnce(&mut DescriptorProvenance, DmaDescriptor) -> Result<(), String>,
) -> Result<T, String> {
    let refill = match refill {
        Ok(refill) => refill,
        Err(error) => {
            provenance.invalidate(DescriptorInvalidation::Run)?;
            return Err(error);
        }
    };
    if let Err(error) = publish_descriptor_rearm(provenance, route, ring, slot, |provenance| {
        publish(provenance, refill)
    }) {
        provenance.invalidate(DescriptorInvalidation::Run)?;
        return Err(error);
    }
    Ok(current)
}

enum DrainedMcuRx {
    Normal(PrivateRawFrameCarrier),
    Response(Option<mt7921_port_spike::DownloadResponse>, Vec<u8>),
}

fn drain_rx_queue(
    wfdma: &ReadPage,
    queue: &mut ActiveMcuRx<'_>,
    expected_sequence: Option<u8>,
    unsolicited: &mut Vec<ReceivedMcuResponse>,
    normal_rx_frames: &mut Vec<PrivateRawFrameCarrier>,
    provenance: &mut DescriptorProvenance,
) -> Result<Option<ReceivedMcuResponse>, String> {
    let result = (|| -> Result<Option<ReceivedMcuResponse>, String> {
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
                provenance.consume_without_mint(
                    DescriptorOccurrenceRoute::McuNormalRx,
                    queue.rx_ring_index,
                    completed_index,
                );
                Err("fragmented MCU RX descriptor is unsupported".into())
            } else if !(36..=2048).contains(&response_len) {
                provenance.consume_without_mint(
                    DescriptorOccurrenceRoute::McuNormalRx,
                    queue.rx_ring_index,
                    completed_index,
                );
                Err(format!(
                    "invalid MCU response descriptor length {response_len}"
                ))
            } else {
                let response = queue
                    .rx_buffers
                    .read_bytes(completed_index * 2048, response_len)?;
                let actual_sequence = response[29];
                let header_length = response
                    .get(24..26)
                    .map(|bytes| u16::from_le_bytes(bytes.try_into().expect("fixed field")));
                let rxd0 = u32::from_le_bytes(response[0..4].try_into().expect("bounded response"));
                let packet_type = (rxd0 >> 27) & 0x1f;
                let packet_flag = (rxd0 >> 16) & 0x0f;
                if packet_type == 7 && packet_flag == 1 {
                    match provenance.seal_frame(
                        DescriptorOccurrenceRoute::McuNormalRx,
                        queue.rx_ring_index,
                        completed_index,
                        response,
                    )? {
                        PrivateFrameSeal::Carried(frame) => Ok(DrainedMcuRx::Normal(frame)),
                        PrivateFrameSeal::Uncovered(bytes) => {
                            Ok(DrainedMcuRx::Normal(PrivateRawFrameCarrier {
                                bytes,
                                occurrence: None,
                            }))
                        }
                    }
                } else {
                    provenance.consume_without_mint(
                        DescriptorOccurrenceRoute::McuNormalRx,
                        queue.rx_ring_index,
                        completed_index,
                    );
                    match parse_download_response(&response, actual_sequence) {
                        Ok(parsed) => Ok(DrainedMcuRx::Response(Some(parsed), response)),
                        Err(error) => {
                            let prefix = response
                                .iter()
                                .take(64)
                                .map(|byte| format!("{byte:02x}"))
                                .collect::<String>();
                            Err(format!(
                                "parse MCU response: {error:?}; rx_ring={} descriptor={} ctrl={:#010x} descriptor_length={} header_length={header_length:?} prefix={prefix}",
                                queue.rx_ring_index, completed_index, descriptor.ctrl, response_len
                            ))
                        }
                    }
                }
            };

            let refill_index = queue.rx_head;
            let refill = DmaDescriptor::rx(DmaSegment {
                iova: queue.rx_buffers.iova + (refill_index * 2048) as u64,
                len: 2048,
            })
            .map_err(|error| format!("rearm MCU RX descriptor: {error:?}"));
            let parsed = publish_current_rearm_or_revoke(
                provenance,
                DescriptorOccurrenceRoute::McuNormalRx,
                queue.rx_ring_index,
                refill_index,
                parsed,
                refill,
                |_, refill| {
                    queue.rx_ring.write_descriptor_at(refill_index, refill);
                    std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
                    queue.rx_head = next_dma_index(queue.rx_head, queue.rx_count);
                    wfdma.write_rx_cpu_index(queue.rx_ring_index, queue.rx_head as u32)
                },
            )?;
            queue.rx_tail = next_dma_index(queue.rx_tail, queue.rx_count);

            let (parsed, response) = match parsed? {
                DrainedMcuRx::Normal(frame) => {
                    println!(
                        "{{\"active_mcu_event\":\"normal_rx_routed\",\"rx_ring\":{},\"rx_descriptor\":{completed_index},\"length\":{response_len}}}",
                        queue.rx_ring_index
                    );
                    normal_rx_frames.push(frame);
                    continue;
                }
                DrainedMcuRx::Response(parsed, response) => (parsed, response),
            };
            let Some(parsed) = parsed else {
                continue;
            };
            let candidate = response_for_sequence(expected_sequence, parsed, response.clone());
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
                if parsed.sequence == 0 {
                    unsolicited.push(ReceivedMcuResponse {
                        event_id: parsed.event_id,
                        option: parsed.option,
                        bytes: response.clone(),
                    });
                }
                println!(
                    "{{\"active_mcu_event\":\"unrelated_rx_drained\",\"sequence\":{},\"event_id\":{},\"rx_ring\":{},\"rx_descriptor\":{completed_index}}}",
                    parsed.sequence, parsed.event_id, queue.rx_ring_index
                );
            }
        }
        Ok(matched)
    })();
    if result.is_err() {
        revoke_before_local_carrier_release(provenance, normal_rx_frames)?;
    }
    result
}

impl ActiveMcuIo<'_> {
    fn rx_irq_mask(&self) -> u32 {
        self.wm.irq_bit | self.wm2.as_ref().map_or(0, |queue| queue.irq_bit) | self.extra_irq_mask
    }

    fn cancelled(&mut self) -> Result<(), String> {
        let stop_requested = self.signal.stop_requested();
        observe_signal_cancellation(&mut self.descriptor_provenance, stop_requested)?;
        if stop_requested {
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
        observe_signal_cancellation(
            &mut self.descriptor_provenance,
            self.signal.stop_requested(),
        )?;
        let result = (|| -> Result<Option<ReceivedMcuResponse>, String> {
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
            let mut matched = drain_rx_queue(
                self.wfdma,
                &mut self.wm,
                expected_sequence,
                &mut self.unsolicited,
                &mut self.normal_rx_frames,
                &mut self.descriptor_provenance,
            )?;
            if let Some(wm2) = self.wm2.as_mut() {
                let wm2_match = drain_rx_queue(
                    self.wfdma,
                    wm2,
                    expected_sequence,
                    &mut self.unsolicited,
                    &mut self.normal_rx_frames,
                    &mut self.descriptor_provenance,
                )?;
                merge_matching_response(&mut matched, wm2_match)?;
            }
            self.wfdma.write_active_wfdma(0xd4204, irq_mask)?;
            Ok(matched)
        })();
        if result.is_err() {
            revoke_before_local_carrier_release(
                &mut self.descriptor_provenance,
                &mut self.normal_rx_frames,
            )?;
        }
        result
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

#[cfg(feature = "fuchsia-passive")]
impl VfioFirmwareLoader<'_> {
    fn send_passive_command(
        &mut self,
        command: &PassiveMcuCommand,
        encoded: &[u8],
        wait_response: bool,
    ) -> Result<(), String> {
        self.mcu.cancelled()?;
        let sequence = *encoded
            .get(39)
            .filter(|sequence| (1..=15).contains(*sequence))
            .ok_or("passive command omitted valid sequence")?;
        if wait_response != command.expects_response() {
            return Err("passive response policy disagreed with encoded command".into());
        }
        let descriptor_index = self.command_index;
        let next = next_dma_index(descriptor_index, MCU_TX_RING_COUNT);
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
        if wait_response {
            let response = self
                .mcu
                .wait_response(sequence, Instant::now() + std::time::Duration::from_secs(3))?;
            if response.option & (1 << 2) != 0 {
                return Err("passive command response was unsolicited".into());
            }
            match command {
                PassiveMcuCommand::AddDevice { .. } | PassiveMcuCommand::AddBss => {
                    let expected_cid = if matches!(command, PassiveMcuCommand::AddDevice { .. }) {
                        1
                    } else {
                        2
                    };
                    let body = response
                        .bytes
                        .get(36..44)
                        .ok_or("unified passive response omitted result")?;
                    let status = u32::from_le_bytes(body[4..8].try_into().expect("fixed field"));
                    if response.event_id != 1 || body[0] != expected_cid || status != 0 {
                        return Err(format!(
                            "unified passive response mismatch: eid={} cid={} status={status}",
                            response.event_id, body[0]
                        ));
                    }
                }
                _ => {}
            }
        } else {
            let deadline = Instant::now() + std::time::Duration::from_secs(1);
            loop {
                self.mcu.cancelled()?;
                let _ = self.mcu.handle_irq(None)?;
                if dma_index_completed(self.mcu.wfdma.read(0xd441c)?, next as u32) {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "passive command TX completion timed out at descriptor {descriptor_index}"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        self.mcu
            .tx_ring
            .write_descriptor_at(descriptor_index, DmaDescriptor::reset());
        self.mcu.payload.zero_bytes(MCU_COMMAND_PAYLOAD_BYTES)?;
        println!(
            r#"{{"passive_scan_event":"command_completed","command":"{command:?}","sequence":{sequence}}}"#
        );
        Ok(())
    }

    fn send_rate_power_bytes(&mut self, encoded: &[u8]) -> Result<(), String> {
        self.mcu.cancelled()?;
        let _template_sequence = *encoded
            .get(39)
            .filter(|sequence| (1..=15).contains(*sequence))
            .ok_or("rate-power command omitted valid sequence")?;
        if encoded.get(36..39) != Some(&[0x5d, 0xa0, 1]) {
            return Err("rate-power command escaped CE SET_RATE_TX_POWER".into());
        }
        self.sequence = self.sequence % 15 + 1;
        let sequence = self.sequence;
        let mut encoded = encoded.to_vec();
        encoded[39] = sequence;
        let descriptor_index = self.command_index;
        let next = next_dma_index(descriptor_index, MCU_TX_RING_COUNT);
        publish_mcu_bytes(
            self.mcu.wfdma,
            self.mcu.tx_ring,
            self.mcu.payload,
            &encoded,
            sequence,
            descriptor_index,
        )?;
        self.command_index = next;
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        loop {
            self.mcu.cancelled()?;
            let _ = self.mcu.handle_irq(None)?;
            if dma_index_completed(self.mcu.wfdma.read(0xd441c)?, next as u32) {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "rate-power command DMA consumption timed out at descriptor {descriptor_index}"
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        self.mcu
            .tx_ring
            .write_descriptor_at(descriptor_index, DmaDescriptor::reset());
        self.mcu.payload.zero_bytes(MCU_COMMAND_PAYLOAD_BYTES)?;
        Ok(())
    }

    fn query_pse_base(&mut self) -> Result<u32, String> {
        self.mcu.cancelled()?;
        self.mcu
            .wfdma
            .write_active_wfdma(0xd4204, self.mcu.rx_irq_mask())?;
        self.sequence = self.sequence % 15 + 1;
        let sequence = self.sequence;
        let encoded = encode_pse_reg_read_command(sequence)
            .map_err(|error| format!("encode PSE REG_READ: {error:?}"))?;
        let descriptor_index = self.command_index;
        let next = next_dma_index(descriptor_index, MCU_TX_RING_COUNT);
        publish_mcu_bytes(
            self.mcu.wfdma,
            self.mcu.tx_ring,
            self.mcu.payload,
            &encoded,
            sequence,
            descriptor_index,
        )?;
        self.command_index = next;
        let response = self
            .mcu
            .wait_response(sequence, Instant::now() + std::time::Duration::from_secs(3))?;
        let value =
            parse_pse_reg_read_response(response.event_id, response.option, &response.bytes)
                .map_err(|error| {
                    format!(
                        "parse PSE REG_READ response eid={} option={:#04x}: {error:?}",
                        response.event_id, response.option
                    )
                })?;
        self.mcu
            .tx_ring
            .write_descriptor_at(descriptor_index, DmaDescriptor::reset());
        self.mcu.payload.zero_bytes(MCU_COMMAND_PAYLOAD_BYTES)?;
        Ok(value)
    }
}

#[cfg(feature = "fuchsia-passive")]
struct VfioRateTxPower<'x, 'a> {
    loader: &'x mut VfioFirmwareLoader<'a>,
}

#[cfg(feature = "fuchsia-passive")]
impl RateTxPowerTransport for VfioRateTxPower<'_, '_> {
    type Error = String;

    fn send_and_wait_consumed(&mut self, encoded: &[u8]) -> Result<(), Self::Error> {
        self.loader.send_rate_power_bytes(encoded)
    }

    fn query_pse_base(&mut self) -> Result<u32, Self::Error> {
        self.loader.query_pse_base()
    }
}

#[cfg(feature = "fuchsia-passive")]
fn program_live_rate_power(
    mechanics: &mut VfioPassiveMechanics<'_, '_, '_>,
    capability: mt7921_port_spike::NicCapability,
) -> Result<(), String> {
    let mut transport = VfioRateTxPower {
        loader: &mut *mechanics.loader,
    };
    let mut authorizer = RateTxPowerAuthorizer::new();
    let authorization = authorizer
        .submit(
            &mut transport,
            capability,
            ConservativePowerLimits {
                alpha2: *b"00",
                max_reg_power_dbm: 20,
                sar_limit_half_dbm: Some(40),
                external_safety_cap_half_dbm: Some(0),
            },
            1,
        )
        .map_err(|error| format!("submit rate-power setup: {error:?}"))?;
    authorizer
        .permits(&authorization)
        .then_some(())
        .ok_or_else(|| "rate-power authorization is not live".into())
}

impl FirmwareLoaderTransport for VfioFirmwareLoader<'_> {
    type Error = String;

    fn next_sequence(&mut self) -> u8 {
        self.sequence = (self.sequence + 1) & 0x0f;
        if self.sequence == 0 {
            self.sequence = 1;
        }
        self.sequence
    }

    fn acpi_configuration(&self) -> u8 {
        // Preflight rejects MTFG, so pinned mt792x_acpi_get_flags returns only BIT(0).
        1
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
        let next = next_dma_index(descriptor_index, MCU_TX_RING_COUNT);
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
        self.mcu.payload.zero_bytes(MCU_COMMAND_PAYLOAD_BYTES)?;
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
        let next = next_dma_index(descriptor_index, MCU_TX_RING_COUNT);
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
        self.mcu.payload.zero_bytes(MCU_COMMAND_PAYLOAD_BYTES)?;
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
        let next = next_dma_index(descriptor_index, MCU_TX_RING_COUNT);
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
        self.mcu.payload.zero_bytes(MCU_COMMAND_PAYLOAD_BYTES)?;
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
            if reset_vfio_device(self.device).is_err() {
                retain_mappings_for_watchdog("loader reset while pinned failed");
            }
            if verify_pci_dma_disabled(self.bdf)
                .and_then(|()| verify_active_reset_containment(self.mcu.wfdma, self.pcie_mac))
                .and_then(|()| set_lab_safety("SAFE"))
                .is_err()
            {
                retain_mappings_for_watchdog("loader post-reset containment verification failed");
            }
            println!(r#"{{"active_fwdl_event":"reset_while_pinned"}}"#);
            Ok(())
        } else {
            Err(format!("loader cleanup failed before reset: {errors:?}"))
        }
    }
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Debug)]
struct PhysicalPassiveError(String);

#[cfg(feature = "fuchsia-passive")]
impl std::fmt::Display for PhysicalPassiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(feature = "fuchsia-passive")]
impl std::error::Error for PhysicalPassiveError {}

#[cfg(feature = "fuchsia-passive")]
struct PassiveMacExecutor<'a> {
    pages: &'a [Option<ReadPage>; PASSIVE_MAC_BAR_PAGES.len()],
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PassivePrepareStep {
    MacMmio,
    ProgramDataRing,
    VerifyDataRing,
    AuthorizeDataIrq,
    EnableDataIrq,
    VerifyDataIrq,
}

#[cfg(feature = "fuchsia-passive")]
fn run_passive_prepare_steps<E>(
    mut execute: impl FnMut(PassivePrepareStep) -> Result<(), E>,
) -> Result<(), E> {
    for step in [
        PassivePrepareStep::MacMmio,
        PassivePrepareStep::ProgramDataRing,
        PassivePrepareStep::VerifyDataRing,
        PassivePrepareStep::AuthorizeDataIrq,
        PassivePrepareStep::EnableDataIrq,
        PassivePrepareStep::VerifyDataIrq,
    ] {
        execute(step)?;
    }
    Ok(())
}

#[cfg(feature = "fuchsia-passive")]
impl PassiveMacExecutor<'_> {
    fn page(&self, address: u32) -> Result<&ReadPage, String> {
        let offset = passive_mac_bar_offset(address)
            .map_err(|error| format!("translate passive MAC address: {error:?}"))?;
        let bar_page = offset & !(PAGE - 1);
        self.pages
            .iter()
            .flatten()
            .find(|page| page.bar_page == bar_page)
            .ok_or_else(|| format!("passive MAC address {address:#010x} has no mapped page"))
    }

    fn read(&self, address: u32) -> Result<u32, String> {
        self.page(address)?.read_passive_mac(address)
    }

    fn write(&self, address: u32, value: u32) -> Result<(), String> {
        self.page(address)?.write_passive_mac(address, value)
    }

    fn execute(&self) -> Result<(), String> {
        for operation in passive_mac_mmio_plan() {
            match operation {
                PassiveMacMmioOperation::Rmw {
                    address,
                    mask,
                    value,
                } => {
                    let initial = self.read(address)?;
                    let programmed = passive_mac_source_rmw_value(initial, mask, value & mask);
                    self.write(address, programmed)?;
                    let readback = self.read(address)?;
                    println!(
                        r#"{{"passive_mac_rmw":{{"address":"{address:#010x}","initial":"{initial:#010x}","mask":"{mask:#010x}","programmed":"{programmed:#010x}","observed":"{readback:#010x}","verification":"source-single-read-write"}}}}"#
                    );
                }
                PassiveMacMmioOperation::WtblClear {
                    index,
                    address,
                    value,
                    busy_mask,
                    timeout_us,
                } => {
                    self.write(address, value)?;
                    let deadline =
                        Instant::now() + std::time::Duration::from_micros(u64::from(timeout_us));
                    loop {
                        let readback = self.read(address)?;
                        if readback & busy_mask == 0 {
                            break;
                        }
                        if Instant::now() >= deadline {
                            return Err(format!("WTBL clear {index} busy timeout"));
                        }
                        std::thread::sleep(std::time::Duration::from_micros(10));
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(feature = "fuchsia-passive")]
fn drain_data_rx_queue(
    wfdma: &ReadPage,
    queue: &mut ActiveMcuRx<'_>,
    provenance: &mut DescriptorProvenance,
    completions: &mut Vec<MgmtTxCompletion>,
) -> Result<Vec<PrivateRawAdvertisementCarrier>, String> {
    let mut advertisements = Vec::new();
    let result = (|| -> Result<(), String> {
        loop {
            let descriptor = queue.rx_ring.read_descriptor_at(queue.rx_tail);
            if !descriptor.is_dma_done() {
                break;
            }
            std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
            let completed_index = queue.rx_tail;
            let length = ((descriptor.ctrl >> 16) & 0x3fff) as usize;
            let parsed = if descriptor.ctrl & (1 << 30) == 0 {
                provenance.consume_without_mint(
                    DescriptorOccurrenceRoute::DataRx,
                    queue.rx_ring_index,
                    completed_index,
                );
                Err("fragmented data RX descriptor is unsupported".into())
            } else if !(24..=2048).contains(&length) {
                provenance.consume_without_mint(
                    DescriptorOccurrenceRoute::DataRx,
                    queue.rx_ring_index,
                    completed_index,
                );
                Err(format!("invalid data RX descriptor length {length}"))
            } else {
                let bytes = queue
                    .rx_buffers
                    .read_bytes(completed_index * 2048, length)?;
                let completion = match mt7921_packet_type(&bytes) {
                    Some(6) => parse_mt7921_tx_free(&bytes)
                        .ok()
                        .map(MgmtTxCompletion::Free),
                    Some(0) if bytes.len() == 40 => parse_mt7921_tx_status(&bytes)
                        .ok()
                        .map(MgmtTxCompletion::Status),
                    _ => None,
                };
                if let Some(completion) = completion {
                    provenance.consume_without_mint(
                        DescriptorOccurrenceRoute::DataRx,
                        queue.rx_ring_index,
                        completed_index,
                    );
                    completions.push(completion);
                    Ok(None)
                } else {
                    match parse_passive_advertisement(&bytes) {
                        Ok(advertisement) => Ok(Some(
                            match provenance.seal_frame(
                                DescriptorOccurrenceRoute::DataRx,
                                queue.rx_ring_index,
                                completed_index,
                                bytes,
                            )? {
                                PrivateFrameSeal::Carried(frame) => {
                                    PrivateRawAdvertisementCarrier {
                                        advertisement,
                                        frame_bytes: frame.bytes,
                                        occurrence: frame.occurrence,
                                    }
                                }
                                PrivateFrameSeal::Uncovered(frame_bytes) => {
                                    PrivateRawAdvertisementCarrier {
                                        advertisement,
                                        frame_bytes,
                                        occurrence: None,
                                    }
                                }
                            },
                        )),
                        Err(error) => {
                            provenance.consume_without_mint(
                                DescriptorOccurrenceRoute::DataRx,
                                queue.rx_ring_index,
                                completed_index,
                            );
                            Err(format!(
                                "reject passive RX descriptor {completed_index}: {error:?}"
                            ))
                        }
                    }
                }
            };

            let refill_index = queue.rx_head;
            let refill = DmaDescriptor::rx(DmaSegment {
                iova: queue.rx_buffers.iova + (refill_index * 2048) as u64,
                len: 2048,
            })
            .map_err(|error| format!("rearm data RX descriptor: {error:?}"));
            let parsed = publish_current_rearm_or_revoke(
                provenance,
                DescriptorOccurrenceRoute::DataRx,
                queue.rx_ring_index,
                refill_index,
                parsed,
                refill,
                |_, refill| {
                    queue.rx_ring.write_descriptor_at(refill_index, refill);
                    std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
                    queue.rx_head = next_dma_index(queue.rx_head, queue.rx_count);
                    wfdma.write_rx_cpu_index(queue.rx_ring_index, queue.rx_head as u32)
                },
            )?;
            queue.rx_tail = next_dma_index(queue.rx_tail, queue.rx_count);
            if let Some(advertisement) = parsed? {
                advertisements.push(advertisement);
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        revoke_before_local_carrier_release(provenance, &mut advertisements)?;
        return Err(error);
    }
    Ok(advertisements)
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MgmtTxCompletion {
    Free(Mt7921TxFree),
    Status(Mt7921TxStatus),
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Default)]
struct MgmtTxCompletionState {
    free: Option<Mt7921TxFree>,
    status: Option<Mt7921TxStatus>,
}

#[cfg(feature = "fuchsia-passive")]
impl MgmtTxCompletionState {
    fn observe(&mut self, completion: MgmtTxCompletion) -> Result<(), String> {
        match completion {
            MgmtTxCompletion::Free(value) if value.token == 0 && self.free.is_none() => {
                self.free = Some(value)
            }
            MgmtTxCompletion::Status(value)
                if value.pid == 3 && value.wcid == 19 && self.status.is_none() =>
            {
                self.status = Some(value)
            }
            _ => return Err("uncorrelated or duplicate management TX completion".into()),
        }
        Ok(())
    }

    fn finished(&self) -> Option<Result<(), String>> {
        let (free, status) = (self.free?, self.status?);
        Some(if !free.dropped && status.acked {
            Ok(())
        } else {
            Err("SAE authentication MPDU was not acknowledged".into())
        })
    }
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Default)]
struct LiveClientState {
    scan_seen: bool,
    scan_authorized: bool,
    power_rate_authorized: bool,
    frame: Option<Vec<u8>>,
}

#[cfg(feature = "fuchsia-passive")]
struct LiveClientEffects {
    state: Arc<Mutex<LiveClientState>>,
    target: [u8; 6],
}

#[cfg(feature = "fuchsia-passive")]
impl Mt7921ClientEffects for LiveClientEffects {
    fn revoke_scan(&mut self) {
        self.state.lock().unwrap().scan_authorized = false;
    }
    fn revoke_lifecycle(&mut self) {
        *self.state.lock().unwrap() = LiveClientState::default();
    }
    fn set_channel(
        &mut self,
        _: fidl_ieee80211::ChannelNumber,
        _: fidl_ieee80211::ChannelBandwidth,
        _: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn join_bss(&mut self, _: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        Ok(())
    }
    fn send_wlan_frame(
        &mut self,
        bytes: &[u8],
        _: fidl_softmac::WlanTxInfoFlags,
    ) -> Result<(), zx::Status> {
        let mut state = self.state.lock().unwrap();
        if !state.scan_authorized
            || !state.power_rate_authorized
            || state.frame.is_some()
            || bytes.get(..2) != Some(&[0xb0, 0])
            || bytes.get(4..10) != Some(&self.target)
        {
            return Err(zx::Status::ACCESS_DENIED);
        }
        state.frame = Some(bytes.to_vec());
        Ok(())
    }
    fn install_key(&mut self, _: &fidl_softmac::WlanKeyConfiguration) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn notify_association_complete(
        &mut self,
        _: &fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn clear_association(
        &mut self,
        _: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn set_link_up(&mut self, _: bool) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn next_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
        Ok(None)
    }
    fn begin_passive_scan(
        &mut self,
        _: u64,
        _: &[fidl_ieee80211::ChannelNumber],
    ) -> Result<(), zx::Status> {
        self.state.lock().unwrap().scan_seen = false;
        Ok(())
    }
    fn observe_passive_scan(
        &mut self,
        _: u64,
        observation: &fuchsia_softmac_port::ScanObservation,
    ) -> Result<(), zx::Status> {
        if observation.bss.bssid == self.target {
            self.state.lock().unwrap().scan_seen = true;
        }
        Ok(())
    }
    fn complete_passive_scan(&mut self, _: u64, success: bool) -> Result<(), zx::Status> {
        let mut state = self.state.lock().unwrap();
        state.scan_authorized = success && state.scan_seen;
        Ok(())
    }
    fn reset(&mut self) -> Result<(), zx::Status> {
        Ok(())
    }
    fn stop(&mut self) -> Result<(), zx::Status> {
        Ok(())
    }
}

#[cfg(feature = "fuchsia-passive")]
struct VfioPassiveMechanics<'a, 'b, 'c> {
    loader: &'a mut VfioFirmwareLoader<'b>,
    ledger: &'c mut ContainmentLedger,
    data: ActiveMcuRx<'b>,
    mac_pages: &'b [Option<ReadPage>; PASSIVE_MAC_BAR_PAGES.len()],
    scan_started: Option<Instant>,
    pending_scan_done: Option<u8>,
    advertisements: Vec<PrivateRawAdvertisementCarrier>,
    tx_completions: Vec<MgmtTxCompletion>,
}

#[cfg(feature = "fuchsia-passive")]
impl Drop for VfioPassiveMechanics<'_, '_, '_> {
    fn drop(&mut self) {
        // The tracker is borrowed through `loader`; revoke it before this
        // owner's queued advertisement carriers are released.
        let _ = self
            .loader
            .mcu
            .descriptor_provenance
            .invalidate(DescriptorInvalidation::Run);
    }
}

#[cfg(feature = "fuchsia-passive")]
impl VfioPassiveMechanics<'_, '_, '_> {
    fn stop_tx_dma_and_reset_ring0(&mut self) -> Result<(), String> {
        let global = self.loader.mcu.wfdma.read(0xd4208)?;
        self.loader
            .mcu
            .wfdma
            .write_active_wfdma(0xd4208, global & !1)?;
        let deadline = Instant::now() + std::time::Duration::from_millis(100);
        while self.loader.mcu.wfdma.read(0xd4208)? & 2 != 0 {
            if Instant::now() >= deadline {
                return Err("REBOOT REQUIRED: TX DMA did not quiesce".into());
            }
            std::thread::sleep(std::time::Duration::from_micros(10));
        }
        let reset = self.loader.mcu.wfdma.read(0xd4100)?;
        self.loader
            .mcu
            .wfdma
            .write_active_wfdma(0xd4100, reset & !(1 << 4))?;
        self.loader
            .mcu
            .wfdma
            .write_active_wfdma(0xd4100, reset | (1 << 4))?;
        if self.loader.mcu.wfdma.read(0xd430c)? != 0 {
            return Err("REBOOT REQUIRED: ring-0 DIDX remained nonzero after reset".into());
        }
        Ok(())
    }

    fn configure_mgmt_tx_ring(&mut self, ring: &DmaArena) -> Result<(), String> {
        self.stop_tx_dma_and_reset_ring0()?;
        self.loader
            .mcu
            .wfdma
            .write_tx_ring_slot(0, ring.iova as u32, 128, 0)?;
        self.loader
            .mcu
            .wfdma
            .write_active_wfdma(0xd4600, 0x0140_0004)?;
        for (offset, expected) in [
            (0xd4300, ring.iova as u32),
            (0xd4304, 128),
            (0xd4308, 0),
            (0xd430c, 0),
            (0xd4600, 0x0140_0004),
        ] {
            let actual = self.loader.mcu.wfdma.read(offset)?;
            if actual != expected {
                return Err(format!(
                    "REBOOT REQUIRED: ring-0 readback {offset:#x}={actual:#x}, expected {expected:#x}"
                ));
            }
        }
        let global = self.loader.mcu.wfdma.read(0xd4208)?;
        self.loader
            .mcu
            .wfdma
            .write_active_wfdma(0xd4208, global | 1)
    }

    fn transmit_one_sae_auth(
        &mut self,
        ring: &mut DmaArena,
        txwi: &mut DmaArena,
        frame_arena: &mut DmaArena,
        frame: &[u8],
    ) -> Result<(), String> {
        if !self.tx_completions.is_empty() {
            return Err("management TX began with stale completion state".into());
        }
        self.configure_mgmt_tx_ring(ring)?;
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        let mut completion_state = MgmtTxCompletionState::default();
        let result = (|| -> Result<(), String> {
            frame_arena.write_bytes(frame)?;
            let encoded = encode_mt7921_5ghz_auth_tx(frame, txwi.iova, frame_arena.iova, 0, 3, 19)
                .map_err(|error| format!("encode one SAE authentication MPDU: {error:?}"))?;
            txwi.write_bytes(&encoded.txwi)?;
            ring.write_descriptor_at(0, encoded.descriptor);
            std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
            self.loader.mcu.wfdma.write_active_wfdma(0xd4308, 1)?;
            loop {
                self.loader.mcu.cancelled()?;
                self.loader.mcu.handle_irq(None)?;
                let _ = drain_data_rx_queue(
                    self.loader.mcu.wfdma,
                    &mut self.data,
                    &mut self.loader.mcu.descriptor_provenance,
                    &mut self.tx_completions,
                )?;
                for completion in self.tx_completions.drain(..) {
                    completion_state.observe(completion)?;
                }
                if let Some(result) = completion_state.finished() {
                    break result;
                }
                if Instant::now() >= deadline {
                    break Err(
                        "SAE management TX completion timed out; frame may have transmitted".into(),
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })();
        let contained = self.stop_tx_dma_and_reset_ring0();
        if let Err(error) = contained {
            return Err(format!(
                "REBOOT REQUIRED: management TX outcome={result:?}; containment failed: {error}"
            ));
        }
        ring.write_descriptor_at(0, DmaDescriptor::reset());
        let txwi_reset = txwi.zero_bytes(PAGE);
        let frame_reset = frame_arena.zero_bytes(PAGE);
        if let Err(error) = result {
            txwi_reset
                .map_err(|wipe| format!("REBOOT REQUIRED: {error}; TXWI reclaim failed: {wipe}"))?;
            frame_reset.map_err(|wipe| {
                format!("REBOOT REQUIRED: {error}; frame reclaim failed: {wipe}")
            })?;
            return Err(format!("REBOOT REQUIRED: SAE spike failed: {error}"));
        }
        txwi_reset.map_err(|error| format!("REBOOT REQUIRED: TXWI reclaim failed: {error}"))?;
        frame_reset.map_err(|error| format!("REBOOT REQUIRED: frame reclaim failed: {error}"))
    }
}

#[cfg(feature = "fuchsia-passive")]
impl SourceExactPassiveMechanics for VfioPassiveMechanics<'_, '_, '_> {
    type Error = PhysicalPassiveError;

    fn prepare_passive_receive(&mut self) -> Result<PassivePrerequisites, Self::Error> {
        self.ledger
            .transition(RunPhase::FirmwareReady, RunPhase::PassivePreparing)
            .map_err(PhysicalPassiveError)?;
        if self.data.rx_ring_index != 2
            || self.data.irq_bit != DATA_RX_IRQ_BIT
            || self.data.rx_count != 8
            || self.loader.mcu.extra_irq_mask != 0
        {
            return Err(PhysicalPassiveError(
                "data RX ring 2 identity mismatch".into(),
            ));
        }
        run_passive_prepare_steps(|step| -> Result<(), PhysicalPassiveError> {
            match step {
                PassivePrepareStep::MacMmio => {
                    PassiveMacExecutor {
                        pages: self.mac_pages,
                    }
                    .execute()
                    .map_err(PhysicalPassiveError)?;
                }
                PassivePrepareStep::ProgramDataRing => {
                    std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
                    self.loader
                        .mcu
                        .wfdma
                        .write_rx_ring_slot(2, self.data.rx_ring.iova as u32, 8, 7, 0)
                        .map_err(PhysicalPassiveError)?;
                }
                PassivePrepareStep::VerifyDataRing => self
                    .loader
                    .mcu
                    .wfdma
                    .verify_rx_ring_slot(2, self.data.rx_ring.iova as u32, 8, 7, 0)
                    .map_err(PhysicalPassiveError)?,
                PassivePrepareStep::AuthorizeDataIrq => self
                    .loader
                    .mcu
                    .wfdma
                    .authorize_passive_data_rx_irq()
                    .map_err(PhysicalPassiveError)?,
                PassivePrepareStep::EnableDataIrq => {
                    self.loader.mcu.extra_irq_mask = DATA_RX_IRQ_BIT;
                    self.loader
                        .mcu
                        .wfdma
                        .write_active_wfdma(0xd4204, self.loader.mcu.rx_irq_mask())
                        .map_err(PhysicalPassiveError)?;
                }
                PassivePrepareStep::VerifyDataIrq => {
                    let expected = self.loader.mcu.rx_irq_mask();
                    let actual = self
                        .loader
                        .mcu
                        .wfdma
                        .read(0xd4204)
                        .map_err(PhysicalPassiveError)?;
                    if actual != expected {
                        return Err(PhysicalPassiveError(format!(
                            "passive RX interrupt mask readback {actual:#010x}, expected {expected:#010x}"
                        )));
                    }
                }
            }
            println!(r#"{{"passive_prepare_step":"{step:?}"}}"#);
            Ok(())
        })?;
        let prerequisites = PassivePrerequisites {
            channel_domain_mask_zero: true,
            mac_mmio_initialized: true,
            data_rx_owned: true,
        };
        self.ledger
            .transition(RunPhase::PassivePreparing, RunPhase::PassiveReady)
            .map_err(PhysicalPassiveError)?;
        Ok(prerequisites)
    }

    fn command(
        &mut self,
        command: &PassiveMcuCommand,
        encoded: &[u8],
        wait_response: bool,
    ) -> Result<(), Self::Error> {
        observe_passive_command_provenance(&mut self.loader.mcu.descriptor_provenance, command)
            .map_err(PhysicalPassiveError)?;
        if matches!(command, PassiveMcuCommand::StartScan { .. }) {
            self.ledger
                .transition(RunPhase::PassiveReady, RunPhase::Scanning)
                .map_err(PhysicalPassiveError)?;
        }
        self.loader
            .send_passive_command(command, encoded, wait_response)
            .map_err(PhysicalPassiveError)?;
        if matches!(command, PassiveMcuCommand::StartScan { .. }) {
            self.scan_started = Some(Instant::now());
        }
        Ok(())
    }

    fn next_event(
        &mut self,
        deadline_nanos: i64,
    ) -> Result<Option<PassiveMechanicsEvent>, Self::Error> {
        self.loader
            .mcu
            .handle_irq(None)
            .map_err(PhysicalPassiveError)?;
        self.advertisements.extend(
            drain_data_rx_queue(
                self.loader.mcu.wfdma,
                &mut self.data,
                &mut self.loader.mcu.descriptor_provenance,
                &mut self.tx_completions,
            )
            .map_err(PhysicalPassiveError)?,
        );
        let mut routed_frames = std::mem::take(&mut self.loader.mcu.normal_rx_frames);
        while !routed_frames.is_empty() {
            let frame = routed_frames.remove(0);
            match frame.parse() {
                Ok(advertisement) => self.advertisements.push(advertisement),
                Err((frame, error)) => {
                    revoke_before_local_carrier_release(
                        &mut self.loader.mcu.descriptor_provenance,
                        &mut routed_frames,
                    )
                    .map_err(PhysicalPassiveError)?;
                    drop(frame);
                    return Err(PhysicalPassiveError(error));
                }
            }
        }
        if let Some(advertisement) = self.advertisements.pop() {
            return Ok(Some(PassiveMechanicsEvent::Advertisement {
                timestamp_nanos: self.loader.start.elapsed().as_nanos() as i64,
                advertisement: advertisement
                    .into_unprovenanced(&mut self.loader.mcu.descriptor_provenance),
            }));
        }
        if let Some(index) = self
            .loader
            .mcu
            .unsolicited
            .iter()
            .position(|event| event.event_id == 0x0d)
        {
            let event = self.loader.mcu.unsolicited.remove(index);
            let done = parse_passive_scan_done(&event.bytes)
                .map_err(|error| PhysicalPassiveError(format!("parse scan done: {error:?}")))?;
            self.loader
                .mcu
                .wfdma
                .verify_rx_ring_slot(
                    2,
                    self.data.rx_ring.iova as u32,
                    8,
                    self.data.rx_head as u32,
                    self.data.rx_tail as u32,
                )
                .map_err(PhysicalPassiveError)?;
            let expected_irq = self.loader.mcu.rx_irq_mask();
            let actual_irq = self
                .loader
                .mcu
                .wfdma
                .read(0xd4204)
                .map_err(PhysicalPassiveError)?;
            if actual_irq != expected_irq {
                return Err(PhysicalPassiveError(format!(
                    "post-scan RX interrupt mask {actual_irq:#010x}, expected {expected_irq:#010x}"
                )));
            }
            self.pending_scan_done = Some(done.scan_sequence);
            return Ok(Some(PassiveMechanicsEvent::ScanDone(done)));
        }
        let started = self
            .scan_started
            .ok_or_else(|| PhysicalPassiveError("scan event requested before START_SCAN".into()))?;
        let allowed = u64::try_from(deadline_nanos)
            .map_err(|_| PhysicalPassiveError("negative passive deadline".into()))?;
        if started.elapsed() > std::time::Duration::from_nanos(allowed + 2_000_000_000) {
            return Err(PhysicalPassiveError(
                "passive scan completion timed out".into(),
            ));
        }
        Ok(None)
    }

    fn confirm_scan_done(&mut self, scan_sequence: u8) -> Result<(), Self::Error> {
        if self.pending_scan_done.take() != Some(scan_sequence) {
            return Err(PhysicalPassiveError(
                "scan completion confirmation did not match pending hardware proof".into(),
            ));
        }
        self.scan_started = None;
        self.ledger
            .transition(RunPhase::Scanning, RunPhase::PassiveReady)
            .map_err(PhysicalPassiveError)
    }
}

#[cfg(feature = "fuchsia-passive")]
fn parse_mac(value: &str) -> Result<[u8; 6], String> {
    let bytes = value
        .split(':')
        .map(|part| u8::from_str_radix(part, 16).map_err(|_| "invalid BSSID".to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    bytes.try_into().map_err(|_| "invalid BSSID".into())
}

#[cfg(feature = "fuchsia-passive")]
fn verify_no_usable_mt792x_acpi_sar() -> Result<(), String> {
    fn inspect(path: &std::path::Path) -> Result<bool, String> {
        if path.is_dir() {
            for entry in std::fs::read_dir(path)
                .map_err(|error| format!("read ACPI directory {}: {error}", path.display()))?
            {
                let entry = entry.map_err(|error| format!("read ACPI table entry: {error}"))?;
                if inspect(&entry.path())? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        let bytes = std::fs::read(path)
            .map_err(|error| format!("read ACPI table {}: {error}", path.display()))?;
        Ok(bytes
            .windows(4)
            .any(|window| window == b"MTCL" || window == b"MTFG"))
    }
    if inspect(std::path::Path::new("/sys/firmware/acpi/tables"))? {
        return Err(
            "ACPI MTCL/MTFG exists; this narrow setup cannot derive its platform flags".into(),
        );
    }
    Ok(())
}

struct Ioas {
    fd: Arc<File>,
    id: u32,
    destroyed: bool,
}

struct DmaArena {
    iommu: Arc<File>,
    ioas: u32,
    ptr: NonNull<u8>,
    len: usize,
    iova: u64,
    mapped: bool,
}
impl DmaArena {
    fn map(iommu: &Arc<File>, ioas: u32, iova: u64) -> Result<Self, String> {
        Self::map_len(iommu, ioas, iova, PAGE)
    }
    fn map_len(iommu: &Arc<File>, ioas: u32, iova: u64, len: usize) -> Result<Self, String> {
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
            iommu: Arc::clone(iommu),
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

struct VfioDisabledFirmwareStage<'a> {
    ring: &'a mut DmaArena,
    payload: &'a mut DmaArena,
}
impl DisabledFirmwareStageTransport for VfioDisabledFirmwareStage<'_> {
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
impl Drop for DmaArena {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}
impl Drop for Ioas {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

impl Ioas {
    fn teardown(&mut self) -> Result<(), String> {
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

#[allow(dead_code)]
struct VfioIrq {
    device: Arc<File>,
    event_fd: OwnedFd,
    index: u32,
    installed: bool,
}
#[allow(dead_code)]
impl VfioIrq {
    fn install(device: &Arc<File>, capability: PciIrqCapability) -> Result<Self, String> {
        if capability.count == 0 || !capability.eventfd {
            return Err("refused non-eventfd VFIO interrupt".into());
        }
        let index = match capability.kind {
            PciIrqKind::Intx => 0,
            PciIrqKind::Msi => 1,
            PciIrqKind::Msix => 2,
        };
        let event_fd_raw = unsafe { eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK) };
        if event_fd_raw < 0 {
            return Err(format!(
                "create IRQ eventfd: {}",
                std::io::Error::last_os_error()
            ));
        }
        let event_fd = unsafe { OwnedFd::from_raw_fd(event_fd_raw) };
        let mut set = IrqSetEventfd {
            header: IrqSetHeader {
                argsz: size::<IrqSetEventfd>(),
                flags: VFIO_IRQ_SET_DATA_EVENTFD | VFIO_IRQ_SET_ACTION_TRIGGER,
                index,
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
            index,
            installed: true,
        })
    }
    fn try_read(&self) -> Result<Option<u64>, String> {
        let mut counter = 0u64;
        let result = unsafe {
            read(
                self.event_fd.as_raw_fd(),
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
            self.device.as_raw_fd(),
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
    }
}

struct ReadPage {
    ptr: NonNull<u8>,
    bar_page: usize,
    active_rx_irq_mask: Cell<u32>,
    mapped: bool,
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
        Ok(Self {
            ptr,
            bar_page,
            active_rx_irq_mask: Cell::new(WM_RX_IRQ_BIT | WM2_RX_IRQ_BIT),
            mapped: true,
        })
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
    #[cfg(feature = "fuchsia-passive")]
    fn read_passive_mac(&self, address: u32) -> Result<u32, String> {
        if !passive_mac_address_allowed(address) {
            return Err(format!(
                "passive MAC read {address:#010x} escaped exact plan"
            ));
        }
        let offset = passive_mac_bar_offset(address)
            .map_err(|error| format!("translate passive MAC read: {error:?}"))?;
        if self.bar_page != offset & !(PAGE - 1) {
            return Err(format!(
                "passive MAC read {address:#010x} used wrong fixed BAR page"
            ));
        }
        let value = self.read(offset)?;
        validate_passive_mac_bar_read(address, value)
            .map(|(_, value)| value)
            .map_err(|error| format!("validate passive MAC read: {error:?}"))
    }
    #[cfg(feature = "fuchsia-passive")]
    fn write_passive_mac(&self, address: u32, value: u32) -> Result<(), String> {
        if !passive_mac_address_allowed(address) {
            return Err(format!(
                "passive MAC write {address:#010x} escaped exact plan"
            ));
        }
        let offset = passive_mac_bar_offset(address)
            .map_err(|error| format!("translate passive MAC write: {error:?}"))?;
        if self.bar_page != offset & !(PAGE - 1) {
            return Err(format!(
                "passive MAC write {address:#010x} used wrong fixed BAR page"
            ));
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
    #[cfg(feature = "fuchsia-passive")]
    fn verify_rx_ring_slot(
        &self,
        index: usize,
        descriptor_base: u32,
        descriptor_count: u32,
        cpu_index: u32,
        dma_index: u32,
    ) -> Result<(), String> {
        if self.bar_page != 0xd4000 || index != 2 {
            return Err("passive RX verification escaped ring 2".into());
        }
        let expected = [descriptor_base, descriptor_count, cpu_index, dma_index];
        for (word, expected) in expected.into_iter().enumerate() {
            let offset = self.bar_page + 0x500 + index * 0x10 + word * 4;
            let actual = self.read(offset)?;
            if actual != expected {
                return Err(format!(
                    "passive RX ring 2 word {word} readback {actual:#010x}, expected {expected:#010x}"
                ));
            }
        }
        Ok(())
    }
    fn write_active_wfdma(&self, offset: usize, value: u32) -> Result<(), String> {
        if self.bar_page != 0xd4000 {
            return Err("active WFDMA write escaped BAR page".into());
        }
        if !active_wfdma_write_allowed(offset, value, self.active_rx_irq_mask.get()) {
            return Err(format!(
                "active WFDMA write {offset:#x}={value:#x} escaped allowlist"
            ));
        }
        let within = offset - self.bar_page;
        unsafe { std::ptr::write_volatile(self.ptr.as_ptr().add(within).cast::<u32>(), value) };
        Ok(())
    }
    #[cfg(feature = "fuchsia-passive")]
    fn authorize_passive_data_rx_irq(&self) -> Result<(), String> {
        if self.bar_page != 0xd4000 {
            return Err("data RX authorization escaped WFDMA BAR page".into());
        }
        self.active_rx_irq_mask
            .set(WM_RX_IRQ_BIT | DATA_RX_IRQ_BIT | WM2_RX_IRQ_BIT);
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
        let _ = self.teardown();
    }
}

impl ReadPage {
    fn teardown(&mut self) -> Result<(), String> {
        if !self.mapped {
            return Ok(());
        }
        if unsafe { munmap(self.ptr.as_ptr(), PAGE) } != 0 {
            return Err(format!(
                "unmap BAR page {:#x}: {}",
                self.bar_page,
                std::io::Error::last_os_error()
            ));
        }
        self.mapped = false;
        Ok(())
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
    #[cfg(feature = "fuchsia-passive")]
    RunOneShotPassivePrepare,
    #[cfg(feature = "fuchsia-passive")]
    RunOneShotPassiveChannel1,
    #[cfg(feature = "fuchsia-passive")]
    RunOneShotPassiveChannels1And6,
    #[cfg(feature = "fuchsia-passive")]
    RunOneShotPassive2Ghz,
    #[cfg(feature = "fuchsia-passive")]
    RunOneShotPassive5GhzNonDfs,
    #[cfg(feature = "fuchsia-passive")]
    RunOneShotPassive5GhzDfsLow,
    #[cfg(feature = "fuchsia-passive")]
    RunOneShotPassive5GhzDfsHigh,
    #[cfg(feature = "fuchsia-passive")]
    RunOneShotPassiveSmeFull,
    #[cfg(feature = "fuchsia-passive")]
    RunOneShotPowerSetup,
    #[cfg(feature = "fuchsia-passive")]
    RunOneShotSaeAuth,
}

impl Operation {
    #[cfg(feature = "fuchsia-passive")]
    fn passive_scan_attempt_limit(self) -> usize {
        if matches!(self, Self::RunOneShotPowerSetup | Self::RunOneShotSaeAuth) {
            5
        } else {
            1
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    fn should_continue_passive_scans(self, attempt: usize, authorized: bool) -> bool {
        !authorized && attempt < self.passive_scan_attempt_limit()
    }

    fn is_passive(self) -> bool {
        #[cfg(feature = "fuchsia-passive")]
        {
            matches!(
                self,
                Self::RunOneShotPassivePrepare
                    | Self::RunOneShotPassiveChannel1
                    | Self::RunOneShotPassiveChannels1And6
                    | Self::RunOneShotPassive2Ghz
                    | Self::RunOneShotPassive5GhzNonDfs
                    | Self::RunOneShotPassive5GhzDfsLow
                    | Self::RunOneShotPassive5GhzDfsHigh
                    | Self::RunOneShotPassiveSmeFull
                    | Self::RunOneShotPowerSetup
                    | Self::RunOneShotSaeAuth
            )
        }
        #[cfg(not(feature = "fuchsia-passive"))]
        {
            false
        }
    }

    fn loads_firmware(self) -> bool {
        matches!(
            self,
            Self::RunOneShotFirmware | Self::RunOneShotChannelDomain
        ) || self.is_passive()
    }

    fn is_active_mcu(self) -> bool {
        self == Self::QueryPatchSemaphore || self.loads_firmware()
    }

    fn needs_pcie_mac(self) -> bool {
        self == Self::PrepareOwnedGlobalTxRings || self.is_active_mcu()
    }

    fn wfdma_writable(self) -> bool {
        matches!(
            self,
            Self::ProgramDisabledFwdlRing
                | Self::MaskAckDisabledFwdl
                | Self::PrepareOwnedGlobalTxRings
                | Self::QueryPatchSemaphore
                | Self::RunOneShotFirmware
                | Self::RunOneShotChannelDomain
        ) || self.is_passive()
    }

    fn conn_writable(self) -> bool {
        matches!(
            self,
            Self::AcquireDriverOwnership
                | Self::QueryPatchSemaphore
                | Self::RunOneShotFirmware
                | Self::RunOneShotChannelDomain
        ) || self.is_passive()
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

    #[cfg(feature = "fuchsia-passive")]
    struct B2aFakeDevice {
        events: Arc<std::sync::Mutex<Vec<fidl_fuchsia_wlan_mlme::MlmeEvent>>>,
        event_tx: futures::channel::mpsc::UnboundedSender<fidl_fuchsia_wlan_mlme::MlmeEvent>,
        event_rx:
            Option<futures::channel::mpsc::UnboundedReceiver<fidl_fuchsia_wlan_mlme::MlmeEvent>>,
        minstrel: Option<wlan_mlme::MinstrelWrapper>,
        fail_mlme_event: bool,
    }

    #[cfg(feature = "fuchsia-passive")]
    impl B2aFakeDevice {
        fn new() -> (
            Self,
            Arc<std::sync::Mutex<Vec<fidl_fuchsia_wlan_mlme::MlmeEvent>>>,
        ) {
            let events = Arc::new(std::sync::Mutex::new(Vec::new()));
            let (event_tx, event_rx) = futures::channel::mpsc::unbounded();
            (
                Self {
                    events: Arc::clone(&events),
                    event_tx,
                    event_rx: Some(event_rx),
                    minstrel: None,
                    fail_mlme_event: false,
                },
                events,
            )
        }

        fn new_with_failed_mlme_transport() -> (
            Self,
            Arc<std::sync::Mutex<Vec<fidl_fuchsia_wlan_mlme::MlmeEvent>>>,
        ) {
            let (mut device, events) = Self::new();
            device.fail_mlme_event = true;
            (device, events)
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    impl wlan_mlme::device::DeviceOps for B2aFakeDevice {
        async fn wlan_softmac_query_response(
            &mut self,
        ) -> Result<fidl_fuchsia_wlan_softmac::WlanSoftmacQueryResponse, zx::Status> {
            Ok(fidl_fuchsia_wlan_softmac::WlanSoftmacQueryResponse {
                sta_addr: Some([7; 6]),
                ..Default::default()
            })
        }
        async fn discovery_support(
            &mut self,
        ) -> Result<fidl_fuchsia_wlan_softmac::DiscoverySupport, zx::Status> {
            Ok(fidl_fuchsia_wlan_softmac::DiscoverySupport {
                scan_offload: Some(fidl_fuchsia_wlan_softmac::ScanOffloadExtension {
                    supported: Some(true),
                    scan_cancel_supported: Some(true),
                }),
                ..Default::default()
            })
        }
        async fn mac_sublayer_support(
            &mut self,
        ) -> Result<fidl_fuchsia_wlan_common::MacSublayerSupport, zx::Status> {
            Ok(Default::default())
        }
        async fn security_support(
            &mut self,
        ) -> Result<fidl_fuchsia_wlan_common::SecuritySupport, zx::Status> {
            Ok(Default::default())
        }
        async fn spectrum_management_support(
            &mut self,
        ) -> Result<fidl_fuchsia_wlan_common::SpectrumManagementSupport, zx::Status> {
            Ok(Default::default())
        }
        fn deliver_eth_frame(&mut self, _packet: &[u8]) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        fn send_wlan_frame(
            &mut self,
            _buffer: fdf::ArenaStaticBox<[u8]>,
            _tx_flags: fidl_fuchsia_wlan_softmac::WlanTxInfoFlags,
            _async_id: Option<fuchsia_trace::Id>,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        async fn set_ethernet_status(
            &mut self,
            _status: wlan_mlme::device::LinkStatus,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        async fn set_channel(
            &mut self,
            _primary: fidl_fuchsia_wlan_ieee80211::ChannelNumber,
            _bandwidth: fidl_fuchsia_wlan_ieee80211::ChannelBandwidth,
            _vht_secondary_80_channel: fidl_fuchsia_wlan_ieee80211::ChannelNumber,
        ) -> Result<(), zx::Status> {
            Ok(())
        }
        async fn set_mac_address(&mut self, _mac_addr: [u8; 6]) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        async fn start_passive_scan(
            &mut self,
            _request: &fidl_fuchsia_wlan_softmac::WlanSoftmacBaseStartPassiveScanRequest,
        ) -> Result<fidl_fuchsia_wlan_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status>
        {
            Ok(
                fidl_fuchsia_wlan_softmac::WlanSoftmacBaseStartPassiveScanResponse {
                    scan_id: Some(7),
                },
            )
        }
        async fn start_active_scan(
            &mut self,
            _request: &fidl_fuchsia_wlan_softmac::WlanSoftmacStartActiveScanRequest,
        ) -> Result<fidl_fuchsia_wlan_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status>
        {
            Err(zx::Status::NOT_SUPPORTED)
        }
        async fn cancel_scan(
            &mut self,
            _request: &fidl_fuchsia_wlan_softmac::WlanSoftmacBaseCancelScanRequest,
        ) -> Result<(), zx::Status> {
            Ok(())
        }
        async fn join_bss(
            &mut self,
            _request: &fidl_fuchsia_wlan_driver::JoinBssRequest,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        async fn enable_beaconing(
            &mut self,
            _request: fidl_fuchsia_wlan_softmac::WlanSoftmacBaseEnableBeaconingRequest,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        async fn disable_beaconing(&mut self) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        async fn install_key(
            &mut self,
            _configuration: &fidl_fuchsia_wlan_softmac::WlanKeyConfiguration,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        async fn notify_association_complete(
            &mut self,
            _configuration: fidl_fuchsia_wlan_softmac::WlanAssociationConfig,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        async fn clear_association(
            &mut self,
            _request: &fidl_fuchsia_wlan_softmac::WlanSoftmacBaseClearAssociationRequest,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        async fn update_wmm_parameters(
            &mut self,
            _request: &fidl_fuchsia_wlan_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        fn take_mlme_event_stream(
            &mut self,
        ) -> Option<futures::channel::mpsc::UnboundedReceiver<fidl_fuchsia_wlan_mlme::MlmeEvent>>
        {
            self.event_rx.take()
        }
        fn send_mlme_event(
            &mut self,
            event: fidl_fuchsia_wlan_mlme::MlmeEvent,
        ) -> Result<(), anyhow::Error> {
            if self.fail_mlme_event {
                return Err(anyhow::anyhow!("injected MLME event transport failure"));
            }
            self.events.lock().unwrap().push(event.clone());
            self.event_tx.unbounded_send(event).map_err(Into::into)
        }
        fn set_minstrel(&mut self, minstrel: wlan_mlme::MinstrelWrapper) {
            self.minstrel = Some(minstrel);
        }
        fn minstrel(&mut self) -> Option<wlan_mlme::MinstrelWrapper> {
            self.minstrel.clone()
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    async fn compile_actual_private_provenance_route<D: wlan_mlme::device::DeviceOps>(
        mlme: &mut wlan_mlme::client::ClientMlme<D>,
        session: &mut ProvenanceSession,
    ) {
        let carried = session
            .source
            .seal_frame(
                DescriptorOccurrenceRoute::DataRx,
                2,
                0,
                passive_advertisement_frame(),
            )
            .unwrap();
        let PrivateFrameSeal::Carried(carrier) = carried else {
            panic!("actual B1 occurrence was not minted");
        };
        // P is inferred here as the binary-private ProvenanceHandle. Neither
        // the adapter nor pinned MLME names the B1 DescriptorOccurrence.
        let rx = session.admit_carrier(carrier).unwrap();
        mt7921_softmac_adapter::handle_pinned_client_rx(mlme, rx, session).await;
    }

    #[cfg(feature = "fuchsia-passive")]
    fn b2a_scan_result(tag: u8) -> fidl_fuchsia_wlan_mlme::ScanResult {
        let channel = fidl_fuchsia_wlan_ieee80211::ChannelNumber {
            band: fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz,
            number: 1,
        };
        fidl_fuchsia_wlan_mlme::ScanResult {
            txn_id: 17,
            timestamp_nanos: 23,
            bss: fidl_fuchsia_wlan_ieee80211::BssDescription {
                bssid: [1, 2, 3, 4, 5, 6],
                bss_type: fidl_fuchsia_wlan_ieee80211::BssType::Infrastructure,
                beacon_period: 100,
                capability_info: 1,
                ies: vec![0, 1, tag],
                primary: channel,
                bandwidth: fidl_fuchsia_wlan_ieee80211::ChannelBandwidth::Cbw20,
                vht_secondary_80_channel: fidl_fuchsia_wlan_ieee80211::ChannelNumber {
                    number: 0,
                    ..channel
                },
                rssi_dbm: -40,
                snr_db: 0,
            },
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    fn assert_private_terminal_corruption_fail_closes(
        corruption: wlan_sme::client::TrustedScanCorruption,
        descriptions: Vec<fidl_fuchsia_wlan_ieee80211::BssDescription>,
    ) {
        let mut session = ProvenanceSession::new();
        let scan = session
            .start_sme_scan(fidl_fuchsia_wlan_sme::ScanRequest::Passive(
                fidl_fuchsia_wlan_sme::PassiveScanRequest {
                    channels: vec![1, 6, 11],
                },
            ))
            .unwrap();
        for (ordinal, bss) in descriptions.into_iter().enumerate() {
            let carried = session
                .source
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
                .unwrap();
            let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
                occurrence: Some(occurrence),
                ..
            }) = carried
            else {
                panic!("actual B1 occurrence was not minted");
            };
            let handle = session.admit(occurrence).unwrap();
            session
                .source
                .rearm(DescriptorOccurrenceRoute::DataRx, 2, 0)
                .unwrap();
            let result = fidl_fuchsia_wlan_mlme::ScanResult {
                txn_id: scan.txn_id,
                timestamp_nanos: ordinal as i64,
                bss,
            };
            wlan_mlme::ScanResultObserver::observe(
                &mut session,
                &wlan_mlme::ScanResultDisposition::Produced(&result),
                handle,
            );
            assert!(session.route_next_sme_result().unwrap());
        }
        session
            .sme_state
            .inject_terminal_corruption_for_test(corruption);
        assert!(
            session
                .finish_sme_scan(fidl_fuchsia_wlan_mlme::ScanEnd {
                    txn_id: scan.txn_id,
                    code: fidl_fuchsia_wlan_mlme::ScanResultCode::Success,
                })
                .is_err()
        );
        assert!(session.invalidated);
        assert!(session.poisoned);
        assert!(!session.inspect_sme_output(|_| panic!("corrupt terminal emitted output")));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn private_validation_rejects_all_independent_lineage_corruptions() {
        use wlan_sme::client::{TrustedFixedField as Field, TrustedScanCorruption as Corruption};

        let base = b2a_scan_result(1).bss;
        let mut accepted = base.clone();
        accepted.rssi_dbm = -39;
        let accepted_pair = || vec![base.clone(), accepted.clone()];

        for corruption in [
            Corruption::AcceptedPermutation,
            Corruption::BadTableIndex,
            Corruption::ProducedBindingPermutation,
            Corruption::MissingRole,
            Corruption::NonLastRepresentative,
            Corruption::DuplicateSplitAggregate,
        ] {
            assert_private_terminal_corruption_fail_closes(corruption, accepted_pair());
        }
        for corruption in [
            Corruption::BadEncounter,
            Corruption::BadTxnId,
            Corruption::BadTimestamp,
            Corruption::DuplicateRole,
            Corruption::InvalidRepresentative,
        ] {
            assert_private_terminal_corruption_fail_closes(corruption, vec![base.clone()]);
        }

        let mut drop_one = base.clone();
        drop_one.primary.number = 6;
        drop_one.rssi_dbm = -50;
        let mut drop_two = base.clone();
        drop_two.primary.number = 11;
        drop_two.rssi_dbm = -60;
        assert_private_terminal_corruption_fail_closes(
            Corruption::DroppedPermutation,
            vec![base.clone(), drop_one, drop_two],
        );

        let mut other_bssid = base.clone();
        other_bssid.bssid[5] ^= 1;
        assert_private_terminal_corruption_fail_closes(
            Corruption::CrossBssidAssignment,
            vec![base.clone(), other_bssid],
        );

        for field in [
            Field::Bssid,
            Field::BssType,
            Field::BeaconPeriod,
            Field::CapabilityInfo,
            Field::Primary,
            Field::Bandwidth,
            Field::VhtSecondary80Channel,
            Field::RssiDbm,
            Field::SnrDb,
        ] {
            assert_private_terminal_corruption_fail_closes(
                Corruption::RepresentativeFixedField(field),
                vec![base.clone()],
            );
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn private_preflight_rejection_is_retained_until_synchronous_fail_close() {
        let mut session = ProvenanceSession::new();
        let released_after_invalidation = Arc::new(AtomicBool::new(false));
        session.next_handle_drop_order_probe = Some(Arc::clone(&released_after_invalidation));
        let scan = session
            .start_sme_scan(fidl_fuchsia_wlan_sme::ScanRequest::Passive(
                fidl_fuchsia_wlan_sme::PassiveScanRequest { channels: vec![1] },
            ))
            .unwrap();
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        let handle = session.admit(occurrence).unwrap();
        let mut result = b2a_scan_result(8);
        result.txn_id = scan.txn_id + 1;
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::Produced(&result),
            handle,
        );
        assert!(session.route_next_sme_result().is_err());
        assert!(session.invalidated);
        assert!(session.poisoned);
        assert!(released_after_invalidation.load(Ordering::Acquire));
        assert!(!session.inspect_sme_output(|_| panic!("rejected result emitted output")));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn private_owner_validation_failure_keeps_local_until_fail_close() {
        let mut session = ProvenanceSession::new();
        let released_after_invalidation = Arc::new(AtomicBool::new(false));
        session.next_handle_drop_order_probe = Some(Arc::clone(&released_after_invalidation));
        let scan = session
            .start_sme_scan(fidl_fuchsia_wlan_sme::ScanRequest::Passive(
                fidl_fuchsia_wlan_sme::PassiveScanRequest { channels: vec![1] },
            ))
            .unwrap();
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        let handle = session.admit(occurrence).unwrap();
        let mut result = b2a_scan_result(9);
        result.txn_id = scan.txn_id;
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::Produced(&result),
            handle,
        );
        session.generation += 1;
        assert!(session.route_next_sme_result().is_err());
        assert!(session.invalidated);
        assert!(released_after_invalidation.load(Ordering::Acquire));
    }

    #[test]
    fn rejected_route_has_no_allocating_owner_transfer_source_shape() {
        let source = include_str!("vfio_read.rs");
        let route = source
            .split("fn route_next_sme_result")
            .nth(1)
            .unwrap()
            .split("fn finish_sme_scan")
            .next()
            .unwrap();
        assert!(!route.contains("carried_results.push"));
        assert!(!route.contains("push(CarriedScanResult"));
        assert!(route.contains("self.fail_close();\n            drop(carried);"));
        assert!(route.contains("self.fail_close();\n                drop(rejected);"));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn produced_subsequence_crosses_futures_into_actual_client_sme_once() {
        let mut session = ProvenanceSession::new();
        let scan = session
            .start_sme_scan(fidl_fuchsia_wlan_sme::ScanRequest::Passive(
                fidl_fuchsia_wlan_sme::PassiveScanRequest { channels: vec![1] },
            ))
            .unwrap();
        let mut handles = Vec::new();
        for _ in 0..3 {
            let carried = session
                .source
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
                .unwrap();
            let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
                occurrence: Some(occurrence),
                ..
            }) = carried
            else {
                panic!("actual B1 occurrence was not minted");
            };
            handles.push(session.admit(occurrence).unwrap());
            session
                .source
                .rearm(DescriptorOccurrenceRoute::DataRx, 2, 0)
                .unwrap();
        }
        let ignored = handles.remove(0);
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::Ignored(
                wlan_mlme::ScanResultIgnore::NonAdvertisement,
            ),
            ignored,
        );
        let conversion = handles.remove(0);
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::ConversionDrop,
            conversion,
        );
        let produced = handles.remove(0);
        let produced_index = produced.index;
        let mut result = b2a_scan_result(9);
        result.txn_id = scan.txn_id;
        result.bss.ies = vec![0, 1, b'x', 1, 4, 2, 4, 11, 22];
        assert_eq!(
            wlan_mlme::ScanResultObserver::observe(
                &mut session,
                &wlan_mlme::ScanResultDisposition::Produced(&result),
                produced,
            ),
            wlan_mlme::ScanResultObserverControl::Suppress
        );
        assert!(session.route_next_sme_result().unwrap());
        assert!(!session.route_next_sme_result().unwrap());
        session
            .finish_sme_scan(fidl_fuchsia_wlan_mlme::ScanEnd {
                txn_id: scan.txn_id,
                code: fidl_fuchsia_wlan_mlme::ScanResultCode::Success,
            })
            .unwrap();
        assert!(session.inspect_sme_output(|output| {
            assert_eq!(output.terminal.generation(), 1);
            assert_eq!(output.terminal.txn_id(), scan.txn_id);
            assert_eq!(output.terminal.input_provenance().len(), 1);
            assert_eq!(
                output
                    .terminal
                    .input_provenance()
                    .next()
                    .expect("one produced carrier")
                    .index,
                produced_index
            );
            assert_eq!(output.terminal.bss_description_list().len(), 1);
        }));
        session.finish().unwrap();
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn trusted_sme_cannot_finish_without_consuming_terminal() {
        let mut session = ProvenanceSession::new();
        let scan = session
            .start_sme_scan(fidl_fuchsia_wlan_sme::ScanRequest::Passive(
                fidl_fuchsia_wlan_sme::PassiveScanRequest { channels: vec![1] },
            ))
            .unwrap();
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        let handle = session.admit(occurrence).unwrap();
        let mut result = b2a_scan_result(5);
        result.txn_id = scan.txn_id;
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::Produced(&result),
            handle,
        );
        session.route_next_sme_result().unwrap();
        assert!(session.finish().is_err());
        assert!(session.invalidated);
        assert!(session.sme_output.is_none());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn trusted_sme_sink_failure_invalidates_owner_before_affine_release() {
        let mut session = ProvenanceSession::new();
        let released_after_invalidation = Arc::new(AtomicBool::new(false));
        session.next_handle_drop_order_probe = Some(Arc::clone(&released_after_invalidation));
        let scan = session
            .start_sme_scan(fidl_fuchsia_wlan_sme::ScanRequest::Passive(
                fidl_fuchsia_wlan_sme::PassiveScanRequest { channels: vec![1] },
            ))
            .unwrap();
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        let handle = session.admit(occurrence).unwrap();
        let mut result = b2a_scan_result(4);
        result.txn_id = scan.txn_id;
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::Produced(&result),
            handle,
        );
        session.route_next_sme_result().unwrap();
        session.reject_next_sme_output = true;
        assert!(
            session
                .finish_sme_scan(fidl_fuchsia_wlan_mlme::ScanEnd {
                    txn_id: scan.txn_id,
                    code: fidl_fuchsia_wlan_mlme::ScanResultCode::Success,
                })
                .is_err()
        );
        assert!(session.invalidated);
        assert!(session.poisoned);
        assert!(released_after_invalidation.load(Ordering::Acquire));
        assert!(session.sme_output.is_none());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn trusted_sme_result_aggregation_panic_retains_affine_input_until_invalidation() {
        let mut session = ProvenanceSession::new();
        let released_after_invalidation = Arc::new(AtomicBool::new(false));
        session.next_handle_drop_order_probe = Some(Arc::clone(&released_after_invalidation));
        let scan = session
            .start_sme_scan(fidl_fuchsia_wlan_sme::ScanRequest::Passive(
                fidl_fuchsia_wlan_sme::PassiveScanRequest { channels: vec![1] },
            ))
            .unwrap();
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        let handle = session.admit(occurrence).unwrap();
        let mut result = b2a_scan_result(7);
        result.txn_id = scan.txn_id;
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::Produced(&result),
            handle,
        );
        session.panic_next_sme_result_aggregation = true;
        assert!(session.route_next_sme_result().is_err());
        assert!(session.invalidated);
        assert!(session.poisoned);
        assert!(!session.inspect_sme_output(|_| panic!("result panic emitted output")));
        assert!(released_after_invalidation.load(Ordering::Acquire));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn trusted_sme_validation_panic_keeps_affine_input_under_owner_until_invalidation() {
        let mut session = ProvenanceSession::new();
        let released_after_invalidation = Arc::new(AtomicBool::new(false));
        session.next_handle_drop_order_probe = Some(Arc::clone(&released_after_invalidation));
        let scan = session
            .start_sme_scan(fidl_fuchsia_wlan_sme::ScanRequest::Passive(
                fidl_fuchsia_wlan_sme::PassiveScanRequest { channels: vec![1] },
            ))
            .unwrap();
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        let handle = session.admit(occurrence).unwrap();
        let mut result = b2a_scan_result(6);
        result.txn_id = scan.txn_id;
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::Produced(&result),
            handle,
        );
        session.route_next_sme_result().unwrap();
        session.panic_next_sme_validation = true;
        assert!(
            session
                .finish_sme_scan(fidl_fuchsia_wlan_mlme::ScanEnd {
                    txn_id: scan.txn_id,
                    code: fidl_fuchsia_wlan_mlme::ScanResultCode::Success,
                })
                .is_err()
        );
        assert!(session.invalidated);
        assert!(session.poisoned);
        assert!(!session.inspect_sme_output(|_| panic!("unvalidated output escaped")));
        assert!(released_after_invalidation.load(Ordering::Acquire));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn private_session_owns_actual_b1_registration_and_invalidates_once() {
        let mut session = ProvenanceSession::new();
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![1, 2, 3])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        let handle = session.admit(occurrence).unwrap();
        assert_eq!(session.validate_index(&handle), Ok(0));
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::Ignored(
                wlan_mlme::ScanResultIgnore::NonAdvertisement,
            ),
            handle,
        );
        session.finish().unwrap();
        session.finish().unwrap_err();
        assert!(!session.invalidated);
        assert!(session.source.sealed.is_empty());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn private_session_caps_first_4096_without_slot_reuse_and_poison_is_permanent() {
        let mut session = ProvenanceSession::new();
        for index in 0..ProvenanceSession::CAPACITY {
            let carried = session
                .source
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
                .unwrap();
            let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
                occurrence: Some(occurrence),
                ..
            }) = carried
            else {
                panic!("actual B1 occurrence {index} was not minted");
            };
            let handle = session.admit(occurrence).unwrap();
            assert_eq!(handle.index, index);
            session
                .source
                .rearm(DescriptorOccurrenceRoute::DataRx, 2, 0)
                .unwrap();
        }
        let stale_identity = session.registrations[0].occurrence.identity;
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("4097th actual B1 occurrence was not retained");
        };
        let overflow_lease = Arc::clone(&occurrence.lease);
        assert_eq!(
            session
                .admit_carrier(PrivateRawFrameCarrier {
                    bytes: vec![9, 8, 7],
                    occurrence: Some(occurrence),
                })
                .err()
                .unwrap(),
            "provenance session arena exhausted"
        );
        assert!(!overflow_lease.current.load(Ordering::Acquire));
        assert!(session.closed);
        assert!(session.poisoned);
        assert!(session.invalidated);
        assert!(session.quarantine.is_empty());
        assert!(
            session
                .admit(DescriptorOccurrence {
                    identity: stale_identity,
                    lease: Arc::clone(&session.source.lease),
                })
                .is_err()
        );
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn private_session_rejects_mismatch_cancellation_and_late_handles() {
        let mut session = ProvenanceSession::new();
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::McuNormalRx, 0, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual MCU-normal B1 occurrence was not minted");
        };
        let handle = session.admit(occurrence).unwrap();
        let mismatch = ProvenanceHandle {
            session: NonZeroU64::new(handle.session.get().checked_add(1).unwrap()).unwrap(),
            generation: handle.generation,
            index: handle.index,
            drop_order_probe: None,
        };
        assert!(session.validate_index(&mismatch).is_err());
        session
            .source
            .invalidate(DescriptorInvalidation::Cancellation)
            .unwrap();
        assert!(session.validate_index(&handle).is_err());
        session.fail_close();
        assert!(session.validate_index(&handle).is_err());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn successful_stale_session_handle_rejection_does_not_poison_new_session() {
        let mut old = ProvenanceSession::new();
        let carried = old
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        let stale = old.admit(occurrence).unwrap();
        let mut new = ProvenanceSession::new();
        assert!(new.validate_index(&stale).is_err());
        assert!(!new.poisoned);
        assert!(!new.closed);
        new.finish().unwrap();
        old.fail_close();
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn ingress_batch_preserves_carriers_in_owner_order_through_real_client_mlme() {
        futures::executor::block_on(async {
            use wlan_mlme::MlmeImpl;

            let mut session = ProvenanceSession::new();
            let mut carriers = Vec::new();
            for (ordinal, route, ring) in [
                (1u8, DescriptorOccurrenceRoute::McuNormalRx, 4),
                (2, DescriptorOccurrenceRoute::DataRx, 2),
                (3, DescriptorOccurrenceRoute::McuNormalRx, 0),
            ] {
                let mut bytes = passive_advertisement_frame();
                *bytes.last_mut().unwrap() = b'0' + ordinal;
                let rcpi = 120 - 20 * (ordinal - 1);
                bytes[28..32].copy_from_slice(&(u32::from(rcpi) * 0x0101).to_le_bytes());
                if route == DescriptorOccurrenceRoute::McuNormalRx {
                    let len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) & 0xffff;
                    bytes[0..4].copy_from_slice(&((7 << 27) | (1 << 16) | len).to_le_bytes());
                }
                let carried = session.source.seal_frame(route, ring, 0, bytes).unwrap();
                let PrivateFrameSeal::Carried(carrier) = carried else {
                    panic!("actual B1 occurrence was not minted");
                };
                carriers.push(carrier);
            }
            let expected = carriers
                .iter()
                .map(|carrier| carrier.occurrence.as_ref().unwrap().identity.occurrence)
                .collect::<Vec<_>>();
            for carrier in carriers.into_iter().rev() {
                session.enqueue(carrier).unwrap();
            }
            let ordered_rx = session.flush_ingress().unwrap();
            assert_eq!(
                session
                    .registrations
                    .iter()
                    .map(|registration| registration.occurrence.identity.occurrence)
                    .collect::<Vec<_>>(),
                expected
            );

            let (device, events) = B2aFakeDevice::new();
            let (timer, _) = wlan_mlme::common::timer::create_timer();
            let mut mlme = wlan_mlme::client::ClientMlme::new(Default::default(), device, timer)
                .await
                .unwrap();
            mlme.handle_mlme_request(wlan_sme::MlmeRequest::Scan(
                fidl_fuchsia_wlan_mlme::ScanRequest {
                    txn_id: 77,
                    scan_type: fidl_fuchsia_wlan_mlme::ScanTypes::Passive,
                    channel_list: vec![fidl_fuchsia_wlan_ieee80211::ChannelNumber {
                        band: fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz,
                        number: 1,
                    }],
                    ssid_list: vec![],
                    probe_delay: 0,
                    min_channel_time: 10,
                    max_channel_time: 20,
                },
            ))
            .await
            .unwrap();
            for rx in ordered_rx {
                mt7921_softmac_adapter::handle_pinned_client_rx(&mut mlme, rx, &mut session).await;
            }
            assert_eq!(session.observed_order, expected);
            // Produced-only private admission suppresses the parallel ordinary
            // MLME event; the futures sidecar is the sole SME input ledger.
            assert!(events.lock().unwrap().is_empty());
            for (expected_tag, expected_rssi) in [(b'1', -50), (b'2', -60), (b'3', -70)] {
                assert!(session.inspect_next_result(|carried| {
                    assert_eq!(*carried.result.bss.ies.last().unwrap(), expected_tag);
                    assert_eq!(carried.result.bss.rssi_dbm, expected_rssi);
                }));
            }
            session.finish().unwrap();
        });
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn real_client_mlme_suppresses_results_rejected_as_out_of_order_or_stale() {
        futures::executor::block_on(async {
            use wlan_mlme::MlmeImpl;

            let mut session = ProvenanceSession::new();
            let mut admitted = Vec::new();
            for (route, ring) in [
                (DescriptorOccurrenceRoute::DataRx, 2),
                (DescriptorOccurrenceRoute::McuNormalRx, 0),
            ] {
                let carried = session
                    .source
                    .seal_frame(route, ring, 0, passive_advertisement_frame())
                    .unwrap();
                let PrivateFrameSeal::Carried(carrier) = carried else {
                    panic!("actual B1 occurrence was not minted");
                };
                admitted.push(session.admit_carrier(carrier).unwrap());
            }
            let first = admitted.remove(0);
            let second = admitted.remove(0);

            let (device, events) = B2aFakeDevice::new();
            let (timer, _) = wlan_mlme::common::timer::create_timer();
            let mut mlme = wlan_mlme::client::ClientMlme::new(Default::default(), device, timer)
                .await
                .unwrap();
            mlme.handle_mlme_request(wlan_sme::MlmeRequest::Scan(
                fidl_fuchsia_wlan_mlme::ScanRequest {
                    txn_id: 78,
                    scan_type: fidl_fuchsia_wlan_mlme::ScanTypes::Passive,
                    channel_list: vec![fidl_fuchsia_wlan_ieee80211::ChannelNumber {
                        band: fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz,
                        number: 1,
                    }],
                    ssid_list: vec![],
                    probe_delay: 0,
                    min_channel_time: 10,
                    max_channel_time: 20,
                },
            ))
            .await
            .unwrap();

            mt7921_softmac_adapter::handle_pinned_client_rx(&mut mlme, second, &mut session).await;
            assert!(session.invalidated);
            assert!(events.lock().unwrap().is_empty());

            // The formerly first handle is stale after fail-close and must
            // remain unable to escape through the ordinary MLME event path.
            mt7921_softmac_adapter::handle_pinned_client_rx(&mut mlme, first, &mut session).await;
            assert!(events.lock().unwrap().is_empty());
        });
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn real_client_mlme_suppresses_result_when_private_sidecar_sink_is_closed() {
        futures::executor::block_on(async {
            use wlan_mlme::MlmeImpl;

            let mut session = ProvenanceSession::new();
            let released_after_invalidation = Arc::new(AtomicBool::new(false));
            session.next_handle_drop_order_probe = Some(Arc::clone(&released_after_invalidation));
            let carried = session
                .source
                .seal_frame(
                    DescriptorOccurrenceRoute::DataRx,
                    2,
                    0,
                    passive_advertisement_frame(),
                )
                .unwrap();
            let PrivateFrameSeal::Carried(carrier) = carried else {
                panic!("actual B1 occurrence was not minted");
            };
            let admitted = session.admit_carrier(carrier).unwrap();
            session.result_rx.close();

            let (device, events) = B2aFakeDevice::new();
            let (timer, _) = wlan_mlme::common::timer::create_timer();
            let mut mlme = wlan_mlme::client::ClientMlme::new(Default::default(), device, timer)
                .await
                .unwrap();
            mlme.handle_mlme_request(wlan_sme::MlmeRequest::Scan(
                fidl_fuchsia_wlan_mlme::ScanRequest {
                    txn_id: 79,
                    scan_type: fidl_fuchsia_wlan_mlme::ScanTypes::Passive,
                    channel_list: vec![fidl_fuchsia_wlan_ieee80211::ChannelNumber {
                        band: fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz,
                        number: 1,
                    }],
                    ssid_list: vec![],
                    probe_delay: 0,
                    min_channel_time: 10,
                    max_channel_time: 20,
                },
            ))
            .await
            .unwrap();

            mt7921_softmac_adapter::handle_pinned_client_rx(&mut mlme, admitted, &mut session)
                .await;
            assert!(session.invalidated);
            assert!(session.poisoned);
            assert!(released_after_invalidation.load(Ordering::Acquire));
            assert!(events.lock().unwrap().is_empty());
        });
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn produced_only_route_bypasses_parallel_mlme_event_transport() {
        futures::executor::block_on(async {
            use wlan_mlme::MlmeImpl;

            let mut session = ProvenanceSession::new();
            let carried = session
                .source
                .seal_frame(
                    DescriptorOccurrenceRoute::DataRx,
                    2,
                    0,
                    passive_advertisement_frame(),
                )
                .unwrap();
            let PrivateFrameSeal::Carried(carrier) = carried else {
                panic!("actual B1 occurrence was not minted");
            };
            let admitted = session.admit_carrier(carrier).unwrap();

            let (device, events) = B2aFakeDevice::new_with_failed_mlme_transport();
            let (timer, _) = wlan_mlme::common::timer::create_timer();
            let mut mlme = wlan_mlme::client::ClientMlme::new(Default::default(), device, timer)
                .await
                .unwrap();
            mlme.handle_mlme_request(wlan_sme::MlmeRequest::Scan(
                fidl_fuchsia_wlan_mlme::ScanRequest {
                    txn_id: 80,
                    scan_type: fidl_fuchsia_wlan_mlme::ScanTypes::Passive,
                    channel_list: vec![fidl_fuchsia_wlan_ieee80211::ChannelNumber {
                        band: fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz,
                        number: 1,
                    }],
                    ssid_list: vec![],
                    probe_delay: 0,
                    min_channel_time: 10,
                    max_channel_time: 20,
                },
            ))
            .await
            .unwrap();

            mt7921_softmac_adapter::handle_pinned_client_rx(&mut mlme, admitted, &mut session)
                .await;
            assert!(!session.invalidated);
            assert!(!session.poisoned);
            assert_eq!(session.outstanding_results, 1);
            assert!(events.lock().unwrap().is_empty());
            session.fail_close();
            assert!(session.invalidated);
            assert_eq!(session.outstanding_results, 0);
        });
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn reordered_ingress_handle_fail_closes_before_classification() {
        let mut session = ProvenanceSession::new();
        let mut handles = Vec::new();
        for (route, ring) in [
            (DescriptorOccurrenceRoute::McuNormalRx, 4),
            (DescriptorOccurrenceRoute::DataRx, 2),
        ] {
            let carried = session.source.seal_frame(route, ring, 0, vec![]).unwrap();
            let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
                occurrence: Some(occurrence),
                ..
            }) = carried
            else {
                panic!("actual B1 occurrence was not minted");
            };
            handles.push(session.admit(occurrence).unwrap());
        }
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::Ignored(
                wlan_mlme::ScanResultIgnore::NonAdvertisement,
            ),
            handles.remove(1),
        );
        assert!(session.invalidated);
        assert!(session.poisoned);
        assert!(session.registrations.is_empty());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn queued_ingress_counts_toward_the_single_4096_cap() {
        let mut session = ProvenanceSession::new();
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        session
            .enqueue(PrivateRawFrameCarrier {
                bytes: Vec::new(),
                occurrence: Some(occurrence),
            })
            .unwrap();
        session
            .source
            .rearm(DescriptorOccurrenceRoute::DataRx, 2, 0)
            .unwrap();
        for _ in 1..ProvenanceSession::CAPACITY {
            let carried = session
                .source
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
                .unwrap();
            let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
                occurrence: Some(occurrence),
                ..
            }) = carried
            else {
                panic!("actual B1 occurrence was not minted");
            };
            session
                .enqueue(PrivateRawFrameCarrier {
                    bytes: Vec::new(),
                    occurrence: Some(occurrence),
                })
                .unwrap();
            session
                .source
                .rearm(DescriptorOccurrenceRoute::DataRx, 2, 0)
                .unwrap();
        }
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("4097th actual B1 occurrence was not minted");
        };
        assert!(
            session
                .enqueue(PrivateRawFrameCarrier {
                    bytes: Vec::new(),
                    occurrence: Some(occurrence),
                })
                .is_err()
        );
        assert!(session.invalidated);
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn transport_failure_and_unclassified_finish_invalidate_before_release() {
        let mut transport_failed = ProvenanceSession::new();
        let carried = transport_failed
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        let handle = transport_failed.admit(occurrence).unwrap();
        let result = b2a_scan_result(1);
        wlan_mlme::ScanResultObserver::observe(
            &mut transport_failed,
            &wlan_mlme::ScanResultDisposition::Produced(&result),
            handle,
        );
        assert_eq!(transport_failed.outstanding_results, 1);
        wlan_mlme::ScanResultObserver::<ProvenanceHandle>::observe_transport_failure(
            &mut transport_failed,
        );
        assert!(transport_failed.invalidated);
        assert!(transport_failed.registrations.is_empty());
        assert_eq!(transport_failed.outstanding_results, 0);
        assert!(transport_failed.result_rx.try_recv().is_err());
        assert!(transport_failed.carried_results.is_empty());

        let mut unclassified = ProvenanceSession::new();
        let carried = unclassified
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        unclassified.admit(occurrence).unwrap();
        assert!(unclassified.finish().is_err());
        assert!(unclassified.invalidated);
        assert!(unclassified.registrations.is_empty());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn uncovered_carrier_invalidates_and_clears_prior_produced_result_before_release() {
        let mut session = ProvenanceSession::new();
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        let handle = session.admit(occurrence).unwrap();
        let result = b2a_scan_result(3);
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::Produced(&result),
            handle,
        );
        assert_eq!(session.outstanding_results, 1);
        assert!(
            session
                .admit_carrier(PrivateRawFrameCarrier {
                    bytes: vec![1, 2, 3],
                    occurrence: None,
                })
                .is_err()
        );
        assert!(session.invalidated);
        assert_eq!(session.outstanding_results, 0);
        assert!(session.result_rx.try_recv().is_err());
        assert!(session.carried_results.is_empty());
        assert!(session.registrations.is_empty());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn private_session_alone_validates_and_exports_exact_result_once_over_mpsc() {
        let mut session = ProvenanceSession::new();
        let carried = session
            .source
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![])
            .unwrap();
        let PrivateFrameSeal::Carried(PrivateRawFrameCarrier {
            occurrence: Some(occurrence),
            ..
        }) = carried
        else {
            panic!("actual B1 occurrence was not minted");
        };
        let handle = session.admit(occurrence).unwrap();
        let generation = handle.generation;
        let index = handle.index;
        let result = b2a_scan_result(2);
        wlan_mlme::ScanResultObserver::observe(
            &mut session,
            &wlan_mlme::ScanResultDisposition::Produced(&result),
            handle,
        );
        assert!(session.inspect_next_result(|carried| {
            assert_eq!(carried.provenance.generation, generation);
            assert_eq!(carried.provenance.index, index);
            assert_eq!(carried.result, result);
        }));
        assert_eq!(session.carried_results.len(), 1);
        assert_eq!(session.source.sealed.len(), 1);
        assert_eq!(
            session.registrations[index].disposition,
            RegistrationDisposition::Produced
        );
        session.finish().unwrap();
        assert!(session.closed);
        assert!(!session.poisoned);
        assert!(!session.invalidated);
        assert!(session.source.sealed.is_empty());
        assert!(session.carried_results.is_empty());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn private_session_drop_invalidates_before_panic_abandonment_releases_fields() {
        let lease = std::panic::catch_unwind(|| {
            let session = ProvenanceSession::new();
            let lease = Arc::clone(&session.source.lease);
            std::panic::panic_any(lease);
        })
        .unwrap_err()
        .downcast::<Arc<DescriptorOccurrenceLease>>()
        .unwrap();
        assert!(!lease.current.load(Ordering::Acquire));
    }

    struct TestMapping {
        ptr: NonNull<u8>,
        len: usize,
    }

    impl TestMapping {
        fn new(len: usize) -> Self {
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
            .unwrap();
            Self { ptr, len }
        }

        fn dma(&self, iova: u64) -> DmaArena {
            DmaArena {
                iommu: Arc::new(File::open("/dev/null").unwrap()),
                ioas: 0,
                ptr: self.ptr,
                len: self.len,
                iova,
                mapped: false,
            }
        }

        fn read_page(&self) -> ReadPage {
            assert_eq!(self.len, PAGE);
            ReadPage {
                ptr: self.ptr,
                bar_page: 0xd4000,
                active_rx_irq_mask: Cell::new(WM_RX_IRQ_BIT | WM2_RX_IRQ_BIT),
                mapped: false,
            }
        }
    }

    impl Drop for TestMapping {
        fn drop(&mut self) {
            assert_eq!(unsafe { munmap(self.ptr.as_ptr(), self.len) }, 0);
        }
    }

    fn carried(seal: PrivateFrameSeal) -> PrivateRawFrameCarrier {
        match seal {
            PrivateFrameSeal::Carried(carrier) => carrier,
            PrivateFrameSeal::Uncovered(_) => panic!("expected covered descriptor route"),
        }
    }

    fn occurrence(carrier: PrivateRawFrameCarrier) -> DescriptorOccurrence {
        carrier.occurrence.expect("covered carrier has provenance")
    }

    #[test]
    fn descriptor_occurrences_are_minted_before_rearm_on_both_routes() {
        let mut provenance = DescriptorProvenance::new();
        let mcu = carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::McuNormalRx, 0, 0, vec![1, 2, 3])
                .unwrap(),
        );
        provenance
            .rearm(DescriptorOccurrenceRoute::McuNormalRx, 0, 7)
            .unwrap();
        let data = carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 3, vec![4, 5])
                .unwrap(),
        );
        provenance
            .rearm(DescriptorOccurrenceRoute::DataRx, 2, 7)
            .unwrap();

        assert_eq!(mcu.bytes, [1, 2, 3]);
        assert_eq!(data.bytes, [4, 5]);
        let mcu_identity = mcu.occurrence.as_ref().unwrap().identity;
        let data_identity = data.occurrence.as_ref().unwrap().identity;
        assert!(mcu_identity.route == DescriptorOccurrenceRoute::McuNormalRx);
        assert_eq!((mcu_identity.ring, mcu_identity.slot), (0, 0));
        assert!(data_identity.route == DescriptorOccurrenceRoute::DataRx);
        assert_eq!((data_identity.ring, data_identity.slot), (2, 3));
        assert_eq!((mcu_identity.occurrence, data_identity.occurrence), (1, 2));
        assert!(matches!(
            provenance.effects.as_slice(),
            [
                DescriptorProvenanceEffect::Mint(first),
                DescriptorProvenanceEffect::Rearm {
                    route: DescriptorOccurrenceRoute::McuNormalRx,
                    ring: 0,
                    slot: 7,
                    ..
                },
                DescriptorProvenanceEffect::Mint(second),
                DescriptorProvenanceEffect::Rearm {
                    route: DescriptorOccurrenceRoute::DataRx,
                    ring: 2,
                    slot: 7,
                    ..
                }
            ] if *first == mcu_identity && *second == data_identity
        ));
    }

    #[test]
    fn actual_mcu_normal_drain_mints_before_physical_rearm_and_index_publish() {
        let ring_mapping = TestMapping::new(PAGE);
        let buffer_mapping = TestMapping::new(8 * 2048);
        let page_mapping = TestMapping::new(PAGE);
        let mut ring = ring_mapping.dma(0x0100_0000);
        let mut buffers = buffer_mapping.dma(0x0101_0000);
        let page = page_mapping.read_page();
        let mut frame = vec![0; 40];
        frame[0..4].copy_from_slice(&((7u32 << 27) | (1 << 16)).to_le_bytes());
        buffers.write_bytes_at(0, &frame).unwrap();
        ring.write_descriptor_at(
            0,
            DmaDescriptor {
                buf0: buffers.iova as u32,
                ctrl: (1 << 31) | (1 << 30) | (frame.len() as u32) << 16,
                buf1: 0,
                info: 0,
            },
        );
        let mut queue = ActiveMcuRx {
            rx_ring: &mut ring,
            rx_buffers: &buffers,
            rx_tail: 0,
            rx_head: 7,
            rx_ring_index: 0,
            rx_count: 8,
            irq_bit: WM_RX_IRQ_BIT,
        };
        let mut unsolicited = Vec::new();
        let mut normal = Vec::new();
        let mut provenance = DescriptorProvenance::new();
        assert!(
            drain_rx_queue(
                &page,
                &mut queue,
                None,
                &mut unsolicited,
                &mut normal,
                &mut provenance,
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(normal.len(), 1);
        assert_eq!(normal[0].bytes, frame);
        assert!(matches!(
            provenance.effects.as_slice(),
            [
                DescriptorProvenanceEffect::Mint(_),
                DescriptorProvenanceEffect::Rearm {
                    route: DescriptorOccurrenceRoute::McuNormalRx,
                    ring: 0,
                    slot: 7,
                    ..
                }
            ]
        ));
        assert_eq!(queue.rx_tail, 1);
        assert_eq!(queue.rx_head, 0);
        assert_eq!(
            queue.rx_ring.read_descriptor_at(7).buf0,
            buffers.iova as u32 + 7 * 2048
        );
    }

    #[test]
    fn actual_mcu_drain_revokes_current_and_queued_carriers_before_later_error() {
        let ring_mapping = TestMapping::new(PAGE);
        let buffer_mapping = TestMapping::new(8 * 2048);
        let page_mapping = TestMapping::new(PAGE);
        let mut ring = ring_mapping.dma(0x0104_0000);
        let mut buffers = buffer_mapping.dma(0x0105_0000);
        let page = page_mapping.read_page();
        let mut frame = vec![0; 40];
        frame[0..4].copy_from_slice(&((7u32 << 27) | (1 << 16)).to_le_bytes());
        buffers.write_bytes_at(0, &frame).unwrap();
        ring.write_descriptor_at(
            0,
            DmaDescriptor {
                buf0: buffers.iova as u32,
                ctrl: (1 << 31) | (1 << 30) | (frame.len() as u32) << 16,
                buf1: 0,
                info: 0,
            },
        );
        ring.write_descriptor_at(
            1,
            DmaDescriptor {
                buf0: buffers.iova as u32 + 2048,
                ctrl: (1 << 31) | (1 << 30) | (35 << 16),
                buf1: 0,
                info: 0,
            },
        );
        let mut queue = ActiveMcuRx {
            rx_ring: &mut ring,
            rx_buffers: &buffers,
            rx_tail: 0,
            rx_head: 7,
            rx_ring_index: 0,
            rx_count: 8,
            irq_bit: WM_RX_IRQ_BIT,
        };
        let mut unsolicited = Vec::new();
        let mut normal = Vec::new();
        let mut provenance = DescriptorProvenance::new();
        let lease = Arc::clone(&provenance.lease);
        let error = match drain_rx_queue(
            &page,
            &mut queue,
            None,
            &mut unsolicited,
            &mut normal,
            &mut provenance,
        ) {
            Ok(_) => panic!("later invalid MCU descriptor unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.contains("invalid MCU response descriptor length"));
        assert!(normal.is_empty());
        assert!(provenance.sealed.is_empty());
        assert!(!lease.current.load(Ordering::Acquire));
    }

    #[cfg(feature = "fuchsia-passive")]
    fn passive_advertisement_frame() -> Vec<u8> {
        let mut rx = vec![0; 24 + 8 + 36 + 5];
        let length = rx.len() as u32;
        rx[0..4].copy_from_slice(&((2u32 << 27) | length).to_le_bytes());
        rx[4..8].copy_from_slice(&(1u32 << 13).to_le_bytes());
        rx[12..16].copy_from_slice(&(1u32 << 8).to_le_bytes());
        rx[28..32].copy_from_slice(&0x7878u32.to_le_bytes());
        let frame = &mut rx[32..];
        frame[0..2].copy_from_slice(&0x0080u16.to_le_bytes());
        frame[16..22].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        frame[32..34].copy_from_slice(&100u16.to_le_bytes());
        frame[34..36].copy_from_slice(&0x0431u16.to_le_bytes());
        frame[36..].copy_from_slice(&[0, 3, b'a', b'p', b'1']);
        rx
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn actual_data_drain_revokes_earlier_carrier_before_later_error_release() {
        let ring_mapping = TestMapping::new(PAGE);
        let buffer_mapping = TestMapping::new(8 * 2048);
        let page_mapping = TestMapping::new(PAGE);
        let mut ring = ring_mapping.dma(0x0102_0000);
        let mut buffers = buffer_mapping.dma(0x0103_0000);
        let page = page_mapping.read_page();
        let frame = passive_advertisement_frame();
        buffers.write_bytes_at(0, &frame).unwrap();
        ring.write_descriptor_at(
            0,
            DmaDescriptor {
                buf0: buffers.iova as u32,
                ctrl: (1 << 31) | (1 << 30) | (frame.len() as u32) << 16,
                buf1: 0,
                info: 0,
            },
        );
        ring.write_descriptor_at(
            1,
            DmaDescriptor {
                buf0: buffers.iova as u32 + 2048,
                ctrl: (1 << 31) | (1 << 30) | (23 << 16),
                buf1: 0,
                info: 0,
            },
        );
        let mut queue = ActiveMcuRx {
            rx_ring: &mut ring,
            rx_buffers: &buffers,
            rx_tail: 0,
            rx_head: 7,
            rx_ring_index: 2,
            rx_count: 8,
            irq_bit: DATA_RX_IRQ_BIT,
        };
        let mut provenance = DescriptorProvenance::new();
        let lease = Arc::clone(&provenance.lease);
        let error = match drain_data_rx_queue(&page, &mut queue, &mut provenance, &mut Vec::new()) {
            Ok(_) => panic!("later invalid descriptor unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.contains("invalid data RX descriptor length"));
        assert!(!lease.current.load(Ordering::Acquire));
        assert!(provenance.sealed.is_empty());
        assert!(matches!(
            provenance.effects.as_slice(),
            [
                DescriptorProvenanceEffect::Mint(_),
                DescriptorProvenanceEffect::Rearm { .. },
                DescriptorProvenanceEffect::Rearm { .. },
                DescriptorProvenanceEffect::Invalidate(DescriptorInvalidation::Run),
            ]
        ));
    }

    #[test]
    fn duplicate_is_rejected_but_sealed_occurrence_survives_slot_reuse() {
        let mut provenance = DescriptorProvenance::new();
        let original = carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::McuNormalRx, 4, 1, vec![1])
                .unwrap(),
        );
        let duplicate = provenance
            .seal_frame(DescriptorOccurrenceRoute::McuNormalRx, 4, 1, vec![2])
            .unwrap();
        assert!(matches!(duplicate, PrivateFrameSeal::Uncovered(bytes) if bytes == [2]));
        let original = occurrence(original);
        provenance.validate(&original).unwrap();
        provenance
            .rearm(DescriptorOccurrenceRoute::McuNormalRx, 4, 1)
            .unwrap();
        provenance.validate(&original).unwrap();
        let replacement = carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::McuNormalRx, 4, 1, vec![3])
                .unwrap(),
        );
        let replacement = occurrence(replacement);
        assert!(replacement.identity.slot_epoch > original.identity.slot_epoch);
        provenance.validate(&original).unwrap();
        provenance.validate(&replacement).unwrap();
    }

    #[test]
    fn early_carriers_remain_sealed_across_a_multi_descriptor_wrap() {
        let mut provenance = DescriptorProvenance::new();
        let mut sealed = Vec::new();
        for (completed, refill) in (0..7).zip([7, 0, 1, 2, 3, 4, 5]) {
            let carrier = carried(
                provenance
                    .seal_frame(
                        DescriptorOccurrenceRoute::DataRx,
                        2,
                        completed,
                        vec![completed as u8],
                    )
                    .unwrap(),
            );
            sealed.push(occurrence(carrier));
            provenance
                .rearm(DescriptorOccurrenceRoute::DataRx, 2, refill)
                .unwrap();
        }
        for occurrence in &sealed {
            provenance.validate(occurrence).unwrap();
        }

        let wrapped = occurrence(carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 7, vec![7])
                .unwrap(),
        ));
        provenance
            .rearm(DescriptorOccurrenceRoute::DataRx, 2, 6)
            .unwrap();
        provenance.validate(&sealed[0]).unwrap();
        provenance.validate(&wrapped).unwrap();

        let replacement = occurrence(carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![8])
                .unwrap(),
        ));
        assert_eq!(replacement.identity.slot, sealed[0].identity.slot);
        assert!(replacement.identity.slot_epoch > sealed[0].identity.slot_epoch);
        provenance.validate(&sealed[0]).unwrap();
        provenance.validate(&replacement).unwrap();
    }

    #[test]
    fn non_advertisement_mcu_descriptors_advance_slots_without_minting() {
        let mut provenance = DescriptorProvenance::new();
        for (completed, refill) in (0..7).zip([7, 0, 1, 2, 3, 4, 5]) {
            provenance.consume_without_mint(DescriptorOccurrenceRoute::McuNormalRx, 0, completed);
            provenance
                .rearm(DescriptorOccurrenceRoute::McuNormalRx, 0, refill)
                .unwrap();
        }
        assert!(!provenance.poisoned);
        assert!(
            provenance
                .effects
                .iter()
                .all(|effect| !matches!(effect, DescriptorProvenanceEffect::Mint(_)))
        );
        let advertisement = carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::McuNormalRx, 0, 7, vec![8])
                .unwrap(),
        );
        assert_eq!(occurrence(advertisement).identity.slot, 7);
    }

    #[test]
    fn occurrence_and_slot_epoch_exhaustion_poison_without_reusing_identity() {
        let mut occurrence_exhausted = DescriptorProvenance::new();
        occurrence_exhausted.next_occurrence = u64::MAX;
        let seal = occurrence_exhausted
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![9])
            .unwrap();
        assert!(matches!(seal, PrivateFrameSeal::Uncovered(bytes) if bytes == [9]));
        assert!(occurrence_exhausted.poisoned);

        let mut slot_exhausted = DescriptorProvenance::new();
        slot_exhausted
            .ring_mut(DescriptorOccurrenceRoute::DataRx, 2)
            .unwrap()
            .slots[7] = DescriptorSlotState::Vacant(u64::MAX);
        slot_exhausted
            .rearm(DescriptorOccurrenceRoute::DataRx, 2, 7)
            .unwrap();
        assert!(slot_exhausted.poisoned);
        assert!(matches!(
            slot_exhausted.effects.last(),
            Some(DescriptorProvenanceEffect::Poison)
        ));
    }

    #[test]
    fn owner_allocator_exhaustion_keeps_mechanics_available_without_minting() {
        let mut provenance =
            DescriptorProvenance::from_owner_allocation(Err("injected owner exhaustion".into()));
        assert!(provenance.poisoned && provenance.revoked);
        let seal = provenance
            .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![1, 2])
            .unwrap();
        assert!(matches!(seal, PrivateFrameSeal::Uncovered(bytes) if bytes == [1, 2]));
        provenance
            .rearm(DescriptorOccurrenceRoute::DataRx, 2, 7)
            .unwrap();
    }

    #[test]
    fn owners_are_unique_and_outstanding_carriers_observe_durable_revocation() {
        let mut first = DescriptorProvenance::new();
        let second = DescriptorProvenance::new();
        let occurrence = occurrence(carried(
            first
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![1])
                .unwrap(),
        ));
        assert_ne!(first.owner, second.owner);
        assert!(second.validate(&occurrence).is_err());
        assert!(occurrence.is_current());
        drop(first);
        assert!(!occurrence.is_current());
    }

    #[test]
    fn b1_drop_boundary_retires_sealed_entries_without_waiting_for_teardown() {
        let mut provenance = DescriptorProvenance::new();
        for slot in 0..7 {
            let occurrence = occurrence(carried(
                provenance
                    .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, slot, vec![slot as u8])
                    .unwrap(),
            ));
            assert_eq!(provenance.sealed.len(), 1);
            provenance.retire(&occurrence);
            assert!(provenance.sealed.is_empty());
            assert!(provenance.validate(&occurrence).is_err());
        }
    }

    #[test]
    fn invalidation_counter_exhaustion_poison_is_durable() {
        for reason in [
            DescriptorInvalidation::Cancellation,
            DescriptorInvalidation::Interface,
            DescriptorInvalidation::Run,
        ] {
            let mut provenance = DescriptorProvenance::new();
            let occurrence = occurrence(carried(
                provenance
                    .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![1])
                    .unwrap(),
            ));
            match reason {
                DescriptorInvalidation::Cancellation => provenance.scan_epoch = u64::MAX,
                DescriptorInvalidation::Interface => provenance.interface_epoch = u64::MAX,
                DescriptorInvalidation::Run => provenance.run_epoch = u64::MAX,
                DescriptorInvalidation::Teardown => unreachable!(),
            }
            provenance.invalidate(reason).unwrap();
            assert!(provenance.poisoned);
            assert!(!occurrence.is_current());
        }
    }

    #[test]
    fn shared_rearm_seam_orders_mint_before_descriptor_and_index_publication() {
        let mut provenance = DescriptorProvenance::new();
        let _carrier = carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![1])
                .unwrap(),
        );
        publish_descriptor_rearm(
            &mut provenance,
            DescriptorOccurrenceRoute::DataRx,
            2,
            7,
            |provenance| {
                provenance
                    .effects
                    .push(DescriptorProvenanceEffect::DescriptorWrite);
                provenance
                    .effects
                    .push(DescriptorProvenanceEffect::ReleaseFence);
                provenance
                    .effects
                    .push(DescriptorProvenanceEffect::IndexPublish);
                Ok(())
            },
        )
        .unwrap();
        assert!(matches!(
            provenance.effects.as_slice(),
            [
                DescriptorProvenanceEffect::Mint(_),
                DescriptorProvenanceEffect::Rearm { .. },
                DescriptorProvenanceEffect::DescriptorWrite,
                DescriptorProvenanceEffect::ReleaseFence,
                DescriptorProvenanceEffect::IndexPublish,
            ]
        ));
    }

    #[test]
    fn both_rearm_seams_revoke_current_carrier_before_refill_error_drop() {
        for (route, ring) in [
            (DescriptorOccurrenceRoute::McuNormalRx, 0),
            (DescriptorOccurrenceRoute::DataRx, 2),
        ] {
            let mut provenance = DescriptorProvenance::new();
            let drops = std::rc::Rc::new(Cell::new(0));
            let current = LocalCarrierDropProbe {
                occurrence: Some(occurrence(carried(
                    provenance.seal_frame(route, ring, 0, vec![1]).unwrap(),
                ))),
                drops: std::rc::Rc::clone(&drops),
            };
            let result = publish_current_rearm_or_revoke(
                &mut provenance,
                route,
                ring,
                7,
                current,
                Err("injected refill construction failure".into()),
                |_, _| unreachable!(),
            );
            assert!(result.is_err());
            assert_eq!(drops.get(), 1);
            assert!(provenance.sealed.is_empty());
        }
    }

    #[test]
    fn production_signal_hook_revokes_before_later_delivery() {
        let mut provenance = DescriptorProvenance::new();
        let occurrence = occurrence(carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::McuNormalRx, 0, 0, vec![1])
                .unwrap(),
        ));
        observe_signal_cancellation(&mut provenance, true).unwrap();
        assert!(!occurrence.is_current());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn production_cancel_scan_hook_revokes_and_start_scan_binds_generation() {
        let mut provenance = DescriptorProvenance::new();
        let prior_scan = occurrence(carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![0])
                .unwrap(),
        ));
        observe_passive_command_provenance(
            &mut provenance,
            &PassiveMcuCommand::StartScan {
                scan_sequence: 17,
                channel: mt7921_port_spike::CandidateChannel {
                    band: mt7921_port_spike::PhysicalBand::Ghz2,
                    number: 1,
                    frequency_mhz: 2412,
                },
            },
        )
        .unwrap();
        provenance.validate(&prior_scan).unwrap();
        let occurrence = occurrence(carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 1, vec![1])
                .unwrap(),
        ));
        assert_eq!(occurrence.identity.scan_id, 17);
        assert!(occurrence.identity.scan_epoch > prior_scan.identity.scan_epoch);
        observe_passive_command_provenance(
            &mut provenance,
            &PassiveMcuCommand::CancelScan { scan_sequence: 17 },
        )
        .unwrap();
        assert!(!prior_scan.is_current());
        assert!(!occurrence.is_current());
    }

    #[test]
    fn cancellation_teardown_interface_and_run_changes_revoke_occurrences() {
        for reason in [
            DescriptorInvalidation::Cancellation,
            DescriptorInvalidation::Teardown,
            DescriptorInvalidation::Interface,
            DescriptorInvalidation::Run,
        ] {
            let mut provenance = DescriptorProvenance::new();
            let carrier = carried(
                provenance
                    .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![7])
                    .unwrap(),
            );
            let occurrence = occurrence(carrier);
            provenance.validate(&occurrence).unwrap();
            provenance.invalidate(reason).unwrap();
            assert!(!occurrence.is_current());
            assert!(provenance.validate(&occurrence).is_err());
            assert!(matches!(
                provenance.effects.last(),
                Some(DescriptorProvenanceEffect::Invalidate(actual)) if *actual == reason
            ));
        }
    }

    #[test]
    fn enclosing_teardown_revokes_before_queued_carriers_are_released() {
        struct QueueDropProbe {
            revoked: std::rc::Rc<Cell<bool>>,
            order: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
        }
        impl Drop for QueueDropProbe {
            fn drop(&mut self) {
                assert!(self.revoked.get(), "queue released before revocation");
                self.order.borrow_mut().push("queue");
            }
        }
        struct EnclosingOwner {
            provenance: DescriptorProvenance,
            _queue: QueueDropProbe,
            revoked: std::rc::Rc<Cell<bool>>,
            order: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
        }
        impl Drop for EnclosingOwner {
            fn drop(&mut self) {
                revoke_descriptor_provenance_before_release(&mut self.provenance);
                assert!(self.provenance.sealed.is_empty());
                self.revoked.set(true);
                self.order.borrow_mut().push("revoke");
            }
        }

        let revoked = std::rc::Rc::new(Cell::new(false));
        let order = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut owner = EnclosingOwner {
            provenance: DescriptorProvenance::new(),
            _queue: QueueDropProbe {
                revoked: std::rc::Rc::clone(&revoked),
                order: std::rc::Rc::clone(&order),
            },
            revoked,
            order: std::rc::Rc::clone(&order),
        };
        let occurrence = occurrence(carried(
            owner
                .provenance
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![1])
                .unwrap(),
        ));
        owner.provenance.validate(&occurrence).unwrap();
        drop(owner);
        assert!(!occurrence.is_current());
        assert_eq!(*order.borrow(), ["revoke", "queue"]);
    }

    struct LocalCarrierDropProbe {
        occurrence: Option<DescriptorOccurrence>,
        drops: std::rc::Rc<Cell<usize>>,
    }

    impl Drop for LocalCarrierDropProbe {
        fn drop(&mut self) {
            let occurrence = self.occurrence.take().unwrap();
            assert!(
                !occurrence.is_current(),
                "local carrier released before provenance revocation"
            );
            self.drops.set(self.drops.get() + 1);
        }
    }

    #[test]
    fn later_data_descriptor_error_revokes_before_earlier_local_carrier_drop() {
        let mut provenance = DescriptorProvenance::new();
        let drops = std::rc::Rc::new(Cell::new(0));
        let earlier = occurrence(carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::DataRx, 2, 0, vec![1])
                .unwrap(),
        ));
        let mut local_advertisements = vec![LocalCarrierDropProbe {
            occurrence: Some(earlier),
            drops: std::rc::Rc::clone(&drops),
        }];
        // This is the exact error-release helper used after a later descriptor
        // fails in `drain_data_rx_queue`.
        revoke_before_local_carrier_release(&mut provenance, &mut local_advertisements).unwrap();
        assert_eq!(drops.get(), 1);
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn routed_parse_error_revokes_failed_and_remaining_taken_carriers_before_drop() {
        let mut provenance = DescriptorProvenance::new();
        let lease = Arc::clone(&provenance.lease);
        let failed = carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::McuNormalRx, 0, 0, vec![1])
                .unwrap(),
        );
        let remaining = carried(
            provenance
                .seal_frame(DescriptorOccurrenceRoute::McuNormalRx, 0, 1, vec![2])
                .unwrap(),
        );
        let (failed, _error) = match failed.parse() {
            Ok(_) => panic!("invalid routed frame unexpectedly parsed"),
            Err(failure) => failure,
        };
        let mut remaining_taken = vec![remaining];
        // This is the exact parse failure and helper sequence used by
        // `VfioPassiveMechanics::next_event`.
        revoke_before_local_carrier_release(&mut provenance, &mut remaining_taken).unwrap();
        assert!(!lease.current.load(Ordering::Acquire));
        drop(failed);
        assert!(provenance.sealed.is_empty());
    }

    #[test]
    fn uncovered_descriptor_routes_preserve_bytes_but_drop_provenance() {
        let mut provenance = DescriptorProvenance::new();
        let seal = provenance
            .seal_frame(
                DescriptorOccurrenceRoute::McuNormalRx,
                9,
                0,
                vec![0xaa, 0xbb],
            )
            .unwrap();
        assert!(matches!(
            seal,
            PrivateFrameSeal::Uncovered(bytes) if bytes == [0xaa, 0xbb]
        ));
        assert!(matches!(
            provenance.effects.as_slice(),
            [DescriptorProvenanceEffect::DropUncovered]
        ));
    }

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
        assert!(active_wfdma_write_allowed(0xd4200, mask, mask));
        assert!(active_wfdma_write_allowed(0xd4204, mask, mask));
        assert!(active_wfdma_write_allowed(0xd4204, WM2_RX_IRQ_BIT, mask));
        assert!(!active_wfdma_write_allowed(0xd4200, DATA_RX_IRQ_BIT, mask));
        assert!(!active_wfdma_write_allowed(0xd4204, DATA_RX_IRQ_BIT, mask));
        let passive_mask = mask | DATA_RX_IRQ_BIT;
        assert!(active_wfdma_write_allowed(
            0xd4204,
            passive_mask,
            passive_mask
        ));
        assert!(!active_wfdma_write_allowed(0xd4200, 1 << 27, mask));
        assert!(!active_wfdma_write_allowed(0xd4204, 1 << 27, mask));
        let deadline = Instant::now() + std::time::Duration::from_millis(10);
        assert!(!response_wait_timed_out(
            deadline - std::time::Duration::from_nanos(1),
            deadline
        ));
        assert!(response_wait_timed_out(deadline, deadline));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn passive_rx_irq_and_mac_pages_are_exactly_gated() {
        let base = WM_RX_IRQ_BIT | WM2_RX_IRQ_BIT;
        let passive = base | DATA_RX_IRQ_BIT;
        assert_eq!(firmware_bootstrap_rx_irq_mask(), base);
        assert_ne!(firmware_bootstrap_rx_irq_mask(), passive);
        assert!(!active_wfdma_write_allowed(
            0xd4204,
            DATA_RX_IRQ_BIT,
            passive
        ));
        assert!(!active_wfdma_write_allowed(
            0xd4204,
            WM_RX_IRQ_BIT | DATA_RX_IRQ_BIT,
            passive
        ));
        assert!(active_wfdma_write_allowed(0xd4204, passive, passive));
        assert!(active_wfdma_write_allowed(0xd4600, 0x0140_0004, passive));
        assert!(!active_wfdma_write_allowed(0xd4600, 0, passive));
        assert!(Operation::RunOneShotPassiveChannel1.wfdma_writable());
        assert!(Operation::RunOneShotPassiveChannel1.conn_writable());
        assert!(Operation::RunOneShotPassiveChannel1.loads_firmware());
        assert!(Operation::RunOneShotPassiveChannels1And6.wfdma_writable());
        assert!(Operation::RunOneShotPassiveChannels1And6.conn_writable());
        assert!(Operation::RunOneShotPassiveChannels1And6.loads_firmware());
        assert!(Operation::RunOneShotPassive2Ghz.wfdma_writable());
        assert!(Operation::RunOneShotPassive2Ghz.conn_writable());
        assert!(Operation::RunOneShotPassive2Ghz.loads_firmware());
        assert!(Operation::RunOneShotPassive5GhzNonDfs.wfdma_writable());
        assert!(Operation::RunOneShotPassive5GhzNonDfs.conn_writable());
        assert!(Operation::RunOneShotPassive5GhzNonDfs.loads_firmware());
        assert!(Operation::RunOneShotPassive5GhzDfsLow.wfdma_writable());
        assert!(Operation::RunOneShotPassive5GhzDfsLow.conn_writable());
        assert!(Operation::RunOneShotPassive5GhzDfsLow.loads_firmware());
        assert!(Operation::RunOneShotPassive5GhzDfsHigh.wfdma_writable());
        assert!(Operation::RunOneShotPassive5GhzDfsHigh.conn_writable());
        assert!(Operation::RunOneShotPassive5GhzDfsHigh.loads_firmware());
        assert!(Operation::RunOneShotPassiveSmeFull.wfdma_writable());
        assert!(Operation::RunOneShotPassiveSmeFull.conn_writable());
        assert!(Operation::RunOneShotPassiveSmeFull.loads_firmware());
        assert!(Operation::RunOneShotPassivePrepare.wfdma_writable());
        assert!(Operation::RunOneShotPassivePrepare.conn_writable());
        assert!(Operation::RunOneShotPassivePrepare.loads_firmware());
        assert!(Operation::RunOneShotPowerSetup.wfdma_writable());
        assert!(Operation::RunOneShotPowerSetup.conn_writable());
        assert!(Operation::RunOneShotPowerSetup.loads_firmware());
        let mut required = passive_mac_mmio_plan()
            .into_iter()
            .map(|operation| match operation {
                PassiveMacMmioOperation::Rmw { address, .. }
                | PassiveMacMmioOperation::WtblClear { address, .. } => {
                    passive_mac_bar_offset(address).unwrap() & !(PAGE - 1)
                }
            })
            .collect::<Vec<_>>();
        required.sort_unstable();
        required.dedup();
        assert_eq!(required, PASSIVE_MAC_BAR_PAGES);
        assert!(!passive_mac_address_allowed(0x820e_4000));
        assert!(!passive_mac_address_allowed(0x820e_40f8));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn power_setup_scan_retries_stop_early_and_fail_closed_after_five() {
        assert_eq!(
            Operation::RunOneShotPowerSetup.passive_scan_attempt_limit(),
            5
        );
        assert_eq!(
            Operation::RunOneShotPassiveChannel1.passive_scan_attempt_limit(),
            1
        );
        assert!(!Operation::RunOneShotPowerSetup.should_continue_passive_scans(1, true));
        for attempt in 1..5 {
            assert!(Operation::RunOneShotPowerSetup.should_continue_passive_scans(attempt, false));
        }
        assert!(!Operation::RunOneShotPowerSetup.should_continue_passive_scans(5, false));
        assert!(!Operation::RunOneShotPassiveChannel1.should_continue_passive_scans(1, false));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn passive_prepare_order_stops_at_every_injected_failure() {
        let expected = [
            PassivePrepareStep::MacMmio,
            PassivePrepareStep::ProgramDataRing,
            PassivePrepareStep::VerifyDataRing,
            PassivePrepareStep::AuthorizeDataIrq,
            PassivePrepareStep::EnableDataIrq,
            PassivePrepareStep::VerifyDataIrq,
        ];
        let mut success = Vec::new();
        run_passive_prepare_steps::<()>(|step| {
            success.push(step);
            Ok(())
        })
        .unwrap();
        assert_eq!(success, expected);

        for fail_at in 0..expected.len() {
            let mut attempted = Vec::new();
            let result = run_passive_prepare_steps(|step| {
                attempted.push(step);
                if attempted.len() - 1 == fail_at {
                    Err(step)
                } else {
                    Ok(())
                }
            });
            assert_eq!(result, Err(expected[fail_at]));
            assert_eq!(attempted, expected[..=fail_at]);
        }
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

    #[test]
    fn watchdog_must_be_armed_active_waiting_and_unexpired() {
        assert!(
            verify_watchdog_status(
                "armed deadline=200\nActiveState=active\nSubState=waiting\n",
                100,
            )
            .is_ok()
        );
        for status in [
            "disarmed\n",
            "armed deadline=200\nActiveState=inactive\nSubState=dead\n",
            "armed deadline=invalid\nActiveState=active\nSubState=waiting\n",
        ] {
            assert!(verify_watchdog_status(status, 100).is_err());
        }
        assert!(
            verify_watchdog_status(
                "armed deadline=100\nActiveState=active\nSubState=waiting\n",
                100,
            )
            .is_err()
        );
    }

    #[test]
    fn watchdog_unavailable_refuses_acquisition_before_activation() {
        assert_eq!(
            ContainmentLedger::acquire(None),
            Err("external reboot watchdog was not verified before acquisition".into())
        );
    }

    #[test]
    fn ledger_resource_mismatch_always_selects_conservative_disable() {
        let mut ledger = ContainmentLedger::acquire(Some(ArmedWatchdog { deadline: 200 })).unwrap();
        assert!(!ledger.must_disable(Hazard::BusMaster, false));
        assert!(ledger.must_disable(Hazard::BusMaster, true));
        ledger.mark_possibly_active(Hazard::BusMaster);
        assert!(ledger.must_disable(Hazard::BusMaster, false));
        ledger.confirm_inactive(Hazard::BusMaster);
        assert!(!ledger.must_disable(Hazard::BusMaster, false));
    }

    #[test]
    fn ambiguous_scan_exit_never_claims_passive_ready() {
        let mut ledger = ContainmentLedger::acquire(Some(ArmedWatchdog { deadline: 200 })).unwrap();
        ledger.phase = RunPhase::Scanning;
        let mut trace = vec![ledger.phase];
        ledger.phase = RunPhase::Faulted;
        trace.push(ledger.phase);
        ledger.phase = RunPhase::Containing;
        trace.push(ledger.phase);
        assert_eq!(
            trace,
            [RunPhase::Scanning, RunPhase::Faulted, RunPhase::Containing]
        );
        assert!(!trace.contains(&RunPhase::PassiveReady));
    }

    #[test]
    fn every_normal_phase_requires_its_exact_predecessor() {
        let mut ledger = ContainmentLedger::acquire(Some(ArmedWatchdog { deadline: 200 })).unwrap();
        for (from, to) in [
            (RunPhase::Acquiring, RunPhase::MappedDmaDisabled),
            (RunPhase::MappedDmaDisabled, RunPhase::AcquiringHostControl),
            (RunPhase::AcquiringHostControl, RunPhase::HostDriverOwned),
            (
                RunPhase::HostDriverOwned,
                RunPhase::WfsysResetAndSelectorRestored,
            ),
            (
                RunPhase::WfsysResetAndSelectorRestored,
                RunPhase::RingsPreparedIrqSourceDisabled,
            ),
            (
                RunPhase::RingsPreparedIrqSourceDisabled,
                RunPhase::DmaAndResponseIrqEnabled,
            ),
            (RunPhase::DmaAndResponseIrqEnabled, RunPhase::FirmwareReady),
            (RunPhase::FirmwareReady, RunPhase::PassivePreparing),
            (RunPhase::PassivePreparing, RunPhase::PassiveReady),
            (RunPhase::PassiveReady, RunPhase::Scanning),
            (RunPhase::Scanning, RunPhase::PassiveReady),
            (RunPhase::PassiveReady, RunPhase::BeaconAuthorized),
            (RunPhase::BeaconAuthorized, RunPhase::PowerConfiguredNoFrame),
        ] {
            assert_eq!(ledger.phase, from);
            ledger.transition(from, to).unwrap();
        }
        assert!(
            ledger
                .transition(RunPhase::PassiveReady, RunPhase::Scanning)
                .is_err()
        );
    }

    #[test]
    fn parked_capsule_helper() {
        let Ok(path) = std::env::var("DRV_TEST_PARK_DROP_PATH") else {
            return;
        };
        let device = Arc::new(File::open("/dev/null").unwrap());
        let iommu = Arc::new(File::open("/dev/null").unwrap());
        let mut ledger = ContainmentLedger::acquire(Some(ArmedWatchdog { deadline: 200 })).unwrap();
        ledger.mark_possibly_active(Hazard::HostControl);
        let mut capsule = ActiveVfioCapsule::new(device, iommu, Some(ledger));
        capsule.drop_probe = Some(CapsuleDropProbe(path.into()));
        if std::env::var("DRV_TEST_PARK_MODE").as_deref() == Ok("retain") {
            capsule.retain_forever();
        }
        drop(capsule);
    }

    #[test]
    fn parked_capsule_never_runs_its_resource_destructor() {
        for mode in ["drop", "retain"] {
            let marker = std::env::temp_dir()
                .join(format!("mt7921-retention-{mode}-{}", std::process::id()));
            let _ = std::fs::remove_file(&marker);
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["parked_capsule_helper", "--nocapture"])
                .env("DRV_TEST_PARK_DROP_PATH", &marker)
                .env("DRV_TEST_PARK_MODE", mode)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert!(child.try_wait().unwrap().is_none(), "park helper returned");
            assert!(!marker.exists(), "parked resource destructor ran");
            child.kill().unwrap();
            child.wait().unwrap();
            assert!(!marker.exists(), "killed process ran Rust destructors");
        }
    }

    #[test]
    fn proven_safe_capsule_uses_ordered_raii_without_parking() {
        let marker = std::env::temp_dir().join(format!("mt7921-safe-drop-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let device = Arc::new(File::open("/dev/null").unwrap());
        let iommu = Arc::new(File::open("/dev/null").unwrap());
        let mut ledger = ContainmentLedger::acquire(Some(ArmedWatchdog { deadline: 200 })).unwrap();
        ledger.mark_possibly_active(Hazard::HostControl);
        ledger.confirm_inactive(Hazard::HostControl);
        let mut capsule = ActiveVfioCapsule::new(device, iommu, Some(ledger));
        capsule.drop_probe = Some(CapsuleDropProbe(marker.clone()));
        drop(capsule);
        assert_eq!(std::fs::read(&marker).unwrap(), b"dropped");
        std::fs::remove_file(marker).unwrap();
    }

    #[test]
    fn containment_preserves_primary_and_cleanup_errors() {
        let outcome = ContainmentOutcome::classify(
            false,
            Some("primary".into()),
            vec!["mask".into(), "reset".into()],
            Vec::new(),
        );
        assert_eq!(
            outcome,
            ContainmentOutcome::RetainUnsafe {
                primary: Some("primary".into()),
                cleanup_errors: vec!["mask".into(), "reset".into()],
            }
        );
        assert!(outcome.must_park());
    }

    #[test]
    fn safe_release_error_never_selects_parking_or_close_retry() {
        let outcome = ContainmentOutcome::classify(
            true,
            Some("primary".into()),
            vec!["quiesce".into()],
            vec![ReleaseFailure {
                action: ObservableRelease::IoasDestroy,
                error: "busy".into(),
            }],
        );
        assert_eq!(
            outcome,
            ContainmentOutcome::SafeReleaseError {
                primary: Some("primary".into()),
                cleanup_errors: vec!["quiesce".into()],
                release_errors: vec![ReleaseFailure {
                    action: ObservableRelease::IoasDestroy,
                    error: "busy".into(),
                }],
            }
        );
        assert!(!outcome.must_park());
        let observable_actions = [
            ObservableRelease::DmaUnmap,
            ObservableRelease::BarMunmap,
            ObservableRelease::IoasDetach,
            ObservableRelease::IoasDestroy,
        ];
        assert_eq!(observable_actions.len(), 4);
    }

    #[test]
    fn acquisition_ledger_records_intent_before_resource_ownership() {
        let mut ledger = AcquisitionLedger::default();
        ledger.record(AcquisitionIntent::BindIommu).unwrap();
        ledger.record(AcquisitionIntent::AllocateIoas).unwrap();
        ledger.record(AcquisitionIntent::AttachIoas).unwrap();
        ledger.record(AcquisitionIntent::MapBar(0xd4000)).unwrap();
        ledger
            .record(AcquisitionIntent::MapDma {
                iova: 0x0100_0000,
                len: PAGE,
            })
            .unwrap();
        ledger.record(AcquisitionIntent::InstallIrq).unwrap();
        assert_eq!(
            ledger.recorded().collect::<Vec<_>>(),
            [
                AcquisitionIntent::BindIommu,
                AcquisitionIntent::AllocateIoas,
                AcquisitionIntent::AttachIoas,
                AcquisitionIntent::MapBar(0xd4000),
                AcquisitionIntent::MapDma {
                    iova: 0x0100_0000,
                    len: PAGE,
                },
                AcquisitionIntent::InstallIrq,
            ]
        );
    }

    #[test]
    fn post_acquisition_failure_funnels_primary_and_observable_release_error() {
        let device = Arc::new(File::open("/dev/null").unwrap());
        let iommu = Arc::new(File::open("/dev/null").unwrap());
        let ledger = ContainmentLedger::acquire(Some(ArmedWatchdog { deadline: 200 })).unwrap();
        let mut capsule = ActiveVfioCapsule::new(device, Arc::clone(&iommu), Some(ledger));
        capsule.ioas = Some(Ioas {
            fd: iommu,
            id: 1,
            destroyed: false,
        });
        let error = (|| -> Result<(), String> {
            finish_owned_acquisition!(
                &mut capsule,
                Err::<(), _>("post-active-acquisition primary".into())
            );
            Ok(())
        })()
        .unwrap_err();
        assert!(error.contains("primary"));
        assert!(error.contains("SAFE acquisition release errors"));
        assert_eq!(
            capsule.containment.as_ref().unwrap().phase,
            RunPhase::SafeReleaseError
        );
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn one_management_tx_requires_correlated_free_and_status_in_either_order() {
        for completions in [
            [
                MgmtTxCompletion::Free(Mt7921TxFree {
                    wcid: None,
                    token: 0,
                    dropped: false,
                    attempts: 1,
                }),
                MgmtTxCompletion::Status(Mt7921TxStatus {
                    wcid: 19,
                    pid: 3,
                    acked: true,
                }),
            ],
            [
                MgmtTxCompletion::Status(Mt7921TxStatus {
                    wcid: 19,
                    pid: 3,
                    acked: true,
                }),
                MgmtTxCompletion::Free(Mt7921TxFree {
                    wcid: None,
                    token: 0,
                    dropped: false,
                    attempts: 1,
                }),
            ],
        ] {
            let mut state = MgmtTxCompletionState::default();
            assert!(state.finished().is_none());
            state.observe(completions[0]).unwrap();
            assert!(state.finished().is_none());
            state.observe(completions[1]).unwrap();
            assert_eq!(state.finished().unwrap(), Ok(()));
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn one_management_tx_rejects_wrong_identity_duplicate_and_failed_ack() {
        let mut state = MgmtTxCompletionState::default();
        assert!(
            state
                .observe(MgmtTxCompletion::Free(Mt7921TxFree {
                    wcid: None,
                    token: 1,
                    dropped: false,
                    attempts: 1
                }))
                .is_err()
        );
        state
            .observe(MgmtTxCompletion::Free(Mt7921TxFree {
                wcid: None,
                token: 0,
                dropped: true,
                attempts: 1,
            }))
            .unwrap();
        assert!(
            state
                .observe(MgmtTxCompletion::Free(Mt7921TxFree {
                    wcid: None,
                    token: 0,
                    dropped: false,
                    attempts: 1
                }))
                .is_err()
        );
        state
            .observe(MgmtTxCompletion::Status(Mt7921TxStatus {
                wcid: 19,
                pid: 3,
                acked: true,
            }))
            .unwrap();
        assert!(state.finished().unwrap().is_err());
    }
}
