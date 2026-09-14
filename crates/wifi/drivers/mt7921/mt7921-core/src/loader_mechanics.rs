//! Resource-free mechanics for the MT7921 firmware-loader DMA protocol.
//!
//! The engine owns only protocol cursors.  Its transport is expressed as
//! semantic effects so resource acquisition and physical handle ownership stay
//! in the embedding driver.

use alloc::vec;
use core::sync::atomic::{Ordering, fence};

use crate::{
    DescriptorError, DmaDescriptor, DmaSegment, DownloadCommand, DownloadCommandError,
    FirmwareImagePart, FirmwareRx, FirmwareRxDisposition, MT7921_FWDL_CHUNK_BYTES,
    MT7921_FWDL_RING_COUNT, MT7921_MCU_RX_BUFFER_BYTES, MT7921_MCU_RX_RING_COUNT,
    MT7921_MCU_TX_RING_COUNT, McuRxIrqRing, McuRxRoute, McuRxRouteError, classify_firmware_rx,
    encode_download_command, mt7921_dma_rx, mt7921_dma_tx, route_mcu_rx_descriptor,
};

pub const MT7921_LOADER_RESPONSE_IRQ_MASK: u32 = (1 << 0) | (1 << 22);
pub const MT7921_LOADER_COMMAND_MAX_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoaderCommandCompletion {
    Response,
    NoResponse,
}

impl LoaderCommandCompletion {
    pub const fn for_download(command: DownloadCommand) -> Self {
        if matches!(
            command,
            DownloadCommand::NicPowerControl | DownloadCommand::FirmwareLogToHost
        ) {
            Self::NoResponse
        } else {
            Self::Response
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoaderCompletion {
    Response(FirmwareRx),
    NoResponse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoaderMechanicsEvent {
    SequenceReserved {
        sequence: u8,
    },
    ResponseInterruptEnabled,
    CommandPublished {
        slot: u16,
        producer: u16,
        sequence: u8,
    },
    CommandReclaimed {
        slot: u16,
    },
    RxReposted {
        ring: McuRxIrqRing,
        consumed: u16,
        posted: u16,
        producer: u16,
    },
    ScatterPublished {
        part: FirmwareImagePart,
        slot: u16,
        producer: u16,
        sequence: u8,
    },
    ScatterReclaimed {
        part: FirmwareImagePart,
        slot: u16,
        sequence: u8,
    },
}

pub trait LoaderMechanicsObserver {
    fn observe_loader_mechanics(&mut self, event: LoaderMechanicsEvent);
}

impl LoaderMechanicsObserver for () {
    fn observe_loader_mechanics(&mut self, _: LoaderMechanicsEvent) {}
}

impl<F: FnMut(LoaderMechanicsEvent)> LoaderMechanicsObserver for F {
    fn observe_loader_mechanics(&mut self, event: LoaderMechanicsEvent) {
        self(event)
    }
}

/// Semantic hardware effects required by [`LoaderMechanics`].
///
/// No method exposes or accepts an MMIO, DMA, interrupt, or device handle.
pub trait LoaderMechanicsTransport {
    type Error;

    fn command_payload_capacity(&self, slot: u16) -> usize;
    fn command_payload_address(&self, slot: u16) -> Result<u64, Self::Error>;
    fn write_command_payload(&mut self, slot: u16, bytes: &[u8]) -> Result<(), Self::Error>;
    fn write_command_descriptor(
        &mut self,
        slot: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error>;
    fn enable_response_interrupts(&mut self, mask: u32) -> Result<(), Self::Error>;
    fn publish_command_producer(&mut self, producer: u16) -> Result<(), Self::Error>;
    /// Observe the device consumer with acquire semantics before later
    /// descriptor or payload reads.
    fn command_dma_index(&mut self) -> Result<u32, Self::Error>;
    fn read_command_descriptor(&mut self, slot: u16) -> Result<DmaDescriptor, Self::Error>;
    fn reclaim_command(&mut self, slot: u16) -> Result<(), Self::Error>;

    fn wait_for_interrupt(&mut self, deadline: u64) -> Result<bool, Self::Error>;
    /// Yield once while polling a completion that need not raise an IRQ.
    fn wait_for_progress(&mut self, deadline: u64) -> Result<bool, Self::Error>;
    fn mask_response_interrupts(&mut self) -> Result<(), Self::Error>;
    fn response_interrupt_status(&mut self) -> Result<u32, Self::Error>;
    fn acknowledge_response_interrupts(&mut self, status: u32) -> Result<(), Self::Error>;
    fn read_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: u16,
    ) -> Result<DmaDescriptor, Self::Error>;
    fn read_rx_buffer(
        &mut self,
        ring: McuRxIrqRing,
        slot: u16,
        bytes: &mut [u8],
    ) -> Result<(), Self::Error>;
    fn rx_buffer_address(&self, ring: McuRxIrqRing, slot: u16) -> Result<u64, Self::Error>;
    fn repost_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error>;
    fn publish_rx_producer(&mut self, ring: McuRxIrqRing, producer: u16)
    -> Result<(), Self::Error>;
    fn prepare_rx_result(
        &mut self,
        ring: McuRxIrqRing,
        slot: u16,
        posted: u16,
        raw: &[u8],
        routed: Result<&McuRxRoute, &McuRxRouteError>,
        disposition: Option<FirmwareRxDisposition>,
    ) -> Result<(), Self::Error>;
    fn complete_rx_result(&mut self, ring: McuRxIrqRing, slot: u16) -> Result<(), Self::Error>;
    fn abort_rx(&mut self) -> Result<(), Self::Error>;

    fn scatter_payload_address(&self) -> Result<u64, Self::Error>;
    fn write_scatter_payload(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    fn write_scatter_descriptor(
        &mut self,
        slot: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error>;
    fn publish_scatter_producer(&mut self, producer: u16) -> Result<(), Self::Error>;
    /// Observe the FWDL consumer with acquire semantics before reclamation.
    fn scatter_dma_index(&mut self) -> Result<u32, Self::Error>;
    fn read_scatter_descriptor(&mut self, slot: u16) -> Result<DmaDescriptor, Self::Error>;
    fn reclaim_scatter(&mut self, slot: u16) -> Result<(), Self::Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoaderMechanicsError<E> {
    Encode(DownloadCommandError),
    InvalidCommandLength,
    InvalidReservedSequence { reserved: u8, current: u8 },
    InvalidCommandDmaIndex(u32),
    InvalidScatterDmaIndex(u32),
    Descriptor(DescriptorError),
    RxDescriptorNotDone { ring: McuRxIrqRing, slot: u16 },
    TxDescriptorNotDone { slot: u16 },
    ScatterDescriptorNotDone { slot: u16 },
    Route(McuRxRouteError),
    DuplicateResponse,
    Timeout,
    CommandPending { slot: u16 },
    ScatterPending,
    NoScatterPending,
    ScatterMismatch,
    InvalidScatterLength,
    Transport(E),
    ContainmentRequired(E),
}

impl<E> LoaderMechanicsError<E> {
    pub const fn requires_containment(&self) -> bool {
        matches!(
            self,
            Self::ContainmentRequired(_)
                | Self::Timeout
                | Self::CommandPending { .. }
                | Self::RxDescriptorNotDone { .. }
                | Self::TxDescriptorNotDone { .. }
                | Self::ScatterDescriptorNotDone { .. }
                | Self::Route(_)
                | Self::DuplicateResponse
                | Self::InvalidCommandDmaIndex(_)
                | Self::InvalidScatterDmaIndex(_)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingScatter {
    part: FirmwareImagePart,
    sequence: u8,
    slot: u16,
    producer: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoaderMechanics {
    sequence: u8,
    command_producer: u16,
    pending_command: Option<u16>,
    rx_head: [u16; 2],
    fwdl_producer: u16,
    pending_scatter: Option<PendingScatter>,
}

impl Default for LoaderMechanics {
    fn default() -> Self {
        Self::new(0)
    }
}

impl LoaderMechanics {
    pub const fn new(sequence: u8) -> Self {
        Self {
            sequence,
            command_producer: 0,
            pending_command: None,
            rx_head: [0; 2],
            fwdl_producer: 0,
            pending_scatter: None,
        }
    }
    pub const fn sequence(&self) -> u8 {
        self.sequence
    }
    pub const fn command_producer(&self) -> u16 {
        self.command_producer
    }
    pub const fn rx_head(&self, ring: McuRxIrqRing) -> u16 {
        self.rx_head[ring_index(ring)]
    }
    pub const fn fwdl_producer(&self) -> u16 {
        self.fwdl_producer
    }
    pub const fn has_pending_scatter(&self) -> bool {
        self.pending_scatter.is_some()
    }

    /// Consume the shared command producer for a caller that supplies the
    /// semantic physical publication effects itself (used by post-loader
    /// command families during the staged cutover).
    pub fn commit_candidate_command(
        &mut self,
        sequence: u8,
    ) -> Result<(u16, u16), LoaderMechanicsError<core::convert::Infallible>> {
        if let Some(slot) = self.pending_command {
            return Err(LoaderMechanicsError::CommandPending { slot });
        }
        let expected = self.sequence % 15 + 1;
        if sequence != expected {
            return Err(LoaderMechanicsError::InvalidReservedSequence {
                reserved: sequence,
                current: expected,
            });
        }
        self.sequence = sequence;
        let slot = self.command_producer;
        let producer = next(slot, MT7921_MCU_TX_RING_COUNT as u16);
        self.command_producer = producer;
        Ok((slot, producer))
    }

    /// Reserve from the sole 1..=15 cursor.  Reservation deliberately mutates
    /// state before any encoder is called, so encoding failure still consumes
    /// the sequence just like a publication attempt of uncertain outcome.
    pub fn reserve_sequence<O: LoaderMechanicsObserver>(&mut self, observer: &mut O) -> u8 {
        self.sequence = self.sequence % 15 + 1;
        observer.observe_loader_mechanics(LoaderMechanicsEvent::SequenceReserved {
            sequence: self.sequence,
        });
        self.sequence
    }

    pub fn execute_download<T: LoaderMechanicsTransport, O: LoaderMechanicsObserver>(
        &mut self,
        transport: &mut T,
        observer: &mut O,
        command: DownloadCommand,
        deadline: u64,
    ) -> Result<LoaderCompletion, LoaderMechanicsError<T::Error>> {
        let sequence = self.reserve_sequence(observer);
        let encoded =
            encode_download_command(command, sequence).map_err(LoaderMechanicsError::Encode)?;
        self.execute_reserved_template(
            transport,
            observer,
            sequence,
            &encoded,
            LoaderCommandCompletion::for_download(command),
            deadline,
        )
    }

    pub fn execute_template<T: LoaderMechanicsTransport, O: LoaderMechanicsObserver>(
        &mut self,
        transport: &mut T,
        observer: &mut O,
        template: &[u8],
        completion: LoaderCommandCompletion,
        deadline: u64,
    ) -> Result<LoaderCompletion, LoaderMechanicsError<T::Error>> {
        if let Some(slot) = self.pending_command {
            return Err(LoaderMechanicsError::CommandPending { slot });
        }
        if template.len() < 48 || template.len() > MT7921_LOADER_COMMAND_MAX_BYTES {
            return Err(LoaderMechanicsError::InvalidCommandLength);
        }
        if template.len() > transport.command_payload_capacity(self.command_producer) {
            return Err(LoaderMechanicsError::InvalidCommandLength);
        }
        let sequence = self.reserve_sequence(observer);
        self.execute_reserved_template(
            transport, observer, sequence, template, completion, deadline,
        )
    }

    /// Publish a sequence already reserved from this engine. The wire byte is
    /// always stamped here; callers cannot make an encoder template authoritative.
    pub fn execute_reserved_template<T: LoaderMechanicsTransport, O: LoaderMechanicsObserver>(
        &mut self,
        t: &mut T,
        o: &mut O,
        sequence: u8,
        template: &[u8],
        completion: LoaderCommandCompletion,
        deadline: u64,
    ) -> Result<LoaderCompletion, LoaderMechanicsError<T::Error>> {
        if let Some(slot) = self.pending_command {
            return Err(LoaderMechanicsError::CommandPending { slot });
        }
        if sequence == 0 || sequence != self.sequence {
            return Err(LoaderMechanicsError::InvalidReservedSequence {
                reserved: sequence,
                current: self.sequence,
            });
        }
        if template.len() < 48 || template.len() > MT7921_LOADER_COMMAND_MAX_BYTES {
            return Err(LoaderMechanicsError::InvalidCommandLength);
        }
        if template.len() > t.command_payload_capacity(self.command_producer) {
            return Err(LoaderMechanicsError::InvalidCommandLength);
        }
        let mut encoded = template.to_vec();
        encoded[39] = sequence;
        let slot = self.command_producer;
        let address = t
            .command_payload_address(slot)
            .map_err(LoaderMechanicsError::Transport)?;
        let descriptor = mt7921_dma_tx(
            DmaSegment {
                iova: address,
                len: encoded
                    .len()
                    .try_into()
                    .map_err(|_| LoaderMechanicsError::InvalidCommandLength)?,
            },
            None,
            0,
        )
        .map_err(LoaderMechanicsError::Descriptor)?;
        t.write_command_payload(slot, &encoded)
            .map_err(LoaderMechanicsError::Transport)?;
        t.write_command_descriptor(slot, descriptor)
            .map_err(LoaderMechanicsError::Transport)?;
        if completion == LoaderCommandCompletion::Response {
            t.enable_response_interrupts(MT7921_LOADER_RESPONSE_IRQ_MASK)
                .map_err(LoaderMechanicsError::Transport)?;
            o.observe_loader_mechanics(LoaderMechanicsEvent::ResponseInterruptEnabled);
        }
        let producer = next(slot, MT7921_MCU_TX_RING_COUNT as u16);
        self.command_producer = producer;
        fence(Ordering::Release);
        // A failed MMIO publication may still have reached the device. Keep
        // exclusive payload ownership until exact consumption and reclaim.
        self.pending_command = Some(slot);
        t.publish_command_producer(producer)
            .map_err(LoaderMechanicsError::ContainmentRequired)?;
        o.observe_loader_mechanics(LoaderMechanicsEvent::CommandPublished {
            slot,
            producer,
            sequence,
        });

        let result = match completion {
            LoaderCommandCompletion::NoResponse => Ok(LoaderCompletion::NoResponse),
            LoaderCommandCompletion::Response => self
                .wait_response(t, o, sequence, deadline)
                .map(LoaderCompletion::Response),
        };
        // A response parse/correlation failure does not transfer the command
        // slot back to the caller.  Reclaim it from exact DIDX + DMA_DONE
        // evidence before surfacing that primary protocol failure.
        let reclaimed = self
            .wait_command_reclaim(t, slot, producer, deadline)
            .and_then(|()| {
                t.reclaim_command(slot)
                    .map_err(LoaderMechanicsError::ContainmentRequired)
            });
        if reclaimed.is_ok() {
            self.pending_command = None;
            o.observe_loader_mechanics(LoaderMechanicsEvent::CommandReclaimed { slot });
        }
        match (result, reclaimed) {
            (_, Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error),
            (Ok(completion), Ok(())) => Ok(completion),
        }
    }

    fn wait_command_reclaim<T: LoaderMechanicsTransport>(
        &mut self,
        t: &mut T,
        slot: u16,
        producer: u16,
        deadline: u64,
    ) -> Result<(), LoaderMechanicsError<T::Error>> {
        loop {
            let didx = t
                .command_dma_index()
                .map_err(LoaderMechanicsError::ContainmentRequired)?;
            if didx >= MT7921_MCU_TX_RING_COUNT {
                return Err(LoaderMechanicsError::InvalidCommandDmaIndex(didx));
            }
            if didx == u32::from(producer) {
                fence(Ordering::Acquire);
                break;
            }
            if !t
                .wait_for_progress(deadline)
                .map_err(LoaderMechanicsError::ContainmentRequired)?
            {
                let didx = t
                    .command_dma_index()
                    .map_err(LoaderMechanicsError::ContainmentRequired)?;
                if didx >= MT7921_MCU_TX_RING_COUNT {
                    return Err(LoaderMechanicsError::InvalidCommandDmaIndex(didx));
                }
                if didx != u32::from(producer) {
                    return Err(LoaderMechanicsError::Timeout);
                }
                fence(Ordering::Acquire);
                break;
            }
        }
        let descriptor = t
            .read_command_descriptor(slot)
            .map_err(LoaderMechanicsError::ContainmentRequired)?;
        if !descriptor.is_dma_done() {
            return Err(LoaderMechanicsError::TxDescriptorNotDone { slot });
        }
        Ok(())
    }

    fn wait_response<T: LoaderMechanicsTransport, O: LoaderMechanicsObserver>(
        &mut self,
        t: &mut T,
        o: &mut O,
        sequence: u8,
        deadline: u64,
    ) -> Result<FirmwareRx, LoaderMechanicsError<T::Error>> {
        loop {
            let interrupted = t
                .wait_for_interrupt(deadline)
                .map_err(LoaderMechanicsError::ContainmentRequired)?;
            if !interrupted {
                return Err(LoaderMechanicsError::Timeout);
            }
            if let Err(source) = t.mask_response_interrupts() {
                t.abort_rx()
                    .map_err(LoaderMechanicsError::ContainmentRequired)?;
                return Err(LoaderMechanicsError::ContainmentRequired(source));
            }
            let operation = (|| {
                let status = t
                    .response_interrupt_status()
                    .map_err(LoaderMechanicsError::ContainmentRequired)?;
                t.acknowledge_response_interrupts(status & MT7921_LOADER_RESPONSE_IRQ_MASK)
                    .map_err(LoaderMechanicsError::ContainmentRequired)?;
                let mut matched = None;
                for ring in [McuRxIrqRing::Wm, McuRxIrqRing::Wm2] {
                    if let Some(candidate) = self.drain_rx(t, o, ring, sequence)? {
                        if matched.is_some() {
                            return Err(LoaderMechanicsError::DuplicateResponse);
                        }
                        matched = Some(candidate);
                    }
                }
                Ok(matched)
            })();
            let unmask = t
                .enable_response_interrupts(MT7921_LOADER_RESPONSE_IRQ_MASK)
                .map_err(LoaderMechanicsError::ContainmentRequired);
            let matched = match (operation, unmask) {
                (Ok(matched), Ok(())) => matched,
                (operation, unmask) => {
                    t.abort_rx()
                        .map_err(LoaderMechanicsError::ContainmentRequired)?;
                    // Once masked, failure to restore the mask dominates the
                    // protocol error because interrupt state is ambiguous.
                    return Err(unmask.err().unwrap_or_else(|| operation.unwrap_err()));
                }
            };
            if let Some(response) = matched {
                return Ok(response);
            }
        }
    }

    fn drain_rx<T: LoaderMechanicsTransport, O: LoaderMechanicsObserver>(
        &mut self,
        t: &mut T,
        o: &mut O,
        ring: McuRxIrqRing,
        sequence: u8,
    ) -> Result<Option<FirmwareRx>, LoaderMechanicsError<T::Error>> {
        let ri = ring_index(ring);
        let mut matched = None;
        loop {
            let consumed = self.rx_head[ri];
            let descriptor = t
                .read_rx_descriptor(ring, consumed)
                .map_err(LoaderMechanicsError::ContainmentRequired)?;
            if !descriptor.is_dma_done() {
                break;
            }
            fence(Ordering::Acquire);
            let length = ((descriptor.ctrl >> 16) & 0x3fff) as usize;
            let mut bytes = vec![0; length.min(MT7921_MCU_RX_BUFFER_BYTES)];
            let read = t
                .read_rx_buffer(ring, consumed, &mut bytes)
                .map_err(LoaderMechanicsError::ContainmentRequired);
            let read_error = read.err();
            let routed = read_error.is_none().then(|| {
                let number = if ring == McuRxIrqRing::Wm { 0 } else { 4 };
                route_mcu_rx_descriptor(number, consumed, descriptor.ctrl, &bytes)
            });
            let disposition = routed.as_ref().and_then(|routed| {
                routed.as_ref().ok().and_then(|route| match route {
                    McuRxRoute::Firmware(response) => {
                        Some(classify_firmware_rx(Some(sequence), &response.response))
                    }
                    _ => None,
                })
            });
            let posted =
                (consumed + MT7921_MCU_RX_RING_COUNT as u16 - 1) % MT7921_MCU_RX_RING_COUNT as u16;
            if let Some(routed) = routed.as_ref() {
                t.prepare_rx_result(
                    ring,
                    consumed,
                    posted,
                    &bytes,
                    routed.as_ref().map_err(|error| error),
                    disposition,
                )
                .map_err(LoaderMechanicsError::ContainmentRequired)?;
            }
            let address = t
                .rx_buffer_address(ring, posted)
                .map_err(LoaderMechanicsError::ContainmentRequired)?;
            let fresh = mt7921_dma_rx(DmaSegment {
                iova: address,
                len: MT7921_MCU_RX_BUFFER_BYTES as u16,
            })
            .map_err(LoaderMechanicsError::Descriptor)?;
            t.repost_rx_descriptor(ring, posted, fresh)
                .map_err(LoaderMechanicsError::ContainmentRequired)?;
            let producer = consumed;
            self.rx_head[ri] = next(consumed, MT7921_MCU_RX_RING_COUNT as u16);
            fence(Ordering::Release);
            t.publish_rx_producer(ring, producer)
                .map_err(LoaderMechanicsError::ContainmentRequired)?;
            o.observe_loader_mechanics(LoaderMechanicsEvent::RxReposted {
                ring,
                consumed,
                posted,
                producer,
            });
            if let Some(error) = read_error {
                return Err(error);
            }
            t.complete_rx_result(ring, consumed)
                .map_err(LoaderMechanicsError::ContainmentRequired)?;
            let routed = routed.expect("successful read routes one descriptor");
            if let McuRxRoute::Firmware(response) = routed.map_err(LoaderMechanicsError::Route)?
                && disposition == Some(FirmwareRxDisposition::Matched)
            {
                if matched.is_some() {
                    return Err(LoaderMechanicsError::DuplicateResponse);
                }
                matched = Some(response);
            }
        }
        Ok(matched)
    }

    pub fn publish_scatter<T: LoaderMechanicsTransport, O: LoaderMechanicsObserver>(
        &mut self,
        t: &mut T,
        o: &mut O,
        part: FirmwareImagePart,
        chunk: &[u8],
    ) -> Result<u8, LoaderMechanicsError<T::Error>> {
        if self.pending_scatter.is_some() {
            return Err(LoaderMechanicsError::ScatterPending);
        }
        if chunk.is_empty() || chunk.len() > MT7921_FWDL_CHUNK_BYTES {
            return Err(LoaderMechanicsError::InvalidScatterLength);
        }
        let sequence = self.reserve_sequence(o);
        self.publish_reserved_scatter(t, o, part, sequence, chunk)?;
        Ok(sequence)
    }

    /// Publish a scatter using a sequence already reserved from this engine.
    pub fn publish_reserved_scatter<T: LoaderMechanicsTransport, O: LoaderMechanicsObserver>(
        &mut self,
        t: &mut T,
        o: &mut O,
        part: FirmwareImagePart,
        sequence: u8,
        chunk: &[u8],
    ) -> Result<(), LoaderMechanicsError<T::Error>> {
        if sequence == 0 || sequence != self.sequence {
            return Err(LoaderMechanicsError::InvalidReservedSequence {
                reserved: sequence,
                current: self.sequence,
            });
        }
        if self.pending_scatter.is_some() {
            return Err(LoaderMechanicsError::ScatterPending);
        }
        if chunk.is_empty() || chunk.len() > MT7921_FWDL_CHUNK_BYTES {
            return Err(LoaderMechanicsError::InvalidScatterLength);
        }
        let slot = self.fwdl_producer;
        let address = t
            .scatter_payload_address()
            .map_err(LoaderMechanicsError::Transport)?;
        let descriptor = mt7921_dma_tx(
            DmaSegment {
                iova: address,
                len: chunk.len() as u16,
            },
            None,
            0,
        )
        .map_err(LoaderMechanicsError::Descriptor)?;
        t.write_scatter_payload(chunk)
            .map_err(LoaderMechanicsError::Transport)?;
        t.write_scatter_descriptor(slot, descriptor)
            .map_err(LoaderMechanicsError::Transport)?;
        let producer = next(slot, MT7921_FWDL_RING_COUNT as u16);
        fence(Ordering::Release);
        t.publish_scatter_producer(producer)
            .map_err(LoaderMechanicsError::ContainmentRequired)?;
        self.fwdl_producer = producer;
        self.pending_scatter = Some(PendingScatter {
            part,
            sequence,
            slot,
            producer,
        });
        o.observe_loader_mechanics(LoaderMechanicsEvent::ScatterPublished {
            part,
            slot,
            producer,
            sequence,
        });
        Ok(())
    }

    pub fn complete_scatter<T: LoaderMechanicsTransport, O: LoaderMechanicsObserver>(
        &mut self,
        t: &mut T,
        o: &mut O,
        part: FirmwareImagePart,
        sequence: u8,
        deadline: u64,
    ) -> Result<(), LoaderMechanicsError<T::Error>> {
        let pending = self
            .pending_scatter
            .ok_or(LoaderMechanicsError::NoScatterPending)?;
        if (pending.part, pending.sequence) != (part, sequence) {
            return Err(LoaderMechanicsError::ScatterMismatch);
        }
        loop {
            let didx = t
                .scatter_dma_index()
                .map_err(LoaderMechanicsError::ContainmentRequired)?;
            if didx >= MT7921_FWDL_RING_COUNT {
                return Err(LoaderMechanicsError::InvalidScatterDmaIndex(didx));
            }
            if didx == u32::from(pending.producer) {
                fence(Ordering::Acquire);
                break;
            }
            if !t
                .wait_for_progress(deadline)
                .map_err(LoaderMechanicsError::ContainmentRequired)?
            {
                let didx = t
                    .scatter_dma_index()
                    .map_err(LoaderMechanicsError::ContainmentRequired)?;
                if didx >= MT7921_FWDL_RING_COUNT {
                    return Err(LoaderMechanicsError::InvalidScatterDmaIndex(didx));
                }
                if didx != u32::from(pending.producer) {
                    return Err(LoaderMechanicsError::Timeout);
                }
                fence(Ordering::Acquire);
                break;
            }
        }
        let descriptor = t
            .read_scatter_descriptor(pending.slot)
            .map_err(LoaderMechanicsError::ContainmentRequired)?;
        if !descriptor.is_dma_done() {
            return Err(LoaderMechanicsError::ScatterDescriptorNotDone { slot: pending.slot });
        }
        t.reclaim_scatter(pending.slot)
            .map_err(LoaderMechanicsError::ContainmentRequired)?;
        self.pending_scatter = None;
        o.observe_loader_mechanics(LoaderMechanicsEvent::ScatterReclaimed {
            part,
            slot: pending.slot,
            sequence,
        });
        Ok(())
    }
}

const fn ring_index(ring: McuRxIrqRing) -> usize {
    if matches!(ring, McuRxIrqRing::Wm) {
        0
    } else {
        1
    }
}
const fn next(index: u16, count: u16) -> u16 {
    (index + 1) % count
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Op {
        CommandPayload(u16, Vec<u8>),
        CommandDescriptor(u16),
        Enable,
        PublishCommand(u16),
        CommandDidx,
        ReclaimCommand(u16),
        Wait,
        Mask,
        Status,
        Ack(u32),
        ReadRx(McuRxIrqRing, u16),
        Repost(McuRxIrqRing, u16),
        RxCidx(McuRxIrqRing, u16),
        ScatterPayload(Vec<u8>),
        ScatterDescriptor(u16),
        PublishScatter(u16),
        ScatterDidx,
        ReclaimScatter(u16),
    }

    #[derive(Clone)]
    struct Rx {
        descriptor: DmaDescriptor,
        bytes: Vec<u8>,
    }

    struct Fake {
        ops: Vec<Op>,
        command_didx: VecDeque<u32>,
        command_descriptor: DmaDescriptor,
        scatter_didx: VecDeque<u32>,
        scatter_descriptor: DmaDescriptor,
        waits: VecDeque<bool>,
        rx: [Vec<Rx>; 2],
        rx_next: [usize; 2],
        fail_publish_command: bool,
        fail_publish_scatter: bool,
        command_capacity: usize,
        written_command_descriptor: Option<DmaDescriptor>,
        command_wipe_bytes: usize,
        scatter_wipe_bytes: usize,
    }

    impl Default for Fake {
        fn default() -> Self {
            Self {
                ops: Vec::new(),
                command_didx: VecDeque::from([1]),
                command_descriptor: done(64),
                scatter_didx: VecDeque::from([1]),
                scatter_descriptor: done(64),
                waits: VecDeque::from([true]),
                rx: core::array::from_fn(|_| {
                    (0..MT7921_MCU_RX_RING_COUNT)
                        .map(|_| Rx {
                            descriptor: mt7921_dma_rx(DmaSegment {
                                iova: 0x2000_0000,
                                len: MT7921_MCU_RX_BUFFER_BYTES as u16,
                            })
                            .unwrap(),
                            bytes: Vec::new(),
                        })
                        .collect()
                }),
                rx_next: [0; 2],
                fail_publish_command: false,
                fail_publish_scatter: false,
                command_capacity: MT7921_LOADER_COMMAND_MAX_BYTES,
                written_command_descriptor: None,
                command_wipe_bytes: 0,
                scatter_wipe_bytes: 0,
            }
        }
    }

    impl Fake {
        fn ri(ring: McuRxIrqRing) -> usize {
            usize::from(ring == McuRxIrqRing::Wm2)
        }
        fn push_rx(&mut self, ring: McuRxIrqRing, bytes: Vec<u8>) {
            let ri = Self::ri(ring);
            let slot = self.rx_next[ri];
            self.rx[ri][slot] = Rx {
                descriptor: done(bytes.len()),
                bytes,
            };
            self.rx_next[ri] += 1;
        }
    }

    fn done(length: usize) -> DmaDescriptor {
        DmaDescriptor {
            buf0: 0,
            ctrl: (1 << 31) | (1 << 30) | ((length as u32) << 16),
            buf1: 0,
            info: 0,
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

    impl LoaderMechanicsTransport for Fake {
        type Error = &'static str;
        fn command_payload_capacity(&self, _: u16) -> usize {
            self.command_capacity
        }
        fn command_payload_address(&self, slot: u16) -> Result<u64, Self::Error> {
            Ok(0x1000_0000 + u64::from(slot) * 4096)
        }
        fn write_command_payload(&mut self, slot: u16, bytes: &[u8]) -> Result<(), Self::Error> {
            self.ops.push(Op::CommandPayload(slot, bytes.to_vec()));
            Ok(())
        }
        fn write_command_descriptor(
            &mut self,
            slot: u16,
            descriptor: DmaDescriptor,
        ) -> Result<(), Self::Error> {
            self.written_command_descriptor = Some(descriptor);
            self.ops.push(Op::CommandDescriptor(slot));
            Ok(())
        }
        fn enable_response_interrupts(&mut self, _: u32) -> Result<(), Self::Error> {
            self.ops.push(Op::Enable);
            Ok(())
        }
        fn publish_command_producer(&mut self, producer: u16) -> Result<(), Self::Error> {
            self.ops.push(Op::PublishCommand(producer));
            if self.fail_publish_command {
                Err("command publish")
            } else {
                Ok(())
            }
        }
        fn command_dma_index(&mut self) -> Result<u32, Self::Error> {
            self.ops.push(Op::CommandDidx);
            Ok(self.command_didx.pop_front().unwrap_or(0))
        }
        fn read_command_descriptor(&mut self, _: u16) -> Result<DmaDescriptor, Self::Error> {
            Ok(self.command_descriptor)
        }
        fn reclaim_command(&mut self, slot: u16) -> Result<(), Self::Error> {
            self.command_wipe_bytes = MT7921_LOADER_COMMAND_MAX_BYTES;
            self.ops.push(Op::ReclaimCommand(slot));
            Ok(())
        }
        fn wait_for_interrupt(&mut self, _: u64) -> Result<bool, Self::Error> {
            self.ops.push(Op::Wait);
            Ok(self.waits.pop_front().unwrap_or(false))
        }
        fn wait_for_progress(&mut self, _: u64) -> Result<bool, Self::Error> {
            self.ops.push(Op::Wait);
            Ok(self.waits.pop_front().unwrap_or(false))
        }
        fn mask_response_interrupts(&mut self) -> Result<(), Self::Error> {
            self.ops.push(Op::Mask);
            Ok(())
        }
        fn response_interrupt_status(&mut self) -> Result<u32, Self::Error> {
            self.ops.push(Op::Status);
            Ok(MT7921_LOADER_RESPONSE_IRQ_MASK)
        }
        fn acknowledge_response_interrupts(&mut self, status: u32) -> Result<(), Self::Error> {
            self.ops.push(Op::Ack(status));
            Ok(())
        }
        fn read_rx_descriptor(
            &mut self,
            ring: McuRxIrqRing,
            slot: u16,
        ) -> Result<DmaDescriptor, Self::Error> {
            self.ops.push(Op::ReadRx(ring, slot));
            Ok(self.rx[Self::ri(ring)][slot as usize].descriptor)
        }
        fn read_rx_buffer(
            &mut self,
            ring: McuRxIrqRing,
            slot: u16,
            bytes: &mut [u8],
        ) -> Result<(), Self::Error> {
            bytes.copy_from_slice(&self.rx[Self::ri(ring)][slot as usize].bytes[..bytes.len()]);
            Ok(())
        }
        fn rx_buffer_address(&self, ring: McuRxIrqRing, slot: u16) -> Result<u64, Self::Error> {
            Ok(0x2000_0000 + (Self::ri(ring) as u64) * 0x10000 + u64::from(slot) * 2048)
        }
        fn repost_rx_descriptor(
            &mut self,
            ring: McuRxIrqRing,
            slot: u16,
            descriptor: DmaDescriptor,
        ) -> Result<(), Self::Error> {
            self.ops.push(Op::Repost(ring, slot));
            self.rx[Self::ri(ring)][slot as usize].descriptor = descriptor;
            Ok(())
        }
        fn publish_rx_producer(
            &mut self,
            ring: McuRxIrqRing,
            producer: u16,
        ) -> Result<(), Self::Error> {
            self.ops.push(Op::RxCidx(ring, producer));
            Ok(())
        }
        fn prepare_rx_result(
            &mut self,
            _: McuRxIrqRing,
            _: u16,
            _: u16,
            _: &[u8],
            _: Result<&McuRxRoute, &McuRxRouteError>,
            _: Option<FirmwareRxDisposition>,
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
            Ok(0x3000_0000)
        }
        fn write_scatter_payload(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
            self.ops.push(Op::ScatterPayload(bytes.to_vec()));
            Ok(())
        }
        fn write_scatter_descriptor(
            &mut self,
            slot: u16,
            _: DmaDescriptor,
        ) -> Result<(), Self::Error> {
            self.ops.push(Op::ScatterDescriptor(slot));
            Ok(())
        }
        fn publish_scatter_producer(&mut self, producer: u16) -> Result<(), Self::Error> {
            self.ops.push(Op::PublishScatter(producer));
            if self.fail_publish_scatter {
                Err("scatter publish")
            } else {
                Ok(())
            }
        }
        fn scatter_dma_index(&mut self) -> Result<u32, Self::Error> {
            self.ops.push(Op::ScatterDidx);
            Ok(self.scatter_didx.pop_front().unwrap_or(0))
        }
        fn read_scatter_descriptor(&mut self, _: u16) -> Result<DmaDescriptor, Self::Error> {
            Ok(self.scatter_descriptor)
        }
        fn reclaim_scatter(&mut self, slot: u16) -> Result<(), Self::Error> {
            self.scatter_wipe_bytes = MT7921_FWDL_CHUNK_BYTES;
            self.ops.push(Op::ReclaimScatter(slot));
            Ok(())
        }
    }

    #[test]
    fn transport_capacity_rejects_257_before_reservation_and_reclaims_full_256_slot() {
        let mut engine = LoaderMechanics::default();
        let mut io = Fake {
            command_capacity: 256,
            ..Fake::default()
        };
        assert_eq!(
            engine.execute_template(
                &mut io,
                &mut (),
                &[0; 257],
                LoaderCommandCompletion::NoResponse,
                1,
            ),
            Err(LoaderMechanicsError::InvalidCommandLength)
        );
        assert_eq!(engine.sequence(), 0);
        assert!(io.ops.is_empty());

        engine
            .execute_template(
                &mut io,
                &mut (),
                &[0xa5; 256],
                LoaderCommandCompletion::NoResponse,
                1,
            )
            .unwrap();
        assert!(matches!(&io.ops[0], Op::CommandPayload(0, bytes) if bytes.len() == 256));
        assert_eq!(io.ops.last(), Some(&Op::ReclaimCommand(0)));
        assert_eq!(engine.command_producer(), 1);
    }

    #[test]
    fn real_no_response_command_reserves_encodes_publishes_and_reclaims() {
        let mut engine = LoaderMechanics::new(15);
        let mut io = Fake::default();
        let mut events = Vec::new();
        assert_eq!(
            engine.execute_download(
                &mut io,
                &mut |e| events.push(e),
                DownloadCommand::FirmwareLogToHost,
                7
            ),
            Ok(LoaderCompletion::NoResponse)
        );
        let expected = encode_download_command(DownloadCommand::FirmwareLogToHost, 1).unwrap();
        assert_eq!(io.ops[0], Op::CommandPayload(0, expected));
        assert_eq!(
            &io.ops[1..],
            &[
                Op::CommandDescriptor(0),
                Op::PublishCommand(1),
                Op::CommandDidx,
                Op::ReclaimCommand(0)
            ]
        );
        assert!(!io.ops.contains(&Op::Enable));
        assert_eq!(engine.sequence(), 1);
        assert_eq!(
            events,
            [
                LoaderMechanicsEvent::SequenceReserved { sequence: 1 },
                LoaderMechanicsEvent::CommandPublished {
                    slot: 0,
                    producer: 1,
                    sequence: 1
                },
                LoaderMechanicsEvent::CommandReclaimed { slot: 0 }
            ]
        );
    }

    #[test]
    fn real_response_command_enables_irq_before_publication_drains_wm2_then_reclaims() {
        let mut engine = LoaderMechanics::default();
        let mut io = Fake::default();
        let mut observer = ();
        io.push_rx(McuRxIrqRing::Wm2, firmware(1, 1, 0));
        let result = engine
            .execute_download(
                &mut io,
                &mut observer,
                DownloadCommand::PatchSemaphoreGet,
                9,
            )
            .unwrap();
        assert!(matches!(result, LoaderCompletion::Response(ref rx) if rx.response.sequence == 1));
        let enable = io.ops.iter().position(|x| *x == Op::Enable).unwrap();
        let publish = io
            .ops
            .iter()
            .position(|x| *x == Op::PublishCommand(1))
            .unwrap();
        let reclaim = io
            .ops
            .iter()
            .position(|x| *x == Op::ReclaimCommand(0))
            .unwrap();
        assert!(enable < publish && publish < reclaim);
        assert!(io.ops.windows(2).any(|w| w
            == [
                Op::Repost(McuRxIrqRing::Wm2, 7),
                Op::RxCidx(McuRxIrqRing::Wm2, 0)
            ]));
    }

    #[test]
    fn encoding_failure_still_consumes_the_sole_sequence() {
        let mut engine = LoaderMechanics::new(14);
        let mut io = Fake::default();
        let mut observer = ();
        assert_eq!(
            engine.execute_download(
                &mut io,
                &mut observer,
                DownloadCommand::ReadEepromBlock { address: 3 },
                0
            ),
            Err(LoaderMechanicsError::Encode(
                DownloadCommandError::InvalidEepromAddress
            ))
        );
        assert_eq!(engine.sequence(), 15);
        assert!(io.ops.is_empty());
    }

    #[test]
    fn passive_candidate_commit_retains_both_cursors_until_publication_boundary() {
        let mut engine = LoaderMechanics::new(14);
        assert_eq!(engine.sequence(), 14);
        assert_eq!(engine.command_producer(), 0);
        assert!(matches!(
            engine.commit_candidate_command(1),
            Err(LoaderMechanicsError::InvalidReservedSequence { .. })
        ));
        assert_eq!((engine.sequence(), engine.command_producer()), (14, 0));
        assert_eq!(engine.commit_candidate_command(15), Ok((0, 1)));
        assert_eq!((engine.sequence(), engine.command_producer()), (15, 1));
    }

    #[test]
    fn seven_posted_rx_rearms_previous_empty_slot_and_wraps_exactly() {
        let mut engine = LoaderMechanics::default();
        let mut io = Fake::default();
        let mut observer = ();
        for _ in 0..8 {
            io.push_rx(McuRxIrqRing::Wm, firmware(9, 1, 0));
        }
        for entry in &mut io.rx[0][1..] {
            entry.descriptor = mt7921_dma_rx(DmaSegment {
                iova: 0x2000_0000,
                len: MT7921_MCU_RX_BUFFER_BYTES as u16,
            })
            .unwrap();
        }
        engine
            .drain_rx(&mut io, &mut observer, McuRxIrqRing::Wm, 1)
            .unwrap();
        engine.rx_head[0] = 7;
        io.rx[0][7].descriptor = done(36);
        engine
            .drain_rx(&mut io, &mut observer, McuRxIrqRing::Wm, 1)
            .unwrap();
        assert!(io.ops.windows(2).any(|w| w
            == [
                Op::Repost(McuRxIrqRing::Wm, 7),
                Op::RxCidx(McuRxIrqRing::Wm, 0)
            ]));
        assert!(io.ops.windows(2).any(|w| w
            == [
                Op::Repost(McuRxIrqRing::Wm, 6),
                Op::RxCidx(McuRxIrqRing::Wm, 7)
            ]));
        assert_eq!(engine.rx_head(McuRxIrqRing::Wm), 1);
    }

    #[test]
    fn rx_stops_at_first_descriptor_not_owned_by_cpu() {
        let mut engine = LoaderMechanics::default();
        let mut io = Fake::default();
        let mut observer = ();
        io.push_rx(McuRxIrqRing::Wm, firmware(1, 1, 0));
        io.rx[0][0].descriptor.ctrl &= !(1 << 31);
        assert_eq!(
            engine.drain_rx(&mut io, &mut observer, McuRxIrqRing::Wm, 1),
            Ok(None)
        );
        assert_eq!(engine.rx_head(McuRxIrqRing::Wm), 0);
    }

    #[test]
    fn command_publication_failure_consumes_both_cursors_and_requires_containment() {
        let mut engine = LoaderMechanics::default();
        let mut io = Fake {
            fail_publish_command: true,
            ..Fake::default()
        };
        let mut observer = ();
        let error = engine
            .execute_download(
                &mut io,
                &mut observer,
                DownloadCommand::FirmwareLogToHost,
                0,
            )
            .unwrap_err();
        assert_eq!(
            error,
            LoaderMechanicsError::ContainmentRequired("command publish")
        );
        assert!(error.requires_containment());
        assert_eq!((engine.sequence(), engine.command_producer()), (1, 1));
    }

    #[test]
    fn high_didx_values_cannot_alias_valid_reclamation_indices() {
        let mut engine = LoaderMechanics::default();
        let mut io = Fake {
            command_didx: VecDeque::from([0x0001_0001]),
            ..Fake::default()
        };
        let error = engine
            .execute_download(&mut io, &mut (), DownloadCommand::FirmwareLogToHost, 1)
            .unwrap_err();
        assert_eq!(
            error,
            LoaderMechanicsError::InvalidCommandDmaIndex(0x0001_0001)
        );
        assert!(error.requires_containment());

        let mut engine = LoaderMechanics::default();
        let mut io = Fake {
            scatter_didx: VecDeque::from([0x0001_0001]),
            ..Fake::default()
        };
        let sequence = engine
            .publish_scatter(&mut io, &mut (), FirmwareImagePart::Patch, &[1])
            .unwrap();
        let error = engine
            .complete_scatter(&mut io, &mut (), FirmwareImagePart::Patch, sequence, 1)
            .unwrap_err();
        assert_eq!(
            error,
            LoaderMechanicsError::InvalidScatterDmaIndex(0x0001_0001)
        );
        assert!(error.requires_containment());
    }

    #[test]
    fn scatter_uses_shared_sequence_single_pending_and_exact_reclamation() {
        let mut engine = LoaderMechanics::new(14);
        let mut io = Fake::default();
        let mut observer = ();
        let sequence = engine
            .publish_scatter(
                &mut io,
                &mut observer,
                FirmwareImagePart::Patch,
                &[0x5a; 4096],
            )
            .unwrap();
        assert_eq!(sequence, 15);
        assert!(engine.has_pending_scatter());
        assert_eq!(
            engine.publish_scatter(&mut io, &mut observer, FirmwareImagePart::Ram, &[1]),
            Err(LoaderMechanicsError::ScatterPending)
        );
        engine
            .complete_scatter(&mut io, &mut observer, FirmwareImagePart::Patch, 15, 4)
            .unwrap();
        assert!(!engine.has_pending_scatter());
        assert_eq!(io.ops.last(), Some(&Op::ReclaimScatter(0)));
        let next = engine
            .publish_scatter(&mut io, &mut observer, FirmwareImagePart::Ram, &[1])
            .unwrap();
        assert_eq!(next, 1);
        assert_eq!(engine.fwdl_producer(), 2);
    }

    #[test]
    fn ambiguous_command_ownership_prevents_shared_payload_overwrite() {
        for failure in 0..3 {
            let mut engine = LoaderMechanics::default();
            let mut io = Fake::default();
            match failure {
                0 => io.fail_publish_command = true,
                1 => {
                    io.command_didx = VecDeque::from([0, 0]);
                    io.waits = VecDeque::from([false]);
                }
                _ => io.command_descriptor.ctrl = 0,
            }
            let template = vec![0x5a; 48];
            assert!(
                engine
                    .execute_template(
                        &mut io,
                        &mut (),
                        &template,
                        LoaderCommandCompletion::NoResponse,
                        1,
                    )
                    .is_err()
            );
            let before = io.ops.clone();
            assert_eq!(
                engine.execute_template(
                    &mut io,
                    &mut (),
                    &template,
                    LoaderCommandCompletion::NoResponse,
                    2,
                ),
                Err(LoaderMechanicsError::CommandPending { slot: 0 })
            );
            assert_eq!(io.ops, before, "retry must not touch DMA or MMIO");
            assert!(matches!(
                engine.commit_candidate_command(3),
                Err(LoaderMechanicsError::CommandPending { slot: 0 })
            ));
        }
    }

    #[test]
    fn successful_reclaim_allows_next_shared_payload_publication_after_move() {
        let mut engine = LoaderMechanics::default();
        let mut io = Fake::default();
        let template = vec![0x5a; 48];
        engine
            .execute_template(
                &mut io,
                &mut (),
                &template,
                LoaderCommandCompletion::NoResponse,
                1,
            )
            .unwrap();
        let mut moved = engine;
        io.command_didx.push_back(2);
        moved
            .execute_template(
                &mut io,
                &mut (),
                &template,
                LoaderCommandCompletion::NoResponse,
                2,
            )
            .unwrap();
        let reclaimed = io
            .ops
            .iter()
            .position(|op| *op == Op::ReclaimCommand(0))
            .unwrap();
        let next_payload = io
            .ops
            .iter()
            .position(|op| matches!(op, Op::CommandPayload(1, _)))
            .unwrap();
        assert!(reclaimed < next_payload);
        assert_eq!(moved.sequence(), 2);
        assert_eq!(moved.command_producer(), 2);
    }

    #[test]
    fn exact_4096_byte_command_and_scatter_boundaries_are_owned_and_fully_wiped() {
        let mut engine = LoaderMechanics::default();
        let mut command = vec![0x5a; MT7921_LOADER_COMMAND_MAX_BYTES];
        command[39] = 0;
        let mut io = Fake {
            command_descriptor: done(MT7921_LOADER_COMMAND_MAX_BYTES),
            ..Fake::default()
        };
        assert_eq!(
            engine
                .execute_template(
                    &mut io,
                    &mut (),
                    &command,
                    LoaderCommandCompletion::NoResponse,
                    1,
                )
                .unwrap(),
            LoaderCompletion::NoResponse
        );
        let descriptor = io.written_command_descriptor.unwrap();
        assert_eq!(descriptor.buf0, 0x1000_0000);
        assert_eq!((descriptor.ctrl >> 16) & 0x3fff, 4096);
        assert_eq!(io.command_wipe_bytes, 4096);
        assert_eq!(
            engine.execute_template(
                &mut Fake::default(),
                &mut (),
                &[0; MT7921_LOADER_COMMAND_MAX_BYTES + 1],
                LoaderCommandCompletion::NoResponse,
                1,
            ),
            Err(LoaderMechanicsError::InvalidCommandLength)
        );

        let mut scatter = Fake {
            scatter_descriptor: done(MT7921_FWDL_CHUNK_BYTES),
            ..Fake::default()
        };
        let sequence = engine
            .publish_scatter(
                &mut scatter,
                &mut (),
                FirmwareImagePart::Ram,
                &[0xa5; MT7921_FWDL_CHUNK_BYTES],
            )
            .unwrap();
        engine
            .complete_scatter(&mut scatter, &mut (), FirmwareImagePart::Ram, sequence, 1)
            .unwrap();
        assert_eq!(scatter.scatter_wipe_bytes, 4096);
        assert_eq!(
            engine.publish_scatter(
                &mut Fake::default(),
                &mut (),
                FirmwareImagePart::Ram,
                &[0; MT7921_FWDL_CHUNK_BYTES + 1],
            ),
            Err(LoaderMechanicsError::InvalidScatterLength)
        );
    }

    #[test]
    fn scatter_doorbell_failure_consumes_sequence_without_committing_fwdl_authority() {
        let mut engine = LoaderMechanics::default();
        let mut io = Fake {
            fail_publish_scatter: true,
            ..Fake::default()
        };
        let mut observer = ();
        let error = engine
            .publish_scatter(&mut io, &mut observer, FirmwareImagePart::Patch, &[1])
            .unwrap_err();
        assert_eq!(
            error,
            LoaderMechanicsError::ContainmentRequired("scatter publish")
        );
        assert_eq!(engine.sequence(), 1);
        assert_eq!(engine.fwdl_producer(), 0);
        assert!(!engine.has_pending_scatter());
        assert_eq!(
            engine.complete_scatter(&mut io, &mut observer, FirmwareImagePart::Ram, 1, 0),
            Err(LoaderMechanicsError::NoScatterPending)
        );
    }

    #[test]
    fn malformed_and_duplicate_responses_are_reposted_before_failure() {
        let mut engine = LoaderMechanics::default();
        let mut io = Fake::default();
        let mut observer = ();
        io.push_rx(McuRxIrqRing::Wm, firmware(1, 1, 0));
        io.push_rx(McuRxIrqRing::Wm2, firmware(1, 1, 0));
        assert_eq!(
            engine.execute_download(
                &mut io,
                &mut observer,
                DownloadCommand::PatchSemaphoreGet,
                0
            ),
            Err(LoaderMechanicsError::DuplicateResponse)
        );
        assert!(io.ops.contains(&Op::Repost(McuRxIrqRing::Wm, 7)));
        assert!(io.ops.contains(&Op::Repost(McuRxIrqRing::Wm2, 7)));
    }
}
