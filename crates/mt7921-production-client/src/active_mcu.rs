//! Private physical adapter for the resource-free shared MCU loader engine.
#![allow(dead_code, reason = "private executor awaits activation cutover")]

use drv_hardware::{
    Backend, Bidirectional, CoherentDma, FromDevice, Interrupt, MmioRegion, ToDevice,
};
use mt7921_core::{
    DMA_DESCRIPTOR_LEN, DmaDescriptor, LoaderCommandCompletion, LoaderCompletion, LoaderMechanics,
    LoaderMechanicsError, LoaderMechanicsTransport, MT7921_LOADER_COMMAND_MAX_BYTES,
    MT7921_MCU_RX_BUFFER_BYTES, McuRxIrqRing,
};

const MCU_TX_DIDX: usize = 0x41c;
const MCU_TX_CIDX: usize = 0x418;
const FWDL_DIDX: usize = 0x40c;
const FWDL_CIDX: usize = 0x408;
const HOST_INT_STATUS: usize = 0x200;
const HOST_INT_ENABLE: usize = 0x204;
const RX_RING_BASE: usize = 0x500;
const RING_STRIDE: usize = 0x10;
const RING_CIDX: usize = 0x08;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompletionKind {
    FirmwareResponse,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum TransactionError<E> {
    InvalidTemplate,
    Io(E),
    ContainmentRequiredIo(E),
    ContainmentRequiredTimeout,
    Descriptor,
    InvalidDmaIndex { ring: McuRxIrqRing, index: u16 },
    Protocol,
}

impl<E> TransactionError<E> {
    pub(super) fn requires_containment(&self) -> bool {
        matches!(
            self,
            Self::ContainmentRequiredIo(_)
                | Self::ContainmentRequiredTimeout
                | Self::Descriptor
                | Self::InvalidDmaIndex { .. }
                | Self::Protocol
        )
    }
}

fn map_error<E>(error: LoaderMechanicsError<E>) -> TransactionError<E> {
    match error {
        LoaderMechanicsError::InvalidCommandLength
        | LoaderMechanicsError::InvalidReservedSequence { .. }
        | LoaderMechanicsError::Encode(_) => TransactionError::InvalidTemplate,
        LoaderMechanicsError::Descriptor(_) => TransactionError::Descriptor,
        LoaderMechanicsError::Timeout => TransactionError::ContainmentRequiredTimeout,
        LoaderMechanicsError::Transport(error) => TransactionError::Io(error),
        LoaderMechanicsError::ContainmentRequired(error) => {
            TransactionError::ContainmentRequiredIo(error)
        }
        _ => TransactionError::Protocol,
    }
}

#[derive(Default)]
pub(super) struct ActiveMcuProtocol(LoaderMechanics);

impl ActiveMcuProtocol {
    pub(super) fn transact<I: LoaderMechanicsTransport>(
        &mut self,
        io: &mut I,
        template: &[u8],
        completion: CompletionKind,
        deadline_ns: u64,
    ) -> Result<Option<mt7921_core::FirmwareRx>, TransactionError<I::Error>> {
        let completion = match completion {
            CompletionKind::FirmwareResponse => LoaderCommandCompletion::Response,
        };
        match self
            .0
            .execute_template(io, &mut (), template, completion, deadline_ns)
            .map_err(map_error)?
        {
            LoaderCompletion::Response(response) => Ok(Some(response)),
            LoaderCompletion::NoResponse => Ok(None),
        }
    }
}

pub(super) struct ActiveMcuViews<'a, B: Backend> {
    pub wfdma: MmioRegion<B>,
    pub tx_ring: &'a mut CoherentDma<B, Bidirectional>,
    pub payloads: &'a mut CoherentDma<B, ToDevice>,
    pub fwdl_ring: &'a mut CoherentDma<B, Bidirectional>,
    pub fwdl_payload: &'a mut CoherentDma<B, ToDevice>,
    pub wm_ring: &'a mut CoherentDma<B, Bidirectional>,
    pub wm_buffers: &'a mut CoherentDma<B, FromDevice>,
    pub wm2_ring: &'a mut CoherentDma<B, Bidirectional>,
    pub wm2_buffers: &'a mut CoherentDma<B, FromDevice>,
    pub interrupt: &'a Interrupt<B>,
}

impl<B: Backend> ActiveMcuViews<'_, B> {
    fn ring_number(ring: McuRxIrqRing) -> usize {
        if ring == McuRxIrqRing::Wm { 0 } else { 4 }
    }
    fn ring_dma(&mut self, ring: McuRxIrqRing) -> &mut CoherentDma<B, Bidirectional> {
        if ring == McuRxIrqRing::Wm {
            self.wm_ring
        } else {
            self.wm2_ring
        }
    }
    fn buffer_dma(&mut self, ring: McuRxIrqRing) -> &mut CoherentDma<B, FromDevice> {
        if ring == McuRxIrqRing::Wm {
            self.wm_buffers
        } else {
            self.wm2_buffers
        }
    }
    fn descriptor(
        dma: &mut CoherentDma<B, Bidirectional>,
        slot: u16,
    ) -> Result<DmaDescriptor, drv_hardware::Error> {
        let mut bytes = [0; DMA_DESCRIPTOR_LEN];
        dma.read(usize::from(slot) * DMA_DESCRIPTOR_LEN, &mut bytes)?;
        Ok(descriptor_from_bytes(bytes))
    }
}

impl<B: Backend> LoaderMechanicsTransport for ActiveMcuViews<'_, B> {
    type Error = drv_hardware::Error;
    fn command_payload_capacity(&self, _: u16) -> usize {
        self.payloads.len()
    }
    fn command_payload_address(&self, _: u16) -> Result<u64, Self::Error> {
        Ok(self.payloads.device_address(0)?.bits())
    }
    fn write_command_payload(&mut self, _: u16, bytes: &[u8]) -> Result<(), Self::Error> {
        self.payloads.write(0, bytes)
    }
    fn write_command_descriptor(
        &mut self,
        slot: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error> {
        self.tx_ring.write(
            usize::from(slot) * DMA_DESCRIPTOR_LEN,
            &descriptor.to_le_bytes(),
        )
    }
    fn enable_response_interrupts(&mut self, mask: u32) -> Result<(), Self::Error> {
        self.wfdma.write_u32(HOST_INT_ENABLE, mask)
    }
    fn publish_command_producer(&mut self, producer: u16) -> Result<(), Self::Error> {
        self.wfdma.write_u32(MCU_TX_CIDX, u32::from(producer))
    }
    fn command_dma_index(&mut self) -> Result<u32, Self::Error> {
        self.wfdma.read_u32(MCU_TX_DIDX)
    }
    fn read_command_descriptor(&mut self, slot: u16) -> Result<DmaDescriptor, Self::Error> {
        Self::descriptor(self.tx_ring, slot)
    }
    fn reclaim_command(&mut self, slot: u16) -> Result<(), Self::Error> {
        self.tx_ring.write(
            usize::from(slot) * DMA_DESCRIPTOR_LEN,
            &DmaDescriptor::reset().to_le_bytes(),
        )?;
        self.payloads
            .write(0, &[0; MT7921_LOADER_COMMAND_MAX_BYTES])
    }
    fn wait_for_interrupt(&mut self, deadline: u64) -> Result<bool, Self::Error> {
        Ok(self.interrupt.wait_until(deadline)?.is_some())
    }
    fn wait_for_progress(&mut self, deadline: u64) -> Result<bool, Self::Error> {
        Ok(self.interrupt.wait_until(deadline)?.is_some())
    }
    fn mask_response_interrupts(&mut self) -> Result<(), Self::Error> {
        self.wfdma.write_u32(HOST_INT_ENABLE, 0)
    }
    fn response_interrupt_status(&mut self) -> Result<u32, Self::Error> {
        self.wfdma.read_u32(HOST_INT_STATUS)
    }
    fn acknowledge_response_interrupts(&mut self, status: u32) -> Result<(), Self::Error> {
        self.wfdma.write_u32(HOST_INT_STATUS, status)
    }
    fn read_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: u16,
    ) -> Result<DmaDescriptor, Self::Error> {
        Self::descriptor(self.ring_dma(ring), slot)
    }
    fn read_rx_buffer(
        &mut self,
        ring: McuRxIrqRing,
        slot: u16,
        bytes: &mut [u8],
    ) -> Result<(), Self::Error> {
        self.buffer_dma(ring)
            .read(usize::from(slot) * MT7921_MCU_RX_BUFFER_BYTES, bytes)
    }
    fn rx_buffer_address(&self, ring: McuRxIrqRing, slot: u16) -> Result<u64, Self::Error> {
        let offset = usize::from(slot) * MT7921_MCU_RX_BUFFER_BYTES;
        Ok(if ring == McuRxIrqRing::Wm {
            self.wm_buffers.device_address(offset)?.bits()
        } else {
            self.wm2_buffers.device_address(offset)?.bits()
        })
    }
    fn repost_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error> {
        self.ring_dma(ring).write(
            usize::from(slot) * DMA_DESCRIPTOR_LEN,
            &descriptor.to_le_bytes(),
        )
    }
    fn publish_rx_producer(
        &mut self,
        ring: McuRxIrqRing,
        producer: u16,
    ) -> Result<(), Self::Error> {
        self.wfdma.write_u32(
            RX_RING_BASE + Self::ring_number(ring) * RING_STRIDE + RING_CIDX,
            u32::from(producer),
        )
    }
    fn prepare_rx_result(
        &mut self,
        _: McuRxIrqRing,
        _: u16,
        _: u16,
        _: &[u8],
        _: Result<&mt7921_core::McuRxRoute, &mt7921_core::McuRxRouteError>,
        _: Option<mt7921_core::FirmwareRxDisposition>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
    fn complete_rx_result(&mut self, _: McuRxIrqRing, _: u16) -> Result<(), Self::Error> {
        Ok(())
    }
    fn abort_rx(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn scatter_payload_address(&self) -> Result<u64, Self::Error> {
        Ok(self.fwdl_payload.device_address(0)?.bits())
    }
    fn write_scatter_payload(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.fwdl_payload.write(0, bytes)
    }
    fn write_scatter_descriptor(
        &mut self,
        slot: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error> {
        self.fwdl_ring.write(
            usize::from(slot) * DMA_DESCRIPTOR_LEN,
            &descriptor.to_le_bytes(),
        )
    }
    fn publish_scatter_producer(&mut self, producer: u16) -> Result<(), Self::Error> {
        self.wfdma.write_u32(FWDL_CIDX, u32::from(producer))
    }
    fn scatter_dma_index(&mut self) -> Result<u32, Self::Error> {
        self.wfdma.read_u32(FWDL_DIDX)
    }
    fn read_scatter_descriptor(&mut self, slot: u16) -> Result<DmaDescriptor, Self::Error> {
        Self::descriptor(self.fwdl_ring, slot)
    }
    fn reclaim_scatter(&mut self, slot: u16) -> Result<(), Self::Error> {
        self.fwdl_ring.write(
            usize::from(slot) * DMA_DESCRIPTOR_LEN,
            &DmaDescriptor::reset().to_le_bytes(),
        )?;
        self.fwdl_payload.write(0, &[0; 4096])
    }
}

fn descriptor_from_bytes(bytes: [u8; DMA_DESCRIPTOR_LEN]) -> DmaDescriptor {
    DmaDescriptor {
        buf0: u32::from_le_bytes(bytes[0..4].try_into().expect("descriptor word")),
        ctrl: u32::from_le_bytes(bytes[4..8].try_into().expect("descriptor word")),
        buf1: u32::from_le_bytes(bytes[8..12].try_into().expect("descriptor word")),
        info: u32::from_le_bytes(bytes[12..16].try_into().expect("descriptor word")),
    }
}
