#![forbid(unsafe_code)]

//! Owned physical resources for the production MT7921 SoftMAC client.
//!
//! Setup opens [`LinuxVfioPciCapabilities`] before sandbox lockdown. After
//! lockdown, [`Mt7921HardwareSession::open`] consumes that inert authority and
//! owns the activated `LinuxVfio` backend (inside `Device<LinuxVfio>`), its
//! IOAS, PCI control descriptor, BAR mapping, DMA arenas, and interrupt. Active
//! data-path authority remains private while containment is brought under this
//! owner. Policy/effects and lab telemetry deliberately remain outside this
//! crate.

mod active_mcu;
mod setup_inputs;
pub use setup_inputs::{
    CredentialBytes, CredentialFile, FirmwareImageExpectation, FirmwareImageKind,
    FirmwareVerificationError, RegulatorySnapshotFile, VerifiedFirmware, VerifiedFirmwareImages,
};

use active_mcu::{ActiveMcuProtocol, ActiveMcuViews, CompletionKind, TransactionError};
use drv_hardware::{
    Backend, Bidirectional, CoherentDma, Device, DmaConstraints, FromDevice, Interrupt, MmioRegion,
    ToDevice,
};
use drv_hardware_backends::{
    LinuxVfio, LinuxVfioError, LinuxVfioPciCapabilities, PciConfigSnapshot, PciControl,
};
use mt7921_core::{
    MT7921_DATA_RX_RING_COUNT, MT7921_MCU_RX_BUFFER_BYTES, OwnershipError, OwnershipEvent,
    OwnershipTransport, PCIE_LPCR_HOST_CLR_OWN, ReadOnlyStatus, acquire_driver_ownership,
};
use std::{
    fmt,
    path::Path,
    time::{Duration, Instant},
};

const PAGE: usize = 4096;
const MT7921_BAR0_BYTES: usize = 0x10_0000;
const MCU_TX_RING_COUNT: usize = 256;
const MCU_COMMAND_SLOT_BYTES: usize = 256;
const MCU_COMMAND_PAYLOAD_BYTES: usize = MCU_TX_RING_COUNT * MCU_COMMAND_SLOT_BYTES;

/// Inert setup result that can cross the sandbox-lockdown boundary.
pub struct Mt7921HardwareSessionConfig {
    vfio: LinuxVfioPciCapabilities,
}

impl Mt7921HardwareSessionConfig {
    /// Open the PCI config, VFIO cdev, and `/dev/iommu` descriptors only.
    ///
    /// This setup phase performs no ioctl, mapping, PCI config access, or
    /// device access. Call [`Mt7921HardwareSession::open`] after lockdown.
    pub fn setup(
        vfio_cdev: impl AsRef<Path>,
        pci_config: impl AsRef<Path>,
    ) -> Result<Self, LinuxVfioError> {
        Ok(Self {
            vfio: LinuxVfioPciCapabilities::open(vfio_cdev, pci_config)?,
        })
    }

    pub fn from_capabilities(vfio: LinuxVfioPciCapabilities) -> Self {
        Self { vfio }
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
    Active,
    Closing,
    Contained,
}

fn ensure_operational(lifecycle: SessionLifecycle) -> Result<(), drv_hardware::Error> {
    if lifecycle == SessionLifecycle::Active {
        Ok(())
    } else {
        Err(drv_hardware::Error::StaleHandle)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainmentStage {
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
    interrupt: Interrupt<B>,
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

struct DriverOwnershipIo<B: Backend> {
    conn: MmioRegion<B>,
    start: Instant,
}

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
        let interrupt = acquire!(Interrupt, device.open_interrupt(0));
        Ok((
            Self {
                interrupt,
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
pub struct Mt7921HardwareSession {
    resources: Option<OwnedHardwareResources<LinuxVfio>>,
    pci_snapshot: PciConfigSnapshot,
    acquisition: AcquisitionLedger,
    containment: ContainmentLedger,
    lifecycle: SessionLifecycle,
    potentially_active: bool,
    mcu: ActiveMcuProtocol,
    // Dropped after resources so the PCI control owner spans their lifetime.
    pci: Option<PciControl>,
}

impl Mt7921HardwareSession {
    /// Activate the pre-opened VFIO authority and acquire the full MT7921 graph.
    ///
    /// This is the post-lockdown entrypoint. PCI remains bus-master-disabled;
    /// the later active mechanics boundary must not enable it until ring and
    /// interrupt programming is complete.
    pub fn open(config: Mt7921HardwareSessionConfig) -> Result<Self, Mt7921HardwareSessionError> {
        let opened = LinuxVfio::activate_pci_coherent(config.vfio)
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
            pci_snapshot,
            acquisition,
            containment: ContainmentLedger {
                vfio_attached: true,
                bar_mapped: true,
                dma_mapped: true,
                irq_installed: true,
                bus_master_enabled: false,
                bme_disabled_command: None,
                reset_generation: None,
                post_reset_registers: None,
                post_reset_pci: None,
            },
            lifecycle: SessionLifecycle::Active,
            potentially_active: false,
            mcu: ActiveMcuProtocol::default(),
            pci: Some(pci),
        })
    }

    pub fn pci_snapshot(&self) -> &PciConfigSnapshot {
        &self.pci_snapshot
    }

    pub fn acquisition(&self) -> &AcquisitionLedger {
        &self.acquisition
    }

    pub fn containment(&self) -> &ContainmentLedger {
        &self.containment
    }

    pub fn generation(&self) -> u64 {
        self.resources
            .as_ref()
            .expect("live session resources")
            .device
            .generation()
    }

    pub fn verify_dma_disabled(
        &mut self,
    ) -> Result<PciConfigSnapshot, drv_hardware_backends::PciControlError> {
        self.pci
            .as_mut()
            .expect("live PCI authority")
            .verify_dma_disabled()
    }

    /// Read the bounded status registers without exposing their BAR pages.
    pub fn read_only_status(&self) -> Result<ReadOnlyStatus, drv_hardware::Error> {
        self.ensure_active()?;
        let resources = self.resources.as_ref().expect("live session resources");
        let wfdma = resources.bar0.slice(0xd4000, PAGE)?;
        let conn = resources.bar0.slice(0xe0000, PAGE)?;
        Ok(ReadOnlyStatus::decode(
            conn.read_u32(0xf0)?,
            conn.read_u32(0x10)?,
            wfdma.read_u32(0x208)?,
        ))
    }

    /// Run the exact bounded `mt7921-core` driver-ownership mechanic.
    ///
    /// The short-lived transport owns only the CONN page and disappears before
    /// this method returns; no reference is retained in the session.
    pub fn acquire_driver_ownership(
        &mut self,
        event: impl FnMut(OwnershipEvent),
    ) -> Result<(), OwnershipError<drv_hardware::Error>> {
        self.ensure_active().map_err(OwnershipError::Transport)?;
        let mut transport = DriverOwnershipIo {
            conn: self
                .resources
                .as_ref()
                .expect("live session resources")
                .bar0
                .slice(0xe0000, PAGE)
                .map_err(OwnershipError::Transport)?,
            start: Instant::now(),
        };
        acquire_driver_ownership(&mut transport, event)
    }

    fn ensure_active(&self) -> Result<(), drv_hardware::Error> {
        ensure_operational(self.lifecycle)
    }

    /// Private and intentionally incomplete until the firmware-loader raw
    /// transaction path is cut over atomically to this executor.
    #[allow(dead_code)]
    fn transact_mcu(
        &mut self,
        template: &[u8],
        completion: CompletionKind,
        deadline_ns: u64,
    ) -> Result<Option<mt7921_core::FirmwareRx>, TransactionError<drv_hardware::Error>> {
        self.ensure_active().map_err(TransactionError::Io)?;
        // Publication below is meaningful only for an active device.  Mark
        // retention first, just as the eventual BME/WFDMA enable path must.
        self.potentially_active = true;
        let result = {
            let resources = self.resources.as_mut().expect("live session resources");
            let mut views = ActiveMcuViews {
                wfdma: resources
                    .bar0
                    .slice(0xd4000, PAGE)
                    .map_err(TransactionError::Io)?,
                tx_ring: &mut resources.dma.mcu_tx_ring,
                payloads: &mut resources.dma.command_payloads,
                wm_ring: &mut resources.dma.mcu_rx_ring,
                wm_buffers: &mut resources.dma.mcu_rx_buffers,
                wm2_ring: &mut resources.dma.wa_rx_ring,
                wm2_buffers: &mut resources.dma.wa_rx_buffers,
                interrupt: &resources.interrupt,
            };
            self.mcu
                .transact(&mut views, template, completion, deadline_ns)
        };
        close_after_transaction_error(&mut self.lifecycle, &result);
        result
    }

    /// Progress containment from the first unverified milestone.
    ///
    /// Failures retain the complete resource graph and PCI owner so callers
    /// can retry. Once contained, repeated calls are idempotent.
    pub fn contain(&mut self) -> Result<ContainmentLedger, Mt7921ContainmentError> {
        advance_containment(
            self.resources.as_mut().expect("live session resources"),
            self.pci.as_mut().expect("live PCI authority"),
            &mut self.lifecycle,
            &mut self.containment,
        )?;
        Ok(self.containment)
    }
}

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

fn park_uncontained<R, P>(resources: &mut Option<R>, pci: &mut Option<P>) {
    if let Some(resources) = resources.take() {
        std::mem::forget(resources);
    }
    if let Some(pci) = pci.take() {
        std::mem::forget(pci);
    }
}

fn finish_active_drop<R, P>(
    lifecycle: SessionLifecycle,
    resources: &mut Option<R>,
    pci: &mut Option<P>,
) {
    if lifecycle != SessionLifecycle::Contained {
        park_uncontained(resources, pci);
    }
}

impl Drop for Mt7921HardwareSession {
    fn drop(&mut self) {
        if !self.potentially_active || self.lifecycle == SessionLifecycle::Contained {
            return;
        }
        let _ = self.contain();
        // Releasing VFIO/IOAS/BAR/DMA/IRQ after unproved containment is less
        // safe than deliberately retaining the complete graph.
        finish_active_drop(self.lifecycle, &mut self.resources, &mut self.pci);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drv_hardware_backends::{
        DeterministicBackend, DeterministicRelease, DeterministicResourceProbe,
    };
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ContainmentCall {
        DisableBme,
        Reset,
        PostResetRegisters,
        PostResetPci,
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

    const ACQUISITION_ORDER: [HardwareResource; 17] = [
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
        HardwareResource::Interrupt,
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
        assert_eq!(ledger.acquired().last(), Some(&HardwareResource::Interrupt));
        assert_eq!(ledger.acquired().len(), 17);
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
        assert_eq!(probe.live_interrupts(), 1);
        assert_eq!(probe.live_dmas(), 15);
        assert_eq!(probe.live_regions(), 1);

        drop(resources);
        assert_no_live_resources(&probe);
        let releases = probe.releases();
        assert_eq!(releases.first(), Some(&DeterministicRelease::Interrupt));
        assert_eq!(releases.last(), Some(&DeterministicRelease::Region));
        assert_eq!(
            releases[1..releases.len() - 1],
            [DeterministicRelease::Dma; 15]
        );
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
        let mut lifecycle = SessionLifecycle::Active;
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
            let mut lifecycle = SessionLifecycle::Active;
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
        assert_eq!(ensure_operational(SessionLifecycle::Active), Ok(()));
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
        let mut lifecycle = SessionLifecycle::Active;
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
    fn uncontained_graph_is_parked_instead_of_normally_released() {
        struct ReleaseProbe(Rc<Cell<usize>>);
        impl Drop for ReleaseProbe {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        let releases = Rc::new(Cell::new(0));
        {
            let (mut containment, mut pci_control, _) = fake_pair(None);
            let mut lifecycle = SessionLifecycle::Active;
            let mut ledger = initial_containment();
            advance_containment(
                &mut containment,
                &mut pci_control,
                &mut lifecycle,
                &mut ledger,
            )
            .unwrap();
            let mut resources = Some(ReleaseProbe(releases.clone()));
            let mut pci = Some(ReleaseProbe(releases.clone()));
            finish_active_drop(lifecycle, &mut resources, &mut pci);
        }
        assert_eq!(releases.get(), 2, "contained graph releases normally");

        {
            let (mut containment, mut pci_control, _) =
                fake_pair(Some(InjectedFailure::ResetIoctl));
            let mut lifecycle = SessionLifecycle::Active;
            let mut ledger = initial_containment();
            assert!(
                advance_containment(
                    &mut containment,
                    &mut pci_control,
                    &mut lifecycle,
                    &mut ledger,
                )
                .is_err()
            );
            let mut resources = Some(ReleaseProbe(releases.clone()));
            let mut pci = Some(ReleaseProbe(releases.clone()));
            finish_active_drop(lifecycle, &mut resources, &mut pci);
            assert!(resources.is_none() && pci.is_none());
        }
        assert_eq!(releases.get(), 2, "uncontained graph was parked");
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
            let mut lifecycle = SessionLifecycle::Active;
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
