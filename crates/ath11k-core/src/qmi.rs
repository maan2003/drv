use alloc::vec::Vec;
use ath11k_platform_backend::{Backend, Bidirectional, CoherentDma, Device, MmioRegion};
use ath11k_qmi::{
    FirmwareAssets, MemoryProvider, MemoryRegion, QmiError,
    wire::{MemorySegment, MemorySegmentResponse},
};

/// Firmware blobs selected by the host before it drops filesystem access.
pub struct Wcn6750FirmwareAssets {
    pub board: Vec<u8>,
    pub calibration: Option<Vec<u8>>,
    pub regulatory: Option<Vec<u8>>,
    pub m3: Option<Vec<u8>>,
}

impl FirmwareAssets for Wcn6750FirmwareAssets {
    fn board_data(&mut self, _: u32) -> Result<Vec<u8>, QmiError> {
        Ok(self.board.clone())
    }
    fn calibration_data(&mut self) -> Result<Option<Vec<u8>>, QmiError> {
        Ok(self.calibration.clone())
    }
    fn regulatory_data(&mut self) -> Result<Option<Vec<u8>>, QmiError> {
        Ok(self.regulatory.clone())
    }
    fn m3_firmware(&mut self) -> Result<Option<Vec<u8>>, QmiError> {
        Ok(self.m3.clone())
    }
}

/// QMI memory effects backed by generation-tied drv-hardware allocations.
///
/// Allocations stay owned for the complete handshake/device lifetime; only
/// allocation-derived IOMMU addresses can be returned to firmware.
pub struct HardwareMemoryProvider<B: Backend> {
    device: Device<B>,
    device_bar_region: Option<u8>,
    firmware: Vec<CoherentDma<B, Bidirectional>>,
    device_bar: Option<MmioRegion<B>>,
    device_bar_request: Option<(u64, u32)>,
}

impl<B: Backend> HardwareMemoryProvider<B> {
    pub fn new(device: Device<B>, device_bar_region: u8) -> Self {
        Self {
            device,
            device_bar_region: Some(device_bar_region),
            firmware: Vec::new(),
            device_bar: None,
            device_bar_request: None,
        }
    }

    /// Record the QMI-selected hybrid BAR without mapping a VFIO region.
    /// Used by the first hardware pass that discovers the DT reg value.
    pub fn discover_device_bar(device: Device<B>) -> Self {
        Self {
            device,
            device_bar_region: None,
            firmware: Vec::new(),
            device_bar: None,
            device_bar_request: None,
        }
    }

    pub fn allocations(&self) -> usize {
        self.firmware.len()
    }

    pub fn device_bar(&self) -> Option<&MmioRegion<B>> {
        self.device_bar.as_ref()
    }
    pub fn take_device_bar(&mut self) -> Option<MmioRegion<B>> {
        self.device_bar.take()
    }

    pub fn device_bar_request(&self) -> Option<(u64, u32)> {
        self.device_bar_request
    }
}

fn qmi_transport_error(_: ath11k_platform_backend::Error) -> QmiError {
    QmiError::Transport
}

impl<B: Backend> MemoryProvider for HardwareMemoryProvider<B> {
    fn provision(
        &mut self,
        requested: &[MemorySegment],
    ) -> Result<Vec<MemorySegmentResponse>, QmiError> {
        let initial_allocations = self.firmware.len();
        let mut responses = Vec::with_capacity(requested.len());
        for segment in requested {
            let allocation = (|| {
                let mut dma = self
                    .device
                    .alloc_coherent::<Bidirectional>(segment.size as usize, 4096)
                    .map_err(qmi_transport_error)?;
                // dma_alloc_coherent returns zero-filled memory in Linux.
                if segment.size != 0 {
                    dma.write(0, &alloc::vec![0; segment.size as usize])
                        .map_err(qmi_transport_error)?;
                }
                let address = dma.device_address(0).map_err(qmi_transport_error)?.bits();
                Ok::<_, QmiError>((dma, address))
            })();
            let (dma, address) = match allocation {
                Ok(value) => value,
                Err(error) => {
                    self.firmware.truncate(initial_allocations);
                    return Err(error);
                }
            };
            responses.push(MemorySegmentResponse {
                address,
                size: segment.size,
                kind: segment.kind,
                restore: 0,
            });
            self.firmware.push(dma);
        }
        Ok(responses)
    }

    fn load_m3(&mut self, firmware: &[u8]) -> Result<MemoryRegion, QmiError> {
        let mut dma = self
            .device
            .alloc_coherent::<Bidirectional>(firmware.len(), 4096)
            .map_err(qmi_transport_error)?;
        dma.write(0, firmware).map_err(qmi_transport_error)?;
        let region = MemoryRegion {
            device_address: dma.device_address(0).map_err(qmi_transport_error)?.bits(),
            size: u32::try_from(firmware.len()).map_err(|_| QmiError::MessageTooLong)?,
        };
        self.firmware.push(dma);
        Ok(region)
    }

    fn map_device_bar(&mut self, address: u64, size: u32) -> Result<(), QmiError> {
        self.device_bar_request = Some((address, size));
        if let Some(index) = self.device_bar_region {
            let region = self
                .device
                .open_region_sized(index, size as usize)
                .map_err(qmi_transport_error)?;
            self.device_bar = Some(region);
        }
        Ok(())
    }
}

/// Owns the event-driven WCN6750 QMI handshake and its AF_QIPCRTR transport.
pub struct Wcn6750QmiSession<T, A, M>
where
    T: ath11k_qmi::Transport,
    A: FirmwareAssets,
    M: MemoryProvider,
{
    handshake: ath11k_qmi::Wcn6750Handshake<A, M>,
    transport: T,
    service_started: bool,
    device_bar_discovered: bool,
}

impl<T, A, M> Wcn6750QmiSession<T, A, M>
where
    T: ath11k_qmi::Transport,
    A: FirmwareAssets,
    M: MemoryProvider,
{
    pub fn new(transport: T, assets: A, memory: M) -> Self {
        Self {
            handshake: ath11k_qmi::Wcn6750Handshake::new(
                ath11k_qmi::HandshakeConfig::default(),
                assets,
                memory,
            ),
            transport,
            service_started: false,
            device_bar_discovered: false,
        }
    }

    pub fn init_service(&mut self) -> Result<(), QmiError> {
        if self.service_started {
            return Ok(());
        }
        self.handshake.init_service(&mut self.transport)?;
        self.service_started = true;
        Ok(())
    }

    pub fn discover_device_bar(&mut self) -> Result<(), QmiError> {
        if !self.service_started {
            self.init_service()?;
        }
        self.handshake.discover_device_bar(&mut self.transport)?;
        self.device_bar_discovered = true;
        Ok(())
    }

    pub fn memory(&self) -> &M {
        self.handshake.memory()
    }

    pub fn wait_for_firmware_ready(&mut self) -> Result<ath11k_qmi::FirmwareReady, QmiError> {
        if !self.service_started {
            self.init_service()?;
        }
        if self.device_bar_discovered {
            self.handshake
                .wait_for_firmware_ready_after_device_bar(&mut self.transport)
        } else {
            self.handshake.wait_for_firmware_ready(&mut self.transport)
        }
    }

    pub fn start_cold_boot_calibration(&mut self) -> Result<(), QmiError> {
        self.handshake
            .start_cold_boot_calibration(&mut self.transport)
    }

    pub fn firmware_start(
        &mut self,
        config: &ath11k_qmi::wire::WlanConfigRequest,
        mode: u32,
        firmware_diagnostics: bool,
    ) -> Result<(), QmiError> {
        self.handshake
            .firmware_start(&mut self.transport, config, mode, firmware_diagnostics)
    }

    pub fn firmware_stop(&mut self) -> Result<(), QmiError> {
        self.handshake.firmware_stop(&mut self.transport)
    }

    pub fn deinit_service(&mut self) {
        if self.service_started {
            self.handshake.deinit_service(&mut self.transport);
            self.service_started = false;
            self.device_bar_discovered = false;
        }
    }

    pub fn deinit(mut self) -> T {
        self.deinit_service();
        self.transport
    }
}

impl<T, A, B> Wcn6750QmiSession<T, A, HardwareMemoryProvider<B>>
where
    T: ath11k_qmi::Transport,
    A: FirmwareAssets,
    B: Backend,
{
    /// Transfer the exact QMI-validated register aperture to the HIF owner.
    pub fn take_device_bar(&mut self) -> Option<MmioRegion<B>> {
        self.handshake.memory_mut().take_device_bar()
    }
}
