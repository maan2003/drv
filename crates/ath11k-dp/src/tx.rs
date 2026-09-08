// PORT-MAP: reusable
//! Client (STA) TCL transmit and WBM completion path.

use alloc::vec::Vec;
use ath11k_hal::descriptors::{
    ReoDestinationRing, RxdmaBufferRing, TclDataCommand, TxCommandInfo, WbmReleaseRing,
};
use ath11k_hal::{RingId, Rings};
use ath11k_platform_backend::{Backend, Device, FromDevice};
use dma_pool::DmaPool;

use crate::dma::{RxBuffer, TxBuffer};
use crate::htt::TxCompletion;
use crate::lifecycle::{DpAllocationError, DpRingOps, Wcn6750DpRings};
use crate::reo::ReoController;
use crate::rx::{RxDescriptorStatus, WCN6750_RX_DESCRIPTOR_BYTES, Wcn6750RxDescriptor};
use crate::{DataPath, DataRings, DpError, RxPacket, TxPacket};

// idr_alloc(..., 0, DP_TX_IDR_SIZE - 1) uses an exclusive upper bound.
const MAX_MSDU_ID: u32 = 32_766;
const RX_BUFFER_SIZE: usize = 2_048;
const RX_POOL_PAGE_SIZE: usize = 4_096;
const RX_BUFFER_ALIGNMENT: usize = 128;
const RX_POOL_HIGH_WATERMARK: usize = 64;
const RX_BUFFER_ID_MASK: u32 = 0x3_ffff;

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

impl ClientTxConfig {
    /// WCN6750 station-vdev defaults from `ath11k_dp_vdev_tx_attach` and its
    /// non-v2 peer-map search path. Queue/TID selection can be refined per
    /// packet without making core duplicate hardware constants.
    pub const fn wcn6750_station(vdev_id: u8) -> Self {
        Self {
            // TCL ring 0 maps to HAL_RX_BUF_RBM_SW0_BM.
            return_buffer_manager: 3,
            pool_id: 0,
            mac_id: 0,
            lmac_id: 0,
            metadata: 1 | ((vdev_id as u16) << 2),
            encapsulation: EncapType::NativeWifi,
            // WCN6750 has htt_peer_map_v2=false: use ADDRY/default search.
            address_search_enable: 2,
            search_type: 0,
            ast_index: 0,
            ast_hash: 0,
            tid: 0,
            // Linux enables this only for CHECKSUM_PARTIAL packets.
            checksum_offload: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxResult {
    pub msdu_id: u32,
    pub status: u8,
    pub acknowledged: bool,
    pub ack_rssi: i8,
    pub peer: Option<crate::PeerId>,
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxCompletionDisposition {
    Retain,
    Free,
    Complete(TxCompletion),
}

fn completion_disposition(release: &WbmReleaseRing) -> Result<TxCompletionDisposition, DpError> {
    match release.release_source() {
        0 => {
            return Ok(TxCompletionDisposition::Complete(TxCompletion {
                status: release.tqm_release_reason(),
                reinject_reason: 0,
                ack_rssi: release.ack_rssi() as i8,
                peer: Some(crate::PeerId(release.peer_id())),
            }));
        }
        3 => {}
        _ => return Ok(TxCompletionDisposition::Retain),
    }
    let completion = TxCompletion::decode_wbm_release(release.as_bytes())?;
    Ok(match completion.status {
        0..=2 => TxCompletionDisposition::Complete(completion),
        3 | 4 => TxCompletionDisposition::Free,
        5 => TxCompletionDisposition::Retain,
        // Unlike Linux, complete a matching owner as failed rather than
        // stranding its DMA mapping on an unrecognized firmware status.
        _ => TxCompletionDisposition::Complete(completion),
    })
}

/// Pure completion-decision seam used by the generated C differential.
#[doc(hidden)]
pub fn tx_completion_disposition(bytes: &[u8]) -> Result<TxCompletionDisposition, DpError> {
    let release = WbmReleaseRing::from_bytes(bytes).map_err(|_| DpError::MalformedDescriptor)?;
    completion_disposition(&release)
}

/// Host-originated transmit attributes which must survive the chip boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostTxFlags {
    pub protected: bool,
    pub favor_reliability: bool,
    pub qos: bool,
}

/// RX decapsulation selected by firmware for the completed MSDU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RxDecapType {
    Raw = 0,
    NativeWifi = 1,
    Ethernet2Dix = 2,
    Ieee8023 = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RxDecryptStatus {
    NotDecrypted,
    Decrypted,
}

/// Descriptor facts needed by the host adapter to construct its RX metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostRxInfo {
    pub decap_type: RxDecapType,
    pub peer: Option<crate::PeerId>,
    pub tid: u8,
    pub decrypt_status: RxDecryptStatus,
    /// Packed RX PHY metadata: channel in low 16 bits and 6 GHz center
    /// frequency in high 16 bits, matching the pinned descriptor contract.
    pub phy_metadata: u32,
    pub bandwidth: u8,
    pub mcs: u8,
    pub packet_type: u8,
    pub nss: u8,
    pub phy_ppdu_id: u16,
}

/// A complete, reassembled raw/native-802.11 MSDU ready for host delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostRxFrame {
    pub bytes: Vec<u8>,
    pub info: HostRxInfo,
}

/// Infallible because the RX buffers have already been returned to hardware;
/// host backpressure cannot safely cause this callback to be retried.
pub trait DpHost {
    fn receive(&mut self, frame: HostRxFrame);
    fn tx_complete(&mut self, result: TxResult);
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostRxDropCounters {
    pub malformed: usize,
    pub fcs_error: usize,
    pub decrypt_error: usize,
    pub tkip_mic_error: usize,
    pub unsupported_decap: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostServiceResult {
    pub tx_delivered: usize,
    pub tx_malformed: usize,
    pub rx_delivered: usize,
    pub rx_dropped: HostRxDropCounters,
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
    header_status: Vec<u8>,
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
    rx_pool: DmaPool<B, FromDevice>,
    rx_buffers: Vec<PendingRx<B>>,
    monitor_status_buffers: Vec<PendingRx<B>>,
    rx_chain: Vec<RxFragment>,
    next_monitor_cookie: u32,
    ring_resources: Wcn6750DpRings,
    reo: Option<ReoController>,
    htt_setup_index: usize,
    host_rx_first: bool,
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
        let rx_pool = match make_rx_pool(device.clone()) {
            Ok(pool) => pool,
            Err(error) => {
                return Err(DpAllocationError::new(error, device, rings, ring_resources));
            }
        };
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
            rx_pool,
            rx_buffers: Vec::new(),
            monitor_status_buffers: Vec::new(),
            rx_chain: Vec::new(),
            next_monitor_cookie: 1,
            ring_resources,
            reo: None,
            htt_setup_index: 0,
            host_rx_first: true,
        })
    }

    #[cfg(test)]
    fn without_allocated_rings(device: Device<B>, rings: R, tx: ClientTxConfig) -> Self {
        let rx_pool = make_rx_pool(device.clone()).expect("static RX pool configuration is valid");
        Self {
            device,
            rings,
            data_rings: None,
            tx,
            next_msdu_id: 0,
            pending: Vec::new(),
            rxdma: None,
            next_rx_cookie: 1,
            rx_pool,
            rx_buffers: Vec::new(),
            monitor_status_buffers: Vec::new(),
            rx_chain: Vec::new(),
            next_monitor_cookie: 1,
            ring_resources: Wcn6750DpRings::default(),
            reo: None,
            htt_setup_index: 0,
            host_rx_first: true,
        }
    }

    /// Tear down the SoC-level rings after all pdev phases have been freed.
    /// The caller must quiesce firmware and interrupt dispatch before starting
    /// the DP free sequence. Pinned `core.c:ath11k_core_deinit` does so through
    /// `ath11k_core_stop` before `ath11k_core_soc_destroy` calls
    /// `dp.c:ath11k_dp_free`. Dropping pending entries also performs the C TX
    /// DMA cleanup boundary.
    pub fn ath11k_dp_free(&mut self) -> Result<(), DpError> {
        if !self.ring_resources.pdev_rx().is_empty()
            || !self.ring_resources.reo_destination().is_empty()
            || self.reo.is_some()
            || self.htt_setup_index != 0
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
            || self.htt_setup_index != 0
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
    pub fn ath11k_dp_pdev_reo_setup(&mut self) -> Result<(), DpError> {
        if self.reo.is_some() {
            return Err(DpError::WrongState);
        }
        let data_rings = self
            .ring_resources
            .allocate_reo_destination(&self.device, &mut self.rings)?;
        let (command, status) = self.ring_resources.reo_controller_rings()?;
        let reo = match self.rings.setup_reo_controller(command, status) {
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
            buffer_size: RX_BUFFER_SIZE,
        };
        let allocation = self
            .ath11k_dp_rxbufs_replenish(config, 4_095)
            .and_then(|()| {
                replenish_pool(
                    &self.rx_pool,
                    &mut self.rings,
                    monitor_ring,
                    0,
                    4,
                    RX_BUFFER_SIZE,
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

    /// Begin the destructive DP teardown sequence. The caller must first
    /// quiesce firmware and interrupt dispatch, and preserve that precondition
    /// through `pdev_free -> pdev_reo_cleanup -> dp_free`. Pinned
    /// `core.c:ath11k_core_deinit` calls `ath11k_core_stop` before the final
    /// `ath11k_core_soc_destroy`/`dp.c:ath11k_dp_free` path.
    pub fn ath11k_dp_pdev_free(&mut self) -> Result<(), DpError> {
        self.ring_resources.free_pdev_rx(&mut self.rings)?;
        self.rx_buffers.clear();
        self.monitor_status_buffers.clear();
        self.rx_chain.clear();
        self.rxdma = None;
        self.htt_setup_index = 0;
        Ok(())
    }

    /// Send the four WCN6750 LMAC RX ring configurations in the order used by
    /// `ath11k_dp_rx_pdev_alloc`. A failed send leaves the completed prefix so
    /// retry resumes without re-sending firmware-visible DMA addresses.
    pub fn configure_htt<C: crate::HttControl>(&mut self, control: &mut C) -> Result<(), DpError> {
        if !C::SEND_ERROR_IS_NON_VISIBLE
            || self.reo.is_none()
            || self.data_rings.is_none()
            || self.ring_resources.pdev_rx().len() != 4
            || self.rx_buffers.is_empty()
            || self.monitor_status_buffers.is_empty()
            || self.htt_setup_index == 4
        {
            return Err(DpError::WrongState);
        }
        while self.htt_setup_index < 4 {
            if !self
                .rings
                .send_htt_ring_setup(self.htt_setup_index, control)?
            {
                return Err(DpError::NoResources);
            }
            self.htt_setup_index += 1;
        }
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

    /// Host-facing form of the RX process/deliver path. Only complete
    /// raw/native-802.11 frames cross this seam; hardware-reported failures
    /// and Ethernet decapsulation are consumed and counted as polling work.
    pub fn service_host<H: DpHost>(
        &mut self,
        work_budget: usize,
        receive_budget: usize,
        host: &mut H,
    ) -> Result<HostServiceResult, DpError> {
        let mut remaining_work = work_budget;
        let mut rx_delivered = 0;
        let mut rx_dropped = HostRxDropCounters::default();
        // Alternate the reserved one-credit probe so neither nonempty ring
        // can starve when the caller supplies a one-descriptor budget.
        let probe_rx_first = receive_budget != 0 && self.host_rx_first;
        if receive_budget != 0 && work_budget != 0 {
            self.host_rx_first = !self.host_rx_first;
        }
        if probe_rx_first && remaining_work != 0 {
            let mut rx_credit = 1;
            self.service_host_rx_once(&mut rx_credit, host, &mut rx_delivered, &mut rx_dropped)?;
            remaining_work -= 1 - rx_credit;
        }
        let tx_budget = remaining_work;
        let (tx_work, tx_delivered, tx_malformed) =
            self.service_host_tx_completions(tx_budget, host)?;
        remaining_work -= tx_work;
        while remaining_work != 0 && rx_delivered < receive_budget {
            if !self.service_host_rx_once(
                &mut remaining_work,
                host,
                &mut rx_delivered,
                &mut rx_dropped,
            )? {
                break;
            }
        }
        Ok(HostServiceResult {
            tx_delivered,
            tx_malformed,
            rx_delivered,
            rx_dropped,
        })
    }

    fn service_host_rx_once<H: DpHost>(
        &mut self,
        remaining_work: &mut usize,
        host: &mut H,
        delivered: &mut usize,
        dropped: &mut HostRxDropCounters,
    ) -> Result<bool, DpError> {
        let received = match self.receive_with_status_bounded(remaining_work) {
            Ok(Some(received)) => received,
            Ok(None) => return Ok(false),
            Err(DpError::MalformedDescriptor | DpError::InvalidFrame) => {
                self.rx_chain.clear();
                dropped.malformed += 1;
                return Ok(true);
            }
            Err(error) => return Err(error),
        };
        match host_frame(received) {
            Ok(frame) => {
                host.receive(frame);
                *delivered += 1;
            }
            Err(HostRxDropReason::Malformed) => dropped.malformed += 1,
            Err(HostRxDropReason::Fcs) => dropped.fcs_error += 1,
            Err(HostRxDropReason::Decrypt) => dropped.decrypt_error += 1,
            Err(HostRxDropReason::TkipMic) => dropped.tkip_mic_error += 1,
            Err(HostRxDropReason::UnsupportedDecap) => dropped.unsupported_decap += 1,
        }
        Ok(true)
    }

    fn service_host_tx_completions<H: DpHost>(
        &mut self,
        budget: usize,
        host: &mut H,
    ) -> Result<(usize, usize, usize), DpError> {
        let ring = self.data_rings.ok_or(DpError::NoResources)?.wbm;
        let mut work = 0;
        let mut delivered = 0;
        let mut malformed = 0;
        while work < budget {
            let Some(descriptor) = self.rings.consume(ring).map_err(map_hal)? else {
                break;
            };
            work += 1;
            let Ok(release) = WbmReleaseRing::from_bytes(descriptor.bytes()) else {
                malformed += 1;
                continue;
            };
            let disposition = match completion_disposition(&release) {
                Ok(disposition) => disposition,
                Err(_) => {
                    malformed += 1;
                    continue;
                }
            };
            if disposition == TxCompletionDisposition::Retain {
                continue;
            }
            let msdu_id = (release.buffer_address().software_cookie() >> 2) & 0x1_ffff;
            let Some(position) = self
                .pending
                .iter()
                .position(|pending| pending.msdu_id == msdu_id)
            else {
                malformed += 1;
                continue;
            };
            drop(self.pending.swap_remove(position));
            let TxCompletionDisposition::Complete(htt) = disposition else {
                continue;
            };
            host.tx_complete(TxResult {
                msdu_id,
                status: htt.status,
                acknowledged: htt.status == 0,
                ack_rssi: htt.ack_rssi,
                peer: htt.peer,
            });
            delivered += 1;
        }
        Ok((work, delivered, malformed))
    }

    /// Submit a host-owned 802.11 frame through the existing TCL DMA path.
    /// The peer remains chip-local; host scheduling attributes are retained
    /// until the matching completion is reported.
    pub fn submit_host_frame(
        &mut self,
        bytes: &[u8],
        peer: crate::PeerId,
        flags: HostTxFlags,
    ) -> Result<(), DpError> {
        if flags.favor_reliability {
            return Err(DpError::UnsupportedTxFlags);
        }
        let fc = u16::from_le_bytes(
            bytes
                .get(..2)
                .ok_or(DpError::InvalidFrame)?
                .try_into()
                .map_err(|_| DpError::InvalidFrame)?,
        );
        let is_qos = fc & 0x000c == 0x0008 && fc & 0x0080 != 0;
        if flags.qos != is_qos || flags.protected != (fc & 0x4000 != 0) {
            return Err(DpError::InvalidFrame);
        }
        self.transmit_client(TxPacket {
            peer,
            bytes: bytes.to_vec(),
        })
    }

    /// Drain WBM release entries as `ath11k_dp_tx_completion_handler` does.
    pub fn service_tx_completions(&mut self) -> Result<Vec<TxResult>, DpError> {
        self.service_tx_completions_bounded(usize::MAX)
            .map(|(results, _)| results)
    }

    fn service_tx_completions_bounded(
        &mut self,
        budget: usize,
    ) -> Result<(Vec<TxResult>, usize), DpError> {
        let ring = self.data_rings.ok_or(DpError::NoResources)?.wbm;
        let mut results = Vec::new();
        let mut work = 0;
        while work < budget {
            let Some(descriptor) = self.rings.consume(ring).map_err(map_hal)? else {
                break;
            };
            work += 1;
            let release = WbmReleaseRing::from_bytes(descriptor.bytes())
                .map_err(|_| DpError::MalformedDescriptor)?;
            let cookie = release.buffer_address().software_cookie();
            let msdu_id = (cookie >> 2) & 0x1_ffff;
            let disposition = completion_disposition(&release)?;
            if disposition == TxCompletionDisposition::Retain {
                continue;
            }
            let position = self
                .pending
                .iter()
                .position(|pending| pending.msdu_id == msdu_id)
                .ok_or(DpError::MalformedDescriptor)?;
            // Removing drops the streaming mapping at the same point as the
            // C completion handler's dma_unmap_single.
            drop(self.pending.swap_remove(position));
            let TxCompletionDisposition::Complete(htt) = disposition else {
                continue;
            };
            results.push(TxResult {
                msdu_id,
                status: htt.status,
                acknowledged: htt.status == 0,
                ack_rssi: htt.ack_rssi,
                peer: htt.peer,
            });
        }
        Ok((results, work))
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
        let mut remaining_work = usize::MAX;
        self.receive_with_status_bounded(&mut remaining_work)
    }

    fn receive_with_status_bounded(
        &mut self,
        remaining_work: &mut usize,
    ) -> Result<Option<ReceivedFrame>, DpError> {
        let reo_ring = self.data_rings.ok_or(DpError::NoResources)?.reo;
        loop {
            if *remaining_work == 0 {
                return Ok(None);
            }
            let descriptor = match self.rings.consume(reo_ring).map_err(map_hal)? {
                Some(descriptor) => descriptor,
                None => return Ok(None),
            };
            *remaining_work -= 1;
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
            entry.buffer.prepare_for_device()?;
            // Return the completed segment before replenishment so the pool
            // can reuse it for the replacement descriptor.
            drop(entry);
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
            &self.rx_pool,
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
        let mut tx = self.tx;
        if tx.encapsulation == EncapType::NativeWifi
            && let Some(tid) = encap_native_wifi(&mut packet.bytes)?
        {
            tx.tid = tid;
        }
        let msdu_id = self.allocate_msdu_id()?;
        let buffer = TxBuffer::map(&self.device, &packet.bytes)?;
        let descriptor = make_tcl_descriptor(&buffer, msdu_id, tx)?;
        self.rings
            .publish(data_rings.tcl, descriptor.into_ring_descriptor())
            .map_err(map_hal)?;
        self.pending.push(PendingTx { msdu_id, buffer });
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn replenish_pool<B: Backend, R: Rings<B>>(
    pool: &DmaPool<B, FromDevice>,
    rings: &mut R,
    ring: RingId,
    pdev_id: u8,
    return_buffer_manager: u8,
    buffer_size: usize,
    count: usize,
    next_cookie: &mut u32,
    buffers: &mut Vec<PendingRx<B>>,
) -> Result<(), DpError> {
    if buffer_size != pool.segment_size() {
        return Err(DpError::NoResources);
    }
    for _ in 0..count {
        let buffer_id = next_unused_rx_buffer_id(next_cookie, |candidate| {
            buffers
                .iter()
                .any(|entry| entry.cookie & RX_BUFFER_ID_MASK == candidate)
        })?;
        let cookie = buffer_id | ((u32::from(pdev_id) & 7) << 18);
        let buffer = RxBuffer::replenish(pool)?;
        let descriptor =
            RxdmaBufferRing::for_buffer(&buffer.device_address()?, cookie, return_buffer_manager);
        rings
            .publish(ring, descriptor.into_descriptor())
            .map_err(map_hal)?;
        buffers.push(PendingRx { cookie, buffer });
    }
    Ok(())
}

/// Source-shaped `idr_alloc(..., 1, max)` replacement: choose an unused live
/// buffer ID rather than blindly reusing the cursor after its 18-bit wrap.
fn next_unused_rx_buffer_id(
    next: &mut u32,
    mut is_used: impl FnMut(u32) -> bool,
) -> Result<u32, DpError> {
    for _ in 0..RX_BUFFER_ID_MASK {
        let candidate = (*next & RX_BUFFER_ID_MASK).max(1);
        *next = if candidate == RX_BUFFER_ID_MASK {
            1
        } else {
            candidate + 1
        };
        if !is_used(candidate) {
            return Ok(candidate);
        }
    }
    Err(DpError::NoResources)
}

fn make_rx_pool<B: Backend>(device: Device<B>) -> Result<DmaPool<B, FromDevice>, DpError> {
    // Pinned `dp_rx.c:ath11k_dp_rxbufs_replenish` uses 2 KiB buffers aligned
    // to DP_RX_BUFFER_ALIGN_SIZE (128). Two segments share each 4 KiB mapping.
    DmaPool::new(
        device,
        RX_BUFFER_SIZE,
        RX_POOL_PAGE_SIZE,
        RX_BUFFER_ALIGNMENT,
        RX_POOL_HIGH_WATERMARK,
    )
    .map_err(|_| DpError::NoResources)
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
    status.fcs_error = end_status.fcs_error;
    status.decrypt_error = end_status.decrypt_error;
    status.tkip_mic_error = end_status.tkip_mic_error;
    status.mpdu_errors = end_status.mpdu_errors;
    status.multicast_broadcast = end_status.multicast_broadcast;
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
        header_status: descriptor.header_status().to_vec(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostRxDropReason {
    Malformed,
    Fcs,
    Decrypt,
    TkipMic,
    UnsupportedDecap,
}

fn host_frame(received: ReceivedFrame) -> Result<HostRxFrame, HostRxDropReason> {
    let status = received.status;
    if status.fcs_error {
        return Err(HostRxDropReason::Fcs);
    }
    if status.tkip_mic_error {
        return Err(HostRxDropReason::TkipMic);
    }
    if status.decrypt_error {
        return Err(HostRxDropReason::Decrypt);
    }
    let decap_type = match status.decap_type {
        0 => RxDecapType::Raw,
        1 => RxDecapType::NativeWifi,
        2 | 3 => return Err(HostRxDropReason::UnsupportedDecap),
        _ => unreachable!("RX decap is a two-bit descriptor field"),
    };
    let decrypted = status.encryption_info_valid
        && status.encryption_type != 7
        && status.mpdu_errors == 0
        && status.decrypted;
    let bytes = match decap_type {
        RxDecapType::Raw => normalize_raw(received.packet.bytes, status, decrypted)?,
        RxDecapType::NativeWifi => normalize_native_wifi(
            received.packet.bytes,
            &received.header_status,
            status,
            decrypted,
        )?,
        RxDecapType::Ethernet2Dix | RxDecapType::Ieee8023 => unreachable!(),
    };
    Ok(HostRxFrame {
        bytes,
        info: HostRxInfo {
            decap_type,
            peer: received.packet.peer.filter(|peer| peer.0 != 0xffff),
            tid: status.tid,
            decrypt_status: if decrypted {
                RxDecryptStatus::Decrypted
            } else {
                RxDecryptStatus::NotDecrypted
            },
            phy_metadata: status.frequency,
            bandwidth: status.bandwidth,
            mcs: status.mcs,
            packet_type: status.packet_type,
            nss: status.nss,
            phy_ppdu_id: status.phy_ppdu_id,
        },
    })
}

fn ieee80211_header_len(frame: &[u8]) -> Result<usize, HostRxDropReason> {
    let fc = u16::from_le_bytes(
        frame
            .get(..2)
            .ok_or(HostRxDropReason::Malformed)?
            .try_into()
            .map_err(|_| HostRxDropReason::Malformed)?,
    );
    let data = fc & 0x000c == 0x0008;
    let qos = data && fc & 0x0080 != 0;
    let mut len = if fc & 0x0300 == 0x0300 { 30 } else { 24 };
    if qos {
        len += 2;
        if fc & 0x8000 != 0 {
            len += 4;
        }
    }
    (frame.len() >= len)
        .then_some(len)
        .ok_or(HostRxDropReason::Malformed)
}

fn address_offsets(frame: &[u8]) -> Result<(usize, usize), HostRxDropReason> {
    let fc = u16::from_le_bytes(
        frame
            .get(..2)
            .ok_or(HostRxDropReason::Malformed)?
            .try_into()
            .map_err(|_| HostRxDropReason::Malformed)?,
    );
    Ok(match fc & 0x0300 {
        0x0000 => (4, 10),
        0x0100 => (16, 10),
        0x0200 => (4, 16),
        0x0300 => (16, 24),
        _ => unreachable!(),
    })
}

fn crypto_lengths(encryption_type: u8) -> (usize, usize, usize) {
    match encryption_type {
        2 | 4 => (8, 0, 4),
        6 => (8, 8, 0),
        8 => (8, 16, 0),
        9 | 10 => (8, 16, 0),
        _ => (0, 0, 0),
    }
}

fn normalize_raw(
    mut bytes: Vec<u8>,
    status: RxDescriptorStatus,
    decrypted: bool,
) -> Result<Vec<u8>, HostRxDropReason> {
    if !status.first_msdu || !status.last_msdu || bytes.len() < 4 {
        return Err(HostRxDropReason::Malformed);
    }
    bytes.truncate(bytes.len() - 4);
    if !decrypted {
        return Ok(bytes);
    }
    let header_len = ieee80211_header_len(&bytes)?;
    let (crypto, mic, icv) = crypto_lengths(status.encryption_type);
    let more_fragments = u16::from_le_bytes([bytes[0], bytes[1]]) & 0x0400 != 0;
    let mmic = usize::from(status.encryption_type == 4 && !more_fragments) * 8;
    if bytes.len() < header_len + crypto + mic + icv + mmic {
        return Err(HostRxDropReason::Malformed);
    }
    let iv_stripped = !status.multicast_broadcast;
    if iv_stripped {
        bytes.drain(header_len..header_len + crypto);
    }
    bytes.truncate(bytes.len() - mic - icv - mmic);
    if iv_stripped {
        bytes[1] &= !0x40; // IEEE80211_FCTL_PROTECTED
    }
    Ok(bytes)
}

fn normalize_native_wifi(
    bytes: Vec<u8>,
    header_status: &[u8],
    status: RxDescriptorStatus,
    decrypted: bool,
) -> Result<Vec<u8>, HostRxDropReason> {
    let native_header_len = ieee80211_header_len(&bytes)?;
    let iv_stripped = decrypted && !status.multicast_broadcast;
    if !status.first_msdu {
        let mut header = bytes[..native_header_len].to_vec();
        let mut fc = u16::from_le_bytes([header[0], header[1]]);
        fc = (fc | 0x0080) & !0x8000;
        if iv_stripped {
            fc &= !0x4000;
        }
        header[..2].copy_from_slice(&fc.to_le_bytes());
        let qos = u16::from(status.tid) | (u16::from(status.mesh_control_present) << 8);
        header.extend_from_slice(&qos.to_le_bytes());
        let crypto = if iv_stripped {
            0
        } else {
            crypto_lengths(status.encryption_type).0
        };
        let crypto_end = native_header_len + crypto;
        header.extend_from_slice(
            bytes
                .get(native_header_len..crypto_end)
                .ok_or(HostRxDropReason::Malformed)?,
        );
        // `skb_push(crypto_len)` copies the parameters in front of the
        // original post-native-header bytes before rebuilding the header.
        header.extend_from_slice(&bytes[native_header_len..]);
        return Ok(header);
    }
    let (native_da, native_sa) = address_offsets(&bytes)?;
    let da: [u8; 6] = bytes
        .get(native_da..native_da + 6)
        .ok_or(HostRxDropReason::Malformed)?
        .try_into()
        .map_err(|_| HostRxDropReason::Malformed)?;
    let sa: [u8; 6] = bytes
        .get(native_sa..native_sa + 6)
        .ok_or(HostRxDropReason::Malformed)?
        .try_into()
        .map_err(|_| HostRxDropReason::Malformed)?;
    let original_len = ieee80211_header_len(header_status)?;
    let mut header = header_status[..original_len].to_vec();
    if status.first_msdu && header[0] & 0x80 != 0 {
        let qos = if header[1] & 0x03 == 0x03 { 30 } else { 24 };
        header[qos] &= !0x80; // IEEE80211_QOS_CTL_A_MSDU_PRESENT
    }
    let (da_offset, sa_offset) = address_offsets(&header)?;
    header[da_offset..da_offset + 6].copy_from_slice(&da);
    header[sa_offset..sa_offset + 6].copy_from_slice(&sa);
    if iv_stripped {
        header[1] &= !0x40;
    } else {
        let (crypto, _, _) = crypto_lengths(status.encryption_type);
        header.extend_from_slice(
            header_status
                .get(original_len..original_len + crypto)
                .ok_or(HostRxDropReason::Malformed)?,
        );
    }
    header.extend_from_slice(&bytes[native_header_len..]);
    Ok(header)
}

/// Pure generated-frame oracle seam; production delivery uses the same
/// normalizer before invoking `DpHost`.
#[doc(hidden)]
pub fn normalize_native_wifi_frame(
    bytes: Vec<u8>,
    header_status: &[u8],
    status: RxDescriptorStatus,
    decrypted: bool,
) -> Result<Vec<u8>, DpError> {
    normalize_native_wifi(bytes, header_status, status, decrypted)
        .map_err(|_| DpError::InvalidFrame)
}

fn make_tcl_descriptor<B: Backend>(
    buffer: &TxBuffer<B>,
    msdu_id: u32,
    config: ClientTxConfig,
) -> Result<TclDataCommand, DpError> {
    Ok(TclDataCommand::for_transmit(
        &buffer.device_address()?,
        client_tx_command_info(msdu_id, buffer.length() as u32, config),
    ))
}

/// Pure per-packet TCL field selection used by the generated C differential.
#[doc(hidden)]
pub fn client_tx_command_info(
    msdu_id: u32,
    data_length: u32,
    config: ClientTxConfig,
) -> TxCommandInfo {
    let cookie = u32::from(config.mac_id) | (msdu_id << 2) | (u32::from(config.pool_id) << 19);
    let checksum_flags = if config.checksum_offload && config.encapsulation != EncapType::Raw {
        (1 << 16) | (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20)
    } else {
        0
    };
    TxCommandInfo {
        metadata_flags: config.metadata,
        descriptor_id: cookie,
        descriptor_type: 0,
        encapsulation_type: config.encapsulation as u8,
        data_length,
        packet_offset: 0,
        // With no host cipher metadata, Linux selects OPEN for raw frames.
        // Other encapsulations ignore this field and retain zero initialization.
        encryption_type: if config.encapsulation == EncapType::Raw {
            7
        } else {
            0
        },
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
    }
}

/// `ath11k_dp_tx_encap_nwifi`: remove the QoS control and clear QoS subtype.
fn encap_native_wifi(frame: &mut Vec<u8>) -> Result<Option<u8>, DpError> {
    let fc_bytes = frame.get(..2).ok_or(DpError::InvalidFrame)?;
    let mut frame_control = u16::from_le_bytes([fc_bytes[0], fc_bytes[1]]);
    let is_data = frame_control & 0x000c == 0x0008;
    if !is_data {
        return Err(DpError::InvalidFrame);
    }
    let is_qos = is_data && frame_control & 0x0080 != 0;
    if !is_qos {
        return Ok(None);
    }
    let has_address4 = frame_control & 0x0300 == 0x0300;
    let qos_offset = if has_address4 { 30 } else { 24 };
    if frame.len() < qos_offset + 2 {
        return Err(DpError::InvalidFrame);
    }
    let tid = frame[qos_offset] & 0x0f;
    frame.drain(qos_offset..qos_offset + 2);
    frame_control &= !0x0080;
    frame[..2].copy_from_slice(&frame_control.to_le_bytes());
    Ok(Some(tid))
}

/// Pure generated-frame oracle seam; production transmit uses the same
/// native-WiFi transform before DMA mapping.
#[doc(hidden)]
pub fn encap_native_wifi_frame(mut frame: Vec<u8>) -> Result<(Vec<u8>, Option<u8>), DpError> {
    let tid = encap_native_wifi(&mut frame)?;
    Ok((frame, tid))
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
    use crate::{HttControl, HttHostMessage};
    use alloc::vec;
    use alloc::{
        collections::{BTreeMap, VecDeque},
        rc::Rc,
    };
    use ath11k_hal::Descriptor;
    use ath11k_platform_backend::{DmaConstraints, DmaDirection, Error as HardwareError, IrqEvent};
    use core::{
        cell::{Cell, RefCell},
        ops::Range,
    };
    use drv_hardware_backends::{DeterministicBackend, Operation};

    mod stateful_tests;

    type DmaWrites = Rc<RefCell<Vec<(u64, Range<usize>)>>>;

    #[derive(Default)]
    struct ModelRings {
        published: Vec<(RingId, Descriptor)>,
        completions: VecDeque<Descriptor>,
        ring_completions: BTreeMap<u16, VecDeque<Descriptor>>,
        completion_ring: Option<RingId>,
        fail_consume_ring: Option<RingId>,
        fail_consume_after: usize,
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

        fn consume(&mut self, ring: RingId) -> Result<Option<Descriptor>, ath11k_hal::HalError> {
            if self.fail_consume_ring == Some(ring) {
                if self.fail_consume_after == 0 {
                    return Err(ath11k_hal::HalError::DeviceFault);
                }
                self.fail_consume_after -= 1;
            }
            if self
                .completion_ring
                .is_some_and(|expected| expected != ring)
            {
                return Ok(None);
            }
            if let Some(completions) = self.ring_completions.get_mut(&ring.0) {
                return Ok(completions.pop_front());
            }
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

        fn send_htt_ring_setup<C: crate::HttControl>(
            &self,
            _: usize,
            _: &mut C,
        ) -> Result<bool, DpError> {
            Ok(false)
        }

        fn setup_reo_controller(
            &self,
            _: RingId,
            _: RingId,
        ) -> Result<crate::reo::ReoController, DpError> {
            Err(DpError::UnsupportedDescriptor)
        }
    }

    struct AggregateBackend {
        next_dma: u64,
        memory: Rc<RefCell<BTreeMap<u64, Vec<u8>>>>,
        dma_writes: DmaWrites,
        mmio_writes: Rc<RefCell<Vec<(usize, u32)>>>,
        fail_dma_write_once: Rc<Cell<bool>>,
        fail_dma_read_token_once: Rc<Cell<Option<u64>>>,
        fail_mmio_write_once: Rc<Cell<bool>>,
    }

    impl Default for AggregateBackend {
        fn default() -> Self {
            Self {
                next_dma: 0,
                memory: Rc::new(RefCell::new(BTreeMap::new())),
                dma_writes: Rc::new(RefCell::new(Vec::new())),
                mmio_writes: Rc::new(RefCell::new(Vec::new())),
                fail_dma_write_once: Rc::new(Cell::new(false)),
                fail_dma_read_token_once: Rc::new(Cell::new(None)),
                fail_mmio_write_once: Rc::new(Cell::new(false)),
            }
        }
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
        fn write_u32(
            &mut self,
            _: &Self::Region,
            offset: usize,
            value: u32,
        ) -> Result<(), HardwareError> {
            if self.fail_mmio_write_once.replace(false) {
                return Err(HardwareError::DeviceFault);
            }
            self.mmio_writes.borrow_mut().push((offset, value));
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
            self.memory.borrow_mut().insert(address, vec![0xa5; size]);
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
            dma: &Self::Dma,
            range: Range<usize>,
            out: &mut [u8],
        ) -> Result<(), HardwareError> {
            if self.fail_dma_read_token_once.get() == Some(*dma) {
                self.fail_dma_read_token_once.set(None);
                return Err(HardwareError::DeviceFault);
            }
            out.copy_from_slice(
                self.memory
                    .borrow()
                    .get(dma)
                    .and_then(|bytes| bytes.get(range))
                    .ok_or(HardwareError::OutOfBounds)?,
            );
            Ok(())
        }
        fn dma_write(
            &mut self,
            dma: &Self::Dma,
            range: Range<usize>,
            bytes: &[u8],
        ) -> Result<(), HardwareError> {
            if self.fail_dma_write_once.replace(false) {
                return Err(HardwareError::DeviceFault);
            }
            self.memory
                .borrow_mut()
                .get_mut(dma)
                .and_then(|target| target.get_mut(range.clone()))
                .ok_or(HardwareError::OutOfBounds)?
                .copy_from_slice(bytes);
            self.dma_writes.borrow_mut().push((*dma, range));
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
        fn release_dma(&mut self, dma: Self::Dma) {
            self.memory.borrow_mut().remove(&dma);
        }
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

        fn send_htt_ring_setup<C: crate::HttControl>(
            &self,
            _: usize,
            _: &mut C,
        ) -> Result<bool, DpError> {
            Ok(false)
        }

        fn setup_reo_controller(
            &self,
            _: RingId,
            _: RingId,
        ) -> Result<crate::reo::ReoController, DpError> {
            Err(DpError::UnsupportedDescriptor)
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

    fn msi_config() -> Vec<crate::DpRingMsi> {
        use ath11k_hal::RingType;
        [
            (RingType::WbmToSwRelease, 0),
            (RingType::WbmToSwRelease, 4),
            (RingType::WbmToSwRelease, 2),
            (RingType::WbmToSwRelease, 3),
            // dp.c:ath11k_dp_srng_msi_setup routes HAL_REO_EXCEPTION through
            // WCN6750's rx_err interrupt group just like REO status.
            (RingType::ReoException, 0),
            (RingType::ReoStatus, 0),
            (RingType::ReoDestination, 0),
            (RingType::ReoDestination, 1),
            (RingType::ReoDestination, 2),
            (RingType::ReoDestination, 3),
            (RingType::RxdmaDestination, 0),
            (RingType::RxdmaMonitorStatus, 0),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (ring_type, ring_number))| crate::DpRingMsi {
            ring_type,
            ring_number,
            mac_id: 0,
            address: 0xfeed_0000,
            data: index as u32 + 1,
        })
        .collect()
    }

    #[test]
    fn wcn6750_station_config_uses_source_defaults() {
        let config = ClientTxConfig::wcn6750_station(7);
        assert_eq!(config.return_buffer_manager, 3);
        assert_eq!(config.metadata, 1 | (7 << 2));
        assert_eq!(config.encapsulation, EncapType::NativeWifi);
        assert_eq!((config.address_search_enable, config.search_type), (2, 0));
        assert_eq!((config.mac_id, config.lmac_id, config.pool_id), (0, 0, 0));
        assert!(!config.checksum_offload);
    }

    #[derive(Default)]
    struct HttMessages {
        messages: Vec<HttHostMessage>,
        fail_at: Option<usize>,
    }

    impl HttControl for HttMessages {
        const SEND_ERROR_IS_NON_VISIBLE: bool = true;

        fn send(&mut self, message: HttHostMessage) -> Result<(), DpError> {
            if self.fail_at == Some(self.messages.len()) {
                return Err(DpError::DeviceFault);
            }
            self.messages.push(message);
            Ok(())
        }

        fn receive(&mut self, _: u64) -> Result<Option<crate::HttTargetMessage>, DpError> {
            Ok(None)
        }
    }

    #[derive(Default)]
    struct AmbiguousHtt {
        sends: usize,
    }

    impl HttControl for AmbiguousHtt {
        fn send(&mut self, _: HttHostMessage) -> Result<(), DpError> {
            self.sends += 1;
            Ok(())
        }

        fn receive(&mut self, _: u64) -> Result<Option<crate::HttTargetMessage>, DpError> {
            Ok(None)
        }
    }

    #[test]
    fn aggregate_allocates_sets_up_and_tears_down_every_phase() {
        let backend = AggregateBackend::default();
        let memory = backend.memory.clone();
        let dma_writes = backend.dma_writes.clone();
        let mmio_writes = backend.mmio_writes.clone();
        let fail_dma_write = backend.fail_dma_write_once.clone();
        let fail_dma_read = backend.fail_dma_read_token_once.clone();
        let fail_mmio_write = backend.fail_mmio_write_once.clone();
        let device = Device::from_backend(backend);
        // Polling-first mode from specs/ARCH-dma-broker.md has no MSI records.
        let rings = crate::HalDpRings::new(&device, &[]).unwrap();
        assert!(
            memory
                .borrow()
                .values()
                .all(|bytes| bytes.iter().all(|byte| *byte == 0))
        );
        let mut dp = match ClientDataPath::ath11k_dp_alloc(device, rings, config()) {
            Ok(dp) => dp,
            Err(_) => panic!("aggregate allocation failed"),
        };
        assert_eq!(dp.ring_resources().common().len(), 15);
        dp.ath11k_dp_pdev_pre_alloc().unwrap();
        dp.ath11k_dp_pdev_reo_setup().unwrap();
        assert_eq!(dp.ring_resources().reo_destination().len(), 4);

        let tcl = dp.data_rings.unwrap().tcl;
        let descriptor = Descriptor::new(vec![0x5a; 32], 32).unwrap();
        dma_writes.borrow_mut().clear();
        mmio_writes.borrow_mut().clear();
        fail_dma_write.set(true);
        assert_eq!(
            dp.rings_mut().publish(tcl, descriptor.clone()),
            Err(ath11k_hal::HalError::DeviceFault)
        );
        assert!(dma_writes.borrow().is_empty());
        assert!(mmio_writes.borrow().is_empty());
        dp.rings_mut().publish(tcl, descriptor.clone()).unwrap();
        assert_eq!(dma_writes.borrow().last().unwrap().1, 0..32);
        assert_eq!(mmio_writes.borrow().last().unwrap().1, 8);

        fail_mmio_write.set(true);
        assert_eq!(
            dp.rings_mut().publish(tcl, descriptor.clone()),
            Err(ath11k_hal::HalError::DeviceFault)
        );
        assert_eq!(dma_writes.borrow().last().unwrap().1, 32..64);
        dp.rings_mut().publish(tcl, descriptor).unwrap();
        assert_eq!(dma_writes.borrow().last().unwrap().1, 32..64);
        assert_eq!(mmio_writes.borrow().last().unwrap().1, 16);

        let reo = dp.data_rings.unwrap().reo;
        let reo_dma = *memory
            .borrow()
            .iter()
            .find(|(_, bytes)| bytes.len() == 2_048 * 64 + 7)
            .map(|(address, _)| address)
            .unwrap();
        memory.borrow_mut().get_mut(&0).unwrap()[..4].copy_from_slice(&16_u32.to_le_bytes());
        memory.borrow_mut().get_mut(&reo_dma).unwrap()[..64].fill(0x33);
        fail_dma_read.set(Some(reo_dma));
        assert_eq!(
            dp.rings_mut().consume(reo),
            Err(ath11k_hal::HalError::DeviceFault)
        );
        let consumed = dp.rings_mut().consume(reo).unwrap().unwrap();
        assert!(consumed.bytes().iter().all(|byte| *byte == 0x33));
        assert_eq!(mmio_writes.borrow().last().unwrap().1, 16);

        memory.borrow_mut().get_mut(&0).unwrap()[..4].copy_from_slice(&32_u32.to_le_bytes());
        memory.borrow_mut().get_mut(&reo_dma).unwrap()[64..128].fill(0x44);
        fail_mmio_write.set(true);
        assert_eq!(
            dp.rings_mut().consume(reo),
            Err(ath11k_hal::HalError::DeviceFault)
        );
        let consumed = dp.rings_mut().consume(reo).unwrap().unwrap();
        assert!(consumed.bytes().iter().all(|byte| *byte == 0x44));
        assert_eq!(mmio_writes.borrow().last().unwrap().1, 32);

        dp.ath11k_dp_pdev_alloc().unwrap();
        assert_eq!(dp.ring_resources().pdev_rx().len(), 4);
        assert_eq!(dp.rx_buffers.len(), 4_095);
        assert_eq!(dp.monitor_status_buffers.len(), 1_023);
        let mut ambiguous = AmbiguousHtt::default();
        assert_eq!(dp.configure_htt(&mut ambiguous), Err(DpError::WrongState));
        assert_eq!(ambiguous.sends, 0);
        let mut htt = HttMessages {
            fail_at: Some(2),
            ..Default::default()
        };
        assert_eq!(dp.configure_htt(&mut htt), Err(DpError::DeviceFault));
        assert_eq!(htt.messages.len(), 2);
        htt.fail_at = None;
        dp.configure_htt(&mut htt).unwrap();
        assert_eq!(htt.messages.len(), 4);
        assert_eq!(
            htt.messages
                .iter()
                .map(|message| u32::from_le_bytes(message.0[..4].try_into().unwrap()))
                .collect::<Vec<_>>(),
            vec![0x0205_000b, 0x0100_010b, 0x0007_010b, 0x0101_010b]
        );
        let words = |message: &HttHostMessage| {
            message
                .0
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>()
        };
        let setups = htt.messages.iter().map(words).collect::<Vec<_>>();
        assert_eq!((setups[0][4], setups[0][6]), (688, 512));
        assert_eq!((setups[1][4], setups[1][6]), (692, 516));
        assert_eq!((setups[2][4], setups[2][6]), (532, 708));
        assert_eq!((setups[3][4], setups[3][6]), (704, 528));
        assert!(
            setups
                .iter()
                .all(|setup| (setup[8], setup[9], setup[10]) == (0, 0, 0))
        );
        assert_eq!(dp.configure_htt(&mut htt), Err(DpError::WrongState));

        dp.ath11k_dp_pdev_free().unwrap();
        dp.ath11k_dp_pdev_reo_cleanup().unwrap();
        dp.ath11k_dp_free().unwrap();

        let reused_id =
            ath11k_hal::Wcn6750Registers::ring_id(ath11k_hal::RingType::WbmIdleLink, 0, 0).unwrap();
        let pointer = usize::from(reused_id.0) * 4;
        memory.borrow_mut().get_mut(&0).unwrap()[pointer..pointer + 4]
            .copy_from_slice(&0xfeed_beef_u32.to_le_bytes());
        let (device, rings) = dp.into_parts().unwrap();
        let mut reused = match ClientDataPath::ath11k_dp_alloc(device, rings, config()) {
            Ok(dp) => dp,
            Err(_) => panic!("reallocation failed"),
        };
        assert_eq!(
            &memory.borrow().get(&0).unwrap()[pointer..pointer + 4],
            &[0; 4]
        );
        reused.ath11k_dp_free().unwrap();
        let _ = reused.into_parts().unwrap();
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
    fn nonempty_msi_mode_requires_reo_exception_and_programs_htt() {
        let device = Device::from_backend(AggregateBackend::default());
        let mut zero_address = msi_config();
        zero_address[0].address = 0;
        assert!(matches!(
            crate::HalDpRings::new(&device, &zero_address),
            Err(DpError::WrongState)
        ));

        let mut missing_reo_exception = msi_config();
        missing_reo_exception.retain(|msi| msi.ring_type != ath11k_hal::RingType::ReoException);
        let device = Device::from_backend(AggregateBackend::default());
        let rings = crate::HalDpRings::new(&device, &missing_reo_exception).unwrap();
        let error = match ClientDataPath::ath11k_dp_alloc(device, rings, config()) {
            Ok(_) => panic!("missing REO-exception MSI was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.cause(), DpError::NoResources);

        let device = Device::from_backend(AggregateBackend::default());
        let rings = crate::HalDpRings::new(&device, &msi_config()).unwrap();
        let mut dp = match ClientDataPath::ath11k_dp_alloc(device, rings, config()) {
            Ok(dp) => dp,
            Err(_) => panic!("complete MSI configuration was rejected"),
        };
        dp.ath11k_dp_pdev_pre_alloc().unwrap();
        dp.ath11k_dp_pdev_reo_setup().unwrap();
        dp.ath11k_dp_pdev_alloc().unwrap();
        let mut htt = HttMessages::default();
        dp.configure_htt(&mut htt).unwrap();
        let words = |message: &HttHostMessage| {
            message
                .0
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>()
        };
        let setups = htt.messages.iter().map(words).collect::<Vec<_>>();
        assert_eq!((setups[2][8], setups[2][10]), (0xfeed_0000, 11));
        assert_eq!((setups[3][8], setups[3][10]), (0xfeed_0000, 12));
        dp.ath11k_dp_pdev_free().unwrap();
        dp.ath11k_dp_pdev_reo_cleanup().unwrap();
        dp.ath11k_dp_free().unwrap();
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
    fn rx_refill_pools_segments_and_preserves_cookie_mapping() {
        let device = DeterministicBackend::device();
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
                pdev_id: 2,
                return_buffer_manager: 3,
                buffer_size: RX_BUFFER_SIZE,
            },
            3,
        )
        .unwrap();

        let infos = dp
            .rings()
            .published
            .iter()
            .map(|(_, descriptor)| {
                RxdmaBufferRing::from_bytes(descriptor.bytes())
                    .unwrap()
                    .info()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            infos.iter().map(|info| info.cookie).collect::<Vec<_>>(),
            vec![(2 << 18) | 1, (2 << 18) | 2, (2 << 18) | 3]
        );
        assert_eq!(
            infos[0].address.abs_diff(infos[1].address),
            RX_BUFFER_SIZE as u64
        );
        assert_eq!(dp.rx_buffers.len(), dp.rings().published.len());

        let completed_address = infos[0].address;
        let completed = dp
            .rx_buffers
            .iter()
            .position(|entry| entry.cookie == infos[0].cookie)
            .unwrap();
        drop(dp.rx_buffers.swap_remove(completed));
        dp.replenish_one().unwrap();
        let replacement =
            RxdmaBufferRing::from_bytes(dp.rings().published.last().unwrap().1.bytes())
                .unwrap()
                .info();
        assert_eq!(replacement.cookie, (2 << 18) | 4);
        assert_eq!(replacement.address, completed_address);
    }

    #[test]
    fn rx_buffer_id_wrap_skips_live_cookie_and_reports_exhaustion() {
        let mut next = RX_BUFFER_ID_MASK;
        let mut live = vec![1];
        let last =
            next_unused_rx_buffer_id(&mut next, |candidate| live.contains(&candidate)).unwrap();
        assert_eq!(last, RX_BUFFER_ID_MASK);
        live.push(last);
        assert_eq!(
            next_unused_rx_buffer_id(&mut next, |candidate| live.contains(&candidate)),
            Ok(2)
        );

        let mut next = 17;
        assert_eq!(
            next_unused_rx_buffer_id(&mut next, |_| true),
            Err(DpError::NoResources)
        );
        assert_eq!(next, 17);
    }

    #[test]
    fn reused_rx_segment_prepares_before_publish_and_discards_prepare_failure() {
        let (device, operations) = DeterministicBackend::recording_noncoherent_device();
        let pool = make_rx_pool(device).unwrap();
        let mut rings = ModelRings::default();
        let mut cookie = 1;
        let mut buffers = Vec::new();
        replenish_pool(
            &pool,
            &mut rings,
            RingId(4),
            0,
            3,
            RX_BUFFER_SIZE,
            1,
            &mut cookie,
            &mut buffers,
        )
        .unwrap();

        let mut completed = buffers.pop().unwrap();
        let completed_address = completed.buffer.device_address().unwrap().bits();
        operations.borrow_mut().clear();
        completed.buffer.sync_and_read(RX_BUFFER_SIZE).unwrap();
        completed.buffer.prepare_for_device().unwrap();
        drop(completed);
        replenish_pool(
            &pool,
            &mut rings,
            RingId(4),
            0,
            3,
            RX_BUFFER_SIZE,
            1,
            &mut cookie,
            &mut buffers,
        )
        .unwrap();
        let replacement = RxdmaBufferRing::from_bytes(rings.published.last().unwrap().1.bytes())
            .unwrap()
            .info();
        assert_eq!(replacement.address, completed_address);
        let operations = operations.borrow();
        let [
            Operation::SyncForCpu {
                dma: cpu_dma,
                range: cpu_range,
            },
            Operation::SyncForDevice {
                dma: device_dma,
                range: device_range,
            },
        ] = operations.as_slice()
        else {
            panic!("unexpected ownership order: {operations:?}");
        };
        assert_eq!((device_dma, device_range), (cpu_dma, cpu_range));
        drop(operations);

        let (device, failures) = DeterministicBackend::noncoherent_device_with_failures();
        let pool = make_rx_pool(device).unwrap();
        let mut rings = ModelRings::default();
        let mut cookie = 1;
        let mut buffers = Vec::new();
        replenish_pool(
            &pool,
            &mut rings,
            RingId(4),
            0,
            3,
            RX_BUFFER_SIZE,
            1,
            &mut cookie,
            &mut buffers,
        )
        .unwrap();
        let mut completed = buffers.pop().unwrap();
        let failed_address = completed.buffer.device_address().unwrap().bits();
        completed.buffer.sync_and_read(RX_BUFFER_SIZE).unwrap();
        failures.fail_next_sync_for_device();
        assert_eq!(
            completed.buffer.prepare_for_device(),
            Err(DpError::DeviceFault)
        );
        assert_eq!(rings.published.len(), 1);
        drop(completed);
        replenish_pool(
            &pool,
            &mut rings,
            RingId(4),
            0,
            3,
            RX_BUFFER_SIZE,
            1,
            &mut cookie,
            &mut buffers,
        )
        .unwrap();
        let after_failure = RxdmaBufferRing::from_bytes(rings.published.last().unwrap().1.bytes())
            .unwrap()
            .info();
        assert_ne!(after_failure.address, failed_address);
    }

    #[test]
    fn client_tx_syncs_then_publishes_exact_tcl_command() {
        let (device, operations) = DeterministicBackend::recording_noncoherent_device();
        let mut tx = config();
        tx.tid = 0;
        let mut dp = ClientDataPath::without_allocated_rings(device, ModelRings::default(), tx);
        dp.configure(DataRings {
            tcl: RingId(1),
            reo: RingId(2),
            wbm: RingId(3),
        })
        .unwrap();
        let mut frame = vec![0; 30];
        frame[0..2].copy_from_slice(&0x0088_u16.to_le_bytes());
        frame[24..26].copy_from_slice(&[5, 0]);
        dp.submit_host_frame(
            &frame,
            crate::PeerId(4),
            HostTxFlags {
                qos: true,
                ..HostTxFlags::default()
            },
        )
        .unwrap();

        assert!(
            matches!(operations.borrow().last(), Some(Operation::SyncForDevice { range, .. }) if range == &(0..28))
        );
        let (_, bytes) = &dp.rings().published[0];
        let command = TclDataCommand::from_bytes(&bytes.bytes()[4..]).unwrap();
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
        frame[0..2].copy_from_slice(&0x4008_u16.to_le_bytes());
        let flags = HostTxFlags {
            protected: true,
            favor_reliability: false,
            qos: false,
        };
        dp.submit_host_frame(&frame, crate::PeerId(4), flags)
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
        let result = dp.service_tx_completions().unwrap().remove(0);
        assert!(result.acknowledged);
        assert!(dp.pending.is_empty());
    }

    #[test]
    fn firmware_reinject_and_inspect_release_without_reporting() {
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
        frame[..2].copy_from_slice(&0x0008_u16.to_le_bytes());
        for _ in 0..2 {
            dp.submit_host_frame(&frame, crate::PeerId(4), HostTxFlags::default())
                .unwrap();
        }
        for (msdu_id, status) in [(0_u32, 3_u32), (1, 4)] {
            let mut release = WbmReleaseRing::new();
            let mut address = RxdmaBufferRing::new();
            address
                .set_software_cookie((1 << 19) | (msdu_id << 2))
                .unwrap();
            release.set_buffer_address(&address);
            release.set_release_source(3).unwrap();
            let mut raw = *release.as_bytes();
            let info0 = u32::from_le_bytes(raw[8..12].try_into().unwrap()) | status << 9;
            raw[8..12].copy_from_slice(&info0.to_le_bytes());
            dp.rings_mut()
                .completions
                .push_back(Descriptor::new(raw.to_vec(), 32).unwrap());
        }
        assert!(dp.service_tx_completions().unwrap().is_empty());
        assert!(dp.pending.is_empty());
    }

    #[test]
    fn unsupported_completion_source_does_not_release_live_owner() {
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
        frame[..2].copy_from_slice(&0x0008_u16.to_le_bytes());
        dp.submit_host_frame(&frame, crate::PeerId(4), HostTxFlags::default())
            .unwrap();
        let mut release = WbmReleaseRing::new();
        let mut address = RxdmaBufferRing::new();
        address.set_software_cookie(1 << 19).unwrap();
        release.set_buffer_address(&address);
        release.set_release_source(1).unwrap();
        dp.rings_mut()
            .completions
            .push_back(release.into_descriptor());

        assert!(dp.service_tx_completions().unwrap().is_empty());
        assert_eq!(dp.pending.len(), 1);
    }

    #[test]
    fn unsupported_reliability_hint_is_rejected_before_dma_mapping() {
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
        frame[..2].copy_from_slice(&0x0008_u16.to_le_bytes());
        assert_eq!(
            dp.submit_host_frame(
                &frame,
                crate::PeerId(4),
                HostTxFlags {
                    favor_reliability: true,
                    ..HostTxFlags::default()
                }
            ),
            Err(DpError::UnsupportedTxFlags)
        );
        assert!(dp.rings().published.is_empty());
    }

    #[test]
    fn mec_notification_without_live_cookie_is_ignored() {
        let device = DeterministicBackend::device();
        let mut dp =
            ClientDataPath::without_allocated_rings(device, ModelRings::default(), config());
        dp.configure(DataRings {
            tcl: RingId(1),
            reo: RingId(2),
            wbm: RingId(3),
        })
        .unwrap();
        let mut release = WbmReleaseRing::new();
        release.set_release_source(3).unwrap();
        let mut raw = *release.as_bytes();
        raw[8..12].copy_from_slice(&(3_u32 | (5 << 9)).to_le_bytes());
        dp.rings_mut()
            .completions
            .push_back(Descriptor::new(raw.to_vec(), 32).unwrap());
        assert!(dp.service_tx_completions().unwrap().is_empty());
        assert!(dp.pending.is_empty());
    }

    #[test]
    fn zero_host_budget_consumes_no_completion() {
        let device = DeterministicBackend::device();
        let mut dp =
            ClientDataPath::without_allocated_rings(device, ModelRings::default(), config());
        dp.configure(DataRings {
            tcl: RingId(1),
            reo: RingId(2),
            wbm: RingId(3),
        })
        .unwrap();
        dp.rings_mut()
            .completions
            .push_back(Descriptor::new(vec![0; 32], 32).unwrap());
        dp.rings_mut().completion_ring = Some(RingId(3));
        struct NoopHost;
        impl DpHost for NoopHost {
            fn receive(&mut self, _: HostRxFrame) {}
            fn tx_complete(&mut self, _: TxResult) {}
        }
        let result = dp.service_host(0, 1, &mut NoopHost).unwrap();
        assert_eq!(result.tx_delivered, 0);
        assert_eq!(dp.rings().completions.len(), 1);
    }

    #[test]
    fn host_observes_valid_tx_before_later_missing_owner() {
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
        frame[..2].copy_from_slice(&0x0008_u16.to_le_bytes());
        dp.submit_host_frame(&frame, crate::PeerId(4), HostTxFlags::default())
            .unwrap();
        let mut valid = WbmReleaseRing::new();
        let mut address = RxdmaBufferRing::new();
        address.set_software_cookie(1 << 19).unwrap();
        valid.set_buffer_address(&address);
        valid.set_release_source(3).unwrap();
        let mut raw = *valid.as_bytes();
        raw[8..12].copy_from_slice(&3_u32.to_le_bytes());
        dp.rings_mut()
            .completions
            .push_back(Descriptor::new(raw.to_vec(), 32).unwrap());
        dp.rings_mut()
            .completions
            .push_back(Descriptor::new(vec![0; 32], 32).unwrap());
        dp.rings_mut().completion_ring = Some(RingId(3));
        #[derive(Default)]
        struct CompletionHost(Vec<TxResult>);
        impl DpHost for CompletionHost {
            fn receive(&mut self, _: HostRxFrame) {}
            fn tx_complete(&mut self, result: TxResult) {
                self.0.push(result);
            }
        }
        let mut host = CompletionHost::default();
        let first = dp.service_host(1, 1, &mut host).unwrap();
        assert_eq!(host.0.len(), 1);
        assert!(host.0[0].acknowledged);
        assert_eq!((first.tx_delivered, first.tx_malformed), (1, 0));
        let second = dp.service_host(1, 0, &mut host).unwrap();
        assert_eq!((second.tx_delivered, second.tx_malformed), (0, 1));
    }

    #[test]
    fn host_observes_tx_completion_before_later_rx_fault() {
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
        frame[..2].copy_from_slice(&0x0008_u16.to_le_bytes());
        dp.submit_host_frame(&frame, crate::PeerId(4), HostTxFlags::default())
            .unwrap();
        let mut release = WbmReleaseRing::new();
        let mut address = RxdmaBufferRing::new();
        address.set_software_cookie(1 << 19).unwrap();
        release.set_buffer_address(&address);
        release.set_release_source(3).unwrap();
        let mut raw = *release.as_bytes();
        raw[8..12].copy_from_slice(&3_u32.to_le_bytes());
        dp.rings_mut()
            .completions
            .push_back(Descriptor::new(raw.to_vec(), 32).unwrap());
        dp.rings_mut().fail_consume_ring = Some(RingId(2));
        dp.rings_mut().fail_consume_after = 1;
        dp.rings_mut().completion_ring = Some(RingId(3));
        #[derive(Default)]
        struct CompletionHost(Vec<TxResult>);
        impl DpHost for CompletionHost {
            fn receive(&mut self, _: HostRxFrame) {}
            fn tx_complete(&mut self, result: TxResult) {
                self.0.push(result);
            }
        }
        let mut host = CompletionHost::default();
        assert_eq!(dp.service_host(3, 1, &mut host), Err(DpError::DeviceFault));
        assert_eq!(host.0.len(), 1);
    }

    #[test]
    fn host_delivery_rejects_failed_and_ethernet_decapped_frames() {
        let mut bytes = vec![0; 2048];
        bytes[46..48].copy_from_slice(&((1_u16 << 12) | (1 << 13)).to_le_bytes());
        bytes[84..88].copy_from_slice(&(1_u32 << 31).to_le_bytes());
        bytes[96..100].copy_from_slice(&5_u32.to_le_bytes());
        bytes[388] = 7;

        let valid = parse_received_buffer(&bytes).unwrap();
        assert_eq!(host_frame(valid).unwrap().bytes, [7]);

        // msdu_start.info2 decap_type = Ethernet2Dix.
        bytes[100..104].copy_from_slice(&(2_u32 << 8).to_le_bytes());
        assert_eq!(
            host_frame(parse_received_buffer(&bytes).unwrap()),
            Err(HostRxDropReason::UnsupportedDecap)
        );
        // attention.info1 FCS error takes precedence over decapsulation.
        bytes[80..84].copy_from_slice(&(1_u32 << 31).to_le_bytes());
        assert_eq!(
            host_frame(parse_received_buffer(&bytes).unwrap()),
            Err(HostRxDropReason::Fcs)
        );
    }

    #[test]
    fn host_decryption_status_requires_valid_non_open_encryption() {
        fn raw(status1: u32, encryption_valid: bool, encryption_type: u8) -> ReceivedFrame {
            let mut bytes = vec![0; 2048];
            bytes[46..48].copy_from_slice(&((1_u16 << 12) | (1 << 13)).to_le_bytes());
            bytes[80..84].copy_from_slice(&status1.to_le_bytes());
            bytes[84..88].copy_from_slice(&(1_u32 << 31).to_le_bytes());
            let msdu_length = if encryption_type == 8 { 53_u32 } else { 45 };
            bytes[96..100].copy_from_slice(&msdu_length.to_le_bytes());
            bytes[168..172].copy_from_slice(&(u32::from(encryption_type) << 2).to_le_bytes());
            if encryption_valid {
                bytes[184..188].copy_from_slice(&(1_u32 << 9).to_le_bytes());
            }
            bytes[388..390].copy_from_slice(&0x4008_u16.to_le_bytes());
            bytes[420] = 9;
            parse_received_buffer(&bytes).unwrap()
        }

        let mut open = raw(0, false, 0);
        open.status.msdu_length = 28;
        open.packet.bytes.truncate(28);
        assert_eq!(
            host_frame(open).unwrap().info.decrypt_status,
            RxDecryptStatus::NotDecrypted
        );
        let encrypted = host_frame(raw(0, true, 6)).unwrap();
        assert_eq!(encrypted.info.decrypt_status, RxDecryptStatus::Decrypted);
        assert_eq!(encrypted.bytes.len(), 25);
        assert_eq!(encrypted.bytes[1] & 0x40, 0);
        let mut multicast = raw(0, true, 6);
        multicast.status.multicast_broadcast = true;
        let multicast = host_frame(multicast).unwrap();
        assert_eq!(multicast.bytes.len(), 33);
        assert_ne!(multicast.bytes[1] & 0x40, 0);
        let ccmp256 = host_frame(raw(0, true, 8)).unwrap();
        assert_eq!(ccmp256.bytes.len(), 25);
        assert_eq!(
            host_frame(raw(1 << 29, true, 6)),
            Err(HostRxDropReason::Decrypt)
        );
        assert_eq!(
            host_frame(raw(1 << 28, true, 4)),
            Err(HostRxDropReason::TkipMic)
        );
    }

    #[test]
    fn native_wifi_delivery_restores_header_status_and_addresses() {
        let mut bytes = vec![0; 2048];
        bytes[46..48].copy_from_slice(&((1_u16 << 12) | (1 << 13)).to_le_bytes());
        bytes[84..88].copy_from_slice(&(1_u32 << 31).to_le_bytes());
        bytes[96..100].copy_from_slice(&25_u32.to_le_bytes());
        bytes[100..104].copy_from_slice(&(1_u32 << 8).to_le_bytes());
        bytes[268..270].copy_from_slice(&0x0088_u16.to_le_bytes());
        bytes[292] = 0x80; // A-MSDU present in original QoS control.
        bytes[388..390].copy_from_slice(&0x0008_u16.to_le_bytes());
        bytes[392..398].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        bytes[398..404].copy_from_slice(&[7, 8, 9, 10, 11, 12]);
        bytes[412] = 0xaa;
        let frame = host_frame(parse_received_buffer(&bytes).unwrap()).unwrap();
        assert_eq!(&frame.bytes[4..10], &[1, 2, 3, 4, 5, 6]);
        assert_eq!(&frame.bytes[10..16], &[7, 8, 9, 10, 11, 12]);
        assert_eq!(frame.bytes[24] & 0x80, 0);
        assert_eq!(frame.bytes[26], 0xaa);

        let mut non_first = parse_received_buffer(&bytes).unwrap();
        non_first.status.first_msdu = false;
        non_first.status.multicast_broadcast = true;
        non_first.status.encryption_info_valid = true;
        non_first.status.encryption_type = 6;
        non_first.status.tid = 5;
        non_first.status.mesh_control_present = true;
        non_first.packet.bytes[1] |= 0x40;
        non_first
            .packet
            .bytes
            .splice(24..24, [1, 2, 3, 4, 5, 6, 7, 8]);
        let non_first = host_frame(non_first).unwrap();
        assert_ne!(non_first.bytes[1] & 0x40, 0);
        assert_ne!(
            u16::from_le_bytes([non_first.bytes[24], non_first.bytes[25]]) & 0x100,
            0
        );
        assert_eq!(&non_first.bytes[26..34], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&non_first.bytes[34..42], &[1, 2, 3, 4, 5, 6, 7, 8]);
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
        last[80..84].copy_from_slice(&((1_u32 << 29) | (1 << 2)).to_le_bytes());
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
        assert_eq!(received.packet.peer, Some(crate::PeerId(0)));
        assert!(received.status.decrypt_error);
        assert_ne!(received.status.mpdu_errors, 0);
        assert!(received.status.multicast_broadcast);
        assert_eq!(
            (received.status.sequence_number, received.status.tid),
            (0, 0)
        );
    }

    #[test]
    fn first_buffer_msdu_length_error_is_not_hidden_by_clean_last_buffer() {
        let mut first = vec![0; 512];
        first[80..84].copy_from_slice(&(1_u32 << 17).to_le_bytes());
        first[96..100].copy_from_slice(&1_u32.to_le_bytes());
        let mut last = vec![0; 512];
        last[46..48].copy_from_slice(&((1_u16 << 12) | (1 << 13)).to_le_bytes());
        last[84..88].copy_from_slice(&(1_u32 << 31).to_le_bytes());
        assert_eq!(
            parse_received_chain(&[
                RxFragment {
                    bytes: first,
                    continuation: true,
                    peer: crate::PeerId(1),
                    sequence_number: 1,
                    tid: 0,
                },
                RxFragment {
                    bytes: last,
                    continuation: false,
                    peer: crate::PeerId(1),
                    sequence_number: 1,
                    tid: 0,
                },
            ]),
            Err(DpError::MalformedDescriptor)
        );
    }

    #[test]
    fn model_reo_completion_syncs_before_rx_descriptor_parse() {
        let (device, operations) = DeterministicBackend::recording_noncoherent_device();
        let bar = device.open_region(0).unwrap();
        let mut image = vec![0; 2048];
        image[46..48].copy_from_slice(&((1_u16 << 12) | (1 << 13) | (2 << 10)).to_le_bytes());
        image[84..88].copy_from_slice(&(1_u32 << 31).to_le_bytes());
        image[96..100].copy_from_slice(&8_u32.to_le_bytes());
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
        dp.rings_mut()
            .ring_completions
            .entry(2)
            .or_default()
            .push_back(reo.into_descriptor());
        let mut tx_frame = vec![0; 24];
        tx_frame[..2].copy_from_slice(&0x0008_u16.to_le_bytes());
        dp.submit_host_frame(&tx_frame, crate::PeerId(4), HostTxFlags::default())
            .unwrap();
        let mut release = WbmReleaseRing::new();
        let mut address = RxdmaBufferRing::new();
        address.set_software_cookie(1 << 19).unwrap();
        release.set_buffer_address(&address);
        release.set_release_source(3).unwrap();
        let mut raw = *release.as_bytes();
        raw[8..12].copy_from_slice(&3_u32.to_le_bytes());
        dp.rings_mut()
            .ring_completions
            .entry(3)
            .or_default()
            .push_back(Descriptor::new(raw.to_vec(), 32).unwrap());
        operations.borrow_mut().clear();
        struct RecordingHost {
            frames: Vec<HostRxFrame>,
            operations: Rc<RefCell<Vec<Operation>>>,
            tx_completed: usize,
        }
        impl DpHost for RecordingHost {
            fn receive(&mut self, frame: HostRxFrame) {
                let operations = self.operations.borrow();
                assert!(matches!(
                    operations.as_slice(),
                    [
                        Operation::SyncForCpu { range: cpu, .. },
                        Operation::SyncForDevice { range: device, .. }
                    ] if cpu.len() == 2048 && cpu == device
                ));
                drop(operations);
                self.frames.push(frame);
            }
            fn tx_complete(&mut self, _: TxResult) {
                self.tx_completed += 1;
            }
        }
        let mut host = RecordingHost {
            frames: Vec::new(),
            operations: operations.clone(),
            tx_completed: 0,
        };
        let result = dp.service_host(1, 1, &mut host).unwrap();
        assert_eq!(result.rx_delivered, 1);
        assert_eq!(result.rx_dropped, HostRxDropCounters::default());
        assert_eq!(host.frames[0].bytes, [1, 2, 3, 4]);
        assert_eq!(host.frames[0].info.decap_type, RxDecapType::Raw);
        assert_eq!(
            host.frames[0].info.decrypt_status,
            RxDecryptStatus::NotDecrypted
        );
        let result = dp.service_host(1, 1, &mut host).unwrap();
        assert_eq!(result.tx_delivered, 1);
        assert_eq!(host.tx_completed, 1);
    }
}
