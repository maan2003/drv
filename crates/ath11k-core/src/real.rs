//! Concrete composition of QMI, CE/HTC, WMI, and HTT for WCN6750.

use crate::{
    AssociationBandwidth, Cipher, CoreError, KeyConfig, KeyKind, Operation, PeerAssociation,
    Subsystems, Wcn6750QmiSession, WlanEvent,
};
use alloc::vec::Vec;
use ath11k_ce::{
    BoundService, CE_COUNT, CeAllocatedPipes, CeCompletionWait, CePipes, CePipesPacketIo, Htc,
    HtcPacketIo, HtcRouter, HtcTransport, ServiceId, WCN6750_SERVICE_TO_PIPE,
};
use ath11k_dp::{
    HalDpRings, HttControl,
    htt::{HttEvent, TARGET_VERSION_MAJOR, version_request},
    transport::{HtcHttTransport, ath11k_dp_htt_connect_service},
    tx::{ClientDataPath, ClientTxConfig},
};
use ath11k_platform_backend::{
    Backend, Bidirectional, CoherentDma, Device, MmioRegion, StreamingDma, ToDevice,
};
use ath11k_qmi::{
    FirmwareReady, Transport as QmiTransport,
    wire::{
        PipeDirection as QmiPipeDirection, ServicePipeConfig, TargetPipeConfig, WlanConfigRequest,
    },
};
use ath11k_wmi::{
    Command, Event, EventId, Transport as WmiTransport, WmiError,
    cmd::{
        Channel as WmiChannel, HtcWmiTransport, Init, KeySeqCounter, MgmtSend, PeerAssoc,
        PeerAssocParams, PeerAuthorize, PeerCreate, PeerDelete, PeerSetParam, ScanChannel,
        ScanChannelList, SetCurrentCountry, StaPowerSaveMode, StaPowerSaveParameter, TxRxStreams,
        VdevCreate, VdevDelete, VdevDown, VdevInstallKey, VdevSetParam, VdevStart, VdevStop,
        VdevUp, Wmi, WmmAccessCategory, WmmUpdate,
    },
};

const MGMT_RX_STATUS_ERROR_MASK: u32 = 0x01 | 0x08 | 0x10 | 0x20;

fn wcn6750_install_key(key: KeyConfig) -> Result<VdevInstallKey, CoreError> {
    let (key_cipher, mic_len) = match key.cipher {
        Cipher::Ccmp128 | Cipher::Ccmp256 => (4, 0),
        Cipher::Tkip => (2, 8),
        Cipher::Gcmp128 | Cipher::Gcmp256 => (9, 0),
        Cipher::BipCmac128 | Cipher::BipGmac128 | Cipher::BipGmac256 => {
            return Err(CoreError::Protocol);
        }
    };
    Ok(VdevInstallKey {
        vdev_id: u32::from(key.vdev.0),
        peer_addr: key.peer,
        key_idx: u32::from(key.index),
        key_flags: match key.kind {
            KeyKind::Pairwise => 0,
            KeyKind::Group => 1,
            KeyKind::IntegrityGroup => return Err(CoreError::Protocol),
        },
        key_cipher,
        key_rsc_counter: KeySeqCounter {
            low: key.receive_sequence_counter as u32,
            high: (key.receive_sequence_counter >> 32) as u32,
        },
        key_data: key.bytes,
        key_txmic_len: mic_len,
        key_rxmic_len: mic_len,
    })
}

fn wcn6750_wmm(vdev: crate::VdevId, wmm: crate::WmmConfig) -> WmmUpdate {
    WmmUpdate {
        vdev_id: u32::from(vdev.0),
        parameter_type: 0,
        access_categories: wmm.access_categories.map(|ac| WmmAccessCategory {
            cw_min: (1u32 << ac.ecw_min) - 1,
            cw_max: (1u32 << ac.ecw_max) - 1,
            aifs: u32::from(ac.aifsn),
            txop_limit: u32::from(ac.txop_limit),
            admission_control_mandatory: u32::from(ac.admission_control_mandatory),
            no_ack: 0,
        }),
    }
}

fn wcn6750_peer_assoc(association: PeerAssociation, local_nss: u8) -> PeerAssoc {
    let band_2ghz = association.primary_mhz < 3000;
    let mut params = PeerAssocParams {
        vdev_id: u32::from(association.vdev.0),
        peer_new_assoc: 1,
        peer_associd: u32::from(association.aid),
        peer_mac: association.peer,
        peer_caps: u32::from(association.capability_info),
        peer_listen_intval: u32::from(association.listen_interval),
        peer_nss: 1,
        peer_legacy_rates: association
            .legacy_rates
            .iter()
            // This is the WMI CCK encoding from
            // ath11k_mac_bitrate_to_rate(), not an 802.11 basic-rate flag.
            .map(|rate| {
                if matches!(*rate, 2 | 4 | 11 | 22) {
                    *rate | 0x80
                } else {
                    *rate
                }
            })
            .collect(),
        peer_phymode: if band_2ghz {
            if association
                .legacy_rates
                .iter()
                .any(|rate| matches!(*rate, 12 | 18 | 24 | 36 | 48 | 72 | 96 | 108))
            {
                1
            } else {
                2
            }
        } else {
            0
        },
        is_wme_set: association.qos,
        qos_flag: association.qos,
        // Pinned WMI clears AUTH while hardware crypto still needs the PTK
        // handshake; authorization is a later controlled-port operation.
        auth_flag: !association.need_ptk_4_way,
        need_ptk_4_way: association.need_ptk_4_way,
        need_gtk_2_way: association.need_gtk_2_way,
        is_pmf_enabled: association.pmf,
        is_assoc: true,
        ..Default::default()
    };
    if let Some(ht) = association.ht_capabilities {
        let cap = u16::from_le_bytes([ht[0], ht[1]]);
        let ampdu = ht[2];
        let rx_mask = &ht[3..13];
        params.ht_flag = true;
        params.peer_ht_caps = u32::from(cap);
        params.peer_max_mpdu = (1u32 << (13 + u32::from(ampdu & 3))) - 1;
        params.peer_mpdu_density = match (ampdu >> 2) & 7 {
            0 => 0,
            1..=3 => 1,
            4 => 2,
            5 => 4,
            6 => 8,
            _ => 16,
        };
        params.peer_ht_rates = rx_mask
            .iter()
            .enumerate()
            .flat_map(|(byte, mask)| {
                (0..8).filter_map(move |bit| {
                    (mask & (1 << bit) != 0).then_some((byte * 8 + bit) as u8)
                })
            })
            .collect();
        if params.peer_ht_rates.is_empty() {
            params.peer_ht_rates.extend(0..8);
        }
        // mac80211 derives sta::rx_nss from the four equal-modulation HT
        // stream masks, not from the later unequal-modulation/special MCS
        // bytes (sta_info.c:3509-3519 in the pinned source).
        params.peer_nss = rx_mask[..4]
            .iter()
            .filter(|mask| **mask != 0)
            .count()
            .max(1)
            .min(usize::from(local_nss)) as u32;
        params.peer_rate_caps |= 0x08;
        if cap & ((1 << 5) | (1 << 6)) != 0 {
            params.peer_rate_caps |= 0x04;
        }
        if cap & (1 << 7) != 0 {
            params.peer_rate_caps |= 0x20;
            params.stbc_flag = true;
        }
        let rx_stbc = (cap >> 8) & 3;
        params.peer_rate_caps |= u32::from(rx_stbc) << 6;
        params.stbc_flag |= rx_stbc != 0;
        if rx_mask[1] != 0 {
            params.peer_rate_caps |= if rx_mask[2] != 0 { 0x200 } else { 0x01 };
        }
        params.ldpc_flag = cap & 1 != 0;
        match (cap >> 2) & 3 {
            0 => params.static_mimops_flag = true,
            1 => params.dynamic_mimops_flag = true,
            3 => params.spatial_mux_flag = true,
            _ => {}
        }
        params.peer_phymode = match (band_2ghz, association.bandwidth) {
            (true, AssociationBandwidth::Bw20) => 5,
            (false, AssociationBandwidth::Bw20) => 4,
        };
    }
    if let Some(vht) = association.vht_capabilities {
        let cap = u32::from_le_bytes(vht[0..4].try_into().unwrap());
        let rx_map = u16::from_le_bytes(vht[4..6].try_into().unwrap());
        params.vht_flag = true;
        params.vht_capable = true;
        params.peer_vht_caps = cap;
        params.peer_max_mpdu = params
            .peer_max_mpdu
            .max((1u32 << (13 + ((cap >> 23) & 7))) - 1);
        let nss = (0..8)
            .rfind(|index| (rx_map >> (2 * index)) & 3 != 3)
            .map_or(1, |index| index + 1);
        params.peer_nss = nss.min(u32::from(local_nss));
        params.rx_mcs_set = u32::from(rx_map);
        params.rx_max_rate = u32::from(u16::from_le_bytes(vht[6..8].try_into().unwrap()));
        let tx_map = u32::from(u16::from_le_bytes(vht[8..10].try_into().unwrap()));
        params.tx_mcs_set = (tx_map & !0x00ff_0000) | 0x0100_0000;
        if params.tx_mcs_set & 3 == 3 {
            params.peer_vht_caps &= !0x0010_0000;
        }
        params.tx_max_rate = u32::from(u16::from_le_bytes(vht[10..12].try_into().unwrap()));
        params.peer_phymode = 8;
    }
    PeerAssoc {
        params,
        hw_crypto_disabled: false,
    }
}

fn management_rx_status_accepted(status: u32) -> bool {
    status & MGMT_RX_STATUS_ERROR_MASK == 0
}

fn wcn6750_event_pdev_is_primary(pdev_id: u32) -> bool {
    // Pinned DP_HW2SW_MACID maps both the SoC ID (0) and first hardware
    // radio ID (1) to the single host pdev. Physical WCN6750 management RX
    // events use the latter.
    pdev_id <= 1
}

pub fn wcn6750_scan_start(scan: crate::ScanConfig) -> ath11k_wmi::cmd::ScanStart {
    use ath11k_wmi::cmd::{ScanControlFlags, ScanEventFlags, ScanStart};
    ScanStart {
        scan_id: scan.id.0,
        scan_requester_id: 1,
        vdev_id: u32::from(scan.vdev.0),
        scan_priority: 2,
        notify_scan_events: 0,
        event_flags: ScanEventFlags {
            started: true,
            completed: true,
            bss_channel: true,
            foreign_channel: true,
            dequeued: true,
            ..Default::default()
        },
        control_flags: ScanControlFlags {
            passive: !scan.active,
            // ath11k_mac_op_hw_scan sets ordinary passive mode when no SSID
            // is supplied; strict-passive is a distinct optional contract.
            channel_stat_event: true,
            // WCN6750 is single-pdev-only, so Linux filters probe requests.
            filter_probe_request: true,
            ..Default::default()
        },
        control_flags_ext: 0,
        dwell_time_active: 50,
        dwell_time_active_2ghz: 0,
        dwell_time_passive: 150,
        dwell_time_active_6ghz: 40,
        dwell_time_passive_6ghz: 30,
        min_rest_time: 50,
        max_rest_time: 500,
        repeat_probe_time: 0,
        probe_spacing_time: 0,
        idle_time: 0,
        max_scan_time: 20_000,
        probe_delay: 5,
        burst_duration: 0,
        n_probes: 0,
        mac_addr: [0; 6],
        mac_mask: [0; 6],
        channels: scan.channels_mhz.into_iter().map(u32::from).collect(),
        ssids: scan.ssids,
        bssids: alloc::vec![[0xff; 6]],
        extra_ie: Vec::new(),
        short_ssid_hints: Vec::new(),
        bssid_hints: Vec::new(),
    }
}

fn wcn6750_scan_channel(channel: crate::RegulatoryChannel) -> ScanChannel {
    let half_dbm = |value: i8| value.max(0).saturating_mul(2) as u8;
    ScanChannel {
        mhz: u32::from(channel.frequency_mhz),
        center_freq1: u32::from(channel.frequency_mhz),
        center_freq2: 0,
        passive: channel.passive,
        allow_ht: channel.allow_ht,
        allow_vht: channel.allow_vht,
        allow_he: channel.allow_he,
        half_rate: false,
        quarter_rate: false,
        psc: false,
        dfs: channel.radar,
        phy_mode: if channel.frequency_mhz < 3_000 { 1 } else { 0 },
        min_power: 0,
        max_power: half_dbm(channel.max_power_dbm),
        max_reg_power: half_dbm(channel.max_reg_power_dbm),
        antenna_max: half_dbm(channel.max_antenna_gain_dbi),
        reg_class_id: 0,
    }
}

fn wcn6750_client_vdev_start(
    vdev: crate::VdevId,
    restart: bool,
    channel: crate::Channel,
    nss: u32,
) -> VdevStart {
    VdevStart {
        restart,
        vdev_id: u32::from(vdev.0),
        beacon_interval: 0,
        dtim_period: 0,
        hidden_ssid: false,
        pmf_enabled: false,
        hw_crypto_disabled: false,
        ssid: None,
        bcn_tx_rate: 0,
        num_noa_descriptors: 0,
        preferred_tx_streams: nss,
        preferred_rx_streams: nss,
        he_ops: 0,
        cac_duration_ms: 0,
        regdomain: 0,
        mbssid_flags: 0,
        mbssid_tx_vdev_id: 0,
        channel: WmiChannel {
            mhz: u32::from(channel.primary_mhz),
            band_center_freq1: u32::from(channel.center1_mhz),
            band_center_freq2: u32::from(channel.center2_mhz),
            info: channel.info,
            reg_info_1: channel.reg_info_1,
            reg_info_2: channel.reg_info_2,
        },
    }
}

const RDP_BYTES: usize = 176 * 4;

pub trait WmiTraceSink {
    fn record(&mut self, command: bool, id: u32, bytes: &[u8]) -> Result<(), CoreError>;
}

#[derive(Default)]
pub struct NoWmiTrace;
impl WmiTraceSink for NoWmiTrace {
    fn record(&mut self, _: bool, _: u32, _: &[u8]) -> Result<(), CoreError> {
        Ok(())
    }
}

struct TracingWmi<T, S> {
    transport: T,
    sink: S,
}
impl<T: WmiTransport, S: WmiTraceSink> WmiTransport for TracingWmi<T, S> {
    const SEND_ERROR_IS_NON_VISIBLE: bool = T::SEND_ERROR_IS_NON_VISIBLE;

    fn send(&mut self, command: Command) -> Result<(), WmiError> {
        let mut bytes = Vec::with_capacity(4 + command.tlvs().len());
        bytes.extend_from_slice(&(command.id.0 & 0x00ff_ffff).to_le_bytes());
        bytes.extend_from_slice(command.tlvs());
        self.sink
            .record(true, command.id.0, &bytes)
            .map_err(|_| WmiError::Transport)?;
        self.transport.send(command)
    }
    fn receive(&mut self, deadline_ns: u64) -> Result<Option<Event>, WmiError> {
        let event = self.transport.receive(deadline_ns)?;
        if let Some(event) = &event {
            let mut bytes = Vec::with_capacity(4 + event.tlvs().len());
            bytes.extend_from_slice(&(event.id.0 & 0x00ff_ffff).to_le_bytes());
            bytes.extend_from_slice(event.tlvs());
            self.sink
                .record(false, event.id.0, &bytes)
                .map_err(|_| WmiError::Transport)?;
        }
        Ok(event)
    }
}

type PacketIo<B, W> = CePipesPacketIo<B, W>;
type Router<B, W> = HtcRouter<PacketIo<B, W>>;
type Endpoint<B, W> = BoundService<PacketIo<B, W>>;
type RealWmi<B, W, S> = Wmi<TracingWmi<HtcWmiTransport<Endpoint<B, W>>, S>>;
type RealHtt<B, W> = HtcHttTransport<Endpoint<B, W>>;
type RealDp<B> = ClientDataPath<B, HalDpRings<B>>;

const fn control_budget_has_room(consumed: usize, budget: usize) -> bool {
    consumed < budget
}

/// Real subsystem owner. Every resource moves forward through an explicit
/// option; no raw descriptor, DMA address, or backend handle crosses this seam.
pub struct Wcn6750Subsystems<B, Q, A, W, D, S = NoWmiTrace>
where
    B: Backend,
    Q: QmiTransport,
    A: ath11k_qmi::FirmwareAssets,
    W: CeCompletionWait,
    D: FnMut() -> u64,
    S: WmiTraceSink,
{
    qmi: Wcn6750QmiSession<Q, A, crate::HardwareMemoryProvider<B>>,
    device: Device<B>,
    waiter: Option<W>,
    dp_interrupts: crate::Wcn6750DpInterrupts<B>,
    mmio: Option<MmioRegion<B>>,
    dp_mmio: Option<MmioRegion<B>>,
    rdp: Option<CoherentDma<B, Bidirectional>>,
    allocated: Option<CeAllocatedPipes<B>>,
    pipes: Option<CePipes<B>>,
    packet_io: Option<PacketIo<B, W>>,
    htc: Option<Htc>,
    router: Option<Router<B, W>>,
    wmi: Option<RealWmi<B, W, S>>,
    htt: Option<RealHtt<B, W>>,
    dp: Option<RealDp<B>>,
    trace: Option<S>,
    deadline: D,
    post_ready_quiet_drain: bool,
    service_ready: Option<ath11k_wmi::event::ServiceReadyState>,
    pending_mgmt_tx: Vec<(u32, StreamingDma<B, ToDevice>)>,
}

impl<B, Q, A, W, D, S> Wcn6750Subsystems<B, Q, A, W, D, S>
where
    B: Backend,
    Q: QmiTransport,
    A: ath11k_qmi::FirmwareAssets,
    W: CeCompletionWait,
    D: FnMut() -> u64,
    S: WmiTraceSink,
{
    pub fn new(
        qmi: Wcn6750QmiSession<Q, A, crate::HardwareMemoryProvider<B>>,
        device: Device<B>,
        waiter: W,
        dp_interrupts: crate::Wcn6750DpInterrupts<B>,
        deadline: D,
        trace: S,
    ) -> Self {
        Self {
            qmi,
            device,
            waiter: Some(waiter),
            dp_interrupts,
            mmio: None,
            dp_mmio: None,
            rdp: None,
            allocated: None,
            pipes: None,
            packet_io: None,
            htc: None,
            router: None,
            wmi: None,
            htt: None,
            dp: None,
            trace: Some(trace),
            deadline,
            post_ready_quiet_drain: false,
            service_ready: None,
            pending_mgmt_tx: Vec::new(),
        }
    }

    /// Enable the lab diagnostic that drains HTC traffic until the control
    /// deadline after WMI unified-ready, before sending the HTT version request.
    pub fn with_post_ready_quiet_drain(mut self, enabled: bool) -> Self {
        self.post_ready_quiet_drain = enabled;
        self
    }

    fn protocol<T>(value: Option<T>) -> Result<T, CoreError> {
        value.ok_or(CoreError::WrongState)
    }

    fn dp_error(error: ath11k_dp::DpError) -> CoreError {
        match error {
            ath11k_dp::DpError::WrongState => CoreError::WrongState,
            ath11k_dp::DpError::NoResources => CoreError::NoResources,
            ath11k_dp::DpError::DeviceFault => CoreError::DeviceFault,
            _ => CoreError::Protocol,
        }
    }

    fn receive_control(&mut self) -> Result<Vec<u8>, CoreError> {
        let deadline = (self.deadline)();
        let raw = Self::protocol(self.packet_io.as_mut())?
            .receive_htc(deadline)
            .map_err(CoreError::HtcControlReceive)?;
        let raw = match raw {
            Some(raw) => raw,
            None => {
                let packet_io = Self::protocol(self.packet_io.as_mut())?;
                let ce0_source_progress = packet_io.source_progress(0).ok();
                let ce2_destination_progress = packet_io.destination_progress(2).ok();
                let ce2_status_progress = packet_io.status_progress(2).ok();
                return Err(CoreError::HtcControlTimeout {
                    ce0_source_progress,
                    ce2_destination_progress,
                    ce2_status_progress,
                });
            }
        };
        let frame = Self::protocol(self.htc.as_mut())?
            .receive(&raw)
            .map_err(|_| CoreError::Protocol)?
            .ok_or(CoreError::Protocol)?;
        Ok(frame.bytes)
    }

    fn connect_service(&mut self, service: ServiceId) -> Result<(), CoreError> {
        let request = Self::protocol(self.htc.as_ref())?
            .connect_request(service)
            .encode();
        let frame = Self::protocol(self.htc.as_mut())?
            .send(0, &request)
            .map_err(|_| CoreError::Protocol)?;
        Self::protocol(self.packet_io.as_mut())?
            .send_htc(0, 0, frame)
            .map_err(CoreError::HtcControlSend)?;
        let response = self.receive_control()?;
        Self::protocol(self.htc.as_mut())?
            .connect_service(service, &response)
            .map_err(|_| CoreError::Protocol)?;
        Ok(())
    }

    fn pump_bounded(&mut self, work_budget: usize) -> Result<usize, CoreError> {
        let deadline = (self.deadline)();
        Self::protocol(self.router.as_ref())?
            .service_receive_bounded(deadline, work_budget)
            .map_err(|_| CoreError::DeviceFault)
    }

    fn pump(&mut self) -> Result<(), CoreError> {
        self.pump_bounded(usize::MAX).map(|_| ())
    }

    fn wait_for_htt_peer_map(
        &mut self,
        vdev: crate::VdevId,
        address: [u8; 6],
    ) -> Result<(), CoreError> {
        loop {
            while let Some(message) = Self::protocol(self.htt.as_mut())?
                .receive(0)
                .map_err(Self::dp_error)?
            {
                match message.decode().map_err(Self::dp_error)? {
                    HttEvent::PeerMap(map) => {
                        let matched =
                            u32::from(map.vdev_id) == u32::from(vdev.0) && map.address == address;
                        Self::protocol(self.dp.as_mut())?
                            .register_peer_map(map)
                            .map_err(Self::dp_error)?;
                        if matched {
                            return Ok(());
                        }
                    }
                    HttEvent::PeerUnmap { peer_id, .. } => Self::protocol(self.dp.as_mut())?
                        .unregister_peer_map(peer_id)
                        .map_err(Self::dp_error)?,
                    _ => {}
                }
            }
            if self.pump_bounded(1)? == 0 {
                return Err(CoreError::Protocol);
            }
        }
    }

    fn wait_for_htt_peer_unmap(
        &mut self,
        vdev: crate::VdevId,
        address: [u8; 6],
    ) -> Result<(), CoreError> {
        let expected = Self::protocol(self.dp.as_ref())?
            .peer_security(u32::from(vdev.0), address)
            .and_then(|security| security.peer_id)
            .ok_or(CoreError::Protocol)?;
        loop {
            while let Some(message) = Self::protocol(self.htt.as_mut())?
                .receive(0)
                .map_err(Self::dp_error)?
            {
                match message.decode().map_err(Self::dp_error)? {
                    HttEvent::PeerUnmap { peer_id, .. } => {
                        Self::protocol(self.dp.as_mut())?
                            .unregister_peer_map(peer_id)
                            .map_err(Self::dp_error)?;
                        if peer_id == expected {
                            return Ok(());
                        }
                    }
                    HttEvent::PeerMap(map) => Self::protocol(self.dp.as_mut())?
                        .register_peer_map(map)
                        .map_err(Self::dp_error)?,
                    _ => {}
                }
            }
            if self.pump_bounded(1)? == 0 {
                return Err(CoreError::Protocol);
            }
        }
    }

    fn wmi_send<R: ath11k_wmi::cmd::EncodeCommand>(
        &mut self,
        request: &R,
    ) -> Result<(), CoreError> {
        match Self::protocol(self.wmi.as_mut())?.send(request) {
            Ok(()) => Ok(()),
            Err(WmiError::NoCredits) => {
                if self.pump_bounded(1)? == 0 {
                    return Err(CoreError::Protocol);
                }
                Self::protocol(self.wmi.as_mut())?
                    .send(request)
                    .map_err(|_| CoreError::Protocol)
            }
            Err(_) => Err(CoreError::Protocol),
        }
    }

    fn qmi_config() -> WlanConfigRequest {
        let direction = |direction: ath11k_ce::PipeDirection| match direction {
            ath11k_ce::PipeDirection::None => QmiPipeDirection::None,
            ath11k_ce::PipeDirection::In => QmiPipeDirection::In,
            ath11k_ce::PipeDirection::Out => QmiPipeDirection::Out,
            ath11k_ce::PipeDirection::InOut => QmiPipeDirection::InOut,
            ath11k_ce::PipeDirection::InOutHostToHost => QmiPipeDirection::InOutHostToHost,
        };
        WlanConfigRequest {
            target_pipes: Some(
                ath11k_ce::WCN6750_TARGET_CE_CONFIG
                    .iter()
                    .map(|p| TargetPipeConfig {
                        pipe_num: u32::from(p.pipe),
                        direction: direction(p.direction),
                        entries: u32::from(p.entries),
                        max_bytes: u32::from(p.bytes_max),
                        flags: p.flags,
                    })
                    .collect(),
            ),
            service_pipes: Some(
                WCN6750_SERVICE_TO_PIPE
                    .iter()
                    .map(|p| ServicePipeConfig {
                        service_id: u32::from(p.service.0),
                        direction: direction(p.direction),
                        pipe_num: u32::from(p.pipe),
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    fn resource_config(&self) -> Result<ath11k_wmi::cmd::ResourceConfig, CoreError> {
        let chains = self
            .service_ready
            .as_ref()
            .and_then(|s| s.service_ready.as_ref())
            .and_then(|s| s.fixed.as_ref())
            .map(|s| s.num_rf_chains)
            .ok_or(CoreError::Protocol)?;
        let chain_mask = (1_u32.checked_shl(chains).unwrap_or(0)).wrapping_sub(1);
        Ok(ath11k_wmi::cmd::ResourceConfig {
            num_vdevs: 3,
            num_peers: 16,
            num_tids: 32,
            num_offload_peers: 3,
            num_offload_reorder_buffs: 3,
            num_peer_keys: 2,
            ast_skid_limit: 16,
            tx_chain_mask: chain_mask,
            rx_chain_mask: chain_mask,
            rx_timeout_pri: [100, 100, 100, 40],
            rx_decap_mode: 1,
            scan_max_pending_req: 4,
            bmiss_offload_max_vdev: 3,
            roam_offload_max_vdev: 3,
            roam_offload_max_ap_profiles: 8,
            tx_dbg_log_size: 1024,
            gtk_offload_max_vdev: 2,
            num_msdu_desc: 0x400,
            max_frag_entries: 0xa,
            num_tdls_vdevs: 1,
            num_tdls_conn_table_entries: 8,
            beacon_tx_offload_max_vdev: 2,
            num_multicast_filter_entries: 0x20,
            num_wow_filters: 0x16,
            use_pdev_id: 1,
            flag1: 1 << 5,
            ..Default::default()
        })
    }

    fn teardown_transport(&mut self) -> Result<(), CoreError> {
        // Firmware can no longer complete these frames. Revoke their
        // device-readable mappings before the WMI/CE owners are dismantled.
        self.pending_mgmt_tx.clear();
        if let Some(wmi) = self.wmi.take() {
            let tracing = wmi.detach();
            self.trace = Some(tracing.sink);
            drop(tracing.transport.into_inner());
        }
        self.htt.take();
        if let Some(router) = self.router.take() {
            match router.try_into_transport() {
                Ok(transport) => {
                    let (mut htc, io) = transport.into_parts();
                    htc.stop();
                    let ((_, _, _, pipes), waiter) = io.into_parts_with_waiter();
                    pipes.free_pipes();
                    self.waiter = Some(waiter);
                }
                Err(router) => {
                    self.router = Some(router);
                    return Err(CoreError::DeviceFault);
                }
            }
        }
        self.packet_io.take();
        self.pipes.take();
        self.allocated.take();
        self.rdp.take();
        self.mmio.take();
        self.htc.take();
        Ok(())
    }
}

impl<B, Q, A, W, D, S> Subsystems for Wcn6750Subsystems<B, Q, A, W, D, S>
where
    B: Backend,
    Q: QmiTransport,
    A: ath11k_qmi::FirmwareAssets,
    W: CeCompletionWait,
    D: FnMut() -> u64,
    S: WmiTraceSink,
{
    fn execute_vdev_start(
        &mut self,
        vdev: crate::VdevId,
        restart: bool,
        channel: crate::Channel,
    ) -> Result<(), crate::VdevStartFailure> {
        let nss = u32::from(
            self.client_nss()
                .map_err(crate::VdevStartFailure::NotSent)?,
        );
        Self::protocol(self.wmi.as_mut())
            .map_err(crate::VdevStartFailure::NotSent)?
            .discard_vdev_start(u32::from(vdev.0));
        self.wmi_send(&wcn6750_client_vdev_start(vdev, restart, channel, nss))
            .map_err(crate::VdevStartFailure::NotSent)?;
        self.pump().map_err(crate::VdevStartFailure::Ambiguous)?;
        let response = Self::protocol(self.wmi.as_mut())
            .map_err(crate::VdevStartFailure::Ambiguous)?
            .wait_for_vdev_start((self.deadline)(), u32::from(vdev.0))
            .map_err(|_| crate::VdevStartFailure::Ambiguous(CoreError::Protocol))?;
        if response.status == 0 {
            Ok(())
        } else {
            Err(crate::VdevStartFailure::Rejected(CoreError::Protocol))
        }
    }

    fn service_dp_host<H: ath11k_dp::tx::DpHost>(
        &mut self,
        work_budget: usize,
        receive_budget: usize,
        host: &mut H,
    ) -> Result<ath11k_dp::tx::HostServiceResult, CoreError> {
        Self::protocol(self.dp.as_mut())?
            .service_host(work_budget, receive_budget, host)
            .map_err(Self::dp_error)
    }

    fn wait_for_firmware_ready(&mut self) -> Result<FirmwareReady, CoreError> {
        let ready = self
            .qmi
            .wait_for_firmware_ready()
            .map_err(|_| CoreError::Protocol)?;
        let mmio = self.qmi.take_device_bar().ok_or(CoreError::Protocol)?;
        let dp_mmio = mmio
            .slice(0, mmio.len())
            .map_err(|_| CoreError::DeviceFault)?;
        self.mmio = Some(
            mmio.map_offsets(crate::wcn6750_register_offset)
                .map_err(|_| CoreError::DeviceFault)?,
        );
        self.dp_mmio = Some(
            dp_mmio
                .map_offsets(crate::wcn6750_register_offset)
                .map_err(|_| CoreError::DeviceFault)?,
        );
        Ok(ready)
    }

    fn next_wlan_event(&mut self) -> Result<Option<WlanEvent>, CoreError> {
        self.next_wlan_event_bounded(usize::MAX)
            .map(|(event, _)| event)
    }

    fn next_wlan_event_bounded(
        &mut self,
        work_budget: usize,
    ) -> Result<(Option<WlanEvent>, bool), CoreError> {
        use ath11k_wmi::event::{Decoder, EventDecoder as _, MgmtRx, MgmtTxCompletion, Scan};
        use ath11k_wmi::tags::{
            WMI_MGMT_RX_EVENTID, WMI_MGMT_TX_COMPLETION_EVENTID, WMI_SCAN_EVENTID,
        };

        let mut consumed = 0;
        while consumed < work_budget {
            let deadline = (self.deadline)();
            let mut event = Self::protocol(self.wmi.as_mut())?
                .next_event(deadline)
                .map_err(|_| CoreError::Protocol)?;
            if event.is_none() {
                consumed = consumed.saturating_add(self.pump_bounded(work_budget - consumed)?);
                if !control_budget_has_room(consumed, work_budget) {
                    return Ok((None, true));
                }
                event = Self::protocol(self.wmi.as_mut())?
                    .next_event(deadline)
                    .map_err(|_| CoreError::Protocol)?;
            }
            let Some(event) = event else {
                return Ok((None, consumed != 0));
            };
            consumed = consumed.saturating_add(1);
            let id = event.id;
            let decoded = match id {
                WMI_MGMT_RX_EVENTID => {
                    let received = Decoder::<MgmtRx>::new(id)
                        .decode(event)
                        .map_err(|_| CoreError::Protocol)?;
                    if !wcn6750_event_pdev_is_primary(received.pdev_id) {
                        continue;
                    }
                    // Match the pinned C receive path's CRC, decrypt, and key
                    // cache-miss rejection. MIC errors are also dropped until
                    // the host receive surface can represent that metadata.
                    if !management_rx_status_accepted(received.status) {
                        continue;
                    }
                    WlanEvent::from(received)
                }
                WMI_MGMT_TX_COMPLETION_EVENTID => {
                    let completion = Decoder::<MgmtTxCompletion>::new(id)
                        .decode(event)
                        .map_err(|_| CoreError::Protocol)?;
                    if !wcn6750_event_pdev_is_primary(completion.pdev_id) {
                        continue;
                    }
                    let Some(index) = self
                        .pending_mgmt_tx
                        .iter()
                        .position(|(buffer_id, _)| *buffer_id == completion.descriptor_id)
                    else {
                        continue;
                    };
                    self.pending_mgmt_tx.remove(index);
                    WlanEvent::from(completion)
                }
                WMI_SCAN_EVENTID => WlanEvent::from(
                    Decoder::<Scan>::new(id)
                        .decode(event)
                        .map_err(|_| CoreError::Protocol)?,
                ),
                EventId(_) => continue,
            };
            return Ok((Some(decoded), true));
        }
        Ok((None, consumed != 0))
    }

    fn client_nss(&self) -> Result<u8, CoreError> {
        let chains = self
            .service_ready
            .as_ref()
            .and_then(|state| state.service_ready.as_ref())
            .and_then(|ready| ready.fixed.as_ref())
            .map(|fixed| fixed.num_rf_chains)
            .ok_or(CoreError::Protocol)?;
        u8::try_from(chains)
            .ok()
            .filter(|chains| *chains != 0)
            .ok_or(CoreError::Protocol)
    }

    fn execute(&mut self, operation: Operation) -> Result<(), CoreError> {
        match operation {
            Operation::QmiInitService => self.qmi.init_service().map_err(CoreError::Qmi),
            Operation::QmiFirmwareStart => self
                .qmi
                .firmware_start(&Self::qmi_config(), 0, false)
                .map_err(CoreError::Qmi),
            Operation::QmiFirmwareStop => self.qmi.firmware_stop().map_err(CoreError::Qmi),
            Operation::QmiDeinitService => {
                self.qmi.deinit_service();
                Ok(())
            }
            Operation::HifPowerUp => {
                self.rdp = Some(
                    self.device
                        .alloc_coherent::<Bidirectional>(RDP_BYTES, 8)
                        .map_err(|_| CoreError::NoResources)?,
                );
                Ok(())
            }
            Operation::CeInitPipes => {
                self.allocated = Some(
                    CeAllocatedPipes::alloc_pipes(&self.device)
                        .map_err(|_| CoreError::NoResources)?,
                );
                let pipes = Self::protocol(self.allocated.take())?
                    .init_pipes(
                        Self::protocol(self.mmio.as_ref())?,
                        Self::protocol(self.rdp.as_ref())?,
                        [None; CE_COUNT],
                    )
                    .map_err(|_| CoreError::DeviceFault)?;
                self.pipes = Some(pipes);
                Ok(())
            }
            Operation::HtcInit => {
                let mut htc = Htc::new(1, true, true);
                htc.connect_service(ServiceId::RESERVED_CONTROL, &[])
                    .map_err(|_| CoreError::Protocol)?;
                self.htc = Some(htc);
                Ok(())
            }
            Operation::WmiAttach => Ok(()),
            Operation::HifStart => {
                let mut io = CePipesPacketIo::new_with_waiter(
                    self.device.clone(),
                    Self::protocol(self.mmio.take())?,
                    Self::protocol(self.rdp.take())?,
                    Self::protocol(self.pipes.take())?,
                    Self::protocol(self.waiter.take())?,
                );
                io.rx_post_buf().map_err(|_| CoreError::DeviceFault)?;
                self.packet_io = Some(io);
                Ok(())
            }
            Operation::HtcWaitTarget => {
                let ready = self.receive_control()?;
                Self::protocol(self.htc.as_mut())?
                    .wait_target(&ready)
                    .map(|_| ())
                    .map_err(|_| CoreError::Protocol)
            }
            Operation::DpHttConnect => self.connect_service(ServiceId::HTT_DATA_MSG),
            Operation::WmiConnect => self.connect_service(ServiceId::WMI_CONTROL),
            Operation::HtcStart => {
                let frame = Self::protocol(self.htc.as_mut())?
                    .start()
                    .map_err(|_| CoreError::Protocol)?;
                Self::protocol(self.packet_io.as_mut())?
                    .send_htc(0, 0, frame)
                    .map_err(|_| CoreError::DeviceFault)?;
                let router = HtcRouter::new(HtcTransport::new(
                    Self::protocol(self.htc.take())?,
                    Self::protocol(self.packet_io.take())?,
                ));
                let htt = ath11k_dp_htt_connect_service(
                    router
                        .endpoint(ServiceId::HTT_DATA_MSG)
                        .map_err(|_| CoreError::Protocol)?,
                );
                let wmi_endpoint = router
                    .endpoint(ServiceId::WMI_CONTROL)
                    .map_err(|_| CoreError::Protocol)?;
                let tracing = TracingWmi {
                    transport: HtcWmiTransport::new(wmi_endpoint),
                    sink: Self::protocol(self.trace.take())?,
                };
                let mut wmi = Wmi::attach(tracing);
                wmi.pdev_attach(0);
                wmi.connect();
                self.htt = Some(htt);
                self.wmi = Some(wmi);
                self.router = Some(router);
                Ok(())
            }
            Operation::WmiWaitServiceReady => {
                self.pump()?;
                self.service_ready = Some(
                    Self::protocol(self.wmi.as_mut())?
                        .wait_for_service_ready((self.deadline)())
                        .map_err(|_| CoreError::Protocol)?,
                );
                Ok(())
            }
            Operation::WmiCommandInit => {
                let resource_config = self.resource_config()?;
                self.wmi_send(&Init {
                    resource_config,
                    memory_chunks: Vec::new(),
                    hardware_mode: None,
                    bands: Vec::new(),
                })
            }
            Operation::WmiWaitUnifiedReady => loop {
                match Self::protocol(self.wmi.as_mut())?.wait_for_unified_ready((self.deadline)()) {
                    Ok(_) if self.post_ready_quiet_drain => break self.pump(),
                    Ok(_) => break Ok(()),
                    Err(WmiError::Timeout) if self.pump_bounded(1)? != 0 => {}
                    Err(_) => break Err(CoreError::Protocol),
                }
            },
            Operation::DpHttVersionRequest => {
                Self::protocol(self.htt.as_mut())?
                    .send(version_request())
                    .map_err(Self::dp_error)?;
                loop {
                    if let Some(message) = Self::protocol(self.htt.as_mut())?
                        .receive(0)
                        .map_err(Self::dp_error)?
                    {
                        if let HttEvent::VersionConfirm { major, .. } =
                            message.decode().map_err(Self::dp_error)?
                        {
                            return (major == TARGET_VERSION_MAJOR)
                                .then_some(())
                                .ok_or(CoreError::Protocol);
                        }
                    } else if self.pump_bounded(1)? == 0 {
                        let router = Self::protocol(self.router.as_ref())?;
                        return Err(CoreError::HttVersionTimeout {
                            ce4_source_progress: router.source_progress(4).ok(),
                            ce1_destination_progress: router.destination_progress(1).ok(),
                            ce1_status_progress: router.status_progress(1).ok(),
                        });
                    }
                }
            }
            Operation::WmiVdevCreate { vdev, mac } => {
                let nss = u32::from(self.client_nss()?);
                self.wmi_send(&VdevCreate {
                    vdev_id: u32::from(vdev.0),
                    vdev_type: 2,
                    vdev_subtype: 0,
                    mac_addr: mac,
                    pdev_id: 0,
                    mbssid_flags: 0,
                    mbssid_tx_vdev_id: 0,
                    band_2ghz: TxRxStreams { tx: nss, rx: nss },
                    band_5ghz: TxRxStreams { tx: nss, rx: nss },
                })
            }
            Operation::WmiSetCurrentCountry { alpha2 } => {
                Self::protocol(self.wmi.as_mut())?.discard_regulatory_update();
                self.wmi_send(&SetCurrentCountry {
                    pdev_id: 0,
                    alpha2: [alpha2[0], alpha2[1], 0],
                })
            }
            Operation::WmiScanChannelList { pdev, channels } => self.wmi_send(&ScanChannelList {
                pdev_id: u32::from(pdev.0),
                append: false,
                channels: channels.into_iter().map(wcn6750_scan_channel).collect(),
            }),
            Operation::WaitRegulatoryUpdate { pdev } if pdev.0 == 0 => {
                self.pump()?;
                let deadline = (self.deadline)();
                Self::protocol(self.wmi.as_mut())?
                    .wait_for_regulatory_update(deadline)
                    .map(|_| ())
                    .map_err(|_| CoreError::Protocol)
            }
            Operation::WmiVdevSetNss { vdev, nss } => self.wmi_send(&VdevSetParam {
                vdev_id: u32::from(vdev.0),
                param_id: 0x22,
                param_value: u32::from(nss),
            }),
            Operation::WmiStaPsRxWake { vdev } => self.wmi_send(&StaPowerSaveParameter {
                vdev_id: u32::from(vdev.0),
                param: 0,
                value: 0,
            }),
            Operation::WmiStaPsTxWake { vdev } => self.wmi_send(&StaPowerSaveParameter {
                vdev_id: u32::from(vdev.0),
                param: 1,
                value: 1,
            }),
            Operation::WmiStaPsPollCount { vdev } => self.wmi_send(&StaPowerSaveParameter {
                vdev_id: u32::from(vdev.0),
                param: 2,
                value: 0,
            }),
            Operation::WmiStaPsDisable { vdev } => self.wmi_send(&StaPowerSaveMode {
                vdev_id: u32::from(vdev.0),
                mode: 0,
            }),
            Operation::WmiVdevSetRtsThreshold { vdev, threshold } => self.wmi_send(&VdevSetParam {
                vdev_id: u32::from(vdev.0),
                param_id: 1,
                param_value: threshold,
            }),
            Operation::WmiVdevStart {
                vdev,
                restart,
                channel,
            } => {
                let nss = u32::from(self.client_nss()?);
                Self::protocol(self.wmi.as_mut())?.discard_vdev_start(u32::from(vdev.0));
                self.wmi_send(&wcn6750_client_vdev_start(vdev, restart, channel, nss))
            }
            Operation::WmiVdevUp { vdev, bssid, aid } => self.wmi_send(&VdevUp {
                vdev_id: u32::from(vdev.0),
                assoc_id: u32::from(aid),
                bssid,
                tx_bssid: None,
                nontx_profile_idx: 0,
                nontx_profile_cnt: 0,
            }),
            Operation::WmiVdevDown { vdev } => self.wmi_send(&VdevDown {
                vdev_id: u32::from(vdev.0),
            }),
            Operation::WmiVdevStop { vdev } => self.wmi_send(&VdevStop {
                vdev_id: u32::from(vdev.0),
            }),
            Operation::WmiVdevDelete { vdev } => self.wmi_send(&VdevDelete {
                vdev_id: u32::from(vdev.0),
            }),
            Operation::WmiPeerCreate { vdev, address } => {
                Self::protocol(self.wmi.as_mut())?.discard_peer_created(u32::from(vdev.0), address);
                self.wmi_send(&PeerCreate {
                    vdev_id: u32::from(vdev.0),
                    peer_addr: address,
                    peer_type: 0,
                })
            }
            Operation::WmiPeerDelete { vdev, address } => {
                Self::protocol(self.wmi.as_mut())?.discard_peer_deleted(u32::from(vdev.0), address);
                self.wmi_send(&PeerDelete {
                    vdev_id: u32::from(vdev.0),
                    peer_addr: address,
                })
            }
            Operation::WmiPeerAssociate(association) => {
                Self::protocol(self.wmi.as_mut())?
                    .discard_peer_associated(u32::from(association.vdev.0), association.peer);
                let local_nss = self.client_nss()?;
                self.wmi_send(&wcn6750_peer_assoc(association, local_nss))
            }
            Operation::WmiWmmUpdate { vdev, wmm } => self.wmi_send(&wcn6750_wmm(vdev, wmm)),
            Operation::WmiPeerSetSmps {
                vdev,
                address,
                mode,
            } => self.wmi_send(&PeerSetParam {
                vdev_id: u32::from(vdev.0),
                peer_addr: address,
                param_id: 1,
                param_value: mode,
            }),
            Operation::WaitPeerCreated { vdev, address } => {
                self.pump()?;
                let deadline = (self.deadline)();
                let response = Self::protocol(self.wmi.as_mut())?
                    .wait_for_peer_created(deadline, u32::from(vdev.0), address)
                    .map_err(|_| CoreError::Protocol)?;
                if response.status == 0 {
                    self.wait_for_htt_peer_map(vdev, address)
                } else {
                    Err(CoreError::Protocol)
                }
            }
            Operation::WaitPeerDeleted { vdev, address } => {
                self.pump()?;
                let deadline = (self.deadline)();
                Self::protocol(self.wmi.as_mut())?
                    .wait_for_peer_deleted(deadline, u32::from(vdev.0), address)
                    .map_err(|_| CoreError::Protocol)?;
                self.wait_for_htt_peer_unmap(vdev, address)
            }
            Operation::WaitPeerAssociated { vdev, address } => {
                self.pump()?;
                let deadline = (self.deadline)();
                Self::protocol(self.wmi.as_mut())?
                    .wait_for_peer_associated(deadline, u32::from(vdev.0), address)
                    .map(|_| ())
                    .map_err(|_| CoreError::Protocol)
            }
            Operation::WmiInstallKey(key) => {
                Self::protocol(self.wmi.as_mut())?
                    .discard_key_installed(u32::from(key.vdev.0), u32::from(key.index));
                self.wmi_send(&wcn6750_install_key(key)?)
            }
            Operation::WaitKeyInstalled { vdev, key_index } => {
                self.pump()?;
                let deadline = (self.deadline)();
                let response = Self::protocol(self.wmi.as_mut())?
                    .wait_for_key_installed(deadline, u32::from(vdev.0), u32::from(key_index))
                    .map_err(|_| CoreError::Protocol)?;
                if response.status == 0 {
                    Ok(())
                } else {
                    Err(CoreError::Protocol)
                }
            }
            Operation::WmiPeerAuthorize {
                vdev,
                address,
                authorized,
            } => self.wmi_send(&PeerAuthorize {
                vdev_id: u32::from(vdev.0),
                peer_addr: address,
                authorized,
            }),
            Operation::WaitVdevSetup { vdev } => {
                self.pump()?;
                let deadline = (self.deadline)();
                let response = Self::protocol(self.wmi.as_mut())?
                    .wait_for_vdev_start(deadline, u32::from(vdev.0))
                    .map_err(|_| CoreError::Protocol)?;
                if response.status == 0 {
                    Ok(())
                } else {
                    Err(CoreError::Protocol)
                }
            }
            Operation::WmiScanStart(scan) => self.wmi_send(&wcn6750_scan_start(scan)),
            Operation::WmiMgmtTx(frame) => {
                if frame.bytes.is_empty()
                    || self.pending_mgmt_tx.len() >= 512
                    || self
                        .pending_mgmt_tx
                        .iter()
                        .any(|(buffer_id, _)| *buffer_id == frame.buffer_id)
                {
                    return Err(CoreError::NoResources);
                }
                let mut payload = self
                    .device
                    .alloc_streaming::<ToDevice>(frame.bytes.len(), 4)
                    .map_err(|_| CoreError::NoResources)?;
                payload
                    .write(0, &frame.bytes)
                    .and_then(|()| payload.sync_for_device(0, frame.bytes.len()))
                    .map_err(|_| CoreError::DeviceFault)?;
                let paddr = payload
                    .device_address(0)
                    .map_err(|_| CoreError::DeviceFault)?
                    .bits();
                self.wmi_send(&MgmtSend {
                    vdev_id: u32::from(frame.vdev.0),
                    desc_id: frame.buffer_id,
                    channel_freq: 0,
                    paddr,
                    frame: frame.bytes,
                    tx_params_valid: false,
                })?;
                self.pending_mgmt_tx.push((frame.buffer_id, payload));
                Ok(())
            }
            Operation::WmiDetach => self.teardown_transport(),
            // The first hardware run intentionally polls DP ring shadows. Do
            // not enable DP eventfds until an MSI doorbell mapping exists.
            Operation::HifIrqEnable => {
                if self.dp_interrupts.is_enabled() {
                    Err(CoreError::WrongState)
                } else {
                    Ok(())
                }
            }
            Operation::HifIrqDisable => {
                self.dp_interrupts.disable();
                Ok(())
            }
            Operation::DpAllocate => {
                let rings =
                    HalDpRings::new(&self.device, Self::protocol(self.dp_mmio.take())?, &[])
                        .map_err(Self::dp_error)?;
                let dp = ClientDataPath::ath11k_dp_alloc(
                    self.device.clone(),
                    rings,
                    ClientTxConfig::wcn6750_station(0),
                )
                .map_err(|error| CoreError::DpAllocation {
                    cause: error.cause(),
                    cleanup: error.cleanup_error(),
                })?;
                self.dp = Some(dp);
                Ok(())
            }
            Operation::DpPdevPreAllocate => Self::protocol(self.dp.as_mut())?
                .ath11k_dp_pdev_pre_alloc()
                .map_err(Self::dp_error),
            Operation::DpReoSetup => Self::protocol(self.dp.as_mut())?
                .ath11k_dp_pdev_reo_setup()
                .map_err(Self::dp_error),
            Operation::DpPdevAllocate => {
                let dp = Self::protocol(self.dp.as_mut())?;
                dp.ath11k_dp_pdev_alloc().map_err(Self::dp_error)?;
                if let Err(error) = dp.configure_htt(Self::protocol(self.htt.as_mut())?) {
                    if let Err(cleanup_error) = dp.ath11k_dp_pdev_free() {
                        return Err(Self::dp_error(cleanup_error));
                    }
                    return Err(Self::dp_error(error));
                }
                Ok(())
            }
            Operation::DpPdevFree => Self::protocol(self.dp.as_mut())?
                .ath11k_dp_pdev_free()
                .map_err(Self::dp_error),
            Operation::DpReoCleanup => Self::protocol(self.dp.as_mut())?
                .ath11k_dp_pdev_reo_cleanup()
                .map_err(Self::dp_error),
            Operation::DpFree => {
                Self::protocol(self.dp.as_mut())?
                    .ath11k_dp_free()
                    .map_err(Self::dp_error)?;
                self.dp.take();
                Ok(())
            }
            // The first client vdev is fixed at zero and was used to build
            // the DP TX metadata when the aggregate was allocated.
            Operation::DpVdevTxAttach { vdev } if vdev.0 == 0 => Ok(()),
            Operation::DpPeerSetup { vdev, address } => {
                self.wmi_send(&PeerSetParam {
                    vdev_id: u32::from(vdev.0),
                    peer_addr: address,
                    param_id: 0x13,
                    // WCN6750 mac_id 0 uses REO destination ring 1.
                    param_value: 1 | (1 << 1),
                })?;
                let (dp, wmi) = (self.dp.as_mut(), self.wmi.as_mut());
                Self::protocol(dp)?
                    .setup_peer(Self::protocol(wmi)?, u32::from(vdev.0), address)
                    .map_err(Self::dp_error)
            }
            Operation::DpPeerCleanup { vdev, address } => Self::protocol(self.dp.as_mut())?
                .cleanup_peer(u32::from(vdev.0), address)
                .map_err(Self::dp_error),
            Operation::DpInstallPeerKey(key) => {
                let security_type = match key.cipher {
                    Cipher::Ccmp128 => ath11k_dp::reo::PeerSecurityType::Ccmp128,
                    Cipher::Ccmp256 => ath11k_dp::reo::PeerSecurityType::Ccmp256,
                    Cipher::Tkip => ath11k_dp::reo::PeerSecurityType::TkipMic,
                    Cipher::Gcmp128 => ath11k_dp::reo::PeerSecurityType::Gcmp128,
                    Cipher::Gcmp256 => ath11k_dp::reo::PeerSecurityType::Gcmp256,
                    _ => return Err(CoreError::Protocol),
                };
                Self::protocol(self.dp.as_mut())?
                    .install_peer_key(
                        u32::from(key.vdev.0),
                        key.peer,
                        key.kind == KeyKind::Pairwise,
                        key.index,
                        security_type,
                    )
                    .map_err(Self::dp_error)
            }
            Operation::HifStop
            | Operation::MacAllocate
            | Operation::MacRegister
            | Operation::RadioStart => Ok(()),
            Operation::MacDestroy
            | Operation::MacUnregister
            | Operation::RegFree
            | Operation::HifPowerDown => Ok(()),
            _ => Err(CoreError::Protocol),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_conversion_preserves_group_rsc_and_tkip_mic_lengths() {
        let command = wcn6750_install_key(KeyConfig {
            vdev: crate::VdevId(3),
            peer: [2, 0, 0, 0, 0, 2],
            index: 2,
            cipher: Cipher::Tkip,
            kind: KeyKind::Group,
            protection: crate::KeyProtection::RxTx,
            receive_sequence_counter: 0x1122_3344_5566_7788,
            bytes: alloc::vec![0x55; 32],
        })
        .unwrap();
        assert_eq!(command.key_flags, 1);
        assert_eq!(command.key_cipher, 2);
        assert_eq!(command.key_rsc_counter.low, 0x5566_7788);
        assert_eq!(command.key_rsc_counter.high, 0x1122_3344);
        assert_eq!((command.key_txmic_len, command.key_rxmic_len), (8, 8));
    }

    #[test]
    fn legacy_peer_conversion_adds_wmi_cck_marker() {
        let command = wcn6750_peer_assoc(
            PeerAssociation {
                vdev: crate::VdevId(0),
                peer: [2, 0, 0, 0, 0, 2],
                aid: 42,
                listen_interval: 0,
                primary_mhz: 2437,
                bandwidth: AssociationBandwidth::Bw20,
                capability_info: 0x0421,
                legacy_rates: alloc::vec![2, 4, 11, 22, 12, 18, 24, 36],
                qos: false,
                ht_capabilities: None,
                vht_capabilities: None,
                wmm: None,
                need_ptk_4_way: false,
                need_gtk_2_way: false,
                pmf: false,
            },
            2,
        );
        assert_eq!(
            command.params.peer_legacy_rates,
            [0x82, 0x84, 0x8b, 0x96, 12, 18, 24, 36]
        );
        assert_eq!(command.params.peer_phymode, 1);
        assert_eq!(ath11k_wmi::cmd::copy_peer_flags(&command.params, false), 1);
    }

    #[test]
    fn ht_peer_conversion_preserves_capabilities_and_mcs() {
        let mut ht = [0; 26];
        ht[..5].copy_from_slice(&[0xef, 0x01, 0x13, 0xff, 0xff]);
        let command = wcn6750_peer_assoc(
            PeerAssociation {
                vdev: crate::VdevId(0),
                peer: [2, 0, 0, 0, 0, 2],
                aid: 42,
                listen_interval: 0,
                primary_mhz: 5180,
                bandwidth: AssociationBandwidth::Bw20,
                capability_info: 0x0421,
                legacy_rates: alloc::vec![12, 18, 24],
                qos: true,
                ht_capabilities: Some(ht),
                vht_capabilities: None,
                wmm: None,
                need_ptk_4_way: false,
                need_gtk_2_way: false,
                pmf: false,
            },
            2,
        );
        assert_eq!(command.params.peer_ht_caps, 0x01ef);
        assert_eq!(command.params.peer_max_mpdu, 65_535);
        assert_eq!(command.params.peer_mpdu_density, 2);
        assert_eq!(command.params.peer_ht_rates, (0..16).collect::<Vec<_>>());
        assert_eq!(command.params.peer_nss, 2);
        assert_eq!(command.params.peer_rate_caps, 0x6d);
        assert_eq!(command.params.peer_phymode, 4);
        assert_eq!(
            ath11k_wmi::cmd::copy_peer_flags(&command.params, false),
            0x0021_9003
        );
    }

    #[test]
    fn ht_special_mcs_bytes_do_not_inflate_peer_nss() {
        let mut ht = [0; 26];
        ht[3] = 0xff;
        ht[12] = 0x01;
        let command = wcn6750_peer_assoc(
            PeerAssociation {
                vdev: crate::VdevId(0),
                peer: [2, 0, 0, 0, 0, 2],
                aid: 42,
                listen_interval: 0,
                primary_mhz: 2437,
                bandwidth: AssociationBandwidth::Bw20,
                capability_info: 0x0421,
                legacy_rates: alloc::vec![2, 4, 11, 22],
                qos: true,
                ht_capabilities: Some(ht),
                vht_capabilities: None,
                wmm: None,
                need_ptk_4_way: false,
                need_gtk_2_way: false,
                pmf: false,
            },
            2,
        );
        assert_eq!(command.params.peer_nss, 1);
    }

    #[test]
    fn qmi_config_preserves_host_to_host_pipe_direction() {
        let config = Wcn6750Subsystems::<
            drv_hardware_backends::DeterministicBackend,
            DummyQmi,
            DummyAssets,
            ath11k_ce::NoCompletionWait,
            fn() -> u64,
        >::qmi_config();
        assert!(config.target_pipes.unwrap().iter().any(|pipe| {
            pipe.pipe_num == 7 && pipe.direction == QmiPipeDirection::InOutHostToHost
        }));
    }

    #[test]
    fn client_vdev_start_uses_c_station_defaults_and_selected_channel() {
        let channel = crate::Channel::client_20mhz(crate::RegulatoryChannel {
            frequency_mhz: 2437,
            max_power_dbm: 20,
            max_reg_power_dbm: 18,
            max_antenna_gain_dbi: 6,
            passive: false,
            radar: false,
            allow_ht: true,
            allow_vht: true,
            allow_he: true,
        });
        let command = wcn6750_client_vdev_start(crate::VdevId(2), true, channel, 2);
        assert!(command.restart);
        assert_eq!(command.vdev_id, 2);
        assert_eq!((command.beacon_interval, command.dtim_period), (0, 0));
        assert_eq!(
            (command.preferred_tx_streams, command.preferred_rx_streams),
            (2, 2)
        );
        assert_eq!(command.channel.mhz, 2437);
        assert_eq!(command.channel.info, channel.info);
        assert_eq!(command.channel.reg_info_1, channel.reg_info_1);
        assert_eq!(command.channel.reg_info_2, channel.reg_info_2);
    }

    #[test]
    fn passive_scan_uses_wcn6750_linux_control_flags() {
        let command = wcn6750_scan_start(crate::ScanConfig {
            vdev: crate::VdevId(0),
            id: crate::ScanId(0xa000),
            active: false,
            channels_mhz: alloc::vec![2412, 2437, 2462],
            ssids: Vec::new(),
        });
        assert!(command.control_flags.passive);
        assert!(command.control_flags.channel_stat_event);
        assert!(command.control_flags.filter_probe_request);
        assert!(!command.control_flags.strict_passive);
    }

    #[test]
    fn regulatory_channel_uses_linux_half_dbm_power_and_capability_bits() {
        let command = wcn6750_scan_channel(crate::RegulatoryChannel {
            frequency_mhz: 2412,
            max_power_dbm: 23,
            max_reg_power_dbm: 20,
            max_antenna_gain_dbi: 6,
            passive: true,
            radar: false,
            allow_ht: true,
            allow_vht: false,
            allow_he: true,
        });
        assert_eq!((command.mhz, command.center_freq1), (2412, 2412));
        assert_eq!(command.phy_mode, 1);
        assert!(command.passive && command.allow_ht && command.allow_he);
        assert_eq!(
            (
                command.max_power,
                command.max_reg_power,
                command.antenna_max
            ),
            (46, 40, 12)
        );
    }

    struct DummyQmi;
    impl QmiTransport for DummyQmi {
        fn start_service(&mut self, _: u32, _: u32) -> Result<(), ath11k_qmi::QmiError> {
            Ok(())
        }
        fn stop_service(&mut self) {}
        fn send(
            &mut self,
            _: ath11k_qmi::Request,
        ) -> Result<ath11k_qmi::TransactionId, ath11k_qmi::QmiError> {
            Err(ath11k_qmi::QmiError::Transport)
        }
        fn receive(&mut self, _: u64) -> Result<ath11k_qmi::Incoming, ath11k_qmi::QmiError> {
            Err(ath11k_qmi::QmiError::Timeout)
        }
        fn now_ns(&self) -> u64 {
            0
        }
    }
    struct DummyAssets;
    impl ath11k_qmi::FirmwareAssets for DummyAssets {
        fn board_data(&mut self, _: u32) -> Result<Vec<u8>, ath11k_qmi::QmiError> {
            Ok(Vec::new())
        }
        fn calibration_data(&mut self) -> Result<Option<Vec<u8>>, ath11k_qmi::QmiError> {
            Ok(None)
        }
        fn regulatory_data(&mut self) -> Result<Option<Vec<u8>>, ath11k_qmi::QmiError> {
            Ok(None)
        }
        fn m3_firmware(&mut self) -> Result<Option<Vec<u8>>, ath11k_qmi::QmiError> {
            Ok(None)
        }
    }
    #[test]
    fn exact_control_budget_defers_the_routed_wmi_event() {
        // The bounded event loop must return after routing the final allowed
        // CE frame. Its WMI payload remains queued for the next host drive.
        assert!(!control_budget_has_room(64, 64));
        assert!(control_budget_has_room(63, 64));
    }

    #[test]
    fn management_rx_rejects_corrupt_or_unrepresentable_status() {
        assert!(management_rx_status_accepted(0));
        assert!(management_rx_status_accepted(0x40));
        for status in [0x01, 0x08, 0x10, 0x20, 0x29] {
            assert!(!management_rx_status_accepted(status));
        }
    }

    #[test]
    fn wcn6750_event_pdev_maps_soc_and_first_hardware_radio() {
        assert!(wcn6750_event_pdev_is_primary(0));
        assert!(wcn6750_event_pdev_is_primary(1));
        assert!(!wcn6750_event_pdev_is_primary(2));
    }
}
