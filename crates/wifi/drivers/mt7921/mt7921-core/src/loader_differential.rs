//! Differential oracle for the shared loader cutover.
//!
//! `active_mcu_legacy_oracle` retains the exact mechanics and tests from
//! `active_mcu.rs` at b23beb972aa9 (plus no_std compatibility imports). This
//! module supplies both it and `LoaderMechanics` with the
//! same semantic device and compares device-visible effects, results, and the
//! state made visible by subsequent publications.  Nothing here is compiled
//! outside `cfg(test)`.

use crate::active_mcu_legacy_oracle as legacy;
use alloc::{format, vec, vec::Vec};
use mt7921_core::{
    ChannelDomainChannel, ChannelDomainCommand, ClcSetCommand, DMA_DESCRIPTOR_LEN, DmaDescriptor,
    DmaSegment, DownloadCommand, FirmwareImagePart, LoaderCommandCompletion, LoaderCompletion,
    LoaderMechanics, LoaderMechanicsError, LoaderMechanicsTransport, MT7921_FWDL_RING_COUNT,
    MT7921_LOADER_RESPONSE_IRQ_MASK, MT7921_MCU_RX_BUFFER_BYTES, McuRxIrqRing, PhysicalBand,
    encode_channel_domain_command, encode_clc_set_command, encode_download_command, mt7921_dma_rx,
    mt7921_dma_tx,
};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

const COMMAND_SLOTS: u16 = 256;
const FWDL_SLOTS: u16 = MT7921_FWDL_RING_COUNT as u16;
const PAYLOAD_BASE: u64 = 0x3456_0000;
const SCATTER_BASE: u64 = 0x6789_0000;
const RX_BASE: [u64; 2] = [0x4567_0000, 0x5678_0000];

#[derive(Clone, Debug, Eq, PartialEq)]
enum Effect {
    CommandPayload(u16, Vec<u8>),
    CommandDescriptor(u16, DmaDescriptor),
    ResponseIrq(u32),
    CommandDoorbell(u16),
    CommandReclaimed(u16),
    Mask,
    Ack(u32),
    RxReposted(McuRxIrqRing, u16, DmaDescriptor),
    RxDoorbell(McuRxIrqRing, u16),
    ScatterPayload(Vec<u8>),
    ScatterDescriptor(u16, DmaDescriptor),
    ScatterDoorbell(u16),
    ScatterReclaimed(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailurePoint {
    CommandDoorbell,
    Wait,
    Mask,
    Status,
    Ack,
    RxRead,
    RxRepost,
    RxDoorbell,
    Unmask,
    CommandDidx,
    CommandReclaim,
    ScatterDoorbell,
    ScatterDidx,
    ScatterReclaim,
}

#[derive(Clone)]
struct RxEntry {
    descriptor: DmaDescriptor,
    bytes: Vec<u8>,
}

#[derive(Clone)]
struct Device {
    effects: Vec<Effect>,
    waits: VecDeque<bool>,
    progress: VecDeque<bool>,
    status: u32,
    command_didx: VecDeque<u32>,
    command_descriptors: [DmaDescriptor; COMMAND_SLOTS as usize],
    scatter_didx: VecDeque<u32>,
    scatter_descriptors: [DmaDescriptor; FWDL_SLOTS as usize],
    command_completion_done: bool,
    scatter_completion_done: bool,
    rx: [Vec<RxEntry>; 2],
    legacy_didx: [u32; 2],
    failure: Option<FailurePoint>,
    masked: bool,
    abort_count: usize,
}

impl Default for Device {
    fn default() -> Self {
        let empty = RxEntry {
            descriptor: rx_empty(0),
            bytes: Vec::new(),
        };
        Self {
            effects: Vec::new(),
            waits: VecDeque::from([true]),
            progress: VecDeque::from([true]),
            status: MT7921_LOADER_RESPONSE_IRQ_MASK,
            command_didx: VecDeque::from([1]),
            command_descriptors: [done(48); COMMAND_SLOTS as usize],
            scatter_didx: VecDeque::from([1]),
            scatter_descriptors: [done(1); FWDL_SLOTS as usize],
            command_completion_done: true,
            scatter_completion_done: true,
            rx: [vec![empty.clone(); 8], vec![empty; 8]],
            legacy_didx: [0; 2],
            failure: None,
            masked: false,
            abort_count: 0,
        }
    }
}

impl Device {
    fn ri(ring: McuRxIrqRing) -> usize {
        usize::from(ring == McuRxIrqRing::Wm2)
    }

    fn install_rx(&mut self, ring: McuRxIrqRing, slot: u16, bytes: Vec<u8>) {
        self.rx[Self::ri(ring)][slot as usize] = RxEntry {
            descriptor: done(bytes.len()),
            bytes,
        };
        self.legacy_didx[Self::ri(ring)] = u32::from((slot + 1) % 8);
    }

    fn fail(&mut self, point: FailurePoint) -> Result<(), &'static str> {
        if self.failure == Some(point) {
            self.failure = None;
            Err("scripted operation failure")
        } else {
            Ok(())
        }
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

fn rx_empty(slot: u16) -> DmaDescriptor {
    mt7921_dma_rx(DmaSegment {
        iova: RX_BASE[0] + u64::from(slot) * MT7921_MCU_RX_BUFFER_BYTES as u64,
        len: MT7921_MCU_RX_BUFFER_BYTES as u16,
    })
    .unwrap()
}

fn firmware(sequence: u8, event: u8, option: u8) -> Vec<u8> {
    let mut bytes = vec![0; 36];
    bytes[24..26].copy_from_slice(&12u16.to_le_bytes());
    bytes[28] = event;
    bytes[29] = sequence;
    bytes[30] = option;
    bytes
}

struct LegacyIo<'a>(&'a mut Device);

impl legacy::McuIo for LegacyIo<'_> {
    type Error = &'static str;
    fn payload_address(&self, slot: usize) -> Result<u64, Self::Error> {
        Ok(PAYLOAD_BASE + slot as u64 * 256)
    }
    fn write_payload(&mut self, slot: usize, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0
            .effects
            .push(Effect::CommandPayload(slot as u16, bytes.to_vec()));
        Ok(())
    }
    fn write_tx_descriptor(
        &mut self,
        slot: usize,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error> {
        self.0.command_descriptors[slot] = descriptor;
        self.0
            .effects
            .push(Effect::CommandDescriptor(slot as u16, descriptor));
        Ok(())
    }
    fn publish_tx(&mut self, producer: u32) -> Result<(), Self::Error> {
        self.0
            .effects
            .push(Effect::CommandDoorbell(producer as u16));
        self.0.fail(FailurePoint::CommandDoorbell)
    }
    fn wait_until(&mut self, _: u64) -> Result<bool, Self::Error> {
        self.0.fail(FailurePoint::Wait)?;
        Ok(self.0.waits.pop_front().unwrap_or(false))
    }
    fn mask_host(&mut self) -> Result<(), Self::Error> {
        self.0.effects.push(Effect::Mask);
        self.0.fail(FailurePoint::Mask)?;
        self.0.masked = true;
        Ok(())
    }
    fn read_host_status(&mut self) -> Result<u32, Self::Error> {
        self.0.fail(FailurePoint::Status)?;
        Ok(self.0.status)
    }
    fn acknowledge(&mut self, status: u32) -> Result<(), Self::Error> {
        self.0.effects.push(Effect::Ack(status));
        self.0.fail(FailurePoint::Ack)
    }
    fn rx_dma_index(&mut self, ring: McuRxIrqRing) -> Result<u32, Self::Error> {
        Ok(self.0.legacy_didx[Device::ri(ring)])
    }
    fn read_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: usize,
    ) -> Result<DmaDescriptor, Self::Error> {
        Ok(self.0.rx[Device::ri(ring)][slot].descriptor)
    }
    fn read_rx_buffer(
        &mut self,
        ring: McuRxIrqRing,
        slot: usize,
        bytes: &mut [u8],
    ) -> Result<(), Self::Error> {
        self.0.fail(FailurePoint::RxRead)?;
        bytes.copy_from_slice(&self.0.rx[Device::ri(ring)][slot].bytes[..bytes.len()]);
        Ok(())
    }
    fn rx_buffer_address(&self, ring: McuRxIrqRing, slot: usize) -> Result<u64, Self::Error> {
        Ok(RX_BASE[Device::ri(ring)] + slot as u64 * MT7921_MCU_RX_BUFFER_BYTES as u64)
    }
    fn rearm_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: usize,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error> {
        self.0
            .effects
            .push(Effect::RxReposted(ring, slot as u16, descriptor));
        self.0.fail(FailurePoint::RxRepost)?;
        self.0.rx[Device::ri(ring)][slot].descriptor = descriptor;
        Ok(())
    }
    fn publish_rx(&mut self, ring: McuRxIrqRing, producer: u32) -> Result<(), Self::Error> {
        self.0
            .effects
            .push(Effect::RxDoorbell(ring, producer as u16));
        self.0.fail(FailurePoint::RxDoorbell)
    }
    fn unmask(&mut self, mask: u32) -> Result<(), Self::Error> {
        self.0.effects.push(Effect::ResponseIrq(mask));
        self.0.fail(FailurePoint::Unmask)?;
        self.0.masked = false;
        Ok(())
    }
}

struct SharedIo<'a>(&'a mut Device);

impl LoaderMechanicsTransport for SharedIo<'_> {
    type Error = &'static str;
    fn command_payload_capacity(&self, _: u16) -> usize {
        256
    }
    fn command_payload_address(&self, slot: u16) -> Result<u64, Self::Error> {
        Ok(PAYLOAD_BASE + u64::from(slot) * 256)
    }
    fn write_command_payload(&mut self, slot: u16, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0
            .effects
            .push(Effect::CommandPayload(slot, bytes.to_vec()));
        Ok(())
    }
    fn write_command_descriptor(
        &mut self,
        slot: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error> {
        self.0.command_descriptors[slot as usize] = descriptor;
        self.0
            .effects
            .push(Effect::CommandDescriptor(slot, descriptor));
        Ok(())
    }
    fn enable_response_interrupts(&mut self, mask: u32) -> Result<(), Self::Error> {
        self.0.effects.push(Effect::ResponseIrq(mask));
        if self.0.masked {
            self.0.fail(FailurePoint::Unmask)?;
            self.0.masked = false;
        }
        Ok(())
    }
    fn publish_command_producer(&mut self, producer: u16) -> Result<(), Self::Error> {
        self.0.effects.push(Effect::CommandDoorbell(producer));
        self.0.fail(FailurePoint::CommandDoorbell)
    }
    fn command_dma_index(&mut self) -> Result<u32, Self::Error> {
        self.0.fail(FailurePoint::CommandDidx)?;
        let didx = self.0.command_didx.pop_front().unwrap_or(0);
        if self.0.command_completion_done && didx < u32::from(COMMAND_SLOTS) {
            let slot = (didx as u16 + COMMAND_SLOTS - 1) % COMMAND_SLOTS;
            self.0.command_descriptors[slot as usize].ctrl |= 1 << 31;
        }
        Ok(didx)
    }
    fn read_command_descriptor(&mut self, slot: u16) -> Result<DmaDescriptor, Self::Error> {
        Ok(self.0.command_descriptors[slot as usize])
    }
    fn reclaim_command(&mut self, slot: u16) -> Result<(), Self::Error> {
        self.0.fail(FailurePoint::CommandReclaim)?;
        self.0.effects.push(Effect::CommandReclaimed(slot));
        Ok(())
    }
    fn wait_for_interrupt(&mut self, _: u64) -> Result<bool, Self::Error> {
        self.0.fail(FailurePoint::Wait)?;
        Ok(self.0.waits.pop_front().unwrap_or(false))
    }
    fn wait_for_progress(&mut self, _: u64) -> Result<bool, Self::Error> {
        Ok(self.0.progress.pop_front().unwrap_or(false))
    }
    fn mask_response_interrupts(&mut self) -> Result<(), Self::Error> {
        self.0.effects.push(Effect::Mask);
        self.0.fail(FailurePoint::Mask)?;
        self.0.masked = true;
        Ok(())
    }
    fn response_interrupt_status(&mut self) -> Result<u32, Self::Error> {
        self.0.fail(FailurePoint::Status)?;
        Ok(self.0.status)
    }
    fn acknowledge_response_interrupts(&mut self, status: u32) -> Result<(), Self::Error> {
        self.0.effects.push(Effect::Ack(status));
        self.0.fail(FailurePoint::Ack)
    }
    fn read_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: u16,
    ) -> Result<DmaDescriptor, Self::Error> {
        Ok(self.0.rx[Device::ri(ring)][slot as usize].descriptor)
    }
    fn read_rx_buffer(
        &mut self,
        ring: McuRxIrqRing,
        slot: u16,
        bytes: &mut [u8],
    ) -> Result<(), Self::Error> {
        self.0.fail(FailurePoint::RxRead)?;
        bytes.copy_from_slice(&self.0.rx[Device::ri(ring)][slot as usize].bytes[..bytes.len()]);
        Ok(())
    }
    fn rx_buffer_address(&self, ring: McuRxIrqRing, slot: u16) -> Result<u64, Self::Error> {
        Ok(RX_BASE[Device::ri(ring)] + u64::from(slot) * MT7921_MCU_RX_BUFFER_BYTES as u64)
    }
    fn repost_rx_descriptor(
        &mut self,
        ring: McuRxIrqRing,
        slot: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error> {
        self.0
            .effects
            .push(Effect::RxReposted(ring, slot, descriptor));
        self.0.fail(FailurePoint::RxRepost)?;
        self.0.rx[Device::ri(ring)][slot as usize].descriptor = descriptor;
        Ok(())
    }
    fn publish_rx_producer(
        &mut self,
        ring: McuRxIrqRing,
        producer: u16,
    ) -> Result<(), Self::Error> {
        self.0.effects.push(Effect::RxDoorbell(ring, producer));
        self.0.fail(FailurePoint::RxDoorbell)
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
        self.0.abort_count += 1;
        Ok(())
    }
    fn scatter_payload_address(&self) -> Result<u64, Self::Error> {
        Ok(SCATTER_BASE)
    }
    fn write_scatter_payload(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0.effects.push(Effect::ScatterPayload(bytes.to_vec()));
        Ok(())
    }
    fn write_scatter_descriptor(
        &mut self,
        slot: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error> {
        self.0.scatter_descriptors[slot as usize] = descriptor;
        self.0
            .effects
            .push(Effect::ScatterDescriptor(slot, descriptor));
        Ok(())
    }
    fn publish_scatter_producer(&mut self, producer: u16) -> Result<(), Self::Error> {
        self.0.effects.push(Effect::ScatterDoorbell(producer));
        self.0.fail(FailurePoint::ScatterDoorbell)
    }
    fn scatter_dma_index(&mut self) -> Result<u32, Self::Error> {
        self.0.fail(FailurePoint::ScatterDidx)?;
        let didx = self.0.scatter_didx.pop_front().unwrap_or(0);
        if self.0.scatter_completion_done && didx < MT7921_FWDL_RING_COUNT {
            let slot = (didx as u16 + FWDL_SLOTS - 1) % FWDL_SLOTS;
            self.0.scatter_descriptors[slot as usize].ctrl |= 1 << 31;
        }
        Ok(didx)
    }
    fn read_scatter_descriptor(&mut self, slot: u16) -> Result<DmaDescriptor, Self::Error> {
        Ok(self.0.scatter_descriptors[slot as usize])
    }
    fn reclaim_scatter(&mut self, slot: u16) -> Result<(), Self::Error> {
        self.0.fail(FailurePoint::ScatterReclaim)?;
        self.0.effects.push(Effect::ScatterReclaimed(slot));
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Outcome {
    Response(Vec<u8>),
    NoResponse,
    Local,
    Containment,
    Protocol,
}

fn legacy_outcome(
    result: Result<Option<mt7921_core::FirmwareRx>, legacy::TransactionError<&'static str>>,
) -> Outcome {
    match result {
        Ok(Some(rx)) => Outcome::Response(rx.bytes),
        Ok(None) => Outcome::NoResponse,
        Err(error) if error.requires_containment() => Outcome::Containment,
        Err(legacy::TransactionError::InvalidTemplate) => Outcome::Local,
        Err(_) => Outcome::Protocol,
    }
}

fn shared_outcome(result: Result<LoaderCompletion, LoaderMechanicsError<&'static str>>) -> Outcome {
    match result {
        Ok(LoaderCompletion::Response(rx)) => Outcome::Response(rx.bytes),
        Ok(LoaderCompletion::NoResponse) => Outcome::NoResponse,
        Err(LoaderMechanicsError::Route(_) | LoaderMechanicsError::DuplicateResponse) => {
            Outcome::Protocol
        }
        Err(error) if error.requires_containment() => Outcome::Containment,
        Err(
            LoaderMechanicsError::InvalidCommandLength
            | LoaderMechanicsError::Encode(_)
            | LoaderMechanicsError::InvalidScatterLength,
        ) => Outcome::Local,
        Err(_) => Outcome::Protocol,
    }
}

// VFIO's pre-cutover command wrapper enabled the response IRQ before entering
// the retained executor and reclaimed after completion. Canonicalization moves
// setup writes into their doorbell transaction and ignores observational MMIO
// reads, while retaining every DMA/MMIO write.
fn canonical(mut effects: Vec<Effect>) -> Vec<Effect> {
    let mut output = Vec::new();
    let mut irq = None;
    let mut masked = false;
    for effect in effects.drain(..) {
        match effect {
            Effect::Mask => {
                masked = true;
                output.push(Effect::Mask);
            }
            Effect::ResponseIrq(mask) if !masked => irq = Some(mask),
            Effect::ResponseIrq(mask) => {
                masked = false;
                output.push(Effect::ResponseIrq(mask));
            }
            Effect::CommandDoorbell(producer) => {
                if let Some(mask) = irq.take() {
                    output.push(Effect::ResponseIrq(mask));
                }
                output.push(Effect::CommandDoorbell(producer));
            }
            other => output.push(other),
        }
    }
    if let Some(mask) = irq {
        output.push(Effect::ResponseIrq(mask));
    }
    output
}

fn assert_only_command_reclaim_delta(
    old: Vec<Effect>,
    new: Vec<Effect>,
    require_new_reclaim: bool,
) {
    let old = canonical(old);
    let new = canonical(new);
    let reclaims = |effects: &[Effect]| {
        effects
            .iter()
            .filter(|effect| matches!(effect, Effect::CommandReclaimed(_)))
            .cloned()
            .collect::<Vec<_>>()
    };
    let old_reclaims = reclaims(&old);
    let new_reclaims = reclaims(&new);
    if old_reclaims.is_empty() {
        let expected = if require_new_reclaim {
            let slot = new
                .iter()
                .find_map(|effect| match effect {
                    Effect::CommandDescriptor(slot, _) => Some(*slot),
                    _ => None,
                })
                .expect("reclamation requires a published descriptor");
            vec![Effect::CommandReclaimed(slot)]
        } else {
            Vec::new()
        };
        assert_eq!(new_reclaims, expected);
    } else {
        assert_eq!(new_reclaims, old_reclaims);
    }
    let without_reclaim = |effects: Vec<Effect>| {
        effects
            .into_iter()
            .filter(|effect| !matches!(effect, Effect::CommandReclaimed(_)))
            .collect::<Vec<_>>()
    };
    assert_eq!(without_reclaim(old), without_reclaim(new));
}

fn assert_unmask_retry_and_reclaim_delta(old: Vec<Effect>, new: Vec<Effect>) {
    let mut old = canonical(old);
    let mut new = canonical(new);
    let slot = new
        .iter()
        .find_map(|effect| match effect {
            Effect::CommandDescriptor(slot, _) => Some(*slot),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        new.iter()
            .filter(|effect| matches!(effect, Effect::CommandReclaimed(_)))
            .cloned()
            .collect::<Vec<_>>(),
        [Effect::CommandReclaimed(slot)]
    );
    new.retain(|effect| !matches!(effect, Effect::CommandReclaimed(_)));
    let retry = old
        .iter()
        .rposition(|effect| *effect == Effect::ResponseIrq(MT7921_LOADER_RESPONSE_IRQ_MASK))
        .unwrap();
    old.remove(retry);
    assert_eq!(old, new);
}

fn run_legacy_response(
    protocol: &mut legacy::ActiveMcuProtocol,
    device: &mut Device,
    template: &[u8],
) -> Outcome {
    device
        .effects
        .push(Effect::ResponseIrq(MT7921_LOADER_RESPONSE_IRQ_MASK));
    let result = protocol.transact(
        &mut LegacyIo(device),
        template,
        legacy::CompletionKind::FirmwareResponse,
        77,
    );
    let outcome = legacy_outcome(result);
    if matches!(outcome, Outcome::Response(_)) {
        let slot = device
            .effects
            .iter()
            .rev()
            .find_map(|e| {
                if let Effect::CommandDescriptor(slot, _) = e {
                    Some(slot)
                } else {
                    None
                }
            })
            .unwrap();
        device.effects.push(Effect::CommandReclaimed(*slot));
    }
    outcome
}

fn run_shared(
    engine: &mut LoaderMechanics,
    device: &mut Device,
    template: &[u8],
    completion: LoaderCommandCompletion,
) -> Outcome {
    shared_outcome(engine.execute_template(
        &mut SharedIo(device),
        &mut (),
        template,
        completion,
        77,
    ))
}

#[test]
fn old_and_shared_match_real_command_clc_channel_rx_and_wrap_traces() {
    let clc = encode_clc_set_command(
        &ClcSetCommand {
            index: 0,
            environment: 1,
            acpi_configuration: 1,
            capability: 1,
            alpha2: *b"00",
            rule_type: [1, 0],
            environment_6ghz: 0,
            mtcl_configuration: 0xff,
            data: vec![0x5a, 0xa5],
        },
        11,
    )
    .unwrap();
    let channel = encode_channel_domain_command(
        &ChannelDomainCommand {
            alpha2: *b"00",
            indoor: true,
            special_unii_mask: 0,
            channels: vec![ChannelDomainChannel {
                band: PhysicalBand::Ghz2,
                number: 1,
                flags: 1 << 1,
            }],
        },
        12,
    )
    .unwrap();
    let download = encode_download_command(DownloadCommand::PatchSemaphoreGet, 9).unwrap();

    for (ring, template) in [
        (McuRxIrqRing::Wm, download.as_slice()),
        (McuRxIrqRing::Wm2, clc.as_slice()),
    ] {
        let mut old_engine = legacy::ActiveMcuProtocol::default();
        let mut new_engine = LoaderMechanics::default();
        let mut old = Device::default();
        let mut new = old.clone();
        old.install_rx(ring, 0, firmware(1, 1, 0));
        new.install_rx(ring, 0, firmware(1, 1, 0));
        assert_eq!(
            run_legacy_response(&mut old_engine, &mut old, template),
            run_shared(
                &mut new_engine,
                &mut new,
                template,
                LoaderCommandCompletion::Response
            )
        );
        assert_eq!(canonical(old.effects), canonical(new.effects), "{ring:?}");
    }

    // Channel-domain is a real no-response encoding. Its publication mechanics
    // are compared to the exact old VFIO wrapper below (DIDX then reset).
    let mut new_engine = LoaderMechanics::default();
    let mut new = Device::default();
    let outcome = run_shared(
        &mut new_engine,
        &mut new,
        &channel,
        LoaderCommandCompletion::NoResponse,
    );
    let mut old = Device::default();
    let mut command = channel.clone();
    command[39] = 1;
    old.effects.push(Effect::CommandPayload(0, command.clone()));
    old.effects.push(Effect::CommandDescriptor(
        0,
        mt7921_dma_tx(
            DmaSegment {
                iova: PAYLOAD_BASE,
                len: command.len() as u16,
            },
            None,
            0,
        )
        .unwrap(),
    ));
    old.effects.push(Effect::CommandDoorbell(1));
    old.effects.push(Effect::CommandReclaimed(0));
    assert_eq!(outcome, Outcome::NoResponse);
    assert_eq!(canonical(old.effects), canonical(new.effects));

    // Observable state and both ring widths wrap without reading private fields.
    let template = encode_download_command(DownloadCommand::PatchSemaphoreGet, 8).unwrap();
    let mut old_engine = legacy::ActiveMcuProtocol::default();
    let mut new_engine = LoaderMechanics::default();
    let mut old = Device::default();
    let mut new = Device::default();
    for n in 0..256u16 {
        let sequence = (n % 15 + 1) as u8;
        let slot = n % 8;
        old.install_rx(McuRxIrqRing::Wm, slot, firmware(sequence, 1, 0));
        new.install_rx(McuRxIrqRing::Wm, slot, firmware(sequence, 1, 0));
        old.command_didx = VecDeque::from([u32::from((n + 1) % 256)]);
        new.command_didx = old.command_didx.clone();
        old.waits = VecDeque::from([true]);
        new.waits = old.waits.clone();
        assert_eq!(
            run_legacy_response(&mut old_engine, &mut old, &template),
            run_shared(
                &mut new_engine,
                &mut new,
                &template,
                LoaderCommandCompletion::Response
            ),
            "iteration {n}"
        );
    }
    assert_eq!(canonical(old.effects), canonical(new.effects));
}

#[test]
fn old_and_shared_match_response_error_and_operation_failure_traces() {
    enum RxCase {
        Match(McuRxIrqRing),
        Unrelated,
        Duplicate,
        Malformed,
        Timeout,
    }
    for case in [
        RxCase::Match(McuRxIrqRing::Wm),
        RxCase::Match(McuRxIrqRing::Wm2),
        RxCase::Unrelated,
        RxCase::Duplicate,
        RxCase::Malformed,
        RxCase::Timeout,
    ] {
        let mut old_engine = legacy::ActiveMcuProtocol::default();
        let mut new_engine = LoaderMechanics::default();
        let mut old = Device::default();
        let mut new = old.clone();
        match case {
            RxCase::Match(ring) => {
                old.install_rx(ring, 0, firmware(1, 1, 0));
                new.install_rx(ring, 0, firmware(1, 1, 0));
            }
            RxCase::Unrelated => {
                old.install_rx(McuRxIrqRing::Wm, 0, firmware(7, 1, 0));
                new.install_rx(McuRxIrqRing::Wm, 0, firmware(7, 1, 0));
                old.waits = VecDeque::from([true, false]);
                new.waits = old.waits.clone();
            }
            RxCase::Duplicate => {
                for d in [&mut old, &mut new] {
                    d.install_rx(McuRxIrqRing::Wm, 0, firmware(1, 1, 0));
                    d.install_rx(McuRxIrqRing::Wm2, 0, firmware(1, 1, 0));
                }
            }
            RxCase::Malformed => {
                let mut bytes = firmware(1, 1, 0);
                bytes[24..26].copy_from_slice(&13u16.to_le_bytes());
                old.install_rx(McuRxIrqRing::Wm, 0, bytes.clone());
                new.install_rx(McuRxIrqRing::Wm, 0, bytes);
            }
            RxCase::Timeout => {
                old.waits = VecDeque::from([false]);
                new.waits = old.waits.clone();
            }
        }
        let template = encode_download_command(DownloadCommand::PatchSemaphoreGet, 4).unwrap();
        assert_eq!(
            run_legacy_response(&mut old_engine, &mut old, &template),
            run_shared(
                &mut new_engine,
                &mut new,
                &template,
                LoaderCommandCompletion::Response
            )
        );
        assert_only_command_reclaim_delta(old.effects, new.effects, true);
    }

    for failure in [
        FailurePoint::CommandDoorbell,
        FailurePoint::Wait,
        FailurePoint::Mask,
        FailurePoint::Status,
        FailurePoint::Ack,
        FailurePoint::RxRead,
        FailurePoint::RxRepost,
        FailurePoint::RxDoorbell,
        FailurePoint::Unmask,
    ] {
        let mut old_engine = legacy::ActiveMcuProtocol::default();
        let mut new_engine = LoaderMechanics::default();
        let mut old = Device {
            failure: Some(failure),
            ..Device::default()
        };
        let mut new = old.clone();
        old.install_rx(McuRxIrqRing::Wm, 0, firmware(1, 1, 0));
        new.install_rx(McuRxIrqRing::Wm, 0, firmware(1, 1, 0));
        let template = encode_download_command(DownloadCommand::PatchSemaphoreGet, 4).unwrap();
        assert_eq!(
            run_legacy_response(&mut old_engine, &mut old, &template),
            run_shared(
                &mut new_engine,
                &mut new,
                &template,
                LoaderCommandCompletion::Response
            ),
            "{failure:?}"
        );
        if failure == FailurePoint::Unmask {
            // The retained executor retries an unmask once during its local
            // unwind; the shared engine instead returns containment-required.
            assert_unmask_retry_and_reclaim_delta(old.effects, new.effects);
        } else {
            assert_only_command_reclaim_delta(
                old.effects,
                new.effects,
                failure != FailurePoint::CommandDoorbell,
            );
        }
        if failure == FailurePoint::Wait {
            assert_eq!(
                new.abort_count, 0,
                "pre-service wait failure must not abort RX"
            );
        }
    }
}

// Exact test-only extraction of b23beb97's VfioFirmwareLoader scatter fields
// and publish/completion mechanics, with physical calls replaced one-for-one by
// the semantic Device effects above.
#[derive(Default)]
struct LegacyScatter {
    sequence: u8,
    index: u16,
    pending: Option<(FirmwareImagePart, u8, u16, u16)>,
}

impl LegacyScatter {
    fn publish(
        &mut self,
        d: &mut Device,
        part: FirmwareImagePart,
        chunk: &[u8],
    ) -> Result<u8, &'static str> {
        if self.pending.is_some() {
            return Err("pending");
        }
        if chunk.is_empty() || chunk.len() > 4096 {
            return Err("length");
        }
        self.sequence = self.sequence % 15 + 1;
        d.effects.push(Effect::ScatterPayload(chunk.to_vec()));
        let slot = self.index;
        let descriptor = mt7921_dma_tx(
            DmaSegment {
                iova: SCATTER_BASE,
                len: chunk.len() as u16,
            },
            None,
            0,
        )
        .unwrap();
        d.scatter_descriptors[slot as usize] = descriptor;
        d.effects.push(Effect::ScatterDescriptor(slot, descriptor));
        let producer = (slot + 1) % FWDL_SLOTS;
        d.effects.push(Effect::ScatterDoorbell(producer));
        d.fail(FailurePoint::ScatterDoorbell)?;
        self.index = producer;
        self.pending = Some((part, self.sequence, slot, producer));
        Ok(self.sequence)
    }
    fn complete(
        &mut self,
        d: &mut Device,
        part: FirmwareImagePart,
        sequence: u8,
    ) -> Result<(), &'static str> {
        let Some((p, s, slot, producer)) = self.pending else {
            return Err("none");
        };
        if (p, s) != (part, sequence) {
            return Err("mismatch");
        }
        d.fail(FailurePoint::ScatterDidx)?;
        if d.scatter_didx.pop_front().unwrap_or(0) != u32::from(producer) {
            return Err("timeout");
        }
        d.fail(FailurePoint::ScatterReclaim)?;
        d.effects.push(Effect::ScatterReclaimed(slot));
        self.pending = None;
        Ok(())
    }
}

#[test]
fn old_and_shared_match_scatter_publish_complete_mismatch_failures_and_wraps() {
    let mut old_engine = LegacyScatter::default();
    let mut new_engine = LoaderMechanics::default();
    let mut old = Device::default();
    let mut new = old.clone();
    for n in 0..128u16 {
        let part = if n & 1 == 0 {
            FirmwareImagePart::Patch
        } else {
            FirmwareImagePart::Ram
        };
        let chunk = vec![(n & 0xff) as u8; usize::from(n % 31 + 1)];
        old.scatter_didx = VecDeque::from([u32::from((n + 1) % 128)]);
        new.scatter_didx = old.scatter_didx.clone();
        let old_sequence = old_engine.publish(&mut old, part, &chunk).unwrap();
        let new_sequence = new_engine
            .publish_scatter(&mut SharedIo(&mut new), &mut (), part, &chunk)
            .unwrap();
        assert_eq!(old_sequence, new_sequence);
        old_engine.complete(&mut old, part, old_sequence).unwrap();
        new_engine
            .complete_scatter(&mut SharedIo(&mut new), &mut (), part, new_sequence, 9)
            .unwrap();
    }
    assert_eq!(old.effects, new.effects);

    for failure in [
        FailurePoint::ScatterDoorbell,
        FailurePoint::ScatterDidx,
        FailurePoint::ScatterReclaim,
    ] {
        let mut old_engine = LegacyScatter::default();
        let mut new_engine = LoaderMechanics::default();
        let mut old = Device {
            failure: Some(failure),
            ..Device::default()
        };
        let mut new = old.clone();
        let old_publish = old_engine.publish(&mut old, FirmwareImagePart::Patch, &[1]);
        let new_publish = new_engine.publish_scatter(
            &mut SharedIo(&mut new),
            &mut (),
            FirmwareImagePart::Patch,
            &[1],
        );
        if failure == FailurePoint::ScatterDoorbell {
            assert!(old_publish.is_err() && new_publish.is_err());
            assert_eq!(old_engine.index, 0);
            assert!(old_engine.pending.is_none());
            assert_eq!(new_engine.fwdl_producer(), 0);
            assert!(!new_engine.has_pending_scatter());
        } else {
            let os = old_publish.unwrap();
            let ns = new_publish.unwrap();
            assert!(
                old_engine
                    .complete(&mut old, FirmwareImagePart::Patch, os)
                    .is_err()
            );
            assert!(
                new_engine
                    .complete_scatter(
                        &mut SharedIo(&mut new),
                        &mut (),
                        FirmwareImagePart::Patch,
                        ns,
                        9
                    )
                    .is_err()
            );
        }
        assert_eq!(old.effects, new.effects, "{failure:?}");
    }

    let mut old_engine = LegacyScatter::default();
    let mut new_engine = LoaderMechanics::default();
    let mut old = Device::default();
    let mut new = old.clone();
    let os = old_engine
        .publish(&mut old, FirmwareImagePart::Patch, &[1])
        .unwrap();
    let ns = new_engine
        .publish_scatter(
            &mut SharedIo(&mut new),
            &mut (),
            FirmwareImagePart::Patch,
            &[1],
        )
        .unwrap();
    assert!(
        old_engine
            .complete(&mut old, FirmwareImagePart::Ram, os)
            .is_err()
    );
    assert!(
        new_engine
            .complete_scatter(
                &mut SharedIo(&mut new),
                &mut (),
                FirmwareImagePart::Ram,
                ns,
                9
            )
            .is_err()
    );
    assert_eq!(old.effects, new.effects);
}

#[test]
fn completion_descriptor_mismatches_are_detected_by_the_differential() {
    // The old VFIO implementation trusted exact DIDX; the shared engine also
    // demands DMA_DONE before reclaim. Preserve and continuously expose that
    // deliberate safety delta rather than hiding it in trace normalization.
    let template = encode_download_command(DownloadCommand::FirmwareLogToHost, 3).unwrap();
    let mut new_engine = LoaderMechanics::default();
    let mut new = Device::default();
    new.command_completion_done = false;
    let outcome = run_shared(
        &mut new_engine,
        &mut new,
        &template,
        LoaderCommandCompletion::NoResponse,
    );
    assert_eq!(outcome, Outcome::Containment);

    let mut new_engine = LoaderMechanics::default();
    let mut new = Device::default();
    let sequence = new_engine
        .publish_scatter(
            &mut SharedIo(&mut new),
            &mut (),
            FirmwareImagePart::Patch,
            &[1],
        )
        .unwrap();
    new.scatter_completion_done = false;
    assert!(matches!(
        new_engine.complete_scatter(
            &mut SharedIo(&mut new),
            &mut (),
            FirmwareImagePart::Patch,
            sequence,
            9
        ),
        Err(LoaderMechanicsError::ScatterDescriptorNotDone { slot: 0 })
    ));
}

#[test]
fn legacy_source_is_the_exact_parent_revision_fixture() {
    // Compile-time use above proves it remains executable; this pins accidental
    // edits without turning behavioral expected arrays into the oracle.
    let source = include_bytes!("active_mcu_legacy_oracle.rs");
    assert!(source.starts_with(b"//! Narrow, private MCU publication/receive executor."));
    assert_eq!(DMA_DESCRIPTOR_LEN, 16);
    assert_eq!(
        format!("{:x}", Sha256::digest(source)),
        "7563fd61a2bd248bd3542e68a88e7868b9cc68a7c99caf5bfde9c47e6dcb72d6"
    );
}
