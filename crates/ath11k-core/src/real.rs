//! Concrete composition of QMI, CE/HTC, WMI, and HTT for WCN6750.

use crate::{CoreError, Operation, Subsystems, Wcn6750QmiSession, WlanEvent};
use alloc::vec::Vec;
use ath11k_ce::{
    BoundService, CE_COUNT, CeAllocatedPipes, CeCompletionWait, CePipes, CePipesPacketIo, Htc,
    HtcPacketIo, HtcRouter, HtcTransport, ServiceId, WCN6750_SERVICE_TO_PIPE,
};
use ath11k_dp::{
    HalDpRings,
    htt::request_target_version,
    transport::{HtcHttTransport, ath11k_dp_htt_connect_service},
    tx::{ClientDataPath, ClientTxConfig},
};
use ath11k_platform_backend::{Backend, Bidirectional, CoherentDma, Device, MmioRegion};
use ath11k_qmi::{
    FirmwareReady, Transport as QmiTransport,
    wire::{
        PipeDirection as QmiPipeDirection, ServicePipeConfig, TargetPipeConfig, WlanConfigRequest,
    },
};
use ath11k_wmi::{
    Command, Event, EventId, Transport as WmiTransport, WmiError,
    cmd::{
        HtcWmiTransport, Init, StaPowerSaveMode, StaPowerSaveParameter, TxRxStreams, VdevCreate,
        VdevSetParam, Wmi,
    },
};

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
            strict_passive: !scan.active,
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

/// Real subsystem owner. Every resource moves forward through an explicit
/// option; no raw descriptor, DMA address, or backend handle crosses this seam.
pub struct Wcn6750Subsystems<B, Q, A, M, W, D, S = NoWmiTrace>
where
    B: Backend,
    Q: QmiTransport,
    A: ath11k_qmi::FirmwareAssets,
    M: ath11k_qmi::MemoryProvider,
    W: CeCompletionWait,
    D: FnMut() -> u64,
    S: WmiTraceSink,
{
    qmi: Wcn6750QmiSession<Q, A, M>,
    device: Device<B>,
    waiter: Option<W>,
    dp_interrupts: crate::Wcn6750DpInterrupts<B>,
    mmio: Option<MmioRegion<B>>,
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
    service_ready: Option<ath11k_wmi::event::ServiceReadyState>,
}

impl<B, Q, A, M, W, D, S> Wcn6750Subsystems<B, Q, A, M, W, D, S>
where
    B: Backend,
    Q: QmiTransport,
    A: ath11k_qmi::FirmwareAssets,
    M: ath11k_qmi::MemoryProvider,
    W: CeCompletionWait,
    D: FnMut() -> u64,
    S: WmiTraceSink,
{
    pub fn new(
        qmi: Wcn6750QmiSession<Q, A, M>,
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
            service_ready: None,
        }
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
            .map_err(|_| CoreError::DeviceFault)?
            .ok_or(CoreError::DeviceFault)?;
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
            .map_err(|_| CoreError::DeviceFault)?;
        let response = self.receive_control()?;
        Self::protocol(self.htc.as_mut())?
            .connect_service(service, &response)
            .map_err(|_| CoreError::Protocol)?;
        Ok(())
    }

    fn pump(&mut self) -> Result<(), CoreError> {
        let deadline = (self.deadline)();
        Self::protocol(self.router.as_ref())?
            .service_receive(deadline)
            .map(|_| ())
            .map_err(|_| CoreError::DeviceFault)
    }

    fn wmi_send<R: ath11k_wmi::cmd::EncodeCommand>(
        &mut self,
        request: &R,
    ) -> Result<(), CoreError> {
        Self::protocol(self.wmi.as_mut())?
            .send(request)
            .map_err(|_| CoreError::Protocol)
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

impl<B, Q, A, M, W, D, S> Subsystems for Wcn6750Subsystems<B, Q, A, M, W, D, S>
where
    B: Backend,
    Q: QmiTransport,
    A: ath11k_qmi::FirmwareAssets,
    M: ath11k_qmi::MemoryProvider,
    W: CeCompletionWait,
    D: FnMut() -> u64,
    S: WmiTraceSink,
{
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
        self.qmi
            .wait_for_firmware_ready()
            .map_err(|_| CoreError::Protocol)
    }

    fn next_wlan_event(&mut self) -> Result<Option<WlanEvent>, CoreError> {
        use ath11k_wmi::event::{Decoder, EventDecoder as _, MgmtRx, Scan};
        use ath11k_wmi::tags::{WMI_MGMT_RX_EVENTID, WMI_SCAN_EVENTID};

        loop {
            let deadline = (self.deadline)();
            let mut event = Self::protocol(self.wmi.as_mut())?
                .next_event(deadline)
                .map_err(|_| CoreError::Protocol)?;
            if event.is_none() {
                self.pump()?;
                event = Self::protocol(self.wmi.as_mut())?
                    .next_event(deadline)
                    .map_err(|_| CoreError::Protocol)?;
            }
            let Some(event) = event else { return Ok(None) };
            let id = event.id;
            let decoded = match id {
                WMI_MGMT_RX_EVENTID => WlanEvent::from(
                    Decoder::<MgmtRx>::new(id)
                        .decode(event)
                        .map_err(|_| CoreError::Protocol)?,
                ),
                WMI_SCAN_EVENTID => WlanEvent::from(
                    Decoder::<Scan>::new(id)
                        .decode(event)
                        .map_err(|_| CoreError::Protocol)?,
                ),
                EventId(_) => continue,
            };
            return Ok(Some(decoded));
        }
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
            Operation::QmiInitService => self.qmi.init_service().map_err(|_| CoreError::Protocol),
            Operation::QmiFirmwareStart => self
                .qmi
                .firmware_start(&Self::qmi_config(), 0, false)
                .map_err(|_| CoreError::Protocol),
            Operation::QmiFirmwareStop => self.qmi.firmware_stop().map_err(|_| CoreError::Protocol),
            Operation::QmiDeinitService => {
                self.qmi.deinit_service();
                Ok(())
            }
            Operation::HifPowerUp => {
                self.mmio = Some(
                    self.device
                        .open_region(0)
                        .map_err(|_| CoreError::DeviceFault)?,
                );
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
            Operation::WmiWaitUnifiedReady => {
                self.pump()?;
                Self::protocol(self.wmi.as_mut())?
                    .wait_for_unified_ready((self.deadline)())
                    .map(|_| ())
                    .map_err(|_| CoreError::Protocol)
            }
            Operation::DpHttVersionRequest => {
                self.pump()?;
                let deadline = (self.deadline)();
                request_target_version(Self::protocol(self.htt.as_mut())?, deadline)
                    .map(|_| ())
                    .map_err(|_| CoreError::Protocol)
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
            Operation::WmiScanStart(scan) => self.wmi_send(&wcn6750_scan_start(scan)),
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
                let rings = HalDpRings::new(&self.device, &[]).map_err(Self::dp_error)?;
                let dp = ClientDataPath::ath11k_dp_alloc(
                    self.device.clone(),
                    rings,
                    ClientTxConfig::wcn6750_station(0),
                )
                .map_err(|error| {
                    error
                        .cleanup_error()
                        .map(Self::dp_error)
                        .unwrap_or_else(|| Self::dp_error(error.cause()))
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
    fn qmi_config_preserves_host_to_host_pipe_direction() {
        let config = Wcn6750Subsystems::<
            drv_hardware_backends::DeterministicBackend,
            DummyQmi,
            DummyAssets,
            DummyMemory,
            ath11k_ce::NoCompletionWait,
            fn() -> u64,
        >::qmi_config();
        assert!(config.target_pipes.unwrap().iter().any(|pipe| {
            pipe.pipe_num == 7 && pipe.direction == QmiPipeDirection::InOutHostToHost
        }));
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
    struct DummyMemory;
    impl ath11k_qmi::MemoryProvider for DummyMemory {
        fn provision(
            &mut self,
            _: &[ath11k_qmi::wire::MemorySegment],
        ) -> Result<Vec<ath11k_qmi::wire::MemorySegmentResponse>, ath11k_qmi::QmiError> {
            Ok(Vec::new())
        }
        fn load_m3(&mut self, _: &[u8]) -> Result<ath11k_qmi::MemoryRegion, ath11k_qmi::QmiError> {
            Err(ath11k_qmi::QmiError::Transport)
        }
        fn map_device_bar(&mut self, _: u64, _: u32) -> Result<(), ath11k_qmi::QmiError> {
            Ok(())
        }
    }
}
