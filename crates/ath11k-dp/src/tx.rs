// PORT-MAP: reusable
//! Client (STA) TCL transmit and WBM completion path.

use alloc::vec::Vec;
use ath11k_hal::descriptors::{
    ReoDestinationRing, RxdmaBufferRing, TclDataCommand, TxCommandInfo, WbmReleaseRing,
};
use ath11k_hal::{RingId, Rings};
use ath11k_platform_backend::{Backend, Device, MmioRegion};

use crate::dma::{RxBuffer, TxBuffer};
use crate::htt::TxCompletion;
use crate::lifecycle::{DpAllocationError, DpRingOps, Wcn6750DpRings};
use crate::reo::ReoController;
use crate::rx::{RxDescriptorStatus, WCN6750_RX_DESCRIPTOR_BYTES, Wcn6750RxDescriptor};
use crate::{DataPath, DataRings, DpError, RxPacket, TxPacket};

// idr_alloc(..., 0, DP_TX_IDR_SIZE - 1) uses an exclusive upper bound.
const MAX_MSDU_ID: u32 = 32_766;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum EncapType {
    Raw = 0,
    NativeWifi = 1,
    Ethernet = 2,
}

/// Per-vdev fields set by `ath11k_dp_vdev_tx_attach` and the client vif.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientTxConfig {
    pub return_buffer_manager: u8,
    pub pool_id: u8,
    pub mac_id: u8,
    pub lmac_id: u8,
    pub metadata: u16,
    pub encapsulation: EncapType,
    pub address_search_enable: u8,
    pub search_type: u8,
    pub ast_index: u16,
    pub ast_hash: u8,
    pub tid: u8,
    pub checksum_offload: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxResult {
    pub msdu_id: u32,
    pub status: u8,
    pub acknowledged: bool,
    pub ack_rssi: i8,
    pub peer: Option<crate::PeerId>,
}

struct PendingTx<B: Backend> {
    msdu_id: u32,
    #[allow(dead_code)]
    buffer: TxBuffer<B>,
}

struct PendingRx<B: Backend> {
    cookie: u32,
    buffer: RxBuffer<B>,
}

struct RxFragment {
    bytes: Vec<u8>,
    continuation: bool,
    peer: crate::PeerId,
    sequence_number: u16,
    tid: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RxdmaConfig {
    pub ring: RingId,
    pub pdev_id: u8,
    pub return_buffer_manager: u8,
    pub buffer_size: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceivedFrame {
    pub packet: RxPacket,
    pub status: RxDescriptorStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceResult {
    pub tx: Vec<TxResult>,
    pub rx: Vec<ReceivedFrame>,
}

/// DMA-owning client datapath. Ring descriptor mechanics remain in HAL.
pub struct ClientDataPath<B: Backend, R: Rings<B>> {
    device: Device<B>,
    rings: R,
    data_rings: Option<DataRings>,
    tx: ClientTxConfig,
    next_msdu_id: u32,
    pending: Vec<PendingTx<B>>,
    rxdma: Option<RxdmaConfig>,
    next_rx_cookie: u32,
    rx_buffers: Vec<PendingRx<B>>,
    monitor_status_buffers: Vec<PendingRx<B>>,
    rx_chain: Vec<RxFragment>,
    next_monitor_cookie: u32,
    ring_resources: Wcn6750DpRings,
    reo: Option<ReoController>,
}

impl<B: Backend, R: DpRingOps<B>> ClientDataPath<B, R> {
    /// Allocate the complete WCN6750 SoC-level TCL/WBM/REO ring set in the
    /// same order as `ath11k_dp_alloc`.
    pub fn ath11k_dp_alloc(
        device: Device<B>,
        mut rings: R,
        tx: ClientTxConfig,
    ) -> Result<Self, DpAllocationError<B, R>> {
        let mut ring_resources = Wcn6750DpRings::default();
        if let Err(error) = ring_resources.allocate_common(&device, &mut rings) {
            return Err(DpAllocationError::new(error, device, rings, ring_resources));
        }
        Ok(Self {
            device,
            rings,
            data_rings: None,
            tx,
            next_msdu_id: 0,
            pending: Vec::new(),
            rxdma: None,
            next_rx_cookie: 1,
            rx_buffers: Vec::new(),
            monitor_status_buffers: Vec::new(),
            rx_chain: Vec::new(),
            next_monitor_cookie: 1,
            ring_resources,
            reo: None,
        })
    }

    #[cfg(test)]
    fn without_allocated_rings(device: Device<B>, rings: R, tx: ClientTxConfig) -> Self {
        Self {
            device,
            rings,
            data_rings: None,
            tx,
            next_msdu_id: 0,
            pending: Vec::new(),
            rxdma: None,
            next_rx_cookie: 1,
            rx_buffers: Vec::new(),
            monitor_status_buffers: Vec::new(),
            rx_chain: Vec::new(),
            next_monitor_cookie: 1,
            ring_resources: Wcn6750DpRings::default(),
            reo: None,
        }
    }

    /// Tear down the SoC-level rings after all pdev phases have been freed.
    /// Dropping pending entries also performs the C TX DMA cleanup boundary.
    pub fn ath11k_dp_free(&mut self) -> Result<(), DpError> {
        if !self.ring_resources.pdev_rx().is_empty()
            || !self.ring_resources.reo_destination().is_empty()
            || self.reo.is_some()
        {
            return Err(DpError::WrongState);
        }
        self.ring_resources.free_common(&mut self.rings)?;
        self.pending.clear();
        Ok(())
    }

    /// Recover the backend owners after the complete DP teardown sequence.
    pub fn into_parts(self) -> Result<(Device<B>, R), DpError> {
        if !self.ring_resources.common().is_empty()
            || !self.ring_resources.pdev_rx().is_empty()
            || !self.ring_resources.reo_destination().is_empty()
            || self.reo.is_some()
        {
            return Err(DpError::WrongState);
        }
        Ok((self.device, self.rings))
    }

    pub fn rings(&self) -> &R {
        &self.rings
    }

    #[cfg(test)]
    fn rings_mut(&mut self) -> &mut R {
        &mut self.rings
    }

    pub fn ring_resources(&self) -> &Wcn6750DpRings {
        &self.ring_resources
    }

    /// `ath11k_dp_pdev_pre_alloc`: initialize the one WCN6750 pdev's buffer
    /// identifiers and pending-TX state before firmware start.
    pub fn ath11k_dp_pdev_pre_alloc(&mut self) -> Result<(), DpError> {
        if !self.pending.is_empty()
            || !self.rx_buffers.is_empty()
            || !self.monitor_status_buffers.is_empty()
        {
            return Err(DpError::WrongState);
        }
        self.next_msdu_id = 0;
        self.next_rx_cookie = 1;
        self.next_monitor_cookie = 1;
        self.rx_chain.clear();
        Ok(())
    }

    /// Allocate the four REO destination rings from
    /// `ath11k_dp_pdev_reo_setup` and bind the client data-ring view.
    pub fn ath11k_dp_pdev_reo_setup(&mut self, mmio: &MmioRegion<B>) -> Result<(), DpError> {
        if self.reo.is_some() {
            return Err(DpError::WrongState);
        }
        let data_rings = self
            .ring_resources
            .allocate_reo_destination(&self.device, &mut self.rings)?;
        let (command, status) = self.ring_resources.reo_controller_rings()?;
        let reo = match ReoController::ath11k_dp_pdev_reo_setup(mmio, command, status) {
            Ok(reo) => reo,
            Err(error) => {
                let _ = self.ring_resources.free_reo_destination(&mut self.rings);
                return Err(error);
            }
        };
        self.data_rings = Some(data_rings);
        self.reo = Some(reo);
        Ok(())
    }

    pub fn ath11k_dp_pdev_reo_cleanup(&mut self) -> Result<(), DpError> {
        if !self.rx_buffers.is_empty() || !self.monitor_status_buffers.is_empty() {
            return Err(DpError::WrongState);
        }
        self.ring_resources.free_reo_destination(&mut self.rings)?;
        self.data_rings = None;
        if let Some(reo) = self.reo.take() {
            reo.ath11k_dp_pdev_reo_cleanup();
        }
        Ok(())
    }

    pub fn ath11k_dp_pdev_alloc(&mut self) -> Result<(), DpError> {
        if self.reo.is_none() || self.data_rings.is_none() {
            return Err(DpError::WrongState);
        }
        let ring = self
            .ring_resources
            .allocate_pdev_rx(&self.device, &mut self.rings)?;
        let monitor_ring = self
            .ring_resources
            .pdev_ring(ath11k_hal::RingType::RxdmaMonitorStatus, 0)?;
        let config = RxdmaConfig {
            ring,
            pdev_id: 0,
            return_buffer_manager: 4,
            buffer_size: 2_048,
        };
        let allocation = self
            .ath11k_dp_rxbufs_replenish(config, 4_095)
            .and_then(|()| {
                replenish_pool(
                    &self.device,
                    &mut self.rings,
                    monitor_ring,
                    0,
                    4,
                    2_048,
                    1_023,
                    &mut self.next_monitor_cookie,
                    &mut self.monitor_status_buffers,
                )
            });
        if let Err(error) = allocation {
            if self.ring_resources.free_pdev_rx(&mut self.rings).is_ok() {
                self.rx_buffers.clear();
                self.monitor_status_buffers.clear();
                self.rxdma = None;
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn ath11k_dp_pdev_free(&mut self) -> Result<(), DpError> {
        self.ring_resources.free_pdev_rx(&mut self.rings)?;
        self.rx_buffers.clear();
        self.monitor_status_buffers.clear();
        self.rx_chain.clear();
        self.rxdma = None;
        Ok(())
    }

    /// `ath11k_dp_service_srng`: bounded interrupt service for client TX/RX.
    pub fn ath11k_dp_service_srng(&mut self, budget: usize) -> Result<ServiceResult, DpError> {
        let tx = self.service_tx_completions()?;
        let mut rx = Vec::new();
        for _ in 0..budget {
            match self.receive_with_status()? {
                Some(frame) => rx.push(frame),
                None => break,
            }
        }
        Ok(ServiceResult { tx, rx })
    }

    /// Drain WBM release entries as `ath11k_dp_tx_completion_handler` does.
    pub fn service_tx_completions(&mut self) -> Result<Vec<TxResult>, DpError> {
        let ring = self.data_rings.ok_or(DpError::NoResources)?.wbm;
        let mut results = Vec::new();
        while let Some(descriptor) = self.rings.consume(ring).map_err(map_hal)? {
            let release = WbmReleaseRing::from_bytes(descriptor.bytes())
                .map_err(|_| DpError::MalformedDescriptor)?;
            let cookie = release.buffer_address().software_cookie();
            let msdu_id = (cookie >> 2) & 0x1_ffff;
            let position = self
                .pending
                .iter()
                .position(|pending| pending.msdu_id == msdu_id)
                .ok_or(DpError::MalformedDescriptor)?;

            let htt = if release.release_source() == 3 {
                TxCompletion::decode_wbm_release(release.as_bytes())?
            } else {
                TxCompletion {
                    status: release.tqm_release_reason(),
                    reinject_reason: 0,
                    ack_rssi: release.ack_rssi() as i8,
                    peer: Some(crate::PeerId(release.peer_id())),
                }
            };
            // MEC notify (5) is WDS-only and unknown firmware statuses are
            // only logged by Linux; neither owns/completes this MSDU.
            if release.release_source() == 3 && htt.status >= 5 {
                continue;
            }
            // Removing drops the streaming mapping at the same point as the
            // C completion handler's dma_unmap_single.
            self.pending.swap_remove(position);
            results.push(TxResult {
                msdu_id,
                status: htt.status,
                acknowledged: htt.status == 0,
                ack_rssi: htt.ack_rssi,
                peer: htt.peer,
            });
        }
        Ok(results)
    }

    /// Configure and initially fill the client RXDMA buffer ring.
    pub fn ath11k_dp_rxbufs_replenish(
        &mut self,
        config: RxdmaConfig,
        count: usize,
    ) -> Result<(), DpError> {
        self.rxdma = Some(config);
        for _ in 0..count {
            self.replenish_one()?;
        }
        Ok(())
    }

    /// Interrupt-driven REO destination processing for direct MSDU buffers.
    pub fn receive_with_status(&mut self) -> Result<Option<ReceivedFrame>, DpError> {
        let reo_ring = self.data_rings.ok_or(DpError::NoResources)?.reo;
        loop {
            let descriptor = match self.rings.consume(reo_ring).map_err(map_hal)? {
                Some(descriptor) => descriptor,
                None => return Ok(None),
            };
            let destination = ReoDestinationRing::from_bytes(descriptor.bytes())
                .map_err(|_| DpError::MalformedDescriptor)?;
            let cookie = destination.buffer_address().software_cookie();
            let position = self
                .rx_buffers
                .iter()
                .position(|entry| entry.cookie == cookie)
                .ok_or(DpError::MalformedDescriptor)?;
            let mut entry = self.rx_buffers.swap_remove(position);
            let bytes = entry.buffer.sync_and_read(entry.buffer.len())?;
            // The C NAPI path replenishes every buffer reaped from the ring,
            // including buffers dropped for a non-routing push reason.
            self.replenish_one()?;
            if destination.push_reason() != 1 {
                continue;
            }
            let msdu = destination.msdu();
            let mpdu = destination.mpdu();
            self.rx_chain.push(RxFragment {
                bytes,
                continuation: msdu.continuation(),
                peer: crate::PeerId(mpdu.peer_id()),
                sequence_number: mpdu.sequence_number(),
                tid: destination.rx_queue_number() as u8,
            });
            if msdu.continuation() {
                continue;
            }
            let result = parse_received_chain(&self.rx_chain);
            self.rx_chain.clear();
            return result.map(Some);
        }
    }

    fn replenish_one(&mut self) -> Result<(), DpError> {
        let config = self.rxdma.ok_or(DpError::NoResources)?;
        replenish_pool(
            &self.device,
            &mut self.rings,
            config.ring,
            config.pdev_id,
            config.return_buffer_manager,
            config.buffer_size,
            1,
            &mut self.next_rx_cookie,
            &mut self.rx_buffers,
        )
    }

    fn allocate_msdu_id(&mut self) -> Result<u32, DpError> {
        for _ in 0..=MAX_MSDU_ID {
            let candidate = self.next_msdu_id;
            self.next_msdu_id = if candidate == MAX_MSDU_ID {
                0
            } else {
                candidate + 1
            };
            if !self.pending.iter().any(|entry| entry.msdu_id == candidate) {
                return Ok(candidate);
            }
        }
        Err(DpError::NoResources)
    }

    fn transmit_client(&mut self, mut packet: TxPacket) -> Result<(), DpError> {
        let data_rings = self.data_rings.ok_or(DpError::NoResources)?;
        if self.tx.encapsulation == EncapType::NativeWifi {
            encap_native_wifi(&mut packet.bytes)?;
        }
        let msdu_id = self.allocate_msdu_id()?;
        let buffer = TxBuffer::map(&self.device, &packet.bytes)?;
        let descriptor = make_tcl_descriptor(&buffer, msdu_id, self.tx)?;
        self.rings
            .publish(data_rings.tcl, descriptor.into_descriptor())
            .map_err(map_hal)?;
        self.pending.push(PendingTx { msdu_id, buffer });
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn replenish_pool<B: Backend, R: Rings<B>>(
    device: &Device<B>,
    rings: &mut R,
    ring: RingId,
    pdev_id: u8,
    return_buffer_manager: u8,
    buffer_size: usize,
    count: usize,
    next_cookie: &mut u32,
    buffers: &mut Vec<PendingRx<B>>,
) -> Result<(), DpError> {
    for _ in 0..count {
        let buffer_id = *next_cookie & 0x3_ffff;
        let cookie = buffer_id | ((u32::from(pdev_id) & 7) << 18);
        *next_cookie = if buffer_id == 0x3_ffff {
            1
        } else {
            buffer_id + 1
        };
        let buffer = RxBuffer::replenish(device, buffer_size)?;
        let descriptor =
            RxdmaBufferRing::for_buffer(&buffer.device_address()?, cookie, return_buffer_manager);
        rings
            .publish(ring, descriptor.into_descriptor())
            .map_err(map_hal)?;
        buffers.push(PendingRx { cookie, buffer });
    }
    Ok(())
}

impl<B: Backend, R: DpRingOps<B>> DataPath for ClientDataPath<B, R> {
    fn configure(&mut self, rings: DataRings) -> Result<(), DpError> {
        if !self.ring_resources.common().is_empty() {
            return Err(DpError::WrongState);
        }
        self.data_rings = Some(rings);
        Ok(())
    }

    fn transmit(&mut self, packet: TxPacket) -> Result<(), DpError> {
        self.transmit_client(packet)
    }

    fn receive(&mut self) -> Result<Option<RxPacket>, DpError> {
        Ok(self.receive_with_status()?.map(|received| received.packet))
    }
}

#[cfg(test)]
fn parse_received_buffer(bytes: &[u8]) -> Result<ReceivedFrame, DpError> {
    parse_received_chain(&[RxFragment {
        bytes: bytes.to_vec(),
        continuation: false,
        peer: crate::PeerId(0xffff),
        sequence_number: 0,
        tid: 0,
    }])
}

fn parse_received_chain(fragments: &[RxFragment]) -> Result<ReceivedFrame, DpError> {
    let first = fragments.first().ok_or(DpError::MalformedDescriptor)?;
    let last = fragments.last().ok_or(DpError::MalformedDescriptor)?;
    let descriptor = Wcn6750RxDescriptor::parse(&first.bytes)?;
    let last_descriptor = Wcn6750RxDescriptor::parse(&last.bytes)?;
    let mut status = descriptor.status();
    let end_status = last_descriptor.status();
    status.first_msdu = end_status.first_msdu;
    status.last_msdu = end_status.last_msdu;
    status.l3_padding = end_status.l3_padding;
    status.msdu_done = end_status.msdu_done;
    status.msdu_length_error = end_status.msdu_length_error;
    status.fcs_error = end_status.fcs_error;
    status.decrypt_error = end_status.decrypt_error;
    status.tkip_mic_error = end_status.tkip_mic_error;
    status.decrypted = end_status.decrypted;
    if !status.multicast_broadcast && first.peer.0 != 0xffff {
        status.peer = first.peer;
        status.sequence_number = first.sequence_number;
        status.tid = first.tid;
    }
    if status.msdu_length_error || !status.msdu_done {
        return Err(DpError::MalformedDescriptor);
    }
    let mut remaining = usize::from(status.msdu_length);
    let mut payload = Vec::with_capacity(remaining);
    for (index, fragment) in fragments.iter().enumerate() {
        let start = WCN6750_RX_DESCRIPTOR_BYTES
            + if index == 0 {
                usize::from(status.l3_padding)
            } else {
                0
            };
        let capacity = fragment.bytes.len().saturating_sub(start);
        let take = remaining.min(capacity);
        payload.extend_from_slice(
            fragment
                .bytes
                .get(start..start + take)
                .ok_or(DpError::MalformedDescriptor)?,
        );
        remaining -= take;
        if remaining == 0 {
            break;
        }
        if !fragment.continuation {
            return Err(DpError::MalformedDescriptor);
        }
    }
    if remaining != 0 {
        return Err(DpError::MalformedDescriptor);
    }
    Ok(ReceivedFrame {
        packet: RxPacket {
            peer: Some(status.peer),
            bytes: payload,
        },
        status,
    })
}

fn make_tcl_descriptor<B: Backend>(
    buffer: &TxBuffer<B>,
    msdu_id: u32,
    config: ClientTxConfig,
) -> Result<TclDataCommand, DpError> {
    let cookie = u32::from(config.mac_id) | (msdu_id << 2) | (u32::from(config.pool_id) << 19);
    let checksum_flags = if config.checksum_offload && config.encapsulation != EncapType::Raw {
        (1 << 16) | (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20)
    } else {
        0
    };
    Ok(TclDataCommand::for_transmit(
        &buffer.device_address()?,
        TxCommandInfo {
            metadata_flags: config.metadata,
            descriptor_id: cookie,
            descriptor_type: 0,
            encapsulation_type: config.encapsulation as u8,
            data_length: buffer.length() as u32,
            packet_offset: 0,
            encryption_type: 0,
            flags0: checksum_flags,
            flags1: 1 << 21,
            address_search_flags: u16::from(config.address_search_enable),
            bss_ast_hash: u16::from(config.ast_hash),
            bss_ast_index: config.ast_index,
            tid: config.tid,
            search_type: config.search_type,
            lmac_id: config.lmac_id,
            dscp_tid_table: 0,
            mesh_enable: false,
            return_buffer_manager: config.return_buffer_manager,
        },
    ))
}

/// `ath11k_dp_tx_encap_nwifi`: remove the QoS control and clear QoS subtype.
fn encap_native_wifi(frame: &mut Vec<u8>) -> Result<(), DpError> {
    let fc_bytes = frame.get(..2).ok_or(DpError::InvalidFrame)?;
    let mut frame_control = u16::from_le_bytes([fc_bytes[0], fc_bytes[1]]);
    let is_data = frame_control & 0x000c == 0x0008;
    if !is_data {
        return Err(DpError::InvalidFrame);
    }
    let is_qos = is_data && frame_control & 0x0080 != 0;
    if !is_qos {
        return Ok(());
    }
    let has_address4 = frame_control & 0x0300 == 0x0300;
    let qos_offset = if has_address4 { 30 } else { 24 };
    if frame.len() < qos_offset + 2 {
        return Err(DpError::InvalidFrame);
    }
    frame.drain(qos_offset..qos_offset + 2);
    frame_control &= !0x0080;
    frame[..2].copy_from_slice(&frame_control.to_le_bytes());
    Ok(())
}

fn map_hal(error: ath11k_hal::HalError) -> DpError {
    match error {
        ath11k_hal::HalError::WrongDescriptorLength => DpError::MalformedDescriptor,
        ath11k_hal::HalError::NoResources => DpError::NoResources,
        ath11k_hal::HalError::DeviceFault => DpError::DeviceFault,
        ath11k_hal::HalError::Unsupported => DpError::UnsupportedDescriptor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec;
    use ath11k_hal::Descriptor;
    use ath11k_platform_backend::{DmaConstraints, DmaDirection, Error as HardwareError, IrqEvent};
    use core::ops::Range;
    use drv_hardware_backends::{DeterministicBackend, Operation};

    #[derive(Default)]
    struct ModelRings {
        published: Vec<(RingId, Descriptor)>,
        completions: VecDeque<Descriptor>,
        next_ring: u16,
        destroyed: Vec<RingId>,
    }

    impl Rings<DeterministicBackend> for ModelRings {
        fn create(
            &mut self,
            _: ath11k_hal::RingKind,
            _: ath11k_hal::RingMemory<DeterministicBackend>,
        ) -> Result<RingId, ath11k_hal::HalError> {
            let id = RingId(self.next_ring);
            self.next_ring += 1;
            Ok(id)
        }

        fn publish(
            &mut self,
            ring: RingId,
            descriptor: Descriptor,
        ) -> Result<(), ath11k_hal::HalError> {
            self.published.push((ring, descriptor));
            Ok(())
        }

        fn consume(&mut self, _: RingId) -> Result<Option<Descriptor>, ath11k_hal::HalError> {
            Ok(self.completions.pop_front())
        }
    }

    impl DpRingOps<DeterministicBackend> for ModelRings {
        fn create_dp_ring(
            &mut self,
            _: crate::DpRingSpec,
            memory: ath11k_hal::RingMemory<DeterministicBackend>,
        ) -> Result<RingId, ath11k_hal::HalError> {
            self.create(ath11k_hal::RingKind::Tcl, memory)
        }

        fn destroy(&mut self, ring: RingId) -> Result<(), ath11k_hal::HalError> {
            self.destroyed.push(ring);
            Ok(())
        }
    }

    #[derive(Default)]
    struct AggregateBackend {
        next_dma: u64,
    }

    impl Backend for AggregateBackend {
        type Region = u8;
        type Dma = u64;
        type Interrupt = u32;

        fn generation(&self) -> u64 {
            0
        }
        fn open_region(&mut self, index: u8) -> Result<Self::Region, HardwareError> {
            Ok(index)
        }
        fn region_len(&self, _: &Self::Region) -> usize {
            0x0200_0000
        }
        fn read_u32(&mut self, _: &Self::Region, _: usize) -> Result<u32, HardwareError> {
            Ok(0)
        }
        fn write_u32(&mut self, _: &Self::Region, _: usize, _: u32) -> Result<(), HardwareError> {
            Ok(())
        }
        fn write_dma_address(
            &mut self,
            _: &Self::Region,
            _: usize,
            _: Option<usize>,
            _: &Self::Dma,
            _: usize,
        ) -> Result<(), HardwareError> {
            Ok(())
        }
        fn dma_device_address(&self, dma: &Self::Dma, offset: usize) -> Result<u64, HardwareError> {
            dma.checked_add(offset as u64).ok_or(HardwareError::Limit)
        }
        fn alloc_dma(
            &mut self,
            size: usize,
            align: usize,
            _: DmaDirection,
            _: bool,
        ) -> Result<Self::Dma, HardwareError> {
            let mask = align.checked_sub(1).ok_or(HardwareError::Invalid)? as u64;
            self.next_dma = self
                .next_dma
                .checked_add(mask)
                .ok_or(HardwareError::Limit)?
                & !mask;
            let address = self.next_dma;
            self.next_dma = self
                .next_dma
                .checked_add(size as u64)
                .ok_or(HardwareError::Limit)?;
            Ok(address)
        }
        fn alloc_dma_constrained(
            &mut self,
            size: usize,
            constraints: DmaConstraints,
            direction: DmaDirection,
            coherent: bool,
        ) -> Result<Self::Dma, HardwareError> {
            if constraints.max_segments == 0 || constraints.max_segment_size < size {
                return Err(HardwareError::Limit);
            }
            self.alloc_dma(size, constraints.alignment, direction, coherent)
        }
        fn dma_read(
            &mut self,
            _: &Self::Dma,
            _: Range<usize>,
            out: &mut [u8],
        ) -> Result<(), HardwareError> {
            out.fill(0);
            Ok(())
        }
        fn dma_write(
            &mut self,
            _: &Self::Dma,
            _: Range<usize>,
            _: &[u8],
        ) -> Result<(), HardwareError> {
            Ok(())
        }
        fn sync_for_cpu(&mut self, _: &Self::Dma, _: Range<usize>) -> Result<(), HardwareError> {
            Ok(())
        }
        fn sync_for_device(&mut self, _: &Self::Dma, _: Range<usize>) -> Result<(), HardwareError> {
            Ok(())
        }
        fn open_interrupt(&mut self, vector: u32) -> Result<Self::Interrupt, HardwareError> {
            Ok(vector)
        }
        fn wait_interrupt(
            &mut self,
            _: &Self::Interrupt,
            _: u64,
        ) -> Result<Option<IrqEvent>, HardwareError> {
            Ok(None)
        }
        fn wait_any(
            &mut self,
            _: &[&Self::Interrupt],
            _: u64,
        ) -> Result<Vec<IrqEvent>, HardwareError> {
            Ok(Vec::new())
        }
        fn reset(&mut self) -> Result<u64, HardwareError> {
            Ok(0)
        }
        fn release_region(&mut self, _: Self::Region) {}
        fn release_dma(&mut self, _: Self::Dma) {}
        fn release_interrupt(&mut self, _: Self::Interrupt) {}
    }

    #[derive(Default)]
    struct AggregateRings {
        next: u16,
        destroyed: Vec<RingId>,
        fail_create: Option<u16>,
        fail_destroy_once: Option<RingId>,
    }

    impl Rings<AggregateBackend> for AggregateRings {
        fn create(
            &mut self,
            _: ath11k_hal::RingKind,
            _: ath11k_hal::RingMemory<AggregateBackend>,
        ) -> Result<RingId, ath11k_hal::HalError> {
            Err(ath11k_hal::HalError::Unsupported)
        }
        fn publish(&mut self, _: RingId, _: Descriptor) -> Result<(), ath11k_hal::HalError> {
            Ok(())
        }
        fn consume(&mut self, _: RingId) -> Result<Option<Descriptor>, ath11k_hal::HalError> {
            Ok(None)
        }
    }

    impl DpRingOps<AggregateBackend> for AggregateRings {
        fn create_dp_ring(
            &mut self,
            spec: crate::DpRingSpec,
            memory: ath11k_hal::RingMemory<AggregateBackend>,
        ) -> Result<RingId, ath11k_hal::HalError> {
            if self.fail_create == Some(self.next) {
                return Err(ath11k_hal::HalError::NoResources);
            }
            assert_eq!(
                memory.entry_bytes as usize,
                ath11k_hal::Wcn6750Registers::entry_size(spec.ring_type)
            );
            let id = RingId(self.next);
            self.next += 1;
            Ok(id)
        }
        fn destroy(&mut self, ring: RingId) -> Result<(), ath11k_hal::HalError> {
            if self.fail_destroy_once == Some(ring) {
                self.fail_destroy_once = None;
                return Err(ath11k_hal::HalError::DeviceFault);
            }
            self.destroyed.push(ring);
            Ok(())
        }
    }

    fn config() -> ClientTxConfig {
        ClientTxConfig {
            return_buffer_manager: 3,
            pool_id: 1,
            mac_id: 0,
            lmac_id: 0,
            metadata: 0x405,
            encapsulation: EncapType::NativeWifi,
            address_search_enable: 1,
            search_type: 1,
            ast_index: 0x1234,
            ast_hash: 7,
            tid: 5,
            checksum_offload: true,
        }
    }

    #[test]
    fn aggregate_allocates_sets_up_and_tears_down_every_phase() {
        let device = Device::from_backend(AggregateBackend::default());
        let mmio = device.open_region(0).unwrap();
        let mut dp =
            match ClientDataPath::ath11k_dp_alloc(device, AggregateRings::default(), config()) {
                Ok(dp) => dp,
                Err(_) => panic!("aggregate allocation failed"),
            };
        assert_eq!(dp.ring_resources().common().len(), 15);
        dp.ath11k_dp_pdev_pre_alloc().unwrap();
        dp.ath11k_dp_pdev_reo_setup(&mmio).unwrap();
        assert_eq!(dp.ring_resources().reo_destination().len(), 4);
        dp.ath11k_dp_pdev_alloc().unwrap();
        assert_eq!(dp.ring_resources().pdev_rx().len(), 4);
        assert_eq!(dp.rx_buffers.len(), 4_095);
        assert_eq!(dp.monitor_status_buffers.len(), 1_023);

        dp.ath11k_dp_pdev_free().unwrap();
        dp.ath11k_dp_pdev_reo_cleanup().unwrap();
        dp.ath11k_dp_free().unwrap();
        let (_, rings) = dp.into_parts().unwrap();
        assert_eq!(
            rings.destroyed,
            (19_u16..23)
                .chain(15..19)
                .chain(0..15)
                .map(RingId)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn aggregate_allocation_error_retains_owners_for_cleanup_retry() {
        let device = Device::from_backend(AggregateBackend::default());
        let rings = AggregateRings {
            fail_create: Some(2),
            fail_destroy_once: Some(RingId(1)),
            ..Default::default()
        };
        let error = match ClientDataPath::ath11k_dp_alloc(device, rings, config()) {
            Ok(_) => panic!("expected allocation failure"),
            Err(error) => error,
        };
        assert_eq!(error.cause(), DpError::NoResources);
        assert_eq!(error.cleanup_error(), Some(DpError::DeviceFault));
        let (_, rings) = match error.into_parts() {
            Ok(parts) => parts,
            Err(_) => panic!("cleanup retry failed"),
        };
        assert_eq!(rings.destroyed, [RingId(0), RingId(1)]);
    }

    #[test]
    fn pdev_allocation_requires_completed_reo_setup() {
        let device = DeterministicBackend::device();
        let mut dp =
            ClientDataPath::without_allocated_rings(device, ModelRings::default(), config());
        assert_eq!(dp.ath11k_dp_pdev_alloc(), Err(DpError::WrongState));
        assert!(dp.ring_resources().pdev_rx().is_empty());
    }

    #[test]
    fn client_tx_syncs_then_publishes_exact_tcl_command() {
        let (device, operations) = DeterministicBackend::recording_device();
        let mut dp =
            ClientDataPath::without_allocated_rings(device, ModelRings::default(), config());
        dp.configure(DataRings {
            tcl: RingId(1),
            reo: RingId(2),
            wbm: RingId(3),
        })
        .unwrap();
        let mut frame = vec![0; 30];
        frame[0..2].copy_from_slice(&0x0088_u16.to_le_bytes());
        frame[24..26].copy_from_slice(&[5, 0]);
        dp.transmit(TxPacket {
            peer: crate::PeerId(4),
            bytes: frame,
        })
        .unwrap();

        assert!(
            matches!(operations.borrow().as_slice(), [Operation::SyncForDevice { range, .. }] if range == &(0..28))
        );
        let (_, bytes) = &dp.rings().published[0];
        let command = TclDataCommand::from_bytes(bytes.bytes()).unwrap();
        assert_eq!(command.data_length(), 28);
        assert_eq!(command.encapsulation_type(), EncapType::NativeWifi as u8);
        assert_eq!(command.buffer_address().software_cookie(), 1 << 19);
        assert_eq!(command.tid(), 5);
        assert!(command.ipv4_checksum() && command.tcp_ipv6_checksum());
    }

    #[test]
    fn firmware_wbm_completion_releases_matching_dma_mapping() {
        let device = DeterministicBackend::device();
        let mut dp =
            ClientDataPath::without_allocated_rings(device, ModelRings::default(), config());
        dp.configure(DataRings {
            tcl: RingId(1),
            reo: RingId(2),
            wbm: RingId(3),
        })
        .unwrap();
        let mut frame = vec![0; 24];
        frame[0..2].copy_from_slice(&0x0008_u16.to_le_bytes());
        dp.transmit(TxPacket {
            peer: crate::PeerId(4),
            bytes: frame,
        })
        .unwrap();

        let mut release = WbmReleaseRing::new();
        let mut address = RxdmaBufferRing::new();
        address.set_software_cookie(1 << 19).unwrap();
        release.set_buffer_address(&address);
        release.set_release_source(3).unwrap();
        // HTT status=OK at bits 12:9 in the overlay.
        let mut raw = *release.as_bytes();
        raw[8..12].copy_from_slice(&3_u32.to_le_bytes());
        dp.rings_mut()
            .completions
            .push_back(Descriptor::new(raw.to_vec(), 32).unwrap());
        assert!(dp.service_tx_completions().unwrap()[0].acknowledged);
        assert!(dp.pending.is_empty());
    }

    #[test]
    fn rx_fixture_validates_then_yields_only_payload() {
        let mut bytes = vec![0; 2048];
        // qcn9074 msdu_end.info4: first, last, and two bytes L3 pad.
        bytes[46..48].copy_from_slice(&((1_u16 << 12) | (1 << 13) | (2 << 10)).to_le_bytes());
        // attention.info2: MSDU done and decrypt status OK.
        bytes[84..88].copy_from_slice(&(1_u32 << 31).to_le_bytes());
        bytes[96..100].copy_from_slice(&4_u32.to_le_bytes());
        bytes[182..184].copy_from_slice(&9_u16.to_le_bytes());
        bytes[390..394].copy_from_slice(&[1, 2, 3, 4]);
        let received = parse_received_buffer(&bytes).unwrap();
        assert_eq!(
            received.packet,
            RxPacket {
                peer: Some(crate::PeerId(9)),
                bytes: vec![1, 2, 3, 4]
            }
        );

        bytes[84..88].fill(0);
        assert_eq!(
            parse_received_buffer(&bytes),
            Err(DpError::MalformedDescriptor)
        );
    }

    #[test]
    fn multi_buffer_msdu_is_coalesced_at_descriptor_boundaries() {
        let mut first = vec![0; 2048];
        first[96..100].copy_from_slice(&1700_u32.to_le_bytes());
        first[390..].fill(0xaa);
        let mut last = vec![0; 2048];
        last[46..48].copy_from_slice(&((1_u16 << 12) | (1 << 13) | (2 << 10)).to_le_bytes());
        last[84..88].copy_from_slice(&(1_u32 << 31).to_le_bytes());
        last[388..430].fill(0xbb);
        let fragments = [
            RxFragment {
                bytes: first,
                continuation: true,
                peer: crate::PeerId(12),
                sequence_number: 33,
                tid: 5,
            },
            RxFragment {
                bytes: last,
                continuation: false,
                peer: crate::PeerId(12),
                sequence_number: 33,
                tid: 5,
            },
        ];
        let received = parse_received_chain(&fragments).unwrap();
        assert_eq!(received.packet.bytes.len(), 1700);
        assert!(
            received.packet.bytes[..1658]
                .iter()
                .all(|byte| *byte == 0xaa)
        );
        assert!(
            received.packet.bytes[1658..]
                .iter()
                .all(|byte| *byte == 0xbb)
        );
        assert_eq!(received.packet.peer, Some(crate::PeerId(12)));
        assert_eq!(
            (received.status.sequence_number, received.status.tid),
            (33, 5)
        );
    }

    #[test]
    fn model_reo_completion_syncs_before_rx_descriptor_parse() {
        let (device, operations) = DeterministicBackend::recording_device();
        let bar = device.open_region(0).unwrap();
        let mut image = vec![0; 2048];
        image[46..48].copy_from_slice(&((1_u16 << 12) | (1 << 13) | (2 << 10)).to_le_bytes());
        image[84..88].copy_from_slice(&(1_u32 << 31).to_le_bytes());
        image[96..100].copy_from_slice(&4_u32.to_le_bytes());
        image[182..184].copy_from_slice(&9_u16.to_le_bytes());
        image[390..394].copy_from_slice(&[1, 2, 3, 4]);
        let source = TxBuffer::map(&device, &image).unwrap();

        let mut dp =
            ClientDataPath::without_allocated_rings(device, ModelRings::default(), config());
        dp.configure(DataRings {
            tcl: RingId(1),
            reo: RingId(2),
            wbm: RingId(3),
        })
        .unwrap();
        dp.ath11k_dp_rxbufs_replenish(
            RxdmaConfig {
                ring: RingId(4),
                pdev_id: 0,
                return_buffer_manager: 3,
                buffer_size: 2_048,
            },
            1,
        )
        .unwrap();

        bar.write_device_address(0x80, Some(0x84), source.device_address().unwrap())
            .unwrap();
        bar.write_u32(0x90, 2048).unwrap();
        bar.write_u32(0x98, 1).unwrap();
        bar.write_device_address(
            0x88,
            Some(0x8c),
            dp.rx_buffers[0].buffer.device_address().unwrap(),
        )
        .unwrap();
        bar.write_u32(0x98, 1 | 2).unwrap();

        let refill = RxdmaBufferRing::from_bytes(dp.rings().published[0].1.bytes()).unwrap();
        let mut reo = ReoDestinationRing::new();
        reo.set_buffer_address(&refill);
        reo.set_push_reason(1).unwrap();
        dp.rings_mut().completions.push_back(reo.into_descriptor());
        let received = dp.receive_with_status().unwrap().unwrap();
        assert_eq!(received.packet.bytes, [1, 2, 3, 4]);
        assert!(matches!(
            operations.borrow().last(),
            Some(Operation::SyncForCpu { range, .. }) if range == &(0..2048)
        ));
    }
}
