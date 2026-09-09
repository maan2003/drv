//! Narrow, private MCU publication/receive executor.
//!
//! This is deliberately not a firmware-loader transport or a production-open
//! boundary.  It owns only the single wire sequence and the three physical
//! ring cursors needed to make one already-prepared MCU transaction.
#![allow(dead_code, reason = "private executor awaits atomic loader cutover")]

use drv_hardware::{
    Backend, Bidirectional, CoherentDma, FromDevice, Interrupt, MmioRegion, ToDevice,
};
use mt7921_core::{
    DMA_DESCRIPTOR_LEN, DmaDescriptor, DmaSegment, FirmwareRx, FirmwareRxDisposition,
    MT7921_MCU_RX_BUFFER_BYTES, MT7921_MCU_RX_RING_COUNT, McuRxIrqActionKind, McuRxIrqMachine,
    McuRxIrqMachineError, McuRxIrqReport, McuRxIrqRing, McuRxIrqTerminal, McuRxIrqTopology,
    McuRxMaskState, McuRxRoute, McuRxRouteError, classify_firmware_rx, mt7921_dma_rx,
    mt7921_dma_tx, route_mcu_rx_descriptor,
};

const COMMAND_SLOT_BYTES: usize = 256;
const MCU_TX_CIDX: usize = 0x418;
const HOST_INT_STATUS: usize = 0x200;
const HOST_INT_ENABLE: usize = 0x204;
const RX_RING_BASE: usize = 0x500;
const RING_STRIDE: usize = 0x10;
const RING_CIDX: usize = 0x08;
const RING_DIDX: usize = 0x0c;

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
    ContainmentRequiredIrq(McuRxIrqMachineError),
    Descriptor,
    InvalidDmaIndex { ring: McuRxIrqRing, index: u32 },
    Route(McuRxRouteError),
    DuplicateResponse,
}

impl<E> TransactionError<E> {
    pub(super) fn requires_containment(&self) -> bool {
        matches!(
            self,
            Self::ContainmentRequiredIo(_)
                | Self::ContainmentRequiredTimeout
                | Self::ContainmentRequiredIrq(_)
                | Self::Descriptor
                | Self::InvalidDmaIndex { .. }
        )
    }
}

pub(super) trait McuIo {
    type Error;

    fn payload_address(&self, slot: usize) -> Result<u64, Self::Error>;
    fn write_payload(&mut self, slot: usize, bytes: &[u8]) -> Result<(), Self::Error>;
    fn write_tx_descriptor(
        &mut self,
        slot: usize,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error>;
    fn publish_tx(&mut self, producer: u32) -> Result<(), Self::Error>;
    fn wait_until(&mut self, deadline_ns: u64) -> Result<bool, Self::Error>;
    fn mask_host(&mut self) -> Result<(), Self::Error>;
    fn read_host_status(&mut self) -> Result<u32, Self::Error>;
    fn acknowledge(&mut self, status: u32) -> Result<(), Self::Error>;
    fn rx_dma_index(&mut self, ring: McuRxIrqRing) -> Result<u32, Self::Error>;
    fn read_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: usize,
    ) -> Result<DmaDescriptor, Self::Error>;
    fn read_rx_buffer(
        &mut self,
        ring: McuRxIrqRing,
        slot: usize,
        bytes: &mut [u8],
    ) -> Result<(), Self::Error>;
    fn rx_buffer_address(&self, ring: McuRxIrqRing, slot: usize) -> Result<u64, Self::Error>;
    fn rearm_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: usize,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error>;
    fn publish_rx(&mut self, ring: McuRxIrqRing, producer: u32) -> Result<(), Self::Error>;
    fn unmask(&mut self, mask: u32) -> Result<(), Self::Error>;
}

#[derive(Default)]
pub(super) struct ActiveMcuProtocol {
    sequence: u8,
    tx_producer: u16,
    rx_cursor: [u16; 2],
}

impl ActiveMcuProtocol {
    pub(super) fn transact<I: McuIo>(
        &mut self,
        io: &mut I,
        template: &[u8],
        completion: CompletionKind,
        deadline_ns: u64,
    ) -> Result<Option<FirmwareRx>, TransactionError<I::Error>> {
        if template.len() < 48 || template.len() > COMMAND_SLOT_BYTES {
            return Err(TransactionError::InvalidTemplate);
        }
        let slot = usize::from(self.tx_producer);
        let sequence = if self.sequence == 15 {
            1
        } else {
            self.sequence + 1
        };
        let mut command = template.to_vec();
        // The executor is the only code allowed to mutate the wire sequence.
        command[39] = sequence;
        let address = io.payload_address(slot).map_err(TransactionError::Io)?;
        let descriptor = mt7921_dma_tx(
            DmaSegment {
                iova: address,
                len: command
                    .len()
                    .try_into()
                    .map_err(|_| TransactionError::InvalidTemplate)?,
            },
            None,
            0,
        )
        .map_err(|_| TransactionError::Descriptor)?;

        io.write_payload(slot, &command)
            .map_err(TransactionError::Io)?;
        io.write_tx_descriptor(slot, descriptor)
            .map_err(TransactionError::Io)?;
        let next = (slot + 1) % 256;
        // From this point publication is uncertain: consume both authorities
        // before ringing the physical producer doorbell.
        self.sequence = sequence;
        self.tx_producer = next as u16;
        io.publish_tx(next as u32)
            .map_err(TransactionError::ContainmentRequiredIo)?;

        loop {
            if !io
                .wait_until(deadline_ns)
                .map_err(TransactionError::ContainmentRequiredIo)?
            {
                return Err(TransactionError::ContainmentRequiredTimeout);
            }
            let response = self.service_irq(io, sequence)?;
            if let Some(response) = response {
                return match completion {
                    CompletionKind::FirmwareResponse => Ok(Some(response)),
                };
            }
        }
    }

    fn service_irq<I: McuIo>(
        &mut self,
        io: &mut I,
        expected_sequence: u8,
    ) -> Result<Option<FirmwareRx>, TransactionError<I::Error>> {
        let mut machine = McuRxIrqMachine::begin(McuRxIrqTopology::firmware());
        let mut matched = None;
        let operation = (|| loop {
            let action = machine.action();
            let report = match action.kind() {
                McuRxIrqActionKind::MaskHost => {
                    io.mask_host()
                        .map_err(TransactionError::ContainmentRequiredIo)?;
                    McuRxIrqReport::Masked
                }
                McuRxIrqActionKind::ReadHostStatus => McuRxIrqReport::HostStatus(
                    io.read_host_status()
                        .map_err(TransactionError::ContainmentRequiredIo)?,
                ),
                McuRxIrqActionKind::Acknowledge(status) => {
                    io.acknowledge(status)
                        .map_err(TransactionError::ContainmentRequiredIo)?;
                    McuRxIrqReport::Acknowledged
                }
                McuRxIrqActionKind::Drain(ring) => {
                    let candidate = self.drain_ring(io, ring, expected_sequence)?;
                    let candidate_matched = candidate.is_some();
                    if matched.is_none() && candidate.is_some() {
                        matched = candidate;
                    }
                    McuRxIrqReport::Drained {
                        ring,
                        matched: candidate_matched,
                    }
                }
                McuRxIrqActionKind::Unmask(mask) => {
                    io.unmask(mask)
                        .map_err(TransactionError::ContainmentRequiredIo)?;
                    McuRxIrqReport::Unmasked
                }
                McuRxIrqActionKind::Done(terminal) => {
                    debug_assert_eq!(matched.is_some(), terminal != McuRxIrqTerminal::None);
                    return Ok(matched);
                }
            };
            machine.report(action.step(), report).map_err(|error| {
                if error == McuRxIrqMachineError::DuplicateMatch {
                    TransactionError::DuplicateResponse
                } else {
                    TransactionError::ContainmentRequiredIrq(error)
                }
            })?;
        })();
        match operation {
            Ok(response) => Ok(response),
            Err(error) => {
                if machine.discard() == McuRxMaskState::KnownMasked
                    && let Err(source) = io.unmask(McuRxIrqTopology::firmware().mask())
                {
                    return Err(TransactionError::ContainmentRequiredIo(source));
                }
                Err(error)
            }
        }
    }

    fn drain_ring<I: McuIo>(
        &mut self,
        io: &mut I,
        ring: McuRxIrqRing,
        expected_sequence: u8,
    ) -> Result<Option<FirmwareRx>, TransactionError<I::Error>> {
        let cursor_index = match ring {
            McuRxIrqRing::Wm => 0,
            McuRxIrqRing::Wm2 => 1,
        };
        let ring_number = if ring == McuRxIrqRing::Wm { 0 } else { 4 };
        let dma_index = io
            .rx_dma_index(ring)
            .map_err(TransactionError::ContainmentRequiredIo)?;
        if dma_index >= MT7921_MCU_RX_RING_COUNT as u32 {
            return Err(TransactionError::InvalidDmaIndex {
                ring,
                index: dma_index,
            });
        }
        let dma_index = dma_index as u16;
        let mut matched = None;
        while self.rx_cursor[cursor_index] != dma_index {
            let slot = usize::from(self.rx_cursor[cursor_index]);
            let descriptor = io
                .read_rx_descriptor(ring, slot)
                .map_err(TransactionError::ContainmentRequiredIo)?;
            let length = ((descriptor.ctrl >> 16) & 0x3fff) as usize;
            let mut bytes = vec![0; length.min(MT7921_MCU_RX_BUFFER_BYTES)];
            let read = io
                .read_rx_buffer(ring, slot, &mut bytes)
                .map_err(TransactionError::ContainmentRequiredIo);
            let refill = (slot + MT7921_MCU_RX_RING_COUNT - 1) % MT7921_MCU_RX_RING_COUNT;
            let buffer_address = io
                .rx_buffer_address(ring, refill)
                .map_err(TransactionError::ContainmentRequiredIo)?;
            let fresh = mt7921_dma_rx(DmaSegment {
                iova: buffer_address,
                len: MT7921_MCU_RX_BUFFER_BYTES as u16,
            })
            .map_err(|_| TransactionError::Descriptor)?;
            io.rearm_rx_descriptor(ring, refill, fresh)
                .map_err(TransactionError::ContainmentRequiredIo)?;
            self.rx_cursor[cursor_index] = ((slot + 1) % MT7921_MCU_RX_RING_COUNT) as u16;
            io.publish_rx(ring, slot as u32)
                .map_err(TransactionError::ContainmentRequiredIo)?;
            // Route/parse errors cannot escape until rearm + CIDX above.
            read?;
            match route_mcu_rx_descriptor(ring_number, slot as u16, descriptor.ctrl, &bytes)
                .map_err(TransactionError::Route)?
            {
                McuRxRoute::Firmware(response) => {
                    if classify_firmware_rx(Some(expected_sequence), &response.response)
                        == FirmwareRxDisposition::Matched
                    {
                        if matched.is_some() {
                            return Err(TransactionError::DuplicateResponse);
                        }
                        matched = Some(response);
                    }
                }
                McuRxRoute::Normal(_) | McuRxRoute::TxFree(_) | McuRxRoute::TxStatus(_) => {}
            }
        }
        Ok(matched)
    }
}

pub(super) struct ActiveMcuViews<'a, B: Backend> {
    pub wfdma: MmioRegion<B>,
    pub tx_ring: &'a mut CoherentDma<B, Bidirectional>,
    pub payloads: &'a mut CoherentDma<B, ToDevice>,
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
}

impl<B: Backend> McuIo for ActiveMcuViews<'_, B> {
    type Error = drv_hardware::Error;

    fn payload_address(&self, slot: usize) -> Result<u64, Self::Error> {
        Ok(self
            .payloads
            .device_address(slot * COMMAND_SLOT_BYTES)?
            .bits())
    }
    fn write_payload(&mut self, slot: usize, bytes: &[u8]) -> Result<(), Self::Error> {
        self.payloads.write(slot * COMMAND_SLOT_BYTES, bytes)
    }
    fn write_tx_descriptor(
        &mut self,
        slot: usize,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error> {
        self.tx_ring
            .write(slot * DMA_DESCRIPTOR_LEN, &descriptor.to_le_bytes())
    }
    fn publish_tx(&mut self, producer: u32) -> Result<(), Self::Error> {
        self.wfdma.write_u32(MCU_TX_CIDX, producer)
    }
    fn wait_until(&mut self, deadline_ns: u64) -> Result<bool, Self::Error> {
        Ok(self.interrupt.wait_until(deadline_ns)?.is_some())
    }
    fn mask_host(&mut self) -> Result<(), Self::Error> {
        self.wfdma.write_u32(HOST_INT_ENABLE, 0)
    }
    fn read_host_status(&mut self) -> Result<u32, Self::Error> {
        self.wfdma.read_u32(HOST_INT_STATUS)
    }
    fn acknowledge(&mut self, status: u32) -> Result<(), Self::Error> {
        self.wfdma.write_u32(HOST_INT_STATUS, status)
    }
    fn rx_dma_index(&mut self, ring: McuRxIrqRing) -> Result<u32, Self::Error> {
        self.wfdma
            .read_u32(RX_RING_BASE + Self::ring_number(ring) * RING_STRIDE + RING_DIDX)
    }
    fn read_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: usize,
    ) -> Result<DmaDescriptor, Self::Error> {
        let mut bytes = [0; DMA_DESCRIPTOR_LEN];
        self.ring_dma(ring)
            .read(slot * DMA_DESCRIPTOR_LEN, &mut bytes)?;
        Ok(descriptor_from_bytes(bytes))
    }
    fn read_rx_buffer(
        &mut self,
        ring: McuRxIrqRing,
        slot: usize,
        bytes: &mut [u8],
    ) -> Result<(), Self::Error> {
        self.buffer_dma(ring)
            .read(slot * MT7921_MCU_RX_BUFFER_BYTES, bytes)
    }
    fn rx_buffer_address(&self, ring: McuRxIrqRing, slot: usize) -> Result<u64, Self::Error> {
        let offset = slot * MT7921_MCU_RX_BUFFER_BYTES;
        Ok(if ring == McuRxIrqRing::Wm {
            self.wm_buffers.device_address(offset)?.bits()
        } else {
            self.wm2_buffers.device_address(offset)?.bits()
        })
    }
    fn rearm_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: usize,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error> {
        self.ring_dma(ring)
            .write(slot * DMA_DESCRIPTOR_LEN, &descriptor.to_le_bytes())
    }
    fn publish_rx(&mut self, ring: McuRxIrqRing, producer: u32) -> Result<(), Self::Error> {
        self.wfdma.write_u32(
            RX_RING_BASE + Self::ring_number(ring) * RING_STRIDE + RING_CIDX,
            producer,
        )
    }
    fn unmask(&mut self, mask: u32) -> Result<(), Self::Error> {
        self.wfdma.write_u32(HOST_INT_ENABLE, mask)
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

#[cfg(test)]
mod tests {
    use super::*;
    use drv_hardware::DmaConstraints;
    use drv_hardware_backends::DeterministicBackend;
    use std::collections::VecDeque;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Op {
        Payload(usize, Vec<u8>),
        TxDescriptor(usize, DmaDescriptor),
        TxCidx(u32),
        Wait(u64),
        Mask,
        Status,
        Ack(u32),
        ReadDidx(McuRxIrqRing),
        ReadDescriptor(McuRxIrqRing, usize),
        ReadBuffer(McuRxIrqRing, usize),
        Rearm(McuRxIrqRing, usize),
        RxCidx(McuRxIrqRing, u32),
        Unmask(u32),
    }

    #[derive(Clone)]
    struct RxEntry {
        descriptor: DmaDescriptor,
        bytes: Vec<u8>,
    }

    struct FakeIo {
        ops: Vec<Op>,
        waits: VecDeque<bool>,
        statuses: VecDeque<u32>,
        didx: [VecDeque<u32>; 2],
        rx: [Vec<RxEntry>; 2],
        rearmed: Vec<(McuRxIrqRing, usize, DmaDescriptor)>,
        payload_base: u64,
        rx_base: [u64; 2],
        fail_publish: bool,
        fail: Option<FailAt>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FailAt {
        Mask,
        Status,
        Ack,
        Rearm,
        RxCidx,
        Unmask,
    }

    impl Default for FakeIo {
        fn default() -> Self {
            Self {
                ops: Vec::new(),
                waits: VecDeque::from([true]),
                statuses: VecDeque::from([McuRxIrqTopology::firmware().mask()]),
                didx: [VecDeque::new(), VecDeque::new()],
                rx: [Vec::new(), Vec::new()],
                rearmed: Vec::new(),
                payload_base: 0x3456_0000,
                rx_base: [0x4567_0000, 0x5678_0000],
                fail_publish: false,
                fail: None,
            }
        }
    }

    impl FakeIo {
        fn ring_index(ring: McuRxIrqRing) -> usize {
            usize::from(ring == McuRxIrqRing::Wm2)
        }
        fn push_rx(&mut self, ring: McuRxIrqRing, bytes: Vec<u8>) {
            let descriptor = DmaDescriptor {
                buf0: 0,
                ctrl: (1 << 31) | (1 << 30) | ((bytes.len() as u32) << 16),
                buf1: 0,
                info: 0,
            };
            self.rx[Self::ring_index(ring)].push(RxEntry { descriptor, bytes });
        }
        fn fails(&mut self, at: FailAt) -> bool {
            if self.fail == Some(at) {
                self.fail.take();
                true
            } else {
                false
            }
        }
    }

    impl McuIo for FakeIo {
        type Error = &'static str;

        fn payload_address(&self, slot: usize) -> Result<u64, Self::Error> {
            Ok(self.payload_base + (slot * COMMAND_SLOT_BYTES) as u64)
        }
        fn write_payload(&mut self, slot: usize, bytes: &[u8]) -> Result<(), Self::Error> {
            self.ops.push(Op::Payload(slot, bytes.to_vec()));
            Ok(())
        }
        fn write_tx_descriptor(
            &mut self,
            slot: usize,
            descriptor: DmaDescriptor,
        ) -> Result<(), Self::Error> {
            self.ops.push(Op::TxDescriptor(slot, descriptor));
            Ok(())
        }
        fn publish_tx(&mut self, producer: u32) -> Result<(), Self::Error> {
            self.ops.push(Op::TxCidx(producer));
            if self.fail_publish {
                Err("publish")
            } else {
                Ok(())
            }
        }
        fn wait_until(&mut self, deadline_ns: u64) -> Result<bool, Self::Error> {
            self.ops.push(Op::Wait(deadline_ns));
            Ok(self.waits.pop_front().unwrap_or(false))
        }
        fn mask_host(&mut self) -> Result<(), Self::Error> {
            self.ops.push(Op::Mask);
            if self.fails(FailAt::Mask) {
                Err("mask")
            } else {
                Ok(())
            }
        }
        fn read_host_status(&mut self) -> Result<u32, Self::Error> {
            self.ops.push(Op::Status);
            if self.fails(FailAt::Status) {
                Err("status")
            } else {
                Ok(self.statuses.pop_front().unwrap_or(0))
            }
        }
        fn acknowledge(&mut self, status: u32) -> Result<(), Self::Error> {
            self.ops.push(Op::Ack(status));
            if self.fails(FailAt::Ack) {
                Err("ack")
            } else {
                Ok(())
            }
        }
        fn rx_dma_index(&mut self, ring: McuRxIrqRing) -> Result<u32, Self::Error> {
            self.ops.push(Op::ReadDidx(ring));
            let index = Self::ring_index(ring);
            Ok(self.didx[index]
                .pop_front()
                .unwrap_or(self.rx[index].len() as u32))
        }
        fn read_rx_descriptor(
            &mut self,
            ring: McuRxIrqRing,
            slot: usize,
        ) -> Result<DmaDescriptor, Self::Error> {
            self.ops.push(Op::ReadDescriptor(ring, slot));
            Ok(self.rx[Self::ring_index(ring)][slot].descriptor)
        }
        fn read_rx_buffer(
            &mut self,
            ring: McuRxIrqRing,
            slot: usize,
            bytes: &mut [u8],
        ) -> Result<(), Self::Error> {
            self.ops.push(Op::ReadBuffer(ring, slot));
            bytes.copy_from_slice(&self.rx[Self::ring_index(ring)][slot].bytes[..bytes.len()]);
            Ok(())
        }
        fn rx_buffer_address(&self, ring: McuRxIrqRing, slot: usize) -> Result<u64, Self::Error> {
            Ok(self.rx_base[Self::ring_index(ring)] + (slot * MT7921_MCU_RX_BUFFER_BYTES) as u64)
        }
        fn rearm_rx_descriptor(
            &mut self,
            ring: McuRxIrqRing,
            slot: usize,
            descriptor: DmaDescriptor,
        ) -> Result<(), Self::Error> {
            self.ops.push(Op::Rearm(ring, slot));
            self.rearmed.push((ring, slot, descriptor));
            if self.fails(FailAt::Rearm) {
                Err("rearm")
            } else {
                Ok(())
            }
        }
        fn publish_rx(&mut self, ring: McuRxIrqRing, producer: u32) -> Result<(), Self::Error> {
            self.ops.push(Op::RxCidx(ring, producer));
            if self.fails(FailAt::RxCidx) {
                Err("rx cidx")
            } else {
                Ok(())
            }
        }
        fn unmask(&mut self, mask: u32) -> Result<(), Self::Error> {
            self.ops.push(Op::Unmask(mask));
            if self.fails(FailAt::Unmask) {
                Err("unmask")
            } else {
                Ok(())
            }
        }
    }

    fn firmware(sequence: u8, event: u8, option: u8) -> Vec<u8> {
        let mut bytes = vec![0; 36];
        bytes[24..26].copy_from_slice(&12u16.to_le_bytes());
        bytes[28] = event;
        bytes[29] = sequence;
        bytes[30] = option;
        bytes
    }

    #[test]
    fn shared_descriptor_uses_backend_address_and_publication_order_and_wraps() {
        let mut protocol = ActiveMcuProtocol {
            sequence: 15,
            ..Default::default()
        };
        let mut io = FakeIo::default();
        io.push_rx(McuRxIrqRing::Wm, firmware(1, 1, 0));
        let template = vec![0xa5; 64];
        protocol
            .transact(&mut io, &template, CompletionKind::FirmwareResponse, 77)
            .unwrap();
        let Op::Payload(0, payload) = &io.ops[0] else {
            panic!()
        };
        assert_eq!(payload[39], 1);
        assert_eq!(&payload[..39], &template[..39]);
        assert_eq!(&payload[40..], &template[40..]);
        let Op::TxDescriptor(0, descriptor) = io.ops[1] else {
            panic!()
        };
        assert_eq!(
            descriptor,
            mt7921_dma_tx(
                DmaSegment {
                    iova: io.payload_base,
                    len: 64
                },
                None,
                0
            )
            .unwrap()
        );
        assert_eq!(io.ops[2], Op::TxCidx(1));
        assert_eq!(protocol.sequence, 1);
    }

    #[test]
    fn template_boundary_rejects_47_and_accepts_48_bytes() {
        let mut protocol = ActiveMcuProtocol::default();
        let mut io = FakeIo::default();
        assert_eq!(
            protocol.transact(&mut io, &[0; 47], CompletionKind::FirmwareResponse, 1),
            Err(TransactionError::InvalidTemplate)
        );
        assert_eq!(
            protocol.transact(&mut io, &[0; 48], CompletionKind::FirmwareResponse, 1),
            Err(TransactionError::ContainmentRequiredTimeout)
        );
    }

    #[test]
    fn seven_posted_ring_refills_prior_empty_slot_advances_and_wraps() {
        let mut protocol = ActiveMcuProtocol::default();
        let mut io = FakeIo::default();
        io.push_rx(McuRxIrqRing::Wm, firmware(7, 1, 0));
        io.push_rx(McuRxIrqRing::Wm, firmware(7, 1, 0));
        io.didx[0] = VecDeque::from([1, 2]);
        protocol.drain_ring(&mut io, McuRxIrqRing::Wm, 1).unwrap();
        protocol.drain_ring(&mut io, McuRxIrqRing::Wm, 1).unwrap();
        let publications = io
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Rearm(..) | Op::RxCidx(..)))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            publications,
            [
                Op::Rearm(McuRxIrqRing::Wm, 7),
                Op::RxCidx(McuRxIrqRing::Wm, 0),
                Op::Rearm(McuRxIrqRing::Wm, 0),
                Op::RxCidx(McuRxIrqRing::Wm, 1),
            ]
        );
        assert_eq!(io.rearmed[0].2.buf0, (io.rx_base[0] + 7 * 2048) as u32);
        assert_eq!(io.rearmed[1].2.buf0, io.rx_base[0] as u32);

        protocol.rx_cursor[0] = 7;
        while io.rx[0].len() < MT7921_MCU_RX_RING_COUNT {
            io.push_rx(McuRxIrqRing::Wm, firmware(7, 1, 0));
        }
        io.didx[0].push_back(0);
        protocol.drain_ring(&mut io, McuRxIrqRing::Wm, 1).unwrap();
        assert_eq!(protocol.rx_cursor[0], 0);
        assert!(io.ops.windows(2).any(|ops| ops
            == [
                Op::Rearm(McuRxIrqRing::Wm, 6),
                Op::RxCidx(McuRxIrqRing::Wm, 7)
            ]));
    }

    #[test]
    fn out_of_range_dma_index_is_rejected_before_ring_access() {
        let mut protocol = ActiveMcuProtocol::default();
        let mut io = FakeIo::default();
        io.didx[0].push_back(8);
        let error = protocol
            .drain_ring(&mut io, McuRxIrqRing::Wm, 1)
            .unwrap_err();
        assert_eq!(
            error,
            TransactionError::InvalidDmaIndex {
                ring: McuRxIrqRing::Wm,
                index: 8,
            }
        );
        assert!(error.requires_containment());
        assert!(!io.ops.iter().any(|op| matches!(op, Op::ReadDescriptor(..))));
    }

    #[test]
    fn every_post_publication_io_failure_requires_containment_and_unwinds_mask() {
        for (failure, detail, known_masked) in [
            (FailAt::Mask, "mask", false),
            (FailAt::Status, "status", true),
            (FailAt::Ack, "ack", true),
            (FailAt::Rearm, "rearm", true),
            (FailAt::RxCidx, "rx cidx", true),
            (FailAt::Unmask, "unmask", true),
        ] {
            let mut protocol = ActiveMcuProtocol::default();
            let mut io = FakeIo {
                fail: Some(failure),
                ..Default::default()
            };
            io.push_rx(McuRxIrqRing::Wm, firmware(1, 1, 0));
            assert_eq!(
                protocol.transact(&mut io, &[0; 48], CompletionKind::FirmwareResponse, 1),
                Err(TransactionError::ContainmentRequiredIo(detail)),
                "{failure:?}"
            );
            let unmask_count = io
                .ops
                .iter()
                .filter(|op| matches!(op, Op::Unmask(_)))
                .count();
            assert_eq!(
                unmask_count,
                if failure == FailAt::Unmask {
                    2
                } else {
                    usize::from(known_masked)
                },
                "{failure:?}"
            );
        }
    }

    #[test]
    fn concrete_views_publish_the_backend_issued_payload_address() {
        let device = DeterministicBackend::device();
        let bar0 = device.open_region(0).unwrap();
        let mut tx_ring = device.alloc_coherent::<Bidirectional>(4096, 4096).unwrap();
        let mut payloads = device
            .alloc_coherent_with_constraints::<ToDevice>(
                256 * COMMAND_SLOT_BYTES,
                DmaConstraints {
                    alignment: 4096,
                    max_device_address: u32::MAX.into(),
                    max_segment_size: 256 * COMMAND_SLOT_BYTES,
                    max_segments: 1,
                },
            )
            .unwrap();
        let mut wm_ring = device.alloc_coherent::<Bidirectional>(4096, 4096).unwrap();
        let mut wm_buffers = device.alloc_coherent::<FromDevice>(16384, 4096).unwrap();
        let mut wm2_ring = device.alloc_coherent::<Bidirectional>(4096, 4096).unwrap();
        let mut wm2_buffers = device.alloc_coherent::<FromDevice>(16384, 4096).unwrap();
        let interrupt = device.open_interrupt(0).unwrap();
        let issued = payloads.device_address(0).unwrap().bits();
        let mut protocol = ActiveMcuProtocol::default();
        {
            let mut views = ActiveMcuViews {
                wfdma: bar0.slice(0xd4000, 4096).unwrap(),
                tx_ring: &mut tx_ring,
                payloads: &mut payloads,
                wm_ring: &mut wm_ring,
                wm_buffers: &mut wm_buffers,
                wm2_ring: &mut wm2_ring,
                wm2_buffers: &mut wm2_buffers,
                interrupt: &interrupt,
            };
            assert_eq!(
                protocol.transact(&mut views, &[0; 48], CompletionKind::FirmwareResponse, 5),
                Err(TransactionError::ContainmentRequiredTimeout)
            );
        }
        let mut descriptor = [0; DMA_DESCRIPTOR_LEN];
        tx_ring.read(0, &mut descriptor).unwrap();
        assert_eq!(descriptor_from_bytes(descriptor).buf0, issued as u32);
        // ToDevice intentionally has no CPU read API; the fake publication
        // test above proves byte 39 while this test proves the physical address.
        assert_eq!(protocol.sequence, 1);
    }

    #[test]
    fn wm_and_wm2_success_execute_shared_router_and_irq_machine() {
        for ring in [McuRxIrqRing::Wm, McuRxIrqRing::Wm2] {
            let mut protocol = ActiveMcuProtocol::default();
            let mut io = FakeIo::default();
            io.push_rx(ring, firmware(1, 1, 0));
            let response = protocol
                .transact(&mut io, &[0; 48], CompletionKind::FirmwareResponse, 9)
                .unwrap()
                .unwrap();
            assert_eq!(response.response.sequence, 1);
            assert!(
                io.ops
                    .windows(2)
                    .any(|w| w == [Op::Rearm(ring, 7), Op::RxCidx(ring, 0)])
            );
            assert_eq!(
                io.ops.last(),
                Some(&Op::Unmask(McuRxIrqTopology::firmware().mask()))
            );
            let irq = io
                .ops
                .iter()
                .filter(|op| {
                    matches!(
                        op,
                        Op::Mask | Op::Status | Op::Ack(_) | Op::ReadDidx(_) | Op::Unmask(_)
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(
                irq,
                [
                    Op::Mask,
                    Op::Status,
                    Op::Ack(McuRxIrqTopology::firmware().mask()),
                    Op::ReadDidx(McuRxIrqRing::Wm),
                    Op::ReadDidx(McuRxIrqRing::Wm2),
                    Op::Unmask(McuRxIrqTopology::firmware().mask()),
                ]
            );
        }
    }

    #[test]
    fn unrelated_and_unsolicited_do_not_complete_and_timeout() {
        let mut protocol = ActiveMcuProtocol::default();
        let mut io = FakeIo {
            waits: VecDeque::from([true, false]),
            ..Default::default()
        };
        io.push_rx(McuRxIrqRing::Wm, firmware(7, 1, 0));
        io.push_rx(McuRxIrqRing::Wm, firmware(1, 7, 0));
        assert_eq!(
            protocol.transact(&mut io, &[0; 48], CompletionKind::FirmwareResponse, 99),
            Err(TransactionError::ContainmentRequiredTimeout)
        );
    }

    #[test]
    fn duplicate_is_rejected_and_parse_error_rearms_before_surface() {
        let mut protocol = ActiveMcuProtocol::default();
        let mut io = FakeIo::default();
        io.push_rx(McuRxIrqRing::Wm, firmware(1, 1, 0));
        io.push_rx(McuRxIrqRing::Wm2, firmware(1, 1, 0));
        assert_eq!(
            protocol.transact(&mut io, &[0; 48], CompletionKind::FirmwareResponse, 1),
            Err(TransactionError::DuplicateResponse)
        );

        let mut protocol = ActiveMcuProtocol::default();
        let mut io = FakeIo::default();
        let mut malformed = vec![0; 36];
        malformed[24..26].copy_from_slice(&13u16.to_le_bytes());
        io.push_rx(McuRxIrqRing::Wm, malformed);
        assert!(matches!(
            protocol.transact(&mut io, &[0; 48], CompletionKind::FirmwareResponse, 1),
            Err(TransactionError::Route(_))
        ));
        let rearm = io
            .ops
            .iter()
            .position(|op| *op == Op::Rearm(McuRxIrqRing::Wm, 7))
            .unwrap();
        let cidx = io
            .ops
            .iter()
            .position(|op| *op == Op::RxCidx(McuRxIrqRing::Wm, 0))
            .unwrap();
        let unmask = io
            .ops
            .iter()
            .rposition(|op| matches!(op, Op::Unmask(_)))
            .unwrap();
        assert!(rearm < cidx && cidx < unmask);
    }

    #[test]
    fn failed_publication_consumes_sequence_and_producer_authority() {
        let mut protocol = ActiveMcuProtocol::default();
        let mut io = FakeIo {
            fail_publish: true,
            ..Default::default()
        };
        assert_eq!(
            protocol.transact(&mut io, &[0; 48], CompletionKind::FirmwareResponse, 1),
            Err(TransactionError::ContainmentRequiredIo("publish"))
        );
        assert_eq!((protocol.sequence, protocol.tx_producer), (1, 1));
    }
}
