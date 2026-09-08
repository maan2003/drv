//! Client (STA) TCL transmit and WBM completion path.

use alloc::vec::Vec;
use ath11k_hal::descriptors::{
    ReoDestinationRing, RxdmaBufferRing, TclDataCommand, TxCommandInfo, WbmReleaseRing,
};
use ath11k_hal::{RingId, Rings};
use ath11k_platform_backend::{Backend, Device};

use crate::dma::{RxBuffer, TxBuffer};
use crate::htt::TxCompletion;
use crate::rx::{RxDescriptorStatus, WCN6750_RX_DESCRIPTOR_BYTES, Wcn6750RxDescriptor};
use crate::{DataPath, DataRings, DpError, RxPacket, TxPacket};

const MAX_MSDU_ID: u32 = (1 << 17) - 1;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RxdmaConfig {
    pub ring: RingId,
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
}

impl<B: Backend, R: Rings<B>> ClientDataPath<B, R> {
    /// Source-shaped allocation seam for `ath11k_dp_alloc`.
    pub fn ath11k_dp_alloc(device: Device<B>, rings: R, tx: ClientTxConfig) -> Self {
        Self {
            device,
            rings,
            data_rings: None,
            tx,
            next_msdu_id: 0,
            pending: Vec::new(),
            rxdma: None,
            next_rx_cookie: 0,
            rx_buffers: Vec::new(),
        }
    }

    /// Source-shaped teardown seam. Dropping pending entries performs the C
    /// `dma_unmap_single(..., DMA_TO_DEVICE)` cleanup boundary.
    pub fn ath11k_dp_free(self) -> (Device<B>, R) {
        (self.device, self.rings)
    }

    pub fn rings(&self) -> &R {
        &self.rings
    }

    pub fn rings_mut(&mut self) -> &mut R {
        &mut self.rings
    }

    /// `ath11k_dp_pdev_pre_alloc`; ID/cookie pools are initialized by alloc.
    pub fn ath11k_dp_pdev_pre_alloc(&mut self) {}

    pub fn ath11k_dp_pdev_alloc(
        &mut self,
        config: RxdmaConfig,
        rx_buffer_count: usize,
    ) -> Result<(), DpError> {
        self.ath11k_dp_rxbufs_replenish(config, rx_buffer_count)
    }

    pub fn ath11k_dp_pdev_free(&mut self) {
        self.rx_buffers.clear();
        self.rxdma = None;
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
            let msdu_id = (cookie >> 2) & MAX_MSDU_ID;
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
        let descriptor = match self.rings.consume(reo_ring).map_err(map_hal)? {
            Some(descriptor) => descriptor,
            None => return Ok(None),
        };
        let destination = ReoDestinationRing::from_bytes(descriptor.bytes())
            .map_err(|_| DpError::MalformedDescriptor)?;
        if destination.buffer_type() != 0 {
            return Err(DpError::UnsupportedDescriptor);
        }
        let cookie = destination.buffer_address().software_cookie();
        let position = self
            .rx_buffers
            .iter()
            .position(|entry| entry.cookie == cookie)
            .ok_or(DpError::MalformedDescriptor)?;
        let mut entry = self.rx_buffers.swap_remove(position);
        let bytes = entry.buffer.sync_and_read(entry.buffer.len())?;
        let result = parse_received_buffer(&bytes);
        // The C NAPI path replenishes every buffer reaped from the ring,
        // including buffers whose descriptors fail later validation.
        self.replenish_one()?;
        result.map(Some)
    }

    fn replenish_one(&mut self) -> Result<(), DpError> {
        let config = self.rxdma.ok_or(DpError::NoResources)?;
        let cookie = self.next_rx_cookie & 0x1f_ffff;
        self.next_rx_cookie = self.next_rx_cookie.wrapping_add(1);
        let buffer = RxBuffer::replenish(&self.device, config.buffer_size)?;
        let descriptor = RxdmaBufferRing::for_buffer(
            &buffer.device_address()?,
            cookie,
            config.return_buffer_manager,
        );
        self.rings
            .publish(config.ring, descriptor.into_descriptor())
            .map_err(map_hal)?;
        self.rx_buffers.push(PendingRx { cookie, buffer });
        Ok(())
    }

    fn allocate_msdu_id(&mut self) -> Result<u32, DpError> {
        for _ in 0..=MAX_MSDU_ID {
            let candidate = self.next_msdu_id;
            self.next_msdu_id = (self.next_msdu_id + 1) & MAX_MSDU_ID;
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

impl<B: Backend, R: Rings<B>> DataPath for ClientDataPath<B, R> {
    fn configure(&mut self, rings: DataRings) -> Result<(), DpError> {
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

fn parse_received_buffer(bytes: &[u8]) -> Result<ReceivedFrame, DpError> {
    let descriptor = Wcn6750RxDescriptor::parse(bytes)?;
    let status = descriptor.status();
    if status.msdu_length_error || !status.msdu_done {
        return Err(DpError::MalformedDescriptor);
    }
    let start = WCN6750_RX_DESCRIPTOR_BYTES + usize::from(status.l3_padding);
    let end = start
        .checked_add(usize::from(status.msdu_length))
        .ok_or(DpError::MalformedDescriptor)?;
    let payload = bytes
        .get(start..end)
        .ok_or(DpError::MalformedDescriptor)?
        .to_vec();
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec;
    use ath11k_hal::Descriptor;
    use drv_hardware_backends::{DeterministicBackend, Operation};

    #[derive(Default)]
    struct ModelRings {
        published: Vec<(RingId, Descriptor)>,
        completions: VecDeque<Descriptor>,
    }

    impl Rings<DeterministicBackend> for ModelRings {
        fn create(
            &mut self,
            _: ath11k_hal::RingKind,
            _: ath11k_hal::RingMemory<DeterministicBackend>,
        ) -> Result<RingId, ath11k_hal::HalError> {
            Ok(RingId(0))
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
    fn client_tx_syncs_then_publishes_exact_tcl_command() {
        let (device, operations) = DeterministicBackend::recording_device();
        let mut dp = ClientDataPath::ath11k_dp_alloc(device, ModelRings::default(), config());
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
        let mut dp = ClientDataPath::ath11k_dp_alloc(device, ModelRings::default(), config());
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
}
