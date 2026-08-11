//! Strictly read-only no-plastic MT7921 VFIO inventory.
#![cfg(target_os = "linux")]
#![allow(unexpected_cfgs)]

#[cfg(feature = "fuchsia-passive")]
use driver_runtime::{PublicationState, TranscriptEvent};
#[cfg(feature = "fuchsia-passive")]
use fidl_fuchsia_wlan_common as fidl_common;
#[cfg(feature = "fuchsia-passive")]
use fidl_fuchsia_wlan_driver as fidl_driver;
#[cfg(feature = "fuchsia-passive")]
use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
#[cfg(feature = "fuchsia-passive")]
use fidl_fuchsia_wlan_internal as fidl_internal;
#[cfg(feature = "fuchsia-passive")]
use fidl_fuchsia_wlan_sme as fidl_sme;
#[cfg(feature = "fuchsia-passive")]
use fidl_fuchsia_wlan_softmac as fidl_softmac;
#[cfg(feature = "fuchsia-passive")]
use fuchsia_softmac_port::{
    BeaconHintAuthorizer, ChannelBandwidth, ChannelNumber, ConservativeRegulatoryPolicy,
    HardwareScanEvent, MlmeScanEvent, PassiveScanner, ScanRequest, ScanResultCode, ScanTypes,
    SoftmacHardware, WlanBand, WlanSoftmacBaseStartPassiveScanRequest, allowed_passive_channels,
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
    FirmwareImagePart, FirmwareLoaderState, FirmwareLoaderTransport, FirmwareOwnershipEvent,
    GlobalTxRingError, GlobalTxRingEvent, GlobalTxRingTransport, IrqLifecycle, IrqResetCleanupStep,
    IrqResetEvent, IrqResetTransport, MT_HIF_REMAP_L1_BAR_OFFSET, MT_HIF_REMAP_WINDOW_BAR_OFFSET,
    MT_TOP_LPCR_HOST_DRV_OWN, MT7921_FWDL_CHUNK_BYTES, MT7921_FWDL_RING_BYTES, McuRxRegisters,
    Mt7921TxFree, Mt7921TxStatus, OwnershipError, OwnershipEvent, OwnershipRoundTripEvent,
    OwnershipRoundTripTransport, OwnershipTransport, PCIE_LPCR_HOST_CLR_OWN,
    PCIE_LPCR_HOST_SET_OWN, Patch, PciIrqCapability, PciIrqKind, ReadOnlyStatus, ReadRegister,
    TopOwnershipError, TopOwnershipEvent, TopOwnershipTransport, TxRingState, WfsysResetEvent,
    WfsysResetTransport, acquire_driver_ownership, acquire_top_driver_ownership,
    encode_download_command, encode_mt7921_5ghz_auth_tx, exercise_irq_reset_boundary,
    load_mt7921_firmware, load_mt7921_firmware_bootstrap,
    load_mt7921_firmware_through_channel_domain, mask_ack_disabled_fwdl_interrupt,
    mt76_pci_aspm_supported, mt7921_dma_rx, mt7921_dma_tx, mt7921_packet_type,
    parse_clc_set_response, parse_download_response, parse_eeprom_block, parse_mt7921_tx_free,
    parse_mt7921_tx_status, parse_nic_capability, prepare_global_rx_rings, prepare_global_tx_rings,
    prepare_mcu_rx_ring, program_disabled_fwdl_ring, read_dynamic_identity_status, reset_wfsys,
    round_trip_driver_ownership, select_vfio_irq, stage_disabled_firmware_chunk,
};
#[cfg(feature = "fuchsia-passive")]
use mt7921_port_spike::{
    ClientChannelContext, ClientDataGeneration, ClientFirmwareEffectsState, ClientPhysicalChannel,
    ClientPhysicalChannelEnsure, ClientRxCandidate, ClientScanEvidence, ClientTargetBssLease,
    ClientEdcaAc, ClientEdcaParameters,
    ConservativePowerLimits, LegacyWmeAssociation, PassiveMacMmioOperation, PassiveMcuCommand,
    PassiveRxError, RateTxPowerAuthorizer, RateTxPowerTransport, candidate_channels,
    classify_preassociation_sae_auth, connac2_group1_pn, encode_client_bss_command,
    encode_client_data_txwi, encode_client_edca_command, encode_client_interface_commands, encode_client_management_tx,
    encode_disable_keys_command, encode_gtk_command, encode_igtk_command, encode_key_v2_command,
    encode_legacy_wme_add_wcid_command, encode_pse_reg_read_command, encode_ptk_command,
    encode_remove_wcid_command, load_mt7921_firmware_with_passive_boundary, parse_connac2_rx_frame,
    parse_passive_advertisement, parse_passive_scan_done, parse_pse_reg_read_response,
    passive_mac_bar_offset, passive_mac_mmio_plan, passive_mac_source_rmw_value,
    validate_passive_mac_bar_read,
};
#[cfg(feature = "fuchsia-passive")]
use mt7921_softmac_adapter::client_device::{
    ClientChannelEnsure, ClientRxFrame, ClientRxSecurity, ClientSupport, Mt7921ClientDevice,
    Mt7921ClientEffects, PinnedClientRuntime,
};
#[cfg(feature = "fuchsia-passive")]
use mt7921_softmac_adapter::ethernet::{BoundedNetstackProof, NetstackProofConfig};
#[cfg(feature = "fuchsia-passive")]
use mt7921_softmac_adapter::{
    LinuxChannelShape, Mt7921SoftmacAdapter, PassiveMechanicsEvent, PassivePrerequisites,
    SourceExactPassiveMechanics, SourceExactPassiveTransport, query_from_capabilities,
    set_channel_request,
};
#[cfg(feature = "fuchsia-passive")]
use num_bigint::BigUint;
#[cfg(feature = "fuchsia-passive")]
use std::collections::VecDeque;
use std::{
    cell::Cell,
    env,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    num::{NonZeroU16, NonZeroU64},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    process::{Command, Stdio},
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
use userspace_vfio::{Ioas, RegionInfo, VfioIrq};
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
#[cfg(feature = "fuchsia-passive")]
fn emit_sae_stage_best_effort(event: &str, emit: impl FnOnce(&str) -> std::io::Result<()>) {
    // Diagnostic output must never become an ownership or cleanup gate. In
    // production the supervisor already captures stderr into the run report.
    let _ = emit(event);
}

#[cfg(feature = "fuchsia-passive")]
fn record_sae_stage(event: &str) {
    emit_sae_stage_best_effort(event, |event| {
        eprintln!(
            "{}",
            TranscriptEvent::public("sae_auth_event", event).json()
        );
        Ok(())
    });
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

#[cfg(feature = "fuchsia-passive")]
fn acquire_sae_tx_resources(
    iommu: &Arc<File>,
    ioas: u32,
    txwi: &mut Option<DmaArena>,
    frame: &mut Option<DmaArena>,
    ring: &mut Option<DmaArena>,
    acquisition: &mut AcquisitionLedger,
    containment: &mut ContainmentLedger,
) -> Result<(), String> {
    if txwi.is_some() || frame.is_some() || ring.is_some() {
        return Err("SAE TX resources were already acquired".into());
    }
    let mut map = |slot: &mut Option<DmaArena>, iova| -> Result<(), String> {
        containment.mark_possibly_active(Hazard::DmaMapping);
        acquisition.record(AcquisitionIntent::MapDma { iova, len: PAGE })?;
        *slot = Some(DmaArena::map_len(iommu, ioas, iova, PAGE)?);
        Ok(())
    };
    map(txwi, 0x0103_0000)?;
    map(frame, 0x0103_1000)?;
    map(ring, 0x0103_2000)?;
    txwi.as_mut().expect("mapped").zero_bytes(PAGE)?;
    frame.as_mut().expect("mapped").zero_bytes(PAGE)?;
    ring.as_mut()
        .expect("mapped")
        .initialize_descriptor_page()?;
    std::sync::atomic::fence(Ordering::Release);
    Ok(())
}

#[cfg(feature = "fuchsia-passive")]
fn run_contained_dma_resource_round_trip(
    capsule: &mut ActiveVfioCapsule,
    info: &RegionInfo,
    bdf: &str,
    operation: Operation,
    wfdma: &ReadPage,
    pcie_mac: &ReadPage,
    selected_irq: PciIrqCapability,
    firmware_images: Option<(&[u8], &[u8])>,
    passive_channel: Option<ChannelNumber>,
) -> Result<(), String> {
    record_sae_stage("vfio_dma_resource_round_trip_begin");
    capsule.active = Some(ActiveVfioResources::default());
    let primary = (|| -> Result<(), String> {
        acquire_active_vfio_resources(
            capsule.active.as_mut().expect("active owner installed"),
            &capsule.device,
            &capsule.iommu,
            capsule.ioas.as_ref().expect("IOAS acquired").id(),
            info,
            operation,
            &mut capsule.acquisition,
            capsule
                .containment
                .as_mut()
                .expect("guarded gate has containment ledger"),
        )?;
        capsule
            .containment
            .as_mut()
            .expect("guarded gate has containment ledger")
            .transition(RunPhase::Contained, RunPhase::MappedDmaDisabled)?;
        record_sae_stage("vfio_dma_resources_mapped core_arenas=10 dma_bytes=126976 bar_pages=4");
        verify_pci_dma_disabled(bdf)?;
        let global = wfdma.read(0xd4208)?;
        let host_irq = wfdma.read(0xd4204)?;
        let mac_irq = pcie_mac.read(0x10188)?;
        if global & 0xf != 0 || host_irq != 0 || mac_irq != 0 {
            return Err(format!(
                "pre-BME state unsafe global={global:#010x} host_irq={host_irq:#010x} mac_irq={mac_irq:#010x}"
            ));
        }
        record_sae_stage(&format!(
            "vfio_dma_pre_bme_verified global={global:#010x} host_irq={host_irq:#010x} mac_irq={mac_irq:#010x} bme=false"
        ));
        let active = capsule.active.as_mut().expect("active owner installed");
        record_sae_stage("vfio_wfdma_prep_begin");
        capsule
            .containment
            .as_mut()
            .expect("guarded gate has containment ledger")
            .mark_possibly_active(Hazard::Wfdma);
        let disabled =
            global & !((1 << 0) | (1 << 2) | (1 << 15) | (1 << 21) | (1 << 27) | (1 << 28));
        wfdma.write_active_wfdma(0xd4208, disabled)?;
        let disable_deadline = Instant::now() + std::time::Duration::from_millis(100);
        while wfdma.read(0xd4208)? & ((1 << 1) | (1 << 3)) != 0 {
            if Instant::now() >= disable_deadline {
                return Err("WFDMA did not quiesce during contained preparation".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let global_ext = wfdma.read(0xd42b0)?;
        if global_ext == u32::MAX {
            return Err("WFDMA extended configuration returned all ones".into());
        }
        wfdma.write_active_wfdma(0xd42b0, global_ext & !(1 << 6))?;
        active
            .dmashdl
            .as_ref()
            .expect("mapped")
            .enable_dmashdl_bypass()?;
        let reset = wfdma.read(0xd4100)?;
        if reset == u32::MAX {
            return Err("WFDMA reset control returned all ones".into());
        }
        wfdma.write_active_wfdma(0xd4100, reset & !0x30)?;
        wfdma.write_active_wfdma(0xd4100, reset | 0x30)?;
        {
            let mut transport = VfioGlobalTxRings { page: wfdma };
            prepare_global_tx_rings(
                &mut transport,
                active.tx_guard.as_ref().expect("mapped").iova,
                active.fwdl_ring.as_ref().expect("mapped").iova,
                active.mcu_tx_ring.as_ref().expect("mapped").iova,
                |_| {},
            )
            .map_err(|error| format!("prepare contained TX rings: {error:?}"))?;
        }
        {
            let mut transport = VfioGlobalRxRings { page: wfdma };
            prepare_global_rx_rings(
                &mut transport,
                active.rx_guard.as_ref().expect("mapped").iova,
                active.mcu_rx_ring.as_ref().expect("mapped").iova,
                |_| {},
            )
            .map_err(|error| format!("prepare contained RX rings: {error:?}"))?;
            wfdma.write_rx_ring_slot(
                2,
                active.data_rx_ring.as_ref().expect("mapped").iova as u32,
                8,
                7,
                0,
            )?;
            wfdma.write_rx_ring_slot(
                4,
                active.mcu_wa_rx_ring.as_ref().expect("mapped").iova as u32,
                8,
                7,
                0,
            )?;
        }
        record_sae_stage("vfio_dma_ring_mmio_prepared tx=18 rx=8 wa_rx=1 dma_enabled=false");
        capsule
            .containment
            .as_mut()
            .expect("guarded gate has containment ledger")
            .mark_possibly_active(Hazard::DeviceIrq);
        active.irq = Some(install_vfio_irq(&capsule.device, selected_irq)?);
        if active
            .irq
            .as_ref()
            .expect("IRQ installed")
            .try_read()?
            .is_some()
        {
            return Err("unexpected IRQ before contained source enable".into());
        }
        if wfdma.read(0xd4200)? != 0 {
            return Err("nonzero host interrupt status during contained preparation".into());
        }
        wfdma.write_active_wfdma(0xd42f0, 0)?;
        wfdma.write_active_wfdma(0xd4680, 4)?;
        wfdma.write_active_wfdma(0xd4688, 0x0040_0004)?;
        wfdma.write_active_wfdma(0xd4690, 0x00c0_0004)?;
        wfdma.write_active_wfdma(0xd4640, 0x0340_0004)?;
        wfdma.write_active_wfdma(0xd4644, 0x0380_0004)?;
        capsule
            .containment
            .as_mut()
            .expect("guarded gate has containment ledger")
            .mark_possibly_active(Hazard::BusMaster);
        record_sae_stage("vfio_dma_bme_enable_before");
        set_pci_bus_master(bdf, true)?;
        record_sae_stage("vfio_dma_bme_enable_after bme=true wfdma_enabled=false");
        let prepared_global = wfdma.read(0xd4208)?;
        let prepared_host_irq = wfdma.read(0xd4204)?;
        let prepared_mac_irq = pcie_mac.read(0x10188)?;
        if prepared_global & 0xf != 0 || prepared_host_irq != 0 || prepared_mac_irq != 0 {
            return Err(format!(
                "prepared transport escaped disabled state global={prepared_global:#010x} host_irq={prepared_host_irq:#010x} mac_irq={prepared_mac_irq:#010x}"
            ));
        }
        record_sae_stage(
            "vfio_wfdma_prep_complete engines=false host_irq=false mac_irq=false bme=true msi_owned=true",
        );
        record_sae_stage("vfio_wfdma_activation_begin");
        let enabled = prepared_global
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
        wfdma.write_active_wfdma(0xd4208, enabled)?;
        wfdma.write_active_wfdma(0xd4204, firmware_bootstrap_rx_irq_mask())?;
        let mut top = VfioTopOwnership {
            selector: active.selector_page.as_ref().expect("mapped"),
            window: active.dynamic_window.as_ref().expect("mapped"),
            start: Instant::now(),
            saved: Cell::new(None),
        };
        acquire_top_driver_ownership(&mut top, |_| {})
            .map_err(|error| format!("contained MT_TOP ownership: {error:?}"))?;
        pcie_mac.disable_pcie_l0s()?;
        active
            .swdef
            .as_ref()
            .expect("mapped")
            .write_swdef_normal()?;
        let active_global = wfdma.read(0xd4208)?;
        let active_host_irq = wfdma.read(0xd4204)?;
        let active_mac_irq = pcie_mac.read(0x10188)?;
        if active_global & 0x5 != 0x5
            || active_host_irq != firmware_bootstrap_rx_irq_mask()
            || active_mac_irq != 0xff
        {
            return Err(format!(
                "transport activation mismatch global={active_global:#010x} host_irq={active_host_irq:#010x} mac_irq={active_mac_irq:#010x}"
            ));
        }
        record_sae_stage(
            "vfio_wfdma_activation_complete engines=true host_irq=wm_wm2 mac_irq=true top_owned=true l0s_disabled=true swdef_normal=true firmware_published=false",
        );
        capsule
            .containment
            .as_mut()
            .expect("guarded gate has containment ledger")
            .transition(
                RunPhase::MappedDmaDisabled,
                RunPhase::DmaAndResponseIrqEnabled,
            )?;
        if let Some((patch_bytes, ram_bytes)) = firmware_images {
            record_sae_stage("vfio_firmware_transport_ready");
            let signal = ActiveSignalGuard::install()?;
            let conn = ReadPage::map(&capsule.device, info, 0xe0000, true)?;
            let mcu = ActiveMcuIo {
                wfdma,
                irq: active.irq.as_mut().expect("IRQ installed"),
                signal: &signal,
                tx_ring: active.mcu_tx_ring.as_mut().expect("mapped"),
                payload: active.command_payload.as_mut().expect("mapped"),
                wm: ActiveMcuRx {
                    rx_ring: active.mcu_rx_ring.as_mut().expect("mapped"),
                    rx_buffers: active.mcu_rx_buffers.as_ref().expect("mapped"),
                    rx_tail: 0,
                    rx_head: 7,
                    rx_ring_index: 0,
                    rx_count: 8,
                    irq_bit: WM_RX_IRQ_BIT,
                },
                wm2: Some(ActiveMcuRx {
                    rx_ring: active.mcu_wa_rx_ring.as_mut().expect("mapped"),
                    rx_buffers: active.mcu_wa_rx_buffers.as_ref().expect("mapped"),
                    rx_tail: 0,
                    rx_head: 7,
                    rx_ring_index: 4,
                    rx_count: 8,
                    irq_bit: WM2_RX_IRQ_BIT,
                }),
                extra_irq_mask: 0,
                unsolicited: Vec::new(),
                normal_rx_frames: VecDeque::new(),
                tx_completions: Vec::new(),
                descriptor_provenance: DescriptorProvenance::new(),
            };
            let mut loader = VfioFirmwareLoader {
                mcu,
                conn: &conn,
                pcie_mac,
                bdf,
                fwdl_ring: active.fwdl_ring.as_mut().expect("mapped"),
                fwdl_payload: active.fwdl_payload.as_mut().expect("mapped"),
                sequence: 0,
                command_index: 0,
                uni_terminal_poisoned: false,
                #[cfg(feature = "fuchsia-passive")]
                client_interface: None,
                fwdl_index: 0,
                pending_scatter: None,
                start: Instant::now(),
            };
            let patch = Patch::parse(patch_bytes)
                .map_err(|error| format!("parse patch for contained loader: {error:?}"))?;
            let firmware = Firmware::parse(ram_bytes)
                .map_err(|error| format!("parse RAM for contained loader: {error:?}"))?;
            #[cfg(feature = "fuchsia-passive")]
            let report = if operation == Operation::RunOneShotPassiveChannel1 {
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
                                rx_ring: active.data_rx_ring.as_mut().expect("mapped"),
                                rx_buffers: active.data_rx_buffers.as_ref().expect("mapped"),
                                rx_tail: 0,
                                rx_head: 7,
                                rx_ring_index: 2,
                                rx_count: 8,
                                irq_bit: DATA_RX_IRQ_BIT,
                            },
                            mac_pages: &active.passive_window_pages,
                            scan_started: None,
                            pending_scan_done: None,
                            advertisements: Vec::new(),
                            tx_completions: Vec::new(),
                            mgmt_tx_outstanding: MgmtTxOutstanding::default(),
                            mgmt_txwi: &mut active.mgmt_txwi,
                            mgmt_frame: &mut active.mgmt_frame,
                            mgmt_tx_ring: &mut active.mgmt_tx_ring,
                        };
                        let transport =
                            SourceExactPassiveTransport::new(mechanics, report.nic_capability)
                                .map_err(|error| error.to_string())?;
                        let channel = passive_channel
                            .ok_or("contained passive scan omitted its selected channel")?;
                        let mut adapter = Mt7921SoftmacAdapter::new(
                            transport,
                            report.nic_capability,
                            candidate_channels(report.nic_capability),
                            vec![channel],
                        )
                        .map_err(|error| error.to_string())?;
                        adapter
                            .set_channel(set_channel_request(
                                channel,
                                ChannelBandwidth::Cbw20,
                                None,
                            ))
                            .map_err(|error| error.to_string())?;
                        record_sae_stage(
                            &format!(
                                "vfio_passive_receive_setup_ready channel={} frequency_mhz={} dwell_min_ms=150 dwell_max_ms=250 intentional_tx=false",
                                channel.number,
                                if channel.band == WlanBand::TwoGhz {
                                    if channel.number == 14 {
                                        2484
                                    } else {
                                        2407 + u16::from(channel.number) * 5
                                    }
                                } else {
                                    5000 + u16::from(channel.number) * 5
                                },
                            ),
                        );
                        let response = adapter
                            .start_passive_scan(WlanSoftmacBaseStartPassiveScanRequest {
                                channels: Some(vec![channel]),
                                min_channel_time: Some(150_000_000),
                                max_channel_time: Some(250_000_000),
                                min_home_time: Some(0),
                            })
                            .map_err(|error| error.to_string())?;
                        let scan_id = response.scan_id.ok_or("passive scan omitted id")?;
                        let mut observations = 0usize;
                        let success = loop {
                            match adapter.next_scan_event().map_err(|error| error.to_string())? {
                                Some(HardwareScanEvent::Observation(observation)) => {
                                    observations += 1;
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
                                None => {
                                    std::thread::sleep(std::time::Duration::from_millis(1));
                                }
                            }
                        };
                        if !success || observations == 0 {
                            return Err(format!(
                                "passive channel {} result success={success} observations={observations}",
                                channel.number,
                            ));
                        }
                        record_sae_stage(&format!(
                            "vfio_passive_observation_ready channel={} scan_id={scan_id} observations={observations}",
                            channel.number,
                        ));
                        Ok(())
                    },
                )
            } else {
                load_mt7921_firmware(&mut loader, patch, firmware)
            }
            .map_err(|error| format!("contained passive firmware initialization: {error:?}"))?;
            record_sae_stage(&format!(
                "vfio_firmware_passive_init_complete patch_sections={} ram_regions={} scatter_chunks={} capability_elements={} eeprom_valid={} clc_rules={} special_unii_mask={:#04x} passive_rx={} probe_tx=false management_tx=false data_tx=false sae=false",
                report.patch_sections,
                report.ram_regions,
                report.scatter_chunks,
                report.nic_capability.element_count,
                report.eeprom_hardware.valid,
                report.clc_rules_applied,
                report.special_unii_mask,
                operation == Operation::RunOneShotPassiveChannel1,
            ));
        }
        Ok(())
    })();

    let mut cleanup = Vec::new();
    record_sae_stage("vfio_dma_cleanup_begin");
    if let Err(error) = wfdma.write_active_wfdma(0xd4204, 0) {
        cleanup.push(format!("mask host IRQ: {error}"));
    }
    if let Err(error) = pcie_mac.write_pcie_mac_interrupt_enable_zero() {
        cleanup.push(format!("mask PCIe MAC IRQ: {error}"));
    }
    match wfdma.read(0xd4208) {
        Ok(global) if global != u32::MAX => {
            let disabled =
                global & !((1 << 0) | (1 << 2) | (1 << 15) | (1 << 21) | (1 << 27) | (1 << 28));
            if let Err(error) = wfdma.write_active_wfdma(0xd4208, disabled) {
                cleanup.push(format!("disable WFDMA: {error}"));
            }
        }
        Ok(_) => cleanup.push("disable WFDMA: all-ones readback".into()),
        Err(error) => cleanup.push(format!("read WFDMA for disable: {error}")),
    }
    let idle_deadline = Instant::now() + std::time::Duration::from_millis(100);
    loop {
        match wfdma.read(0xd4208) {
            Ok(global) if global & 0xa == 0 => break,
            Ok(global) if Instant::now() >= idle_deadline => {
                cleanup.push(format!(
                    "WFDMA busy during contained cleanup: {global:#010x}"
                ));
                break;
            }
            Ok(_) => std::thread::sleep(std::time::Duration::from_millis(1)),
            Err(error) => {
                cleanup.push(format!("read WFDMA idle state: {error}"));
                break;
            }
        }
    }
    let bme_disabled = match set_pci_bus_master(bdf, false) {
        Ok(()) => true,
        Err(error) => {
            cleanup.push(format!("disable BME: {error}"));
            false
        }
    };
    if let Some(irq) = capsule
        .active
        .as_mut()
        .and_then(|active| active.irq.as_mut())
        && let Err(error) = irq.disable()
    {
        cleanup.push(format!("disable MSI: {error}"));
    }
    if !bme_disabled {
        park_retention_capsule_ref(capsule);
    }
    // A verified BME clear is the terminal ownership boundary even when an
    // internally busy WFDMA engine failed to report idle.
    if bme_disabled
        && let Some(command_payload) = capsule
            .active
            .as_mut()
            .and_then(|active| active.command_payload.as_mut())
        && let Err(error) = command_payload.secure_zero_bytes(MCU_COMMAND_PAYLOAD_BYTES)
    {
        cleanup.push(format!("secure wipe command payload: {error}"));
    }
    record_sae_stage("vfio_dma_cleanup_masks_and_bme_disabled");
    let release_errors = capsule.release_observable();
    if !release_errors.is_empty() {
        cleanup.push(format!("resource release: {release_errors:?}"));
    }
    record_sae_stage("vfio_dma_resources_unmapped_before_reset");
    if let Err(error) = reset_vfio_device(&capsule.device) {
        cleanup.push(format!("VFIO containment reset: {error}"));
    }
    let safe = verify_active_reset_containment(wfdma, pcie_mac)
        .and_then(|()| verify_pci_dma_disabled(bdf));
    match &safe {
        Ok(()) => record_sae_stage("vfio_dma_safe_state_verified"),
        Err(error) => {
            cleanup.push(format!("safe-state verification: {error}"));
            record_sae_stage("vfio_dma_safe_state_unproven_retaining");
            park_retention_capsule_ref(capsule);
        }
    }
    if safe.is_ok() {
        if let Some(ledger) = capsule.containment.as_mut() {
            for hazard in [
                Hazard::DmaMapping,
                Hazard::BusMaster,
                Hazard::Wfdma,
                Hazard::DeviceIrq,
                Hazard::HostControl,
                Hazard::LabMutated,
            ] {
                ledger.confirm_inactive(hazard);
            }
            ledger.phase = RunPhase::Contained;
        }
    }
    match (primary, cleanup.is_empty()) {
        (Ok(()), true) => {
            record_sae_stage("vfio_dma_resource_round_trip_complete");
            Ok(())
        }
        (Err(primary), true) => Err(primary),
        (Ok(()), false) => Err(format!("DMA cleanup errors: {cleanup:?}")),
        (Err(primary), false) => Err(format!("{primary}; DMA cleanup errors: {cleanup:?}")),
    }
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
impl SaeCredential {
    fn into_passphrase(mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

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
    let credential_len = env::var("DRV_SAE_CREDENTIAL_LEN")
        .map_err(|_| "DRV_SAE_CREDENTIAL_LEN is required")?
        .parse::<usize>()
        .map_err(|_| "DRV_SAE_CREDENTIAL_LEN is invalid")?;
    read_sae_credential_exact(raw_fd, credential_len)
}

#[cfg(feature = "fuchsia-passive")]
fn read_sae_credential_exact(
    raw_fd: RawFd,
    credential_len: usize,
) -> Result<SaeCredential, String> {
    if !(8..=63).contains(&credential_len) {
        return Err("SAE credential length is invalid".into());
    }
    let mut credential = vec![0; credential_len];
    // SAFETY: the root launcher transfers this inherited descriptor exactly
    // once to this one-shot process; taking ownership also closes it promptly.
    let mut file = unsafe { File::from_raw_fd(raw_fd) };
    if file.read_exact(&mut credential).is_err() {
        credential.fill(0);
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
        return Err("read exact SAE credential bytes failed".into());
    }
    Ok(SaeCredential(credential))
}

#[cfg(feature = "fuchsia-passive")]
fn live_client_support(mut query: fidl_softmac::WlanSoftmacQueryResponse) -> ClientSupport {
    query.mac_role = Some(fidl_common::WlanMacRole::Client);
    query.hardware_capability = Some(
        fidl_driver::WlanSoftmacHardwareCapabilityBit::Qos as u32,
    );
    for band in query.band_caps.get_or_insert_default() {
        band.basic_rates.get_or_insert_with(|| match band.band {
            Some(fidl_ieee80211::WlanBand::TwoGhz) => {
                vec![0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24]
            }
            Some(fidl_ieee80211::WlanBand::FiveGhz) => {
                vec![0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c]
            }
            _ => vec![],
        });
    }
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
                hash_to_element_supported: Some(true),
            }),
            ..Default::default()
        },
        spectrum_management: Default::default(),
    }
}

#[cfg(feature = "fuchsia-passive")]
struct SaeCommittedSelfTestMechanics {
    rx: VecDeque<ClientRxFrame>,
    open_auth_response: Option<ClientRxFrame>,
    status77: Option<ClientRxFrame>,
    association_responses: VecDeque<ClientRxFrame>,
    queued_data_after_association: VecDeque<(usize, ClientRxFrame)>,
    rx_during_cid2: Option<ClientRxFrame>,
    association_tx_count: usize,
    tx: Arc<Mutex<Vec<(Vec<u8>, u16, u8)>>>,
    outstanding: MgmtTxOutstanding,
    ring_cidx: u32,
    ring_didx: u32,
    descriptor_done: bool,
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Default)]
struct ComebackSelfTestEffects {
    order: Arc<Mutex<Vec<&'static str>>>,
    fail_association: bool,
}

#[cfg(feature = "fuchsia-passive")]
impl mt7921_softmac_adapter::client_device::Mt7921ClientEffects for ComebackSelfTestEffects {
    fn revoke_scan(&mut self) {}
    fn prepare_runtime_handoff(
        &mut self,
    ) -> mt7921_softmac_adapter::client_device::ClientRuntimeScanState {
        mt7921_softmac_adapter::client_device::ClientRuntimeScanState::ExternalSelection
    }
    fn revoke_lifecycle(&mut self) {}
    fn set_channel(
        &mut self,
        _: ChannelNumber,
        _: ChannelBandwidth,
        _: ChannelNumber,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn join_bss(&mut self, _: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        Ok(())
    }
    fn send_wlan_frame(
        &mut self,
        bytes: &[u8],
        flags: fidl_softmac::WlanTxInfoFlags,
        io: &mut dyn mt7921_softmac_adapter::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        io.transmit_client(bytes, flags)
    }
    fn install_key(
        &mut self,
        _: &fidl_softmac::WlanKeyConfiguration,
        _: &mut dyn mt7921_softmac_adapter::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn notify_association_complete(
        &mut self,
        configuration: &fidl_softmac::WlanAssociationConfig,
        io: &mut dyn mt7921_softmac_adapter::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        if let Some(wmm) = configuration.wmm_params {
            let ac = |value: fidl_driver::WlanWmmAccessCategoryParameters| ClientEdcaAc {
                cw_min: (1u16 << value.ecw_min) - 1,
                cw_max: (1u16 << value.ecw_max) - 1,
                txop: value.txop_limit,
                aifs: u16::from(value.aifsn),
                acm: value.acm,
            };
            let encoded = encode_client_edca_command(
                1,
                0,
                ClientEdcaParameters {
                    ac: [ac(wmm.ac_vo_params), ac(wmm.ac_vi_params), ac(wmm.ac_be_params), ac(wmm.ac_bk_params)],
                },
            )
            .map_err(|_| zx::Status::INVALID_ARGS)?;
            io.submit_edca(&encoded)?;
        }
        self.order.lock().unwrap().push(
            if configuration.qos == Some(true) && configuration.wmm_params.is_some() {
                "wmm"
            } else {
                "non_wmm"
            },
        );
        io.submit_uni(2, &[])?;
        self.order.lock().unwrap().push("cid2");
        if self.fail_association {
            return Err(zx::Status::IO_REFUSED);
        }
        io.submit_uni(3, &[])?;
        self.order.lock().unwrap().push("cid3");
        Ok(())
    }
    fn clear_association(
        &mut self,
        _: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
        _: &mut dyn mt7921_softmac_adapter::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn set_link_up(&mut self, _: bool) -> Result<(), zx::Status> {
        Ok(())
    }
    fn next_rx(
        &mut self,
        io: &mut dyn mt7921_softmac_adapter::client_device::Mt7921ClientIo,
    ) -> Result<Option<ClientRxFrame>, zx::Status> {
        let frame = loop {
            let frame = io.next_client_rx()?;
            let protected_disconnect = frame.as_ref().is_some_and(|frame| {
                frame.bytes.get(..2).is_some_and(|control| {
                    let control = u16::from_le_bytes([control[0], control[1]]);
                    control & 0x400c == 0x4000 && matches!((control >> 4) & 15, 10 | 12)
                })
            });
            if protected_disconnect {
                record_sae_stage(
                    "client_rx_filtered reason=protected_unverified subtype=10 self_test=true",
                );
                continue;
            }
            break frame;
        };
        if frame.is_some() {
            self.order.lock().unwrap().push("rx");
        }
        Ok(frame)
    }
    fn begin_passive_scan(&mut self, _: u64, _: &[ChannelNumber]) -> Result<(), zx::Status> {
        Ok(())
    }
    fn observe_passive_scan(
        &mut self,
        _: u64,
        _: &fuchsia_softmac_port::ScanObservation,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn complete_passive_scan(&mut self, _: u64, _: bool) -> Result<(), zx::Status> {
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
impl SourceExactPassiveMechanics for SaeCommittedSelfTestMechanics {
    type Error = std::io::Error;

    fn prepare_passive_receive(&mut self) -> Result<PassivePrerequisites, Self::Error> {
        Ok(PassivePrerequisites {
            channel_domain_mask_zero: true,
            mac_mmio_initialized: true,
            data_rx_owned: true,
        })
    }
    fn command(&mut self, _: &PassiveMcuCommand, _: &[u8], _: bool) -> Result<(), Self::Error> {
        Ok(())
    }
    fn next_event(&mut self, _: i64) -> Result<Option<PassiveMechanicsEvent>, Self::Error> {
        Ok(None)
    }
    fn confirm_scan_done(&mut self, _: u8) -> Result<(), Self::Error> {
        Ok(())
    }
    fn submit_client_uni(&mut self, cid: u8, _: &[u8]) -> Result<(), zx::Status> {
        if cid == 2
            && let Some(frame) = self.rx_during_cid2.take()
        {
            self.rx.push_back(frame);
            println!("self_test_control_wait_rx cid=2 queued=persistent");
        }
        Ok(())
    }
    fn submit_client_edca(&mut self, encoded: &[u8]) -> Result<(), zx::Status> {
        if encoded.len() != 108
            || encoded.get(36..39) != Some(&[0x1d, 0xa0, 1])
            || encoded.get(64..84)
                != Some(&[
                    7, 0, 15, 0, 94, 0, 2, 0, 0, 0, 3, 0, 7, 0, 47, 0, 2, 0, 0, 0,
                ])
            || encoded.get(104..107) != Some(&[0, 1, 0])
        {
            return Err(zx::Status::IO_DATA_INTEGRITY);
        }
        println!("self_test_wmm_edca completion=true dma_consumed=true firmware_ack=not_requested_linux ac_vo=aifs2,cwmin3,cwmax7,txop47 ac_vi=aifs2,cwmin7,cwmax15,txop94 ac_be=aifs3,cwmin15,cwmax1023,txop0 ac_bk=aifs7,cwmin15,cwmax1023,txop0 tid7_ac=vo qidx3_programmed=true data_ring=0");
        Ok(())
    }
    fn transmit_client(
        &mut self,
        bytes: &[u8],
        _: fidl_softmac::WlanTxInfoFlags,
    ) -> Result<(), zx::Status> {
        println!(
            "self_test_management_tx stage=ownership cidx={} didx={} dma_done={} outstanding={}",
            self.ring_cidx,
            self.ring_didx,
            self.descriptor_done,
            !self.outstanding.is_empty()
        );
        if !self.outstanding.is_empty() {
            println!("self_test_management_tx outcome=blocked reason=completion_outstanding");
            return Err(zx::Status::SHOULD_WAIT);
        }
        if self.ring_cidx != 0 || self.ring_didx != 0 {
            if self.ring_cidx != 1 || self.ring_didx != 1 || !self.descriptor_done {
                return Err(zx::Status::IO_DATA_INTEGRITY);
            }
            // A normal DIDX write is ignored; only ring-0's DTX pointer reset
            // transitions the device-owned index.
            let didx_after_direct_write = self.ring_didx;
            if didx_after_direct_write != 1 {
                return Err(zx::Status::IO_DATA_INTEGRITY);
            }
            println!("self_test_management_tx stage=direct_didx_write result=ignored");
            self.ring_didx = 0;
            self.ring_cidx = 0;
            self.descriptor_done = false;
            println!(
                "self_test_management_tx stage=ring_local_reset register=dtx_ptr bit=0 result=complete"
            );
        }
        let (token, pid) = self
            .outstanding
            .reserve()
            .map_err(|_| zx::Status::NO_RESOURCES)?;
        self.ring_cidx = 1;
        self.ring_didx = 1;
        self.descriptor_done = true;
        self.tx.lock().unwrap().push((bytes.to_vec(), token, pid));
        println!("self_test_management_tx outcome=committed token={token} pid={pid}");
        if bytes.get(28..32) == Some(&[126, 0, 20, 0]) {
            if let Some(frame) = self.status77.take() {
                self.rx.push_back(frame);
            }
        }
        let control = bytes
            .get(..2)
            .map(|field| u16::from_le_bytes([field[0], field[1]]))
            .unwrap_or(u16::MAX);
        if control & 0x00fc == 0x00b0 {
            if let Some(frame) = self.open_auth_response.take() {
                self.rx.push_back(frame);
            }
        }
        if control & 0x00fc == 0 {
            self.association_tx_count += 1;
            if let Some(frame) = self.association_responses.pop_front() {
                self.rx.push_back(frame);
            }
            while self
                .queued_data_after_association
                .front()
                .is_some_and(|(attempt, _)| *attempt == self.association_tx_count)
            {
                let (_, frame) = self.queued_data_after_association.pop_front().unwrap();
                self.rx.push_back(frame);
            }
        }
        Ok(())
    }
    fn next_client_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
        if let Some(entry) = self.outstanding.entries.first() {
            let (token, pid) = (entry.token, entry.pid);
            self.outstanding
                .observe(MgmtTxCompletion::Status(Mt7921TxStatus {
                    wcid: 19,
                    pid,
                    acked: true,
                }))
                .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
            self.outstanding
                .observe(MgmtTxCompletion::Free(Mt7921TxFree {
                    wcid: Some(19),
                    token,
                    dropped: false,
                    attempts: 1,
                }))
                .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
            println!("self_test_management_tx completion=paired token={token} pid={pid}");
        }
        Ok(self.rx.pop_front())
    }
}

#[cfg(feature = "fuchsia-passive")]
impl Drop for SaeCommittedSelfTestMechanics {
    fn drop(&mut self) {
        if !self.outstanding.is_empty() {
            println!("self_test_management_tx teardown=contained outstanding=true reuse=false");
        }
    }
}

#[cfg(feature = "fuchsia-passive")]
fn e2e48_translation_error_eapol_frame(client: [u8; 6], peer: [u8; 6]) -> Vec<u8> {
    // E2E48's exact post-association descriptor shape: GROUP1/2/3,
    // HDR_TRANS clear, HDR_TRANS_ERROR set, and unicast-search sentinel.
    let mut rx = vec![0; 56 + 34];
    let length = rx.len() as u32;
    rx[0..4].copy_from_slice(&((2u32 << 27) | length).to_le_bytes());
    rx[4..8].copy_from_slice(&((0x07u32 << 11) | 1023).to_le_bytes());
    rx[8..12].copy_from_slice(&0x4200_0c40u32.to_le_bytes());
    rx[12..16].copy_from_slice(&(36u32 << 8).to_le_bytes());
    rx[52..56].copy_from_slice(&0x7878u32.to_le_bytes());
    let frame = &mut rx[56..];
    frame[0..2].copy_from_slice(&0x0208u16.to_le_bytes());
    frame[4..10].copy_from_slice(&client);
    frame[10..16].copy_from_slice(&peer);
    frame[16..22].copy_from_slice(&peer);
    frame[24..34].copy_from_slice(&[0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e, 1, 2]);
    rx
}

#[cfg(feature = "fuchsia-passive")]
async fn run_sae_committed_fallback_self_test() -> Result<(), String> {
    let client = [2, 0, 0, 0, 0, 1];
    let peer = [2, 0, 0, 0, 0, 2];
    let channel = ChannelNumber {
        band: WlanBand::FiveGhz,
        number: 36,
    };
    let mut valid_comeback = vec![0; 30];
    valid_comeback.extend_from_slice(&[56, 5, 3, 20, 0, 0, 0]);
    let mut malformed_comeback = vec![0; 30];
    malformed_comeback.extend_from_slice(&[56, 4, 3, 20, 0, 0]);
    if association_comeback_interval(&valid_comeback, 30) != Some((20, 20))
        || association_comeback_interval(&malformed_comeback, 30).is_some()
        || association_comeback_interval(&valid_comeback[..30], 30).is_some()
    {
        return Err("self-test association comeback IE contract failed".into());
    }
    println!(
        "self_test_association_comeback result=pass ie_id_lengths=56:5 valid_tu=20 valid_ms=20 malformed=failure no_ie=failure"
    );
    let raw_eapol = e2e48_translation_error_eapol_frame(client, peer);
    let parsed_eapol = parse_connac2_rx_frame(&raw_eapol)
        .map_err(|error| format!("self-test E2E48 raw EAPOL parse: {error:?}"))?;
    if parsed_eapol.bytes.get(24..32) != Some(&[0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]) {
        return Err("self-test E2E48 raw EAPOL decapsulation failed".into());
    }
    let m1 = classify_client_data_frame(&parsed_eapol.bytes, client, peer);
    if m1.frame_type != 2
        || m1.subtype != 0
        || m1.to_ds
        || !m1.from_ds
        || !m1.addr1_is_client
        || !m1.addr2_is_peer
        || !m1.addr3_is_bssid
        || !m1.snap_present
        || m1.ether_type != Some(0x888e)
        || m1.llc_result != "valid"
    {
        return Err("self-test exact AP-to-STA EAPOL M1 classification failed".into());
    }
    let association = LegacyWmeAssociation {
        bss_index: 0,
        peer_wcid: 7,
        aid: 42,
        peer,
        rcpi: 100,
        negotiated_qos: true,
        mfp_required: false,
    };
    let mut rx_gate = ClientFirmwareEffectsState::default();
    let rx_channel = mt7921_port_spike::ClientChannelLease {
        channel: ClientPhysicalChannel {
            band: 1,
            primary: 36,
            center: 36,
            bandwidth: 0,
            center2: 0,
        },
        generation: 1,
    };
    rx_gate
        .bind_join(peer, rx_channel, 100)
        .map_err(|error| format!("self-test E2E48 bind: {error}"))?;
    let mut activation_commands = Vec::new();
    rx_gate
        .prepare_preauth_peer(
            LegacyWmeAssociation {
                aid: 0,
                negotiated_qos: false,
                ..association
            },
            rx_channel,
            |cid, command| {
                activation_commands.push((cid, command.to_vec()));
                Ok(())
            },
        )
        .map_err(|error| format!("self-test preauth peer: {error}"))?;
    rx_gate
        .associate(association, rx_channel, |cid, command| {
            activation_commands.push((cid, command.to_vec()));
            Ok(())
        })
        .map_err(|error| format!("self-test E2E48 association: {error}"))?;
    let expected_preauth = mt7921_port_spike::encode_preauth_peer_wcid_command(1, 0, 7, peer, 100)
        .map_err(|error| format!("self-test preauth peer fixture: {error}"))?;
    let expected_bss = encode_client_bss_command(2, 0, peer, 36, 100, true, true)
        .map_err(|error| format!("self-test association BSS fixture: {error}"))?;
    let expected_peer = encode_legacy_wme_add_wcid_command(3, 0, 7, 42, peer, 100)
        .map_err(|error| format!("self-test association peer fixture: {error}"))?;
    let wtbl_structure = expected_peer[120] == 7
        && expected_peer[121] == 1
        && expected_peer[122..124] == [4, 0]
        && expected_peer[68..74] == expected_peer[132..138]
        && expected_peer[148..156] == [1, 0, 12, 0, 0, 1, 1, 1]
        && expected_peer[160..168] == [6, 0, 8, 0, 1, 0, 1, 0];
    let unavailable_readback =
        classify_wtbl_peer_readback(&peer, Err(WtblPeerReadback::Unavailable));
    let all_ones_readback = classify_wtbl_peer_readback(&peer, Err(WtblPeerReadback::AllOnes));
    if activation_commands != [(3, expected_preauth), (2, expected_bss), (3, expected_peer)]
        || !wtbl_structure
        || unavailable_readback != WtblPeerReadback::Unavailable
        || all_ones_readback != WtblPeerReadback::AllOnes
        || !rx_gate.bss_programmed
        || !rx_gate.association.is_some_and(|active| {
            active.bss_index == 0
                && active.peer_wcid == 7
                && active.aid == 42
                && active.peer == peer
        })
        || rx_gate.association_generation.is_none()
        || rx_gate.controlled_port_open
        || rx_gate.tx_generation(true).is_err()
    {
        return Err(
            "self-test association did not publish exact data-RX/EAPOL-ready activation".into(),
        );
    }
    println!(
        "self_test_association_activation result=pass transcript=DEV,BSS,peer_preauth,SAE,assoc_response,peer_associated cid_order=3,2,3 preauth_peer_wcid=7 preauth_aid=0 associated_aid=42 wtbl_reset_set=true nested_generic_peer_match=true rx_lookup=true no_rx_trans=true diagnostic_readback_nonfatal=true readback_categories=unavailable,all_ones bss_active=true association_generation=true controlled_port_open=false eapol_ready=true"
    );
    let generation = ClientDataGeneration::Association(rx_gate.association_generation.unwrap());
    let candidate = ClientRxCandidate {
        generation,
        eapol: true,
        wcid: 7,
        tid: 0,
        group: false,
        key_id: 0,
        security_mode: 0,
        cm: false,
        clm: false,
        icv_error: false,
        mic_error: false,
        fcs_error: false,
        pn: [0; 6],
    };
    rx_gate
        .deliver_rx(candidate)
        .map_err(|error| format!("self-test immediate WCID7 EAPOL gate: {error}"))?;
    rx_gate
        .deliver_rx(candidate)
        .map_err(|error| format!("self-test retried WCID7 EAPOL gate: {error}"))?;
    let mut non_eapol = candidate;
    non_eapol.eapol = false;
    if rx_gate.deliver_rx(non_eapol).is_ok() {
        return Err("self-test E2E48 sentinel admitted non-EAPOL data".into());
    }
    let protected_disassociation = ClientRxCandidate {
        generation,
        eapol: false,
        wcid: 7,
        tid: 0,
        group: false,
        key_id: 0,
        security_mode: 4,
        cm: false,
        clm: false,
        icv_error: false,
        mic_error: false,
        fcs_error: false,
        pn: [0, 0, 0, 0, 0, 1],
    };
    if rx_gate
        .deliver_protected_management_rx(protected_disassociation)
        .is_ok()
    {
        return Err("self-test protected management admitted before PMF/PTK".into());
    }
    rx_gate.association.as_mut().unwrap().mfp_required = true;
    rx_gate
        .install_ptk(&[0x11; 16], 0, |_, _| Ok(()))
        .map_err(|error| format!("self-test protected management PTK: {error}"))?;
    rx_gate
        .deliver_protected_management_rx(protected_disassociation)
        .map_err(|error| format!("self-test current protected management: {error}"))?;
    if rx_gate
        .deliver_protected_management_rx(protected_disassociation)
        .is_ok()
    {
        return Err("self-test protected management replay was admitted".into());
    }
    let mut protected_fixture = vec![0; 42];
    protected_fixture[0..2].copy_from_slice(&0x40a0u16.to_le_bytes());
    protected_fixture[4..10].copy_from_slice(&client);
    protected_fixture[10..16].copy_from_slice(&peer);
    protected_fixture[16..22].copy_from_slice(&peer);
    protected_fixture[32..34].copy_from_slice(&9u16.to_le_bytes());
    strip_verified_management_ccmp(&mut protected_fixture)
        .map_err(|_| "self-test verified protected management strip failed")?;
    if protected_fixture.len() != 26
        || u16::from_le_bytes(protected_fixture[24..26].try_into().unwrap()) != 9
        || strip_verified_management_ccmp(&mut vec![0; 25]).is_ok()
    {
        return Err("self-test protected management plaintext shape failed".into());
    }
    println!(
        "self_test_protected_management result=pass exact_ciphertext_len=42 pre_key=protected_unverified current_key=decrypted_dispatched reason=9 replay=filtered malformed=terminal comeback_retry=continues"
    );
    let mut backlog = VecDeque::new();
    let mut backlog_provenance = DescriptorProvenance::new();
    for value in 0..CLIENT_RX_BACKLOG_CAPACITY {
        enqueue_client_rx_backlog(
            &mut backlog_provenance,
            &mut backlog,
            PrivateRawFrameCarrier {
                bytes: vec![value as u8],
                occurrence: None,
            },
        )?;
    }
    if !backlog
        .iter()
        .enumerate()
        .all(|(index, frame)| frame.bytes == [index as u8])
    {
        return Err("self-test persistent RX backlog reordered frames".into());
    }
    if enqueue_client_rx_backlog(
        &mut backlog_provenance,
        &mut backlog,
        PrivateRawFrameCarrier {
            bytes: vec![0xff],
            occurrence: None,
        },
    )
    .is_ok()
        || !backlog.is_empty()
    {
        return Err("self-test persistent RX backlog did not fail closed".into());
    }
    println!(
        "self_test_control_wait_rx result=pass cid2_arrival=persistent fifo_order=true capacity=64 overflow=fail_closed teardown=wipe"
    );
    let mut malformed = raw_eapol;
    malformed[0..4].copy_from_slice(&((2u32 << 27) | 55).to_le_bytes());
    if parse_connac2_rx_frame(&malformed) != Err(PassiveRxError::Truncated) {
        return Err("self-test E2E48 malformed descriptor was not terminal".into());
    }
    println!(
        "self_test_client_rx result=pass rxd2=0x42000c40 hdr_trans=false raw_80211=true from_ds=true rfc1042=true ether_type=0x888e eapol_m1=immediate_and_retried_admitted wcid=7 non_eapol=filtered malformed=terminal"
    );
    let mut auth = vec![0; 32];
    auth[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
    auth[4..10].copy_from_slice(&client);
    auth[10..16].copy_from_slice(&peer);
    auth[16..22].copy_from_slice(&peer);
    auth[24..26].copy_from_slice(&3u16.to_le_bytes());
    auth[26..28].copy_from_slice(&1u16.to_le_bytes());
    auth[28..30].copy_from_slice(&77u16.to_le_bytes());
    auth[30..32].copy_from_slice(&20u16.to_le_bytes());
    let status77 = ClientRxFrame {
        bytes: auth,
        status: fidl_softmac::WlanRxInfo {
            rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
            valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
            phy: fidl_ieee80211::WlanPhyType::Ofdm,
            data_rate: 0,
            primary: channel,
            bandwidth: ChannelBandwidth::Cbw20,
            vht_secondary_80_channel: ChannelNumber {
                number: 0,
                ..channel
            },
            mcs: 0,
            rssi_dbm: -40,
            snr_dbh: 0,
        },
        security: None,
    };
    let missing_tx = Arc::new(Mutex::new(Vec::new()));
    let mut missing_completion = SaeCommittedSelfTestMechanics {
        rx: VecDeque::new(),
        open_auth_response: None,
        status77: None,
        association_responses: VecDeque::new(),
        queued_data_after_association: VecDeque::new(),
        rx_during_cid2: None,
        association_tx_count: 0,
        tx: Arc::clone(&missing_tx),
        outstanding: MgmtTxOutstanding::default(),
        ring_cidx: 0,
        ring_didx: 0,
        descriptor_done: false,
    };
    let mut synthetic_group20 = vec![0; 32];
    synthetic_group20[28..32].copy_from_slice(&[126, 0, 20, 0]);
    missing_completion
        .transmit_client(&synthetic_group20, fidl_softmac::WlanTxInfoFlags::empty())
        .map_err(|e| format!("self-test missing-completion first commit: {e}"))?;
    if missing_completion
        .transmit_client(&synthetic_group20, fidl_softmac::WlanTxInfoFlags::empty())
        != Err(zx::Status::SHOULD_WAIT)
        || missing_tx.lock().unwrap().len() != 1
    {
        return Err("self-test missing completion did not block ring reuse".into());
    }
    drop(missing_completion);
    let capability = mt7921_port_spike::NicCapability {
        element_count: 2,
        mac_address: Some(client),
        phy: Some(mt7921_port_spike::NicPhyCapability {
            ht: true,
            vht: true,
            has_5ghz: true,
            max_bandwidth: 2,
            spatial_streams: 2,
            hardware_path: 3,
            he: true,
        }),
        has_6ghz: Some(false),
        chip_capability: None,
        unknown_elements: 0,
    };
    let mut auth_response = vec![0u8; 24];
    auth_response[0] = 0xb0;
    auth_response[4..10].copy_from_slice(&client);
    auth_response[10..16].copy_from_slice(&peer);
    auth_response[16..22].copy_from_slice(&peer);
    auth_response.extend_from_slice(&[0, 0, 2, 0, 0, 0]);
    let mut comeback_response = vec![0u8; 24];
    comeback_response[0] = 0x10;
    comeback_response[4..10].copy_from_slice(&client);
    comeback_response[10..16].copy_from_slice(&peer);
    comeback_response[16..22].copy_from_slice(&peer);
    comeback_response.extend_from_slice(&[1, 0, 30, 0, 1, 0, 56, 5, 3, 20, 0, 0, 0]);
    let mut foreign_reason9 = vec![0u8; 24];
    foreign_reason9[0] = 0xa0;
    foreign_reason9[4..10].copy_from_slice(&client);
    foreign_reason9[10..16].copy_from_slice(&[2, 0, 0, 0, 0, 99]);
    foreign_reason9[16..22].copy_from_slice(&[2, 0, 0, 0, 0, 99]);
    foreign_reason9[22..24].copy_from_slice(&(6u16 << 4).to_le_bytes());
    foreign_reason9.extend_from_slice(&9u16.to_le_bytes());
    let mut peer_reason9 = foreign_reason9.clone();
    peer_reason9[10..16].copy_from_slice(&peer);
    peer_reason9[16..22].copy_from_slice(&peer);
    let mut protected_disassociation = vec![0x5a; 42];
    protected_disassociation[0..2].copy_from_slice(&0x40a0u16.to_le_bytes());
    protected_disassociation[2..4].fill(0);
    protected_disassociation[4..10].copy_from_slice(&client);
    protected_disassociation[10..16].copy_from_slice(&peer);
    protected_disassociation[16..22].copy_from_slice(&peer);
    protected_disassociation[22..24].copy_from_slice(&(7u16 << 4).to_le_bytes());
    let comeback_tx = Arc::new(Mutex::new(Vec::new()));
    let comeback_transport = SourceExactPassiveTransport::new(
        SaeCommittedSelfTestMechanics {
            rx: VecDeque::new(),
            open_auth_response: Some(ClientRxFrame {
                bytes: auth_response,
                status: fidl_softmac::WlanRxInfo {
                    rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
                    valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
                    phy: fidl_ieee80211::WlanPhyType::Ofdm,
                    data_rate: 0,
                    primary: channel,
                    bandwidth: ChannelBandwidth::Cbw80,
                    vht_secondary_80_channel: ChannelNumber {
                        number: 0,
                        ..channel
                    },
                    mcs: 0,
                    rssi_dbm: -40,
                    snr_dbh: 0,
                },
                security: None,
            }),
            status77: None,
            association_responses: VecDeque::from([ClientRxFrame {
                bytes: comeback_response,
                status: fidl_softmac::WlanRxInfo {
                    rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
                    valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
                    phy: fidl_ieee80211::WlanPhyType::Ofdm,
                    data_rate: 0,
                    primary: channel,
                    bandwidth: ChannelBandwidth::Cbw80,
                    vht_secondary_80_channel: ChannelNumber {
                        number: 0,
                        ..channel
                    },
                    mcs: 0,
                    rssi_dbm: -40,
                    snr_dbh: 0,
                },
                security: None,
            }]),
            queued_data_after_association: VecDeque::from([
                (
                    1,
                    ClientRxFrame {
                        bytes: protected_disassociation,
                        status: fidl_softmac::WlanRxInfo {
                            rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
                            valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
                            phy: fidl_ieee80211::WlanPhyType::Ofdm,
                            data_rate: 0,
                            primary: channel,
                            bandwidth: ChannelBandwidth::Cbw80,
                            vht_secondary_80_channel: ChannelNumber {
                                number: 0,
                                ..channel
                            },
                            mcs: 0,
                            rssi_dbm: -40,
                            snr_dbh: 0,
                        },
                        security: None,
                    },
                ),
                (
                    2,
                    ClientRxFrame {
                        bytes: foreign_reason9,
                        status: fidl_softmac::WlanRxInfo {
                            rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
                            valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
                            phy: fidl_ieee80211::WlanPhyType::Ofdm,
                            data_rate: 0,
                            primary: channel,
                            bandwidth: ChannelBandwidth::Cbw80,
                            vht_secondary_80_channel: ChannelNumber {
                                number: 0,
                                ..channel
                            },
                            mcs: 0,
                            rssi_dbm: -40,
                            snr_dbh: 0,
                        },
                        security: None,
                    },
                ),
                (
                    2,
                    ClientRxFrame {
                        bytes: peer_reason9,
                        status: fidl_softmac::WlanRxInfo {
                            rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
                            valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
                            phy: fidl_ieee80211::WlanPhyType::Ofdm,
                            data_rate: 0,
                            primary: channel,
                            bandwidth: ChannelBandwidth::Cbw80,
                            vht_secondary_80_channel: ChannelNumber {
                                number: 0,
                                ..channel
                            },
                            mcs: 0,
                            rssi_dbm: -40,
                            snr_dbh: 0,
                        },
                        security: None,
                    },
                ),
            ]),
            rx_during_cid2: None,
            association_tx_count: 0,
            tx: Arc::clone(&comeback_tx),
            outstanding: MgmtTxOutstanding::default(),
            ring_cidx: 0,
            ring_didx: 0,
            descriptor_done: false,
        },
        capability,
    )
    .map_err(|e| format!("self-test comeback transport: {e}"))?;
    let candidates = candidate_channels(capability);
    let comeback_adapter = Mt7921SoftmacAdapter::new(
        comeback_transport,
        capability,
        candidates.clone(),
        vec![channel],
    )
    .map_err(|e| format!("self-test comeback adapter: {e}"))?;
    let comeback_support = live_client_support(query_from_capabilities(capability, &candidates));
    let comeback_device_info = wlan_mlme::mlme_device_info_from_softmac(comeback_support.query.clone())
        .map_err(|e| format!("self-test comeback device info: {e}"))?;
    let (comeback_device, comeback_runner) = Mt7921ClientDevice::new(
        ComebackSelfTestEffects::default(),
        comeback_adapter,
        comeback_support.clone(),
    );
    let mut comeback_runtime = PinnedClientRuntime::new(
        comeback_device,
        comeback_runner,
        wlan_sme::client::ClientConfig::default(),
        comeback_device_info,
        comeback_support.security,
        comeback_support.spectrum_management,
        fuchsia_inspect::Inspector::default(),
    )
    .await
    .map_err(|e| format!("self-test comeback runtime: {e}"))?;
    let comeback_request = fidl_sme::ConnectRequest {
        ssid: b"test".to_vec(),
        bss_description: fidl_ieee80211::BssDescription {
            bssid: peer,
            bss_type: fidl_ieee80211::BssType::Infrastructure,
            beacon_period: 100,
            capability_info: 1,
            ies: vec![0, 4, b't', b'e', b's', b't', 1, 2, 0x8c, 0x12],
            primary: channel,
            bandwidth: ChannelBandwidth::Cbw80,
            vht_secondary_80_channel: ChannelNumber {
                number: 0,
                ..channel
            },
            rssi_dbm: -40,
            snr_db: 20,
        },
        multiple_bss_candidates: false,
        authentication: fidl_internal::Authentication {
            protocol: fidl_internal::Protocol::Open,
            credentials: None,
        },
        deprecated_scan_type: fidl_common::ScanType::Passive,
    };
    let comeback_started = Instant::now();
    let comeback_result = comeback_runtime
        .connect(
            comeback_request.clone(),
            Instant::now() + std::time::Duration::from_millis(100),
        )
        .await;
    let association_requests: Vec<_> = comeback_tx
        .lock()
        .unwrap()
        .iter()
        .filter(|(frame, _, _)| {
            frame
                .get(..2)
                .is_some_and(|fc| u16::from_le_bytes([fc[0], fc[1]]) & 0x00fc == 0)
        })
        .map(|(frame, _, _)| frame.clone())
        .collect();
    if comeback_result != Err(mt7921_softmac_adapter::client_device::PinnedConnectError::Timeout)
        || comeback_started.elapsed() < std::time::Duration::from_millis(15)
        || association_requests.len() != 2
        || association_requests[0][22..24] == association_requests[1][22..24]
        || association_requests[0][24..] != association_requests[1][24..]
    {
        return Err(format!(
            "self-test production comeback timer failed result={comeback_result:?} requests={}",
            association_requests.len()
        ));
    }
    println!(
        "self_test_association_comeback_runtime result=pass timer_stream=driven tu=20 protected_len42=filtered retry_requests=2 sequence=fresh body=identical foreign_reason9=ignored peer_reason9=accepted_connect_failure outer_deadline_ms=100"
    );
    let mut burst_auth = vec![0u8; 24];
    burst_auth[0] = 0xb0;
    burst_auth[4..10].copy_from_slice(&client);
    burst_auth[10..16].copy_from_slice(&peer);
    burst_auth[16..22].copy_from_slice(&peer);
    burst_auth.extend_from_slice(&[0, 0, 2, 0, 0, 0]);
    let mut burst_assoc = vec![0u8; 24];
    burst_assoc[0] = 0x10;
    burst_assoc[4..10].copy_from_slice(&client);
    burst_assoc[10..16].copy_from_slice(&peer);
    burst_assoc[16..22].copy_from_slice(&peer);
    burst_assoc.extend_from_slice(&[
        1, 0, 0, 0, 42, 0, 1, 2, 0x8c, 0x12, 48, 20, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac,
        4, 1, 0, 0, 0x0f, 0xac, 2, 0, 0,
    ]);
    burst_assoc.extend_from_slice(&[
        0xdd, 0x18, 0x00, 0x50, 0xf2, 0x02, 0x01, 0x01, 0x80, 0x00, 0x03, 0xa4, 0x00,
        0x00, 0x27, 0xa4, 0x00, 0x00, 0x42, 0x43, 0x5e, 0x00, 0x62, 0x32, 0x2f, 0x00,
    ]);
    let mut burst_m1 = vec![0x08, 0x02, 0, 0];
    burst_m1.extend_from_slice(&client);
    burst_m1.extend_from_slice(&peer);
    burst_m1.extend_from_slice(&peer);
    burst_m1.extend_from_slice(&[0, 0, 0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
    burst_m1.extend_from_slice(&[1, 3, 0, 95, 2, 0, 0x8a, 0, 16]);
    burst_m1.extend_from_slice(&1u64.to_be_bytes());
    burst_m1.extend_from_slice(&[0x11; 32]);
    burst_m1.extend_from_slice(&[0; 16 + 8 + 8 + 16]);
    burst_m1.extend_from_slice(&[0, 0]);
    let start = eapol_start_frame(client, peer, true);
    if !is_authenticator_m1(&burst_m1)
        || start.get(..2) != Some(&[0x88, 0x01])
        || start.get(4..10) != Some(&peer)
        || start.get(10..16) != Some(&client)
        || start.get(16..22) != Some(&[0x01, 0x80, 0xc2, 0, 0, 3])
        || start.get(24..38)
            != Some(&[7, 0, 0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e, 1, 1, 0, 0])
    {
        return Err("self-test EAPOL-Start standards fixture failed".into());
    }
    let start_txwi = encode_client_data_txwi(
        start.len(),
        0x1234_5000,
        7,
        9,
        true,
        false,
        true,
        7,
    )
    .map_err(|error| format!("self-test EAPOL-Start TXWI: {error}"))?;
    let linux_words = [
        0x0600_0046u32,
        0x8072_6807,
        0x8000_2028,
        0x1000_7800,
        0,
        0x409,
        0x004b_0004,
        0x0028_0000,
        0x0000_8007,
        0,
        0x1234_5000,
        0x0000_8026,
        0,
        0,
        0,
        0,
    ];
    let linux_golden = linux_words
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    let management_txwi = encode_client_management_tx(
        &burst_auth,
        0x2234_4000,
        0x2234_5000,
        8,
        10,
    )
    .map_err(|error| format!("self-test management TXWI: {error}"))?;
    if start_txwi.as_slice() != linux_golden
        || management_txwi.txwi[..32] == start_txwi[..32]
    {
        return Err("self-test Linux EAPOL-Start descriptor golden diverged".into());
    }
    println!(
        "self_test_eapol_liveness result=pass type=start timer_ms=1000 one_shot=true immediate_m1=suppressed timeout_flood=false linux_golden=exact qos=true tid=7 wcid=7 management_data=distinct missing_wcid=blocked"
    );
    let burst_status = || fidl_softmac::WlanRxInfo {
        rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
        valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
        phy: fidl_ieee80211::WlanPhyType::Ofdm,
        data_rate: 0,
        primary: channel,
        bandwidth: ChannelBandwidth::Cbw80,
        vht_secondary_80_channel: ChannelNumber {
            number: 0,
            ..channel
        },
        mcs: 0,
        rssi_dbm: -40,
        snr_dbh: 0,
    };
    let burst_tx = Arc::new(Mutex::new(Vec::new()));
    let burst_order = Arc::new(Mutex::new(Vec::new()));
    let burst_transport = SourceExactPassiveTransport::new(
        SaeCommittedSelfTestMechanics {
            rx: VecDeque::new(),
            open_auth_response: Some(ClientRxFrame {
                bytes: burst_auth,
                status: burst_status(),
                security: None,
            }),
            status77: None,
            association_responses: VecDeque::from([ClientRxFrame {
                bytes: burst_assoc,
                status: burst_status(),
                security: None,
            }]),
            queued_data_after_association: VecDeque::new(),
            rx_during_cid2: Some(ClientRxFrame {
                bytes: burst_m1,
                status: burst_status(),
                security: None,
            }),
            association_tx_count: 0,
            tx: Arc::clone(&burst_tx),
            outstanding: MgmtTxOutstanding::default(),
            ring_cidx: 0,
            ring_didx: 0,
            descriptor_done: false,
        },
        capability,
    )
    .map_err(|e| format!("self-test burst transport: {e}"))?;
    let burst_adapter = Mt7921SoftmacAdapter::new(
        burst_transport,
        capability,
        candidates.clone(),
        vec![channel],
    )
    .map_err(|e| format!("self-test burst adapter: {e}"))?;
    let burst_support = live_client_support(query_from_capabilities(capability, &candidates));
    let burst_info = wlan_mlme::mlme_device_info_from_softmac(burst_support.query.clone())
        .map_err(|e| format!("self-test burst device info: {e}"))?;
    let (burst_device, burst_runner) = Mt7921ClientDevice::new(
        ComebackSelfTestEffects {
            order: Arc::clone(&burst_order),
            fail_association: false,
        },
        burst_adapter,
        burst_support.clone(),
    );
    let mut burst_runtime = PinnedClientRuntime::new(
        burst_device,
        burst_runner,
        wlan_sme::client::ClientConfig::default(),
        burst_info,
        burst_support.security,
        burst_support.spectrum_management,
        fuchsia_inspect::Inspector::default(),
    )
    .await
    .map_err(|e| format!("self-test burst runtime: {e}"))?;
    let mut burst_request = comeback_request.clone();
    burst_request.bss_description.capability_info = 0x11;
    burst_request.bss_description.ies.extend_from_slice(&[
        48, 20, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 2, 0, 0,
    ]);
    burst_request.authentication = fidl_internal::Authentication {
        protocol: fidl_internal::Protocol::Wpa2Personal,
        credentials: Some(Box::new(fidl_internal::Credentials::Wpa(
            fidl_internal::WpaCredentials::Psk([1; 32]),
        ))),
    };
    let _ = burst_runtime
        .connect(
            burst_request,
            Instant::now() + std::time::Duration::from_millis(100),
        )
        .await;
    let order = burst_order.lock().unwrap();
    let wmm = order.iter().position(|stage| *stage == "wmm");
    let cid2 = order.iter().position(|stage| *stage == "cid2");
    let cid3 = order.iter().position(|stage| *stage == "cid3");
    let m1_rx = order
        .iter()
        .enumerate()
        .filter(|(_, stage)| **stage == "rx")
        .nth(2)
        .map(|(index, _)| index);
    let m2 = burst_tx.lock().unwrap().iter().any(|(frame, _, _)| {
        frame
            .windows(8)
            .any(|window| window == [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e])
    });
    let wmm_request = burst_tx.lock().unwrap().iter().any(|(frame, _, _)| {
        frame.first() == Some(&0x00)
            && frame.ends_with(&[0xdd, 0x07, 0x00, 0x50, 0xf2, 0x02, 0x00, 0x01, 0x00])
    });
    if !matches!((wmm, cid2, cid3, m1_rx), (Some(w), Some(a), Some(b), Some(c)) if w < a && a < b && b < c)
        || !m2
        || !wmm_request
    {
        return Err(format!(
            "self-test association control priority failed order={order:?} m2={m2} wmm_request={wmm_request}"
        ));
    }
    drop(order);
    println!(
        "self_test_association_control_priority result=pass m1_arrival=during_cid2_wait persistent_fifo=true cid_order=2,3 before_m1=true m2=true wmm_request=true negotiated_qos=true ac_params_installed=true"
    );
    let tx = Arc::new(Mutex::new(Vec::new()));
    let transport = SourceExactPassiveTransport::new(
        SaeCommittedSelfTestMechanics {
            rx: VecDeque::new(),
            open_auth_response: None,
            status77: Some(status77),
            association_responses: VecDeque::new(),
            queued_data_after_association: VecDeque::new(),
            rx_during_cid2: None,
            association_tx_count: 0,
            tx: Arc::clone(&tx),
            outstanding: MgmtTxOutstanding::default(),
            ring_cidx: 0,
            ring_didx: 0,
            descriptor_done: false,
        },
        capability,
    )
    .map_err(|e| format!("self-test transport: {e}"))?;
    let candidates = candidate_channels(capability);
    let adapter =
        Mt7921SoftmacAdapter::new(transport, capability, candidates.clone(), vec![channel])
            .map_err(|e| format!("self-test adapter: {e}"))?;
    let physical = client_physical_channel(
        channel,
        ChannelBandwidth::Cbw80,
        ChannelNumber {
            number: 0,
            ..channel
        },
    )
    .map_err(|e| format!("self-test channel: {e:?}"))?;
    let shared = Arc::new(Mutex::new(LiveClientState {
        selection: ClientTargetBssLease::retain(ClientScanEvidence {
            scan_id: 7,
            observation_generation: 1,
            observation_timestamp_nanos: 1,
            bssid: peer,
            channel: physical,
        })
        .map_err(|e| format!("self-test selection: {e:?}"))?,
        channel: ClientChannelContext::default(),
    }));
    let effects = LiveClientEffects {
        state: Arc::clone(&shared),
        target: peer,
        client,
        rcpi: 100,
        firmware: ClientFirmwareEffectsState::default(),
        post_association_data_wait: None,
        eapol_start_deadline: None,
        eapol_start_emitted: false,
    };
    let support = live_client_support(query_from_capabilities(capability, &candidates));
    let device_info = wlan_mlme::mlme_device_info_from_softmac(support.query.clone())
        .map_err(|e| format!("self-test device info: {e}"))?;
    let security = support.security.clone();
    let spectrum = support.spectrum_management.clone();
    let (mut device, runner) = Mt7921ClientDevice::new(effects, adapter, support);
    DeviceOps::set_channel(
        &mut device,
        channel,
        ChannelBandwidth::Cbw80,
        ChannelNumber {
            number: 0,
            ..channel
        },
    )
    .await
    .map_err(|e| format!("self-test set channel: {e}"))?;
    {
        let mut state = shared.lock().unwrap();
        let secondary = ChannelNumber {
            number: 0,
            ..channel
        };
        state
            .mark_rate_power_ready(peer, channel, ChannelBandwidth::Cbw80, secondary)
            .map_err(|e| format!("self-test ready: {e}"))?;
        state
            .authorize_sae(peer, channel, ChannelBandwidth::Cbw80, secondary)
            .map_err(|e| format!("self-test authorize: {e}"))?;
    }
    let mut config = wlan_sme::client::ClientConfig::default();
    config.wpa3_supported = true;
    let mut runtime = PinnedClientRuntime::new(
        device,
        runner,
        config,
        device_info,
        security,
        spectrum,
        fuchsia_inspect::Inspector::default(),
    )
    .await
    .map_err(|e| format!("self-test runtime: {e}"))?;
    let request = fidl_sme::ConnectRequest {
        ssid: b"test".to_vec(),
        bss_description: fidl_ieee80211::BssDescription {
            bssid: peer,
            bss_type: fidl_ieee80211::BssType::Infrastructure,
            beacon_period: 100,
            capability_info: 0x11,
            ies: vec![
                0, 4, b't', b'e', b's', b't', 1, 2, 0x8c, 0x12, 48, 20, 1, 0, 0, 0x0f, 0xac, 4, 1,
                0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 8, 0xcc, 0, 244, 1, 0x20,
            ],
            primary: channel,
            bandwidth: ChannelBandwidth::Cbw80,
            vht_secondary_80_channel: ChannelNumber {
                number: 0,
                ..channel
            },
            rssi_dbm: -40,
            snr_db: 20,
        },
        multiple_bss_candidates: false,
        authentication: fidl_internal::Authentication {
            protocol: fidl_internal::Protocol::Wpa3Personal,
            credentials: Some(Box::new(fidl_internal::Credentials::Wpa(
                fidl_internal::WpaCredentials::Passphrase(b"synthetic-password".to_vec()),
            ))),
        },
        deprecated_scan_type: fidl_common::ScanType::Passive,
    };
    let result = runtime
        .connect(
            request,
            Instant::now() + std::time::Duration::from_millis(100),
        )
        .await;
    if result != Err(mt7921_softmac_adapter::client_device::PinnedConnectError::Timeout) {
        return Err(format!("self-test connect result: {result:?}"));
    }
    let tx = tx.lock().unwrap();
    if tx.len() < 2
        || tx[0].0.get(28..32) != Some(&[126, 0, 20, 0])
        || tx[1].0.get(28..32) != Some(&[126, 0, 19, 0])
        || tx[0].1 == tx[1].1
        || tx[0].2 == tx[1].2
    {
        return Err("self-test did not preserve group20 commit through status77 fallback".into());
    }
    println!(
        "self_test_result=pass post_request_state=authenticating timer=refreshed status77=accepted fallback_group=19 identities=distinct missing_completion=blocked teardown=contained"
    );
    Ok(())
}

fn run() -> Result<(), String> {
    let operation_argument = env::args().nth(1);
    #[cfg(feature = "fuchsia-passive")]
    if operation_argument.as_deref() == Some("--self-test-sae-committed-fallback") {
        return futures::executor::block_on(run_sae_committed_fallback_self_test());
    }
    let operation = match operation_argument.as_deref() {
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
        Some("--run-one-shot-passive-channel") => Operation::RunOneShotPassiveChannel1,
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
    let contained_passive_channel = if operation == Operation::RunOneShotPassiveChannel1 {
        let number = if operation_argument.as_deref() == Some("--run-one-shot-passive-channel") {
            env::args()
                .nth(2)
                .ok_or("--run-one-shot-passive-channel requires a channel")?
                .parse::<u8>()
                .map_err(|_| "invalid passive channel")?
        } else {
            1
        };
        let band = if (1..=14).contains(&number) {
            WlanBand::TwoGhz
        } else if matches!(
            number,
            36 | 40
                | 44
                | 48
                | 52
                | 56
                | 60
                | 64
                | 100
                | 104
                | 108
                | 112
                | 116
                | 120
                | 124
                | 128
                | 132
                | 136
                | 140
                | 144
                | 149
                | 153
                | 157
                | 161
                | 165
        ) {
            WlanBand::FiveGhz
        } else {
            return Err(format!("unsupported bounded passive channel {number}"));
        };
        Some(ChannelNumber { band, number })
    } else {
        None
    };
    #[cfg(feature = "fuchsia-passive")]
    if operation.uses_contained_transport_gate() {
        record_sae_stage(match operation {
            Operation::RunOneShotFirmware => "vfio_firmware_process_started",
            Operation::RunOneShotPassiveChannel1 => "vfio_passive_process_started",
            _ => "process_enter",
        });
    }
    let contained_firmware_images = if matches!(
        operation,
        Operation::RunOneShotFirmware | Operation::RunOneShotPassiveChannel1
    ) {
        let patch = decompress_patch()?;
        let ram = decompress_ram()?;
        Patch::parse(&patch).map_err(|error| format!("parse verified patch: {error:?}"))?;
        Firmware::parse(&ram).map_err(|error| format!("parse verified RAM: {error:?}"))?;
        record_sae_stage(if operation == Operation::RunOneShotPassiveChannel1 {
            "vfio_passive_artifacts_ready"
        } else {
            "vfio_firmware_artifacts_ready"
        });
        Some((patch, ram))
    } else {
        None
    };
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
        let channel = env::var("DRV_SAE_CHANNEL")
            .map_err(|_| "DRV_SAE_CHANNEL is required for power setup")?
            .parse::<u8>()
            .map_err(|_| "DRV_SAE_CHANNEL is invalid")?;
        if !matches!(
            channel,
            36 | 40
                | 44
                | 48
                | 52
                | 56
                | 60
                | 64
                | 100
                | 104
                | 108
                | 112
                | 116
                | 120
                | 124
                | 128
                | 132
                | 136
                | 140
                | 144
                | 149
                | 153
                | 157
                | 161
                | 165
        ) {
            return Err("DRV_SAE_CHANNEL is unsupported".into());
        }
        let client = parse_client_mac(
            &env::var("DRV_SAE_CLIENT_MAC")
                .map_err(|_| "DRV_SAE_CLIENT_MAC is required for power setup")?,
        )?;
        verify_no_usable_mt792x_acpi_sar()?;
        Some((bssid, ssid, channel, client))
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
    if operation.records_active_transport_stages() {
        record_sae_stage("watchdog_verified");
    }
    let containment = operation
        .is_active_mcu()
        .then(|| ContainmentLedger::acquire(watchdog))
        .transpose()?;

    if operation.records_active_transport_stages() {
        record_sae_stage("vfio_cdev_open_before");
    }
    let device = Arc::new(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&vfio)
            .map_err(|error| format!("open {vfio}: {error}"))?,
    );
    if operation.records_active_transport_stages() {
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
    if operation.records_active_transport_stages() {
        record_sae_stage("iommufd_open_after");
    }
    let mut capsule = ActiveVfioCapsule::new(device, iommu, containment);
    // Advisory preflight facts are re-read with the complete resource owner
    // installed, before the first stateful VFIO operation is attempted.
    if operation.records_active_transport_stages() {
        record_sae_stage("second_pci_identity_before");
    }
    verify_pci_identity(&bdf)?;
    if operation.records_active_transport_stages() {
        record_sae_stage("second_pci_identity_after");
    }
    if !operation.uses_contained_transport_gate() {
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
        if operation.records_active_transport_stages() {
            record_sae_stage("vfio_bind_iommufd_before");
        }
        ioctl_mut(
            capsule.device.as_raw_fd(),
            VFIO_DEVICE_BIND_IOMMUFD,
            &mut bind,
            "bind iommufd",
        )?;
        if operation.records_active_transport_stages() {
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
        if operation.records_active_transport_stages() {
            record_sae_stage("ioas_allocate_before");
        }
        ioctl_mut(
            capsule.iommu.as_raw_fd(),
            IOMMU_IOAS_ALLOC,
            &mut alloc,
            "allocate IOAS",
        )?;
        if operation.records_active_transport_stages() {
            record_sae_stage("ioas_allocate_after");
        }
        capsule.ioas = Some(Ioas::from_allocated(&capsule.iommu, alloc.out_ioas_id));
        capsule.acquisition.record(AcquisitionIntent::AttachIoas)?;
        let mut attach = Attach {
            argsz: size::<Attach>(),
            pt_id: capsule.ioas.as_ref().expect("IOAS acquired").id(),
            ..Default::default()
        };
        if let Some(ledger) = capsule.containment.as_mut() {
            ledger.mark_possibly_active(Hazard::IoasAttached);
        }
        if operation.records_active_transport_stages() {
            record_sae_stage("vfio_attach_iommufd_pt_before");
        }
        ioctl_mut(
            capsule.device.as_raw_fd(),
            VFIO_DEVICE_ATTACH_IOMMUFD_PT,
            &mut attach,
            "attach IOAS",
        )?;
        capsule.ioas_attached = true;
        if operation.records_active_transport_stages() {
            record_sae_stage("vfio_attach_iommufd_pt_after");
            verify_pci_dma_disabled(&bdf).map_err(|error| {
                format!("vfio_attached_d0_preflight_not_ready; refusing BAR query: {error}")
            })?;
            record_sae_stage("vfio_attached_d0_preflight_already_ready");
        }

        let active_device_info = if operation.records_active_transport_stages() {
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
                device_info.argsz, device_info.flags, device_info.num_regions, device_info.num_irqs
            ));
            Some(device_info)
        } else {
            None
        };

        let active_bar0 = if let Some(device_info) = active_device_info.as_ref() {
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
            Some(bar0.ok_or("required BAR0 region was not discovered")?)
        } else {
            None
        };

        if operation.uses_contained_transport_gate() {
            let bar0 = active_bar0.expect("contained transport discovered BAR0");
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
                "vfio_bar0_mmap_before page={MT_HIF_REMAP_WINDOW_BAR_OFFSET:#x} length={} prot=read_write flags=shared region_size={} region_offset={}",
                PAGE, bar0.size, bar0.offset
            ));
            let mut window = match ReadPage::map(
                &capsule.device,
                &bar0,
                MT_HIF_REMAP_WINDOW_BAR_OFFSET,
                true,
            ) {
                Ok(page) => page,
                Err(error) => {
                    record_sae_stage(&format!(
                        "vfio_bar0_mmap_error page={MT_HIF_REMAP_WINDOW_BAR_OFFSET:#x} error={error}"
                    ));
                    return Err(error);
                }
            };
            record_sae_stage(&format!(
                "vfio_bar0_mmap_after page={MT_HIF_REMAP_WINDOW_BAR_OFFSET:#x} length=4096"
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
            let selected = (saved_selector & !0xffff) | 0x7001;
            let identity = (|| -> Result<(u32, u32, u32, u32, u32), String> {
                record_sae_stage(&format!(
                    "vfio_remap_selector_select_write_before offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} saved={saved_selector:#010x} value={selected:#010x} base=0x7001"
                ));
                if let Err(error) = page.write_remap_selector(selected) {
                    record_sae_stage(&format!(
                        "vfio_remap_selector_select_write_error offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} error={error}"
                    ));
                    return Err(error);
                }
                record_sae_stage(&format!(
                    "vfio_remap_selector_select_write_after offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} value={selected:#010x}"
                ));
                record_sae_stage(&format!(
                    "vfio_remap_selector_select_verify_before offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x}"
                ));
                let verified = page.read(MT_HIF_REMAP_L1_BAR_OFFSET)?;
                record_sae_stage(&format!(
                    "vfio_remap_selector_select_verify_after offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} value={verified:#010x} base={:#06x}",
                    verified & 0xffff
                ));
                if verified & 0xffff != 0x7001 {
                    return Err(format!(
                        "L1 selector did not retain 0x7001: {verified:#010x}"
                    ));
                }

                let chip_offset = MT_HIF_REMAP_WINDOW_BAR_OFFSET + 0x0200;
                record_sae_stage(&format!(
                    "vfio_dynamic_identity_read_before name=chip_id physical=0x70010200 bar_offset={chip_offset:#x}"
                ));
                let chip_id = match window.read(chip_offset) {
                    Ok(value) => value,
                    Err(error) => {
                        record_sae_stage(&format!(
                            "vfio_dynamic_identity_read_error name=chip_id physical=0x70010200 bar_offset={chip_offset:#x} error={error}"
                        ));
                        return Err(error);
                    }
                };
                record_sae_stage(&format!(
                    "vfio_dynamic_identity_read_after name=chip_id physical=0x70010200 bar_offset={chip_offset:#x} value={chip_id:#010x}"
                ));
                if chip_id == u32::MAX {
                    return Err("MT_HW_CHIPID returned all ones".into());
                }
                if chip_id != 0x7961 {
                    return Err(format!(
                        "MT_HW_CHIPID is {chip_id:#010x}, expected 0x00007961"
                    ));
                }

                let bound_offset = MT_HIF_REMAP_WINDOW_BAR_OFFSET + 0x0020;
                record_sae_stage(&format!(
                    "vfio_dynamic_identity_read_before name=hardware_bound physical=0x70010020 bar_offset={bound_offset:#x}"
                ));
                let hardware_bound = match window.read(bound_offset) {
                    Ok(value) => value,
                    Err(error) => {
                        record_sae_stage(&format!(
                            "vfio_dynamic_identity_read_error name=hardware_bound physical=0x70010020 bar_offset={bound_offset:#x} error={error}"
                        ));
                        return Err(error);
                    }
                };
                record_sae_stage(&format!(
                    "vfio_dynamic_identity_read_after name=hardware_bound physical=0x70010020 bar_offset={bound_offset:#x} value={hardware_bound:#010x}"
                ));
                if hardware_bound == u32::MAX {
                    return Err("MT_HW_BOUND returned all ones".into());
                }

                let revision_offset = MT_HIF_REMAP_WINDOW_BAR_OFFSET + 0x0204;
                record_sae_stage(&format!(
                    "vfio_dynamic_identity_read_before name=revision physical=0x70010204 bar_offset={revision_offset:#x}"
                ));
                let revision = match window.read(revision_offset) {
                    Ok(value) => value,
                    Err(error) => {
                        record_sae_stage(&format!(
                            "vfio_dynamic_identity_read_error name=revision physical=0x70010204 bar_offset={revision_offset:#x} error={error}"
                        ));
                        return Err(error);
                    }
                };
                record_sae_stage(&format!(
                    "vfio_dynamic_identity_read_after name=revision physical=0x70010204 bar_offset={revision_offset:#x} value={revision:#010x}"
                ));
                if revision == u32::MAX {
                    return Err("MT_HW_REV returned all ones".into());
                }
                let effective_chip_id = if hardware_bound & (1 << 7) != 0 {
                    0x7920
                } else {
                    chip_id
                };
                let composite_revision = (effective_chip_id << 16) | (revision & 0xff);
                Ok((
                    chip_id,
                    hardware_bound,
                    revision,
                    effective_chip_id,
                    composite_revision,
                ))
            })();

            record_sae_stage(&format!(
                "vfio_remap_selector_restore_write_before offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} value={saved_selector:#010x}"
            ));
            let restore_write = page.write_remap_selector(saved_selector);
            match &restore_write {
                Ok(()) => record_sae_stage(&format!(
                    "vfio_remap_selector_restore_write_after offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} value={saved_selector:#010x}"
                )),
                Err(error) => record_sae_stage(&format!(
                    "vfio_remap_selector_restore_write_error offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} error={error}"
                )),
            }
            restore_write?;
            record_sae_stage(&format!(
                "vfio_remap_selector_restore_verify_before offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x}"
            ));
            let restored = page.read(MT_HIF_REMAP_L1_BAR_OFFSET)?;
            record_sae_stage(&format!(
                "vfio_remap_selector_restore_verify_after offset={MT_HIF_REMAP_L1_BAR_OFFSET:#x} value={restored:#010x} expected={saved_selector:#010x} equal={}",
                restored == saved_selector
            ));
            if restored != saved_selector {
                return Err(format!(
                    "L1 selector restore mismatch: saved={saved_selector:#010x} restored={restored:#010x}"
                ));
            }
            let (chip_id, hardware_bound, revision, effective_chip_id, composite_revision) =
                identity?;
            record_sae_stage(&format!(
                "vfio_dynamic_identity_complete chip_id={chip_id:#010x} hardware_bound={hardware_bound:#010x} revision={revision:#010x} effective_chip_id={effective_chip_id:#010x} composite_revision={composite_revision:#010x}"
            ));

            record_sae_stage("vfio_post_identity_pci_preflight_before config_bytes=256");
            let config_path = format!("/sys/bus/pci/devices/{bdf}/config");
            let mut config_file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&config_path)
                .map_err(|error| format!("open post-identity PCI config: {error}"))?;
            let mut config = [0u8; 256];
            config_file
                .read_exact(&mut config)
                .map_err(|error| format!("read post-identity PCI config: {error}"))?;
            let command = u16::from_le_bytes(config[4..6].try_into().expect("fixed field"));
            let mut capability = usize::from(config[0x34] & !3);
            let mut power = None;
            for _ in 0..48 {
                if capability < 0x40 || capability + 6 > config.len() {
                    break;
                }
                if config[capability] == 1 {
                    let pmcsr = u16::from_le_bytes(
                        config[capability + 4..capability + 6]
                            .try_into()
                            .expect("fixed field"),
                    );
                    power = Some((capability, pmcsr));
                    break;
                }
                capability = usize::from(config[capability + 1] & !3);
            }
            let (pm_capability_offset, pmcsr) =
                power.ok_or("post-identity PCI PM capability is absent")?;
            let mse = command & (1 << 1) != 0;
            let bme = command & (1 << 2) != 0;
            let power_state = pmcsr & 3;
            record_sae_stage(&format!(
                "vfio_post_identity_pci_preflight_after command={command:#06x} pm_capability_offset={pm_capability_offset:#04x} pmcsr={pmcsr:#06x} mse={mse} bme={bme} power_state={power_state}"
            ));
            if !mse || bme {
                return Err(format!(
                    "post-identity PCI command requires MSE=1 BME=0, read {command:#06x}"
                ));
            }
            if power_state != 0 {
                return Err(format!(
                    "post-identity PCI device is not in D0: PMCSR {pmcsr:#06x}"
                ));
            }
            let device_path = std::fs::canonicalize(format!("/sys/bus/pci/devices/{bdf}"))
                .map_err(|error| format!("resolve PCI device path: {error}"))?;
            let parent_path = device_path
                .parent()
                .ok_or("PCI endpoint has no parent bridge")?;
            let parent_bdf = parent_path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("PCI parent bridge path is invalid")?;
            let mut parent_config = [0u8; 256];
            File::open(parent_path.join("config"))
                .and_then(|mut file| file.read_exact(&mut parent_config))
                .map_err(|error| format!("read parent PCI config {parent_bdf}: {error}"))?;
            let aspm_supported = mt76_pci_aspm_supported(&config, Some(&parent_config))
                .map_err(|error| format!("parse PCIe Link Control: {error:?}"))?;
            record_sae_stage(&format!(
                "vfio_ownership_aspm_predicate endpoint={bdf} parent={parent_bdf} supported={aspm_supported}"
            ));

            let selected_command = command | 0x0400;
            let intx_disable = (|| -> Result<(), String> {
                record_sae_stage(&format!(
                    "vfio_pci_intx_disable_write_before offset=0x04 bytes=2 saved={command:#06x} value={selected_command:#06x}"
                ));
                config_file
                    .seek(SeekFrom::Start(4))
                    .and_then(|_| config_file.write_all(&selected_command.to_le_bytes()))
                    .map_err(|error| format!("write PCI INTx disable: {error}"))?;
                record_sae_stage(&format!(
                    "vfio_pci_intx_disable_write_after offset=0x04 bytes=2 value={selected_command:#06x}"
                ));
                record_sae_stage("vfio_pci_intx_disable_verify_before offset=0x04 bytes=2");
                let mut raw = [0u8; 2];
                config_file
                    .seek(SeekFrom::Start(4))
                    .and_then(|_| config_file.read_exact(&mut raw))
                    .map_err(|error| format!("read PCI INTx disable: {error}"))?;
                let readback = u16::from_le_bytes(raw);
                record_sae_stage(&format!(
                    "vfio_pci_intx_disable_verify_after offset=0x04 bytes=2 value={readback:#06x} expected={selected_command:#06x} equal={}",
                    readback == selected_command
                ));
                if readback != selected_command {
                    return Err(format!(
                        "PCI INTx disable mismatch: expected {selected_command:#06x}, read {readback:#06x}"
                    ));
                }

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
                    record_sae_stage(&format!(
                        "vfio_pci_irq_info_query_before index={index} kind={kind:?} argsz={}",
                        irq.argsz
                    ));
                    if let Err(error) = ioctl_mut(
                        capsule.device.as_raw_fd(),
                        VFIO_DEVICE_GET_IRQ_INFO,
                        &mut irq,
                        "query post-identity VFIO IRQ",
                    ) {
                        record_sae_stage(&format!(
                            "vfio_pci_irq_info_query_error index={index} kind={kind:?} argsz={} error={error}",
                            irq.argsz
                        ));
                        return Err(error);
                    }
                    record_sae_stage(&format!(
                        "vfio_pci_irq_info_query_after index={index} kind={kind:?} argsz={} flags={:#010x} count={}",
                        irq.argsz, irq.flags, irq.count
                    ));
                    capabilities.push(PciIrqCapability {
                        kind,
                        count: irq.count,
                        eventfd: irq.flags & 1 != 0,
                    });
                }
                let selected_irq = select_vfio_irq(&capabilities)
                    .ok_or("VFIO exposes no eventfd-capable PCI interrupt")?;
                if selected_irq.kind == PciIrqKind::Intx {
                    return Err("active MCU preflight selected only level INTx".into());
                }
                record_sae_stage(&format!(
                    "vfio_pci_irq_selection_complete kind={:?} count={} eventfd={}",
                    selected_irq.kind, selected_irq.count, selected_irq.eventfd
                ));

                let mut query_info = DeviceInfo {
                    argsz: size::<DeviceInfo>(),
                    ..Default::default()
                };
                record_sae_stage(&format!(
                    "vfio_reset_capability_query_before argsz={}",
                    query_info.argsz
                ));
                if let Err(error) = ioctl_mut(
                    capsule.device.as_raw_fd(),
                    VFIO_DEVICE_GET_INFO,
                    &mut query_info,
                    "query post-identity VFIO device info",
                ) {
                    record_sae_stage(&format!(
                        "vfio_reset_capability_query_error argsz={} error={error}",
                        query_info.argsz
                    ));
                    return Err(error);
                }
                record_sae_stage(&format!(
                    "vfio_reset_capability_query_after argsz={} flags={:#010x} num_regions={} num_irqs={} cap_offset={}",
                    query_info.argsz,
                    query_info.flags,
                    query_info.num_regions,
                    query_info.num_irqs,
                    query_info.cap_offset
                ));
                if query_info.flags & 0x3 != 0x3 {
                    return Err(format!(
                        "VFIO device requires RESET|PCI flags, read {:#010x}",
                        query_info.flags
                    ));
                }

                record_sae_stage(&format!(
                    "vfio_bar0_mmap_before page=0x10000 length={} prot=read_write flags=shared region_size={} region_offset={}",
                    PAGE, bar0.size, bar0.offset
                ));
                let mut pcie_mac_page = match ReadPage::map(&capsule.device, &bar0, 0x10000, true) {
                    Ok(page) => page,
                    Err(error) => {
                        record_sae_stage(&format!(
                            "vfio_bar0_mmap_error page=0x10000 error={error}"
                        ));
                        return Err(error);
                    }
                };
                record_sae_stage("vfio_bar0_mmap_after page=0x10000 length=4096");
                record_sae_stage("vfio_pcie_mac_int_enable_read_before offset=0x10188 bytes=4");
                let saved_mac_interrupt_enable = pcie_mac_page.read(0x10188)?;
                record_sae_stage(&format!(
                    "vfio_pcie_mac_int_enable_saved offset=0x10188 bytes=4 value={saved_mac_interrupt_enable:#010x}"
                ));
                if saved_mac_interrupt_enable == u32::MAX {
                    return Err("MT_PCIE_MAC_INT_ENABLE returned all ones".into());
                }
                let disable = (|| -> Result<(), String> {
                    record_sae_stage(
                        "vfio_pcie_mac_int_enable_zero_write_before offset=0x10188 bytes=4 value=0x00000000",
                    );
                    if let Err(error) = pcie_mac_page.write_pcie_mac_interrupt_enable_zero() {
                        record_sae_stage(&format!(
                            "vfio_pcie_mac_int_enable_zero_write_error offset=0x10188 bytes=4 error={error}"
                        ));
                        return Err(error);
                    }
                    record_sae_stage(
                        "vfio_pcie_mac_int_enable_zero_write_after offset=0x10188 bytes=4 value=0x00000000",
                    );
                    record_sae_stage(
                        "vfio_pcie_mac_int_enable_zero_verify_before offset=0x10188 bytes=4",
                    );
                    let zero_readback = pcie_mac_page.read(0x10188)?;
                    record_sae_stage(&format!(
                        "vfio_pcie_mac_int_enable_zero_verify_after offset=0x10188 bytes=4 value={zero_readback:#010x} expected=0x00000000 equal={}",
                        zero_readback == 0
                    ));
                    if zero_readback != 0 {
                        return Err(format!(
                            "MT_PCIE_MAC_INT_ENABLE zero readback is {zero_readback:#010x}"
                        ));
                    }
                    Ok(())
                })();

                let ownership = disable.as_ref().map_or(Ok(()), |_| {
                    (|| -> Result<(), String> {
                        record_sae_stage(
                            "vfio_ownership_round_trip_begin page=0xe0000 offset=0xe0010",
                        );
                        let mut conn_page = ReadPage::map(&capsule.device, &bar0, 0xe0000, true)?;
                        let result = {
                            let mut transport = VfioOwnership {
                                page: &conn_page,
                                start: Instant::now(),
                            };
                            round_trip_driver_ownership(
                                &mut transport,
                                aspm_supported,
                                record_ownership_round_trip_stage,
                            )
                            .map_err(|error| format!("ownership round trip: {error:?}"))
                        };
                        record_sae_stage("vfio_ownership_bar0_munmap_before page=0xe0000");
                        let unmap = conn_page.teardown();
                        match &unmap {
                            Ok(()) => {
                                record_sae_stage("vfio_ownership_bar0_munmap_after page=0xe0000")
                            }
                            Err(error) => record_sae_stage(&format!(
                                "vfio_ownership_bar0_munmap_error page=0xe0000 error={error}"
                            )),
                        }
                        unmap?;
                        result?;
                        record_sae_stage("vfio_ownership_round_trip_passed");
                        Ok(())
                    })()
                });

                let irq_reset = ownership.as_ref().map_or(Ok(()), |_| {
                    (|| -> Result<(), String> {
                        record_sae_stage("vfio_irq_reset_boundary_begin");
                        let mut wfdma_page = ReadPage::map(&capsule.device, &bar0, 0xd4000, true)?;
                        let result = {
                            let ledger = capsule
                                .containment
                                .as_mut()
                                .expect("guarded gate has containment ledger");
                            for hazard in [
                                Hazard::HostControl,
                                Hazard::DeviceIrq,
                                Hazard::Wfdma,
                                Hazard::LabMutated,
                            ] {
                                ledger.mark_possibly_active(hazard);
                            }
                            let mut transport = VfioIrqResetBoundary {
                                wfsys: VfioWfsysReset {
                                    selector: &page,
                                    window: &window,
                                    start: Instant::now(),
                                    saved: saved_selector,
                                },
                                device: &capsule.device,
                                wfdma: &wfdma_page,
                                pcie_mac: &pcie_mac_page,
                                irq: None,
                                selected: selected_irq,
                                bdf: &bdf,
                                ledger,
                            };
                            exercise_irq_reset_boundary(
                                &mut transport,
                                selected_irq,
                                record_irq_reset_stage,
                            )
                        };
                        if result.as_ref().is_err_and(|error| {
                            error
                                .cleanup
                                .iter()
                                .any(|(step, _)| *step == IrqResetCleanupStep::VerifyContained)
                        }) {
                            record_sae_stage("vfio_irq_reset_boundary_unsafe_retaining_resources");
                            park_retention_capsule_ref(&mut capsule);
                        }
                        let dma_resources = result.as_ref().map_or(Ok(()), |_| {
                            run_contained_dma_resource_round_trip(
                                &mut capsule,
                                &bar0,
                                &bdf,
                                operation,
                                &wfdma_page,
                                &pcie_mac_page,
                                selected_irq,
                                contained_firmware_images
                                    .as_ref()
                                    .map(|(patch, ram)| (patch.as_slice(), ram.as_slice())),
                                contained_passive_channel,
                            )
                        });
                        record_sae_stage("vfio_irq_reset_wfdma_munmap_before page=0xd4000");
                        wfdma_page.teardown()?;
                        record_sae_stage("vfio_irq_reset_wfdma_munmap_after page=0xd4000");
                        result.map_err(|error| format!("IRQ/reset boundary: {error:?}"))?;
                        dma_resources?;
                        record_sae_stage("vfio_irq_reset_boundary_passed_contained");
                        Ok(())
                    })()
                });

                let restore = if ownership.is_err() {
                    record_sae_stage(&format!(
                        "vfio_pcie_mac_int_enable_restore_write_before offset=0x10188 bytes=4 value={saved_mac_interrupt_enable:#010x}"
                    ));
                    (|| -> Result<Option<u32>, String> {
                        if let Err(error) = pcie_mac_page
                            .restore_pcie_mac_interrupt_enable(saved_mac_interrupt_enable)
                        {
                            record_sae_stage(&format!(
                                "vfio_pcie_mac_int_enable_restore_write_error offset=0x10188 bytes=4 error={error}"
                            ));
                            return Err(error);
                        }
                        record_sae_stage(&format!(
                            "vfio_pcie_mac_int_enable_restore_write_after offset=0x10188 bytes=4 value={saved_mac_interrupt_enable:#010x}"
                        ));
                        record_sae_stage(
                            "vfio_pcie_mac_int_enable_restore_verify_before offset=0x10188 bytes=4",
                        );
                        let restored = pcie_mac_page.read(0x10188)?;
                        record_sae_stage(&format!(
                            "vfio_pcie_mac_int_enable_restore_verify_after offset=0x10188 bytes=4 value={restored:#010x} expected={saved_mac_interrupt_enable:#010x} equal={}",
                            restored == saved_mac_interrupt_enable
                        ));
                        if restored != saved_mac_interrupt_enable {
                            return Err(format!(
                                "MT_PCIE_MAC_INT_ENABLE restore mismatch: saved={saved_mac_interrupt_enable:#010x} restored={restored:#010x}"
                            ));
                        }
                        Ok(Some(restored))
                    })()
                } else {
                    Ok(None)
                };
                record_sae_stage("vfio_bar0_munmap_before page=0x10000 length=4096");
                let unmap = pcie_mac_page.teardown();
                match &unmap {
                    Ok(()) => record_sae_stage("vfio_bar0_munmap_after page=0x10000 length=4096"),
                    Err(error) => record_sae_stage(&format!(
                        "vfio_bar0_munmap_error page=0x10000 error={error}"
                    )),
                }
                unmap?;
                let restored_mac_interrupt_enable = restore?;
                disable?;
                ownership?;
                irq_reset?;
                record_sae_stage(&format!(
                    "vfio_irq_reset_cleanup_complete saved_mac={saved_mac_interrupt_enable:#010x} restored_on_pre_reset_error={restored_mac_interrupt_enable:?} safe_mac=0x00000000"
                ));
                Ok(())
            })();

            record_sae_stage(&format!(
                "vfio_pci_command_restore_write_before offset=0x04 bytes=2 value={command:#06x}"
            ));
            let restore_write = config_file
                .seek(SeekFrom::Start(4))
                .and_then(|_| config_file.write_all(&command.to_le_bytes()))
                .map_err(|error| format!("restore PCI Command: {error}"));
            match &restore_write {
                Ok(()) => record_sae_stage(&format!(
                    "vfio_pci_command_restore_write_after offset=0x04 bytes=2 value={command:#06x}"
                )),
                Err(error) => record_sae_stage(&format!(
                    "vfio_pci_command_restore_write_error offset=0x04 bytes=2 error={error}"
                )),
            }
            restore_write?;
            record_sae_stage("vfio_pci_command_restore_verify_before offset=0x04 bytes=2");
            let mut restored_raw = [0u8; 2];
            config_file
                .seek(SeekFrom::Start(4))
                .and_then(|_| config_file.read_exact(&mut restored_raw))
                .map_err(|error| format!("verify restored PCI Command: {error}"))?;
            let restored_command = u16::from_le_bytes(restored_raw);
            record_sae_stage(&format!(
                "vfio_pci_command_restore_verify_after offset=0x04 bytes=2 value={restored_command:#06x} expected={command:#06x} equal={}",
                restored_command == command
            ));
            if restored_command != command {
                return Err(format!(
                    "PCI Command restore mismatch: saved={command:#06x} restored={restored_command:#06x}"
                ));
            }
            intx_disable?;
            record_sae_stage(&format!(
                "vfio_pci_intx_disable_complete selected={selected_command:#06x} restored={restored_command:#06x}"
            ));

            record_sae_stage(&format!(
                "vfio_bar0_munmap_before page={MT_HIF_REMAP_WINDOW_BAR_OFFSET:#x} length=4096"
            ));
            window.teardown()?;
            record_sae_stage(&format!(
                "vfio_bar0_munmap_after page={MT_HIF_REMAP_WINDOW_BAR_OFFSET:#x} length=4096"
            ));
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

        let info = if let Some(info) = active_bar0 {
            info
        } else {
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
            info
        };

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
            record_sae_stage(&format!(
                "vfio_region_discovery_release_error errors={release_errors:?}"
            ));
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
            let mut irq = install_vfio_irq(&device, selected)?;
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
        let mut arena = DmaArena::map(&iommu, ioas.id(), 0x0100_0000)?;
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
        let mut ring = DmaArena::map(&iommu, ioas.id(), 0x0100_0000)?;
        let mut payload = DmaArena::map(&iommu, ioas.id(), 0x0100_1000)?;
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
        let mut guard = DmaArena::map(&iommu, ioas.id(), 0x0100_0000)?;
        let mut fwdl = DmaArena::map(&iommu, ioas.id(), 0x0100_1000)?;
        let mut mcu = DmaArena::map(&iommu, ioas.id(), 0x0100_2000)?;
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
            capsule.ioas.as_ref().expect("IOAS acquired").id(),
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
        let mgmt_txwi = mgmt_txwi;
        #[cfg(feature = "fuchsia-passive")]
        let mgmt_frame = mgmt_frame;
        #[cfg(feature = "fuchsia-passive")]
        let mgmt_tx_ring = mgmt_tx_ring;
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
                wfdma.write_rx_ring_slot(2, data_rx_ring.iova as u32, 8, 7, 0)?;
                wfdma.write_rx_ring_slot(4, mcu_wa_rx_ring.iova as u32, 8, 7, 0)?;
            }
            capsule
                .containment
                .as_mut()
                .expect("active MCU operation has containment ledger")
                .mark_possibly_active(Hazard::DeviceIrq);
            acquisition_ledger.record(AcquisitionIntent::InstallIrq)?;
            *irq = Some(install_vfio_irq(&device, selected)?);
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
            wfdma.write_active_wfdma(0xd4688, 0x0040_0004)?;
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
            if operation == Operation::RunOneShotFirmware {
                println!(
                    "{{\"firmware_bootstrap_event\":\"transport_ready\",\"bme\":true,\"wfdma_global\":\"{global:#010x}\",\"irq_mask\":\"{response_irq_mask:#010x}\",\"rings\":[\"fwdl_tx\",\"mcu_tx\",\"wm_rx\",\"wm2_rx\"]}}"
                );
                std::io::stdout().flush().map_err(|error| {
                    format!("flush firmware transport-ready milestone: {error}")
                })?;
            }
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
                    normal_rx_frames: VecDeque::new(),
                    tx_completions: Vec::new(),
                    descriptor_provenance: DescriptorProvenance::new(),
                };
                let mut loader = VfioFirmwareLoader {
                    mcu,
                    conn: &conn,
                    pcie_mac,
                    bdf: &bdf,
                    fwdl_ring: &mut *fwdl_ring,
                    fwdl_payload: &mut *fwdl_payload,
                    sequence: 0,
                    command_index: 0,
                    uni_terminal_poisoned: false,
                    #[cfg(feature = "fuchsia-passive")]
                    client_interface: None,
                    fwdl_index: 0,
                    pending_scatter: None,
                    start: Instant::now(),
                };
                let patch = Patch::parse(patch_bytes)
                    .map_err(|error| format!("parse patch for loader: {error:?}"))?;
                let firmware = Firmware::parse(ram_bytes)
                    .map_err(|error| format!("parse RAM for loader: {error:?}"))?;
                if operation == Operation::RunOneShotFirmware {
                    println!(
                        r#"{{"firmware_bootstrap_event":"begin","patch_version":"{:#010x}","patch_build":"{}","ram_version":"{}","ram_build":"{}","downloadable_regions":{}}}"#,
                        patch.header.patch_version,
                        String::from_utf8_lossy(patch.header.build_date).trim_end_matches('\0'),
                        String::from_utf8_lossy(firmware.trailer.firmware_version)
                            .trim_end_matches('\0'),
                        String::from_utf8_lossy(firmware.trailer.build_date).trim_end_matches('\0'),
                        firmware
                            .regions()
                            .filter(|region| region.is_downloadable())
                            .count(),
                    );
                    std::io::stdout()
                        .flush()
                        .map_err(|error| format!("flush firmware bootstrap begin: {error}"))?;
                }
                #[cfg(feature = "fuchsia-passive")]
                let result = if operation == Operation::RunOneShotFirmware {
                    load_mt7921_firmware_bootstrap(&mut loader, patch, firmware)
                } else if operation == Operation::RunOneShotPassivePrepare {
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
                                mgmt_tx_outstanding: MgmtTxOutstanding::default(),
                                mgmt_txwi,
                                mgmt_frame,
                                mgmt_tx_ring,
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
                                mgmt_tx_outstanding: MgmtTxOutstanding::default(),
                                mgmt_txwi,
                                mgmt_frame,
                                mgmt_tx_ring,
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
                                    let channel = power_target.as_ref().expect("power target").2;
                                    channels_for(WlanBand::FiveGhz, &[channel])
                                }
                                Operation::RunOneShotSaeAuth => {
                                    let channel = power_target.as_ref().expect("SAE target").2;
                                    channels_for(WlanBand::FiveGhz, &[channel])
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
                                power_target.as_ref().map(|(bssid, ssid, _, _)| {
                                    BeaconHintAuthorizer::new(*bssid, ssid.clone())
                                });
                            let mut beacon_authorization = None;
                            let mut target_bss = None;
                            let mut target_selection = None;
                            let mut total_observations = 0usize;
                            for channel in &channels {
                                adapter
                                    .set_channel(set_channel_request(
                                        *channel,
                                        ChannelBandwidth::Cbw20,
                                        None,
                                    ))
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
                                                    target_selection = Some(
                                                        retain_client_selection(
                                                            scan_id,
                                                            u64::try_from(
                                                                total_observations + observations,
                                                            )
                                                            .map_err(|_| {
                                                                "selector observation generation overflow"
                                                            })?,
                                                            &observation,
                                                        )
                                                        .map_err(|status| status.to_string())?,
                                                    );
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
                                    .ok_or("target beacon did not authorize current channel")?;
                                let beacon_authorizer = beacon_authorizer
                                    .as_ref()
                                    .expect("power setup created beacon authorizer");
                                if !beacon_authorizer.permits(beacon_authorization) {
                                    return Err(
                                        "target beacon authorization is no longer live".into()
                                    );
                                }
                                if operation == Operation::RunOneShotSaeAuth {
                                    let selection = target_selection
                                        .take()
                                        .ok_or("target selection evidence was not retained")?;
                                    let shared = Arc::new(Mutex::new(LiveClientState {
                                        selection,
                                        ..Default::default()
                                    }));
                                    let target_rcpi = target_bss
                                        .as_ref()
                                        .map(|bss| {
                                            ((i16::from(bss.rssi_dbm) + 110) * 2).clamp(0, 220)
                                                as u8
                                        })
                                        .ok_or("target BSS was not retained")?;
                                    let effects = LiveClientEffects {
                                        state: shared.clone(),
                                        target: power_target.as_ref().expect("SAE target").0,
                                        client: power_target
                                            .as_ref()
                                            .expect("SAE target")
                                            .3
                                            .bytes(),
                                        rcpi: target_rcpi,
                                        firmware: ClientFirmwareEffectsState::default(),
                                        post_association_data_wait: None,
                                        eapol_start_deadline: None,
                                        eapol_start_emitted: false,
                                        // Peer/key WCID state remains association-owned. The
                                        // first-VIF OMAC/BSS/WCID context is installed below.
                                    };
                                    let mut query =
                                        query_from_capabilities(report.nic_capability, &candidates);
                                    query.sta_addr =
                                        Some(power_target.as_ref().expect("SAE target").3.bytes());
                                    let support = live_client_support(query);
                                    let device_info = wlan_mlme::mlme_device_info_from_softmac(
                                        support.query.clone(),
                                    )
                                    .map_err(|_| "convert SoftMAC device info failed")?;
                                    let security_support = support.security.clone();
                                    let spectrum_support = support.spectrum_management.clone();
                                    let (mut device, runner, ethernet_device, ethernet_tx) =
                                        Mt7921ClientDevice::new_with_ethernet(
                                            effects, adapter, support, 32,
                                        )
                                        .map_err(
                                            |error| {
                                                format!(
                                                    "construct pinned Ethernet boundary: {error}"
                                                )
                                            },
                                        )?;
                                    let bss =
                                        target_bss.as_ref().ok_or("target BSS was not retained")?;
                                    // Linux establishes the complete chandef before
                                    // rate-power and association work. Do the one
                                    // source-exact width transition while the passive
                                    // phase still permits CHANNEL_SWITCH; ClientMlme's
                                    // later replay must resolve to this same context.
                                    futures::executor::block_on(DeviceOps::set_channel(
                                        &mut device,
                                        bss.primary,
                                        bss.bandwidth,
                                        bss.vht_secondary_80_channel,
                                    ))
                                    .map_err(|status| {
                                        format!("DeviceOps target channel context failed: {status}")
                                    })?;
                                    runner.with_physical(|adapter| {
                                        adapter.with_transport_mut(|transport| {
                                            let mechanics = transport.mechanics_mut();
                                            mechanics.ledger.transition(
                                                RunPhase::PassiveReady,
                                                RunPhase::BeaconAuthorized,
                                            )?;
                                            let client =
                                                power_target.as_ref().expect("SAE target").3;
                                            mechanics.loader.program_client_interface(client)?;
                                            record_sae_stage(
                                                "client_interface_programmed omac=0 bss=0 wcid=19 identity_match=true",
                                            );
                                            program_live_rate_power(
                                                mechanics,
                                                report.nic_capability,
                                            )?;
                                            acquire_sae_tx_resources(
                                                iommu,
                                                ioas.id(),
                                                mechanics.mgmt_txwi,
                                                mechanics.mgmt_frame,
                                                mechanics.mgmt_tx_ring,
                                                acquisition_ledger,
                                                mechanics.ledger,
                                            )
                                        })
                                    })?;
                                    record_sae_stage(
                                        "sae_tx_resources_acquired after_beacon=true after_rate_power=true",
                                    );
                                    {
                                        let mut state = shared.lock().unwrap();
                                        state.mark_rate_power_ready(
                                            bss.bssid,
                                            bss.primary,
                                            bss.bandwidth,
                                            bss.vht_secondary_80_channel,
                                        )?;
                                        state.authorize_sae(
                                            bss.bssid,
                                            bss.primary,
                                            bss.bandwidth,
                                            bss.vht_secondary_80_channel,
                                        )?;
                                    }
                                    let passphrase = sae_credential
                                        .take()
                                        .ok_or("SAE credential unavailable")?
                                        .into_passphrase();
                                    let request = fidl_sme::ConnectRequest {
                                        ssid: power_target.as_ref().expect("SAE target").1.clone(),
                                        bss_description: bss.clone(),
                                        multiple_bss_candidates: false,
                                        authentication: fidl_internal::Authentication {
                                            protocol: fidl_internal::Protocol::Wpa3Personal,
                                            credentials: Some(Box::new(
                                                fidl_internal::Credentials::Wpa(
                                                    fidl_internal::WpaCredentials::Passphrase(
                                                        passphrase,
                                                    ),
                                                ),
                                            )),
                                        },
                                        deprecated_scan_type: fidl_common::ScanType::Passive,
                                    };
                                    let mut sme_config = wlan_sme::client::ClientConfig::default();
                                    sme_config.wpa3_supported = true;
                                    let mut runtime =
                                        futures::executor::block_on(PinnedClientRuntime::new(
                                            device,
                                            runner,
                                            sme_config,
                                            device_info,
                                            security_support,
                                            spectrum_support,
                                            fuchsia_inspect::Inspector::default(),
                                        ))
                                        .map_err(|_| "construct pinned SME/MLME runtime failed")?;
                                    let deadline =
                                        Instant::now() + std::time::Duration::from_secs(25);
                                    futures::executor::block_on(runtime.connect(request, deadline))
                                        .map_err(|error| {
                                            format!("pinned SME/MLME connect failed: {error:?}")
                                        })?;
                                    record_sae_stage(
                                        "pinned_sme_connected association=true key_install=true controlled_port=true",
                                    );
                                    let mut proof = BoundedNetstackProof::new(
                                        ethernet_device,
                                        ethernet_tx,
                                        NetstackProofConfig {
                                            dns_name: "example.com.".into(),
                                            server_port: NonZeroU16::new(80).unwrap(),
                                            http_request: b"GET / HTTP/1.0\r\nHost: example.com\r\nConnection: close\r\n\r\n"
                                                .to_vec(),
                                            expected_response_prefix: b"HTTP/1.".to_vec(),
                                        },
                                    )
                                    .map_err(|error| format!("construct Netstack proof: {error}"))?;
                                    let mut pump = runtime.associated_data_pump();
                                    proof
                                        .prove_dhcp(&mut pump, deadline)
                                        .map_err(|error| format!("DHCP proof failed: {error}"))?;
                                    record_sae_stage("internet_proof_dhcp=true");
                                    proof
                                        .prove_dns(&mut pump, deadline)
                                        .map_err(|error| format!("DNS proof failed: {error}"))?;
                                    record_sae_stage("internet_proof_dns=true");
                                    proof
                                        .prove_tcp(&mut pump, deadline)
                                        .map_err(|error| format!("TCP proof failed: {error}"))?;
                                    record_sae_stage("internet_proof_tcp=true");
                                    proof
                                        .prove_http(&mut pump, deadline)
                                        .map_err(|error| format!("HTTP proof failed: {error}"))?;
                                    record_sae_stage("internet_proof_http=true");
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
                                    None,
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
                let result = if operation == Operation::RunOneShotFirmware {
                    load_mt7921_firmware_bootstrap(&mut loader, patch, firmware)
                } else if operation == Operation::RunOneShotChannelDomain {
                    load_mt7921_firmware_through_channel_domain(&mut loader, patch, firmware)
                } else {
                    load_mt7921_firmware(&mut loader, patch, firmware)
                };
                let report =
                    result.map_err(|error| format!("one-shot firmware loader: {error:?}"))?;
                if operation == Operation::RunOneShotFirmware {
                    println!(
                        r#"{{"firmware_bootstrap_event":"n9_ready_and_capability_response","download_ready":{},"patch":"{:?}","patch_sections":{},"ram_regions":{},"scatter_chunks":{},"scatter_bytes":{},"capability_elements":{},"eeprom_read":false,"calibration":false,"radio":false}}"#,
                        report.download_ready_observed,
                        report.patch,
                        report.patch_sections,
                        report.ram_regions,
                        report.scatter_chunks,
                        report.scatter_bytes,
                        report.nic_capability.element_count,
                    );
                    std::io::stdout().flush().map_err(|error| {
                        format!("flush firmware bootstrap ready milestone: {error}")
                    })?;
                }
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
                normal_rx_frames: VecDeque::new(),
                tx_completions: Vec::new(),
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
        let bme_disabled = match set_pci_bus_master(&bdf, false) {
            Ok(()) => true,
            Err(error) => {
                cleanup_errors.push(error);
                false
            }
        };
        if let Some(installed) = irq.as_mut()
            && let Err(error) = installed.disable()
        {
            cleanup_errors.push(error);
        }
        if !bme_disabled {
            retain_mappings_for_watchdog("BME clear failed; DMA mappings remain device-owned");
        }
        // BME readback makes the command arena host-owned even if WFDMA's
        // internal busy indication did not clear before the deadline.
        if bme_disabled {
            if let Err(error) = command_payload.secure_zero_bytes(MCU_COMMAND_PAYLOAD_BYTES) {
                cleanup_errors.push(format!("secure wipe command payload: {error}"));
            }
        }
        #[cfg(feature = "fuchsia-passive")]
        let mut release_errors = attempt_all_cleanup(
            [
                (ActiveArenaKind::MgmtRing, mgmt_tx_ring),
                (ActiveArenaKind::MgmtFrame, mgmt_frame),
                (ActiveArenaKind::MgmtTxwi, mgmt_txwi),
            ],
            |(kind, slot)| {
                slot.as_mut().map_or(Ok(()), |arena| {
                    arena
                        .teardown()
                        .map_err(|error| format!("teardown {kind:?}: {error}"))
                })
            },
        );
        #[cfg(not(feature = "fuchsia-passive"))]
        let mut release_errors = Vec::new();
        release_errors.extend(attempt_all_cleanup(
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
        ));
        if release_errors.is_empty() {
            ledger.confirm_inactive(Hazard::DmaMapping);
            println!("{{\"active_mcu_event\":\"dma_mappings_released_before_reset\"}}");
        } else {
            println!(
                "{{\"active_mcu_event\":\"dma_mapping_release_errors_before_reset\",\"count\":{}}}",
                release_errors.len()
            );
        }
        match reset_vfio_device(&device) {
            Ok(()) => println!("{{\"active_mcu_event\":\"vfio_device_reset_after_unmap\"}}"),
            Err(error) => {
                cleanup_errors.push(format!("VFIO reset after DMA unmap: {error}"));
                retain_mappings_for_watchdog("reset after DMA unmap failed");
            }
        }
        match verify_pci_dma_disabled(&bdf)
            .and_then(|()| verify_active_reset_containment(wfdma, pcie_mac))
            .and_then(|()| set_lab_safety("SAFE"))
        {
            Ok(()) => {
                println!("{{\"active_mcu_event\":\"post_reset_safe_state_verified\"}}");
                for hazard in [
                    Hazard::HostControl,
                    Hazard::DeviceIrq,
                    Hazard::Wfdma,
                    Hazard::BusMaster,
                    Hazard::LabMutated,
                ] {
                    ledger.confirm_inactive(hazard);
                }
            }
            Err(error) => {
                cleanup_errors.push(format!("post-reset safe-state verification: {error}"));
                retain_mappings_for_watchdog("post-reset containment verification failed");
            }
        }
        if let Err(error) = std::io::stdout().flush() {
            cleanup_errors.push(format!("flush containment milestones: {error}"));
        }
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
    payload.write_bytes_at(payload_offset, bytes)?;
    let descriptor = mt7921_dma_tx(
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

#[cfg(feature = "fuchsia-passive")]
fn revoke_before_local_frame_release(
    provenance: &mut DescriptorProvenance,
    queued: &mut VecDeque<PrivateRawFrameCarrier>,
) -> Result<(), String> {
    provenance.invalidate(DescriptorInvalidation::Run)?;
    while let Some(mut frame) = queued.pop_front() {
        frame.bytes.fill(0);
    }
    Ok(())
}

#[cfg(feature = "fuchsia-passive")]
const CLIENT_RX_BACKLOG_CAPACITY: usize = 64;

#[cfg(feature = "fuchsia-passive")]
fn enqueue_client_rx_backlog(
    provenance: &mut DescriptorProvenance,
    queued: &mut VecDeque<PrivateRawFrameCarrier>,
    frame: PrivateRawFrameCarrier,
) -> Result<(), String> {
    if queued.len() >= CLIENT_RX_BACKLOG_CAPACITY {
        revoke_before_local_frame_release(provenance, queued)?;
        let mut frame = frame;
        frame.bytes.fill(0);
        return Err("persistent client RX backlog capacity exceeded".into());
    }
    queued.push_back(frame);
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
    normal_rx_frames: VecDeque<PrivateRawFrameCarrier>,
    tx_completions: Vec<MgmtTxCompletion>,
    descriptor_provenance: DescriptorProvenance,
}

impl Drop for ActiveMcuIo<'_> {
    fn drop(&mut self) {
        // Drop bodies run before fields. Revoke sealed identities before
        // `normal_rx_frames` or any DMA/resource field can be released.
        let _ = self
            .descriptor_provenance
            .invalidate(DescriptorInvalidation::Interface);
        while let Some(mut frame) = self.normal_rx_frames.pop_front() {
            frame.bytes.fill(0);
        }
    }
}

struct VfioFirmwareLoader<'a> {
    mcu: ActiveMcuIo<'a>,
    conn: &'a ReadPage,
    pcie_mac: &'a ReadPage,
    bdf: &'a str,
    fwdl_ring: &'a mut DmaArena,
    fwdl_payload: &'a mut DmaArena,
    sequence: u8,
    command_index: usize,
    uni_terminal_poisoned: bool,
    #[cfg(feature = "fuchsia-passive")]
    client_interface: Option<ClientInterfaceFirmwareState>,
    fwdl_index: usize,
    pending_scatter: Option<(FirmwareImagePart, u8, usize, u32)>,
    start: Instant,
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClientInterfaceFirmwareState {
    identity: ClientVifIdentity,
    dev_maybe_active: bool,
    bss_maybe_active: bool,
}

struct ReceivedMcuResponse {
    event_id: u8,
    option: u8,
    bytes: Vec<u8>,
}

#[cfg(feature = "fuchsia-passive")]
fn classify_uni_ack(expected_cid: u8, response: &ReceivedMcuResponse) -> Result<(), String> {
    if response.option & (1 << 2) != 0 {
        return Err("unified MCU response was unsolicited".into());
    }
    let body = response
        .bytes
        .get(36..44)
        .ok_or("unified MCU response omitted result")?;
    let status = u32::from_le_bytes(body[4..8].try_into().expect("fixed field"));
    if response.event_id != 1 || body[0] != expected_cid || status != 0 {
        return Err(format!(
            "unified MCU response mismatch: eid={} cid={} status={status}",
            response.event_id, body[0]
        ));
    }
    Ok(())
}

#[cfg(feature = "fuchsia-passive")]
fn validate_uni_request(expected_cid: u8, encoded: &[u8]) -> Result<u8, String> {
    let sequence = *encoded
        .get(39)
        .filter(|sequence| (1..=15).contains(*sequence))
        .ok_or("unified command omitted valid sequence")?;
    let total = u16::try_from(encoded.len()).map_err(|_| "unified command exceeded u16 length")?;
    let expected_txd0 = u32::from(total) | (2 << 23) | (0x20 << 25);
    let expected_txd1 = (1u32 << 31) | (1 << 16);
    if encoded.len() < 48
        || u32::from_le_bytes(encoded[0..4].try_into().expect("checked envelope")) != expected_txd0
        || u32::from_le_bytes(encoded[4..8].try_into().expect("checked envelope")) != expected_txd1
        || u16::from_le_bytes(encoded[32..34].try_into().expect("checked envelope")) != total - 32
        || u16::from_le_bytes(encoded[34..36].try_into().expect("checked envelope"))
            != u16::from(expected_cid)
        || encoded[36] != 0
        || encoded[37] != 0xa0
        || encoded[38] != 0
        || encoded[40..43] != [0, 0, 0]
        || encoded[43] != 0x07
        || encoded[44..48] != [0, 0, 0, 0]
    {
        return Err("unified command envelope, CID, or length mismatch".into());
    }
    Ok(sequence)
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UniCommandReclaim {
    ResetAndZero,
    ContainWithDmaOwned,
}

#[cfg(feature = "fuchsia-passive")]
const fn uni_command_reclaim(tx_consumed: bool) -> UniCommandReclaim {
    if tx_consumed {
        UniCommandReclaim::ResetAndZero
    } else {
        UniCommandReclaim::ContainWithDmaOwned
    }
}

#[cfg(feature = "fuchsia-passive")]
fn reclaim_uni_dma_slot(
    tx_ring: &mut DmaArena,
    payload: &mut DmaArena,
    descriptor_index: usize,
) -> Result<(), String> {
    tx_ring.write_descriptor_at(descriptor_index, DmaDescriptor::reset());
    payload.secure_zero_bytes(MCU_COMMAND_PAYLOAD_BYTES)
}

/// Encode pinned Linux's smallest STA_REC_UPDATE: disconnect WCID and reset
/// its WTBL entry. This deliberately contains no key or capability material.
#[cfg(feature = "fuchsia-passive")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveArenaKind {
    #[cfg(feature = "fuchsia-passive")]
    MgmtRing,
    #[cfg(feature = "fuchsia-passive")]
    MgmtFrame,
    #[cfg(feature = "fuchsia-passive")]
    MgmtTxwi,
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
const PASSIVE_MAC_BAR_PAGES: [usize; 9] = [
    0x0f000, 0x21000, 0x23000, 0x24000, 0x34000, 0x38000, 0xa1000, 0xa3000, 0xa4000,
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
        0xd420c => value == 1,
        0xd42f0 => value == 0 || value == 4,
        0xd4680 => value == 4,
        0xd4688 => value == 0x0040_0004,
        0xd4690 => value == 0x00c0_0004,
        0xd4640 => value == 0x0340_0004,
        0xd4644 => value == 0x0380_0004,
        0xd4600 => value == 0x0140_0004,
        0xd4308 | 0xd430c | 0xd4408 => value < 128,
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

#[cfg(feature = "fuchsia-passive")]
fn passive_mac_read_address_allowed(address: u32) -> bool {
    passive_mac_address_allowed(address)
        || matches!(
            address,
            0x820e_5000 | 0x820e_5004 | 0x820f_5000 | 0x820f_5004
        )
}

#[cfg(feature = "fuchsia-passive")]
fn passive_mac_read_bar_offset(address: u32) -> Result<usize, String> {
    match address {
        0x820e_5000 | 0x820e_5004 => Ok(0x0002_1400 + (address - 0x820e_5000) as usize),
        0x820f_5000 | 0x820f_5004 => Ok(0x000a_1400 + (address - 0x820f_5000) as usize),
        _ => passive_mac_bar_offset(address)
            .map_err(|error| format!("translate passive MAC read: {error:?}")),
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
    Completion(MgmtTxCompletion),
}

fn drain_rx_queue(
    wfdma: &ReadPage,
    queue: &mut ActiveMcuRx<'_>,
    expected_sequence: Option<u8>,
    unsolicited: &mut Vec<ReceivedMcuResponse>,
    normal_rx_frames: &mut VecDeque<PrivateRawFrameCarrier>,
    tx_completions: &mut Vec<MgmtTxCompletion>,
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
            } else if !(12..=2048).contains(&response_len) {
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
                let rxd0 = u32::from_le_bytes(response[0..4].try_into().expect("bounded response"));
                let packet_type = (rxd0 >> 27) & 0x1f;
                let packet_flag = (rxd0 >> 16) & 0x0f;
                let completion = match packet_type {
                    6 => Some(
                        parse_mt7921_tx_free(&response)
                            .map(MgmtTxCompletion::Free)
                            .map_err(|error| format!("parse TX_FREE: {error:?}")),
                    ),
                    0 if response_len >= 40 && (rxd0 & 0xffff) as usize == response_len => Some(
                        parse_mt7921_tx_status(&response)
                            .map(MgmtTxCompletion::Status)
                            .map_err(|error| format!("parse TXS: {error:?}")),
                    ),
                    _ => None,
                };
                if let Some(completion) = completion {
                    provenance.consume_without_mint(
                        DescriptorOccurrenceRoute::McuNormalRx,
                        queue.rx_ring_index,
                        completed_index,
                    );
                    completion.map(DrainedMcuRx::Completion)
                } else if response_len < 36 {
                    provenance.consume_without_mint(
                        DescriptorOccurrenceRoute::McuNormalRx,
                        queue.rx_ring_index,
                        completed_index,
                    );
                    Err(format!(
                        "invalid MCU response descriptor length {response_len}"
                    ))
                } else if packet_type == 7 && packet_flag == 1 {
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
                    let actual_sequence = response[29];
                    let header_length = response
                        .get(24..26)
                        .map(|bytes| u16::from_le_bytes(bytes.try_into().expect("fixed field")));
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
            let refill = mt7921_dma_rx(DmaSegment {
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
                    enqueue_client_rx_backlog(provenance, normal_rx_frames, frame)?;
                    continue;
                }
                DrainedMcuRx::Response(parsed, response) => (parsed, response),
                DrainedMcuRx::Completion(completion) => {
                    record_sae_stage(&format!(
                        "management_tx_completion_routed rx_ring={} completion={completion:?}",
                        queue.rx_ring_index
                    ));
                    tx_completions.push(completion);
                    continue;
                }
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
        revoke_before_local_frame_release(provenance, normal_rx_frames)?;
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
                &mut self.tx_completions,
                &mut self.descriptor_provenance,
            )?;
            if let Some(wm2) = self.wm2.as_mut() {
                let wm2_match = drain_rx_queue(
                    self.wfdma,
                    wm2,
                    expected_sequence,
                    &mut self.unsolicited,
                    &mut self.normal_rx_frames,
                    &mut self.tx_completions,
                    &mut self.descriptor_provenance,
                )?;
                merge_matching_response(&mut matched, wm2_match)?;
            }
            self.wfdma.write_active_wfdma(0xd4204, irq_mask)?;
            Ok(matched)
        })();
        if result.is_err() {
            revoke_before_local_frame_release(
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
    fn send_client_edca_bytes(&mut self, encoded: &[u8]) -> Result<(), String> {
        self.ensure_mcu_tx_allowed()?;
        self.mcu.cancelled()?;
        if encoded.len() != 108 || encoded.get(36..39) != Some(&[0x1d, 0xa0, 1]) {
            return Err("client EDCA escaped CE SET_EDCA_PARMS".into());
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
                self.uni_terminal_poisoned = true;
                return Err(format!(
                    "EDCA command DMA consumption timed out at descriptor {descriptor_index}"
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        reclaim_uni_dma_slot(self.mcu.tx_ring, self.mcu.payload, descriptor_index)?;
        Ok(())
    }

    fn ensure_mcu_tx_allowed(&self) -> Result<(), String> {
        if self.uni_terminal_poisoned {
            Err("MCU TX transport is terminally poisoned; containment required".into())
        } else {
            Ok(())
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    fn program_client_interface(&mut self, identity: ClientVifIdentity) -> Result<(), String> {
        if self.client_interface.is_some() {
            return Err("client interface firmware context was already dirty".into());
        }
        let [dev, bss] = encode_client_interface_commands(identity.bytes(), true, 14, 15)?;
        // Publication can become ambiguous at any point after entry. Mark each
        // object dirty before submitting it so mandatory cleanup will disable
        // every object firmware may have observed.
        self.client_interface = Some(ClientInterfaceFirmwareState {
            identity,
            dev_maybe_active: true,
            bss_maybe_active: false,
        });
        self.send_acknowledged_uni_command(1, &dev)?;
        record_sae_stage("client_dev_info_active_acked omac=0 identity_match=true");

        self.client_interface
            .as_mut()
            .expect("client interface state installed")
            .bss_maybe_active = true;
        self.send_acknowledged_uni_command(2, &bss)?;
        record_sae_stage("client_bss_info_basic_acked bss=0 wmm=0 wcid=19");
        Ok(())
    }

    #[cfg(feature = "fuchsia-passive")]
    fn disable_client_interface(&mut self) -> Result<(), String> {
        let Some(state) = self.client_interface else {
            return Ok(());
        };
        let [bss, dev] = encode_client_interface_commands(state.identity.bytes(), false, 12, 13)?;
        let mut errors = Vec::new();
        if state.bss_maybe_active {
            match self.send_acknowledged_uni_command(2, &bss) {
                Ok(()) => {
                    self.client_interface
                        .as_mut()
                        .expect("client interface state retained")
                        .bss_maybe_active = false;
                    record_sae_stage("client_bss_info_basic_disabled_acked bss=0");
                }
                Err(error) => errors.push(format!("disable client BSS context: {error}")),
            }
        }
        if state.dev_maybe_active {
            match self.send_acknowledged_uni_command(1, &dev) {
                Ok(()) => {
                    self.client_interface
                        .as_mut()
                        .expect("client interface state retained")
                        .dev_maybe_active = false;
                    record_sae_stage("client_dev_info_active_disabled_acked omac=0");
                }
                Err(error) => errors.push(format!("disable client DEV context: {error}")),
            }
        }
        if self
            .client_interface
            .is_some_and(|state| !state.bss_maybe_active && !state.dev_maybe_active)
        {
            self.client_interface = None;
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn send_acknowledged_uni_command(
        &mut self,
        expected_cid: u8,
        encoded: &[u8],
    ) -> Result<(), String> {
        let mut publication = PublicationState::Local;
        self.ensure_mcu_tx_allowed()?;
        self.mcu.cancelled()?;
        let sequence = validate_uni_request(expected_cid, encoded)?;
        if encoded.len() > MCU_COMMAND_SLOT_BYTES {
            return Err("unified command exceeded one DMA slot".into());
        }
        let descriptor_index = self.command_index;
        let next = next_dma_index(descriptor_index, MCU_TX_RING_COUNT);
        let pre_cidx = self.mcu.wfdma.read(0xd4418).map_err(|error| {
            println!(
                "{{\"active_mcu_event\":\"uni_ring_read_error\",\"stage\":\"pre_publish_cidx\",\"sequence\":{sequence},\"cid\":{expected_cid},\"tx_descriptor\":{descriptor_index}}}"
            );
            error
        })?;
        let pre_didx = self.mcu.wfdma.read(0xd441c).map_err(|error| {
            println!(
                "{{\"active_mcu_event\":\"uni_ring_read_error\",\"stage\":\"pre_publish_didx\",\"sequence\":{sequence},\"cid\":{expected_cid},\"tx_descriptor\":{descriptor_index}}}"
            );
            error
        })?;
        let pre_owned_by_cpu = self
            .mcu
            .tx_ring
            .read_descriptor_at(descriptor_index)
            .is_dma_done();
        println!(
            "{{\"active_mcu_event\":\"uni_ring_pre_publish\",\"sequence\":{sequence},\"cid\":{expected_cid},\"tx_descriptor\":{descriptor_index},\"cidx\":{pre_cidx},\"didx\":{pre_didx},\"cpu_owned\":{pre_owned_by_cpu}}}"
        );
        if pre_cidx != descriptor_index as u32
            || pre_didx != descriptor_index as u32
            || !pre_owned_by_cpu
        {
            self.uni_terminal_poisoned = true;
            return Err(format!(
                "unified MCU ring ownership mismatch before publication; containment required: descriptor={descriptor_index} cidx={pre_cidx} didx={pre_didx} cpu_owned={pre_owned_by_cpu}"
            ));
        }
        self.mcu
            .wfdma
            .write_active_wfdma(0xd4204, self.mcu.rx_irq_mask())?;
        publication.begin().expect("fresh publication state");
        if let Err(error) = publish_mcu_bytes(
            self.mcu.wfdma,
            self.mcu.tx_ring,
            self.mcu.payload,
            encoded,
            sequence,
            descriptor_index,
        ) {
            // Envelope/descriptor failures were excluded before DMA. The
            // remaining failure is producer publication, where ownership is
            // uncertain and the slot must not be reused or overwritten.
            self.uni_terminal_poisoned = true;
            return Err(format!(
                "unified MCU publication failed with uncertain DMA ownership; containment required: {error}"
            ));
        }
        publication.published().expect("publication completed");
        self.command_index = next;
        let post_cidx = self.mcu.wfdma.read(0xd4418).map_err(|error| {
            self.uni_terminal_poisoned = true;
            println!(
                "{{\"active_mcu_event\":\"uni_ring_read_error\",\"stage\":\"post_publish_cidx\",\"sequence\":{sequence},\"cid\":{expected_cid},\"tx_descriptor\":{descriptor_index}}}"
            );
            format!(
                "unified MCU producer read failed after publication; containment required: {error}"
            )
        })?;
        let post_didx = self.mcu.wfdma.read(0xd441c).map_err(|error| {
            self.uni_terminal_poisoned = true;
            println!(
                "{{\"active_mcu_event\":\"uni_ring_read_error\",\"stage\":\"post_publish_didx\",\"sequence\":{sequence},\"cid\":{expected_cid},\"tx_descriptor\":{descriptor_index}}}"
            );
            format!(
                "unified MCU consumer read failed after publication; containment required: {error}"
            )
        })?;
        let post_owned_by_cpu = self
            .mcu
            .tx_ring
            .read_descriptor_at(descriptor_index)
            .is_dma_done();
        println!(
            "{{\"active_mcu_event\":\"uni_ring_post_publish\",\"sequence\":{sequence},\"cid\":{expected_cid},\"tx_descriptor\":{descriptor_index},\"cidx\":{post_cidx},\"didx\":{post_didx},\"cpu_owned\":{post_owned_by_cpu}}}"
        );
        let response = self
            .mcu
            .wait_response(sequence, Instant::now() + std::time::Duration::from_secs(3));

        // A response normally implies consumption, but DIDX is the ownership
        // boundary: never overwrite a slot while WFDMA may still read it.
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        let consumed = loop {
            let didx = match self.mcu.wfdma.read(0xd441c) {
                Ok(value) => value,
                Err(error) => {
                    self.uni_terminal_poisoned = true;
                    println!(
                        "{{\"active_mcu_event\":\"uni_ring_read_error\",\"stage\":\"ownership_wait_didx\",\"sequence\":{sequence},\"cid\":{expected_cid},\"tx_descriptor\":{descriptor_index}}}"
                    );
                    return Err(format!(
                        "unified MCU ownership read failed; containment required: {error}"
                    ));
                }
            };
            if dma_index_completed(didx, next as u32) {
                let cpu_owned = self
                    .mcu
                    .tx_ring
                    .read_descriptor_at(descriptor_index)
                    .is_dma_done();
                println!(
                    "{{\"active_mcu_event\":\"uni_ring_consumed\",\"sequence\":{sequence},\"cid\":{expected_cid},\"tx_descriptor\":{descriptor_index},\"cidx\":{},\"didx\":{didx},\"cpu_owned\":{cpu_owned}}}",
                    self.command_index
                );
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        if uni_command_reclaim(consumed) == UniCommandReclaim::ContainWithDmaOwned {
            self.uni_terminal_poisoned = publication.requires_containment();
            let final_cidx = self.mcu.wfdma.read(0xd4418);
            let final_didx = self.mcu.wfdma.read(0xd441c);
            let cpu_owned = self
                .mcu
                .tx_ring
                .read_descriptor_at(descriptor_index)
                .is_dma_done();
            println!(
                "{{\"active_mcu_event\":\"uni_ring_timeout\",\"sequence\":{sequence},\"cid\":{expected_cid},\"tx_descriptor\":{descriptor_index},\"cidx\":\"{final_cidx:?}\",\"didx\":\"{final_didx:?}\",\"cpu_owned\":{cpu_owned}}}"
            );
            return Err(format!(
                "unified MCU command sequence {sequence} timed out with DMA slot still device-owned; containment required"
            ));
        }

        // Key-bearing CID3 commands will use this same boundary. Once DIDX
        // proves reclamation safe, cleanup runs for timeout and negative ACK.
        reclaim_uni_dma_slot(self.mcu.tx_ring, self.mcu.payload, descriptor_index)?;
        let result = classify_uni_ack(expected_cid, &response?);
        if result.is_ok() {
            publication.acknowledged().expect("published command");
        }
        result
    }

    fn send_passive_command(
        &mut self,
        command: &PassiveMcuCommand,
        encoded: &[u8],
        wait_response: bool,
    ) -> Result<(), String> {
        self.ensure_mcu_tx_allowed()?;
        self.mcu.cancelled()?;
        let sequence = *encoded
            .get(39)
            .filter(|sequence| (1..=15).contains(*sequence))
            .ok_or("passive command omitted valid sequence")?;
        if wait_response != command.expects_response() {
            return Err("passive response policy disagreed with encoded command".into());
        }
        if let Some(expected_cid) = match command {
            PassiveMcuCommand::AddDevice { .. } => Some(1),
            PassiveMcuCommand::AddBss => Some(2),
            _ => None,
        } {
            self.send_acknowledged_uni_command(expected_cid, encoded)?;
            println!(
                r#"{{"passive_scan_event":"command_completed","command":"{command:?}","sequence":{sequence}}}"#
            );
            return Ok(());
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
        self.ensure_mcu_tx_allowed()?;
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
        self.ensure_mcu_tx_allowed()?;
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
        self.ensure_mcu_tx_allowed()?;
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
        let milestone = match command {
            DownloadCommand::PatchFinish => Some("patch_published_and_finished"),
            DownloadCommand::FirmwareStart { .. } => Some("ram_published_firmware_start_acked"),
            DownloadCommand::GetNicCapability => Some("nic_capability_response"),
            DownloadCommand::ReadEepromBlock { .. } => Some("eeprom_efuse_acquired"),
            _ => None,
        };
        if let Some(event) = milestone {
            println!("{{\"firmware_bootstrap_event\":\"{event}\",\"sequence\":{sequence}}}");
            std::io::stdout()
                .flush()
                .map_err(|error| format!("flush firmware command milestone: {error}"))?;
        }
        Ok(completion)
    }

    fn set_clc(
        &mut self,
        command: &ClcSetCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<Option<ClcSetResponse>, Self::Error> {
        self.ensure_mcu_tx_allowed()?;
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
        println!(
            "{{\"firmware_bootstrap_event\":\"clc_calibration_configured\",\"sequence\":{sequence},\"rule_index\":{},\"response\":{}}}",
            command.index,
            response.is_some(),
        );
        std::io::stdout()
            .flush()
            .map_err(|error| format!("flush CLC/calibration milestone: {error}"))?;
        Ok(response)
    }

    fn set_channel_domain(
        &mut self,
        command: &ChannelDomainCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<(), Self::Error> {
        self.ensure_mcu_tx_allowed()?;
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
        let descriptor = mt7921_dma_tx(
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
        let ready = self.conn.read(0xe00f0)? & 3 == 3;
        if ready {
            println!("{{\"firmware_bootstrap_event\":\"n9_ready\"}}");
            std::io::stdout()
                .flush()
                .map_err(|error| format!("flush N9-ready milestone: {error}"))?;
        }
        Ok(ready)
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
        // The passive boundary always exits through this transaction, whether
        // connect succeeded, failed, reset, or stopped. Disable firmware's BSS
        // before DEV while command/RX transport is still live; ambiguity is
        // retained as a cleanup error and contained by the mandatory reset.
        #[cfg(feature = "fuchsia-passive")]
        if let Err(error) = self.disable_client_interface() {
            errors.push(format!("client interface teardown: {error}"));
        }
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
            println!(r#"{{"active_fwdl_event":"transport_quiesced"}}"#);
            std::io::stdout()
                .flush()
                .map_err(|error| format!("flush firmware transport cleanup milestone: {error}"))?;
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WtblPeerReadback {
    Match,
    Mismatch,
    Unavailable,
    AllOnes,
}

#[cfg(feature = "fuchsia-passive")]
impl WtblPeerReadback {
    const fn label(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Mismatch => "mismatch",
            Self::Unavailable => "unavailable",
            Self::AllOnes => "all_ones",
        }
    }
}

#[cfg(feature = "fuchsia-passive")]
fn classify_wtbl_peer_readback(
    peer: &[u8],
    words: Result<(u32, u32), WtblPeerReadback>,
) -> WtblPeerReadback {
    let (word0, word1) = match words {
        Ok(words) => words,
        Err(category) => return category,
    };
    if peer.len() == 6
        && word0 as u16 == u16::from_le_bytes([peer[4], peer[5]])
        && word1 == u32::from_le_bytes(peer[..4].try_into().unwrap())
    {
        WtblPeerReadback::Match
    } else {
        WtblPeerReadback::Mismatch
    }
}

#[cfg(feature = "fuchsia-passive")]
struct PassiveMacExecutor<'a> {
    pages: &'a [Option<ReadPage>; PASSIVE_MAC_BAR_PAGES.len()],
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PassivePrepareStep {
    MacMmio,
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
        let offset = passive_mac_read_bar_offset(address)?;
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

    fn clear_wtbl_admission_counts(&self, wcid: u8) -> Result<(), String> {
        let address = 0x820d_4230;
        let initial = self.read(address)?;
        self.write(address, (initial & !0x03ff) | u32::from(wcid) | (1 << 12))?;
        let deadline = Instant::now() + std::time::Duration::from_micros(5000);
        loop {
            if self.read(address)? & (1 << 31) == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("WTBL admission-count clear remained busy".into());
            }
            std::hint::spin_loop();
        }
    }

    fn wtbl_peer_readback(&self, wcid: u8, peer: &[u8]) -> WtblPeerReadback {
        if wcid != 7 || peer.len() != 6 {
            return WtblPeerReadback::Unavailable;
        }
        let address = 0x820d_8000 | (u32::from(wcid) << 8);
        let read = |address| {
            self.read(address).map_err(|error| {
                if error.contains("returned all ones") {
                    WtblPeerReadback::AllOnes
                } else {
                    WtblPeerReadback::Unavailable
                }
            })
        };
        classify_wtbl_peer_readback(
            peer,
            read(address).and_then(|word0| read(address + 4).map(|word1| (word0, word1))),
        )
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
    mut normal_rx_frames: Option<&mut VecDeque<PrivateRawFrameCarrier>>,
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
                record_sae_stage(&format!(
                    "client_rx_descriptor ring={} descriptor={} len={} packet_type={:?}",
                    queue.rx_ring_index,
                    completed_index,
                    length,
                    mt7921_packet_type(&bytes)
                ));
                if let Some(header) = bytes.get(..16) {
                    let rxd0 = u32::from_le_bytes(header[0..4].try_into().unwrap());
                    let rxd1 = u32::from_le_bytes(header[4..8].try_into().unwrap());
                    let rxd2 = u32::from_le_bytes(header[8..12].try_into().unwrap());
                    let rxd3 = u32::from_le_bytes(header[12..16].try_into().unwrap());
                    let groups = (rxd1 >> 11) & 0x1f;
                    let metadata_len = 24
                        + if groups & 0x08 != 0 { 16 } else { 0 }
                        + if groups & 0x01 != 0 { 16 } else { 0 }
                        + if groups & 0x02 != 0 { 8 } else { 0 }
                        + if groups & 0x04 != 0 { 8 } else { 0 }
                        + if groups & 0x10 != 0 { 72 } else { 0 }
                        + 2 * ((rxd2 >> 14) & 0x3);
                    record_sae_stage(&format!(
                        "client_rx_rxd rxd0={rxd0:#010x} rxd1={rxd1:#010x} rxd2={rxd2:#010x} rxd3={rxd3:#010x} reported_len={} groups={groups:#04x} hdr_trans={} hdr_offset={} metadata_len={} channel={} wcid={} tid={} security_mode={} key_id={} errors={:#010x}",
                        rxd0 & 0xffff,
                        rxd2 >> 13 & 1,
                        rxd2 >> 14 & 0x3,
                        metadata_len,
                        rxd3 >> 8 & 0xff,
                        rxd1 & 0x03ff,
                        rxd2 >> 16 & 0x0f,
                        rxd1 >> 16 & 0x1f,
                        rxd1 >> 21 & 0x03,
                        (rxd1 & 0x1e00_0000) | (rxd2 & 0x0380_0000),
                    ));
                }
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
                    let passive = match parse_connac2_rx_frame(&bytes) {
                        Ok(frame)
                            if frame
                                .bytes
                                .get(..2)
                                .map(|control| {
                                    u16::from_le_bytes([control[0], control[1]]) & 0x00fc == 0x00b0
                                })
                                .unwrap_or(false) =>
                        {
                            Err(PassiveRxError::UnsupportedFrame)
                        }
                        _ => parse_passive_advertisement(&bytes),
                    };
                    match passive {
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
                        Err(PassiveRxError::UnsupportedFrame) if normal_rx_frames.is_some() => {
                            let frame = match provenance.seal_frame(
                                DescriptorOccurrenceRoute::DataRx,
                                queue.rx_ring_index,
                                completed_index,
                                bytes,
                            )? {
                                PrivateFrameSeal::Carried(frame) => frame,
                                PrivateFrameSeal::Uncovered(bytes) => PrivateRawFrameCarrier {
                                    bytes,
                                    occurrence: None,
                                },
                            };
                            enqueue_client_rx_backlog(
                                provenance,
                                normal_rx_frames.as_deref_mut().unwrap(),
                                frame,
                            )?;
                            Ok(None)
                        }
                        Err(PassiveRxError::RxError) => {
                            let class = bytes.get(..12).map_or("unknown", |header| {
                                let rxd1 = u32::from_le_bytes(header[4..8].try_into().unwrap());
                                let rxd2 = u32::from_le_bytes(header[8..12].try_into().unwrap());
                                if rxd1 & (1 << 27) != 0 {
                                    "fcs"
                                } else if rxd1 & (1 << 26) != 0 {
                                    "mic"
                                } else if rxd1 & (1 << 25) != 0 {
                                    "integrity"
                                } else if rxd2 & (1 << 23) != 0 {
                                    "amsdu"
                                } else if rxd2 & (1 << 24) != 0 {
                                    "length"
                                } else if rxd2 & (1 << 25) != 0 {
                                    "translation"
                                } else {
                                    "unknown"
                                }
                            });
                            provenance.consume_without_mint(
                                DescriptorOccurrenceRoute::DataRx,
                                queue.rx_ring_index,
                                completed_index,
                            );
                            record_sae_stage(&format!(
                                "client_rx_filtered reason=hardware_rx_error class={class} rearm=pending"
                            ));
                            Ok(None)
                        }
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
            let refill = mt7921_dma_rx(DmaSegment {
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
            record_sae_stage(&format!(
                "client_rx_rearm ring={} consumed_descriptor={} refill_descriptor={} result=published",
                queue.rx_ring_index, completed_index, refill_index
            ));
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MgmtTxCompletion {
    Free(Mt7921TxFree),
    Status(Mt7921TxStatus),
}

#[cfg(feature = "fuchsia-passive")]
struct MgmtTxCompletionState {
    token: u16,
    pid: u8,
    free: Option<Mt7921TxFree>,
    status: Option<Mt7921TxStatus>,
}

#[cfg(feature = "fuchsia-passive")]
impl MgmtTxCompletionState {
    fn new(token: u16, pid: u8) -> Self {
        Self {
            token,
            pid,
            free: None,
            status: None,
        }
    }

    fn observe(&mut self, completion: MgmtTxCompletion) -> Result<(), String> {
        match completion {
            MgmtTxCompletion::Free(value) if value.token == self.token && self.free.is_none() => {
                self.free = Some(value)
            }
            MgmtTxCompletion::Status(value)
                if value.pid == self.pid && value.wcid == 19 && self.status.is_none() =>
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MgmtTxPublicationOutcome {
    NotPublished,
    Committed,
    Completed,
    AmbiguousOwnership,
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Default)]
struct MgmtTxOutstanding {
    next_token: u16,
    next_pid: u8,
    entries: Vec<MgmtTxCompletionState>,
}

#[cfg(feature = "fuchsia-passive")]
impl MgmtTxOutstanding {
    fn reserve(&mut self) -> Result<(u16, u8), String> {
        if self.next_token >= 8192 {
            return Err("management TX token space exhausted before teardown".into());
        }
        let token = self.next_token;
        let pid = 3u8
            .checked_add(self.next_pid)
            .filter(|pid| *pid < 127)
            .ok_or("management TX PID space exhausted before teardown")?;
        self.next_token += 1;
        self.next_pid += 1;
        self.entries.push(MgmtTxCompletionState::new(token, pid));
        Ok((token, pid))
    }

    fn abandon_last(&mut self, token: u16, pid: u8) {
        if self
            .entries
            .last()
            .is_some_and(|entry| entry.token == token && entry.pid == pid)
        {
            self.entries.pop();
        }
    }

    fn observe(
        &mut self,
        completion: MgmtTxCompletion,
    ) -> Result<Option<MgmtTxPublicationOutcome>, String> {
        let entry = match completion {
            MgmtTxCompletion::Free(value) => self
                .entries
                .iter_mut()
                .find(|entry| entry.token == value.token),
            MgmtTxCompletion::Status(value) => {
                self.entries.iter_mut().find(|entry| entry.pid == value.pid)
            }
        }
        .ok_or("uncorrelated management TX completion")?;
        entry.observe(completion)?;
        let Some(result) = entry.finished() else {
            return Ok(None);
        };
        result?;
        let token = entry.token;
        let pid = entry.pid;
        self.entries
            .retain(|entry| entry.token != token || entry.pid != pid);
        Ok(Some(MgmtTxPublicationOutcome::Completed))
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(feature = "fuchsia-passive")]
struct ReceivedSaeAuth {
    receiver: [u8; 6],
    transmitter: [u8; 6],
    bssid: [u8; 6],
    algorithm: u16,
    sequence: u16,
    status: fidl_ieee80211::StatusCode,
    fields: Vec<u8>,
}

#[cfg(feature = "fuchsia-passive")]
fn record_sae_commit_structure(frame: &[u8]) -> Result<(), String> {
    let header = frame
        .get(..32)
        .ok_or("SAE commit is shorter than the authentication header")?;
    let algorithm = u16::from_le_bytes(header[24..26].try_into().unwrap());
    let transaction = u16::from_le_bytes(header[26..28].try_into().unwrap());
    let status = u16::from_le_bytes(header[28..30].try_into().unwrap());
    let group = u16::from_le_bytes(header[30..32].try_into().unwrap());
    if algorithm != 3 || transaction != 1 || status != 126 {
        return Err(format!(
            "unexpected SAE commit header algorithm={algorithm} transaction={transaction} status={status} group={group}"
        ));
    }
    let (scalar_len, element_len, order_hex, p_hex, b_hex) = match group {
        19 => (
            32,
            64,
            b"ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551".as_slice(),
            b"ffffffff00000001000000000000000000000000ffffffffffffffffffffffff".as_slice(),
            b"5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b".as_slice(),
        ),
        20 => (
            48,
            96,
            b"ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973".as_slice(),
            b"fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff".as_slice(),
            b"b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef".as_slice(),
        ),
        _ => return Err(format!("unexpected SAE commit group {group}")),
    };
    let fixed_end = 32 + scalar_len + element_len;
    let fixed = frame
        .get(..fixed_end)
        .ok_or("SAE commit is shorter than the selected group's fixed body")?;
    let scalar = &fixed[32..32 + scalar_len];
    let element = &fixed[32 + scalar_len..fixed_end];
    let scalar_value = BigUint::from_bytes_be(scalar);
    let order = BigUint::parse_bytes(order_hex, 16).unwrap();
    let scalar_range = scalar_value > BigUint::from(1u8) && scalar_value < order;
    let p = BigUint::parse_bytes(p_hex, 16).unwrap();
    let b = BigUint::parse_bytes(b_hex, 16).unwrap();
    let coordinate_len = element_len / 2;
    let x = BigUint::from_bytes_be(&element[..coordinate_len]);
    let y = BigUint::from_bytes_be(&element[coordinate_len..]);
    let three_x = (&x * BigUint::from(3u8)) % &p;
    let rhs = (x.modpow(&BigUint::from(3u8), &p) + (&p - three_x) + b) % &p;
    let element_on_curve = x < p && y < p && y.modpow(&BigUint::from(2u8), &p) == rhs;
    let mut tail = frame.len().saturating_sub(fixed_end);
    let mut tail_ies = Vec::new();
    let mut offset = fixed_end;
    while tail != 0 {
        let header = frame
            .get(offset..offset + 2)
            .ok_or("SAE commit has a truncated tail IE header")?;
        let len = usize::from(header[1]);
        frame
            .get(offset + 2..offset + 2 + len)
            .ok_or("SAE commit has a truncated tail IE body")?;
        tail_ies.push(format!("{}:{len}", header[0]));
        offset += 2 + len;
        tail = frame.len().saturating_sub(offset);
    }
    let receiver = &frame[4..10];
    let transmitter = &frame[10..16];
    let bssid = &frame[16..22];
    let sequence_control = u16::from_le_bytes(frame[22..24].try_into().unwrap());
    println!(
        r#"{{"sae_commit_structure":{{"fc":"0x{:04x}","receiver":"{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}","transmitter":"{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}","bssid":"{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}","seq_control":{},"algorithm":{},"transaction":{},"status":{},"group":{},"body_len":{},"scalar_len":{},"element_len":{},"tail_len":{},"scalar_range":{},"element_on_curve":{},"tail_ies":"{}"}}}}"#,
        u16::from_le_bytes(frame[0..2].try_into().unwrap()),
        receiver[0],
        receiver[1],
        receiver[2],
        receiver[3],
        receiver[4],
        receiver[5],
        transmitter[0],
        transmitter[1],
        transmitter[2],
        transmitter[3],
        transmitter[4],
        transmitter[5],
        bssid[0],
        bssid[1],
        bssid[2],
        bssid[3],
        bssid[4],
        bssid[5],
        sequence_control,
        algorithm,
        transaction,
        status,
        group,
        frame.len() - 30,
        scalar.len(),
        element.len(),
        frame.len() - fixed_end,
        scalar_range,
        element_on_curve,
        tail_ies.join(",")
    );
    Ok(())
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Default)]
struct LiveClientState {
    selection: ClientTargetBssLease,
    channel: ClientChannelContext,
}

#[cfg(feature = "fuchsia-passive")]
impl LiveClientState {
    fn authorize_sae(
        &mut self,
        bssid: [u8; 6],
        channel: ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: ChannelNumber,
    ) -> Result<u64, String> {
        let physical = client_physical_channel(channel, bandwidth, secondary)
            .map_err(|status| status.to_string())?;
        let ClientPhysicalChannelEnsure::Current(channel) = self.channel.ensure_channel(physical)
        else {
            return Err("SAE authorization does not match physical channel".into());
        };
        self.selection.authorize_sae(bssid, channel)?;
        self.channel.authorize_channel(physical)?;
        Ok(channel.generation)
    }

    fn mark_rate_power_ready(
        &mut self,
        bssid: [u8; 6],
        channel: ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: ChannelNumber,
    ) -> Result<(), String> {
        self.selection.mark_rate_power_ready(
            bssid,
            client_physical_channel(channel, bandwidth, secondary)
                .map_err(|status| status.to_string())?,
        )
    }
}

#[cfg(feature = "fuchsia-passive")]
fn client_physical_channel(
    channel: ChannelNumber,
    bandwidth: fidl_ieee80211::ChannelBandwidth,
    secondary: ChannelNumber,
) -> Result<ClientPhysicalChannel, zx::Status> {
    let band = match channel.band {
        WlanBand::TwoGhz => 0,
        WlanBand::FiveGhz => 1,
        _ => return Err(zx::Status::INVALID_ARGS),
    };
    let shape = LinuxChannelShape::from_fidl(channel, bandwidth, Some(secondary))
        .ok_or(zx::Status::INVALID_ARGS)?;
    Ok(ClientPhysicalChannel {
        band,
        primary: u16::from(channel.number),
        center: u16::from(shape.center_channel),
        bandwidth: shape.bandwidth,
        center2: u16::from(shape.center_channel2),
    })
}

#[cfg(feature = "fuchsia-passive")]
fn retain_client_selection(
    scan_id: u64,
    observation_generation: u64,
    observation: &fuchsia_softmac_port::ScanObservation,
) -> Result<ClientTargetBssLease, zx::Status> {
    ClientTargetBssLease::retain(ClientScanEvidence {
        scan_id,
        observation_generation,
        observation_timestamp_nanos: observation.timestamp_nanos,
        bssid: observation.bss.bssid,
        channel: client_physical_channel(
            observation.bss.primary,
            observation.bss.bandwidth,
            observation.bss.vht_secondary_80_channel,
        )?,
    })
    .map_err(|_| zx::Status::INVALID_ARGS)
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClientDataFrameClassification {
    frame_type: u8,
    subtype: u8,
    to_ds: bool,
    from_ds: bool,
    protected: bool,
    addr1_is_client: bool,
    addr2_is_peer: bool,
    addr3_is_bssid: bool,
    snap_present: bool,
    ether_type: Option<u16>,
    llc_result: &'static str,
}

#[cfg(feature = "fuchsia-passive")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClientManagementFrameClassification {
    subtype: u8,
    addr1_is_client: bool,
    addr2_is_peer: bool,
    addr3_is_bssid: bool,
}

#[cfg(feature = "fuchsia-passive")]
fn classify_client_management_frame(
    bytes: &[u8],
    client: [u8; 6],
    peer: [u8; 6],
) -> ClientManagementFrameClassification {
    let control = bytes
        .get(..2)
        .map(|value| u16::from_le_bytes([value[0], value[1]]))
        .unwrap_or(0);
    ClientManagementFrameClassification {
        subtype: ((control >> 4) & 15) as u8,
        addr1_is_client: bytes.get(4..10) == Some(&client),
        addr2_is_peer: bytes.get(10..16) == Some(&peer),
        addr3_is_bssid: bytes.get(16..22) == Some(&peer),
    }
}

#[cfg(feature = "fuchsia-passive")]
fn strip_verified_management_ccmp(bytes: &mut Vec<u8>) -> Result<(), ()> {
    if bytes.len() >= 42 {
        let mut plaintext = Vec::with_capacity(bytes.len() - 16);
        plaintext.extend_from_slice(&bytes[..24]);
        plaintext.extend_from_slice(&bytes[32..bytes.len() - 8]);
        *bytes = plaintext;
    }
    (bytes.len() >= 26).then_some(()).ok_or(())
}

#[cfg(feature = "fuchsia-passive")]
fn management_ie_id_lengths(bytes: &[u8], offset: usize) -> String {
    let mut cursor = offset;
    let mut fields = Vec::new();
    while cursor < bytes.len() {
        let Some(header) = bytes.get(cursor..cursor + 2) else {
            fields.push("malformed".to_string());
            break;
        };
        let length = usize::from(header[1]);
        fields.push(format!("{}:{length}", header[0]));
        let Some(next) = cursor.checked_add(2 + length) else {
            fields.push("overflow".to_string());
            break;
        };
        if next > bytes.len() {
            fields.push("truncated".to_string());
            break;
        }
        cursor = next;
    }
    fields.join(",")
}

#[cfg(feature = "fuchsia-passive")]
fn association_comeback_interval(bytes: &[u8], offset: usize) -> Option<(u32, u64)> {
    let mut cursor = offset;
    let mut comeback = None;
    while cursor < bytes.len() {
        let header = bytes.get(cursor..cursor.checked_add(2)?)?;
        let length = usize::from(header[1]);
        let next = cursor.checked_add(2 + length)?;
        let body = bytes.get(cursor + 2..next)?;
        if header[0] == 56 {
            if comeback.is_some() || body.len() != 5 || body[0] != 3 {
                return None;
            }
            let tu = u32::from_le_bytes(body[1..5].try_into().ok()?);
            if tu == 0 {
                return None;
            }
            comeback = Some((tu, u64::from(tu) * 1024 / 1000));
        }
        cursor = next;
    }
    comeback
}

#[cfg(feature = "fuchsia-passive")]
fn classify_client_data_frame(
    bytes: &[u8],
    client: [u8; 6],
    peer: [u8; 6],
) -> ClientDataFrameClassification {
    let control = bytes
        .get(..2)
        .map(|value| u16::from_le_bytes([value[0], value[1]]))
        .unwrap_or(0);
    let frame_type = ((control >> 2) & 3) as u8;
    let subtype = ((control >> 4) & 15) as u8;
    let to_ds = control & 0x0100 != 0;
    let from_ds = control & 0x0200 != 0;
    let protected = control & 0x4000 != 0;
    let addr1_is_client = bytes.get(4..10) == Some(&client);
    let addr2_is_peer = bytes.get(10..16) == Some(&peer);
    let addr3_is_bssid = bytes.get(16..22) == Some(&peer);

    let mut body_offset = 24usize;
    if to_ds && from_ds {
        body_offset += 6;
    }
    let qos = subtype & 8 != 0;
    let amsdu = if qos {
        let value = bytes.get(body_offset..body_offset + 2);
        body_offset += 2;
        value.is_some_and(|value| value[0] & 0x80 != 0)
    } else {
        false
    };
    // Pinned Fuchsia `DataFrame::parse_frame_type_unchecked` consumes HT
    // control whenever FrameControl::htc_order() is set, after Addr4/QoS.
    if control & 0x8000 != 0 {
        body_offset += 4;
    }
    let (llc_result, snap_present, ether_type) = if frame_type != 2 {
        ("not_data", false, None)
    } else if amsdu {
        ("amsdu", false, None)
    } else if bytes.len() < body_offset {
        ("header_truncated", false, None)
    } else if bytes.len() < body_offset + 8 {
        ("llc_truncated", false, None)
    } else {
        let snap = bytes.get(body_offset..body_offset + 6) == Some(&[0xaa, 0xaa, 3, 0, 0, 0]);
        let ether_type = Some(u16::from_be_bytes([
            bytes[body_offset + 6],
            bytes[body_offset + 7],
        ]));
        (if snap { "valid" } else { "non_snap" }, snap, ether_type)
    };
    ClientDataFrameClassification {
        frame_type,
        subtype,
        to_ds,
        from_ds,
        protected,
        addr1_is_client,
        addr2_is_peer,
        addr3_is_bssid,
        snap_present,
        ether_type,
        llc_result,
    }
}

#[cfg(feature = "fuchsia-passive")]
struct LiveClientEffects {
    state: Arc<Mutex<LiveClientState>>,
    target: [u8; 6],
    client: [u8; 6],
    rcpi: u8,
    firmware: ClientFirmwareEffectsState,
    post_association_data_wait: Option<Instant>,
    eapol_start_deadline: Option<(Instant, u64)>,
    eapol_start_emitted: bool,
}

#[cfg(feature = "fuchsia-passive")]
const EAPOL_START_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

#[cfg(feature = "fuchsia-passive")]
fn eapol_start_frame(client: [u8; 6], peer: [u8; 6], qos: bool) -> Vec<u8> {
    let mut frame = vec![if qos { 0x88 } else { 0x08 }, 0x01, 0, 0];
    frame.extend_from_slice(&peer);
    frame.extend_from_slice(&client);
    frame.extend_from_slice(&[0x01, 0x80, 0xc2, 0x00, 0x00, 0x03]);
    frame.extend_from_slice(&[0, 0]);
    if qos {
        // Linux/mac80211 maps the control-port packet to voice priority 7;
        // the QoS control field is part of the 26-byte 802.11 header.
        frame.extend_from_slice(&[7, 0]);
    }
    frame.extend_from_slice(&[0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
    // The pinned EAPOL stack's IEEE802DOT1X2001 version, Start type, and an
    // empty packet body (IEEE 802.1X).
    frame.extend_from_slice(&[1, 1, 0, 0]);
    frame
}

#[cfg(feature = "fuchsia-passive")]
fn is_authenticator_m1(bytes: &[u8]) -> bool {
    let Some(body_offset) = bytes
        .windows(8)
        .position(|window| window == [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e])
        .map(|offset| offset + 8)
    else {
        return false;
    };
    let Some(eapol) = bytes.get(body_offset..) else {
        return false;
    };
    if eapol.len() < 9 || eapol[1] != 3 {
        return false;
    }
    let packet_body_len = usize::from(u16::from_be_bytes([eapol[2], eapol[3]]));
    if packet_body_len < 95 || eapol.len() < 4 + packet_body_len {
        return false;
    }
    let key_info = u16::from_be_bytes([eapol[5], eapol[6]]);
    key_info & 0x0008 != 0 && key_info & 0x0080 != 0 && key_info & 0x0100 == 0
}

#[cfg(feature = "fuchsia-passive")]
impl Mt7921ClientEffects for LiveClientEffects {
    fn prepare_runtime_handoff(
        &mut self,
    ) -> mt7921_softmac_adapter::client_device::ClientRuntimeScanState {
        self.state.lock().unwrap().channel.revoke_authorization();
        mt7921_softmac_adapter::client_device::ClientRuntimeScanState::ExternalSelection
    }

    fn revoke_scan(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.selection.invalidate();
        state.channel.revoke_authorization();
    }
    fn revoke_lifecycle(&mut self) {
        *self.state.lock().unwrap() = LiveClientState::default();
        // Device reset/stop owns transport containment. Do not issue new DMA
        // after lifecycle revocation; forget only after the owner contained it.
        self.firmware = ClientFirmwareEffectsState::default();
        self.post_association_data_wait = None;
        self.eapol_start_deadline = None;
        self.eapol_start_emitted = false;
    }
    fn ensure_channel(
        &self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: fidl_ieee80211::ChannelNumber,
    ) -> Result<ClientChannelEnsure, zx::Status> {
        let requested = client_physical_channel(primary, bandwidth, secondary)?;
        let state = self.state.lock().unwrap();
        let ensure = state.channel.ensure_channel(requested);
        record_sae_stage(&format!(
            "channel_context_ensure result={} requested_band={} requested_primary={} protocol_width={bandwidth:?} secondary80={} current={:?}",
            if matches!(ensure, ClientPhysicalChannelEnsure::Current(_)) {
                "current"
            } else {
                "transition_required"
            },
            requested.band,
            requested.primary,
            secondary.number,
            match ensure {
                ClientPhysicalChannelEnsure::Current(current) => Some(current),
                ClientPhysicalChannelEnsure::TransitionRequired { current, .. } => current,
            }
        ));
        Ok(match ensure {
            ClientPhysicalChannelEnsure::Current(_) => ClientChannelEnsure::Current,
            ClientPhysicalChannelEnsure::TransitionRequired { .. } => {
                ClientChannelEnsure::TransitionRequired
            }
        })
    }
    fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        let physical = client_physical_channel(primary, bandwidth, secondary)?;
        let mut state = self.state.lock().unwrap();
        state.selection.channel_changed(physical);
        state
            .channel
            .establish_channel(physical)
            .map_err(|_| zx::Status::NO_RESOURCES)?;
        Ok(())
    }
    fn join_bss(&mut self, request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        let bssid = request.bssid.ok_or(zx::Status::INVALID_ARGS)?;
        if bssid != self.target
            || request.bss_type != Some(fidl_ieee80211::BssType::Infrastructure)
            || request.remote != Some(true)
        {
            return Err(zx::Status::INVALID_ARGS);
        }
        let state = self.state.lock().unwrap();
        let channel = state
            .channel
            .authorized_channel()
            .map_err(|_| zx::Status::BAD_STATE)?;
        if !state.selection.permits_join(bssid, channel) {
            return Err(zx::Status::ACCESS_DENIED);
        }
        drop(state);
        self.firmware
            .bind_join(
                bssid,
                channel,
                request.beacon_period.ok_or(zx::Status::INVALID_ARGS)?,
            )
            .map_err(|_| zx::Status::BAD_STATE)
    }
    fn send_wlan_frame(
        &mut self,
        bytes: &[u8],
        flags: fidl_softmac::WlanTxInfoFlags,
        io: &mut dyn mt7921_softmac_adapter::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        let control = bytes
            .get(..2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .ok_or(zx::Status::INVALID_ARGS)?;
        let management = control & 0x000c == 0;
        let sae = control & 0x00fc == 0x00b0;
        let state = self.state.lock().unwrap();
        let channel = state.channel.authorized_channel();
        if channel.is_err()
            || bytes.get(4..10) != Some(&self.target)
            || bytes.get(10..16) != Some(&self.client)
            || (sae && bytes.get(16..22) != Some(&self.target))
        {
            return Err(zx::Status::ACCESS_DENIED);
        }
        if management {
            if (sae && bytes.get(24..26) != Some(&[3, 0]))
                || flags.contains(fidl_softmac::WlanTxInfoFlags::PROTECTED)
            {
                return Err(zx::Status::ACCESS_DENIED);
            }
            if sae && bytes.get(26..28) == Some(&[1, 0]) {
                record_sae_commit_structure(bytes).map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
            }
            if control & 0x00fc == 0 {
                let capability = bytes
                    .get(24..26)
                    .map(|field| u16::from_le_bytes([field[0], field[1]]));
                let listen_interval = bytes
                    .get(26..28)
                    .map(|field| u16::from_le_bytes([field[0], field[1]]));
                let sequence = bytes
                    .get(22..24)
                    .map(|field| u16::from_le_bytes([field[0], field[1]]) >> 4);
                record_sae_stage(&format!(
                    "association_request_structure capability={} listen_interval={} retry={} sequence={} ie_id_lengths={} fixed_fields_complete={}",
                    capability
                        .map_or_else(|| "unknown".to_string(), |value| format!("0x{value:04x}")),
                    listen_interval
                        .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                    control & 0x0800 != 0,
                    sequence.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                    management_ie_id_lengths(bytes, 28),
                    capability.is_some() && listen_interval.is_some() && sequence.is_some(),
                ));
            }
            drop(state);
            if sae && self.firmware.preauth_peer.is_none() {
                self.firmware
                    .prepare_preauth_peer(
                        LegacyWmeAssociation {
                            bss_index: 0,
                            peer_wcid: 7,
                            aid: 0,
                            peer: self.target,
                            rcpi: self.rcpi,
                            negotiated_qos: false,
                            mfp_required: false,
                        },
                        channel.expect("authorized channel was checked"),
                        |cid, command| {
                            io.submit_uni(cid, command)
                                .map_err(|status| status.to_string())
                        },
                    )
                    .map_err(|_| zx::Status::IO)?;
                record_sae_stage(
                    "firmware_wcid_stage stage=preauth peer_wcid=7 sta_state=none aid=0 peer_identity=true keys=false port_open=false",
                );
            }
            io.transmit_client(bytes, flags)?;
            if sae {
                record_sae_stage(match bytes.get(26..28) {
                    Some([1, 0]) => "sae_commit_tx_acked",
                    Some([2, 0]) => "sae_confirm_tx_acked",
                    _ => "sae_protocol_tx_acked",
                });
            }
            return Ok(());
        }
        drop(state);
        let eapol = bytes
            .windows(8)
            .any(|window| window == [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
        self.firmware
            .tx_generation(eapol)
            .map_err(|_| zx::Status::ACCESS_DENIED)?;
        let association = self
            .firmware
            .association
            .filter(|association| association.peer_wcid == 7 && association.peer == self.target)
            .ok_or(zx::Status::BAD_STATE)?;
        let to_ds = control & 0x0100 != 0;
        let from_ds = control & 0x0200 != 0;
        let qos = (control >> 4) & 8 != 0;
        let qos_offset = if to_ds && from_ds { 30 } else { 24 };
        let tid = if qos {
            bytes
                .get(qos_offset)
                .map(|value| value & 15)
                .ok_or(zx::Status::INVALID_ARGS)?
        } else {
            0
        };
        if eapol && qos != association.negotiated_qos {
            return Err(zx::Status::BAD_STATE);
        }
        if qos && !self.firmware.qos_tx_ready() {
            record_sae_stage("client_data_tx_blocked reason=edca_not_programmed");
            return Err(zx::Status::BAD_STATE);
        }
        record_sae_stage(&format!(
            "client_data_tx_public fc=0x{control:04x} protected={} to_ds={to_ds} qos={qos} tid={tid} frame_len={} wcid=7 qidx={} rate={} addr1_is_bssid={} addr2_is_sta={} addr3_is_pae_group={} ack_ra_unicast={} sequence_owner=hardware fcs_owner=hardware",
            control & 0x4000 != 0,
            bytes.len(),
            if eapol { 3 } else { 1 },
            if eapol { "ofdm6" } else { "auto" },
            bytes.get(4..10) == Some(&self.target),
            bytes.get(10..16) == Some(&self.client),
            bytes.get(16..22) == Some(&[0x01, 0x80, 0xc2, 0, 0, 3]),
            bytes.get(4).is_some_and(|byte| byte & 1 == 0),
        ));
        if !eapol && !flags.contains(fidl_softmac::WlanTxInfoFlags::PROTECTED) {
            return Err(zx::Status::ACCESS_DENIED);
        }
        io.transmit_client(bytes, flags)
    }
    fn install_key(
        &mut self,
        configuration: &fidl_softmac::WlanKeyConfiguration,
        io: &mut dyn mt7921_softmac_adapter::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        if configuration.protection != Some(fidl_softmac::WlanProtection::RxTx)
            || configuration.cipher_oui != Some([0, 15, 172])
        {
            return Err(zx::Status::INVALID_ARGS);
        }
        let association = self.firmware.association.ok_or(zx::Status::BAD_STATE)?;
        let key = configuration
            .key
            .as_deref()
            .ok_or(zx::Status::INVALID_ARGS)?;
        let key_id = configuration.key_idx.ok_or(zx::Status::INVALID_ARGS)?;
        let key_type = configuration.key_type.ok_or(zx::Status::INVALID_ARGS)?;
        let result = match key_type {
            fidl_ieee80211::KeyType::Pairwise
                if configuration.peer_addr == Some(association.peer)
                    && key_id == 0
                    && configuration.cipher_type == Some(4) =>
            {
                self.firmware
                    .install_ptk(key, configuration.rsc.unwrap_or(0), |cid, command| {
                        io.submit_uni(cid, command)
                            .map_err(|status| status.to_string())
                    })
            }
            fidl_ieee80211::KeyType::Group
                if configuration.peer_addr == Some([0xff; 6])
                    && (1..=3).contains(&key_id)
                    && configuration.cipher_type == Some(4) =>
            {
                self.firmware.install_gtk(
                    key_id,
                    key,
                    configuration.rsc.unwrap_or(0),
                    |cid, command| {
                        io.submit_uni(cid, command)
                            .map_err(|status| status.to_string())
                    },
                )
            }
            fidl_ieee80211::KeyType::Igtk
                if configuration.peer_addr == Some([0xff; 6])
                    && (4..=5).contains(&key_id)
                    && configuration.cipher_type == Some(6) =>
            {
                self.firmware.install_igtk(key_id, key, |cid, command| {
                    io.submit_uni(cid, command)
                        .map_err(|status| status.to_string())
                })
            }
            _ => return Err(zx::Status::INVALID_ARGS),
        };
        result.map_err(|_| zx::Status::IO)?;
        self.eapol_start_deadline = None;
        record_sae_stage(match key_type {
            fidl_ieee80211::KeyType::Pairwise => "traffic_key_ptk_installed=true",
            fidl_ieee80211::KeyType::Group => "traffic_key_gtk_installed=true",
            fidl_ieee80211::KeyType::Igtk => "traffic_key_igtk_installed=true",
            _ => "traffic_key_installed=true",
        });
        Ok(())
    }
    fn notify_association_complete(
        &mut self,
        configuration: &fidl_softmac::WlanAssociationConfig,
        io: &mut dyn mt7921_softmac_adapter::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        let Some(peer) = configuration.bssid else {
            record_sae_stage("association_config_validation result=invalid clause=missing_bssid");
            return Err(zx::Status::INVALID_ARGS);
        };
        let Some(aid) = configuration.aid else {
            record_sae_stage("association_config_validation result=invalid clause=missing_aid");
            return Err(zx::Status::INVALID_ARGS);
        };
        // Client MLME masks the five reserved on-wire AID bits before this
        // FIDL boundary. Requiring their raw 0xc000 form here rejects every
        // successful infrastructure association before firmware activation.
        if !(1..=2007).contains(&aid) {
            record_sae_stage(&format!(
                "association_config_validation result=invalid clause=normalized_aid_range aid={aid}"
            ));
            return Err(zx::Status::INVALID_ARGS);
        }
        if peer != self.target {
            record_sae_stage("association_config_validation result=denied clause=foreign_bssid");
            return Err(zx::Status::ACCESS_DENIED);
        }
        let negotiated_qos = configuration.qos.unwrap_or(false);
        if negotiated_qos != configuration.wmm_params.is_some() {
            record_sae_stage(
                "association_config_validation result=invalid clause=wmm_negotiation",
            );
            return Err(zx::Status::INVALID_ARGS);
        }
        record_sae_stage(&format!(
            "association_config_validation result=pass bssid_match=true normalized_aid={aid} keys=false port_open=false protected_management=closed"
        ));
        let channel = self
            .state
            .lock()
            .unwrap()
            .channel
            .authorized_channel()
            .map_err(|_| zx::Status::BAD_STATE)?;
        self.firmware
            .associate(
                LegacyWmeAssociation {
                    bss_index: 0,
                    peer_wcid: 7,
                    aid,
                    peer,
                    rcpi: self.rcpi,
                    negotiated_qos,
                    // This FIDL association seam does not carry RSN MFP
                    // negotiation. IGTK installation remains supported but
                    // cannot become a mandatory readiness predicate here.
                    mfp_required: false,
                },
                channel,
                |cid, command| {
                    io.submit_uni(cid, command)
                        .map_err(|status| status.to_string())
                },
            )
            .map_err(|_| zx::Status::IO)?;
        if let Some(wmm) = configuration.wmm_params {
            let convert = |ac: fidl_driver::WlanWmmAccessCategoryParameters| {
                if ac.ecw_min > 14 || ac.ecw_max > 14 || ac.ecw_max < ac.ecw_min {
                    return Err(zx::Status::INVALID_ARGS);
                }
                Ok(ClientEdcaAc {
                    cw_min: (1u16 << ac.ecw_min) - 1,
                    cw_max: (1u16 << ac.ecw_max) - 1,
                    txop: ac.txop_limit,
                    aifs: u16::from(ac.aifsn),
                    acm: ac.acm,
                })
            };
            let params = ClientEdcaParameters {
                ac: [
                    convert(wmm.ac_vo_params)?,
                    convert(wmm.ac_vi_params)?,
                    convert(wmm.ac_be_params)?,
                    convert(wmm.ac_bk_params)?,
                ],
            };
            if let Err(error) = self
                .firmware
                .program_edca(params, |command| {
                    io.submit_edca(command).map_err(|status| status.to_string())
                })
            {
                record_sae_stage(&format!("wmm_edca_program result=error reason={error}"));
                let _ = self.firmware.teardown(|cid, command| {
                    io.submit_uni(cid, command).map_err(|status| status.to_string())
                });
                return Err(zx::Status::IO);
            }
            record_sae_stage(&format!(
                "wmm_edca_program result=complete completion=true readback=transport_owned bss=0 wmm=0 ac_vo=aifs{},cwmin{},cwmax{},txop{},acm{} ac_vi=aifs{},cwmin{},cwmax{},txop{},acm{} ac_be=aifs{},cwmin{},cwmax{},txop{},acm{} ac_bk=aifs{},cwmin{},cwmax{},txop{},acm{} tid7_ac=vo qidx3_programmed=true data_ring=0 shared_with_management=true",
                params.ac[0].aifs, params.ac[0].cw_min, params.ac[0].cw_max, params.ac[0].txop, params.ac[0].acm,
                params.ac[1].aifs, params.ac[1].cw_min, params.ac[1].cw_max, params.ac[1].txop, params.ac[1].acm,
                params.ac[2].aifs, params.ac[2].cw_min, params.ac[2].cw_max, params.ac[2].txop, params.ac[2].acm,
                params.ac[3].aifs, params.ac[3].cw_min, params.ac[3].cw_max, params.ac[3].txop, params.ac[3].acm,
            ));
        }
        let generation = self
            .firmware
            .association_generation
            .expect("successful association publishes its generation");
        self.post_association_data_wait = Some(Instant::now());
        self.eapol_start_deadline = Some((Instant::now() + EAPOL_START_WAIT, generation));
        self.eapol_start_emitted = false;
        record_sae_stage(&format!(
            "firmware_wcid_stage stage=associated peer_wcid=7 sta_state=assoc normalized_aid={aid} peer_identity=true keys=false port_open=false protected_management=closed"
        ));
        record_sae_stage(&format!(
            "association_data_rx_activation bss_active=true bss_idx=0 bmc_wcid=19 peer_wcid=7 wtbl_state=assoc no_rx_trans=true association_generation={generation} controlled_port_open=false eapol_ready=true"
        ));
        record_sae_stage(
            "association_firmware_configured=true eapol_start_emitted=false supplicant_wait=authenticator_m1",
        );
        Ok(())
    }
    fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
        io: &mut dyn mt7921_softmac_adapter::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        let association = self.firmware.association.ok_or(zx::Status::BAD_STATE)?;
        if request.peer_addr != Some(association.peer) {
            return Err(zx::Status::INVALID_ARGS);
        }
        self.firmware
            .teardown(|cid, command| {
                io.submit_uni(cid, command)
                    .map_err(|status| status.to_string())
            })
            .map_err(|_| zx::Status::IO)?;
        self.post_association_data_wait = None;
        self.eapol_start_deadline = None;
        self.eapol_start_emitted = false;
        self.revoke_scan();
        Ok(())
    }
    fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status> {
        self.firmware
            .set_controlled_port(up)
            .map_err(|_| zx::Status::BAD_STATE)?;
        if up {
            self.eapol_start_deadline = None;
        }
        record_sae_stage(if up {
            "controlled_port_open=true"
        } else {
            "controlled_port_open=false"
        });
        Ok(())
    }
    fn next_rx(
        &mut self,
        io: &mut dyn mt7921_softmac_adapter::client_device::Mt7921ClientIo,
    ) -> Result<Option<ClientRxFrame>, zx::Status> {
        let Some(mut frame) = io.next_client_rx()? else {
            if let Some((deadline, generation)) = self.eapol_start_deadline {
                if self.firmware.association_generation != Some(generation)
                    || self.firmware.association.is_none()
                    || self.firmware.ptk_installed
                {
                    self.eapol_start_deadline = None;
                    record_sae_stage(
                        "eapol_liveness type=start timer=cancelled one_shot=suppressed",
                    );
                } else if Instant::now() >= deadline {
                    // Consume the one-shot before entering the synchronous TX
                    // path. A failed completion must not turn this into a
                    // retrying fallback.
                    self.eapol_start_deadline = None;
                    self.eapol_start_emitted = true;
                    record_sae_stage("eapol_liveness type=start timer=expired one_shot=committed");
                    let qos = self
                        .firmware
                        .association
                        .expect("generation-checked association")
                        .negotiated_qos;
                    let start = eapol_start_frame(self.client, self.target, qos);
                    self.send_wlan_frame(&start, fidl_softmac::WlanTxInfoFlags::empty(), io)?;
                    record_sae_stage("eapol_liveness type=start timer=expired one_shot=completed");
                }
            }
            if let Some(started) = self
                .post_association_data_wait
                .filter(|started| started.elapsed() >= std::time::Duration::from_secs(1))
            {
                record_sae_stage(&format!(
                    "post_association_first_data result=deadline elapsed_ms={} data_candidate=false",
                    started.elapsed().as_millis()
                ));
                self.post_association_data_wait = None;
            }
            return Ok(None);
        };
        let control = frame
            .bytes
            .get(..2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .ok_or(zx::Status::IO_DATA_INTEGRITY)?;
        let authentication = control & 0x00fc == 0x00b0;
        if authentication {
            let admitted = {
                let state = self.state.lock().unwrap();
                state
                    .channel
                    .authorized_channel()
                    .ok()
                    .filter(|channel| {
                        state.selection.permits_join(self.target, *channel)
                            && channel.channel.band
                                == match frame.status.primary.band {
                                    WlanBand::TwoGhz => 0,
                                    WlanBand::FiveGhz => 1,
                                    _ => u8::MAX,
                                }
                            && channel.channel.primary == u16::from(frame.status.primary.number)
                    })
                    .is_some()
            };
            if !admitted {
                record_sae_stage("client_rx_filtered reason=stale_or_wrong_channel subtype=auth");
                return Ok(None);
            }
            match classify_preassociation_sae_auth(&frame.bytes, self.client, self.target) {
                Ok(Some(auth)) => record_sae_stage(&format!(
                    "client_rx_admitted subtype=auth transaction={} status={} address_match=true",
                    auth.transaction, auth.status
                )),
                Ok(None) => unreachable!("authentication subtype was checked"),
                Err(reason) => {
                    record_sae_stage(&format!(
                        "client_rx_filtered reason={reason} subtype=auth address_match=false"
                    ));
                    return Ok(None);
                }
            }
        } else if control & 0x000c == 0 {
            let subtype = ((control >> 4) & 15) as u8;
            let protected = control & 0x4000 != 0;
            if protected && matches!(subtype, 10 | 12) {
                let association_generation = self.firmware.association_generation;
                let pmf = self
                    .firmware
                    .association
                    .is_some_and(|association| association.mfp_required);
                let key_current = self.firmware.ptk_installed
                    && self.firmware.ptk_rx_pn.is_some()
                    && !self.firmware.firmware_uncertain
                    && association_generation.is_some();
                let decrypted = frame.security.is_some_and(|security| {
                    security.security_mode == 4
                        && !security.cm
                        && !security.clm
                        && !security.icv_error
                        && !security.mic_error
                        && !security.fcs_error
                        && security.pn.is_some()
                });
                record_sae_stage(&format!(
                    "protected_management_candidate subtype={subtype} fc_protected=true rx_security={} decrypted={decrypted} key_current={key_current} association_generation={} pmf={pmf}",
                    frame.security.is_some(),
                    association_generation
                        .map_or_else(|| "none".to_string(), |value| value.to_string()),
                ));
                let admitted = frame.security.and_then(|security| {
                    association_generation.map(|generation| ClientRxCandidate {
                        generation: ClientDataGeneration::Association(generation),
                        eapol: false,
                        wcid: security.wcid,
                        tid: security.tid,
                        group: frame.bytes.get(4).is_some_and(|byte| byte & 1 != 0),
                        key_id: security.key_id,
                        security_mode: security.security_mode,
                        cm: security.cm,
                        clm: security.clm,
                        icv_error: security.icv_error,
                        mic_error: security.mic_error,
                        fcs_error: security.fcs_error,
                        pn: security.pn.unwrap_or([0; 6]),
                    })
                });
                if !decrypted
                    || admitted.is_none()
                    || self
                        .firmware
                        .deliver_protected_management_rx(admitted.unwrap())
                        .is_err()
                {
                    record_sae_stage(&format!(
                        "client_rx_filtered reason=protected_unverified subtype={subtype}"
                    ));
                    return Ok(None);
                }
                // Connac2 leaves the CCMP header and MIC in an otherwise
                // decrypted management MPDU. Strip them only after the
                // current-key/generation and replay checks above succeed.
                strip_verified_management_ccmp(&mut frame.bytes)
                    .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
                record_sae_stage(&format!(
                    "protected_management_admitted subtype={subtype} decrypted=true key_generation_current=true"
                ));
            }
            let classification =
                classify_client_management_frame(&frame.bytes, self.client, self.target);
            let channel_generation_match = {
                let state = self.state.lock().unwrap();
                state
                    .channel
                    .authorized_channel()
                    .ok()
                    .is_some_and(|channel| {
                        self.firmware.joined.is_some_and(|joined| {
                            joined.bssid == self.target
                                && joined.channel == channel.channel.primary
                                && joined.channel_generation == channel.generation
                                && channel.channel.band
                                    == match frame.status.primary.band {
                                        WlanBand::TwoGhz => 0,
                                        WlanBand::FiveGhz => 1,
                                        _ => u8::MAX,
                                    }
                                && channel.channel.primary == u16::from(frame.status.primary.number)
                        })
                    })
            };
            if classification.subtype == 1 {
                let capability = frame
                    .bytes
                    .get(24..26)
                    .map(|field| u16::from_le_bytes([field[0], field[1]]));
                let status = frame
                    .bytes
                    .get(26..28)
                    .map(|field| u16::from_le_bytes([field[0], field[1]]));
                let raw_aid = frame
                    .bytes
                    .get(28..30)
                    .map(|field| u16::from_le_bytes([field[0], field[1]]));
                let sequence = frame
                    .bytes
                    .get(22..24)
                    .map(|field| u16::from_le_bytes([field[0], field[1]]) >> 4);
                record_sae_stage(&format!(
                    "association_response_structure capability={} status={} raw_aid={} retry={} sequence={} fixed_fields_complete={}",
                    capability
                        .map_or_else(|| "unknown".to_string(), |value| format!("0x{value:04x}")),
                    status.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                    raw_aid.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                    control & 0x0800 != 0,
                    sequence.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                    capability.is_some()
                        && status.is_some()
                        && raw_aid.is_some()
                        && sequence.is_some(),
                ));
                record_sae_stage(&format!(
                    "association_response_candidate addr1_is_client={} addr2_is_peer={} addr3_is_bssid={} channel_generation_match={channel_generation_match}",
                    classification.addr1_is_client,
                    classification.addr2_is_peer,
                    classification.addr3_is_bssid,
                ));
            }
            if !channel_generation_match
                || !self
                    .firmware
                    .accepts_joined_management(&frame.bytes, self.client)
            {
                let reason = if !channel_generation_match {
                    "stale_or_wrong_channel"
                } else if !classification.addr1_is_client {
                    "foreign_receiver"
                } else {
                    "foreign_bss"
                };
                if classification.subtype == 1 {
                    record_sae_stage(&format!(
                        "association_response_drop subreason={reason} channel_generation_match={channel_generation_match}"
                    ));
                }
                record_sae_stage(&format!(
                    "client_rx_filtered reason={reason} subtype={}",
                    classification.subtype
                ));
                return Ok(None);
            }
            if classification.subtype == 1 {
                record_sae_stage(
                    "association_response_admitted address_match=true channel_generation_match=true",
                );
                record_sae_stage(&format!(
                    "association_response_ies ie_id_lengths={}",
                    management_ie_id_lengths(&frame.bytes, 30)
                ));
                let status = frame
                    .bytes
                    .get(26..28)
                    .map(|field| u16::from_le_bytes([field[0], field[1]]));
                let comeback = association_comeback_interval(&frame.bytes, 30);
                if let Some((tu, ms)) = comeback {
                    record_sae_stage(&format!(
                        "association_comeback advertised=true valid=true tu={tu} ms={ms}"
                    ));
                } else if status == Some(30) {
                    record_sae_stage(
                        "association_comeback advertised=unknown valid=false tu=unknown ms=unknown",
                    );
                }
                record_sae_stage(&match status {
                    Some(0) => "mlme_association_disposition result=success status=0 retry_supported=true".to_string(),
                    Some(30) if comeback.is_some() => "mlme_association_disposition result=comeback status=30 retry_supported=true".to_string(),
                    Some(status) => format!(
                        "mlme_association_disposition result=failure status={status} retry_supported=false"
                    ),
                    None => "mlme_association_disposition result=malformed status=unknown retry_supported=true".to_string(),
                });
            }
            if matches!(classification.subtype, 10 | 12) {
                self.eapol_start_deadline = None;
            }
        }
        if control & 0x000c == 0x0008 {
            if let Some(started) = self.post_association_data_wait.take() {
                record_sae_stage(&format!(
                    "post_association_first_data result=observed elapsed_ms={} data_candidate=true",
                    started.elapsed().as_millis()
                ));
            }
            let classification = classify_client_data_frame(&frame.bytes, self.client, self.target);
            let security = frame.security.ok_or(zx::Status::IO_DATA_INTEGRITY)?;
            let eapol = classification.snap_present
                && classification.ether_type == Some(0x888e)
                && classification.llc_result == "valid";
            let generation = self.firmware.tx_generation(eapol);
            let association_generation_match =
                self.firmware
                    .association_generation
                    .is_some_and(|expected| {
                        generation == Ok(ClientDataGeneration::Association(expected))
                    });
            record_sae_stage(&format!(
                "client_data_candidate frame_type={} subtype={} to_ds={} from_ds={} protected={} addr1_is_client={} addr2_is_peer={} addr3_is_bssid={} snap_present={} ether_type={} llc_result={} wcid={} association_generation_match={association_generation_match}",
                classification.frame_type,
                classification.subtype,
                classification.to_ds,
                classification.from_ds,
                classification.protected,
                classification.addr1_is_client,
                classification.addr2_is_peer,
                classification.addr3_is_bssid,
                classification.snap_present,
                classification.ether_type.map_or(0, u16::from),
                classification.llc_result,
                security.wcid,
            ));
            record_sae_stage(&format!(
                "firmware_rx_lookup observed_wcid={} peer_wcid=7 lookup_match={} firmware_wcid_stage={} association_generation_match={association_generation_match}",
                security.wcid,
                security.wcid == 7,
                if self.firmware.association.is_some() {
                    "associated"
                } else if self.firmware.preauth_peer.is_some() {
                    "preauth"
                } else {
                    "absent"
                },
            ));
            let drop = |subreason| {
                record_sae_stage(&format!(
                    "client_data_drop subreason={subreason} wcid={} association_generation_match={association_generation_match}",
                    security.wcid
                ));
            };
            if self.firmware.association.is_none() {
                drop("no_association");
                return Ok(None);
            }
            if classification.to_ds || !classification.from_ds {
                drop("direction_not_ap_to_sta");
                return Ok(None);
            }
            if !classification.addr1_is_client {
                drop("foreign_receiver");
                return Ok(None);
            }
            if !classification.addr2_is_peer {
                drop("foreign_transmitter");
                return Ok(None);
            }
            let current_channel = {
                let state = self.state.lock().unwrap();
                state.channel.authorized_channel().is_ok_and(|channel| {
                    channel.channel.band
                        == match frame.status.primary.band {
                            WlanBand::TwoGhz => 0,
                            WlanBand::FiveGhz => 1,
                            _ => u8::MAX,
                        }
                        && channel.channel.primary == u16::from(frame.status.primary.number)
                })
            };
            if !current_channel {
                drop("wrong_channel");
                return Ok(None);
            }
            if security.wcid == 1023 && classification.llc_result != "valid" {
                drop(match classification.llc_result {
                    "llc_truncated" | "header_truncated" => "malformed_llc",
                    "amsdu" => "amsdu_unicast_search_miss",
                    _ => "non_snap_sentinel",
                });
                return Ok(None);
            }
            if security.wcid == 1023 && !eapol {
                drop("non_eapol_sentinel");
                return Ok(None);
            }
            if security.wcid == 1023 && !classification.addr3_is_bssid {
                drop("foreign_bssid_sentinel");
                return Ok(None);
            }
            let generation = generation.map_err(|_| {
                drop("security_generation_gate");
                zx::Status::ACCESS_DENIED
            })?;
            self.firmware
                .deliver_rx(ClientRxCandidate {
                    generation,
                    eapol,
                    wcid: security.wcid,
                    tid: security.tid,
                    group: frame.bytes.get(4).is_some_and(|byte| byte & 1 != 0),
                    key_id: security.key_id,
                    security_mode: security.security_mode,
                    cm: security.cm,
                    clm: security.clm,
                    icv_error: security.icv_error,
                    mic_error: security.mic_error,
                    fcs_error: security.fcs_error,
                    pn: security.pn.unwrap_or([0; 6]),
                })
                .map_err(|_| {
                    drop("security_replay_or_integrity");
                    zx::Status::IO_DATA_INTEGRITY
                })?;
            if eapol && is_authenticator_m1(&frame.bytes) {
                self.eapol_start_deadline = None;
                record_sae_stage("eapol_liveness type=start timer=cancelled one_shot=suppressed");
            }
            record_sae_stage(&format!(
                "client_data_admitted eapol={eapol} wcid={} association_generation_match={association_generation_match}",
                security.wcid
            ));
        } else if control & 0x000c != 0 {
            return Err(zx::Status::IO_DATA_INTEGRITY);
        } else if authentication {
            let sequence = frame
                .bytes
                .get(26..28)
                .map(|value| u16::from_le_bytes([value[0], value[1]]))
                .ok_or(zx::Status::IO_DATA_INTEGRITY)?;
            let status = frame
                .bytes
                .get(28..30)
                .map(|value| u16::from_le_bytes([value[0], value[1]]))
                .ok_or(zx::Status::IO_DATA_INTEGRITY)?;
            record_sae_stage(match sequence {
                1 => "sae_peer_commit_rx",
                2 => "sae_peer_confirm_rx",
                _ => "sae_peer_protocol_rx",
            });
            println!(r#"{{"sae_peer_status":{{"sequence":{sequence},"status":{status}}}}}"#);
        }
        Ok(Some(frame))
    }
    fn begin_passive_scan(
        &mut self,
        _: u64,
        _: &[fidl_ieee80211::ChannelNumber],
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn observe_passive_scan(
        &mut self,
        _: u64,
        observation: &fuchsia_softmac_port::ScanObservation,
    ) -> Result<(), zx::Status> {
        let _ = observation;
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn complete_passive_scan(&mut self, _: u64, success: bool) -> Result<(), zx::Status> {
        let _ = success;
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn reset(&mut self) -> Result<(), zx::Status> {
        self.revoke_scan();
        if self.firmware.firmware_uncertain {
            Err(zx::Status::IO)
        } else {
            Ok(())
        }
    }
    fn stop(&mut self) -> Result<(), zx::Status> {
        self.revoke_scan();
        if self.firmware.firmware_uncertain {
            Err(zx::Status::IO)
        } else {
            Ok(())
        }
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
    mgmt_tx_outstanding: MgmtTxOutstanding,
    mgmt_txwi: &'c mut Option<DmaArena>,
    mgmt_frame: &'c mut Option<DmaArena>,
    mgmt_tx_ring: &'c mut Option<DmaArena>,
}

#[cfg(feature = "fuchsia-passive")]
impl Drop for VfioPassiveMechanics<'_, '_, '_> {
    fn drop(&mut self) {
        if !self.mgmt_tx_outstanding.is_empty() {
            self.loader.uni_terminal_poisoned = true;
            record_sae_stage(
                "management_tx_teardown outcome=contained outstanding_completion=true token_reuse=false",
            );
        }
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
    fn preserve_client_rx_during_control_wait(&mut self) -> Result<(), String> {
        self.loader.mcu.handle_irq(None)?;
        drain_data_rx_queue(
            self.loader.mcu.wfdma,
            &mut self.data,
            &mut self.loader.mcu.descriptor_provenance,
            &mut self.tx_completions,
            Some(&mut self.loader.mcu.normal_rx_frames),
        )?;
        self.retire_mgmt_tx_completions()?;
        record_sae_stage(&format!(
            "control_wait_rx_preserved backlog={} capacity={CLIENT_RX_BACKLOG_CAPACITY}",
            self.loader.mcu.normal_rx_frames.len()
        ));
        Ok(())
    }

    fn retire_mgmt_tx_completions(&mut self) -> Result<(), String> {
        self.tx_completions
            .append(&mut self.loader.mcu.tx_completions);
        for completion in self.tx_completions.drain(..) {
            if self.mgmt_tx_outstanding.observe(completion)?
                == Some(MgmtTxPublicationOutcome::Completed)
            {
                record_sae_stage("management_tx_completion outcome=completed");
            }
        }
        Ok(())
    }

    fn transmit_owned_client_frame(&mut self, frame: &[u8]) -> Result<(), String> {
        let mut ring = self
            .mgmt_tx_ring
            .take()
            .ok_or("client TX ring arena missing")?;
        let mut txwi = self.mgmt_txwi.take().ok_or("client TXWI arena missing")?;
        let mut payload = self.mgmt_frame.take().ok_or("client frame arena missing")?;
        let result = self.transmit_one_sae_auth(&mut ring, &mut txwi, &mut payload, frame);
        *self.mgmt_tx_ring = Some(ring);
        *self.mgmt_txwi = Some(txwi);
        *self.mgmt_frame = Some(payload);
        result
    }

    /// Reset only management TX ring 0 after its descriptor is CPU-owned.
    ///
    /// Linux v7.1.5 keeps `MT_WFDMA0_GLO_CFG_TX_DMA_EN` enabled for the device
    /// lifetime and clears it only in universal DMA cleanup/suspend. Ring-local
    /// reuse uses `MT_WFDMA0_RST_DTX_PTR`; it must not stop the MCU TX ring.
    fn reset_consumed_mgmt_tx_ring(&mut self) -> Result<(), String> {
        let global = self.loader.mcu.wfdma.read(0xd4208)?;
        if global & 1 == 0 {
            return Err("REBOOT REQUIRED: global WFDMA TX unexpectedly disabled".into());
        }
        let before_cidx = self.loader.mcu.wfdma.read(0xd4308)?;
        let before_didx = self.loader.mcu.wfdma.read(0xd430c)?;
        record_sae_stage(&format!(
            "management_tx_pre_submit stage=ring_local_reset before_cidx={before_cidx} before_didx={before_didx}"
        ));
        // CIDX is host-owned. DIDX is device-owned on this WFDMA generation:
        // a direct write is ignored, so reset only ring 0 through the
        // corresponding MT_WFDMA0_RST_DTX_PTR bit and wait for its device
        // index transition to become observable.
        if before_cidx != 0 || before_didx != 0 {
            self.loader.mcu.wfdma.write_active_wfdma(0xd420c, 1)?;
            self.loader.mcu.wfdma.write_active_wfdma(0xd4308, 0)?;
        }
        let deadline = Instant::now() + std::time::Duration::from_millis(10);
        let (cidx, didx) = loop {
            let indices = (
                self.loader.mcu.wfdma.read(0xd4308)?,
                self.loader.mcu.wfdma.read(0xd430c)?,
            );
            if indices == (0, 0) || Instant::now() >= deadline {
                break indices;
            }
            std::thread::sleep(std::time::Duration::from_micros(10));
        };
        record_sae_stage(&format!(
            "management_tx_pre_submit stage=reset_verify register=dtx_ptr ring_bit=0 after_cidx={cidx} after_didx={didx} result={}",
            if cidx == 0 && didx == 0 {
                "complete"
            } else {
                "timeout"
            }
        ));
        if cidx != 0 || didx != 0 {
            return Err(format!(
                "REBOOT REQUIRED: ring-0 indices remained nonzero after reset: cidx={cidx} didx={didx}"
            ));
        }
        if self.loader.mcu.wfdma.read(0xd4208)? & 1 == 0 {
            return Err("REBOOT REQUIRED: ring-0 reset disabled global WFDMA TX".into());
        }
        record_sae_stage("management_tx_pre_submit stage=global_tx result=enabled");
        record_sae_stage(&format!(
            "management_tx_ring_reclaimed cidx={cidx} didx={didx} global_tx_enabled=true uni_poisoned={}",
            self.loader.uni_terminal_poisoned
        ));
        Ok(())
    }

    fn configure_mgmt_tx_ring_for_submission(&mut self, ring: &DmaArena) -> Result<(), String> {
        let cidx = self.loader.mcu.wfdma.read(0xd4308)?;
        let didx = self.loader.mcu.wfdma.read(0xd430c)?;
        let descriptor_done = ring.read_descriptor_at(0).is_dma_done();
        let outstanding = !self.mgmt_tx_outstanding.is_empty();
        record_sae_stage(&format!(
            "management_tx_pre_submit stage=ownership cidx={cidx} didx={didx} dma_done={descriptor_done} outstanding={outstanding}"
        ));
        if outstanding {
            return Err("management TX completion is still outstanding; ring reuse blocked".into());
        }
        if (cidx != 0 || didx != 0) && (cidx != 1 || didx != 1 || !descriptor_done) {
            return Err(format!(
                "management TX ring is not safely reclaimable: cidx={cidx} didx={didx} dma_done={descriptor_done}"
            ));
        }
        self.reset_consumed_mgmt_tx_ring()?;
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
        record_sae_stage("management_tx_pre_submit stage=configuration result=complete");
        Ok(())
    }

    fn transmit_one_sae_auth(
        &mut self,
        ring: &mut DmaArena,
        txwi: &mut DmaArena,
        frame_arena: &mut DmaArena,
        frame: &[u8],
    ) -> Result<(), String> {
        self.retire_mgmt_tx_completions()?;
        record_sae_stage(&format!(
            "management_tx_pre_submit stage=completion_check result={}",
            if self.mgmt_tx_outstanding.is_empty() {
                "retired"
            } else {
                "blocked"
            }
        ));
        if !self.mgmt_tx_outstanding.is_empty() {
            return Err("management TX completion is still outstanding; ring reuse blocked".into());
        }
        if let Err(error) = self.configure_mgmt_tx_ring_for_submission(ring) {
            self.loader.uni_terminal_poisoned = true;
            return Err(format!(
                "REBOOT REQUIRED: management ring configuration failed; MCU TX blocked until universal containment: {error}"
            ));
        }
        ring.write_descriptor_at(0, DmaDescriptor::reset());
        txwi.zero_bytes(PAGE)
            .map_err(|error| format!("REBOOT REQUIRED: pre-submit TXWI wipe failed: {error}"))?;
        frame_arena
            .zero_bytes(PAGE)
            .map_err(|error| format!("REBOOT REQUIRED: pre-submit frame wipe failed: {error}"))?;
        record_sae_stage("management_tx_pre_submit stage=buffer_wipe result=complete");
        let (token, pid) = self.mgmt_tx_outstanding.reserve()?;
        record_sae_stage("management_tx_pre_submit stage=identity result=allocated");
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        let mut outcome = MgmtTxPublicationOutcome::NotPublished;
        let result = (|| -> Result<(), String> {
            frame_arena.write_bytes(frame)?;
            let control = frame
                .get(..2)
                .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                .ok_or("client TX omitted frame control")?;
            let (txwi_bytes, descriptor) = if control & 0x000c == 0 {
                let encoded =
                    encode_client_management_tx(frame, txwi.iova, frame_arena.iova, token, pid)?;
                (encoded.txwi.to_vec(), encoded.descriptor)
            } else if control & 0x000c == 0x0008 {
                let eapol = frame
                    .windows(8)
                    .any(|window| window == [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
                let qos = (control >> 4) & 8 != 0;
                let qos_offset = if control & 0x0300 == 0x0300 { 30 } else { 24 };
                let tid = if qos {
                    frame
                        .get(qos_offset)
                        .map(|value| value & 15)
                        .ok_or("QoS client data omitted QoS control")?
                } else if eapol {
                    // mac80211 control-port traffic retains voice priority
                    // even when the peer did not negotiate QoS.
                    7
                } else {
                    0
                };
                let encoded = encode_client_data_txwi(
                    frame.len(),
                    frame_arena.iova,
                    token,
                    pid,
                    eapol,
                    control & 0x4000 != 0,
                    qos,
                    tid,
                )?;
                let dwords = (0..8)
                    .map(|index| {
                        u32::from_le_bytes(encoded[index * 4..index * 4 + 4].try_into().unwrap())
                    })
                    .collect::<Vec<_>>();
                let descriptor_hash = encoded.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
                    (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
                });
                record_sae_stage(&format!(
                    "client_data_tx_descriptor txd={dwords:08x?} txp_len={} txp_token={} descriptor_hash=fnv1a64:{descriptor_hash:016x}",
                    frame.len(), token
                ));
                let descriptor = mt7921_dma_tx(
                    DmaSegment {
                        iova: txwi.iova,
                        len: encoded.len() as u16,
                    },
                    None,
                    0,
                )
                .map_err(|error| format!("encode client data descriptor: {error:?}"))?;
                (encoded.to_vec(), descriptor)
            } else {
                return Err("unsupported client TX frame type".into());
            };
            txwi.write_bytes(&txwi_bytes)?;
            ring.write_descriptor_at(0, descriptor);
            std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
            // A doorbell failure is ambiguous until DIDX and DMA_DONE jointly
            // prove that the device consumed the complete TXWI/TXP envelope.
            outcome = MgmtTxPublicationOutcome::AmbiguousOwnership;
            self.loader.mcu.wfdma.write_active_wfdma(0xd4308, 1)?;
            loop {
                let didx = self.loader.mcu.wfdma.read(0xd430c)?;
                let descriptor_done = ring.read_descriptor_at(0).is_dma_done();
                if didx == 1 && descriptor_done {
                    outcome = MgmtTxPublicationOutcome::Committed;
                    record_sae_stage(
                        "management_tx outcome=committed descriptor_consumed=true dma_done=true ownership=device",
                    );
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "SAE management TX descriptor consumption timed out; didx={didx} descriptor_done={descriptor_done}"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Ok(())
        })();

        if outcome == MgmtTxPublicationOutcome::AmbiguousOwnership {
            self.loader.uni_terminal_poisoned = true;
            return Err(format!(
                "REBOOT REQUIRED: management TX outcome={result:?}; ring-0 descriptor ownership is uncertain; MCU TX blocked until universal containment"
            ));
        }
        if outcome == MgmtTxPublicationOutcome::NotPublished {
            self.mgmt_tx_outstanding.abandon_last(token, pid);
        }
        if outcome == MgmtTxPublicationOutcome::Committed {
            result?;
            // DIDX plus DMA_DONE transfers the enqueue contract to the device,
            // but does not return the backing storage to the host.  Keep the
            // descriptor, TXWI, and frame intact until the next submission's
            // ring-local reset; TXS/TX_FREE remain correlated asynchronously.
            record_sae_stage(
                "management_tx outcome=committed next=reclaim_deferred ownership=device",
            );
            return Ok(());
        }
        ring.write_descriptor_at(0, DmaDescriptor::reset());
        txwi.zero_bytes(PAGE)
            .map_err(|error| format!("REBOOT REQUIRED: TXWI reclaim failed: {error}"))?;
        frame_arena
            .zero_bytes(PAGE)
            .map_err(|error| format!("REBOOT REQUIRED: frame reclaim failed: {error}"))?;
        result
    }

    fn receive_one_sae_auth(
        &mut self,
        client: [u8; 6],
        peer: [u8; 6],
        deadline: Instant,
    ) -> Result<ReceivedSaeAuth, String> {
        loop {
            self.loader.mcu.cancelled()?;
            self.loader.mcu.handle_irq(None)?;
            let _ = drain_data_rx_queue(
                self.loader.mcu.wfdma,
                &mut self.data,
                &mut self.loader.mcu.descriptor_provenance,
                &mut self.tx_completions,
                Some(&mut self.loader.mcu.normal_rx_frames),
            )?;
            self.retire_mgmt_tx_completions()?;
            let matching = self
                .loader
                .mcu
                .normal_rx_frames
                .iter()
                .position(|frame| {
                    parse_connac2_rx_frame(&frame.bytes)
                        .ok()
                        .and_then(|parsed| parsed.bytes.get(..30).map(<[u8]>::to_vec))
                        .is_some_and(|auth| {
                            u16::from_le_bytes([auth[0], auth[1]]) & 0x00fc == 0x00b0
                                && auth[4..10] == client
                                && auth[10..16] == peer
                                && auth[16..22] == peer
                        })
                });
            if let Some(index) = matching {
                let frame = self
                    .loader
                    .mcu
                    .normal_rx_frames
                    .remove(index)
                    .expect("matching persistent RX index exists");
                if let Some(occurrence) = frame.occurrence.as_ref() {
                    self.loader.mcu.descriptor_provenance.retire(occurrence);
                }
                let parsed = parse_connac2_rx_frame(&frame.bytes)
                    .map_err(|error| format!("parse SAE Connac2 RX envelope: {error:?}"))?;
                let bytes = parsed.bytes;
                let auth = &bytes[..30];
                let receiver: [u8; 6] = auth[4..10].try_into().expect("fixed field");
                let transmitter: [u8; 6] = auth[10..16].try_into().expect("fixed field");
                let bssid: [u8; 6] = auth[16..22].try_into().expect("fixed field");
                let algorithm = u16::from_le_bytes([auth[24], auth[25]]);
                let sequence = u16::from_le_bytes([auth[26], auth[27]]);
                let status_raw = u16::from_le_bytes([auth[28], auth[29]]);
                let fields = bytes[30..].to_vec();
                let status = fidl_ieee80211::StatusCode::from_primitive(status_raw)
                    .ok_or_else(|| format!("unknown SAE status code {status_raw}"))?;
                return Ok(ReceivedSaeAuth {
                    receiver,
                    transmitter,
                    bssid,
                    algorithm,
                    sequence,
                    status,
                    fields,
                });
            }
            if Instant::now() >= deadline {
                return Err("bounded SAE peer response timed out".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

#[cfg(feature = "fuchsia-passive")]
impl SourceExactPassiveMechanics for VfioPassiveMechanics<'_, '_, '_> {
    type Error = PhysicalPassiveError;

    fn submit_client_uni(&mut self, expected_cid: u8, encoded: &[u8]) -> Result<(), zx::Status> {
        let sta_update_wcid = (expected_cid == 3 && encoded.len() == 176)
            .then(|| encoded.get(49).copied())
            .flatten();
        let structure = sta_update_wcid.map(|wcid| {
            let peer_match = encoded[68..74] == encoded[132..138];
            let reset_and_set =
                encoded[120] == wcid && encoded[121] == 1 && encoded[122..124] == [4, 0];
            let rx_lookup = encoded[148..156] == [1, 0, 12, 0, 0, 1, 1, 1];
            let no_rx_trans = encoded[160..168] == [6, 0, 8, 0, 1, 0, 1, 0];
            (wcid, peer_match, reset_and_set, rx_lookup, no_rx_trans)
        });
        if let Some((wcid, peer_match, reset_and_set, rx_lookup, no_rx_trans)) = structure
            && !(peer_match && reset_and_set && rx_lookup && no_rx_trans)
        {
            record_sae_stage(&format!(
                "firmware_wtbl_update result=error stage=command_structure wcid={wcid} nested_generic_peer_match={peer_match} reset_and_set={reset_and_set} rx_lookup={rx_lookup} no_rx_trans={no_rx_trans}"
            ));
            return Err(zx::Status::IO_DATA_INTEGRITY);
        }
        if let Some(wcid) = sta_update_wcid {
            if let Err(error) = (PassiveMacExecutor {
                pages: self.mac_pages,
            })
            .clear_wtbl_admission_counts(wcid)
            {
                let category = if error.contains("all ones") {
                    "all_ones"
                } else if error.contains("busy") {
                    "busy_timeout"
                } else {
                    "unavailable"
                };
                record_sae_stage(&format!(
                    "firmware_wtbl_update result=error stage=admission_clear wcid={wcid} category={category}"
                ));
                return Err(zx::Status::IO);
            }
            record_sae_stage(&format!(
                "linux_sta_update_precondition wcid={wcid} admission_counts_cleared=true"
            ));
        }
        let command = self
            .loader
            .send_acknowledged_uni_command(expected_cid, encoded);
        let preserve = self.preserve_client_rx_during_control_wait();
        if command.is_err() {
            if let Some(wcid) = sta_update_wcid {
                record_sae_stage(&format!(
                    "firmware_wtbl_update result=error stage=cid3_ack wcid={wcid}"
                ));
            }
            return Err(zx::Status::IO);
        }
        preserve.map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
        // Linux's association STA add is CID 3 with this exact five-TLV
        // fixture. Observe, but never mutate, the source-owned RX state after
        // its ACK so a no-data run distinguishes filtering from ring ingress.
        if expected_cid == 3 && encoded.len() == 176 {
            let mac = PassiveMacExecutor {
                pages: self.mac_pages,
            };
            let wcid = encoded[49];
            let peer = &encoded[68..74];
            let (_, basic_peer_match, wtbl_reset_set, rx_lookup, header_translation) =
                structure.expect("176-byte CID3 structure was validated before publication");
            record_sae_stage(&format!(
                "firmware_wtbl_readback result=attempt wcid={wcid} authoritative=false"
            ));
            let peer_readback = mac.wtbl_peer_readback(wcid, peer).label();
            record_sae_stage(&format!(
                "firmware_wtbl_update result=acked wcid={wcid} basic_peer_match={basic_peer_match} nested_generic_peer_match={basic_peer_match} reset_and_set={wtbl_reset_set} rx_lookup={rx_lookup} no_rx_trans={header_translation} peer_readback={peer_readback} readback_authoritative=false"
            ));
            let rfcr = [
                mac.read(0x820e_5000),
                mac.read(0x820e_5004),
                mac.read(0x820f_5000),
                mac.read(0x820f_5004),
            ];
            let ring = [
                self.loader.mcu.wfdma.read(0xd4208),
                self.loader.mcu.wfdma.read(0xd4520),
                self.loader.mcu.wfdma.read(0xd4524),
                self.loader.mcu.wfdma.read(0xd4528),
                self.loader.mcu.wfdma.read(0xd452c),
            ];
            match (rfcr, ring) {
                (
                    [Ok(rfcr0), Ok(rfcr1), Ok(rfcr0_band1), Ok(rfcr1_band1)],
                    [Ok(glo), Ok(base), Ok(count), Ok(cidx), Ok(didx)],
                ) => record_sae_stage(&format!(
                    "association_rx_config rfcr0={rfcr0:#010x} rfcr1={rfcr1:#010x} rfcr0_band1={rfcr0_band1:#010x} rfcr1_band1={rfcr1_band1:#010x} wfdma_glo={glo:#010x} data_ring_base={base:#010x} data_ring_count={count} data_ring_cidx={cidx} data_ring_didx={didx}"
                )),
                _ => record_sae_stage("association_rx_config result=read_unavailable"),
            }
        }
        Ok(())
    }

    fn submit_client_edca(&mut self, encoded: &[u8]) -> Result<(), zx::Status> {
        self.loader.send_client_edca_bytes(encoded).map_err(|error| {
            record_sae_stage(&format!(
                "wmm_edca_program result=error completion=false reason={error}"
            ));
            zx::Status::IO
        })?;
        record_sae_stage(
            "wmm_edca_transport completion=true dma_didx_consumed=true descriptor_reclaimed=true firmware_ack=not_requested_linux",
        );
        Ok(())
    }

    fn transmit_client(
        &mut self,
        bytes: &[u8],
        flags: fidl_softmac::WlanTxInfoFlags,
    ) -> Result<(), zx::Status> {
        let protected = bytes
            .get(..2)
            .map(|control| u16::from_le_bytes([control[0], control[1]]) & 0x4000 != 0)
            .ok_or(zx::Status::INVALID_ARGS)?;
        if protected != flags.contains(fidl_softmac::WlanTxInfoFlags::PROTECTED) {
            return Err(zx::Status::INVALID_ARGS);
        }
        let transmit = self.transmit_owned_client_frame(bytes);
        let preserve = self.preserve_client_rx_during_control_wait();
        transmit.map_err(|error| {
            let category = if error.contains("completion is still outstanding") {
                "completion_outstanding"
            } else if error.contains("not safely reclaimable") {
                "ownership_not_reclaimable"
            } else if error.contains("ring-0 indices") {
                "index_verify"
            } else if error.contains("global WFDMA TX") {
                "global_tx_disabled"
            } else if error.contains("wipe") {
                "buffer_wipe"
            } else if error.contains("configuration") || error.contains("readback") {
                "ring_configuration"
            } else {
                "pre_submit_io"
            };
            record_sae_stage(&format!(
                "management_tx_pre_submit result=error category={category}"
            ));
            zx::Status::IO
        })?;
        preserve.map_err(|_| zx::Status::IO_DATA_INTEGRITY)
    }

    fn next_client_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
        record_sae_stage("next_client_rx poll=begin");
        self.loader
            .mcu
            .handle_irq(None)
            .map_err(|_| zx::Status::IO)?;
        drain_data_rx_queue(
            self.loader.mcu.wfdma,
            &mut self.data,
            &mut self.loader.mcu.descriptor_provenance,
            &mut self.tx_completions,
            Some(&mut self.loader.mcu.normal_rx_frames),
        )
        .map_err(|error| {
            record_sae_stage(&format!(
                "next_client_rx result=error stage=drain reason={error}"
            ));
            zx::Status::IO_DATA_INTEGRITY
        })?;
        self.retire_mgmt_tx_completions()
            .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
        let Some(frame) = self.loader.mcu.normal_rx_frames.pop_front() else {
            record_sae_stage("next_client_rx result=empty");
            return Ok(None);
        };
        if let Some(occurrence) = frame.occurrence.as_ref() {
            self.loader.mcu.descriptor_provenance.retire(occurrence);
        }
        let header = frame.bytes.get(..24).ok_or(zx::Status::IO_DATA_INTEGRITY)?;
        let rxd1 = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let rxd2 = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let parsed = parse_connac2_rx_frame(&frame.bytes).map_err(|error| {
            record_sae_stage(&format!(
                "next_client_rx result=error stage=envelope reason={error:?}"
            ));
            zx::Status::IO_DATA_INTEGRITY
        })?;
        let security = ClientRxSecurity {
            wcid: (rxd1 & 0x03ff) as u16,
            tid: ((rxd2 >> 16) & 0x0f) as u8,
            key_id: ((rxd1 >> 21) & 0x03) as u8,
            security_mode: ((rxd1 >> 16) & 0x1f) as u8,
            cm: rxd1 & (1 << 23) != 0,
            clm: rxd1 & (1 << 24) != 0,
            icv_error: rxd1 & (1 << 25) != 0,
            mic_error: rxd1 & (1 << 26) != 0,
            fcs_error: rxd1 & (1 << 27) != 0,
            pn: parsed.pn,
        };
        let subtype = parsed.bytes.first().map(|control| control >> 4);
        record_sae_stage(&format!(
            "next_client_rx result=frame len={} packet_type={:?} subtype={subtype:?}",
            parsed.bytes.len(),
            mt7921_packet_type(&frame.bytes)
        ));
        let primary = ChannelNumber {
            band: match parsed.band {
                mt7921_port_spike::PhysicalBand::Ghz2 => WlanBand::TwoGhz,
                mt7921_port_spike::PhysicalBand::Ghz5 => WlanBand::FiveGhz,
                mt7921_port_spike::PhysicalBand::Ghz6 => return Err(zx::Status::NOT_SUPPORTED),
            },
            number: parsed.channel,
        };
        Ok(Some(ClientRxFrame {
            bytes: parsed.bytes,
            status: fidl_softmac::WlanRxInfo {
                rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
                valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
                phy: fidl_ieee80211::WlanPhyType::Ofdm,
                data_rate: 0,
                primary,
                bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw20,
                vht_secondary_80_channel: ChannelNumber {
                    number: 0,
                    ..primary
                },
                mcs: 0,
                rssi_dbm: parsed.rssi_dbm,
                snr_dbh: 0,
            },
            security: Some(security),
        }))
    }

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
                None,
            )
            .map_err(PhysicalPassiveError)?,
        );
        self.retire_mgmt_tx_completions()
            .map_err(PhysicalPassiveError)?;
        let mut routed_frames = std::mem::take(&mut self.loader.mcu.normal_rx_frames);
        while !routed_frames.is_empty() {
            let frame = routed_frames.pop_front().expect("queue is nonempty");
            match frame.parse() {
                Ok(advertisement) => self.advertisements.push(advertisement),
                Err((frame, error)) => {
                    revoke_before_local_frame_release(
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClientVifIdentity([u8; 6]);

#[cfg(feature = "fuchsia-passive")]
impl ClientVifIdentity {
    const fn bytes(self) -> [u8; 6] {
        self.0
    }
}

#[cfg(feature = "fuchsia-passive")]
fn parse_client_mac(value: &str) -> Result<ClientVifIdentity, String> {
    let address = parse_mac(value).map_err(|_| "invalid client MAC".to_string())?;
    if address[0] & 3 != 2 {
        return Err("client MAC must be a locally administered unicast address".into());
    }
    Ok(ClientVifIdentity(address))
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

struct DmaArena {
    mapping: Option<userspace_vfio::DmaMapping>,
    #[cfg(test)]
    ptr: Option<NonNull<u8>>,
    len: usize,
    iova: u64,
}
impl DmaArena {
    fn map(iommu: &Arc<File>, ioas: u32, iova: u64) -> Result<Self, String> {
        Self::map_len(iommu, ioas, iova, PAGE)
    }
    fn map_len(iommu: &Arc<File>, ioas: u32, iova: u64, len: usize) -> Result<Self, String> {
        Ok(Self {
            mapping: Some(userspace_vfio::DmaMapping::map(
                iommu, ioas, iova, len, PAGE,
            )?),
            #[cfg(test)]
            ptr: None,
            len,
            iova,
        })
    }
    fn initialize_fwdl_descriptors(&mut self) -> Result<(), String> {
        if MT7921_FWDL_RING_BYTES > self.len {
            return Err("firmware ring exceeds DMA arena".into());
        }
        self.zero(0, self.len)?;
        for offset in (0..MT7921_FWDL_RING_BYTES).step_by(16) {
            self.write_word(offset + 4, 1 << 31)?;
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        Ok(())
    }
    fn initialize_descriptor_page(&mut self) -> Result<(), String> {
        self.zero(0, self.len)?;
        for offset in (0..self.len).step_by(16) {
            self.write_word(offset + 4, 1 << 31)?;
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
        if let Some(mapping) = self.mapping.as_mut() {
            mapping.write(offset, bytes)
        } else {
            #[cfg(test)]
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    self.ptr.unwrap().as_ptr().add(offset),
                    bytes.len(),
                );
                Ok(())
            }
            #[cfg(not(test))]
            unreachable!()
        }
    }
    fn zero_bytes(&mut self, length: usize) -> Result<(), String> {
        if length > self.len {
            return Err("DMA zero exceeds arena".into());
        }
        self.zero(0, length)
    }
    fn secure_zero_bytes(&mut self, length: usize) -> Result<(), String> {
        if length > self.len {
            return Err("DMA secure zero exceeds arena".into());
        }
        if let Some(mapping) = self.mapping.as_mut() {
            mapping.secure_zero(length)
        } else {
            #[cfg(test)]
            {
                for offset in 0..length {
                    unsafe { std::ptr::write_volatile(self.ptr.unwrap().as_ptr().add(offset), 0) }
                }
                Ok(())
            }
            #[cfg(not(test))]
            unreachable!()
        }
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
            self.write_word(offset + index * 4, word).unwrap();
        }
    }
    fn read_descriptor(&self) -> DmaDescriptor {
        self.read_descriptor_at(0)
    }
    fn read_descriptor_at(&self, descriptor_index: usize) -> DmaDescriptor {
        let offset = descriptor_index * 16;
        assert!(offset + 16 <= self.len);
        let word = |index: usize| self.read_word(offset + index * 4).unwrap();
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
        if let Some(mapping) = self.mapping.as_ref() {
            mapping.read(offset, end - offset)
        } else {
            #[cfg(test)]
            {
                let mut bytes = vec![0; end - offset];
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        self.ptr.unwrap().as_ptr().add(offset),
                        bytes.as_mut_ptr(),
                        bytes.len(),
                    );
                }
                Ok(bytes)
            }
            #[cfg(not(test))]
            unreachable!()
        }
    }
    fn teardown(&mut self) -> Result<(), String> {
        self.mapping
            .as_mut()
            .map_or(Ok(()), userspace_vfio::DmaMapping::teardown)
    }

    fn write_word(&mut self, offset: usize, value: u32) -> Result<(), String> {
        if let Some(mapping) = self.mapping.as_mut() {
            mapping.write_u32(offset, value)
        } else {
            #[cfg(test)]
            {
                unsafe {
                    std::ptr::write_volatile(
                        self.ptr.unwrap().as_ptr().add(offset).cast::<u32>(),
                        value,
                    )
                };
                Ok(())
            }
            #[cfg(not(test))]
            unreachable!()
        }
    }
    fn read_word(&self, offset: usize) -> Result<u32, String> {
        if let Some(mapping) = self.mapping.as_ref() {
            mapping.read_u32(offset)
        } else {
            #[cfg(test)]
            {
                Ok(unsafe {
                    std::ptr::read_volatile(self.ptr.unwrap().as_ptr().add(offset).cast::<u32>())
                })
            }
            #[cfg(not(test))]
            unreachable!()
        }
    }
    fn zero(&mut self, offset: usize, length: usize) -> Result<(), String> {
        if offset != 0 {
            return self.write_bytes_at(offset, &vec![0; length]);
        }
        if let Some(mapping) = self.mapping.as_mut() {
            mapping.zero(length)
        } else {
            #[cfg(test)]
            {
                unsafe { std::ptr::write_bytes(self.ptr.unwrap().as_ptr(), 0, length) };
                Ok(())
            }
            #[cfg(not(test))]
            unreachable!()
        }
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
fn disable_vfio_irq_index(device: &File, capability: PciIrqCapability) -> Result<(), String> {
    let index = match capability.kind {
        PciIrqKind::Intx => 0,
        PciIrqKind::Msi => 1,
        PciIrqKind::Msix => 2,
    };
    userspace_vfio::disable_irq(device, index)
}

fn install_vfio_irq(device: &Arc<File>, capability: PciIrqCapability) -> Result<VfioIrq, String> {
    let index = match capability.kind {
        PciIrqKind::Intx => 0,
        PciIrqKind::Msi => 1,
        PciIrqKind::Msix => 2,
    };
    VfioIrq::install(
        device,
        userspace_vfio::IrqCapability {
            index,
            count: capability.count,
            eventfd: capability.eventfd,
        },
    )
}

struct ReadPage {
    mapping: Option<userspace_vfio::RegionMapping>,
    #[cfg(test)]
    ptr: Option<NonNull<u8>>,
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
        Ok(Self {
            mapping: Some(userspace_vfio::RegionMapping::map(
                device, region, bar_page, PAGE, writable,
            )?),
            #[cfg(test)]
            ptr: None,
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
        if let Some(mapping) = self.mapping.as_ref() {
            mapping.read_u32(within)
        } else {
            #[cfg(test)]
            {
                Ok(unsafe {
                    std::ptr::read_volatile(self.ptr.unwrap().as_ptr().add(within).cast::<u32>())
                })
            }
            #[cfg(not(test))]
            unreachable!()
        }
    }
    fn write_within(&self, within: usize, value: u32) -> Result<(), String> {
        if let Some(mapping) = self.mapping.as_ref() {
            mapping.write_u32(within, value)
        } else {
            #[cfg(test)]
            {
                unsafe {
                    std::ptr::write_volatile(
                        self.ptr.unwrap().as_ptr().add(within).cast::<u32>(),
                        value,
                    )
                };
                Ok(())
            }
            #[cfg(not(test))]
            unreachable!()
        }
    }
    fn write_clear_own(&self) -> Result<(), String> {
        let offset = ReadRegister::ConnOnLowPowerControl.bar_offset();
        let within = offset - self.bar_page;
        if self.bar_page != 0xe0000 || within + 4 > PAGE {
            return Err("CLR_OWN write escaped immutable allowlist".into());
        }
        self.write_within(within, PCIE_LPCR_HOST_CLR_OWN)?;
        Ok(())
    }
    fn write_set_own(&self) -> Result<(), String> {
        let offset = ReadRegister::ConnOnLowPowerControl.bar_offset();
        let within = offset - self.bar_page;
        if self.bar_page != 0xe0000 || within + 4 > PAGE {
            return Err("SET_OWN write escaped immutable allowlist".into());
        }
        self.write_within(within, PCIE_LPCR_HOST_SET_OWN)?;
        Ok(())
    }
    fn write_remap_selector(&self, value: u32) -> Result<(), String> {
        let within = MT_HIF_REMAP_L1_BAR_OFFSET - self.bar_page;
        if self.bar_page != 0xfe000 || within + 4 > PAGE {
            return Err("remap selector write escaped immutable allowlist".into());
        }
        self.write_within(within, value)?;
        Ok(())
    }
    fn write_top_driver_own(&self) -> Result<(), String> {
        let offset = MT_HIF_REMAP_WINDOW_BAR_OFFSET + 0x10;
        let within = offset - self.bar_page;
        if self.bar_page != MT_HIF_REMAP_WINDOW_BAR_OFFSET || within + 4 > PAGE {
            return Err("MT_TOP driver-own write escaped immutable allowlist".into());
        }
        self.write_within(within, MT_TOP_LPCR_HOST_DRV_OWN)?;
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
        self.write_within(within, value)?;
        Ok(())
    }
    #[cfg(feature = "fuchsia-passive")]
    fn read_passive_mac(&self, address: u32) -> Result<u32, String> {
        if !passive_mac_read_address_allowed(address) {
            return Err(format!(
                "passive MAC read {address:#010x} escaped exact plan"
            ));
        }
        let offset = passive_mac_read_bar_offset(address)?;
        if self.bar_page != offset & !(PAGE - 1) {
            return Err(format!(
                "passive MAC read {address:#010x} used wrong fixed BAR page"
            ));
        }
        let value = self.read(offset)?;
        if passive_mac_address_allowed(address) {
            validate_passive_mac_bar_read(address, value)
                .map(|(_, value)| value)
                .map_err(|error| format!("validate passive MAC read: {error:?}"))
        } else if value == u32::MAX {
            Err(format!(
                "passive MAC read {address:#010x} returned all ones"
            ))
        } else {
            Ok(value)
        }
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
        self.write_within(within, value)?;
        Ok(())
    }
    fn write_pcie_mac_interrupt_enable_zero(&self) -> Result<(), String> {
        self.write_pcie_mac_interrupt_enable(0)
    }
    fn write_pcie_mac_interrupt_enable(&self, value: u32) -> Result<(), String> {
        if value != 0 && value != 0xff {
            return Err("PCIe MAC interrupt value escaped allowlist".into());
        }
        self.write_pcie_mac_interrupt_enable_raw(value)
    }
    fn restore_pcie_mac_interrupt_enable(&self, saved: u32) -> Result<(), String> {
        self.write_pcie_mac_interrupt_enable_raw(saved)
    }
    fn write_pcie_mac_interrupt_enable_raw(&self, value: u32) -> Result<(), String> {
        if self.bar_page != 0x10000 {
            return Err("PCIe MAC interrupt write escaped immutable allowlist".into());
        }
        let within = 0x10188 - self.bar_page;
        self.write_within(within, value)?;
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
        self.write_within(within, value)?;
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
        self.write_within(within, 0)?;
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
        self.write_within(within, value)?;
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
        self.write_within(within, value)?;
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
            self.write_within(within, value)?;
        }
        Ok(())
    }
    fn reset_all_tx_indices(&self, value: u32) -> Result<(), String> {
        if self.bar_page != 0xd4000 || value != u32::MAX {
            return Err("DTX reset escaped all-rings-only allowlist".into());
        }
        let within = 0xd420c - self.bar_page;
        self.write_within(within, value)?;
        Ok(())
    }
    fn write_fwdl_interrupt_enable(&self, value: u32) -> Result<(), String> {
        if self.bar_page != 0xd4000 || value != 0 {
            return Err("interrupt-mask write escaped zero-only allowlist".into());
        }
        let within = 0xd4204 - self.bar_page;
        self.write_within(within, value)?;
        Ok(())
    }
    fn acknowledge_fwdl_interrupt(&self, value: u32) -> Result<(), String> {
        if self.bar_page != 0xd4000 || value & !(1 << 26) != 0 {
            return Err("interrupt acknowledgement escaped FWDL-only allowlist".into());
        }
        let within = 0xd4200 - self.bar_page;
        self.write_within(within, value)?;
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
            self.write_within(within, value)?;
        }
        Ok(())
    }
    fn write_rx_cpu_index(&self, index: usize, value: u32) -> Result<(), String> {
        if self.bar_page != 0xd4000 || index >= 8 || value >= 8 {
            return Err("RX producer write escaped slot allowlist".into());
        }
        let within = 0x500 + index * 0x10 + 8;
        self.write_within(within, value)?;
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
        self.write_within(within, value)?;
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
        self.mapping
            .as_mut()
            .map_or(Ok(()), userspace_vfio::RegionMapping::teardown)?;
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
        let irq = userspace_vfio::irq_capability(device, index as u32)?;
        capabilities.push(PciIrqCapability {
            kind,
            count: irq.count,
            eventfd: irq.eventfd,
        });
    }
    Ok(capabilities)
}

fn verify_vfio_reset_supported(device: &File) -> Result<(), String> {
    userspace_vfio::reset_device_supported(device)
}

fn reset_vfio_device(device: &File) -> Result<(), String> {
    userspace_vfio::reset_device(device)
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
    fn sleep_us_range(&mut self, _minimum: u64, maximum: u64) {
        std::thread::sleep(std::time::Duration::from_micros(maximum));
    }
}
impl OwnershipRoundTripTransport for VfioOwnership<'_> {
    fn write_set_own(&mut self) -> Result<(), Self::Error> {
        self.page.write_set_own()
    }
}

fn log_ownership_event(event: OwnershipEvent) {
    match event {
        OwnershipEvent::ClearOwnBefore { attempt, at_ms } => println!(
            "{{\"ownership_event\":\"clear_own_before\",\"attempt\":{attempt},\"at_ms\":{at_ms}}}"
        ),
        OwnershipEvent::ClearOwnWritten { attempt, at_ms } => println!(
            "{{\"ownership_event\":\"clear_own_written\",\"attempt\":{attempt},\"at_ms\":{at_ms},\"value\":\"{PCIE_LPCR_HOST_CLR_OWN:#010x}\"}}"
        ),
        OwnershipEvent::AspmDelay {
            attempt,
            at_ms,
            minimum_us,
            maximum_us,
        } => println!(
            "{{\"ownership_event\":\"aspm_delay\",\"attempt\":{attempt},\"at_ms\":{at_ms},\"minimum_us\":{minimum_us},\"maximum_us\":{maximum_us}}}"
        ),
        OwnershipEvent::StatusReadBefore { attempt, at_ms } => println!(
            "{{\"ownership_event\":\"status_read_before\",\"attempt\":{attempt},\"at_ms\":{at_ms}}}"
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

#[cfg(feature = "fuchsia-passive")]
#[allow(dead_code)]
fn record_ownership_round_trip_stage(event: OwnershipRoundTripEvent) {
    match event {
        OwnershipRoundTripEvent::SnapshotReadBefore => {
            record_sae_stage("vfio_ownership_snapshot_read_before offset=0xe0010 bytes=4")
        }
        OwnershipRoundTripEvent::Snapshot { raw, state } => record_sae_stage(&format!(
            "vfio_ownership_snapshot_read_after offset=0xe0010 bytes=4 raw={raw:#010x} state={state:?}"
        )),
        OwnershipRoundTripEvent::Driver(
            event @ (OwnershipEvent::ClearOwnBefore { .. }
            | OwnershipEvent::AspmDelay { .. }
            | OwnershipEvent::AttemptExpired { .. }
            | OwnershipEvent::Acquired { .. }
            | OwnershipEvent::TimedOut { .. }),
        ) => record_sae_stage(&format!("vfio_ownership_driver_transition event={event:?}")),
        OwnershipRoundTripEvent::Firmware(
            event @ (FirmwareOwnershipEvent::SetOwnBefore { .. }
            | FirmwareOwnershipEvent::AttemptExpired { .. }
            | FirmwareOwnershipEvent::Restored { .. }
            | FirmwareOwnershipEvent::TimedOut { .. }),
        ) => record_sae_stage(&format!(
            "vfio_ownership_rollback_transition event={event:?}"
        )),
        OwnershipRoundTripEvent::Driver(_) | OwnershipRoundTripEvent::Firmware(_) => {}
        OwnershipRoundTripEvent::Complete { restored } => record_sae_stage(&format!(
            "vfio_ownership_round_trip_complete restored={restored:?}"
        )),
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
    fn uses_contained_transport_gate(self) -> bool {
        matches!(
            self,
            Self::RunOneShotFirmware | Self::RunOneShotPassiveChannel1
        )
    }

    fn records_active_transport_stages(self) -> bool {
        if self.uses_contained_transport_gate() {
            return true;
        }
        #[cfg(feature = "fuchsia-passive")]
        {
            self == Self::RunOneShotSaeAuth
        }
        #[cfg(not(feature = "fuchsia-passive"))]
        {
            false
        }
    }

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

#[cfg(feature = "fuchsia-passive")]
struct VfioIrqResetBoundary<'a> {
    wfsys: VfioWfsysReset<'a>,
    device: &'a Arc<File>,
    wfdma: &'a ReadPage,
    pcie_mac: &'a ReadPage,
    irq: Option<VfioIrq>,
    selected: PciIrqCapability,
    bdf: &'a str,
    ledger: &'a mut ContainmentLedger,
}

#[cfg(feature = "fuchsia-passive")]
impl WfsysResetTransport for VfioIrqResetBoundary<'_> {
    type Error = String;
    fn now_ms(&self) -> u64 {
        self.wfsys.now_ms()
    }
    fn read_reset_control(&mut self) -> Result<u32, Self::Error> {
        self.wfsys.read_reset_control()
    }
    fn write_reset_control(&mut self, value: u32) -> Result<(), Self::Error> {
        self.wfsys.write_reset_control(value)
    }
    fn sleep_ms(&mut self, milliseconds: u64) {
        self.wfsys.sleep_ms(milliseconds)
    }
}

#[cfg(feature = "fuchsia-passive")]
impl IrqResetTransport for VfioIrqResetBoundary<'_> {
    fn install_irq(&mut self, capability: PciIrqCapability) -> Result<(), Self::Error> {
        self.ledger.mark_possibly_active(Hazard::DeviceIrq);
        self.irq = Some(install_vfio_irq(self.device, capability)?);
        Ok(())
    }
    fn mask_host_irq(&mut self) -> Result<(), Self::Error> {
        self.wfdma.write_active_wfdma(0xd4204, 0)
    }
    fn enable_pcie_mac_irq(&mut self) -> Result<(), Self::Error> {
        self.pcie_mac.write_pcie_mac_interrupt_enable(0xff)
    }
    fn disable_pcie_mac_irq(&mut self) -> Result<(), Self::Error> {
        self.pcie_mac.write_pcie_mac_interrupt_enable_zero()
    }
    fn disable_irq(&mut self) -> Result<(), Self::Error> {
        let Some(irq) = self.irq.as_mut() else {
            return disable_vfio_irq_index(self.device, self.selected);
        };
        match irq.disable() {
            Ok(()) => Ok(()),
            Err(owner) => match disable_vfio_irq_index(self.device, self.selected) {
                Ok(()) => Err(format!("disable IRQ owner: {owner}")),
                Err(explicit) => Err(format!(
                    "disable IRQ owner: {owner}; explicit index disable: {explicit}"
                )),
            },
        }
    }
    fn containment_reset(&mut self) -> Result<(), Self::Error> {
        reset_vfio_device(self.device)
    }
    fn verify_contained(&mut self) -> Result<(), Self::Error> {
        let global = self.wfdma.read(0xd4208)?;
        let host_irq = self.wfdma.read(0xd4204)?;
        let mac_irq = self.pcie_mac.read(0x10188)?;
        record_sae_stage(&format!(
            "vfio_irq_reset_safe_state global={global:#010x} host_irq={host_irq:#010x} mac_irq={mac_irq:#010x} bme=false"
        ));
        verify_active_reset_containment(self.wfdma, self.pcie_mac)?;
        verify_pci_dma_disabled(self.bdf)?;
        for hazard in [
            Hazard::HostControl,
            Hazard::DeviceIrq,
            Hazard::Wfdma,
            Hazard::BusMaster,
            Hazard::LabMutated,
        ] {
            self.ledger.confirm_inactive(hazard);
        }
        self.ledger.phase = RunPhase::Contained;
        Ok(())
    }
}

#[cfg(feature = "fuchsia-passive")]
fn record_irq_reset_stage(event: IrqResetEvent) {
    if matches!(
        event,
        IrqResetEvent::Wfsys(WfsysResetEvent::StatusRead { .. })
    ) {
        return;
    }
    record_sae_stage(&format!("vfio_irq_reset_boundary event={event:?}"));
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
    #[derive(Default)]
    struct TestClientIo {
        uni: Vec<Vec<u8>>,
        tx: Vec<Vec<u8>>,
        rx: VecDeque<ClientRxFrame>,
        fail_uni: bool,
    }

    #[cfg(feature = "fuchsia-passive")]
    fn test_channel_lease(primary: u16) -> mt7921_port_spike::ClientChannelLease {
        mt7921_port_spike::ClientChannelLease {
            channel: ClientPhysicalChannel {
                band: 1,
                primary,
                center: primary,
                bandwidth: 0,
                center2: 0,
            },
            generation: 1,
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    fn prepare_test_preauth(
        state: &mut ClientFirmwareEffectsState,
        association: LegacyWmeAssociation,
    ) {
        state
            .prepare_preauth_peer(
                LegacyWmeAssociation {
                    aid: 0,
                    negotiated_qos: false,
                    mfp_required: false,
                    ..association
                },
                test_channel_lease(36),
                |_, _| Ok(()),
            )
            .unwrap();
    }

    #[cfg(feature = "fuchsia-passive")]
    fn selected_live_state(bssid: [u8; 6], channel: ClientPhysicalChannel) -> LiveClientState {
        LiveClientState {
            selection: ClientTargetBssLease::retain(ClientScanEvidence {
                scan_id: 7,
                observation_generation: 1,
                observation_timestamp_nanos: 1,
                bssid,
                channel,
            })
            .unwrap(),
            channel: ClientChannelContext::default(),
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    fn ready_and_authorize(
        state: &mut LiveClientState,
        bssid: [u8; 6],
        channel: ChannelNumber,
        bandwidth: ChannelBandwidth,
    ) {
        let secondary = ChannelNumber {
            number: 0,
            ..channel
        };
        state
            .mark_rate_power_ready(bssid, channel, bandwidth, secondary)
            .unwrap();
        state
            .authorize_sae(bssid, channel, bandwidth, secondary)
            .unwrap();
    }

    #[cfg(feature = "fuchsia-passive")]
    fn install_selection_and_authorize(
        state: &mut LiveClientState,
        bssid: [u8; 6],
        channel: ChannelNumber,
        bandwidth: ChannelBandwidth,
    ) {
        let physical = client_physical_channel(
            channel,
            bandwidth,
            ChannelNumber {
                number: 0,
                ..channel
            },
        )
        .unwrap();
        state.selection = selected_live_state(bssid, physical).selection;
        ready_and_authorize(state, bssid, channel, bandwidth);
    }

    #[cfg(feature = "fuchsia-passive")]
    impl mt7921_softmac_adapter::client_device::Mt7921ClientIo for TestClientIo {
        fn submit_uni(&mut self, cid: u8, bytes: &[u8]) -> Result<(), zx::Status> {
            if self.fail_uni || validate_uni_request(cid, bytes).is_err() {
                return Err(zx::Status::IO);
            }
            self.uni.push(bytes.to_vec());
            Ok(())
        }
        fn submit_edca(&mut self, bytes: &[u8]) -> Result<(), zx::Status> {
            if bytes.get(36..39) != Some(&[0x1d, 0xa0, 1]) || bytes.len() != 108 {
                return Err(zx::Status::IO_DATA_INTEGRITY);
            }
            self.uni.push(bytes.to_vec());
            Ok(())
        }
        fn transmit_client(
            &mut self,
            bytes: &[u8],
            _: fidl_softmac::WlanTxInfoFlags,
        ) -> Result<(), zx::Status> {
            self.tx.push(bytes.to_vec());
            Ok(())
        }
        fn next_client_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
            Ok(self.rx.pop_front())
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    struct FallbackMechanics {
        rx: VecDeque<ClientRxFrame>,
        pending_status77: Option<ClientRxFrame>,
        tx: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    #[cfg(feature = "fuchsia-passive")]
    impl SourceExactPassiveMechanics for FallbackMechanics {
        type Error = std::io::Error;

        fn prepare_passive_receive(&mut self) -> Result<PassivePrerequisites, Self::Error> {
            Ok(PassivePrerequisites {
                channel_domain_mask_zero: true,
                mac_mmio_initialized: true,
                data_rx_owned: true,
            })
        }

        fn command(&mut self, _: &PassiveMcuCommand, _: &[u8], _: bool) -> Result<(), Self::Error> {
            Ok(())
        }

        fn next_event(&mut self, _: i64) -> Result<Option<PassiveMechanicsEvent>, Self::Error> {
            Ok(None)
        }

        fn confirm_scan_done(&mut self, _: u8) -> Result<(), Self::Error> {
            Ok(())
        }

        fn submit_client_uni(&mut self, _: u8, _: &[u8]) -> Result<(), zx::Status> {
            Ok(())
        }

        fn transmit_client(
            &mut self,
            bytes: &[u8],
            _: fidl_softmac::WlanTxInfoFlags,
        ) -> Result<(), zx::Status> {
            self.tx.lock().unwrap().push(bytes.to_vec());
            // This synchronous fake acknowledges publication by returning Ok.
            // Make the peer response observable only after the initial
            // group-20 commit has reached that acknowledgement boundary.
            if bytes.get(28..32) == Some(&[126, 0, 20, 0]) {
                if let Some(status77) = self.pending_status77.take() {
                    self.rx.push_back(status77);
                }
            }
            Ok(())
        }

        fn next_client_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
            Ok(self.rx.pop_front())
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn sae_credential_declared_length_does_not_wait_for_eof() {
        use std::os::fd::IntoRawFd;
        use std::os::unix::net::UnixStream;

        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"eight-byte-secret").unwrap();
        let credential = read_sae_credential_exact(reader.into_raw_fd(), 8).unwrap();
        assert_eq!(credential.0, b"eight-by");
        // The peer deliberately remains open: returning proves there was no
        // read-to-EOF dependency. Remaining bytes are discarded on close.
        drop(writer);
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn live_support_satisfies_pinned_device_info_contract() {
        let capability = mt7921_port_spike::NicCapability {
            element_count: 1,
            mac_address: Some([2, 3, 4, 5, 6, 7]),
            phy: Some(mt7921_port_spike::NicPhyCapability {
                ht: true,
                vht: true,
                has_5ghz: true,
                max_bandwidth: 2,
                spatial_streams: 2,
                hardware_path: 3,
                he: true,
            }),
            has_6ghz: Some(false),
            chip_capability: None,
            unknown_elements: 0,
        };
        let candidates = candidate_channels(capability);
        let support = live_client_support(query_from_capabilities(capability, &candidates));
        let info = wlan_mlme::mlme_device_info_from_softmac(support.query).unwrap();
        assert_eq!(info.role, fidl_common::WlanMacRole::Client);
        let band = info
            .bands
            .iter()
            .find(|band| band.band == WlanBand::FiveGhz)
            .unwrap();
        assert_eq!(
            band.basic_rates,
            [0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c]
        );
        assert_eq!(band.ht_cap.as_ref().unwrap().bytes[0..3], [0xf3, 0x09, 3]);
        assert_eq!(
            band.vht_cap.as_ref().unwrap().bytes,
            [0xb2, 0x71, 0x90, 0x33, 0xfa, 0xff, 0, 0, 0xfa, 0xff, 0, 0]
        );
        assert!(info.qos_capable);
        assert_eq!(
            info.softmac_hardware_capability,
            fidl_driver::WlanSoftmacHardwareCapabilityBit::Qos as u32
        );
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn remove_wcid_matches_pinned_linux_cid3_fixture() {
        let peer = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
        let encoded = encode_remove_wcid_command(9, 0, 7, 42, peer, false).unwrap();
        assert_eq!(
            encoded,
            [
                88, 0, 0, 65, 0, 0, 1, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 56, 0, 3, 0, 0, 160, 0, 9, 0, 0, 0, 7, 0, 0, 0, 0, 0, 7, 2, 0, 1,
                0, 0, 0, 0, 0, 20, 0, 2, 0, 1, 0, 0, 0, 42, 0, 16, 32, 48, 64, 80, 96, 1, 0, 13, 0,
                12, 0, 7, 1, 0, 0, 0, 0, 0, 0,
            ]
        );
        assert_eq!(validate_uni_request(3, &encoded).unwrap(), 9);
        assert!(validate_uni_request(2, &encoded).is_err());
        let mut wrong_length = encoded.clone();
        wrong_length[32] -= 1;
        assert!(validate_uni_request(3, &wrong_length).is_err());
        let mut wrong_txd = encoded.clone();
        wrong_txd[3] ^= 4;
        assert!(validate_uni_request(3, &wrong_txd).is_err());
        let qos = encode_remove_wcid_command(9, 0, 7, 42, peer, true).unwrap();
        assert_eq!(qos[65], 1);
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn add_wcid_matches_pinned_linux_legacy_wme_fixture() {
        let encoded = encode_legacy_wme_add_wcid_command(
            9,
            0,
            7,
            42,
            [0x10, 0x20, 0x30, 0x40, 0x50, 0x60],
            100,
        )
        .unwrap();
        assert_eq!(encoded.len(), 176);
        assert_eq!(validate_uni_request(3, &encoded).unwrap(), 9);
        assert_eq!(&encoded[48..56], &[0, 7, 5, 0, 1, 0, 0, 0]);
        assert_eq!(
            &encoded[56..76],
            &[
                0, 0, 20, 0, 2, 0, 1, 0, 2, 1, 42, 0, 16, 32, 48, 64, 80, 96, 1, 0
            ]
        );
        assert_eq!(encoded[112], 2);
        assert_eq!(&encoded[116..124], &[13, 0, 60, 0, 7, 1, 4, 0]);
        assert_eq!(
            &encoded[128..148],
            &[
                0, 0, 20, 0, 16, 32, 48, 64, 80, 96, 0, 0, 0, 1, 0, 0, 42, 0, 0, 0
            ]
        );
        assert_eq!(&encoded[168..176], &[13, 0, 8, 0, 1, 0, 1, 0]);

        let preauth = mt7921_port_spike::encode_preauth_peer_wcid_command(
            8,
            0,
            7,
            [0x10, 0x20, 0x30, 0x40, 0x50, 0x60],
            100,
        )
        .unwrap();
        assert_eq!(preauth[65], 0);
        assert_eq!(u16::from_le_bytes(preauth[66..68].try_into().unwrap()), 0);
        assert_eq!(&preauth[68..74], &[0x10, 0x20, 0x30, 0x40, 0x50, 0x60]);
        assert_eq!(&preauth[74..76], &[3, 0]);
        assert_eq!(preauth[112], 0);
        assert_eq!(preauth[141], 0);
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn key_v2_commands_keep_linux_physical_allocation_and_targets() {
        let ptk = encode_ptk_command(1, 0, 7, &[0x11; 16]).unwrap();
        assert_eq!(ptk.as_bytes().len(), 136);
        assert_eq!(validate_uni_request(3, ptk.as_bytes()).unwrap(), 1);
        assert_eq!(&ptk.as_bytes()[48..56], &[0, 7, 1, 0, 1, 0, 0, 0]);
        assert_eq!(
            &ptk.as_bytes()[56..68],
            &[17, 0, 44, 0, 0, 1, 0, 0, 5, 36, 0, 16]
        );

        let gtk = encode_gtk_command(2, 0, 2, &[0x22; 16]).unwrap();
        assert_eq!(&gtk.as_bytes()[48..56], &[0, 19, 1, 0, 1, 14, 0, 0]);
        assert_eq!(&gtk.as_bytes()[64..68], &[5, 36, 2, 16]);

        let igtk = encode_igtk_command(3, 0, 4, &[0x44; 16], 2, &[0x22; 16]).unwrap();
        assert_eq!(igtk.as_bytes().len(), 136);
        assert_eq!(&igtk.as_bytes()[58..62], &[80, 0, 0, 2]);
        assert_eq!(&igtk.as_bytes()[64..68], &[5, 36, 2, 16]);
        assert_eq!(&igtk.as_bytes()[68..84], &[0x22; 16]);
        assert_eq!(&igtk.as_bytes()[100..104], &[10, 36, 0, 16]);
        assert_eq!(&igtk.as_bytes()[104..120], &[0x44; 16]);

        let disabled = encode_disable_keys_command(4, 0, 19, 0x0e).unwrap();
        assert_eq!(disabled.as_bytes().len(), 136);
        assert_eq!(&disabled.as_bytes()[56..64], &[17, 0, 8, 0, 1, 0, 0, 0]);
        assert!(disabled.as_bytes()[64..].iter().all(|byte| *byte == 0));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn client_firmware_effects_ack_before_readiness_and_teardown_in_order() {
        let association = LegacyWmeAssociation {
            bss_index: 0,
            peer_wcid: 7,
            aid: 42,
            peer: [0x10, 0x20, 0x30, 0x40, 0x50, 0x60],
            rcpi: 100,
            negotiated_qos: true,
            mfp_required: true,
        };
        let mut state = ClientFirmwareEffectsState::default();
        state
            .bind_join(association.peer, test_channel_lease(36), 100)
            .unwrap();
        prepare_test_preauth(&mut state, association);
        assert!(state.set_controlled_port(true).is_err());
        let mut association_commands = Vec::new();
        state
            .associate(association, test_channel_lease(36), |_, command| {
                association_commands.push(command.to_vec());
                Ok(())
            })
            .unwrap();
        assert_eq!(association_commands.len(), 2);
        validate_uni_request(2, &association_commands[0]).unwrap();
        validate_uni_request(3, &association_commands[1]).unwrap();
        assert!(state.association.is_some());
        assert!(state.set_controlled_port(true).is_err());
        state
            .install_ptk(&[0x11; 16], 0, |_, command| {
                assert_eq!(&command[48..56], &[0, 7, 1, 0, 1, 0, 0, 0]);
                Ok(())
            })
            .unwrap();
        state
            .install_gtk(2, &[0x22; 16], 0, |_, command| {
                assert_eq!(&command[48..56], &[0, 19, 1, 0, 1, 14, 0, 0]);
                Ok(())
            })
            .unwrap();
        assert!(state.set_controlled_port(true).is_err());
        state
            .install_igtk(4, &[0x44; 16], |_, command| {
                assert_eq!(&command[68..84], &[0x22; 16]);
                assert_eq!(&command[104..120], &[0x44; 16]);
                Ok(())
            })
            .unwrap();
        state.set_controlled_port(true).unwrap();
        assert!(state.controlled_port_open);

        let mut teardown = Vec::new();
        state
            .teardown(|_, command| {
                teardown.push((command.len(), command[49], command[58], command[60]));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            teardown,
            vec![
                (136, 19, 8, 1),
                (136, 7, 8, 1),
                (88, 7, 20, 2),
                (96, 0, 0, 1)
            ]
        );
        assert!(!state.controlled_port_open);
        assert!(state.association.is_none());
        assert!(state.gtk.is_none());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn client_firmware_effect_failure_revokes_port_and_requires_teardown() {
        let association = LegacyWmeAssociation {
            bss_index: 0,
            peer_wcid: 7,
            aid: 42,
            peer: [1, 2, 3, 4, 5, 6],
            rcpi: 100,
            negotiated_qos: true,
            mfp_required: false,
        };
        let mut state = ClientFirmwareEffectsState::default();
        state
            .bind_join(association.peer, test_channel_lease(36), 100)
            .unwrap();
        prepare_test_preauth(&mut state, association);
        state
            .associate(association, test_channel_lease(36), |_, _| Ok(()))
            .unwrap();
        state.install_ptk(&[1; 16], 0, |_, _| Ok(())).unwrap();
        assert!(
            state
                .install_gtk(1, &[2; 16], 0, |_, _| Err("negative ACK".into()))
                .is_err()
        );
        assert!(state.firmware_uncertain);
        assert!(!state.controlled_port_open);
        assert!(state.set_controlled_port(true).is_err());
        state.teardown(|_, _| Ok(())).unwrap();
        assert!(!state.firmware_uncertain);
        assert!(state.association.is_none());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn client_data_txwi_txp_matches_pinned_eapol_and_ethernet_fixtures() {
        let eapol =
            encode_client_data_txwi(120, 0x1234_5000, 7, 9, true, false, true, 7).unwrap();
        let data =
            encode_client_data_txwi(100, 0x2234_5000, 8, 10, false, true, false, 0).unwrap();
        let words = |bytes: &[u8; 64]| {
            (0..8)
                .map(|i| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            words(&eapol),
            vec![
                0x0600_0098,
                0x8072_6807,
                0x8000_2028,
                0x1000_7800,
                0,
                0x409,
                0x004b_0004,
                0x0028_0000
            ]
        );
        assert_eq!(
            words(&data),
            vec![
                0x0200_0084,
                0x8000_8007,
                0x28,
                0x7802,
                0,
                0x40a,
                0,
                0x0028_0000
            ]
        );
        let non_qos_eapol =
            encode_client_data_txwi(36, 0x3234_5000, 9, 11, true, false, false, 7).unwrap();
        assert_eq!(words(&non_qos_eapol)[1], 0x8002_6007);
        assert_eq!(words(&non_qos_eapol)[2], 0x8000_2020);
        assert_eq!(
            &eapol[32..46],
            &[7, 128, 0, 0, 0, 0, 0, 0, 0, 80, 52, 18, 120, 128]
        );
        let start_frame = eapol_start_frame([6, 5, 4, 3, 2, 1], [1, 2, 3, 4, 5, 6], true);
        let start = encode_client_data_txwi(
            start_frame.len(),
            0x1234_5000,
            7,
            9,
            true,
            false,
            true,
            7,
        )
        .unwrap();
        let all_words = (0..16)
            .map(|i| u32::from_le_bytes(start[i * 4..i * 4 + 4].try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            all_words,
            vec![
                0x0600_0046,
                0x8072_6807,
                0x8000_2028,
                0x1000_7800,
                0,
                0x409,
                0x004b_0004,
                0x0028_0000,
                0x0000_8007,
                0,
                0x1234_5000,
                0x0000_8026,
                0,
                0,
                0,
                0,
            ]
        );
        let mut management = vec![0; 30];
        management[..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        let management = encode_client_management_tx(
            &management,
            0x2234_4000,
            0x2234_5000,
            8,
            10,
        )
        .unwrap();
        assert_ne!(&management.txwi[..32], &start[..32]);
        assert!(
            encode_client_data_txwi(100, 0x1000, 1, 9, false, false, false, 0).is_err()
        );
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn client_data_generations_replay_and_teardown_fail_closed() {
        let association = LegacyWmeAssociation {
            bss_index: 0,
            peer_wcid: 7,
            aid: 42,
            peer: [1, 2, 3, 4, 5, 6],
            rcpi: 100,
            negotiated_qos: true,
            mfp_required: false,
        };
        let mut state = ClientFirmwareEffectsState::default();
        state
            .bind_join(association.peer, test_channel_lease(36), 100)
            .unwrap();
        prepare_test_preauth(&mut state, association);
        state
            .associate(association, test_channel_lease(36), |_, _| Ok(()))
            .unwrap();
        let association_generation =
            ClientDataGeneration::Association(state.association_generation.unwrap());
        assert!(
            state
                .deliver_rx(ClientRxCandidate {
                    generation: association_generation,
                    eapol: true,
                    // Hardware reports the pre-key raw EAPOL path against
                    // the unicast-search sentinel rather than peer WCID 7.
                    wcid: 1023,
                    tid: 7,
                    group: false,
                    key_id: 0,
                    security_mode: 0,
                    cm: false,
                    clm: false,
                    icv_error: false,
                    mic_error: false,
                    fcs_error: false,
                    pn: [0; 6],
                })
                .is_ok()
        );
        assert!(
            state
                .install_ptk(&[1; 16], 1 << 48, |_, _| panic!("invalid RSC submitted"))
                .is_err()
        );
        state.install_ptk(&[1; 16], 5, |_, _| Ok(())).unwrap();
        state.install_gtk(2, &[2; 16], 9, |_, _| Ok(())).unwrap();
        state.set_controlled_port(true).unwrap();
        let authorized = ClientDataGeneration::Authorized(state.authorized_generation.unwrap());
        let normal = ClientRxCandidate {
            generation: authorized,
            eapol: false,
            wcid: 7,
            tid: 3,
            group: false,
            key_id: 0,
            security_mode: 4,
            cm: false,
            clm: false,
            icv_error: false,
            mic_error: false,
            fcs_error: false,
            pn: [0, 0, 0, 0, 0, 6],
        };
        state.deliver_rx(normal).unwrap();
        assert!(state.deliver_rx(normal).is_err());
        let mut sentinel_data = normal;
        sentinel_data.wcid = 1023;
        sentinel_data.pn[5] = 7;
        assert!(state.deliver_rx(sentinel_data).is_err());
        let mut wrong_crypto = normal;
        wrong_crypto.pn[5] = 7;
        wrong_crypto.cm = true;
        assert!(state.deliver_rx(wrong_crypto).is_err());

        state.publish_tx(11, authorized).unwrap();
        state.set_controlled_port(false).unwrap();
        assert!(state.authorized_generation.is_none());
        assert!(state.publish_tx(12, authorized).is_err());
        assert!(state.deliver_rx(normal).is_err());
        assert!(state.teardown(|_, _| Ok(())).is_err());
        assert!(state.association.is_some());
        state.complete_tx(11).unwrap();
        state.teardown(|_, _| Ok(())).unwrap();
        assert!(state.association_generation.is_none());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn connac2_group1_ccmp_pn_uses_pinned_linux_byte_order() {
        assert_eq!(
            connac2_group1_pn(&[6, 5, 4, 3, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap(),
            [1, 2, 3, 4, 5, 6]
        );
        assert!(connac2_group1_pn(&[0; 5]).is_err());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn client_management_encoder_binds_subtype_and_rejects_non_management() {
        let mut association = vec![0u8; 30];
        association[0..2].copy_from_slice(&0x0000u16.to_le_bytes());
        let encoded = encode_client_management_tx(&association, 0x1000, 0x2000, 7, 11).unwrap();
        assert_eq!(
            u32::from_le_bytes(encoded.txwi[8..12].try_into().unwrap()) & 0xf,
            0
        );
        assert_eq!(
            u16::from_le_bytes(encoded.txwi[44..46].try_into().unwrap()),
            0x801e
        );
        assert_eq!(
            u16::from_le_bytes(encoded.txwi[32..34].try_into().unwrap()),
            0x8007
        );
        assert_eq!(
            u32::from_le_bytes(encoded.txwi[20..24].try_into().unwrap()) & 0xff,
            11
        );
        association[0..2].copy_from_slice(&0x0008u16.to_le_bytes());
        assert!(encode_client_management_tx(&association, 0x1000, 0x2000, 0, 3).is_err());
        assert!(encode_client_management_tx(&association[..20], 0x1000, 0x2000, 0, 3).is_err());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn live_scan_authorization_then_connect_ensures_existing_channel_context() {
        let peer = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
        let channel = ChannelNumber {
            band: WlanBand::FiveGhz,
            number: 36,
        };
        let observation = fuchsia_softmac_port::ScanObservation {
            kind: fuchsia_softmac_port::AdvertisementKind::Beacon,
            timestamp_nanos: 1,
            bss: fidl_ieee80211::BssDescription {
                bssid: peer,
                bss_type: fidl_ieee80211::BssType::Infrastructure,
                beacon_period: 100,
                capability_info: 0x11,
                ies: vec![],
                primary: channel,
                bandwidth: ChannelBandwidth::Cbw40,
                vht_secondary_80_channel: ChannelNumber {
                    number: 0,
                    ..channel
                },
                rssi_dbm: -40,
                snr_db: 20,
            },
        };
        let shared = Arc::new(Mutex::new(LiveClientState {
            selection: retain_client_selection(7, 1, &observation).unwrap(),
            ..Default::default()
        }));
        let mut effects = LiveClientEffects {
            state: shared.clone(),
            target: peer,
            client: [6, 5, 4, 3, 2, 1],
            rcpi: 100,
            firmware: ClientFirmwareEffectsState::default(),
            post_association_data_wait: None,
            eapol_start_deadline: None,
            eapol_start_emitted: false,
        };

        // The selector's scan 7 result is moved into the runtime. External BSS
        // selection must not create a second hardware scan identity here.
        effects.prepare_runtime_handoff();
        assert_eq!(
            effects.begin_passive_scan(8, &[channel]),
            Err(zx::Status::NOT_SUPPORTED)
        );
        effects
            .set_channel(
                channel,
                ChannelBandwidth::Cbw40,
                ChannelNumber {
                    number: 0,
                    ..channel
                },
            )
            .unwrap();
        {
            let mut state = shared.lock().unwrap();
            assert!(
                state
                    .mark_rate_power_ready(
                        peer,
                        channel,
                        ChannelBandwidth::Cbw80,
                        ChannelNumber {
                            number: 0,
                            ..channel
                        },
                    )
                    .is_err()
            );
            state
                .mark_rate_power_ready(
                    peer,
                    channel,
                    ChannelBandwidth::Cbw40,
                    ChannelNumber {
                        number: 0,
                        ..channel
                    },
                )
                .unwrap();
            assert!(
                state
                    .authorize_sae(
                        [9; 6],
                        channel,
                        ChannelBandwidth::Cbw40,
                        ChannelNumber {
                            number: 0,
                            ..channel
                        },
                    )
                    .is_err()
            );
            let generation = state
                .authorize_sae(
                    peer,
                    channel,
                    ChannelBandwidth::Cbw40,
                    ChannelNumber {
                        number: 0,
                        ..channel
                    },
                )
                .unwrap();
            assert_eq!(
                state.channel.authorized_channel().unwrap().generation,
                generation
            );
            assert!(
                state
                    .authorize_sae(
                        peer,
                        channel,
                        ChannelBandwidth::Cbw40,
                        ChannelNumber {
                            number: 0,
                            ..channel
                        },
                    )
                    .is_err()
            );
        }

        assert_eq!(
            effects
                .ensure_channel(
                    channel,
                    ChannelBandwidth::Cbw40,
                    ChannelNumber {
                        number: 0,
                        ..channel
                    },
                )
                .unwrap(),
            ClientChannelEnsure::Current
        );
        effects
            .join_bss(&fidl_driver::JoinBssRequest {
                bssid: Some(peer),
                bss_type: Some(fidl_ieee80211::BssType::Infrastructure),
                remote: Some(true),
                beacon_period: Some(100),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(effects.firmware.joined.unwrap().channel_generation, 1);
        effects.reset().unwrap();
        assert_eq!(
            effects.join_bss(&fidl_driver::JoinBssRequest {
                bssid: Some(peer),
                bss_type: Some(fidl_ieee80211::BssType::Infrastructure),
                remote: Some(true),
                beacon_period: Some(100),
                ..Default::default()
            }),
            Err(zx::Status::BAD_STATE)
        );
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn live_client_effects_close_sae_eapol_keys_port_data_and_teardown_in_order() {
        let peer = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
        let mut io = TestClientIo::default();
        let mut effects = LiveClientEffects {
            state: Arc::new(Mutex::new(LiveClientState::default())),
            target: peer,
            client: [6, 5, 4, 3, 2, 1],
            rcpi: 100,
            firmware: ClientFirmwareEffectsState::default(),
            post_association_data_wait: None,
            eapol_start_deadline: None,
            eapol_start_emitted: false,
        };
        let association = fidl_softmac::WlanAssociationConfig {
            bssid: Some(peer),
            // MLME has already removed the reserved on-wire AID bits.
            aid: Some(4),
            qos: Some(true),
            wmm_params: Some(fidl_driver::WlanWmmParameters {
                apsd: false,
                ac_be_params: fidl_driver::WlanWmmAccessCategoryParameters {
                    ecw_min: 4,
                    ecw_max: 10,
                    aifsn: 3,
                    txop_limit: 0,
                    acm: false,
                },
                ac_bk_params: fidl_driver::WlanWmmAccessCategoryParameters {
                    ecw_min: 4,
                    ecw_max: 10,
                    aifsn: 7,
                    txop_limit: 0,
                    acm: false,
                },
                ac_vi_params: fidl_driver::WlanWmmAccessCategoryParameters {
                    ecw_min: 3,
                    ecw_max: 4,
                    aifsn: 2,
                    txop_limit: 94,
                    acm: false,
                },
                ac_vo_params: fidl_driver::WlanWmmAccessCategoryParameters {
                    ecw_min: 2,
                    ecw_max: 3,
                    aifsn: 2,
                    txop_limit: 47,
                    acm: false,
                },
            }),
            ..Default::default()
        };
        let channel = ChannelNumber {
            band: WlanBand::FiveGhz,
            number: 36,
        };
        effects
            .set_channel(
                channel,
                ChannelBandwidth::Cbw20,
                ChannelNumber {
                    number: 0,
                    ..channel
                },
            )
            .unwrap();
        {
            let mut state = effects.state.lock().unwrap();
            install_selection_and_authorize(&mut state, peer, channel, ChannelBandwidth::Cbw20);
        }
        effects
            .join_bss(&fidl_driver::JoinBssRequest {
                bssid: Some(peer),
                bss_type: Some(fidl_ieee80211::BssType::Infrastructure),
                remote: Some(true),
                beacon_period: Some(100),
                ..Default::default()
            })
            .unwrap();
        let mut sae = vec![0; 30];
        sae[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        sae[4..10].copy_from_slice(&peer);
        sae[10..16].copy_from_slice(&effects.client);
        sae[16..22].copy_from_slice(&peer);
        sae[24..26].copy_from_slice(&3u16.to_le_bytes());
        sae[26..28].copy_from_slice(&2u16.to_le_bytes());
        effects
            .send_wlan_frame(&sae, fidl_softmac::WlanTxInfoFlags::empty(), &mut io)
            .unwrap();
        let mut association_request = vec![0; 28];
        association_request[..2].copy_from_slice(&0x0800u16.to_le_bytes());
        association_request[4..10].copy_from_slice(&peer);
        association_request[10..16].copy_from_slice(&effects.client);
        association_request[16..22].copy_from_slice(&peer);
        association_request[22..24].copy_from_slice(&(19u16 << 4).to_le_bytes());
        association_request[24..26].copy_from_slice(&0x0011u16.to_le_bytes());
        association_request[26..28].copy_from_slice(&5u16.to_le_bytes());
        association_request.extend_from_slice(&[0, 3, 1, 2, 3, 48, 2, 4, 5, 244, 1, 0x20]);
        assert_eq!(
            management_ie_id_lengths(&association_request, 28),
            "0:3,48:2,244:1"
        );
        assert_eq!(
            u16::from_le_bytes(association_request[24..26].try_into().unwrap()),
            0x0011
        );
        assert_eq!(
            u16::from_le_bytes(association_request[26..28].try_into().unwrap()),
            5
        );
        effects
            .send_wlan_frame(
                &association_request,
                fidl_softmac::WlanTxInfoFlags::empty(),
                &mut io,
            )
            .unwrap();
        let rx_status = fidl_softmac::WlanRxInfo {
            rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
            valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
            phy: fidl_ieee80211::WlanPhyType::Ofdm,
            data_rate: 0,
            primary: channel,
            bandwidth: ChannelBandwidth::Cbw20,
            vht_secondary_80_channel: ChannelNumber {
                number: 0,
                ..channel
            },
            mcs: 0,
            rssi_dbm: -40,
            snr_dbh: 0,
        };
        let mut association_response = vec![0; 30];
        association_response[..2].copy_from_slice(&0x0010u16.to_le_bytes());
        association_response[4..10].copy_from_slice(&[1, 1, 1, 1, 1, 1]);
        association_response[10..16].copy_from_slice(&peer);
        association_response[16..22].copy_from_slice(&peer);
        association_response[28..30].copy_from_slice(&0xc004u16.to_le_bytes());
        io.rx.push_back(ClientRxFrame {
            bytes: association_response.clone(),
            status: rx_status.clone(),
            security: None,
        });
        assert!(effects.next_rx(&mut io).unwrap().is_none());
        assert!(effects.firmware.association.is_none());

        association_response[4..10].copy_from_slice(&effects.client);
        association_response[26..28].copy_from_slice(&30u16.to_le_bytes());
        io.rx.push_back(ClientRxFrame {
            bytes: association_response.clone(),
            status: rx_status.clone(),
            security: None,
        });
        assert_eq!(
            effects.next_rx(&mut io).unwrap().unwrap().bytes,
            association_response
        );
        assert!(effects.firmware.association.is_none());
        assert_eq!(io.uni.len(), 1);

        association_response[26..28].copy_from_slice(&0u16.to_le_bytes());
        io.rx.push_back(ClientRxFrame {
            bytes: association_response.clone(),
            status: rx_status.clone(),
            security: None,
        });
        assert_eq!(
            effects.next_rx(&mut io).unwrap().unwrap().bytes,
            association_response
        );
        assert!(effects.firmware.association.is_none());
        for invalid_aid in [0, 2008, 0xc004] {
            assert_eq!(
                effects.notify_association_complete(
                    &fidl_softmac::WlanAssociationConfig {
                        bssid: Some(peer),
                        aid: Some(invalid_aid),
                        qos: Some(true),
                        ..Default::default()
                    },
                    &mut io,
                ),
                Err(zx::Status::INVALID_ARGS)
            );
        }
        assert!(effects.firmware.association.is_none());
        effects
            .notify_association_complete(&association, &mut io)
            .unwrap();
        assert_eq!(io.uni.len(), 4);
        assert_eq!(u16::from_le_bytes(io.uni[2][66..68].try_into().unwrap()), 4);
        assert_eq!(
            u16::from_le_bytes(io.uni[2][144..146].try_into().unwrap()),
            4
        );
        assert_eq!(io.uni[3].get(36..39), Some(&[0x1d, 0xa0, 1][..]));
        assert!(effects.firmware.qos_tx_ready());
        assert_eq!(
            u16::from_le_bytes(association_response[28..30].try_into().unwrap()),
            0xc004
        );
        assert_eq!(effects.firmware.association.unwrap().aid, 4);
        assert!(!effects.firmware.controlled_port_open);
        assert!(!effects.firmware.ptk_installed);
        assert!(effects.firmware.ptk_rx_pn.is_none());
        let generation = effects.firmware.association_generation.unwrap();
        effects.eapol_start_deadline = Some((Instant::now(), generation));
        assert!(effects.next_rx(&mut io).unwrap().is_none());
        assert_eq!(
            io.tx.last(),
            Some(&eapol_start_frame(effects.client, peer, true))
        );
        assert!(effects.eapol_start_emitted);
        let tx_after_start = io.tx.len();
        assert!(effects.next_rx(&mut io).unwrap().is_none());
        assert_eq!(io.tx.len(), tx_after_start);
        let mut protected_disassociation = vec![0x5a; 42];
        protected_disassociation[0..2].copy_from_slice(&0x40a0u16.to_le_bytes());
        protected_disassociation[2..4].fill(0);
        protected_disassociation[4..10].copy_from_slice(&effects.client);
        protected_disassociation[10..16].copy_from_slice(&peer);
        protected_disassociation[16..22].copy_from_slice(&peer);
        protected_disassociation[22..24].fill(0);
        io.rx.push_back(ClientRxFrame {
            bytes: protected_disassociation.clone(),
            status: rx_status.clone(),
            security: None,
        });
        assert!(effects.next_rx(&mut io).unwrap().is_none());
        let peer_security = ClientRxSecurity {
            wcid: 7,
            tid: 0,
            key_id: 0,
            security_mode: 0,
            cm: false,
            clm: false,
            icv_error: false,
            mic_error: false,
            fcs_error: false,
            pn: None,
        };
        let mut inbound_eapol = vec![0x08, 0x02, 0, 0];
        inbound_eapol.extend_from_slice(&effects.client);
        inbound_eapol.extend_from_slice(&peer);
        inbound_eapol.extend_from_slice(&peer);
        inbound_eapol.extend_from_slice(&[0, 0, 0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
        inbound_eapol.extend_from_slice(&[1, 3, 0, 95, 2, 0, 0x8a, 0, 16]);
        inbound_eapol.extend_from_slice(&1u64.to_be_bytes());
        inbound_eapol.extend_from_slice(&[0x11; 32]);
        inbound_eapol.extend_from_slice(&[0; 16 + 8 + 8 + 16]);
        inbound_eapol.extend_from_slice(&[0, 0]);
        assert!(is_authenticator_m1(&inbound_eapol));
        io.rx.push_back(ClientRxFrame {
            bytes: inbound_eapol.clone(),
            status: rx_status.clone(),
            security: Some(peer_security),
        });
        assert_eq!(
            effects.next_rx(&mut io).unwrap().unwrap().bytes,
            inbound_eapol
        );
        effects.eapol_start_deadline = Some((Instant::now(), generation));
        effects.eapol_start_emitted = false;
        let tx_before_immediate_m1 = io.tx.len();
        io.rx.push_back(ClientRxFrame {
            bytes: inbound_eapol.clone(),
            status: rx_status.clone(),
            security: Some(peer_security),
        });
        assert_eq!(
            effects.next_rx(&mut io).unwrap().unwrap().bytes,
            inbound_eapol
        );
        assert!(effects.eapol_start_deadline.is_none());
        assert!(effects.next_rx(&mut io).unwrap().is_none());
        assert_eq!(io.tx.len(), tx_before_immediate_m1);

        let mut sentinel_data = inbound_eapol.clone();
        sentinel_data[30..32].copy_from_slice(&[0x08, 0x00]);
        let sentinel_security = ClientRxSecurity {
            wcid: 1023,
            ..peer_security
        };
        io.rx.push_back(ClientRxFrame {
            bytes: sentinel_data,
            status: rx_status.clone(),
            security: Some(sentinel_security),
        });
        assert!(effects.next_rx(&mut io).unwrap().is_none());

        let mut eapol = vec![0x88, 0x01, 0, 0];
        eapol.extend_from_slice(&peer);
        eapol.extend_from_slice(&effects.client);
        eapol.extend_from_slice(&peer);
        eapol.extend_from_slice(&[0, 0, 7, 0, 0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e, 1, 2]);
        effects
            .send_wlan_frame(&eapol, fidl_softmac::WlanTxInfoFlags::empty(), &mut io)
            .unwrap();

        let key =
            |key_type, peer_addr, key_idx, cipher_type, byte| fidl_softmac::WlanKeyConfiguration {
                protection: Some(fidl_softmac::WlanProtection::RxTx),
                cipher_oui: Some([0, 15, 172]),
                cipher_type: Some(cipher_type),
                key_type: Some(key_type),
                peer_addr: Some(peer_addr),
                key_idx: Some(key_idx),
                key: Some(vec![byte; 16]),
                rsc: Some(0),
            };
        effects
            .install_key(
                &key(fidl_ieee80211::KeyType::Pairwise, peer, 0, 4, 0x11),
                &mut io,
            )
            .unwrap();
        effects.firmware.association.as_mut().unwrap().mfp_required = true;
        protected_disassociation[32..34].copy_from_slice(&9u16.to_le_bytes());
        io.rx.push_back(ClientRxFrame {
            bytes: protected_disassociation,
            status: rx_status.clone(),
            security: Some(ClientRxSecurity {
                security_mode: 4,
                pn: Some([0, 0, 0, 0, 0, 1]),
                ..peer_security
            }),
        });
        let admitted = effects.next_rx(&mut io).unwrap().unwrap();
        assert_eq!(admitted.bytes.len(), 26);
        assert_eq!(&admitted.bytes[24..26], &9u16.to_le_bytes());
        // This association fixture does not negotiate MFP through FIDL; the
        // mutation above solely exercises the verified post-key RX branch.
        effects.firmware.association.as_mut().unwrap().mfp_required = false;
        assert_eq!(effects.set_link_up(true), Err(zx::Status::BAD_STATE));
        effects
            .install_key(
                &key(fidl_ieee80211::KeyType::Group, [0xff; 6], 2, 4, 0x22),
                &mut io,
            )
            .unwrap();
        effects.set_link_up(true).unwrap();
        assert!(effects.firmware.controlled_port_open);
        let mut data = vec![0x08, 0x01, 0, 0];
        data.extend_from_slice(&peer);
        data.extend_from_slice(&effects.client);
        data.extend_from_slice(&peer);
        data.extend_from_slice(&[0, 0, 0xaa, 0xaa, 3, 0, 0, 0, 0x08, 0x00, 9, 8]);
        effects
            .send_wlan_frame(&data, fidl_softmac::WlanTxInfoFlags::PROTECTED, &mut io)
            .unwrap();
        effects
            .clear_association(
                &fidl_softmac::WlanSoftmacBaseClearAssociationRequest {
                    peer_addr: Some(peer),
                },
                &mut io,
            )
            .unwrap();
        assert_eq!(io.uni.len(), 10);
        assert_eq!(
            io.tx,
            [
                sae,
                association_request,
                eapol_start_frame(effects.client, peer, true),
                eapol,
                data
            ]
        );
        assert!(effects.firmware.association.is_none());

        let mut physically_unbound = LiveClientEffects {
            state: Arc::new(Mutex::new(LiveClientState::default())),
            target: peer,
            client: [6, 5, 4, 3, 2, 1],
            rcpi: 100,
            firmware: ClientFirmwareEffectsState::default(),
            post_association_data_wait: None,
            eapol_start_deadline: None,
            eapol_start_emitted: false,
        };
        physically_unbound
            .set_channel(
                channel,
                ChannelBandwidth::Cbw20,
                ChannelNumber {
                    number: 0,
                    ..channel
                },
            )
            .unwrap();
        {
            let mut state = physically_unbound.state.lock().unwrap();
            install_selection_and_authorize(&mut state, peer, channel, ChannelBandwidth::Cbw20);
        }
        physically_unbound
            .firmware
            .bind_join(peer, test_channel_lease(36), 100)
            .unwrap();
        let mut no_wcid_io = TestClientIo::default();
        assert_eq!(
            physically_unbound.send_wlan_frame(
                &eapol_start_frame(physically_unbound.client, peer, true),
                fidl_softmac::WlanTxInfoFlags::empty(),
                &mut no_wcid_io,
            ),
            Err(zx::Status::ACCESS_DENIED)
        );
        assert!(no_wcid_io.tx.is_empty());
        assert_eq!(
            {
                let mut failed = TestClientIo {
                    fail_uni: true,
                    ..Default::default()
                };
                physically_unbound.notify_association_complete(&association, &mut failed)
            },
            Err(zx::Status::IO)
        );
        assert!(physically_unbound.firmware.association.is_none());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn live_client_pre_port_sae_generation_reaches_physical_io_only_when_current() {
        let peer = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
        let client = [6, 5, 4, 3, 2, 1];
        let channel = ChannelNumber {
            band: WlanBand::FiveGhz,
            number: 36,
        };
        let shared = Arc::new(Mutex::new(LiveClientState::default()));
        let mut io = TestClientIo::default();
        let mut effects = LiveClientEffects {
            state: shared.clone(),
            target: peer,
            client,
            rcpi: 100,
            firmware: ClientFirmwareEffectsState::default(),
            post_association_data_wait: None,
            eapol_start_deadline: None,
            eapol_start_emitted: false,
        };
        effects
            .set_channel(
                channel,
                ChannelBandwidth::Cbw20,
                ChannelNumber {
                    number: 0,
                    ..channel
                },
            )
            .unwrap();
        {
            let mut state = shared.lock().unwrap();
            install_selection_and_authorize(&mut state, peer, channel, ChannelBandwidth::Cbw20);
        }
        let frame = |sequence: u16| {
            let mut bytes = vec![0; 30];
            bytes[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
            bytes[4..10].copy_from_slice(&peer);
            bytes[10..16].copy_from_slice(&client);
            bytes[16..22].copy_from_slice(&peer);
            bytes[24..26].copy_from_slice(&3u16.to_le_bytes());
            bytes[26..28].copy_from_slice(&sequence.to_le_bytes());
            bytes
        };

        let mut foreign = frame(1);
        foreign[4] ^= 1;
        assert_eq!(
            effects.send_wlan_frame(&foreign, fidl_softmac::WlanTxInfoFlags::empty(), &mut io),
            Err(zx::Status::ACCESS_DENIED)
        );
        assert_eq!(
            effects.send_wlan_frame(&frame(1), fidl_softmac::WlanTxInfoFlags::PROTECTED, &mut io),
            Err(zx::Status::ACCESS_DENIED)
        );
        let mut open_system = frame(1);
        open_system[24..26].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            effects.send_wlan_frame(
                &open_system,
                fidl_softmac::WlanTxInfoFlags::empty(),
                &mut io
            ),
            Err(zx::Status::ACCESS_DENIED)
        );

        effects
            .send_wlan_frame(&frame(2), fidl_softmac::WlanTxInfoFlags::empty(), &mut io)
            .unwrap();
        assert_eq!(io.tx, vec![frame(2)]);
        effects.revoke_scan();
        assert_eq!(
            effects.send_wlan_frame(&frame(2), fidl_softmac::WlanTxInfoFlags::empty(), &mut io,),
            Err(zx::Status::ACCESS_DENIED)
        );

        {
            let mut state = shared.lock().unwrap();
            install_selection_and_authorize(&mut state, peer, channel, ChannelBandwidth::Cbw20);
        }
        effects
            .send_wlan_frame(&frame(2), fidl_softmac::WlanTxInfoFlags::empty(), &mut io)
            .unwrap();
        assert_eq!(io.tx, vec![frame(2), frame(2)]);
        assert!(!effects.firmware.controlled_port_open);
        assert!(effects.firmware.association.is_none());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn live_effects_admit_only_current_preassociation_sae_authentication() {
        let peer = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
        let client = [6, 5, 4, 3, 2, 1];
        let channel = ChannelNumber {
            band: WlanBand::FiveGhz,
            number: 36,
        };
        let mut effects = LiveClientEffects {
            state: Arc::new(Mutex::new(LiveClientState::default())),
            target: peer,
            client,
            rcpi: 100,
            firmware: ClientFirmwareEffectsState::default(),
            post_association_data_wait: None,
            eapol_start_deadline: None,
            eapol_start_emitted: false,
        };
        effects
            .set_channel(
                channel,
                ChannelBandwidth::Cbw20,
                ChannelNumber {
                    number: 0,
                    ..channel
                },
            )
            .unwrap();
        install_selection_and_authorize(
            &mut effects.state.lock().unwrap(),
            peer,
            channel,
            ChannelBandwidth::Cbw20,
        );

        let auth = |transmitter: [u8; 6], body: &[u8], primary: ChannelNumber| {
            let mut bytes = vec![0; 30];
            bytes[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
            bytes[4..10].copy_from_slice(&client);
            bytes[10..16].copy_from_slice(&transmitter);
            bytes[16..22].copy_from_slice(&peer);
            bytes[24..26].copy_from_slice(&3u16.to_le_bytes());
            bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
            bytes[28..30].copy_from_slice(&77u16.to_le_bytes());
            bytes.extend_from_slice(body);
            ClientRxFrame {
                bytes,
                status: fidl_softmac::WlanRxInfo {
                    rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
                    valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
                    phy: fidl_ieee80211::WlanPhyType::Ofdm,
                    data_rate: 0,
                    primary,
                    bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw20,
                    vht_secondary_80_channel: ChannelNumber {
                        number: 0,
                        ..primary
                    },
                    mcs: 0,
                    rssi_dbm: -40,
                    snr_dbh: 0,
                },
                security: None,
            }
        };
        let mut io = TestClientIo::default();
        io.rx.push_back(auth(peer, &20u16.to_le_bytes(), channel));
        assert!(effects.next_rx(&mut io).unwrap().is_some());

        let mut preassociation_data = auth(peer, &20u16.to_le_bytes(), channel);
        preassociation_data.bytes[0..2].copy_from_slice(&0x0208u16.to_le_bytes());
        io.rx.push_back(preassociation_data);
        assert!(effects.next_rx(&mut io).unwrap().is_none());

        io.rx.push_back(auth([9; 6], &20u16.to_le_bytes(), channel));
        assert!(effects.next_rx(&mut io).unwrap().is_none());
        io.rx.push_back(auth(peer, &[20], channel));
        assert!(effects.next_rx(&mut io).unwrap().is_none());
        let mut non_auth = auth(peer, &20u16.to_le_bytes(), channel);
        non_auth.bytes[0..2].copy_from_slice(&0x0080u16.to_le_bytes());
        io.rx.push_back(non_auth);
        assert!(effects.next_rx(&mut io).unwrap().is_none());
        io.rx.push_back(auth(
            peer,
            &20u16.to_le_bytes(),
            ChannelNumber {
                number: 40,
                ..channel
            },
        ));
        assert!(effects.next_rx(&mut io).unwrap().is_none());
        effects.reset().unwrap();
        io.rx.push_back(auth(peer, &20u16.to_le_bytes(), channel));
        assert!(effects.next_rx(&mut io).unwrap().is_none());
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn unified_request_binding_precedes_any_dma_or_mmio_publication() {
        let source = include_str!("vfio_read.rs");
        let submit = source
            .split("fn send_acknowledged_uni_command(")
            .nth(1)
            .unwrap()
            .split("fn send_passive_command(")
            .next()
            .unwrap();
        let validate = submit
            .find("validate_uni_request(expected_cid, encoded)")
            .unwrap();
        let irq = submit.find("write_active_wfdma(0xd4204").unwrap();
        let publish = submit.find("publish_mcu_bytes(").unwrap();
        assert!(validate < irq && irq < publish);
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn consumed_uni_slot_resets_descriptor_and_securely_wipes_payload() {
        fn arena(fill: u8, len: usize) -> DmaArena {
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
            unsafe { std::ptr::write_bytes(ptr.as_ptr(), fill, len) };
            DmaArena {
                mapping: None,
                ptr: Some(ptr),
                len,
                iova: 0,
            }
        }
        let mut ring = arena(0x5a, PAGE);
        let mut payload = arena(0xa5, MCU_COMMAND_PAYLOAD_BYTES);
        ring.write_descriptor_at(
            2,
            mt7921_dma_tx(
                DmaSegment {
                    iova: 0x1000,
                    len: 8,
                },
                None,
                0,
            )
            .unwrap(),
        );
        reclaim_uni_dma_slot(&mut ring, &mut payload, 2).unwrap();
        assert_eq!(ring.read_descriptor_at(2), DmaDescriptor::reset());
        assert!(
            payload
                .read_bytes(0, MCU_COMMAND_PAYLOAD_BYTES)
                .unwrap()
                .iter()
                .all(|byte| *byte == 0)
        );
        unsafe {
            munmap(ring.ptr.unwrap().as_ptr(), ring.len);
            munmap(payload.ptr.unwrap().as_ptr(), payload.len);
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn unified_command_timeout_reclaims_consumed_slot_but_contains_owned_slot() {
        assert_eq!(uni_command_reclaim(true), UniCommandReclaim::ResetAndZero);
        assert_eq!(
            uni_command_reclaim(false),
            UniCommandReclaim::ContainWithDmaOwned
        );
        let source = include_str!("vfio_read.rs");
        let submit = source
            .split("fn send_acknowledged_uni_command(")
            .nth(1)
            .unwrap()
            .split("fn send_passive_command(")
            .next()
            .unwrap();
        assert!(submit.contains("self.uni_terminal_poisoned = true"));
        assert!(submit.contains("containment required"));
        let loader = source
            .split("impl VfioFirmwareLoader<'_> {")
            .nth(1)
            .unwrap()
            .split("struct VfioRateTxPower")
            .next()
            .unwrap();
        for method in [
            "fn send_acknowledged_uni_command(",
            "fn send_passive_command(",
            "fn send_rate_power_bytes(",
            "fn query_pse_base(",
        ] {
            let body = loader.split(method).nth(1).unwrap();
            let guard = body.find("ensure_mcu_tx_allowed()?").unwrap();
            let publish = body.find("publish_mcu_bytes(").unwrap();
            assert!(guard < publish, "{method}");
        }

        let active_cleanup = source
            .split("let ledger = capsule")
            .find(|segment| segment.contains("DMA busy during teardown"))
            .unwrap()
            .split("if !release_errors.is_empty()")
            .next()
            .unwrap();
        assert!(
            active_cleanup.find("if !bme_disabled").unwrap()
                < active_cleanup.find("attempt_all_cleanup").unwrap()
        );
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn normal_rx_burst_is_delivered_in_descriptor_order() {
        let mut routed = VecDeque::new();
        for descriptor in 0..4u8 {
            routed.push_back(PrivateRawFrameCarrier {
                bytes: vec![descriptor],
                occurrence: None,
            });
        }
        assert_eq!(
            (0..4)
                .map(|_| routed.pop_front().unwrap().bytes[0])
                .collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn long_session_cid2_uses_the_next_synchronous_uni_slot() {
        let mut command_index = 0;
        for _ in 0..43 {
            let next = next_dma_index(command_index, MCU_TX_RING_COUNT);
            assert!(dma_index_completed(next as u32, next as u32));
            command_index = next;
        }
        let cid = 2;
        let next = next_dma_index(command_index, MCU_TX_RING_COUNT);
        assert_eq!((cid, command_index, next), (2, 43, 44));

        let source = include_str!("vfio_read.rs");
        let submit = source
            .split("fn send_acknowledged_uni_command(")
            .nth(1)
            .unwrap()
            .split("fn send_passive_command(")
            .next()
            .unwrap();
        assert!(submit.contains("uni_ring_pre_publish"));
        assert!(submit.contains("uni_ring_post_publish"));
        assert!(submit.contains("uni_ring_consumed"));
        assert!(submit.contains("uni_ring_timeout"));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn unified_ack_rejects_wrong_envelope_cid_status_and_truncation() {
        let response = |event_id, option, cid, status: u32| {
            let mut bytes = vec![0; 44];
            bytes[36] = cid;
            bytes[40..44].copy_from_slice(&status.to_le_bytes());
            ReceivedMcuResponse {
                event_id,
                option,
                bytes,
            }
        };
        assert!(classify_uni_ack(3, &response(1, 0, 3, 0)).is_ok());
        assert!(classify_uni_ack(3, &response(2, 0, 3, 0)).is_err());
        assert!(classify_uni_ack(3, &response(1, 1 << 2, 3, 0)).is_err());
        assert!(classify_uni_ack(3, &response(1, 0, 2, 0)).is_err());
        assert!(classify_uni_ack(3, &response(1, 0, 3, 5)).is_err());
        assert!(
            classify_uni_ack(
                3,
                &ReceivedMcuResponse {
                    event_id: 1,
                    option: 0,
                    bytes: vec![0; 43],
                }
            )
            .is_err()
        );
    }

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

    #[test]
    fn contained_transport_activation_stops_before_firmware_source_shape() {
        let source = include_str!("vfio_read.rs");
        let boundary = source
            .split("fn run_contained_dma_resource_round_trip")
            .nth(1)
            .unwrap()
            .split("pub fn main")
            .next()
            .unwrap();
        let activation_only = boundary
            .split("if let Some((patch_bytes, ram_bytes)) = firmware_images")
            .next()
            .unwrap();
        let mapped = boundary.find("acquire_active_vfio_resources(").unwrap();
        let disabled = boundary.find("vfio_dma_pre_bme_verified").unwrap();
        let prep = boundary.find("vfio_wfdma_prep_begin").unwrap();
        let sanitize = boundary
            .find("write_active_wfdma(0xd4208, disabled)")
            .unwrap();
        let irq = boundary.find("install_vfio_irq").unwrap();
        let bme = boundary.find("set_pci_bus_master(bdf, true)").unwrap();
        let complete = boundary.find("vfio_wfdma_prep_complete").unwrap();
        let activation = boundary.find("vfio_wfdma_activation_begin").unwrap();
        let engine = boundary
            .find("write_active_wfdma(0xd4208, enabled)")
            .unwrap();
        let activated = boundary.find("vfio_wfdma_activation_complete").unwrap();
        let mask = boundary.find("write_active_wfdma(0xd4204, 0)").unwrap();
        let idle = boundary
            .find("WFDMA busy during contained cleanup")
            .unwrap();
        let bme_off = boundary.find("set_pci_bus_master(bdf, false)").unwrap();
        let unmap = boundary.find("capsule.release_observable()").unwrap();
        let reset = boundary.find("reset_vfio_device(&capsule.device)").unwrap();
        assert!(mapped < disabled && disabled < prep && prep < sanitize);
        assert!(sanitize < irq && irq < bme && bme < complete);
        assert!(complete < activation && activation < engine && engine < activated);
        assert!(activated < mask && mask < idle && idle < bme_off);
        assert!(bme_off < unmap && unmap < reset);
        assert!(!activation_only.contains("load_mt7921_firmware"));
        assert!(!activation_only.contains("publish_mcu_command"));
        assert!(!activation_only.contains("dma_and_response_irq_enabled"));
        assert!(!activation_only.contains("write_active_wfdma(0xd4204, response_irq_mask)"));
        assert!(!activation_only.contains("publish_mcu_bytes"));
        assert!(!activation_only.contains("write_active_wfdma(0xd4408"));
        assert!(!activation_only.contains("write_active_wfdma(0xd4418"));
    }

    #[test]
    fn contained_rx2_lifecycle_is_source_exact_and_precedes_rx_dma() {
        let source = include_str!("vfio_read.rs");
        let resource_prep = source
            .split("fn acquire_active_vfio_resources")
            .nth(1)
            .unwrap()
            .split("fn run_contained_dma_resource_round_trip")
            .next()
            .unwrap();
        let data_descriptors = resource_prep
            .find("let prepared_data = prepare_mcu_rx_ring")
            .unwrap();
        let data_fence = resource_prep
            .find("atomic::fence(std::sync::atomic::Ordering::Release)")
            .unwrap();
        assert!(data_descriptors < data_fence);

        let boundary = source
            .split("fn run_contained_dma_resource_round_trip")
            .nth(1)
            .unwrap()
            .split("pub fn main")
            .next()
            .unwrap();
        let rx_rings = boundary.find("prepare_global_rx_rings(").unwrap();
        let rx2_identity = boundary
            .find("active.data_rx_ring.as_ref().expect(\"mapped\").iova as u32")
            .unwrap();
        let rx2_ext = boundary
            .find("write_active_wfdma(0xd4688, 0x0040_0004)")
            .unwrap();
        let rx_dma = boundary
            .find("write_active_wfdma(0xd4208, enabled)")
            .unwrap();
        assert!(rx_rings < rx2_identity && rx2_identity < rx2_ext && rx2_ext < rx_dma);
        assert!(active_wfdma_write_allowed(
            0xd4688,
            0x0040_0004,
            firmware_bootstrap_rx_irq_mask()
        ));
        assert!(!active_wfdma_write_allowed(
            0xd4688,
            0,
            firmware_bootstrap_rx_irq_mask()
        ));

        let accepts = |writes: &[(usize, u32)]| writes.contains(&(0xd4688, 0x0040_0004));
        assert!(accepts(&[
            (0xd4680, 4),
            (0xd4688, 0x0040_0004),
            (0xd4690, 0x00c0_0004),
        ]));
        assert!(!accepts(&[(0xd4680, 4), (0xd4690, 0x00c0_0004)]));

        let accepts_identity =
            |base, count, cidx, didx| base == 0x0101_0000 && count == 8 && cidx == 7 && didx == 0;
        assert!(accepts_identity(0x0101_0000, 8, 7, 0));
        assert!(!accepts_identity(0x0100_3000, 8, 0, 0));

        let passive_prepare = source
            .split("fn prepare_passive_receive(&mut self)")
            .nth(1)
            .unwrap()
            .split("fn command(")
            .next()
            .unwrap();
        assert!(!passive_prepare.contains("write_rx_ring_slot"));
        assert!(!passive_prepare.contains("ProgramDataRing"));
    }

    #[test]
    fn passive_firmware_init_reuses_contained_transport_source_shape() {
        assert!(Operation::RunOneShotFirmware.uses_contained_transport_gate());
        let source = include_str!("vfio_read.rs");
        let boundary = source
            .split("fn run_contained_dma_resource_round_trip")
            .nth(1)
            .unwrap()
            .split("pub fn main")
            .next()
            .unwrap();
        let activated = boundary.find("vfio_wfdma_activation_complete").unwrap();
        let ready = boundary.find("vfio_firmware_transport_ready").unwrap();
        let loader = boundary.find("load_mt7921_firmware(&mut loader").unwrap();
        let eeprom = source.find("eeprom_efuse_acquired").unwrap();
        let clc = source.find("clc_calibration_configured").unwrap();
        let cleanup = boundary.find("vfio_dma_cleanup_begin").unwrap();
        assert!(activated < ready && ready < loader && loader < cleanup);
        assert!(eeprom < clc);
        assert!(!boundary.contains("load_mt7921_firmware_through_channel_domain(&mut loader"));
        assert!(!boundary.contains("load_mt7921_firmware_with_passive_boundary(&mut loader"));

        let run = source
            .split("fn run() -> Result<(), String>")
            .nth(1)
            .unwrap();
        let process = run.find("vfio_firmware_process_started").unwrap();
        let artifacts = run.find("vfio_firmware_artifacts_ready").unwrap();
        let attach = run.find("VFIO_DEVICE_BIND_IOMMUFD").unwrap();
        assert!(process < artifacts && artifacts < attach);
    }

    #[test]
    fn contained_channel_one_scan_is_receive_only_and_bounded_source_shape() {
        assert!(Operation::RunOneShotPassiveChannel1.uses_contained_transport_gate());
        let source = include_str!("vfio_read.rs");
        let boundary = source
            .split("fn run_contained_dma_resource_round_trip")
            .nth(1)
            .unwrap()
            .split("pub fn main")
            .next()
            .unwrap();
        let acquisition = boundary
            .split("acquire_active_vfio_resources(")
            .nth(1)
            .unwrap()
            .split("record_sae_stage")
            .next()
            .unwrap();
        assert!(acquisition.contains("operation,"));
        assert!(boundary.contains("RunPhase::MappedDmaDisabled"));
        assert!(boundary.contains("RunPhase::DmaAndResponseIrqEnabled"));
        let configured = boundary
            .find("load_mt7921_firmware_with_passive_boundary")
            .unwrap();
        let tuned = boundary.find(".set_channel(").unwrap();
        let rx_ready = boundary.find("vfio_passive_receive_setup_ready").unwrap();
        let scan = boundary.find(".start_passive_scan(").unwrap();
        let observed = boundary.find("vfio_passive_observation_ready").unwrap();
        let cleanup = boundary.find("vfio_dma_cleanup_begin").unwrap();
        assert!(
            configured < tuned
                && tuned < rx_ready
                && rx_ready < scan
                && scan < observed
                && observed < cleanup
        );
        assert!(boundary.contains("min_channel_time: Some(150_000_000)"));
        assert!(boundary.contains("max_channel_time: Some(250_000_000)"));
        assert!(boundary.contains("passive_channel"));
        for forbidden in [
            "transmit_one_sae_auth",
            "configure_mgmt_tx_ring",
            "encode_mt7921_5ghz_auth_tx",
            "send_rate_power_bytes",
            "start_active_scan",
        ] {
            assert!(!boundary.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn live_wpa3_path_uses_one_pinned_owner_through_http() {
        let source = include_str!("vfio_read.rs");
        let exchange = source
            .split("if operation == Operation::RunOneShotSaeAuth {")
            .find(|segment| segment.contains("PinnedClientRuntime::new"))
            .unwrap()
            .split("let transport = adapter.into_transport();")
            .next()
            .unwrap();
        for required in [
            "Mt7921ClientDevice::new_with_ethernet",
            "fidl_internal::Protocol::Wpa3Personal",
            ".into_passphrase()",
            "PinnedClientRuntime::new",
            "runtime.connect(request, deadline)",
            "runtime.associated_data_pump()",
            ".prove_dhcp(&mut pump, deadline)",
            ".prove_dns(&mut pump, deadline)",
            ".prove_tcp(&mut pump, deadline)",
            ".prove_http(&mut pump, deadline)",
        ] {
            assert!(exchange.contains(required), "{required}");
        }
        let connect = exchange.find("runtime.connect(request, deadline)").unwrap();
        let dhcp = exchange.find(".prove_dhcp(&mut pump, deadline)").unwrap();
        let dns = exchange.find(".prove_dns(&mut pump, deadline)").unwrap();
        let tcp = exchange.find(".prove_tcp(&mut pump, deadline)").unwrap();
        let http = exchange.find(".prove_http(&mut pump, deadline)").unwrap();
        assert!(connect < dhcp && dhcp < dns && dns < tcp && tcp < http);
        for forbidden in [
            "SaeHandshake::new",
            "receive_one_sae_auth(",
            "association=false",
        ] {
            assert!(!exchange.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn sae_stage_reporting_cannot_gate_following_cleanup() {
        let cleaned = std::cell::Cell::new(false);
        emit_sae_stage_best_effort("simulated_would_block", |_| {
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
        });
        cleaned.set(true);
        assert!(cleaned.get());

        let source = include_str!("vfio_read.rs");
        let recorder = source
            .split("fn record_sae_stage(event: &str) {")
            .nth(1)
            .unwrap()
            .split("#[repr(C)]")
            .next()
            .unwrap();
        for forbidden in ["OpenOptions", "File::", "sync_all", "SAE_STAGE_PATH"] {
            assert!(!recorder.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn recovery_supervisor_disarms_after_proven_restore_even_when_experiment_failed() {
        let source = include_str!("../../lab/selector-write-recovery-supervisor.sh");
        let recovered = source
            .split("if ((${#states[@]} == 0))")
            .nth(1)
            .unwrap()
            .split("sleep 2")
            .next()
            .unwrap();
        assert!(!recovered.contains("experiment_rc == 0"));
        for proof in [
            "! $unsafe",
            "driver == mt7921e",
            "power == D0",
            "iwd_active == active",
            "$association && $dhcp && $default_route && $connectivity",
        ] {
            assert!(recovered.contains(proof), "{proof}");
        }
        let failure = recovered.find("experiment_rc != 0").unwrap();
        let disarm = recovered.find("wifi-lab-watchdog disarm").unwrap();
        let complete = recovered.find("COMPLETE realtime=").unwrap();
        assert!(disarm < failure && failure < complete);
        assert!(recovered.contains("reason=experiment_rc_$experiment_rc"));
    }

    #[test]
    fn recovery_supervisor_binds_sae_target_to_live_bss_and_channel_before_handoff() {
        let supervisor = include_str!("../../lab/selector-write-recovery-supervisor.sh");
        let derive = supervisor.find("device_path=$(readlink -f").unwrap();
        let iw = supervisor.find("iw dev").unwrap();
        let address = supervisor
            .find("client_mac=$(cat \"$net/address\"")
            .unwrap();
        let export = supervisor
            .find("export DRV_SAE_BSSID=$connected_bssid DRV_SAE_CHANNEL=$connected_channel")
            .unwrap();
        let handoff = supervisor.find("wifi-driver-lab \"$bdf\" 300").unwrap();
        assert!(derive < iw && iw < address && address < export && export < handoff);
        assert!(supervisor.contains("multiple connected target Wi-Fi interfaces"));
        assert!(supervisor.contains("target Wi-Fi interface is not connected"));
        assert!(supervisor.contains("DRV_SAE_CLIENT_MAC=$connected_client_mac"));
        assert!(supervisor.contains("local unicast VIF address"));
        let pre_handoff = &supervisor[..handoff];
        for forbidden in ["passphrase", "password", ".psk", "/var/lib/iwd"] {
            assert!(!pre_handoff.contains(forbidden), "{forbidden}");
        }
        let source = include_str!("vfio_read.rs");
        let target = source
            .split("let power_target =")
            .nth(1)
            .unwrap()
            .split("let mut sae_credential")
            .next()
            .unwrap();
        assert!(target.contains("DRV_SAE_CHANNEL"));
        assert!(target.contains("DRV_SAE_CLIENT_MAC"));
        assert!(target.contains("Some((bssid, ssid, channel, client))"));
        let channels = source
            .split("let channels = match operation")
            .nth(1)
            .unwrap()
            .split("let mut adapter")
            .next()
            .unwrap();
        assert!(channels.contains("power_target.as_ref().expect(\"SAE target\").2"));
        assert!(!channels.contains("WlanBand::FiveGhz, &[36]"));
    }

    #[test]
    fn recovery_supervisor_accepts_only_integer_or_dot_zero_iw_frequency() {
        let supervisor = include_str!("../../lab/selector-write-recovery-supervisor.sh");
        let function = supervisor
            .split("normalize_iw_frequency() {")
            .nth(1)
            .unwrap()
            .split("\n}")
            .next()
            .unwrap();
        let invoke = |value: &str| {
            Command::new("/run/current-system/sw/bin/bash")
                .arg("-c")
                .arg(format!(
                    "normalize_iw_frequency() {{{function}\n}}; normalize_iw_frequency '{value}'"
                ))
                .output()
                .unwrap()
        };
        for (fixture, expected) in [("5180", "5180\n"), ("5180.0", "5180\n")] {
            let output = invoke(fixture);
            assert!(output.status.success(), "{fixture}");
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        }
        for rejected in ["5180.5", "5180.", ".0", "five"] {
            assert!(!invoke(rejected).status.success(), "{rejected}");
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn handoff_client_identity_requires_one_local_unicast_address() {
        assert_eq!(
            parse_client_mac("8a:fd:2a:8b:70:5a").unwrap().bytes(),
            [0x8a, 0xfd, 0x2a, 0x8b, 0x70, 0x5a]
        );
        for invalid in [
            "",
            "50:5a:65:f6:f9:89",
            "8b:fd:2a:8b:70:5a",
            "00:00:00:00:00:00",
            "not-a-mac",
        ] {
            assert!(parse_client_mac(invalid).is_err(), "{invalid}");
        }
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn preauth_interface_context_is_dirty_before_submit_and_disabled_before_quiesce() {
        let source = include_str!("vfio_read.rs");
        let program = source
            .split("fn program_client_interface(")
            .nth(1)
            .unwrap()
            .split("fn disable_client_interface(")
            .next()
            .unwrap();
        let dev_dirty = program.find("dev_maybe_active: true").unwrap();
        let dev_submit = program
            .find("send_acknowledged_uni_command(1, &dev)")
            .unwrap();
        let bss_dirty = program.find(".bss_maybe_active = true").unwrap();
        let bss_submit = program
            .find("send_acknowledged_uni_command(2, &bss)")
            .unwrap();
        assert!(dev_dirty < dev_submit && dev_submit < bss_dirty && bss_dirty < bss_submit);

        let disable = source
            .split("fn disable_client_interface(")
            .nth(1)
            .unwrap()
            .split("fn send_acknowledged_uni_command(")
            .next()
            .unwrap();
        let bss_disable = disable
            .find("send_acknowledged_uni_command(2, &bss)")
            .unwrap();
        let dev_disable = disable
            .find("send_acknowledged_uni_command(1, &dev)")
            .unwrap();
        assert!(bss_disable < dev_disable);
        assert!(disable.contains("errors.push"));

        let cleanup = source
            .split("fn fail_closed_cleanup(&mut self, state: FirmwareLoaderState)")
            .nth(1)
            .unwrap()
            .split("#[derive(Debug)]")
            .next()
            .unwrap();
        let interface_disable = cleanup.find("self.disable_client_interface()").unwrap();
        let irq_disable = cleanup
            .find("write_pcie_mac_interrupt_enable_zero")
            .unwrap();
        let dma_disable = cleanup.find("write_active_wfdma(0xd4208").unwrap();
        assert!(interface_disable < irq_disable && interface_disable < dma_disable);
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn sae_uses_typed_vif_identity_without_overwriting_factory_identity() {
        let source = include_str!("vfio_read.rs");
        let target = source
            .split("let power_target =")
            .nth(1)
            .unwrap()
            .split("let mut sae_credential")
            .next()
            .unwrap();
        assert!(target.contains("parse_client_mac"));
        assert!(source.contains("struct ClientVifIdentity([u8; 6])"));

        let runtime = source
            .split("let effects = LiveClientEffects")
            .find(|part| part.contains("program_client_interface(client)"))
            .unwrap()
            .split("let passphrase")
            .next()
            .unwrap();
        assert!(runtime.matches(".bytes()").count() >= 2);
        assert!(runtime.contains("program_client_interface(client)"));
        assert!(!runtime.contains("query.factory_addr"));
    }

    #[test]
    fn sae_routes_to_consolidated_firmware_transport_not_early_dma_gate() {
        assert!(!Operation::RunOneShotSaeAuth.uses_contained_transport_gate());
        assert!(Operation::RunOneShotSaeAuth.records_active_transport_stages());
        assert!(Operation::RunOneShotSaeAuth.loads_firmware());
        assert!(Operation::RunOneShotSaeAuth.is_active_mcu());

        let source = include_str!("vfio_read.rs");
        let early_gate = source
            .split("if operation.uses_contained_transport_gate() {")
            .find(|segment| segment.contains("return Ok(None);"))
            .unwrap();
        assert!(early_gate.contains("run_contained_dma_resource_round_trip"));

        let consolidated = source
            .split("if operation.is_active_mcu() {")
            .find(|segment| segment.contains("let firmware_images = if operation.loads_firmware()"))
            .unwrap();
        for required in [
            "decompress_patch()",
            "decompress_ram()",
            "load_mt7921_firmware_with_passive_boundary",
            "Operation::RunOneShotSaeAuth",
            "PinnedClientRuntime::new",
        ] {
            assert!(consolidated.contains(required), "{required}");
        }
    }

    #[test]
    fn sae_records_the_shared_host_preflight_and_vfio_boundaries() {
        assert!(Operation::RunOneShotSaeAuth.records_active_transport_stages());
        assert!(!Operation::RunOneShotSaeAuth.uses_contained_transport_gate());
        let source = include_str!("vfio_read.rs");
        let startup = source
            .split("record_sae_stage(\"credential_read\");")
            .nth(1)
            .unwrap()
            .split("let base_acquisition")
            .next()
            .unwrap();
        let bdf = startup.find("DRV_PCI_BDF").unwrap();
        let identity = startup.find("verify_pci_identity(&bdf)").unwrap();
        let watchdog = startup.find("verify_external_watchdog_armed").unwrap();
        let marker = startup
            .find("record_sae_stage(\"watchdog_verified\")")
            .unwrap();
        let vfio = startup
            .find("record_sae_stage(\"vfio_cdev_open_before\")")
            .unwrap();
        assert!(bdf < identity && identity < watchdog && watchdog < marker && marker < vfio);
        assert!(startup.matches("records_active_transport_stages()").count() >= 6);

        let post_attach = source
            .split("record_sae_stage(\"vfio_attach_iommufd_pt_after\");")
            .nth(1)
            .unwrap()
            .split("let mut info = RegionInfo")
            .next()
            .unwrap();
        let d0 = post_attach.find("verify_pci_dma_disabled(&bdf)").unwrap();
        let d0_marker = post_attach
            .find("vfio_attached_d0_preflight_already_ready")
            .unwrap();
        let device_info = post_attach.find("vfio_device_get_info_before").unwrap();
        let region_info = post_attach
            .find("vfio_device_get_region_info_before")
            .unwrap();
        let contained = post_attach
            .find("if operation.uses_contained_transport_gate()")
            .unwrap();
        assert!(
            d0 < d0_marker
                && d0_marker < device_info
                && device_info < region_info
                && region_info < contained
        );
    }

    #[test]
    fn sae_tx_dma_authority_is_lazy_and_revoked_before_reset() {
        let source = include_str!("vfio_read.rs");
        let generic_acquisition = source
            .split("fn acquire_active_vfio_resources(")
            .nth(1)
            .unwrap()
            .split("fn acquire_sae_tx_resources(")
            .next()
            .unwrap();
        assert!(!generic_acquisition.contains("map_dma!(mgmt_"));

        let sae = source
            .split("if operation == Operation::RunOneShotSaeAuth {")
            .find(|segment| segment.contains("PinnedClientRuntime::new"))
            .unwrap()
            .split("let transport = adapter.into_transport();")
            .next()
            .unwrap();
        let power = sae.find("program_live_rate_power").unwrap();
        let acquire = sae.find("acquire_sae_tx_resources").unwrap();
        let runtime = sae.find("PinnedClientRuntime::new").unwrap();
        let connect = sae.find("runtime.connect(request, deadline)").unwrap();
        assert!(power < acquire && acquire < runtime && runtime < connect);
        assert!(!sae.contains("transmit_one_sae_auth"));

        let cleanup = source
            .split("ledger.phase = RunPhase::Containing;")
            .nth(1)
            .unwrap()
            .split("post-reset safe-state verification")
            .next()
            .unwrap();
        let mgmt = cleanup.find("ActiveArenaKind::MgmtRing").unwrap();
        let reset = cleanup.find("reset_vfio_device(&device)").unwrap();
        assert!(mgmt < reset);
    }

    #[test]
    fn firmware_bootstrap_boundary_and_cleanup_source_shape() {
        let source = include_str!("vfio_read.rs");
        let dispatch = source
            .split("let result = if operation == Operation::RunOneShotFirmware")
            .nth(1)
            .unwrap()
            .split("let report =")
            .next()
            .unwrap();
        assert!(dispatch.starts_with(" {\n                    load_mt7921_firmware_bootstrap"));

        let cleanup = source
            .split("ledger.phase = RunPhase::Containing;")
            .nth(1)
            .unwrap()
            .split("if !release_errors.is_empty()")
            .next()
            .unwrap();
        let mask = cleanup.find("write_active_wfdma(0xd4204, 0)").unwrap();
        let disable_dma = cleanup
            .find("write_active_wfdma(0xd4208, disabled)")
            .unwrap();
        let bme = cleanup.find("set_pci_bus_master(&bdf, false)").unwrap();
        let irq = cleanup.find("installed.disable()").unwrap();
        let unmap = cleanup.find("attempt_all_cleanup(").unwrap();
        let reset = cleanup.find("reset_vfio_device(&device)").unwrap();
        let verify = cleanup.find("verify_active_reset_containment").unwrap();
        assert!(mask < disable_dma && disable_dma < bme && bme < irq);
        assert!(irq < unmap && unmap < reset && reset < verify);
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
                mapping: None,
                ptr: Some(self.ptr),
                len: self.len,
                iova,
            }
        }

        fn read_page(&self) -> ReadPage {
            assert_eq!(self.len, PAGE);
            ReadPage {
                mapping: None,
                ptr: Some(self.ptr),
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
    fn ring4_demuxes_exchange18_txs_and_paired_tx_free_before_mcu_parsing() {
        let ring_mapping = TestMapping::new(PAGE);
        let buffer_mapping = TestMapping::new(8 * 2048);
        let page_mapping = TestMapping::new(PAGE);
        let mut ring = ring_mapping.dma(0x0100_0000);
        let mut buffers = buffer_mapping.dma(0x0101_0000);
        let page = page_mapping.read_page();
        let txs = [
            0x28, 0x00, 0x01, 0x00, 0x00, 0x00, 0x36, 0x00, 0x4b, 0x80, 0x00, 0x80, 0x00, 0x03,
            0x0b, 0x00, 0x09, 0x00, 0x13, 0x04, 0x00, 0x00, 0x00, 0x03, 0xf7, 0x07, 0x1c, 0x00,
            0xfd, 0xe7, 0x00, 0x82, 0xff, 0xff, 0xff, 0xff, 0x63, 0x62, 0xff, 0xff,
        ];
        buffers.write_bytes_at(0, &txs).unwrap();
        let mut tx_free = [0u8; 16];
        tx_free[0..4].copy_from_slice(&((6u32 << 27) | (1 << 16) | 16).to_le_bytes());
        tx_free[8..12].copy_from_slice(&((1u32 << 31) | (19 << 14)).to_le_bytes());
        tx_free[12..16].copy_from_slice(&1u32.to_le_bytes());
        buffers.write_bytes_at(2048, &tx_free).unwrap();
        ring.write_descriptor_at(
            1,
            DmaDescriptor {
                buf0: buffers.iova as u32 + 2048,
                ctrl: (1 << 31) | (1 << 30) | (16 << 16),
                buf1: 0,
                info: 0,
            },
        );
        ring.write_descriptor_at(
            0,
            DmaDescriptor {
                buf0: buffers.iova as u32,
                ctrl: 0xc028_0000,
                buf1: 0,
                info: 0,
            },
        );
        let mut queue = ActiveMcuRx {
            rx_ring: &mut ring,
            rx_buffers: &buffers,
            rx_tail: 0,
            rx_head: 7,
            rx_ring_index: 4,
            rx_count: 8,
            irq_bit: WM2_RX_IRQ_BIT,
        };
        let mut unsolicited = Vec::new();
        let mut normal = VecDeque::new();
        let mut completions = Vec::new();
        let mut provenance = DescriptorProvenance::new();
        assert!(
            drain_rx_queue(
                &page,
                &mut queue,
                None,
                &mut unsolicited,
                &mut normal,
                &mut completions,
                &mut provenance,
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            completions,
            [
                MgmtTxCompletion::Status(Mt7921TxStatus {
                    wcid: 19,
                    pid: 3,
                    acked: true,
                }),
                MgmtTxCompletion::Free(Mt7921TxFree {
                    wcid: Some(19),
                    token: 0,
                    dropped: false,
                    attempts: 1,
                }),
            ]
        );
        assert!(unsolicited.is_empty());
        assert!(normal.is_empty());
        assert_eq!(queue.rx_tail, 2);
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
        let mut normal = VecDeque::new();
        let mut completions = Vec::new();
        let mut provenance = DescriptorProvenance::new();
        assert!(
            drain_rx_queue(
                &page,
                &mut queue,
                None,
                &mut unsolicited,
                &mut normal,
                &mut completions,
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
        let mut normal = VecDeque::new();
        let mut completions = Vec::new();
        let mut provenance = DescriptorProvenance::new();
        let lease = Arc::clone(&provenance.lease);
        let error = match drain_rx_queue(
            &page,
            &mut queue,
            None,
            &mut unsolicited,
            &mut normal,
            &mut completions,
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
    fn e2e48_hdr_trans_error_without_translation_preserves_raw_eapol() {
        let client = [1, 2, 3, 4, 5, 6];
        let peer = [6, 5, 4, 3, 2, 1];
        let rx = e2e48_translation_error_eapol_frame(client, peer);
        let parsed = parse_connac2_rx_frame(&rx).unwrap();
        assert_eq!(&parsed.bytes[4..10], &client);
        assert_eq!(&parsed.bytes[10..22], &[peer, peer].concat());
        assert_eq!(&parsed.bytes[24..32], &[0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);

        let mut malformed = rx;
        malformed[0..4].copy_from_slice(&((2u32 << 27) | 55).to_le_bytes());
        assert_eq!(
            parse_connac2_rx_frame(&malformed),
            Err(PassiveRxError::Truncated)
        );
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn data_candidate_classification_uses_exact_ds_qos_ht_and_llc_offsets() {
        let client = [2, 0, 0, 0, 0, 1];
        let peer = [2, 0, 0, 0, 0, 2];
        let make = |control: u16, receiver: [u8; 6], ether_type: u16| {
            let to_ds = control & 0x0100 != 0;
            let from_ds = control & 0x0200 != 0;
            let qos = (control >> 4) & 8 != 0;
            let htc = control & 0x8000 != 0;
            let header_len = 24
                + usize::from(to_ds && from_ds) * 6
                + usize::from(qos) * 2
                + usize::from(htc) * 4;
            let mut frame = vec![0; header_len + 10];
            frame[..2].copy_from_slice(&control.to_le_bytes());
            frame[4..10].copy_from_slice(&receiver);
            frame[10..16].copy_from_slice(&peer);
            frame[16..22].copy_from_slice(&peer);
            frame[header_len..header_len + 8].copy_from_slice(&[0xaa, 0xaa, 3, 0, 0, 0, 0, 0]);
            frame[header_len + 6..header_len + 8].copy_from_slice(&ether_type.to_be_bytes());
            frame
        };

        for control in [0x0208, 0x0288, 0x8388] {
            let frame = make(control, client, 0x888e);
            let classified = classify_client_data_frame(&frame, client, peer);
            assert_eq!(classified.llc_result, "valid");
            assert!(classified.snap_present);
            assert_eq!(classified.ether_type, Some(0x888e));
            assert!(classified.addr1_is_client);
            assert!(classified.addr2_is_peer);
            assert!(classified.addr3_is_bssid);
        }

        let foreign = make(0x0208, [9; 6], 0x888e);
        assert!(!classify_client_data_frame(&foreign, client, peer).addr1_is_client);
        let non_eapol = make(0x0208, client, 0x0800);
        assert_eq!(
            classify_client_data_frame(&non_eapol, client, peer).ether_type,
            Some(0x0800)
        );
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn actual_data_drain_routes_status77_and_rearms_before_advancing() {
        let ring_mapping = TestMapping::new(PAGE);
        let buffer_mapping = TestMapping::new(8 * 2048);
        let page_mapping = TestMapping::new(PAGE);
        let mut ring = ring_mapping.dma(0x0102_0000);
        let mut buffers = buffer_mapping.dma(0x0103_0000);
        let page = page_mapping.read_page();
        let client = [1, 2, 3, 4, 5, 6];
        let peer = [6, 5, 4, 3, 2, 1];
        let mut rx = vec![0; 24 + 8 + 32];
        let length = rx.len() as u32;
        rx[0..4].copy_from_slice(&((2u32 << 27) | length).to_le_bytes());
        rx[4..8].copy_from_slice(&(1u32 << 13).to_le_bytes());
        rx[12..16].copy_from_slice(&(1u32 << 8).to_le_bytes());
        rx[28..32].copy_from_slice(&0x7878u32.to_le_bytes());
        {
            let auth = &mut rx[32..];
            auth[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
            auth[4..10].copy_from_slice(&client);
            auth[10..16].copy_from_slice(&peer);
            auth[16..22].copy_from_slice(&peer);
            auth[24..26].copy_from_slice(&3u16.to_le_bytes());
            auth[26..28].copy_from_slice(&1u16.to_le_bytes());
            auth[28..30].copy_from_slice(&77u16.to_le_bytes());
            auth[30..32].copy_from_slice(&20u16.to_le_bytes());
        }
        buffers.write_bytes_at(0, &rx).unwrap();
        ring.write_descriptor_at(
            0,
            DmaDescriptor {
                buf0: buffers.iova as u32,
                ctrl: (1 << 31) | (1 << 30) | (length << 16),
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
        let mut normal = VecDeque::new();
        assert!(
            drain_data_rx_queue(
                &page,
                &mut queue,
                &mut provenance,
                &mut Vec::new(),
                Some(&mut normal),
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(normal.len(), 1);
        let parsed = parse_connac2_rx_frame(&normal[0].bytes).unwrap();
        assert_eq!(parsed.bytes, rx[32..]);
        assert_eq!(
            classify_preassociation_sae_auth(&parsed.bytes, client, peer)
                .unwrap()
                .unwrap(),
            mt7921_port_spike::PreAssociationSaeAuth {
                transaction: 1,
                status: 77,
            }
        );
        assert!(matches!(
            provenance.effects.as_slice(),
            [
                DescriptorProvenanceEffect::Mint(_),
                DescriptorProvenanceEffect::Rearm {
                    route: DescriptorOccurrenceRoute::DataRx,
                    ring: 2,
                    slot: 7,
                    ..
                }
            ]
        ));
        assert_eq!((queue.rx_tail, queue.rx_head), (1, 0));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn status77_descriptor_rearm_reaches_pinned_runtime_group19_fallback() {
        futures::executor::block_on(async {
            let ring_mapping = TestMapping::new(PAGE);
            let buffer_mapping = TestMapping::new(8 * 2048);
            let page_mapping = TestMapping::new(PAGE);
            let mut ring = ring_mapping.dma(0x0102_0000);
            let mut buffers = buffer_mapping.dma(0x0103_0000);
            let page = page_mapping.read_page();
            let client = [2, 0, 0, 0, 0, 1];
            let peer = [2, 0, 0, 0, 0, 2];
            let channel = ChannelNumber {
                band: WlanBand::FiveGhz,
                number: 36,
            };

            // Physical E2E shape: 24-byte RXD + GROUP4 + GROUP2 +
            // GROUP3/RXV + 32-byte status-77 Authentication frame.
            let mut rx = vec![0; 24 + 16 + 8 + 8 + 32];
            let length = rx.len() as u32;
            rx[0..4].copy_from_slice(&((7u32 << 27) | (1 << 16) | length).to_le_bytes());
            rx[4..8].copy_from_slice(&((1u32 << 14) | (1 << 12) | (1 << 13)).to_le_bytes());
            rx[12..16].copy_from_slice(&(1u32 << 8).to_le_bytes());
            rx[52..56].copy_from_slice(&0x7878u32.to_le_bytes());
            {
                let auth = &mut rx[56..];
                auth[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
                auth[4..10].copy_from_slice(&client);
                auth[10..16].copy_from_slice(&peer);
                auth[16..22].copy_from_slice(&peer);
                auth[24..26].copy_from_slice(&3u16.to_le_bytes());
                auth[26..28].copy_from_slice(&1u16.to_le_bytes());
                auth[28..30].copy_from_slice(&77u16.to_le_bytes());
                auth[30..32].copy_from_slice(&20u16.to_le_bytes());
            }
            buffers.write_bytes_at(0, &rx).unwrap();
            ring.write_descriptor_at(
                0,
                DmaDescriptor {
                    buf0: buffers.iova as u32,
                    ctrl: (1 << 31) | (1 << 30) | (length << 16),
                    buf1: 0,
                    info: 0,
                },
            );
            let mut queue = ActiveMcuRx {
                rx_ring: &mut ring,
                rx_buffers: &buffers,
                rx_tail: 0,
                rx_head: 7,
                rx_ring_index: 4,
                rx_count: 8,
                irq_bit: WM2_RX_IRQ_BIT,
            };
            let mut provenance = DescriptorProvenance::new();
            let mut normal = VecDeque::new();
            let mut unsolicited = Vec::new();
            let mut completions = Vec::new();
            assert!(
                drain_rx_queue(
                    &page,
                    &mut queue,
                    None,
                    &mut unsolicited,
                    &mut normal,
                    &mut completions,
                    &mut provenance,
                )
                .unwrap()
                .is_none()
            );
            assert_eq!((queue.rx_tail, queue.rx_head), (1, 0));
            assert!(matches!(
                provenance.effects.as_slice(),
                [
                    DescriptorProvenanceEffect::Mint(_),
                    DescriptorProvenanceEffect::Rearm {
                        route: DescriptorOccurrenceRoute::McuNormalRx,
                        ring: 4,
                        slot: 7,
                        ..
                    }
                ]
            ));
            let parsed = parse_connac2_rx_frame(&normal.pop_front().unwrap().bytes).unwrap();
            let status77 = ClientRxFrame {
                bytes: parsed.bytes,
                status: fidl_softmac::WlanRxInfo {
                    rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
                    valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
                    phy: fidl_ieee80211::WlanPhyType::Ofdm,
                    data_rate: 0,
                    primary: channel,
                    bandwidth: ChannelBandwidth::Cbw20,
                    vht_secondary_80_channel: ChannelNumber {
                        number: 0,
                        ..channel
                    },
                    mcs: 0,
                    rssi_dbm: parsed.rssi_dbm,
                    snr_dbh: 0,
                },
                security: None,
            };

            let capability = mt7921_port_spike::NicCapability {
                element_count: 2,
                mac_address: Some(client),
                phy: Some(mt7921_port_spike::NicPhyCapability {
                    ht: true,
                    vht: true,
                    has_5ghz: true,
                    max_bandwidth: 2,
                    spatial_streams: 2,
                    hardware_path: 3,
                    he: true,
                }),
                has_6ghz: Some(false),
                chip_capability: None,
                unknown_elements: 0,
            };
            let transmitted = Arc::new(Mutex::new(Vec::new()));
            let transport = SourceExactPassiveTransport::new(
                FallbackMechanics {
                    rx: VecDeque::new(),
                    pending_status77: Some(status77),
                    tx: Arc::clone(&transmitted),
                },
                capability,
            )
            .unwrap();
            let candidates = candidate_channels(capability);
            let adapter =
                Mt7921SoftmacAdapter::new(transport, capability, candidates.clone(), vec![channel])
                    .unwrap();
            let physical = client_physical_channel(
                channel,
                ChannelBandwidth::Cbw80,
                ChannelNumber {
                    number: 0,
                    ..channel
                },
            )
            .unwrap();
            let shared = Arc::new(Mutex::new(selected_live_state(peer, physical)));
            let effects = LiveClientEffects {
                state: Arc::clone(&shared),
                target: peer,
                client,
                rcpi: 100,
                firmware: ClientFirmwareEffectsState::default(),
                post_association_data_wait: None,
                eapol_start_deadline: None,
                eapol_start_emitted: false,
            };
            let support = live_client_support(query_from_capabilities(capability, &candidates));
            let device_info =
                wlan_mlme::mlme_device_info_from_softmac(support.query.clone()).unwrap();
            let security = support.security.clone();
            let spectrum = support.spectrum_management.clone();
            let (mut device, runner) = Mt7921ClientDevice::new(effects, adapter, support);
            DeviceOps::set_channel(
                &mut device,
                channel,
                ChannelBandwidth::Cbw80,
                ChannelNumber {
                    number: 0,
                    ..channel
                },
            )
            .await
            .unwrap();
            {
                let state = &mut shared.lock().unwrap();
                state
                    .mark_rate_power_ready(
                        peer,
                        channel,
                        ChannelBandwidth::Cbw80,
                        ChannelNumber {
                            number: 0,
                            ..channel
                        },
                    )
                    .unwrap();
                state
                    .authorize_sae(
                        peer,
                        channel,
                        ChannelBandwidth::Cbw80,
                        ChannelNumber {
                            number: 0,
                            ..channel
                        },
                    )
                    .unwrap();
            }
            let mut runtime = PinnedClientRuntime::new(
                device,
                runner,
                {
                    let mut config = wlan_sme::client::ClientConfig::default();
                    config.wpa3_supported = true;
                    config
                },
                device_info,
                security,
                spectrum,
                fuchsia_inspect::Inspector::default(),
            )
            .await
            .unwrap();
            let request = fidl_sme::ConnectRequest {
                ssid: b"test".to_vec(),
                bss_description: fidl_ieee80211::BssDescription {
                    bssid: peer,
                    bss_type: fidl_ieee80211::BssType::Infrastructure,
                    beacon_period: 100,
                    capability_info: 0x11,
                    ies: vec![
                        0, 4, b't', b'e', b's', b't', 1, 2, 0x8c, 0x12, 48, 20, 1, 0, 0, 0x0f,
                        0xac, 4, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 8, 0xcc, 0, 244, 1,
                        0x20,
                    ],
                    primary: channel,
                    bandwidth: ChannelBandwidth::Cbw80,
                    vht_secondary_80_channel: ChannelNumber {
                        number: 0,
                        ..channel
                    },
                    rssi_dbm: -40,
                    snr_db: 20,
                },
                multiple_bss_candidates: false,
                authentication: fidl_internal::Authentication {
                    protocol: fidl_internal::Protocol::Wpa3Personal,
                    credentials: Some(Box::new(fidl_internal::Credentials::Wpa(
                        fidl_internal::WpaCredentials::Passphrase(b"synthetic-password".to_vec()),
                    ))),
                },
                deprecated_scan_type: fidl_common::ScanType::Passive,
            };
            assert_eq!(
                runtime
                    .connect(
                        request,
                        Instant::now() + std::time::Duration::from_millis(100),
                    )
                    .await,
                Err(mt7921_softmac_adapter::client_device::PinnedConnectError::Timeout)
            );
            let transmitted = transmitted.lock().unwrap();
            assert!(transmitted.len() >= 2);
            assert_eq!(&transmitted[0][28..32], &[126, 0, 20, 0]);
            assert_eq!(&transmitted[1][28..32], &[126, 0, 19, 0]);
            assert_eq!(
                &transmitted[1][transmitted[1].len() - 5..],
                &[0xff, 0x03, 0x5c, 0x14, 0x00]
            );
        });
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
        let error =
            match drain_data_rx_queue(&page, &mut queue, &mut provenance, &mut Vec::new(), None) {
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

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn hardware_bad_rx_frame_is_rearmed_and_filtered_nonterminally() {
        let ring_mapping = TestMapping::new(PAGE);
        let buffer_mapping = TestMapping::new(8 * 2048);
        let page_mapping = TestMapping::new(PAGE);
        let mut ring = ring_mapping.dma(0x0102_0000);
        let mut buffers = buffer_mapping.dma(0x0103_0000);
        let page = page_mapping.read_page();
        let mut rx = vec![0; 34];
        let rx_len = rx.len() as u32;
        rx[0..4].copy_from_slice(&((2u32 << 27) | rx_len).to_le_bytes());
        rx[4..8].copy_from_slice(&((1u32 << 13) | (1 << 27)).to_le_bytes());
        rx[12..16].copy_from_slice(&(36u32 << 8).to_le_bytes());
        buffers.write_bytes_at(0, &rx).unwrap();
        ring.write_descriptor_at(
            0,
            DmaDescriptor {
                buf0: buffers.iova as u32,
                ctrl: (1 << 31) | (1 << 30) | (rx_len << 16),
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
        assert!(
            drain_data_rx_queue(
                &page,
                &mut queue,
                &mut DescriptorProvenance::new(),
                &mut Vec::new(),
                Some(&mut VecDeque::new()),
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!((queue.rx_tail, queue.rx_head), (1, 0));
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
        assert!(active_wfdma_write_allowed(0xd420c, 1, passive));
        assert!(!active_wfdma_write_allowed(0xd420c, 0, passive));
        assert!(!active_wfdma_write_allowed(0xd420c, u32::MAX, passive));
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
        required.push(passive_mac_bar_offset(0x820d_8700).unwrap() & !(PAGE - 1));
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
        capsule.ioas = Some(Ioas::from_allocated(&iommu, 1));
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
    fn management_tx_reclaims_only_ring0_and_preserves_global_tx() {
        let source = include_str!("vfio_read.rs");
        let reset = source
            .split("fn reset_consumed_mgmt_tx_ring(")
            .nth(1)
            .unwrap()
            .split("fn configure_mgmt_tx_ring_for_submission(")
            .next()
            .unwrap();
        assert!(reset.contains("write_active_wfdma(0xd4308, 0)"));
        assert!(!reset.contains("write_active_wfdma(0xd430c"));
        assert!(reset.contains("write_active_wfdma(0xd420c, 1)"));
        assert!(reset.contains("read(0xd4308)"));
        assert!(reset.contains("read(0xd430c)"));
        assert!(reset.contains("management_tx_ring_reclaimed"));
        assert!(reset.contains("uni_poisoned"));
        assert!(!reset.contains("write_active_wfdma(0xd4208"));
        assert!(!reset.contains("write_active_wfdma(0xd4100"));
        assert!(!reset.contains("write_rx_ring_slot"));
        assert!(!reset.contains("write_rx_cpu_index"));
        assert!(!reset.contains("0xd45"));

        let configure = source
            .split("fn configure_mgmt_tx_ring_for_submission(")
            .nth(1)
            .unwrap()
            .split("fn transmit_one_sae_auth(")
            .next()
            .unwrap();
        assert!(!configure.contains("write_active_wfdma(0xd4208"));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn management_tx_commits_before_asynchronous_rx_completion() {
        let source = include_str!("vfio_read.rs");
        let transmit = source
            .split("fn transmit_one_sae_auth(")
            .nth(1)
            .unwrap()
            .split("fn receive_one_sae_auth(")
            .next()
            .unwrap();
        let publish = transmit.find("write_active_wfdma(0xd4308, 1)").unwrap();
        let ownership = transmit
            .find("configure_mgmt_tx_ring_for_submission(ring)")
            .unwrap();
        let wipe = transmit.find("stage=buffer_wipe result=complete").unwrap();
        let identity = transmit.find("stage=identity result=allocated").unwrap();
        let didx = transmit.find("read(0xd430c)").unwrap();
        let descriptor_done = transmit.find("is_dma_done()").unwrap();
        let committed = transmit
            .find("MgmtTxPublicationOutcome::Committed")
            .unwrap();
        let enqueue_success = transmit.rfind("Ok(())").unwrap();
        assert!(ownership < wipe && wipe < identity && identity < publish);
        assert!(publish < didx && didx < descriptor_done && descriptor_done < committed);
        assert!(committed < enqueue_success);
        assert!(!transmit.contains("drain_data_rx_queue("));
        assert!(!transmit.contains("TX completion timed out"));
        let publication_intent = transmit
            .find("MgmtTxPublicationOutcome::AmbiguousOwnership")
            .unwrap();
        let poison = publication_intent
            + transmit[publication_intent..]
                .find("self.loader.uni_terminal_poisoned = true")
                .unwrap();
        let deferred_reclaim = transmit.find("next=reclaim_deferred").unwrap();
        assert!(publication_intent < publish);
        assert!(publish < poison && poison < deferred_reclaim);
        assert!(!transmit[deferred_reclaim..].contains("reset_consumed_mgmt_tx_ring()"));
        assert!(!transmit.contains("write_active_wfdma(0xd4208"));
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn completed_management_burst_leaves_mcu_transport_enabled() {
        let source = include_str!("vfio_read.rs");
        let transmit = source
            .split("fn transmit_one_sae_auth(")
            .nth(1)
            .unwrap()
            .split("fn receive_one_sae_auth(")
            .next()
            .unwrap();
        assert!(transmit.contains("MgmtTxPublicationOutcome::Committed"));
        assert!(transmit.contains("next=reclaim_deferred"));
        let configure = source
            .split("fn configure_mgmt_tx_ring_for_submission(")
            .nth(1)
            .unwrap()
            .split("fn transmit_one_sae_auth(")
            .next()
            .unwrap();
        assert!(configure.contains("reset_consumed_mgmt_tx_ring()"));
        assert!(!transmit.contains("global & !1"));

        let unified = source
            .split("fn send_acknowledged_uni_command(")
            .nth(1)
            .unwrap()
            .split("fn send_passive_command(")
            .next()
            .unwrap();
        assert!(unified.contains("uni_ring_pre_publish"));
        assert!(unified.contains("publish_mcu_bytes("));
        assert!(!unified.contains("configure_mgmt_tx_ring"));

        let cleanup = source.split("fn fail_closed_cleanup(").nth(1).unwrap();
        assert!(cleanup.contains("write_active_wfdma(0xd4208, disabled)"));
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
            let mut state = MgmtTxCompletionState::new(0, 3);
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
        let mut state = MgmtTxCompletionState::new(0, 3);
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

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn committed_management_frames_keep_distinct_identities_until_delayed_completion() {
        let mut outstanding = MgmtTxOutstanding::default();
        let first = outstanding.reserve().unwrap();
        let second = outstanding.reserve().unwrap();
        assert_ne!(
            first.0, second.0,
            "group fallback must not reuse the TX token"
        );
        assert_ne!(
            first.1, second.1,
            "group fallback must not reuse the TX PID"
        );
        assert_eq!(outstanding.entries.len(), 2);

        for completion in [
            MgmtTxCompletion::Status(Mt7921TxStatus {
                wcid: 19,
                pid: second.1,
                acked: true,
            }),
            MgmtTxCompletion::Free(Mt7921TxFree {
                wcid: None,
                token: first.0,
                dropped: false,
                attempts: 1,
            }),
            MgmtTxCompletion::Free(Mt7921TxFree {
                wcid: None,
                token: second.0,
                dropped: false,
                attempts: 1,
            }),
            MgmtTxCompletion::Status(Mt7921TxStatus {
                wcid: 19,
                pid: first.1,
                acked: true,
            }),
        ] {
            outstanding.observe(completion).unwrap();
        }
        assert!(
            outstanding.is_empty(),
            "both delayed completion pairs must retire"
        );
    }

    #[cfg(feature = "fuchsia-passive")]
    #[test]
    fn unpublished_or_missing_completion_never_reuses_management_identity() {
        let mut outstanding = MgmtTxOutstanding::default();
        let unpublished = outstanding.reserve().unwrap();
        outstanding.abandon_last(unpublished.0, unpublished.1);
        let committed = outstanding.reserve().unwrap();
        assert_ne!(
            unpublished, committed,
            "pre-publication failure must not recycle identity"
        );
        let later = outstanding.reserve().unwrap();
        assert_ne!(
            committed, later,
            "missing completion must block identity reuse"
        );
        assert_eq!(outstanding.entries.len(), 2);

        let source = include_str!("vfio_read.rs");
        let transmit = source
            .split("fn transmit_one_sae_auth(")
            .nth(1)
            .unwrap()
            .split("fn receive_one_sae_auth(")
            .next()
            .unwrap();
        assert!(transmit.contains("MgmtTxPublicationOutcome::AmbiguousOwnership"));
        assert!(transmit.contains("uni_terminal_poisoned = true"));
        assert!(transmit.contains("MgmtTxPublicationOutcome::Committed"));
        assert!(!transmit.contains("TX completion timed out"));
    }
}
