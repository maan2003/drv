use alloc::vec::Vec;
use ath11k_platform_backend::{Backend, Bidirectional, CoherentDma, Device, MmioRegion};
use ath11k_qmi::{
    FirmwareAssets, MemoryProvider, MemoryRegion, QmiError,
    wire::{MemorySegment, MemorySegmentResponse},
};
use sha2::{Digest, Sha256};

const REDWOOD_BOARD_BYTES: usize = 59_924;
const REDWOOD_REGDB_BYTES: usize = 24_278;
const REDWOOD_BOARD_SHA256: [u8; 32] = [
    0xb4, 0x5a, 0x60, 0xf0, 0x7e, 0x4c, 0x83, 0x8b, 0x6f, 0x52, 0x2a, 0xb5, 0x87, 0x22, 0x9c, 0x4a,
    0x9f, 0xee, 0xfd, 0x53, 0xe7, 0xe0, 0x40, 0xbd, 0xe1, 0x82, 0x0a, 0x9e, 0x94, 0x06, 0xbf, 0xd1,
];
const REDWOOD_REGDB_SHA256: [u8; 32] = [
    0x2f, 0xe6, 0xb7, 0x9e, 0x6d, 0x36, 0xe1, 0x90, 0xf3, 0x9e, 0x89, 0x16, 0xbe, 0xe5, 0xae, 0x4c,
    0xf9, 0x7a, 0xb5, 0xe7, 0x15, 0x19, 0xaf, 0x5a, 0xf8, 0x92, 0x0b, 0xb7, 0x57, 0x74, 0x0d, 0xd9,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wcn6750FirmwareAssetError {
    BoardLength,
    BoardSha256,
    RegulatoryLength,
    RegulatorySha256,
}

/// Firmware blobs selected by the host before it drops filesystem access.
pub struct Wcn6750FirmwareAssets {
    pub board: Vec<u8>,
    pub calibration: Option<Vec<u8>>,
    pub regulatory: Option<Vec<u8>>,
    pub m3: Option<Vec<u8>>,
}

impl Wcn6750FirmwareAssets {
    /// Validate and bind the exact board and regulatory assets selected for Redwood.
    pub fn new_redwood(
        board: Vec<u8>,
        regulatory: Vec<u8>,
    ) -> Result<Self, Wcn6750FirmwareAssetError> {
        validate_asset(
            &board,
            REDWOOD_BOARD_BYTES,
            REDWOOD_BOARD_SHA256,
            Wcn6750FirmwareAssetError::BoardLength,
            Wcn6750FirmwareAssetError::BoardSha256,
        )?;
        validate_asset(
            &regulatory,
            REDWOOD_REGDB_BYTES,
            REDWOOD_REGDB_SHA256,
            Wcn6750FirmwareAssetError::RegulatoryLength,
            Wcn6750FirmwareAssetError::RegulatorySha256,
        )?;
        Ok(Self {
            board,
            calibration: None,
            regulatory: Some(regulatory),
            m3: None,
        })
    }
}

fn validate_asset(
    bytes: &[u8],
    expected_len: usize,
    expected_sha256: [u8; 32],
    length_error: Wcn6750FirmwareAssetError,
    hash_error: Wcn6750FirmwareAssetError,
) -> Result<(), Wcn6750FirmwareAssetError> {
    if bytes.len() != expected_len {
        return Err(length_error);
    }
    if Sha256::digest(bytes).as_slice() != expected_sha256 {
        return Err(hash_error);
    }
    Ok(())
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

#[cfg(test)]
mod asset_tests {
    use super::*;

    const SHA256_ABC: [u8; 32] = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
        0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
        0x15, 0xad,
    ];

    #[test]
    fn exact_asset_is_accepted() {
        assert_eq!(
            validate_asset(
                b"abc",
                3,
                SHA256_ABC,
                Wcn6750FirmwareAssetError::BoardLength,
                Wcn6750FirmwareAssetError::BoardSha256,
            ),
            Ok(())
        );
    }

    #[test]
    fn wrong_asset_length_is_typed() {
        assert!(matches!(
            Wcn6750FirmwareAssets::new_redwood(Vec::new(), Vec::new()),
            Err(Wcn6750FirmwareAssetError::BoardLength)
        ));
    }

    #[test]
    fn wrong_asset_hash_is_typed() {
        assert!(matches!(
            Wcn6750FirmwareAssets::new_redwood(alloc::vec![0; REDWOOD_BOARD_BYTES], Vec::new()),
            Err(Wcn6750FirmwareAssetError::BoardSha256)
        ));
    }
}
