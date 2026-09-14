#![forbid(unsafe_code)]

//! Owned physical resources for the production MT7921 SoftMAC client.
//!
//! Setup opens [`LinuxVfioPciCapabilities`] before sandbox lockdown. After
//! lockdown, the single firmware-bootstrap operation consumes that inert authority and
//! owns the activated `LinuxVfio` backend (inside `Device<LinuxVfio>`), its
//! IOAS, PCI control descriptor, BAR mapping, DMA arenas, and interrupt. Active
//! data-path authority remains private while containment is brought under this
//! owner. Policy/effects and lab telemetry deliberately remain outside this
//! crate.

mod activation;
mod active_mcu;
mod firmware_loader;
mod receive;
pub use receive::ReceivedEvent;
mod peer;
mod radio;
mod setup_inputs;
mod softmac;
pub use setup_inputs::{
    CredentialBytes, CredentialFile, FirmwareImageExpectation, FirmwareImageKind,
    FirmwareVerificationError, RegulatoryDatabaseFile, VerifiedFirmware, VerifiedFirmwareImages,
    VerifiedRegulatoryDatabase,
};

use activation::activate;
use active_mcu::ActiveMcuProtocol;
#[cfg(test)]
use active_mcu::{ActiveMcuViews, TransactionError};
use drv_hardware::{
    Backend, Bidirectional, CoherentDma, Device, DmaConstraints, FromDevice, Interrupt, MmioRegion,
    ToDevice,
};
use drv_hardware_backends::{
    LinuxVfio, LinuxVfioError, LinuxVfioPciCapabilities, LockedLinuxVfioPciCapabilities, PciControl,
};
use firmware_loader::ProductionFirmwareLoader;
use mt7921_core::{
    ActivationFailure, ActivationStage, ActivationState, FirmwareLoaderError, FirmwareLoaderReport,
    MT7921_DATA_RX_RING_COUNT, MT7921_LOADER_COMMAND_MAX_BYTES, MT7921_MCU_RX_BUFFER_BYTES,
};
#[cfg(test)]
use mt7921_core::{OwnershipTransport, PCIE_LPCR_HOST_CLR_OWN, acquire_driver_ownership};
#[cfg(test)]
use std::time::Duration;
use std::{
    fmt,
    path::{Path, PathBuf},
    time::Instant,
};

const PAGE: usize = 4096;
const MT7921_BAR0_BYTES: usize = 0x10_0000;
const MCU_COMMAND_PAYLOAD_BYTES: usize = MT7921_LOADER_COMMAND_MAX_BYTES;

/// Inert setup result that can cross the sandbox-lockdown boundary.
pub struct Mt7921HardwareSessionConfig {
    vfio: LockedLinuxVfioPciCapabilities,
}

pub struct Mt7921HardwareSessionSetup {
    vfio: LinuxVfioPciCapabilities,
}

impl Mt7921HardwareSessionConfig {
    /// Open the PCI config, VFIO cdev, and `/dev/iommu` descriptors only.
    ///
    /// This setup phase performs no ioctl, mapping, PCI config access, or
    /// device access. Pass the result to [`run_firmware_bootstrap`] after lockdown.
    pub fn setup(
        vfio_cdev: impl AsRef<Path>,
        bdf: &str,
    ) -> Result<Mt7921HardwareSessionSetup, LinuxVfioError> {
        let paths = verified_vfio_pci_paths(
            vfio_cdev.as_ref(),
            bdf,
            Path::new("/sys/bus/pci/devices"),
            Path::new("/dev/vfio/devices"),
        )?;
        Ok(Mt7921HardwareSessionSetup {
            vfio: LinuxVfioPciCapabilities::open(paths.vfio_cdev, paths.pci_config)?,
        })
    }
}

#[derive(Debug)]
struct VerifiedVfioPciPaths {
    vfio_cdev: PathBuf,
    pci_config: PathBuf,
}

fn verified_vfio_pci_paths(
    configured_cdev: &Path,
    bdf: &str,
    pci_devices: &Path,
    vfio_devices: &Path,
) -> Result<VerifiedVfioPciPaths, LinuxVfioError> {
    let valid_bdf = bdf.len() == 12
        && bdf.as_bytes()[4] == b':'
        && bdf.as_bytes()[7] == b':'
        && bdf.as_bytes()[10] == b'.'
        && bdf.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10) || byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
        });
    if !valid_bdf {
        return Err(LinuxVfioError::Setup(
            "PCI BDF is not canonical dddd:bb:dd.f".into(),
        ));
    }
    let device = std::fs::canonicalize(pci_devices.join(bdf))
        .map_err(|error| LinuxVfioError::Setup(format!("resolve PCI BDF {bdf}: {error}")))?;
    if device.file_name().and_then(|name| name.to_str()) != Some(bdf) {
        return Err(LinuxVfioError::Setup(
            "PCI BDF symlink resolved to a different endpoint".into(),
        ));
    }

    let mut cdevs = std::fs::read_dir(device.join("vfio-dev"))
        .map_err(|error| LinuxVfioError::Setup(format!("enumerate VFIO cdev for {bdf}: {error}")))?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            LinuxVfioError::Setup(format!("enumerate VFIO cdev for {bdf}: {error}"))
        })?;
    cdevs.sort();
    if cdevs.len() != 1
        || !cdevs[0]
            .as_encoded_bytes()
            .strip_prefix(b"vfio")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.iter().all(u8::is_ascii_digit))
    {
        return Err(LinuxVfioError::Setup(
            "PCI endpoint does not expose exactly one VFIO cdev".into(),
        ));
    }
    let derived_cdev = std::fs::canonicalize(vfio_devices.join(&cdevs[0]))
        .map_err(|error| LinuxVfioError::Setup(format!("resolve derived VFIO cdev: {error}")))?;
    let configured_cdev = std::fs::canonicalize(configured_cdev)
        .map_err(|error| LinuxVfioError::Setup(format!("resolve configured VFIO cdev: {error}")))?;
    if configured_cdev != derived_cdev {
        return Err(LinuxVfioError::Setup(
            "configured VFIO cdev does not belong to the requested PCI BDF".into(),
        ));
    }

    let group = std::fs::canonicalize(device.join("iommu_group"))
        .map_err(|error| LinuxVfioError::Setup(format!("resolve IOMMU group: {error}")))?;
    let members = std::fs::read_dir(group.join("devices"))
        .map_err(|error| LinuxVfioError::Setup(format!("enumerate IOMMU group: {error}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| LinuxVfioError::Setup(format!("enumerate IOMMU group: {error}")))?;
    if members.len() != 1 || members[0].file_name() != std::ffi::OsStr::new(bdf) {
        return Err(LinuxVfioError::Setup(
            "PCI endpoint does not exclusively own its complete IOMMU group".into(),
        ));
    }
    let driver = std::fs::canonicalize(members[0].path().join("driver"))
        .map_err(|error| LinuxVfioError::Setup(format!("resolve IOMMU member driver: {error}")))?;
    if driver.file_name().and_then(|name| name.to_str()) != Some("vfio-pci") {
        return Err(LinuxVfioError::Setup(
            "IOMMU group member is not bound to vfio-pci".into(),
        ));
    }

    Ok(VerifiedVfioPciPaths {
        vfio_cdev: derived_cdev,
        pci_config: device.join("config"),
    })
}

impl Mt7921HardwareSessionSetup {
    /// Register the inert IRQ capability with the entered Linux service
    /// reactor before lockdown. Ownership stays inside the VFIO capability.
    pub fn with_async_interrupt(self) -> Result<Self, LinuxVfioError> {
        Ok(Self {
            vfio: self.vfio.with_async_interrupt()?,
        })
    }

    pub fn lock_down(self) -> Result<Mt7921HardwareSessionConfig, linux_self_sandbox::Error> {
        Ok(Mt7921HardwareSessionConfig {
            vfio: self.vfio.lock_down()?,
        })
    }

    /// Retain only the precreated protocol-runtime and control-channel FDs
    /// alongside the driver's device capabilities in the same sandbox.
    pub fn lock_down_with_service(
        self,
        service: linux_self_sandbox::WifiServiceFds,
    ) -> Result<Mt7921HardwareSessionConfig, linux_self_sandbox::Error> {
        Ok(Mt7921HardwareSessionConfig {
            vfio: self.vfio.lock_down_with_service(Some(service))?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HardwareResource {
    Bar0,
    TxGuard,
    FirmwareDownloadRing,
    McuTxRing,
    RxGuard,
    McuRxRing,
    McuRxBuffers,
    CommandPayloads,
    FirmwareDownloadPayload,
    WaRxRing,
    WaRxBuffers,
    DataRxRing,
    DataRxBuffers,
    ManagementTxwi,
    ManagementFrame,
    ManagementTxRing,
    Interrupt,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AcquisitionLedger {
    acquired: Vec<HardwareResource>,
}

impl AcquisitionLedger {
    pub fn acquired(&self) -> &[HardwareResource] {
        &self.acquired
    }

    fn record(&mut self, resource: HardwareResource) {
        self.acquired.push(resource);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainmentLedger {
    vfio_attached: bool,
    bar_mapped: bool,
    dma_mapped: bool,
    irq_installed: bool,
    bus_master_enabled: bool,
    bme_disabled_command: Option<u16>,
    transport_quiesced: bool,
    reset_generation: Option<u64>,
    post_reset_registers: Option<PostResetRegisters>,
    post_reset_pci: Option<PostResetPciSnapshot>,
}

impl ContainmentLedger {
    pub fn vfio_attached(&self) -> bool {
        self.vfio_attached
    }

    pub fn bar_mapped(&self) -> bool {
        self.bar_mapped
    }

    pub fn dma_mapped(&self) -> bool {
        self.dma_mapped
    }

    pub fn irq_installed(&self) -> bool {
        self.irq_installed
    }

    pub fn bus_master_enabled(&self) -> bool {
        self.bus_master_enabled
    }

    pub fn bme_disabled_command(&self) -> Option<u16> {
        self.bme_disabled_command
    }
    pub fn reset_generation(&self) -> Option<u64> {
        self.reset_generation
    }
    pub fn post_reset_registers(&self) -> Option<PostResetRegisters> {
        self.post_reset_registers
    }
    pub fn post_reset_pci(&self) -> Option<PostResetPciSnapshot> {
        self.post_reset_pci
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PostResetRegisters {
    pub wfdma_global_config: u32,
    pub host_interrupt_enable: u32,
    pub pcie_mac_interrupt_enable: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PostResetPciSnapshot {
    pub command: u16,
    pub power_state: u8,
    pub vendor: u16,
    pub device: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionLifecycle {
    ResourcesMappedDmaDisabled,
    Activating(ActivationStage),
    LoaderTransportActive,
    BootstrapQuiesced,
    FirmwareInitialized,
    ProtocolStarted,
    Closing,
    Contained,
}

#[cfg(test)]
fn ensure_operational(lifecycle: SessionLifecycle) -> Result<(), drv_hardware::Error> {
    if matches!(
        lifecycle,
        SessionLifecycle::ResourcesMappedDmaDisabled | SessionLifecycle::LoaderTransportActive
    ) {
        Ok(())
    } else {
        Err(drv_hardware::Error::StaleHandle)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainmentStage {
    QuiesceTransport,
    DisableBusMaster,
    Reset,
    PostResetRegisters,
    PostResetPci,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mt7921ContainmentError {
    stage: ContainmentStage,
    detail: String,
    ledger: ContainmentLedger,
}

impl Mt7921ContainmentError {
    pub fn stage(&self) -> ContainmentStage {
        self.stage
    }
    pub fn ledger(&self) -> &ContainmentLedger {
        &self.ledger
    }
}

impl fmt::Display for Mt7921ContainmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MT7921 containment failed at {:?}: {}",
            self.stage, self.detail
        )
    }
}

impl std::error::Error for Mt7921ContainmentError {}

#[derive(Debug)]
pub enum Mt7921HardwareSessionError {
    Activate(LinuxVfioError),
    WrongPciIdentity {
        vendor: u16,
        device: u16,
    },
    Acquire {
        resource: HardwareResource,
        source: drv_hardware::Error,
        ledger: AcquisitionLedger,
    },
}

impl fmt::Display for Mt7921HardwareSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Activate(error) => write!(f, "activate MT7921 VFIO device: {error}"),
            Self::WrongPciIdentity { vendor, device } => write!(
                f,
                "VFIO PCI identity is {vendor:04x}:{device:04x}, expected 14c3:7961"
            ),
            Self::Acquire {
                resource, source, ..
            } => write!(f, "acquire MT7921 {resource:?}: {source:?}"),
        }
    }
}

impl std::error::Error for Mt7921HardwareSessionError {}

#[allow(dead_code, reason = "staged arenas remain owned for containment")]
struct DmaArenas<B: Backend> {
    tx_guard: CoherentDma<B, Bidirectional>,
    fwdl_ring: CoherentDma<B, Bidirectional>,
    mcu_tx_ring: CoherentDma<B, Bidirectional>,
    rx_guard: CoherentDma<B, Bidirectional>,
    mcu_rx_ring: CoherentDma<B, Bidirectional>,
    mcu_rx_buffers: CoherentDma<B, FromDevice>,
    command_payloads: CoherentDma<B, ToDevice>,
    fwdl_payload: CoherentDma<B, ToDevice>,
    wa_rx_ring: CoherentDma<B, Bidirectional>,
    wa_rx_buffers: CoherentDma<B, FromDevice>,
    data_rx_ring: CoherentDma<B, Bidirectional>,
    data_rx_buffers: CoherentDma<B, FromDevice>,
    management_txwi: CoherentDma<B, ToDevice>,
    management_frame: CoherentDma<B, ToDevice>,
    management_tx_ring: CoherentDma<B, Bidirectional>,
}

struct OwnedHardwareResources<B: Backend> {
    // Release externally observable resources before the shared backend owner.
    #[allow(dead_code, reason = "IRQ ownership is retained through reset")]
    interrupt: Option<Interrupt<B>>,
    #[allow(dead_code, reason = "DMA ownership is retained through reset")]
    dma: DmaArenas<B>,
    bar0: MmioRegion<B>,
    device: Device<B>,
}

#[derive(Debug)]
struct AcquireFailure {
    resource: HardwareResource,
    source: drv_hardware::Error,
    ledger: AcquisitionLedger,
}

#[cfg(test)]
struct DriverOwnershipIo<B: Backend> {
    conn: MmioRegion<B>,
    start: Instant,
}

#[cfg(test)]
impl<B: Backend> OwnershipTransport for DriverOwnershipIo<B> {
    type Error = drv_hardware::Error;

    fn now_ms(&self) -> u64 {
        self.start
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn write_clear_own(&mut self) -> Result<(), Self::Error> {
        self.conn.write_u32(0x10, PCIE_LPCR_HOST_CLR_OWN)
    }

    fn read_low_power_control(&mut self) -> Result<u32, Self::Error> {
        self.conn.read_u32(0x10)
    }

    fn sleep_ms(&mut self, milliseconds: u64) {
        std::thread::sleep(Duration::from_millis(milliseconds));
    }
}

impl<B: Backend> OwnedHardwareResources<B> {
    fn acquire(device: Device<B>) -> Result<(Self, AcquisitionLedger), AcquireFailure> {
        let mut ledger = AcquisitionLedger::default();
        macro_rules! acquire {
            ($resource:ident, $operation:expr) => {{
                match $operation {
                    Ok(value) => {
                        ledger.record(HardwareResource::$resource);
                        value
                    }
                    Err(source) => {
                        return Err(AcquireFailure {
                            resource: HardwareResource::$resource,
                            source,
                            ledger,
                        });
                    }
                }
            }};
        }
        macro_rules! dma {
            ($resource:ident, $direction:ty, $bytes:expr) => {
                acquire!(
                    $resource,
                    device.alloc_coherent_with_constraints::<$direction>(
                        $bytes,
                        DmaConstraints {
                            alignment: PAGE,
                            max_device_address: u32::MAX.into(),
                            max_segment_size: $bytes,
                            max_segments: 1,
                        },
                    )
                )
            };
        }

        let bar0 = acquire!(Bar0, device.open_region_sized(0, MT7921_BAR0_BYTES));
        let dma = DmaArenas {
            tx_guard: dma!(TxGuard, Bidirectional, PAGE),
            fwdl_ring: dma!(FirmwareDownloadRing, Bidirectional, PAGE),
            mcu_tx_ring: dma!(McuTxRing, Bidirectional, PAGE),
            rx_guard: dma!(RxGuard, Bidirectional, PAGE),
            mcu_rx_ring: dma!(McuRxRing, Bidirectional, PAGE),
            mcu_rx_buffers: dma!(McuRxBuffers, FromDevice, 4 * PAGE),
            command_payloads: dma!(CommandPayloads, ToDevice, MCU_COMMAND_PAYLOAD_BYTES),
            fwdl_payload: dma!(FirmwareDownloadPayload, ToDevice, PAGE),
            wa_rx_ring: dma!(WaRxRing, Bidirectional, PAGE),
            wa_rx_buffers: dma!(WaRxBuffers, FromDevice, 4 * PAGE),
            data_rx_ring: dma!(DataRxRing, Bidirectional, PAGE),
            data_rx_buffers: dma!(
                DataRxBuffers,
                FromDevice,
                MT7921_DATA_RX_RING_COUNT * MT7921_MCU_RX_BUFFER_BYTES
            ),
            management_txwi: dma!(ManagementTxwi, ToDevice, PAGE),
            management_frame: dma!(ManagementFrame, ToDevice, PAGE),
            management_tx_ring: dma!(ManagementTxRing, Bidirectional, PAGE),
        };
        Ok((
            Self {
                // Interrupt installation is an activation effect.  Acquisition
                // must leave the device fully masked with no eventfd assigned.
                interrupt: None,
                dma,
                bar0,
                device,
            },
            ledger,
        ))
    }
}

trait ContainmentAuthority {
    fn reset(&mut self) -> Result<u64, String>;
    fn post_reset_registers(&mut self) -> Result<PostResetRegisters, String>;
}

fn validate_post_reset_registers(
    registers: PostResetRegisters,
) -> Result<PostResetRegisters, String> {
    for (name, value) in [
        ("WFDMA GLO_CFG", registers.wfdma_global_config),
        ("HOST_INT_EN", registers.host_interrupt_enable),
        ("PCIe MAC INT_ENABLE", registers.pcie_mac_interrupt_enable),
    ] {
        if value == u32::MAX {
            return Err(format!("post-reset {name} returned all ones"));
        }
    }
    if registers.wfdma_global_config & 0xf != 0 {
        return Err(format!(
            "post-reset WFDMA GLO_CFG remained active: {:#010x}",
            registers.wfdma_global_config
        ));
    }
    if registers.host_interrupt_enable != 0 {
        return Err(format!(
            "post-reset HOST_INT_EN remained active: {:#010x}",
            registers.host_interrupt_enable
        ));
    }
    if registers.pcie_mac_interrupt_enable != 0 {
        return Err(format!(
            "post-reset PCIe MAC INT_ENABLE remained active: {:#010x}",
            registers.pcie_mac_interrupt_enable
        ));
    }
    Ok(registers)
}

impl<B: Backend> ContainmentAuthority for OwnedHardwareResources<B> {
    fn reset(&mut self) -> Result<u64, String> {
        self.device
            .reset()
            .map_err(|error| format!("reset device: {error:?}"))
    }

    fn post_reset_registers(&mut self) -> Result<PostResetRegisters, String> {
        // Every handle acquired before reset is stale by construction. Reopen
        // BAR0 at the new generation rather than reading through `self.bar0`.
        let bar0 = self
            .device
            .open_region_sized(0, MT7921_BAR0_BYTES)
            .map_err(|error| format!("reopen post-reset BAR0: {error:?}"))?;
        let wfdma = bar0
            .slice(0xd4000, PAGE)
            .map_err(|error| format!("slice post-reset WFDMA page: {error:?}"))?;
        let pcie_mac = bar0
            .slice(0x10000, PAGE)
            .map_err(|error| format!("slice post-reset PCIe MAC page: {error:?}"))?;
        let registers = PostResetRegisters {
            wfdma_global_config: wfdma
                .read_u32(0x208)
                .map_err(|error| format!("read post-reset WFDMA GLO_CFG: {error:?}"))?,
            host_interrupt_enable: wfdma
                .read_u32(0x204)
                .map_err(|error| format!("read post-reset HOST_INT_EN: {error:?}"))?,
            pcie_mac_interrupt_enable: pcie_mac
                .read_u32(0x188)
                .map_err(|error| format!("read post-reset PCIe MAC INT_ENABLE: {error:?}"))?,
        };
        validate_post_reset_registers(registers)
    }
}

trait PciContainmentAuthority {
    fn disable_bus_master(&mut self) -> Result<u16, String>;
    fn verify_dma_disabled(&mut self) -> Result<PostResetPciSnapshot, String>;
}

impl PciContainmentAuthority for PciControl {
    fn disable_bus_master(&mut self) -> Result<u16, String> {
        PciControl::disable_bus_master(self)
            .map_err(|error| format!("disable PCI bus master: {error}"))
    }

    fn verify_dma_disabled(&mut self) -> Result<PostResetPciSnapshot, String> {
        let snapshot = PciControl::verify_dma_disabled(self)
            .map_err(|error| format!("verify PCI DMA disabled: {error}"))?;
        Ok(PostResetPciSnapshot {
            command: snapshot.command(),
            power_state: snapshot.power_state(),
            vendor: snapshot.vendor_id(),
            device: snapshot.device_id(),
        })
    }
}

fn advance_containment(
    resources: &mut impl ContainmentAuthority,
    pci: &mut impl PciContainmentAuthority,
    lifecycle: &mut SessionLifecycle,
    ledger: &mut ContainmentLedger,
) -> Result<(), Mt7921ContainmentError> {
    if *lifecycle == SessionLifecycle::Contained {
        return Ok(());
    }
    *lifecycle = SessionLifecycle::Closing;
    let failure = |stage, detail: String, ledger: &ContainmentLedger| Mt7921ContainmentError {
        stage,
        detail,
        ledger: *ledger,
    };
    if ledger.bme_disabled_command.is_none() {
        let command = pci
            .disable_bus_master()
            .map_err(|detail| failure(ContainmentStage::DisableBusMaster, detail, ledger))?;
        ledger.bme_disabled_command = Some(command);
        ledger.bus_master_enabled = false;
    }
    if ledger.reset_generation.is_none() {
        let generation = resources
            .reset()
            .map_err(|detail| failure(ContainmentStage::Reset, detail, ledger))?;
        ledger.reset_generation = Some(generation);
    }
    if ledger.post_reset_registers.is_none() {
        let registers = resources
            .post_reset_registers()
            .map_err(|detail| failure(ContainmentStage::PostResetRegisters, detail, ledger))?;
        ledger.post_reset_registers = Some(registers);
    }
    if ledger.post_reset_pci.is_none() {
        let snapshot = pci
            .verify_dma_disabled()
            .map_err(|detail| failure(ContainmentStage::PostResetPci, detail, ledger))?;
        ledger.post_reset_pci = Some(snapshot);
    }
    ledger.bar_mapped = false;
    ledger.dma_mapped = false;
    ledger.irq_installed = false;
    *lifecycle = SessionLifecycle::Contained;
    Ok(())
}

/// Complete owned physical MT7921 resource graph.
///
/// `resources.device` owns the `LinuxVfio`, iommufd and IOAS state. Every BAR,
/// DMA, and IRQ handle shares that same backend identity without borrowing the
/// session, so this type has no self-referential lifetime.
struct Mt7921HardwareSession {
    resources: Option<OwnedHardwareResources<LinuxVfio>>,
    acquisition: AcquisitionLedger,
    containment: ContainmentLedger,
    lifecycle: SessionLifecycle,
    potentially_active: bool,
    mcu: ActiveMcuProtocol,
    receive: receive::RxRouting,
    start: Instant,
    activation_state: ActivationState,
    // Dropped after resources so the PCI control owner spans their lifetime.
    pci: Option<PciControl>,
}

impl Mt7921HardwareSession {
    /// Activate the pre-opened VFIO authority and acquire the full MT7921 graph.
    ///
    /// This is the post-lockdown entrypoint. PCI remains bus-master-disabled;
    /// the later active mechanics boundary must not enable it until ring and
    /// interrupt programming is complete.
    fn open(config: Mt7921HardwareSessionConfig) -> Result<Self, Mt7921HardwareSessionError> {
        let opened = LinuxVfio::activate_locked_pci_coherent(config.vfio)
            .map_err(Mt7921HardwareSessionError::Activate)?;
        let (backend, pci, pci_snapshot) = opened.into_parts();
        if pci_snapshot.vendor_id() != 0x14c3 || pci_snapshot.device_id() != 0x7961 {
            return Err(Mt7921HardwareSessionError::WrongPciIdentity {
                vendor: pci_snapshot.vendor_id(),
                device: pci_snapshot.device_id(),
            });
        }
        let (resources, acquisition) =
            OwnedHardwareResources::acquire(Device::from_backend(backend)).map_err(|failure| {
                Mt7921HardwareSessionError::Acquire {
                    resource: failure.resource,
                    source: failure.source,
                    ledger: failure.ledger,
                }
            })?;
        Ok(Self {
            resources: Some(resources),
            acquisition,
            containment: ContainmentLedger {
                vfio_attached: true,
                bar_mapped: true,
                dma_mapped: true,
                irq_installed: false,
                bus_master_enabled: false,
                bme_disabled_command: None,
                transport_quiesced: false,
                reset_generation: None,
                post_reset_registers: None,
                post_reset_pci: None,
            },
            lifecycle: SessionLifecycle::ResourcesMappedDmaDisabled,
            potentially_active: false,
            mcu: ActiveMcuProtocol::default(),
            receive: receive::RxRouting::default(),
            start: Instant::now(),
            activation_state: ActivationState::initial(),
            pci: Some(pci),
        })
    }

    fn open_and_activate(
        config: Mt7921HardwareSessionConfig,
    ) -> Result<Self, Mt7921FirmwareRunError> {
        let mut session = Self::open(config).map_err(Mt7921FirmwareRunError::Open)?;
        if let Err(error) = session.activate_loader() {
            let acquisition = session.acquisition.clone();
            let containment = session.contain();
            return Err(Mt7921FirmwareRunError::Activation {
                stage: format!("{:?}", error.primary.stage),
                detail: format!(
                    "{}; transport cleanup errors: {:?}",
                    error.primary.source, error.cleanup
                ),
                acquisition,
                containment: Box::new(containment),
            });
        }
        Ok(session)
    }

    fn activate_loader(&mut self) -> Result<(), ActivationFailure<String>> {
        self.potentially_active = true;
        self.lifecycle = SessionLifecycle::Activating(ActivationStage::PrepareDescriptors);
        let result = activate(
            self.resources.as_mut().expect("live session resources"),
            self.pci.as_mut().expect("live PCI authority"),
            &mut self.acquisition,
            &mut self.containment,
        );
        match result {
            Ok(state) => {
                self.activation_state = state;
                self.lifecycle = SessionLifecycle::LoaderTransportActive;
                Ok(())
            }
            Err(error) => {
                self.activation_state = error.state;
                self.lifecycle = SessionLifecycle::Activating(error.primary.stage);
                Err(error)
            }
        }
    }

    fn firmware_loader(&mut self) -> ProductionFirmwareLoader<'_, LinuxVfio, PciControl> {
        ProductionFirmwareLoader {
            resources: self.resources.as_mut().expect("live session resources"),
            pci: self.pci.as_mut().expect("live PCI authority"),
            acquisition: &mut self.acquisition,
            containment: &mut self.containment,
            activation_state: &mut self.activation_state,
            mechanics: &mut self.mcu.0,
            receive: &mut self.receive,
            start: self.start,
        }
    }

    fn load_firmware_bootstrap(
        &mut self,
        images: &VerifiedFirmwareImages,
    ) -> Result<FirmwareLoaderReport, FirmwareLoaderError<String>> {
        let verified = images.open();
        let result = mt7921_core::load_mt7921_firmware_bootstrap(
            &mut self.firmware_loader(),
            verified.patch,
            verified.ram,
        );
        self.lifecycle = if result.is_ok() {
            SessionLifecycle::BootstrapQuiesced
        } else {
            SessionLifecycle::Closing
        };
        result
    }

    /// Progress containment from the first unverified milestone.
    ///
    /// Failures retain the complete resource graph and PCI owner so callers
    /// can retry. Once contained, repeated calls are idempotent.
    fn contain(&mut self) -> Result<ContainmentLedger, Mt7921ContainmentError> {
        self.receive.abort();
        if !self.containment.transport_quiesced {
            self.lifecycle = SessionLifecycle::Closing;
            let errors = activation::quiesce(
                self.resources.as_mut().expect("live session resources"),
                self.pci.as_mut().expect("live PCI authority"),
                &mut self.acquisition,
                &mut self.containment,
                &mut self.activation_state,
            );
            if !errors.is_empty() {
                return Err(Mt7921ContainmentError {
                    stage: ContainmentStage::QuiesceTransport,
                    detail: format!("{errors:?}"),
                    ledger: self.containment,
                });
            }
            // Once reset has begun, old BAR handles may be stale even when it
            // fails. A retry must resume reset, never revisit these handles.
            self.containment.transport_quiesced = true;
        }
        advance_containment(
            self.resources.as_mut().expect("live session resources"),
            self.pci.as_mut().expect("live PCI authority"),
            &mut self.lifecycle,
            &mut self.containment,
        )?;
        Ok(self.containment)
    }
}

/// Result of the single production-owned firmware bootstrap operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mt7921FirmwareRunReport {
    pub firmware: FirmwareLoaderReport,
    pub acquisition: AcquisitionLedger,
    pub containment: ContainmentLedger,
}

#[derive(Debug)]
pub enum Mt7921FirmwareRunError {
    Radio {
        source: zx::Status,
        acquisition: AcquisitionLedger,
        containment: Box<Result<ContainmentLedger, Mt7921ContainmentError>>,
    },
    Regulatory {
        source: mt7921_core::RateTxPowerError,
        acquisition: AcquisitionLedger,
        containment: Box<Result<ContainmentLedger, Mt7921ContainmentError>>,
    },
    Open(Mt7921HardwareSessionError),
    Activation {
        stage: String,
        detail: String,
        acquisition: AcquisitionLedger,
        containment: Box<Result<ContainmentLedger, Mt7921ContainmentError>>,
    },
    Loader {
        source: Box<FirmwareLoaderError<String>>,
        acquisition: AcquisitionLedger,
        containment: Box<Result<ContainmentLedger, Mt7921ContainmentError>>,
    },
    Containment {
        source: Box<Mt7921ContainmentError>,
        acquisition: AcquisitionLedger,
    },
}

impl fmt::Display for Mt7921FirmwareRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Radio { source, .. } => write!(f, "prepare MT7921 passive radio: {source:?}"),
            Self::Regulatory { source, .. } => {
                write!(f, "derive MT7921 regulatory policy: {source:?}")
            }
            Self::Open(error) => write!(f, "open production MT7921 resources: {error}"),
            Self::Activation { stage, detail, .. } => {
                write!(f, "activate MT7921 loader at {stage}: {detail}")
            }
            Self::Loader { source, .. } => write!(f, "run MT7921 firmware loader: {source:?}"),
            Self::Containment { source, .. } => {
                write!(f, "contain MT7921 after firmware loader: {source}")
            }
        }
    }
}

impl std::error::Error for Mt7921FirmwareRunError {}

/// Activate, bootstrap the verified patch and RAM images, and return only
/// after loader cleanup plus reset containment have both completed.
pub fn run_firmware_bootstrap(
    config: Mt7921HardwareSessionConfig,
    images: VerifiedFirmwareImages,
) -> Result<Mt7921FirmwareRunReport, Mt7921FirmwareRunError> {
    let mut session = Mt7921HardwareSession::open_and_activate(config)?;
    let loader = session.load_firmware_bootstrap(&images);
    let acquisition = session.acquisition.clone();
    session.lifecycle = SessionLifecycle::Closing;
    let containment = session.contain();
    match (loader, containment) {
        (Ok(firmware), Ok(containment)) => Ok(Mt7921FirmwareRunReport {
            firmware,
            acquisition,
            containment,
        }),
        (Err(source), containment) => Err(Mt7921FirmwareRunError::Loader {
            source: Box::new(source),
            acquisition,
            containment: Box::new(containment),
        }),
        (Ok(_), Err(source)) => Err(Mt7921FirmwareRunError::Containment {
            source: Box::new(source),
            acquisition,
        }),
    }
}

/// An exclusively owned, initialized MT7921 device. Construction completes
/// firmware loading and initial EEPROM/CLC setup, before MAC initialization;
/// channel-domain and radio setup remain pending. It is not an unchecked
/// wrapper around a resource graph. Protocol and driver remain in one process.
///
/// Operational ownership cannot be forged:
/// ```compile_fail
/// use mt7921_production_client::Mt7921Driver;
/// let driver = Mt7921Driver { session: (), firmware: () };
/// ```
pub struct Mt7921Driver {
    session: Mt7921HardwareSession,
    firmware: FirmwareLoaderReport,
    regulatory: mt7921_core::RegulatoryRatePowerSnapshot,
    mac_initialization: radio::MacInitialization,
    radio_preparation: radio::RadioPreparation,
    data_rx: receive::DataRx,
    scan: Option<radio::PassiveScan>,
    channel_change: Option<radio::ChannelChange>,
    current_channel: Option<mt7921_core::CandidateChannel>,
    observations: std::collections::VecDeque<peer::ObservedBss>,
    peer_join: Option<peer::PeerJoin>,
    joined: Option<peer::ObservedBss>,
    next_scan_id: u64,
    upcalls: Option<Box<dyn wlan_softmac_host::WlanSoftmacUpcalls>>,
}

impl Mt7921Driver {
    pub fn initialize(
        config: Mt7921HardwareSessionConfig,
        images: VerifiedFirmwareImages,
        database: VerifiedRegulatoryDatabase,
    ) -> Result<Self, Mt7921FirmwareRunError> {
        let mut session = Mt7921HardwareSession::open_and_activate(config)?;
        let verified = images.open();
        let firmware = mt7921_core::initialize_mt7921_firmware(
            &mut session.firmware_loader(),
            verified.patch,
            verified.ram,
        );
        match firmware {
            Ok(firmware) => {
                let regulatory = match database.world_snapshot(1, firmware.nic_capability) {
                    Ok(regulatory) => regulatory,
                    Err(source) => {
                        session.lifecycle = SessionLifecycle::Closing;
                        let acquisition = session.acquisition.clone();
                        let containment = session.contain();
                        return Err(Mt7921FirmwareRunError::Regulatory {
                            source,
                            acquisition,
                            containment: Box::new(containment),
                        });
                    }
                };
                let radio_preparation =
                    match radio::RadioPreparation::new(verified.ram, &firmware, &regulatory) {
                        Ok(preparation) => preparation,
                        Err(source) => {
                            session.lifecycle = SessionLifecycle::Closing;
                            let acquisition = session.acquisition.clone();
                            let containment = session.contain();
                            return Err(Mt7921FirmwareRunError::Radio {
                                source,
                                acquisition,
                                containment: Box::new(containment),
                            });
                        }
                    };
                session.lifecycle = SessionLifecycle::FirmwareInitialized;
                Ok(Self {
                    session,
                    firmware,
                    regulatory,
                    mac_initialization: radio::MacInitialization::new(),
                    radio_preparation,
                    data_rx: receive::DataRx::default(),
                    scan: None,
                    channel_change: None,
                    current_channel: None,
                    observations: std::collections::VecDeque::new(),
                    peer_join: None,
                    joined: None,
                    next_scan_id: 1,
                    upcalls: None,
                })
            }
            Err(source) => {
                session.lifecycle = SessionLifecycle::Closing;
                let acquisition = session.acquisition.clone();
                let containment = session.contain();
                // Session Drop retries containment and then releases the
                // kernel references. Failure is not a successful reset proof.
                Err(Mt7921FirmwareRunError::Loader {
                    source: Box::new(source),
                    acquisition,
                    containment: Box::new(containment),
                })
            }
        }
    }

    /// World-domain receive-only channels. This is not transmit authorization.
    pub fn passive_channels(&self) -> Vec<mt7921_core::CandidateChannel> {
        mt7921_core::candidate_channels(self.firmware.nic_capability)
            .into_iter()
            .filter(|candidate| {
                candidate.band != mt7921_core::PhysicalBand::Ghz6
                    && self.regulatory.channels().iter().any(|rule| {
                        rule.band == candidate.band
                            && rule.channel == candidate.number
                            && rule.present
                            && !rule.disabled
                            && rule.max_reg_power_dbm.is_some()
                    })
            })
            .collect()
    }

    /// Diagnostic facts, not a capability for creating another driver.
    pub fn firmware(&self) -> &FirmwareLoaderReport {
        &self.firmware
    }

    /// Take one committed event retained during MCU command processing.
    pub fn take_event(&mut self) -> Option<ReceivedEvent> {
        self.session.receive.take_event()
    }

    /// Consume operational ownership. On failure, only retrying containment
    /// remains available; the failure cannot be converted back to a driver.
    pub fn contain(mut self) -> Result<ContainedMt7921, Mt7921ShutdownError> {
        match self.session.contain() {
            Ok(_) => Ok(ContainedMt7921 { driver: self }),
            Err(source) => Err(Mt7921ShutdownError {
                driver: self,
                source,
            }),
        }
    }
}

/// Constructed only after BME disable, reset, register and PCI checks succeed.
/// Retains the graph until its owner chooses to release it by dropping this
/// value. There is deliberately no conversion back to operational ownership.
///
/// ```compile_fail
/// use mt7921_production_client::{ContainedMt7921, Mt7921Driver};
/// fn forge(driver: Mt7921Driver) -> ContainedMt7921 {
///     ContainedMt7921 { driver }
/// }
/// ```
pub struct ContainedMt7921 {
    driver: Mt7921Driver,
}

impl ContainedMt7921 {
    pub fn ledger(&self) -> &ContainmentLedger {
        &self.driver.session.containment
    }
}

/// A failed consuming transition retains the entire uncontained device.
pub struct Mt7921ShutdownError {
    driver: Mt7921Driver,
    source: Mt7921ContainmentError,
}

impl Mt7921ShutdownError {
    pub fn source(&self) -> &Mt7921ContainmentError {
        &self.source
    }

    pub fn retry(self) -> Result<ContainedMt7921, Self> {
        self.driver.contain()
    }
}

impl fmt::Debug for Mt7921ShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mt7921ShutdownError")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for Mt7921ShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(f)
    }
}

impl std::error::Error for Mt7921ShutdownError {}

#[cfg(test)]
fn close_after_transaction_error<T, E>(
    lifecycle: &mut SessionLifecycle,
    result: &Result<T, TransactionError<E>>,
) {
    if result
        .as_ref()
        .is_err_and(TransactionError::requires_containment)
    {
        *lifecycle = SessionLifecycle::Closing;
    }
}

impl Drop for Mt7921HardwareSession {
    fn drop(&mut self) {
        if !self.potentially_active || self.lifecycle == SessionLifecycle::Contained {
            return;
        }
        let _ = self.contain();
        // Drop releases our mappings and final VFIO/IOMMUFD references.
        // Kernel teardown disables PCI DMA and unmaps before unpinning, even
        // when this explicit functional-reset attempt failed. Do not park and
        // prevent that cleanup. This is not evidence permitting device restart.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drv_hardware_backends::{
        DeterministicBackend, DeterministicRelease, DeterministicResourceProbe,
    };
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ContainmentCall {
        DisableBme,
        Reset,
        PostResetRegisters,
        PostResetPci,
    }

    #[test]
    fn bdf_provenance_binds_exact_cdev_and_exclusive_vfio_group() {
        use std::os::unix::fs::symlink;
        let root =
            std::env::temp_dir().join(format!("mt7921-vfio-provenance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bdf = "0000:01:00.0";
        let endpoint = root.join("pci").join(bdf);
        let group = root.join("groups/7");
        let driver = root.join("drivers/vfio-pci");
        let cdev = root.join("dev/vfio7");
        std::fs::create_dir_all(endpoint.join("vfio-dev/vfio7")).unwrap();
        std::fs::create_dir_all(group.join("devices").join(bdf)).unwrap();
        std::fs::create_dir_all(&driver).unwrap();
        std::fs::create_dir_all(cdev.parent().unwrap()).unwrap();
        std::fs::write(endpoint.join("config"), []).unwrap();
        std::fs::write(&cdev, []).unwrap();
        symlink(&group, endpoint.join("iommu_group")).unwrap();
        symlink(&driver, group.join("devices").join(bdf).join("driver")).unwrap();

        let paths =
            verified_vfio_pci_paths(&cdev, bdf, &root.join("pci"), &root.join("dev")).unwrap();
        assert_eq!(paths.vfio_cdev, std::fs::canonicalize(&cdev).unwrap());
        assert_eq!(paths.pci_config, endpoint.join("config"));

        let wrong = root.join("dev/vfio8");
        std::fs::write(&wrong, []).unwrap();
        assert!(
            verified_vfio_pci_paths(&wrong, bdf, &root.join("pci"), &root.join("dev"))
                .unwrap_err()
                .to_string()
                .contains("does not belong")
        );
        std::fs::create_dir_all(group.join("devices/0000:02:00.0")).unwrap();
        assert!(
            verified_vfio_pci_paths(&cdev, bdf, &root.join("pci"), &root.join("dev"))
                .unwrap_err()
                .to_string()
                .contains("exclusively own")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum InjectedFailure {
        DisableBme,
        IrqDisable,
        IoasUnmap,
        ResetIoctl,
        BarOpen,
        WfdmaRead,
        HostIrqRead,
        MacIrqRead,
        PostResetPci,
    }

    struct FakeContainment {
        calls: Rc<RefCell<Vec<ContainmentCall>>>,
        failure: Rc<RefCell<Option<InjectedFailure>>>,
        registers: PostResetRegisters,
    }

    impl ContainmentAuthority for FakeContainment {
        fn reset(&mut self) -> Result<u64, String> {
            self.calls.borrow_mut().push(ContainmentCall::Reset);
            let injected = self.failure.borrow_mut().take();
            match injected {
                Some(
                    failure @ (InjectedFailure::IrqDisable
                    | InjectedFailure::IoasUnmap
                    | InjectedFailure::ResetIoctl),
                ) => Err(format!("injected {failure:?}")),
                other => {
                    *self.failure.borrow_mut() = other;
                    Ok(2)
                }
            }
        }

        fn post_reset_registers(&mut self) -> Result<PostResetRegisters, String> {
            self.calls
                .borrow_mut()
                .push(ContainmentCall::PostResetRegisters);
            let injected = self.failure.borrow_mut().take();
            match injected {
                Some(
                    failure @ (InjectedFailure::BarOpen
                    | InjectedFailure::WfdmaRead
                    | InjectedFailure::HostIrqRead
                    | InjectedFailure::MacIrqRead),
                ) => Err(format!("injected {failure:?}")),
                other => {
                    *self.failure.borrow_mut() = other;
                    validate_post_reset_registers(self.registers)
                }
            }
        }
    }

    struct FakePci {
        calls: Rc<RefCell<Vec<ContainmentCall>>>,
        failure: Rc<RefCell<Option<InjectedFailure>>>,
    }

    impl PciContainmentAuthority for FakePci {
        fn disable_bus_master(&mut self) -> Result<u16, String> {
            self.calls.borrow_mut().push(ContainmentCall::DisableBme);
            if *self.failure.borrow() == Some(InjectedFailure::DisableBme) {
                self.failure.borrow_mut().take();
                Err("injected BME clear/readback".into())
            } else {
                Ok(0x2)
            }
        }

        fn verify_dma_disabled(&mut self) -> Result<PostResetPciSnapshot, String> {
            self.calls.borrow_mut().push(ContainmentCall::PostResetPci);
            if *self.failure.borrow() == Some(InjectedFailure::PostResetPci) {
                self.failure.borrow_mut().take();
                Err("injected PCI post-read".into())
            } else {
                Ok(PostResetPciSnapshot {
                    command: 0x2,
                    power_state: 0,
                    vendor: 0x14c3,
                    device: 0x7961,
                })
            }
        }
    }

    fn initial_containment() -> ContainmentLedger {
        ContainmentLedger {
            vfio_attached: true,
            bar_mapped: true,
            dma_mapped: true,
            irq_installed: true,
            bus_master_enabled: false,
            bme_disabled_command: None,
            transport_quiesced: false,
            reset_generation: None,
            post_reset_registers: None,
            post_reset_pci: None,
        }
    }

    fn fake_pair(
        failure: Option<InjectedFailure>,
    ) -> (FakeContainment, FakePci, Rc<RefCell<Vec<ContainmentCall>>>) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let failure = Rc::new(RefCell::new(failure));
        (
            FakeContainment {
                calls: calls.clone(),
                failure: failure.clone(),
                registers: PostResetRegisters {
                    wfdma_global_config: 0,
                    host_interrupt_enable: 0,
                    pcie_mac_interrupt_enable: 0,
                },
            },
            FakePci {
                calls: calls.clone(),
                failure,
            },
            calls,
        )
    }

    const ACQUISITION_ORDER: [HardwareResource; 16] = [
        HardwareResource::Bar0,
        HardwareResource::TxGuard,
        HardwareResource::FirmwareDownloadRing,
        HardwareResource::McuTxRing,
        HardwareResource::RxGuard,
        HardwareResource::McuRxRing,
        HardwareResource::McuRxBuffers,
        HardwareResource::CommandPayloads,
        HardwareResource::FirmwareDownloadPayload,
        HardwareResource::WaRxRing,
        HardwareResource::WaRxBuffers,
        HardwareResource::DataRxRing,
        HardwareResource::DataRxBuffers,
        HardwareResource::ManagementTxwi,
        HardwareResource::ManagementFrame,
        HardwareResource::ManagementTxRing,
    ];

    fn assert_no_live_resources(probe: &DeterministicResourceProbe) {
        assert_eq!(probe.live_interrupts(), 0);
        assert_eq!(probe.live_dmas(), 0);
        assert_eq!(probe.live_regions(), 0);
    }

    #[test]
    fn acquisition_owns_complete_resource_graph_without_internal_borrows() {
        let (device, probe) = DeterministicBackend::device_with_resource_probe(None);
        let (resources, ledger) = OwnedHardwareResources::acquire(device).unwrap();
        assert_eq!(ledger.acquired().first(), Some(&HardwareResource::Bar0));
        assert_eq!(
            ledger.acquired().last(),
            Some(&HardwareResource::ManagementTxRing)
        );
        assert_eq!(ledger.acquired().len(), 16);
        assert_eq!(resources.bar0.len(), MT7921_BAR0_BYTES);
        assert_eq!(
            resources.dma.command_payloads.len(),
            MCU_COMMAND_PAYLOAD_BYTES
        );
        assert_eq!(
            resources.dma.data_rx_buffers.len(),
            MT7921_DATA_RX_RING_COUNT * MT7921_MCU_RX_BUFFER_BYTES
        );
        assert!(resources.dma.tx_guard.device_address(0).is_ok());
        assert!(resources.dma.rx_guard.device_address(0).is_ok());
        assert_eq!(probe.live_interrupts(), 0);
        assert_eq!(probe.live_dmas(), 15);
        assert_eq!(probe.live_regions(), 1);

        drop(resources);
        assert_no_live_resources(&probe);
        let releases = probe.releases();
        assert_eq!(releases.last(), Some(&DeterministicRelease::Region));
        assert_eq!(
            releases[..releases.len() - 1],
            [DeterministicRelease::Dma; 15]
        );
    }

    #[test]
    fn production_command_buffer_accepts_real_channel_domain_reuses_address_and_wipes() {
        use mt7921_core::{
            DMA_DESCRIPTOR_LEN, DmaDescriptor, LoaderMechanicsTransport, NicCapability,
            NicPhyCapability, conservative_channel_domain, encode_channel_domain_command,
        };

        let command = conservative_channel_domain(
            NicCapability {
                element_count: 1,
                mac_address: None,
                phy: Some(NicPhyCapability {
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
            },
            *b"00",
            true,
            0,
        )
        .unwrap();
        let encoded = encode_channel_domain_command(&command, 1).unwrap();
        assert_eq!(encoded.len(), 388);

        let device = DeterministicBackend::device();
        let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        resources.interrupt = Some(resources.device.open_interrupt(0).unwrap());
        let issued_address = resources
            .dma
            .command_payloads
            .device_address(0)
            .unwrap()
            .bits();
        resources
            .dma
            .command_payloads
            .write(0, &[0xa5; MCU_COMMAND_PAYLOAD_BYTES])
            .unwrap();
        let descriptor = DmaDescriptor {
            buf0: issued_address as u32,
            ctrl: (encoded.len() as u32) << 16,
            buf1: 0,
            info: 0,
        };
        {
            let dma = &mut resources.dma;
            let mut receive = receive::RxRouting::default();
            let mut views = ActiveMcuViews {
                wfdma: resources.bar0.slice(0xd4000, PAGE).unwrap(),
                tx_ring: &mut dma.mcu_tx_ring,
                payloads: &mut dma.command_payloads,
                fwdl_ring: &mut dma.fwdl_ring,
                fwdl_payload: &mut dma.fwdl_payload,
                wm_ring: &mut dma.mcu_rx_ring,
                wm_buffers: &mut dma.mcu_rx_buffers,
                wm2_ring: &mut dma.wa_rx_ring,
                wm2_buffers: &mut dma.wa_rx_buffers,
                interrupt: resources.interrupt.as_ref().expect("live interrupt"),
                receive: &mut receive,
                start: Instant::now(),
            };
            assert_eq!(
                views.command_payload_capacity(255),
                MCU_COMMAND_PAYLOAD_BYTES
            );
            assert_eq!(views.command_payload_address(255).unwrap(), issued_address);
            assert_eq!(views.command_payload_address(0).unwrap(), issued_address);
            for slot in [255, 0] {
                views.write_command_payload(slot, &encoded).unwrap();
                views.write_command_descriptor(slot, descriptor).unwrap();
                views.reclaim_command(slot).unwrap();
            }
        }

        for slot in [255usize, 0] {
            let mut bytes = [0; DMA_DESCRIPTOR_LEN];
            resources
                .dma
                .mcu_tx_ring
                .read(slot * DMA_DESCRIPTOR_LEN, &mut bytes)
                .unwrap();
            assert_eq!(bytes, DmaDescriptor::reset().to_le_bytes());
        }
    }

    #[test]
    fn every_partial_acquisition_releases_all_live_resources() {
        for (index, expected) in ACQUISITION_ORDER.into_iter().enumerate() {
            let fail_at = index + 1;
            let (device, probe) = DeterministicBackend::device_with_resource_probe(Some(fail_at));
            let failure = match OwnedHardwareResources::acquire(device) {
                Ok(_) => panic!("acquisition {fail_at} unexpectedly succeeded"),
                Err(failure) => failure,
            };
            assert_eq!(failure.resource, expected);
            assert_eq!(failure.ledger.acquired(), &ACQUISITION_ORDER[..index]);
            assert_eq!(probe.attempts(), fail_at);
            assert_no_live_resources(&probe);
            let releases = probe.releases();
            assert_eq!(releases.len(), index);
            if let Some(region) = releases
                .iter()
                .position(|release| *release == DeterministicRelease::Region)
            {
                assert_eq!(region, releases.len() - 1);
            }
            assert!(!releases.contains(&DeterministicRelease::Interrupt));
        }
    }

    #[test]
    fn transport_views_are_recreated_instead_of_stored_in_the_owner() {
        let (mut resources, _) =
            OwnedHardwareResources::acquire(DeterministicBackend::device()).unwrap();
        {
            let wfdma = resources.bar0.slice(0xd4000, PAGE).unwrap();
            resources.dma.fwdl_payload.write(0, &[1, 2, 3, 4]).unwrap();
            assert_eq!(wfdma.len(), PAGE);
        }
        let conn = resources.bar0.slice(0xe0000, PAGE).unwrap();
        resources.dma.fwdl_payload.write(4, &[5, 6, 7, 8]).unwrap();
        assert_eq!(conn.len(), PAGE);
    }

    #[test]
    fn driver_ownership_uses_only_the_short_lived_conn_page() {
        let (device, operations) = DeterministicBackend::recording_device();
        let bar0 = device.open_region(0).unwrap();
        let mut transport = DriverOwnershipIo {
            conn: bar0.slice(0xe0000, PAGE).unwrap(),
            start: Instant::now(),
        };
        acquire_driver_ownership(&mut transport, |_| {}).unwrap();
        assert_eq!(
            operations.borrow().as_slice(),
            &[
                drv_hardware_backends::Operation::WriteU32 {
                    region: 0,
                    offset: 0xe0010,
                    value: PCIE_LPCR_HOST_CLR_OWN,
                },
                drv_hardware_backends::Operation::ReadU32 {
                    region: 0,
                    offset: 0xe0010,
                    value: 0,
                },
            ]
        );
    }

    #[test]
    fn containment_orders_verified_milestones_and_is_idempotent() {
        let (mut resources, mut pci, calls) = fake_pair(None);
        let mut lifecycle = SessionLifecycle::LoaderTransportActive;
        let mut ledger = initial_containment();
        advance_containment(&mut resources, &mut pci, &mut lifecycle, &mut ledger).unwrap();
        assert_eq!(
            *calls.borrow(),
            [
                ContainmentCall::DisableBme,
                ContainmentCall::Reset,
                ContainmentCall::PostResetRegisters,
                ContainmentCall::PostResetPci
            ]
        );
        assert_eq!(lifecycle, SessionLifecycle::Contained);
        assert_eq!(ledger.reset_generation(), Some(2));
        assert!(ledger.post_reset_registers().is_some());
        assert!(ledger.post_reset_pci().is_some());
        advance_containment(&mut resources, &mut pci, &mut lifecycle, &mut ledger).unwrap();
        assert_eq!(calls.borrow().len(), 4);
    }

    #[test]
    fn every_containment_failure_retains_authority_and_retries_first_missing_milestone() {
        for failure in [
            InjectedFailure::DisableBme,
            InjectedFailure::IrqDisable,
            InjectedFailure::IoasUnmap,
            InjectedFailure::ResetIoctl,
            InjectedFailure::BarOpen,
            InjectedFailure::WfdmaRead,
            InjectedFailure::HostIrqRead,
            InjectedFailure::MacIrqRead,
            InjectedFailure::PostResetPci,
        ] {
            let (mut resources, mut pci, calls) = fake_pair(Some(failure));
            let mut lifecycle = SessionLifecycle::LoaderTransportActive;
            let mut ledger = initial_containment();
            let error = advance_containment(&mut resources, &mut pci, &mut lifecycle, &mut ledger)
                .unwrap_err();
            let expected_stage = match failure {
                InjectedFailure::DisableBme => ContainmentStage::DisableBusMaster,
                InjectedFailure::IrqDisable
                | InjectedFailure::IoasUnmap
                | InjectedFailure::ResetIoctl => ContainmentStage::Reset,
                InjectedFailure::BarOpen
                | InjectedFailure::WfdmaRead
                | InjectedFailure::HostIrqRead
                | InjectedFailure::MacIrqRead => ContainmentStage::PostResetRegisters,
                InjectedFailure::PostResetPci => ContainmentStage::PostResetPci,
            };
            assert_eq!(error.stage(), expected_stage, "{failure:?}");
            assert_eq!(error.ledger(), &ledger, "{failure:?}");
            assert_eq!(lifecycle, SessionLifecycle::Closing, "{failure:?}");
            let reset_before_retry = calls
                .borrow()
                .iter()
                .filter(|call| **call == ContainmentCall::Reset)
                .count();
            advance_containment(&mut resources, &mut pci, &mut lifecycle, &mut ledger).unwrap();
            let reset_after_retry = calls
                .borrow()
                .iter()
                .filter(|call| **call == ContainmentCall::Reset)
                .count();
            if error.stage() == ContainmentStage::PostResetRegisters
                || error.stage() == ContainmentStage::PostResetPci
            {
                assert_eq!(
                    reset_after_retry, reset_before_retry,
                    "reset repeated after {failure:?}"
                );
            }
            assert_eq!(lifecycle, SessionLifecycle::Contained);
        }
    }

    #[test]
    fn post_reset_register_gates_reject_each_all_ones_and_active_value() {
        let safe = PostResetRegisters {
            wfdma_global_config: 0,
            host_interrupt_enable: 0,
            pcie_mac_interrupt_enable: 0,
        };
        assert_eq!(validate_post_reset_registers(safe), Ok(safe));
        for registers in [
            PostResetRegisters {
                wfdma_global_config: u32::MAX,
                ..safe
            },
            PostResetRegisters {
                host_interrupt_enable: u32::MAX,
                ..safe
            },
            PostResetRegisters {
                pcie_mac_interrupt_enable: u32::MAX,
                ..safe
            },
            PostResetRegisters {
                wfdma_global_config: 1,
                ..safe
            },
            PostResetRegisters {
                host_interrupt_enable: 1,
                ..safe
            },
            PostResetRegisters {
                pcie_mac_interrupt_enable: 1,
                ..safe
            },
        ] {
            assert!(validate_post_reset_registers(registers).is_err());
        }
    }

    #[test]
    fn closing_and_contained_sessions_reject_operational_access() {
        assert_eq!(
            ensure_operational(SessionLifecycle::LoaderTransportActive),
            Ok(())
        );
        assert_eq!(
            ensure_operational(SessionLifecycle::Closing),
            Err(drv_hardware::Error::StaleHandle)
        );
        assert_eq!(
            ensure_operational(SessionLifecycle::Contained),
            Err(drv_hardware::Error::StaleHandle)
        );
    }

    #[test]
    fn active_failure_retains_authority_for_resumable_containment() {
        let (mut resources, mut pci, calls) = fake_pair(Some(InjectedFailure::ResetIoctl));
        let mut lifecycle = SessionLifecycle::LoaderTransportActive;
        let mut ledger = initial_containment();
        assert!(
            advance_containment(&mut resources, &mut pci, &mut lifecycle, &mut ledger).is_err()
        );
        assert_eq!(lifecycle, SessionLifecycle::Closing);
        assert_eq!(ledger.bme_disabled_command(), Some(0x2));
        advance_containment(&mut resources, &mut pci, &mut lifecycle, &mut ledger).unwrap();
        assert_eq!(lifecycle, SessionLifecycle::Contained);
        assert_eq!(
            calls
                .borrow()
                .iter()
                .filter(|call| **call == ContainmentCall::DisableBme)
                .count(),
            1
        );
    }

    #[test]
    fn containment_required_transaction_error_closes_session_operations() {
        for error in [
            TransactionError::ContainmentRequiredTimeout,
            TransactionError::InvalidDmaIndex {
                ring: mt7921_core::McuRxIrqRing::Wm,
                index: 8,
            },
            TransactionError::Descriptor,
        ] {
            let mut lifecycle = SessionLifecycle::LoaderTransportActive;
            let result: Result<(), TransactionError<drv_hardware::Error>> = Err(error);
            close_after_transaction_error(&mut lifecycle, &result);
            assert_eq!(lifecycle, SessionLifecycle::Closing);
            assert_eq!(
                ensure_operational(lifecycle),
                Err(drv_hardware::Error::StaleHandle)
            );
        }
    }
}
