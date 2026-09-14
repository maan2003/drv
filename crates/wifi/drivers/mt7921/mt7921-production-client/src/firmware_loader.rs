//! Private adapter from typed hardware resources to the shared loader engine.

use crate::{
    AcquisitionLedger, ContainmentLedger, OwnedHardwareResources,
    activation::{ActivationPci, quiesce},
    active_mcu::ActiveMcuViews,
};
use drv_hardware::Backend;
use mt7921_core::{
    ActivationState, ClcSetCommand, ClcSetResponse, DownloadCommand, FirmwareCommandCompletion,
    FirmwareImagePart, FirmwareLoaderState, FirmwareLoaderTransport, LoaderCommandCompletion,
    LoaderCompletion, LoaderMechanics, LoaderMechanicsError,
};
use std::time::{Duration, Instant};

pub(super) struct ProductionFirmwareLoader<'a, B: Backend, P> {
    pub resources: &'a mut OwnedHardwareResources<B>,
    pub pci: &'a mut P,
    pub acquisition: &'a mut AcquisitionLedger,
    pub containment: &'a mut ContainmentLedger,
    pub activation_state: &'a mut ActivationState,
    pub mechanics: LoaderMechanics,
    pub start: Instant,
}

impl<B: Backend, P: ActivationPci> ProductionFirmwareLoader<'_, B, P> {
    fn with_views<R>(
        &mut self,
        operation: impl FnOnce(
            &mut LoaderMechanics,
            &mut ActiveMcuViews<'_, B>,
        ) -> Result<R, LoaderMechanicsError<drv_hardware::Error>>,
    ) -> Result<R, String> {
        let resources = &mut *self.resources;
        let dma = &mut resources.dma;
        let mut views = ActiveMcuViews {
            wfdma: resources
                .bar0
                .slice(0xd4000, 4096)
                .map_err(|error| format!("slice WFDMA: {error:?}"))?,
            tx_ring: &mut dma.mcu_tx_ring,
            payloads: &mut dma.command_payloads,
            fwdl_ring: &mut dma.fwdl_ring,
            fwdl_payload: &mut dma.fwdl_payload,
            wm_ring: &mut dma.mcu_rx_ring,
            wm_buffers: &mut dma.mcu_rx_buffers,
            wm2_ring: &mut dma.wa_rx_ring,
            wm2_buffers: &mut dma.wa_rx_buffers,
            interrupt: resources
                .interrupt
                .as_ref()
                .ok_or("loader interrupt already released")?,
            start: self.start,
        };
        operation(&mut self.mechanics, &mut views)
            .map_err(|error| format!("shared loader mechanics: {error:?}"))
    }
}

impl<B: Backend, P: ActivationPci> FirmwareLoaderTransport for ProductionFirmwareLoader<'_, B, P> {
    type Error = String;

    fn next_sequence(&mut self) -> u8 {
        self.mechanics.reserve_sequence(&mut ())
    }

    fn acpi_configuration(&self) -> u8 {
        1
    }

    fn command(
        &mut self,
        command: DownloadCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<FirmwareCommandCompletion, Self::Error> {
        let response = !matches!(
            command,
            DownloadCommand::NicPowerControl | DownloadCommand::FirmwareLogToHost
        );
        let deadline = self
            .now_ms()
            .saturating_add(if response { 3_000 } else { 1_000 });
        let completion = self.with_views(|mechanics, views| {
            mechanics.execute_reserved_template(
                views,
                &mut (),
                sequence,
                encoded,
                if response {
                    LoaderCommandCompletion::Response
                } else {
                    LoaderCommandCompletion::NoResponse
                },
                deadline,
            )
        })?;
        match completion {
            LoaderCompletion::NoResponse => Ok(FirmwareCommandCompletion::NoResponse),
            LoaderCompletion::Response(response) => mt7921_core::classify_mcu_completion(
                command,
                mt7921_core::McuResponse {
                    event_id: response.response.event_id,
                    option: response.response.option,
                    bytes: &response.bytes,
                },
            )
            .map_err(|error| format!("classify MCU completion: {error}")),
        }
    }

    fn set_clc(
        &mut self,
        command: &ClcSetCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<Option<ClcSetResponse>, Self::Error> {
        let response_expected = command.expects_response();
        let deadline = self
            .now_ms()
            .saturating_add(if response_expected { 3_000 } else { 1_000 });
        match self.with_views(|mechanics, views| {
            mechanics.execute_reserved_template(
                views,
                &mut (),
                sequence,
                encoded,
                if response_expected {
                    LoaderCommandCompletion::Response
                } else {
                    LoaderCommandCompletion::NoResponse
                },
                deadline,
            )
        })? {
            LoaderCompletion::NoResponse => Ok(None),
            LoaderCompletion::Response(response) => {
                mt7921_core::classify_clc_response(mt7921_core::McuResponse {
                    event_id: response.response.event_id,
                    option: response.response.option,
                    bytes: &response.bytes,
                })
                .map(Some)
                .map_err(|error| format!("classify CLC response: {error}"))
            }
        }
    }

    fn set_channel_domain(
        &mut self,
        _: &mt7921_core::ChannelDomainCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<(), Self::Error> {
        let deadline = self.now_ms().saturating_add(1_000);
        self.with_views(|mechanics, views| {
            mechanics.execute_reserved_template(
                views,
                &mut (),
                sequence,
                encoded,
                LoaderCommandCompletion::NoResponse,
                deadline,
            )
        })
        .map(|_| ())
    }

    fn publish_scatter(
        &mut self,
        part: FirmwareImagePart,
        sequence: u8,
        chunk: &[u8],
    ) -> Result<(), Self::Error> {
        self.with_views(|mechanics, views| {
            mechanics.publish_reserved_scatter(views, &mut (), part, sequence, chunk)
        })
    }

    fn wait_scatter_completion(
        &mut self,
        part: FirmwareImagePart,
        sequence: u8,
        deadline_ms: u64,
    ) -> Result<(), Self::Error> {
        self.with_views(|mechanics, views| {
            mechanics.complete_scatter(views, &mut (), part, sequence, deadline_ms)
        })
    }

    fn firmware_download_state(&mut self) -> Result<u8, Self::Error> {
        let conn = self
            .resources
            .bar0
            .slice(0xe0000, 4096)
            .map_err(|error| format!("slice CONN: {error:?}"))?;
        Ok((conn
            .read_u32(0xf0)
            .map_err(|error| format!("read firmware state: {error:?}"))?
            & 7) as u8)
    }

    fn firmware_n9_ready(&mut self) -> Result<bool, Self::Error> {
        let conn = self
            .resources
            .bar0
            .slice(0xe0000, 4096)
            .map_err(|error| format!("slice CONN: {error:?}"))?;
        Ok(conn
            .read_u32(0xf0)
            .map_err(|error| format!("read N9 state: {error:?}"))?
            & 3
            == 3)
    }

    fn now_ms(&self) -> u64 {
        self.start
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn sleep_ms(&mut self, duration_ms: u64) {
        std::thread::sleep(Duration::from_millis(duration_ms));
    }

    fn fail_closed_cleanup(&mut self, _: FirmwareLoaderState) -> Result<(), Self::Error> {
        let errors = quiesce(
            self.resources,
            self.pci,
            self.acquisition,
            self.containment,
            self.activation_state,
        );
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("{errors:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HardwareResource, activation::ActivationPci};
    use drv_hardware::Device;
    use drv_hardware_backends::DeterministicBackend;
    use mt7921_core::{BusMasterState, InterruptInstallState};

    struct FakePci(u16);
    impl ActivationPci for FakePci {
        fn disable_intx(&mut self) -> Result<u16, String> {
            self.0 |= 1 << 10;
            Ok(self.0)
        }
        fn enable_bus_master(&mut self) -> Result<u16, String> {
            self.0 |= 4;
            Ok(self.0)
        }
        fn verify_bus_master_enabled(&mut self) -> Result<u16, String> {
            (self.0 & 4 != 0).then_some(self.0).ok_or("BME off".into())
        }
        fn disable_bus_master(&mut self) -> Result<u16, String> {
            self.0 &= !4;
            Ok(self.0)
        }
        fn verify_bus_master_disabled(&mut self) -> Result<u16, String> {
            (self.0 & 4 == 0).then_some(self.0).ok_or("BME on".into())
        }
    }

    #[test]
    fn real_loader_scatter_deadline_is_non_irq_and_cleanup_is_shared_quiesce() {
        let device = Device::from_backend(DeterministicBackend::default());
        let (mut resources, mut acquisition) = OwnedHardwareResources::acquire(device).unwrap();
        resources.interrupt = Some(resources.device.open_interrupt(0).unwrap());
        acquisition.record(HardwareResource::Interrupt);
        let mut containment = ContainmentLedger {
            vfio_attached: true,
            bar_mapped: true,
            dma_mapped: true,
            irq_installed: true,
            bus_master_enabled: true,
            bme_disabled_command: None,
            reset_generation: None,
            post_reset_registers: None,
            post_reset_pci: None,
        };
        let mut activation_state = ActivationState {
            mutation_attempted: true,
            interrupt: InterruptInstallState::InstalledAndQuiet,
            bus_master: BusMasterState::Enabled,
        };
        let mut pci = FakePci(0x406);
        let mut loader = ProductionFirmwareLoader {
            resources: &mut resources,
            pci: &mut pci,
            acquisition: &mut acquisition,
            containment: &mut containment,
            activation_state: &mut activation_state,
            mechanics: LoaderMechanics::default(),
            start: Instant::now(),
        };
        let sequence = loader.next_sequence();
        loader
            .publish_scatter(FirmwareImagePart::Patch, sequence, &[0x5a; 64])
            .unwrap();
        assert!(
            loader
                .wait_scatter_completion(FirmwareImagePart::Patch, sequence, 0)
                .unwrap_err()
                .contains("Timeout")
        );
        loader
            .fail_closed_cleanup(FirmwareLoaderState::RamDownloading)
            .unwrap();
        assert!(loader.resources.interrupt.is_none());
        assert!(!loader.containment.irq_installed);
        assert!(!loader.containment.bus_master_enabled);
        assert_eq!(loader.activation_state.bus_master, BusMasterState::Disabled);
    }
}
