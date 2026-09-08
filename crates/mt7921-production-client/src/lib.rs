#![forbid(unsafe_code)]

//! Owned physical resources for the production MT7921 SoftMAC client.
//!
//! Setup opens [`LinuxVfioPciCapabilities`] before sandbox lockdown. After
//! lockdown, [`Mt7921HardwareSession::open`] consumes that inert authority and
//! owns the activated `LinuxVfio` backend (inside `Device<LinuxVfio>`), its
//! IOAS, PCI control descriptor, BAR mapping, DMA arenas, and interrupt. Loader
//! and passive mechanics code receives only short-lived views; the session
//! never stores references to its own fields. Policy/effects and lab telemetry
//! deliberately remain outside this crate.

mod setup_inputs;
pub use setup_inputs::{
    CredentialBytes, CredentialFile, FirmwareImageExpectation, FirmwareImageKind,
    FirmwareVerificationError, RegulatorySnapshotFile, VerifiedFirmware, VerifiedFirmwareImages,
};

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
const PASSIVE_MAC_BAR_PAGES: [usize; 13] = [
    0x08000, 0x09000, 0x0c000, 0x0f000, 0x21000, 0x23000, 0x24000, 0x34000, 0x38000, 0x39000,
    0xa1000, 0xa3000, 0xa4000,
];

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
}

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
    interrupt: Interrupt<B>,
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

/// Complete owned physical MT7921 resource graph.
///
/// `resources.device` owns the `LinuxVfio`, iommufd and IOAS state. Every BAR,
/// DMA, and IRQ handle shares that same backend identity without borrowing the
/// session, so this type has no self-referential lifetime.
pub struct Mt7921HardwareSession {
    resources: OwnedHardwareResources<LinuxVfio>,
    pci_snapshot: PciConfigSnapshot,
    acquisition: AcquisitionLedger,
    containment: ContainmentLedger,
    // Dropped after resources so the PCI control owner spans their lifetime.
    pci: PciControl,
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
            resources,
            pci_snapshot,
            acquisition,
            containment: ContainmentLedger {
                vfio_attached: true,
                bar_mapped: true,
                dma_mapped: true,
                irq_installed: true,
                bus_master_enabled: false,
            },
            pci,
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
        self.resources.device.generation()
    }

    pub fn verify_dma_disabled(
        &mut self,
    ) -> Result<PciConfigSnapshot, drv_hardware_backends::PciControlError> {
        self.pci.verify_dma_disabled()
    }

    /// Read the bounded status registers without exposing their BAR pages.
    pub fn read_only_status(&self) -> Result<ReadOnlyStatus, drv_hardware::Error> {
        let wfdma = self.resources.bar0.slice(0xd4000, PAGE)?;
        let conn = self.resources.bar0.slice(0xe0000, PAGE)?;
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
        let mut transport = DriverOwnershipIo {
            conn: self
                .resources
                .bar0
                .slice(0xe0000, PAGE)
                .map_err(OwnershipError::Transport)?,
            start: Instant::now(),
        };
        acquire_driver_ownership(&mut transport, event)
    }

    /// Create a short-lived firmware-loader view of session-owned resources.
    pub fn firmware_loader(
        &mut self,
    ) -> Result<Mt7921FirmwareLoaderResources<'_>, drv_hardware::Error> {
        Ok(Mt7921FirmwareLoaderResources {
            wfdma: self.resources.bar0.slice(0xd4000, PAGE)?,
            conn: self.resources.bar0.slice(0xe0000, PAGE)?,
            pcie_mac: self.resources.bar0.slice(0x10000, PAGE)?,
            dmashdl: self.resources.bar0.slice(0xd6000, PAGE)?,
            fwdl_ring: &mut self.resources.dma.fwdl_ring,
            mcu_tx_ring: &mut self.resources.dma.mcu_tx_ring,
            tx_guard: &mut self.resources.dma.tx_guard,
            rx_guard: &mut self.resources.dma.rx_guard,
            mcu_rx_ring: &mut self.resources.dma.mcu_rx_ring,
            mcu_rx_buffers: &mut self.resources.dma.mcu_rx_buffers,
            command_payloads: &mut self.resources.dma.command_payloads,
            fwdl_payload: &mut self.resources.dma.fwdl_payload,
            wa_rx_ring: &mut self.resources.dma.wa_rx_ring,
            wa_rx_buffers: &mut self.resources.dma.wa_rx_buffers,
            interrupt: &self.resources.interrupt,
        })
    }

    /// Create a short-lived passive data-path mechanics view.
    pub fn passive_mechanics(
        &mut self,
    ) -> Result<Mt7921PassiveMechanicsResources<'_>, drv_hardware::Error> {
        Ok(Mt7921PassiveMechanicsResources {
            swdef: self.resources.bar0.slice(0x9f000, PAGE)?,
            dmashdl: self.resources.bar0.slice(0xd6000, PAGE)?,
            mac_pages: PASSIVE_MAC_BAR_PAGES
                .into_iter()
                .map(|offset| self.resources.bar0.slice(offset, PAGE))
                .collect::<Result<Vec<_>, _>>()?,
            data_rx_ring: &mut self.resources.dma.data_rx_ring,
            data_rx_buffers: &mut self.resources.dma.data_rx_buffers,
            management_txwi: &mut self.resources.dma.management_txwi,
            management_frame: &mut self.resources.dma.management_frame,
            management_tx_ring: &mut self.resources.dma.management_tx_ring,
        })
    }
}

pub struct Mt7921FirmwareLoaderResources<'a> {
    pub wfdma: MmioRegion<LinuxVfio>,
    pub conn: MmioRegion<LinuxVfio>,
    pub pcie_mac: MmioRegion<LinuxVfio>,
    pub dmashdl: MmioRegion<LinuxVfio>,
    pub fwdl_ring: &'a mut CoherentDma<LinuxVfio, Bidirectional>,
    pub mcu_tx_ring: &'a mut CoherentDma<LinuxVfio, Bidirectional>,
    pub tx_guard: &'a mut CoherentDma<LinuxVfio, Bidirectional>,
    pub rx_guard: &'a mut CoherentDma<LinuxVfio, Bidirectional>,
    pub mcu_rx_ring: &'a mut CoherentDma<LinuxVfio, Bidirectional>,
    pub mcu_rx_buffers: &'a mut CoherentDma<LinuxVfio, FromDevice>,
    pub command_payloads: &'a mut CoherentDma<LinuxVfio, ToDevice>,
    pub fwdl_payload: &'a mut CoherentDma<LinuxVfio, ToDevice>,
    pub wa_rx_ring: &'a mut CoherentDma<LinuxVfio, Bidirectional>,
    pub wa_rx_buffers: &'a mut CoherentDma<LinuxVfio, FromDevice>,
    pub interrupt: &'a Interrupt<LinuxVfio>,
}

pub struct Mt7921PassiveMechanicsResources<'a> {
    pub swdef: MmioRegion<LinuxVfio>,
    pub dmashdl: MmioRegion<LinuxVfio>,
    pub mac_pages: Vec<MmioRegion<LinuxVfio>>,
    pub data_rx_ring: &'a mut CoherentDma<LinuxVfio, Bidirectional>,
    pub data_rx_buffers: &'a mut CoherentDma<LinuxVfio, FromDevice>,
    pub management_txwi: &'a mut CoherentDma<LinuxVfio, ToDevice>,
    pub management_frame: &'a mut CoherentDma<LinuxVfio, ToDevice>,
    pub management_tx_ring: &'a mut CoherentDma<LinuxVfio, Bidirectional>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use drv_hardware_backends::{
        DeterministicBackend, DeterministicRelease, DeterministicResourceProbe,
    };

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
}
