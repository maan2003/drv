// SPDX-License-Identifier: GPL-2.0-only

//! Chip-independent ownership of the pinned Fuchsia client MLME/SME/RSN loop.

use crate::ethernet::{
    DriverEthernetPort, EthernetIngressError, HostEthernetDevice, ethernet_port,
};
use crate::{ClientRuntimeDriver, WlanSoftmac, WlanSoftmacLifecycle, WlanSoftmacUpcalls};
use fdf::ArenaStaticBox;
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_driver as fidl_driver;
use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
use fidl_fuchsia_wlan_mlme as fidl_mlme;
use fidl_fuchsia_wlan_sme as fidl_sme;
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use futures::channel::mpsc;
use futures::{FutureExt, Stream, StreamExt};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use wlan_mlme::MlmeImpl;
use wlan_mlme::device::{DeviceOps, LinkStatus};
use wlan_sme::Station;

const UPCALL_QUEUE_CAPACITY: usize = 256;
const ETHERNET_QUEUE_CAPACITY: usize = 256;

struct StartedDevice<D> {
    device: D,
    // A failed stop remains pending. Later explicit stop calls and Drop retry it.
    stop_pending: bool,
}

struct HostIo {
    ethernet: DriverEthernetPort,
    unpublished_ethernet_device: Option<HostEthernetDevice>,
    pending_ethernet_devices: VecDeque<HostEthernetDevice>,
    ethernet_mac_address: [u8; 6],
    ethernet_queue_capacity: usize,
    minstrel: Option<wlan_mlme::MinstrelWrapper>,
}

enum Upcall {
    Recv {
        bytes: Vec<u8>,
        info: fidl_softmac::WlanRxInfo,
    },
    TxResult(fidl_softmac::WlanTxResult),
    ScanComplete {
        status: zx::Status,
        scan_id: u64,
    },
}

struct UpcallQueue {
    live: bool,
    overflowed: bool,
    raw_queued: usize,
    queue: VecDeque<Upcall>,
}

/// All callbacks share one bounded ordered queue. Raw RX drops at capacity;
/// control callbacks evict the oldest raw frame, or latch a fatal overflow if
/// the queue contains only control callbacks.
struct UpcallSender(Arc<Mutex<UpcallQueue>>);

impl UpcallSender {
    fn push_control(&mut self, upcall: Upcall) {
        let mut state = self.0.lock().unwrap();
        if !state.live {
            return;
        }
        if state.queue.len() == UPCALL_QUEUE_CAPACITY {
            if let Some(index) = state
                .queue
                .iter()
                .position(|queued| matches!(queued, Upcall::Recv { .. }))
            {
                state.queue.remove(index);
                state.raw_queued -= 1;
            } else {
                state.live = false;
                state.overflowed = true;
                state.queue.clear();
                state.raw_queued = 0;
                return;
            }
        }
        state.queue.push_back(upcall);
    }
}

impl WlanSoftmacUpcalls for UpcallSender {
    fn recv(&mut self, bytes: Vec<u8>, info: fidl_softmac::WlanRxInfo) {
        let mut state = self.0.lock().unwrap();
        if state.live && state.queue.len() < UPCALL_QUEUE_CAPACITY {
            state.raw_queued += 1;
            state.queue.push_back(Upcall::Recv { bytes, info });
        }
    }

    fn report_tx_result(&mut self, result: fidl_softmac::WlanTxResult) {
        self.push_control(Upcall::TxResult(result));
    }

    fn notify_scan_complete(&mut self, status: zx::Status, scan_id: u64) {
        self.push_control(Upcall::ScanComplete { status, scan_id });
    }
}

fn revoke_and_drain(upcalls: &Mutex<UpcallQueue>) {
    let mut state = upcalls.lock().unwrap();
    state.live = false;
    state.queue.clear();
    state.raw_queued = 0;
}

fn drain_completed_attempt(upcalls: &Mutex<UpcallQueue>) -> bool {
    let mut state = upcalls.lock().unwrap();
    state.queue.clear();
    state.raw_queued = 0;
    !state.overflowed
}

fn sme_is_retry_quiescent(status: &wlan_sme::client::ClientSmeStatus) -> bool {
    matches!(status, wlan_sme::client::ClientSmeStatus::Idle)
}

fn stop_device<D: WlanSoftmacLifecycle>(
    device: &Mutex<StartedDevice<D>>,
) -> Result<(), zx::Status> {
    let mut state = device.lock().unwrap();
    if !state.stop_pending {
        return Ok(());
    }
    match state.device.stop() {
        Ok(()) => {
            state.stop_pending = false;
            Ok(())
        }
        Err(status) => Err(status),
    }
}

fn ethernet_status(error: EthernetIngressError) -> zx::Status {
    match error {
        EthernetIngressError::Closed => zx::Status::CANCELED,
        EthernetIngressError::LinkDown => zx::Status::BAD_STATE,
        EthernetIngressError::Backpressure => zx::Status::SHOULD_WAIT,
        EthernetIngressError::InvalidFrame(_) => zx::Status::IO_DATA_INTEGRITY,
    }
}

struct HostMlmeDevice<D> {
    device: Arc<Mutex<StartedDevice<D>>>,
    io: Arc<Mutex<HostIo>>,
    event_sink: mpsc::UnboundedSender<fidl_mlme::MlmeEvent>,
    event_stream: Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>,
}

impl<D> HostMlmeDevice<D> {
    fn new(device: Arc<Mutex<StartedDevice<D>>>, io: Arc<Mutex<HostIo>>) -> Self {
        let (event_sink, event_stream) = mpsc::unbounded();
        Self {
            device,
            io,
            event_sink,
            event_stream: Some(event_stream),
        }
    }
}

impl<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> DeviceOps for HostMlmeDevice<D> {
    async fn wlan_softmac_query_response(
        &mut self,
    ) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status> {
        self.device.lock().unwrap().device.query()
    }
    async fn discovery_support(&mut self) -> Result<fidl_softmac::DiscoverySupport, zx::Status> {
        self.device.lock().unwrap().device.query_discovery_support()
    }
    async fn mac_sublayer_support(
        &mut self,
    ) -> Result<fidl_common::MacSublayerSupport, zx::Status> {
        self.device
            .lock()
            .unwrap()
            .device
            .query_mac_sublayer_support()
    }
    async fn security_support(&mut self) -> Result<fidl_common::SecuritySupport, zx::Status> {
        self.device.lock().unwrap().device.query_security_support()
    }
    async fn spectrum_management_support(
        &mut self,
    ) -> Result<fidl_common::SpectrumManagementSupport, zx::Status> {
        self.device
            .lock()
            .unwrap()
            .device
            .query_spectrum_management_support()
    }
    fn deliver_eth_frame(&mut self, packet: &[u8]) -> Result<(), zx::Status> {
        self.io
            .lock()
            .unwrap()
            .ethernet
            .deliver(packet)
            .map_err(ethernet_status)
    }
    fn send_wlan_frame(
        &mut self,
        buffer: ArenaStaticBox<[u8]>,
        mut flags: fidl_softmac::WlanTxInfoFlags,
        _: Option<fuchsia_trace::Id>,
    ) -> Result<(), zx::Status> {
        if buffer.get(1).is_some_and(|byte| byte & 0x40 != 0) {
            flags |= fidl_softmac::WlanTxInfoFlags::PROTECTED;
        }
        self.device.lock().unwrap().device.queue_tx(&buffer, flags)
    }
    async fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
        if status != LinkStatus::UP {
            let mut io = self.io.lock().unwrap();
            io.pending_ethernet_devices.clear();
            io.unpublished_ethernet_device = None;
            io.ethernet.set_link(false);
            drop(io);
            return self.device.lock().unwrap().device.set_link_up(false);
        }

        let host = {
            let mut io = self.io.lock().unwrap();
            if io.ethernet.is_closed() {
                io.pending_ethernet_devices.clear();
                let (host, driver) =
                    ethernet_port(io.ethernet_mac_address, io.ethernet_queue_capacity)
                        .map_err(|_| zx::Status::NO_RESOURCES)?;
                io.ethernet = driver;
                Some(host)
            } else {
                io.unpublished_ethernet_device.take()
            }
        };
        if let Err(status) = self.device.lock().unwrap().device.set_link_up(true) {
            let mut io = self.io.lock().unwrap();
            io.pending_ethernet_devices.clear();
            io.unpublished_ethernet_device = None;
            io.ethernet.teardown();
            return Err(status);
        }
        let mut io = self.io.lock().unwrap();
        io.ethernet.set_link(true);
        if let Some(host) = host {
            io.pending_ethernet_devices.clear();
            io.pending_ethernet_devices.push_back(host);
        }
        Ok(())
    }
    async fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        self.device.lock().unwrap().device.set_channel(
            fidl_softmac::WlanSoftmacBaseSetChannelRequest {
                primary: Some(primary),
                bandwidth: Some(bandwidth),
                vht_secondary_80_channel: Some(secondary),
            },
        )
    }
    async fn set_mac_address(&mut self, _: [u8; 6]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    async fn start_passive_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        self.device
            .lock()
            .unwrap()
            .device
            .start_passive_scan(request.clone())
    }
    async fn start_active_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
        self.device
            .lock()
            .unwrap()
            .device
            .start_active_scan(request.clone())
    }
    async fn cancel_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        self.device
            .lock()
            .unwrap()
            .device
            .cancel_scan(request.clone())
    }
    async fn join_bss(&mut self, request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        self.device.lock().unwrap().device.join_bss(request.clone())
    }
    async fn enable_beaconing(
        &mut self,
        _: fidl_softmac::WlanSoftmacBaseEnableBeaconingRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    async fn disable_beaconing(&mut self) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    async fn install_key(
        &mut self,
        key: &fidl_softmac::WlanKeyConfiguration,
    ) -> Result<(), zx::Status> {
        self.device.lock().unwrap().device.install_key(key.clone())
    }
    async fn notify_association_complete(
        &mut self,
        config: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        self.device
            .lock()
            .unwrap()
            .device
            .notify_association_complete(config)
    }
    async fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        self.device
            .lock()
            .unwrap()
            .device
            .clear_association(request.clone())
    }
    async fn update_wmm_parameters(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        self.device
            .lock()
            .unwrap()
            .device
            .update_wmm_parameters(request.clone())
    }
    fn take_mlme_event_stream(&mut self) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
        self.event_stream.take()
    }
    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
        self.event_sink.unbounded_send(event).map_err(Into::into)
    }
    fn set_minstrel(&mut self, minstrel: wlan_mlme::MinstrelWrapper) {
        self.io.lock().unwrap().minstrel = Some(minstrel);
    }
    fn minstrel(&mut self) -> Option<wlan_mlme::MinstrelWrapper> {
        self.io.lock().unwrap().minstrel.clone()
    }
}

fn sae_group(frame: &fidl_mlme::SaeFrame) -> Option<u16> {
    (frame.seq_num == 1 && frame.sae_fields.len() >= 2)
        .then(|| u16::from_le_bytes([frame.sae_fields[0], frame.sae_fields[1]]))
}

type SmeTimerAction = Box<dyn FnOnce(&mut wlan_sme::client::ClientSme)>;
type MlmeTimerAction = wlan_mlme::common::timer::Event<wlan_mlme::client::TimedEvent>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectError {
    Timeout,
    /// The exact terminal result reported by SME. Policy needs the credential
    /// classification to decide whether a fresh attempt is permitted.
    Failed(fidl_sme::ConnectResult),
    Driver(DriverError),
    Containment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DriverError {
    MlmeRequest { name: &'static str, detail: String },
    ClientRx(zx::Status),
    Ethernet(zx::Status),
    RequestStreamClosed,
    EventStreamClosed,
    ConnectTransactionClosed,
    ConnectStateMismatch,
    AlreadyConnected,
    ConnectInProgress,
    NoConnectInProgress,
    RetryCleanup,
    ControlBudgetExhausted,
    Stopped,
    UpcallOverflow,
}

struct ConnectAttempt {
    transaction: wlan_sme::client::ConnectTransactionStream,
    deadline: std::time::Instant,
}

/// Bounded production owner for SME, MLME, RSN, timers, device events, and
/// chip-supplied RX. No parallel association state is attached to this owner.
pub struct ClientRuntime<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> {
    device: Arc<Mutex<StartedDevice<D>>>,
    upcalls: Arc<Mutex<UpcallQueue>>,
    io: Arc<Mutex<HostIo>>,
    sme: wlan_sme::client::ClientSme,
    mlme: wlan_mlme::client::ClientMlme<HostMlmeDevice<D>>,
    requests: wlan_sme::MlmeStream,
    events: mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>,
    sme_timers: Pin<Box<dyn Stream<Item = SmeTimerAction>>>,
    mlme_timers: Pin<Box<dyn Stream<Item = MlmeTimerAction>>>,
    timer_runtime: tokio::runtime::Runtime,
    connect_attempt: Option<ConnectAttempt>,
    connection: Option<wlan_sme::client::ConnectTransactionStream>,
    revoked: bool,
}

impl<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> ClientRuntime<D> {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        device: D,
        sme_config: wlan_sme::client::ClientConfig,
        device_info: fidl_mlme::DeviceInfo,
        security: fidl_common::SecuritySupport,
        spectrum: fidl_common::SpectrumManagementSupport,
        inspector: fuchsia_inspect::Inspector,
    ) -> Result<Self, anyhow::Error> {
        Self::new_with_ethernet_capacity(
            device,
            sme_config,
            device_info,
            security,
            spectrum,
            inspector,
            ETHERNET_QUEUE_CAPACITY,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn new_with_ethernet_capacity(
        device: D,
        sme_config: wlan_sme::client::ClientConfig,
        device_info: fidl_mlme::DeviceInfo,
        security: fidl_common::SecuritySupport,
        spectrum: fidl_common::SpectrumManagementSupport,
        inspector: fuchsia_inspect::Inspector,
        ethernet_queue_capacity: usize,
    ) -> Result<Self, anyhow::Error> {
        let timer_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let (ethernet_device, ethernet) =
            ethernet_port(device_info.sta_addr, ethernet_queue_capacity)
                .map_err(|error| anyhow::anyhow!("invalid host Ethernet endpoint: {error:?}"))?;
        let upcalls = Arc::new(Mutex::new(UpcallQueue {
            live: true,
            overflowed: false,
            raw_queued: 0,
            queue: VecDeque::new(),
        }));
        let device = Arc::new(Mutex::new(StartedDevice {
            device,
            stop_pending: false,
        }));
        let io = Arc::new(Mutex::new(HostIo {
            ethernet,
            unpublished_ethernet_device: Some(ethernet_device),
            pending_ethernet_devices: VecDeque::new(),
            ethernet_mac_address: device_info.sta_addr,
            ethernet_queue_capacity,
            minstrel: None,
        }));
        let mut mlme_device = HostMlmeDevice::new(device.clone(), io.clone());
        let events = mlme_device
            .take_mlme_event_stream()
            .ok_or_else(|| anyhow::anyhow!("MLME event stream was already taken"))?;
        let (mlme_timer, mlme_timer_stream) = wlan_mlme::common::timer::create_timer();
        let mlme =
            wlan_mlme::client::ClientMlme::new(Default::default(), mlme_device, mlme_timer).await?;
        let (sme, _sink, requests, sme_timer_stream) = wlan_sme::client::ClientSme::new(
            sme_config,
            device_info,
            inspector.clone(),
            inspector.root().create_child("sme"),
            security,
            spectrum,
        );
        let sme_timers = Box::pin(
            wlan_mlme::common::timer::make_async_timed_event_stream(sme_timer_stream).map(
                |event| {
                    Box::new(move |sme: &mut wlan_sme::client::ClientSme| {
                        Station::on_timeout(sme, event)
                    }) as SmeTimerAction
                },
            ),
        );
        let mlme_timers = Box::pin(wlan_mlme::common::timer::make_async_timed_event_stream(
            mlme_timer_stream,
        ));
        {
            let mut state = device.lock().unwrap();
            if let Err(status) = state.device.start(Box::new(UpcallSender(upcalls.clone()))) {
                revoke_and_drain(&upcalls);
                return Err(anyhow::anyhow!("SoftMAC start failed: {status}"));
            }
            state.stop_pending = true;
        }
        Ok(Self {
            device,
            upcalls,
            io,
            sme,
            mlme,
            requests,
            events,
            sme_timers,
            mlme_timers,
            timer_runtime,
            connect_attempt: None,
            connection: None,
            revoked: false,
        })
    }

    pub fn sme(&self) -> &wlan_sme::client::ClientSme {
        &self.sme
    }

    /// Transfers the next Ethernet generation to the network service.
    /// Link-down revokes the transferred descriptor with HUP; a later link-up
    /// publishes a fresh descriptor while the runtime retains its driver peer.
    pub fn take_ethernet_device(&mut self) -> Option<HostEthernetDevice> {
        self.io.lock().unwrap().pending_ethernet_devices.pop_front()
    }

    /// Revoke callbacks and queues before tearing down Ethernet and stopping
    /// the device. If device stop fails, the runtime stays callback-revoked
    /// and a later call retries only the device stop operation.
    pub fn stop(&mut self) -> Result<(), zx::Status> {
        self.revoked = true;
        self.connect_attempt = None;
        self.connection = None;
        revoke_and_drain(&self.upcalls);
        self.io.lock().unwrap().ethernet.teardown();
        stop_device(&self.device)
    }

    async fn pump_upcalls(&mut self) -> Result<bool, ConnectError> {
        const UPCALL_BUDGET: usize = 64;
        let mut progressed = false;
        for _ in 0..UPCALL_BUDGET {
            if self.upcalls.lock().unwrap().overflowed {
                let _ = self.stop();
                return Err(ConnectError::Driver(DriverError::UpcallOverflow));
            }
            let upcall = {
                let mut state = self.upcalls.lock().unwrap();
                let upcall = state.queue.pop_front();
                if matches!(upcall, Some(Upcall::Recv { .. })) {
                    state.raw_queued -= 1;
                }
                upcall
            };
            match upcall {
                Some(Upcall::ScanComplete { status, scan_id }) => {
                    MlmeImpl::handle_scan_complete(&mut self.mlme, status, scan_id).await;
                }
                Some(Upcall::TxResult(result)) => {
                    if let Some(minstrel) = self.io.lock().unwrap().minstrel.clone() {
                        minstrel.lock().handle_tx_result_report(&result);
                    }
                }
                Some(Upcall::Recv { bytes, info }) => {
                    let auth = safe_auth_stage(&bytes);
                    let eapol = bytes
                        .windows(8)
                        .any(|window| window == [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
                    MlmeImpl::handle_mac_frame_rx(
                        &mut self.mlme,
                        &bytes,
                        info,
                        fuchsia_trace::Id::new(),
                    )
                    .await;
                    if let Some((algorithm, transaction, status, rejected_group)) = auth {
                        println!(
                            "client_mlme_rx stage=handle_complete algorithm={algorithm} transaction={transaction} status={status} rejected_group={rejected_group:?}"
                        );
                    }
                    if eapol {
                        println!("client_eapol_stage=mlme_handle_complete");
                    }
                }
                None => break,
            }
            progressed = true;
        }
        Ok(progressed)
    }

    async fn drain_control(&mut self, budget: usize) -> Result<(bool, bool), ConnectError> {
        let mut progressed = false;
        for _ in 0..budget {
            let mut cycle_progressed = false;
            match self.requests.try_recv() {
                Ok(request) => {
                    let sae_frame_tx = matches!(&request, wlan_sme::MlmeRequest::SaeFrameTx(_));
                    let eapol_tx = matches!(&request, wlan_sme::MlmeRequest::Eapol(_));
                    if let wlan_sme::MlmeRequest::SaeFrameTx(frame) = &request {
                        println!(
                            "client_sae_stage=sme_sae_frame_tx transaction={} status={} group={:?}",
                            frame.seq_num,
                            frame.status_code.into_primitive(),
                            sae_group(frame)
                        );
                    }
                    if eapol_tx {
                        println!("client_eapol_stage=sme_tx_request");
                    }
                    let name = request.name();
                    // Diagnostic: surface every MLME request the SME issues so the
                    // post-4-way sequence (SetKeys GTK/IGTK, SetCtrlPort, Deauth) is
                    // visible when the connect fails after PTK.
                    println!("client_mlme_request name={name}");
                    wlan_mlme::MlmeImpl::handle_mlme_request(&mut self.mlme, request)
                        .await
                        .map_err(|error| {
                            ConnectError::Driver(DriverError::MlmeRequest {
                                name,
                                // Pinned MLME errors contain status/contract names,
                                // never request frame or credential bytes.
                                detail: error.to_string(),
                            })
                        })?;
                    if sae_frame_tx {
                        println!(
                            "client_sae_stage=mlme_request_complete state={}",
                            self.mlme.sae_state_name()
                        );
                    }
                    if eapol_tx {
                        println!("client_eapol_stage=mlme_tx_request_complete");
                    }
                    progressed = true;
                    cycle_progressed = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Closed) => {
                    return Err(ConnectError::Driver(DriverError::RequestStreamClosed));
                }
            }
            match self.events.try_recv() {
                Ok(event) => {
                    if let fidl_mlme::MlmeEvent::OnSaeFrameRx { frame } = &event {
                        println!(
                            "client_sae_stage=mlme_sae_frame_rx algorithm=3 transaction={} status={} group={:?}",
                            frame.seq_num,
                            frame.status_code.into_primitive(),
                            sae_group(frame)
                        );
                    }
                    let eapol_ind = matches!(&event, fidl_mlme::MlmeEvent::EapolInd { .. });
                    if let fidl_mlme::MlmeEvent::EapolConf { resp } = &event {
                        // MLME never propagates a failed EAPOL send as an
                        // error; it only reports it here. Surface it so a
                        // rejected M2 cannot hide behind a successful request.
                        println!(
                            "client_eapol_stage=mlme_eapol_confirm result={:?}",
                            resp.result_code
                        );
                    }
                    if eapol_ind {
                        println!(
                            "client_eapol_stage=mlme_indication_forwarded_to_sme controlled_port_closed_allowed=true"
                        );
                    }
                    Station::on_mlme_event(&mut self.sme, event);
                    progressed = true;
                    cycle_progressed = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Closed) => {
                    return Err(ConnectError::Driver(DriverError::EventStreamClosed));
                }
            }
            if !cycle_progressed {
                return Ok((progressed, true));
            }
        }
        Ok((progressed, false))
    }

    async fn pump_once(&mut self) -> Result<bool, ConnectError> {
        const CONTROL_BUDGET: usize = 64;

        if self.upcalls.lock().unwrap().overflowed {
            let _ = self.stop();
            return Err(ConnectError::Driver(DriverError::UpcallOverflow));
        }
        let (mut progressed, mut control_quiescent) = self.drain_control(CONTROL_BUDGET).await?;
        if let Some(action) = self.timer_runtime.block_on(async {
            tokio::task::yield_now().await;
            self.sme_timers.as_mut().next().now_or_never().flatten()
        }) {
            action(&mut self.sme);
            progressed = true;
            control_quiescent = false;
        }
        if let Some(event) = self.timer_runtime.block_on(async {
            tokio::task::yield_now().await;
            self.mlme_timers.as_mut().next().now_or_never().flatten()
        }) {
            println!(
                "client_mlme_timer stage=stream_dequeued timer_id={} event={:?}",
                event.id, event.event
            );
            wlan_mlme::MlmeImpl::handle_timeout(&mut self.mlme, event.event).await;
            progressed = true;
            control_quiescent = false;
        }

        if !control_quiescent {
            let (control_progressed, quiescent) = self.drain_control(CONTROL_BUDGET).await?;
            progressed |= control_progressed;
            control_quiescent = quiescent;
        }
        if !control_quiescent {
            println!(
                "client_runtime_control stage=budget_exhausted budget={CONTROL_BUDGET} rx_dequeued=false"
            );
            return Err(ConnectError::Driver(DriverError::ControlBudgetExhausted));
        }

        let device_progressed = self
            .device
            .lock()
            .unwrap()
            .device
            .drive()
            .map_err(|status| ConnectError::Driver(DriverError::ClientRx(status)))?;
        let upcall_progressed = self.pump_upcalls().await?;
        progressed |= device_progressed || upcall_progressed;
        if device_progressed || upcall_progressed {
            let (_, quiescent) = self.drain_control(CONTROL_BUDGET).await?;
            if !quiescent {
                println!(
                    "client_runtime_control stage=post_rx_budget_exhausted budget={CONTROL_BUDGET} rx_dequeued=false"
                );
                return Err(ConnectError::Driver(DriverError::ControlBudgetExhausted));
            }
        }
        Ok(progressed)
    }

    /// Advance post-association SME/MLME control, timers, hardware RX, and one
    /// driver-bound Ethernet frame. No backend lock is held across MLME TX.
    pub async fn pump_associated_once(&mut self) -> Result<bool, ConnectError> {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        let mut progressed = self.pump_once().await?;
        let frame = self
            .io
            .lock()
            .unwrap()
            .ethernet
            .take_transmit()
            .map_err(|error| {
                let status = ethernet_status(error);
                println!("client_data_seam_error direction=netstack_to_driver status={status}");
                ConnectError::Driver(DriverError::Ethernet(status))
            })?;
        if let Some(frame) = frame {
            if let Err(error) = wlan_mlme::MlmeImpl::handle_eth_frame_tx(
                &mut self.mlme,
                frame.as_bytes(),
                fuchsia_trace::Id::new(),
            ) {
                println!(
                    "client_data_tx_error stage=ethernet_pump kind=target_rejected error={error}"
                );
            }
            progressed = true;
        }
        Ok(progressed)
    }

    pub async fn connect(
        &mut self,
        request: fidl_sme::ConnectRequest,
        deadline: std::time::Instant,
    ) -> Result<fidl_sme::ConnectResult, ConnectError> {
        self.begin_connect(request, deadline)?;
        loop {
            if let Some(result) = self.drive_connect_once().await? {
                return Ok(result);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Start one policy-selected connect attempt while retaining its SME
    /// transaction in the runtime. This permits a service loop to keep
    /// processing control requests without dropping an in-flight attempt.
    pub fn begin_connect(
        &mut self,
        request: fidl_sme::ConnectRequest,
        deadline: std::time::Instant,
    ) -> Result<(), ConnectError> {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        if self.connection.is_some() {
            return Err(ConnectError::Driver(DriverError::AlreadyConnected));
        }
        if self.connect_attempt.is_some() {
            return Err(ConnectError::Driver(DriverError::ConnectInProgress));
        }
        self.connect_attempt = Some(ConnectAttempt {
            transaction: self.sme.on_connect_command(request),
            deadline,
        });
        Ok(())
    }

    /// Advance the retained connect attempt once. `Ok(None)` means the
    /// service should continue driving it; a returned error has already
    /// completed retry cleanup or terminal containment as appropriate.
    pub async fn drive_connect_once(
        &mut self,
    ) -> Result<Option<fidl_sme::ConnectResult>, ConnectError> {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        if self.connect_attempt.is_none() {
            return Err(ConnectError::Driver(DriverError::NoConnectInProgress));
        }
        match self.drive_connect_once_inner().await {
            Ok(result) => Ok(result),
            Err(error) => Err(self.finish_connect_error(error)),
        }
    }

    /// Request a policy-owned disconnect and drive the pinned SME/MLME until
    /// it reaches Idle. The retained transaction still carries the resulting
    /// `OnDisconnect` event for the policy service to consume.
    pub async fn disconnect(
        &mut self,
        reason: fidl_sme::UserDisconnectReason,
        deadline: std::time::Instant,
    ) -> Result<(), ConnectError> {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        if self.connect_attempt.is_some() {
            return Err(ConnectError::Driver(DriverError::ConnectInProgress));
        }
        if self.connection.is_none() && sme_is_retry_quiescent(&self.sme.status()) {
            return Ok(());
        }
        self.sme.on_disconnect_command(reason, Default::default());
        let result = loop {
            if std::time::Instant::now() >= deadline {
                break Err(ConnectError::Timeout);
            }
            let progressed = match self.pump_once().await {
                Ok(progressed) => progressed,
                Err(error) => break Err(error),
            };
            if sme_is_retry_quiescent(&self.sme.status()) {
                break Ok(());
            }
            if !progressed {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) if !self.revoked => Err(self.contain_error(error)),
            Err(error) => Err(error),
        }
    }

    /// Cancel an in-flight connect without dropping its transaction. Success
    /// is acknowledged only after SME Idle and either its terminal result or
    /// the transaction closure used by SME for command cancellation are
    /// observed; ambiguous termination is contained.
    pub async fn cancel_connect(
        &mut self,
        reason: fidl_sme::UserDisconnectReason,
        deadline: std::time::Instant,
    ) -> Result<fidl_sme::ConnectResult, ConnectError> {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        if self.connect_attempt.is_none() {
            return Err(ConnectError::Driver(DriverError::NoConnectInProgress));
        }
        self.sme.on_disconnect_command(reason, Default::default());
        let mut terminal = None;
        let mut transaction_closed = false;
        let result = loop {
            if std::time::Instant::now() >= deadline {
                break Err(ConnectError::Timeout);
            }
            let progressed = match self.pump_once().await {
                Ok(progressed) => progressed,
                Err(error) => break Err(error),
            };
            loop {
                let event = self
                    .connect_attempt
                    .as_mut()
                    .expect("connect attempt checked above")
                    .transaction
                    .try_recv();
                match event {
                    Ok(wlan_sme::client::ConnectTransactionEvent::OnConnectResult {
                        result,
                        is_reconnect,
                    }) => {
                        terminal = Some(
                            wlan_sme::client::ConnectTransactionEvent::OnConnectResult {
                                result,
                                is_reconnect,
                            }
                            .into_fidl(),
                        );
                    }
                    Ok(_) => {}
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Closed) => {
                        transaction_closed = true;
                        break;
                    }
                }
            }
            if (terminal.is_some() || transaction_closed)
                && sme_is_retry_quiescent(&self.sme.status())
            {
                break Ok(());
            }
            if !progressed {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        if let Err(error) = result {
            return Err(if self.revoked {
                error
            } else {
                self.contain_error(error)
            });
        }
        self.connect_attempt = None;
        self.io.lock().unwrap().ethernet.set_link(false);
        if !self.finish_failed_attempt_cleanup() {
            return Err(self.contain_error(ConnectError::Driver(DriverError::RetryCleanup)));
        }
        let terminal = terminal.unwrap_or(fidl_sme::ConnectTransactionEvent::OnConnectResult {
            result: fidl_sme::ConnectResult {
                code: fidl_ieee80211::StatusCode::Canceled,
                is_credential_rejected: false,
                is_reconnect: false,
            },
        });
        let fidl_sme::ConnectTransactionEvent::OnConnectResult { result } = terminal else {
            unreachable!("only connect results are retained as terminal")
        };
        Ok(result)
    }

    async fn drive_connect_once_inner(
        &mut self,
    ) -> Result<Option<fidl_sme::ConnectResult>, ConnectError> {
        if std::time::Instant::now()
            >= self
                .connect_attempt
                .as_ref()
                .expect("connect attempt checked by caller")
                .deadline
        {
            return Err(ConnectError::Timeout);
        }
        self.pump_once().await?;
        loop {
            let event = self
                .connect_attempt
                .as_mut()
                .expect("connect attempt checked by caller")
                .transaction
                .try_recv();
            match event {
                Ok(event @ wlan_sme::client::ConnectTransactionEvent::OnConnectResult { .. }) => {
                    let fidl_sme::ConnectTransactionEvent::OnConnectResult { result } =
                        event.into_fidl()
                    else {
                        unreachable!("matched connect result")
                    };
                    if result.code != fidl_ieee80211::StatusCode::Success {
                        return Err(ConnectError::Failed(result));
                    }
                    self.pump_once().await?;
                    if !self.sme.status().is_connected() {
                        return Err(ConnectError::Driver(DriverError::ConnectStateMismatch));
                    }
                    let attempt = self
                        .connect_attempt
                        .take()
                        .expect("connect attempt retained");
                    self.connection = Some(attempt.transaction);
                    return Ok(Some(result));
                }
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Closed) => {
                    return Err(ConnectError::Driver(DriverError::ConnectTransactionClosed));
                }
            }
        }
    }

    fn finish_connect_error(&mut self, error: ConnectError) -> ConnectError {
        self.connect_attempt = None;
        if self.revoked {
            return error;
        }
        if matches!(error, ConnectError::Failed(_)) && self.finish_failed_attempt_cleanup() {
            return error;
        }
        let error = if matches!(error, ConnectError::Failed(_)) {
            ConnectError::Driver(DriverError::RetryCleanup)
        } else {
            error
        };
        self.contain_error(error)
    }

    fn finish_failed_attempt_cleanup(&mut self) -> bool {
        // A completed SME failure is retryable only when both owners can
        // prove quiescence. Keep the data plane closed before asking the
        // device to revoke and drain its attempt, then discard callbacks
        // that raced with that device-side drain.
        self.io.lock().unwrap().ethernet.set_link(false);
        let sme_quiescent = sme_is_retry_quiescent(&self.sme.status());
        let device_quiescent = sme_quiescent
            && self
                .device
                .lock()
                .unwrap()
                .device
                .finish_failed_connect_attempt()
                .is_ok();
        device_quiescent && drain_completed_attempt(&self.upcalls)
    }

    fn contain_error(&mut self, error: ConnectError) -> ConnectError {
        self.revoked = true;
        self.connect_attempt = None;
        self.connection = None;
        revoke_and_drain(&self.upcalls);
        self.io.lock().unwrap().ethernet.teardown();
        match self.device.lock().unwrap().device.reset() {
            Ok(()) => error,
            Err(_) => ConnectError::Containment,
        }
    }

    /// Pop one retained post-connect SME event. Driving hardware and protocol
    /// progress remains explicit through [`Self::pump_associated_once`].
    pub fn next_connection_event(
        &mut self,
    ) -> Result<Option<fidl_sme::ConnectTransactionEvent>, ConnectError> {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        let Some(connection) = self.connection.as_mut() else {
            return Ok(None);
        };
        match connection.try_recv() {
            Ok(event) => {
                if matches!(
                    &event,
                    wlan_sme::client::ConnectTransactionEvent::OnDisconnect { info }
                        if !info.is_sme_reconnecting
                ) {
                    self.connection = None;
                }
                Ok(Some(event.into_fidl()))
            }
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Closed) => {
                self.connection = None;
                Err(ConnectError::Driver(DriverError::ConnectTransactionClosed))
            }
        }
    }
}

fn safe_auth_stage(bytes: &[u8]) -> Option<(u16, u16, u16, Option<u16>)> {
    let control = u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?);
    if control & 0x00fc != 0x00b0 {
        return None;
    }
    let algorithm = u16::from_le_bytes(bytes.get(24..26)?.try_into().ok()?);
    let transaction = u16::from_le_bytes(bytes.get(26..28)?.try_into().ok()?);
    let status = u16::from_le_bytes(bytes.get(28..30)?.try_into().ok()?);
    let rejected_group = if status == 77 {
        Some(u16::from_le_bytes(bytes.get(30..32)?.try_into().ok()?))
    } else {
        None
    };
    Some((algorithm, transaction, status, rejected_group))
}

impl<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> Drop for ClientRuntime<D> {
    fn drop(&mut self) {
        // A preceding failed explicit stop is retried once here. There is no
        // callback or queue reactivation between attempts.
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Effects {
        calls: Vec<&'static str>,
        upcalls: Option<Box<dyn WlanSoftmacUpcalls>>,
        stop_failures: usize,
        reset_failure: bool,
        query_failure: bool,
        tx_flags: Vec<fidl_softmac::WlanTxInfoFlags>,
        simulate_ap: bool,
        suppress_auth_response: bool,
        reject_next_auth: bool,
        pending_rx: VecDeque<Vec<u8>>,
        retry_cleanup: bool,
        stale_callback_during_cleanup: bool,
        link_failure: bool,
    }

    #[derive(Clone)]
    struct Fake(Arc<Mutex<Effects>>);

    impl Fake {
        fn new(stop_failures: usize) -> (Self, Arc<Mutex<Effects>>) {
            let effects = Arc::new(Mutex::new(Effects {
                stop_failures,
                ..Default::default()
            }));
            (Self(effects.clone()), effects)
        }
    }

    impl WlanSoftmacLifecycle for Fake {
        fn start(&mut self, upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status> {
            let mut effects = self.0.lock().unwrap();
            effects.calls.push("start");
            effects.upcalls = Some(upcalls);
            Ok(())
        }

        fn stop(&mut self) -> Result<(), zx::Status> {
            let mut effects = self.0.lock().unwrap();
            effects.calls.push("stop");
            if effects.stop_failures != 0 {
                effects.stop_failures -= 1;
                Err(zx::Status::IO)
            } else {
                Ok(())
            }
        }
    }

    impl ClientRuntimeDriver for Fake {
        fn drive(&mut self) -> Result<bool, zx::Status> {
            let mut effects = self.0.lock().unwrap();
            let Some(bytes) = effects.pending_rx.pop_front() else {
                return Ok(false);
            };
            effects.upcalls.as_mut().unwrap().recv(bytes, rx_info());
            Ok(true)
        }
        fn set_link_up(&mut self, _: bool) -> Result<(), zx::Status> {
            if self.0.lock().unwrap().link_failure {
                Err(zx::Status::IO)
            } else {
                Ok(())
            }
        }
        fn finish_failed_connect_attempt(&mut self) -> Result<(), zx::Status> {
            let mut effects = self.0.lock().unwrap();
            effects.calls.push("finish_failed_connect_attempt");
            if !effects.retry_cleanup {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            if effects.stale_callback_during_cleanup {
                effects
                    .upcalls
                    .as_mut()
                    .unwrap()
                    .recv(auth_response(0), rx_info());
                effects
                    .upcalls
                    .as_mut()
                    .unwrap()
                    .recv(stale_data_frame(), rx_info());
            }
            Ok(())
        }
        fn reset(&mut self) -> Result<(), zx::Status> {
            let mut effects = self.0.lock().unwrap();
            effects.calls.push("reset");
            if effects.reset_failure {
                Err(zx::Status::IO)
            } else {
                Ok(())
            }
        }
    }

    macro_rules! record {
        ($self:ident, $name:literal, $value:expr) => {{
            $self.0.lock().unwrap().calls.push($name);
            Ok($value)
        }};
    }

    impl WlanSoftmac for Fake {
        fn query(&mut self) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status> {
            if self.0.lock().unwrap().query_failure {
                self.0.lock().unwrap().calls.push("query");
                return Err(zx::Status::IO);
            }
            record!(
                self,
                "query",
                fidl_softmac::WlanSoftmacQueryResponse {
                    sta_addr: Some([2, 0, 0, 0, 0, 1]),
                    factory_addr: Some([2, 0, 0, 0, 0, 1]),
                    mac_role: Some(fidl_common::WlanMacRole::Client),
                    hardware_capability: Some(0),
                    band_caps: Some(vec![fidl_softmac::WlanSoftmacBandCapability {
                        band: Some(fidl_ieee80211::WlanBand::TwoGhz),
                        basic_rates: Some(vec![0x82, 0x84]),
                        primary_channels: Some(vec![wlan_channel()]),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }
            )
        }
        fn query_discovery_support(
            &mut self,
        ) -> Result<fidl_softmac::DiscoverySupport, zx::Status> {
            record!(self, "discovery", Default::default())
        }
        fn query_mac_sublayer_support(
            &mut self,
        ) -> Result<fidl_common::MacSublayerSupport, zx::Status> {
            record!(self, "mac", Default::default())
        }
        fn query_security_support(&mut self) -> Result<fidl_common::SecuritySupport, zx::Status> {
            record!(self, "security", Default::default())
        }
        fn query_spectrum_management_support(
            &mut self,
        ) -> Result<fidl_common::SpectrumManagementSupport, zx::Status> {
            record!(self, "spectrum", Default::default())
        }
        fn set_channel(
            &mut self,
            _: fidl_softmac::WlanSoftmacBaseSetChannelRequest,
        ) -> Result<(), zx::Status> {
            record!(self, "channel", ())
        }
        fn join_bss(&mut self, _: fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
            record!(self, "join", ())
        }
        fn install_key(&mut self, _: fidl_softmac::WlanKeyConfiguration) -> Result<(), zx::Status> {
            record!(self, "key", ())
        }
        fn notify_association_complete(
            &mut self,
            _: fidl_softmac::WlanAssociationConfig,
        ) -> Result<(), zx::Status> {
            record!(self, "assoc", ())
        }
        fn clear_association(
            &mut self,
            _: fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
        ) -> Result<(), zx::Status> {
            record!(self, "clear", ())
        }
        fn start_passive_scan(
            &mut self,
            _: fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
        ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
            record!(self, "passive", Default::default())
        }
        fn start_active_scan(
            &mut self,
            _: fidl_softmac::WlanSoftmacStartActiveScanRequest,
        ) -> Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
            record!(self, "active", Default::default())
        }
        fn cancel_scan(
            &mut self,
            _: fidl_softmac::WlanSoftmacBaseCancelScanRequest,
        ) -> Result<(), zx::Status> {
            record!(self, "cancel", ())
        }
        fn update_wmm_parameters(
            &mut self,
            _: fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
        ) -> Result<(), zx::Status> {
            record!(self, "wmm", ())
        }
        fn queue_tx(
            &mut self,
            bytes: &[u8],
            flags: fidl_softmac::WlanTxInfoFlags,
        ) -> Result<(), zx::Status> {
            let mut effects = self.0.lock().unwrap();
            if effects.simulate_ap {
                match bytes.first().copied() {
                    Some(0xb0) => {
                        let status = if effects.reject_next_auth {
                            effects.reject_next_auth = false;
                            1
                        } else {
                            0
                        };
                        if !effects.suppress_auth_response {
                            effects.pending_rx.push_back(auth_response(status));
                        }
                    }
                    Some(0x00) => effects.pending_rx.push_back(association_response()),
                    Some(0xc0) => {}
                    _ => return Err(zx::Status::NOT_SUPPORTED),
                }
            } else {
                assert_eq!(bytes, [1, 0x40, 3]);
            }
            effects.tx_flags.push(flags);
            effects.calls.push("tx");
            Ok(())
        }
    }

    fn parts(fake: Fake) -> (HostMlmeDevice<Fake>, Arc<Mutex<Effects>>) {
        let effects = fake.0.clone();
        let device = Arc::new(Mutex::new(StartedDevice {
            device: fake,
            stop_pending: true,
        }));
        let (_, ethernet) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        let io = Arc::new(Mutex::new(HostIo {
            ethernet,
            unpublished_ethernet_device: None,
            pending_ethernet_devices: VecDeque::new(),
            ethernet_mac_address: [2, 0, 0, 0, 0, 1],
            ethernet_queue_capacity: 4,
            minstrel: None,
        }));
        (HostMlmeDevice::new(device, io), effects)
    }

    fn wlan_channel() -> fidl_ieee80211::ChannelNumber {
        fidl_ieee80211::ChannelNumber {
            band: fidl_ieee80211::WlanBand::TwoGhz,
            number: 1,
        }
    }

    fn rx_info() -> fidl_softmac::WlanRxInfo {
        fidl_softmac::WlanRxInfo {
            rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
            valid_fields: fidl_softmac::WlanRxInfoValid::empty(),
            phy: fidl_ieee80211::WlanPhyType::Dsss,
            data_rate: 0,
            primary: wlan_channel(),
            bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw20,
            vht_secondary_80_channel: wlan_channel(),
            mcs: 0,
            rssi_dbm: 0,
            snr_dbh: 0,
        }
    }

    fn auth_response(status: u16) -> Vec<u8> {
        let client = [2, 0, 0, 0, 0, 1];
        let ap = [2, 0, 0, 0, 0, 2];
        let mut bytes = vec![0xb0, 0, 0, 0];
        bytes.extend_from_slice(&client);
        bytes.extend_from_slice(&ap);
        bytes.extend_from_slice(&ap);
        bytes.extend_from_slice(&[0, 0, 0, 0, 2, 0]);
        bytes.extend_from_slice(&status.to_le_bytes());
        bytes
    }

    fn association_response() -> Vec<u8> {
        let client = [2, 0, 0, 0, 0, 1];
        let ap = [2, 0, 0, 0, 0, 2];
        let mut bytes = vec![0x10, 0, 0, 0];
        bytes.extend_from_slice(&client);
        bytes.extend_from_slice(&ap);
        bytes.extend_from_slice(&ap);
        bytes.extend_from_slice(&[0, 0, 1, 0, 0, 0, 42, 0, 1, 2, 0x82, 0x84]);
        bytes
    }

    fn stale_data_frame() -> Vec<u8> {
        let mut bytes = vec![0x08, 0x02, 0, 0];
        bytes.extend_from_slice(&[2, 0, 0, 0, 0, 1]);
        bytes.extend_from_slice(&[2, 0, 0, 0, 0, 2]);
        bytes.extend_from_slice(&[2, 0, 0, 0, 0, 3]);
        bytes.extend_from_slice(&[0, 0, 0xaa, 0xaa, 3, 0, 0, 0, 0x08, 0x00, 1]);
        bytes
    }

    fn tx_result() -> fidl_softmac::WlanTxResult {
        fidl_softmac::WlanTxResult {
            tx_result_entry: [fidl_softmac::WlanTxResultEntry {
                tx_vector_idx: 0,
                attempts: 0,
            }; fidl_softmac::WLAN_TX_RESULT_MAX_ENTRY as usize],
            peer_addr: [0; 6],
            result_code: fidl_softmac::WlanTxResultCode::Success,
        }
    }

    #[test]
    fn host_mlme_device_forwards_the_complete_applicable_surface() {
        let (fake, _) = Fake::new(0);
        let (mut device, effects) = parts(fake);
        futures::executor::block_on(async {
            device.wlan_softmac_query_response().await.unwrap();
            device.discovery_support().await.unwrap();
            device.mac_sublayer_support().await.unwrap();
            device.security_support().await.unwrap();
            device.spectrum_management_support().await.unwrap();
            device
                .set_channel(
                    wlan_channel(),
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    wlan_channel(),
                )
                .await
                .unwrap();
            device.join_bss(&Default::default()).await.unwrap();
            device.install_key(&Default::default()).await.unwrap();
            device
                .notify_association_complete(Default::default())
                .await
                .unwrap();
            device.clear_association(&Default::default()).await.unwrap();
            device
                .start_passive_scan(&Default::default())
                .await
                .unwrap();
            device.start_active_scan(&Default::default()).await.unwrap();
            device.cancel_scan(&Default::default()).await.unwrap();
            device
                .update_wmm_parameters(&Default::default())
                .await
                .unwrap();
        });
        device
            .send_wlan_frame(
                vec![1, 0x40, 3].into(),
                fidl_softmac::WlanTxInfoFlags::empty(),
                None,
            )
            .unwrap();
        assert_eq!(
            effects.lock().unwrap().calls,
            [
                "query",
                "discovery",
                "mac",
                "security",
                "spectrum",
                "channel",
                "join",
                "key",
                "assoc",
                "clear",
                "passive",
                "active",
                "cancel",
                "wmm",
                "tx"
            ]
        );
        assert_eq!(
            effects.lock().unwrap().tx_flags,
            [fidl_softmac::WlanTxInfoFlags::PROTECTED]
        );
    }

    fn runtime(fake: Fake) -> ClientRuntime<Fake> {
        runtime_with_device_info(fake, device_info())
    }

    fn runtime_with_device_info(
        fake: Fake,
        device_info: fidl_mlme::DeviceInfo,
    ) -> ClientRuntime<Fake> {
        futures::executor::block_on(ClientRuntime::new(
            fake,
            Default::default(),
            device_info,
            Default::default(),
            Default::default(),
            Default::default(),
        ))
        .unwrap()
    }

    fn retry_device_info() -> fidl_mlme::DeviceInfo {
        let mut info = device_info();
        info.bands.push(fidl_mlme::BandCapability {
            band: fidl_ieee80211::WlanBand::TwoGhz,
            basic_rates: vec![0x82, 0x84],
            ht_cap: None,
            vht_cap: None,
            primary_channels: vec![wlan_channel()],
        });
        info
    }

    fn device_info() -> fidl_mlme::DeviceInfo {
        fidl_mlme::DeviceInfo {
            sta_addr: [2, 0, 0, 0, 0, 1],
            factory_addr: [2, 0, 0, 0, 0, 1],
            role: fidl_common::WlanMacRole::Client,
            bands: vec![],
            softmac_hardware_capability: 0,
            qos_capable: false,
        }
    }

    fn connect_request() -> fidl_sme::ConnectRequest {
        fidl_sme::ConnectRequest {
            ssid: b"test".to_vec(),
            bss_description: fidl_ieee80211::BssDescription {
                bssid: [2, 0, 0, 0, 0, 2],
                bss_type: fidl_ieee80211::BssType::Infrastructure,
                beacon_period: 100,
                capability_info: 1,
                ies: vec![0, 4, b't', b'e', b's', b't', 1, 2, 0x82, 0x84],
                primary: wlan_channel(),
                bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw20,
                vht_secondary_80_channel: wlan_channel(),
                rssi_dbm: -30,
                snr_db: 20,
            },
            multiple_bss_candidates: false,
            authentication: fidl_fuchsia_wlan_internal::Authentication {
                protocol: fidl_fuchsia_wlan_internal::Protocol::Open,
                credentials: None,
            },
            deprecated_scan_type: fidl_common::ScanType::Passive,
        }
    }

    #[test]
    fn runtime_is_constructible_and_all_upcalls_enter_the_host_pump() {
        let (fake, effects) = Fake::new(0);
        let mut runtime = runtime(fake);
        {
            let mut effects = effects.lock().unwrap();
            let upcalls = effects.upcalls.as_mut().unwrap();
            upcalls.recv(vec![0, 0], rx_info());
            upcalls.notify_scan_complete(zx::Status::OK, 9);
            upcalls.report_tx_result(tx_result());
        }
        assert!(futures::executor::block_on(runtime.pump_upcalls()).unwrap());
        assert!(runtime.upcalls.lock().unwrap().queue.is_empty());
    }

    #[test]
    fn control_at_capacity_evicts_oldest_raw_and_preserves_remaining_order() {
        let state = Arc::new(Mutex::new(UpcallQueue {
            live: true,
            overflowed: false,
            raw_queued: 0,
            queue: VecDeque::new(),
        }));
        let mut sender = UpcallSender(state.clone());
        for marker in 0..UPCALL_QUEUE_CAPACITY + 8 {
            sender.recv(vec![marker as u8], rx_info());
        }
        sender.notify_scan_complete(zx::Status::OK, 3);
        sender.report_tx_result(tx_result());
        let state = state.lock().unwrap();
        assert_eq!(state.queue.len(), UPCALL_QUEUE_CAPACITY);
        assert_eq!(state.raw_queued, UPCALL_QUEUE_CAPACITY - 2);
        assert!(matches!(
            state.queue.front(),
            Some(Upcall::Recv { bytes, .. }) if bytes == &[2]
        ));
        assert!(matches!(
            state.queue.get(UPCALL_QUEUE_CAPACITY - 2),
            Some(Upcall::ScanComplete { scan_id: 3, .. })
        ));
        assert!(matches!(state.queue.back(), Some(Upcall::TxResult(_))));
    }

    #[test]
    fn all_control_overflow_is_fatal_and_next_pump_contains_device() {
        let (fake, effects) = Fake::new(0);
        let mut runtime = runtime(fake);
        {
            let mut effects = effects.lock().unwrap();
            let upcalls = effects.upcalls.as_mut().unwrap();
            for _ in 0..=UPCALL_QUEUE_CAPACITY {
                upcalls.report_tx_result(tx_result());
            }
        }
        assert_eq!(
            futures::executor::block_on(runtime.pump_associated_once()),
            Err(ConnectError::Driver(DriverError::UpcallOverflow))
        );
        let state = runtime.upcalls.lock().unwrap();
        assert!(!state.live);
        assert!(state.overflowed);
        assert!(state.queue.is_empty());
        drop(state);
        assert_eq!(
            effects
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|call| **call == "stop")
                .count(),
            1
        );
    }

    #[test]
    fn mlme_initialization_failure_never_starts_the_device() {
        let (fake, effects) = Fake::new(0);
        effects.lock().unwrap().query_failure = true;
        let result = futures::executor::block_on(ClientRuntime::new(
            fake,
            Default::default(),
            device_info(),
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        assert!(result.is_err());
        assert_eq!(effects.lock().unwrap().calls, ["query"]);
    }

    #[test]
    fn stop_drains_queued_callbacks_and_excludes_late_callbacks() {
        let (fake, effects) = Fake::new(0);
        let mut runtime = runtime(fake);
        effects
            .lock()
            .unwrap()
            .upcalls
            .as_mut()
            .unwrap()
            .recv(vec![0, 0], rx_info());
        runtime.stop().unwrap();
        effects
            .lock()
            .unwrap()
            .upcalls
            .as_mut()
            .unwrap()
            .notify_scan_complete(zx::Status::OK, 1);
        assert!(runtime.upcalls.lock().unwrap().queue.is_empty());
    }

    #[test]
    fn failed_connect_terminally_revokes_host_state_even_when_reset_fails() {
        for reset_failure in [false, true] {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().reset_failure = reset_failure;
            let mut runtime = runtime(fake);
            assert!(runtime.take_ethernet_device().is_none());
            effects
                .lock()
                .unwrap()
                .upcalls
                .as_mut()
                .unwrap()
                .recv(vec![0, 0], rx_info());

            let error = futures::executor::block_on(
                runtime.connect(connect_request(), std::time::Instant::now()),
            )
            .unwrap_err();
            assert_eq!(
                error,
                if reset_failure {
                    ConnectError::Containment
                } else {
                    ConnectError::Timeout
                }
            );
            assert!(runtime.revoked);
            assert!(runtime.upcalls.lock().unwrap().queue.is_empty());
            assert!(runtime.take_ethernet_device().is_none());
            assert_eq!(
                futures::executor::block_on(
                    runtime.connect(connect_request(), std::time::Instant::now()),
                ),
                Err(ConnectError::Driver(DriverError::Stopped))
            );
            assert_eq!(
                futures::executor::block_on(runtime.pump_associated_once()),
                Err(ConnectError::Driver(DriverError::Stopped))
            );
            effects
                .lock()
                .unwrap()
                .upcalls
                .as_mut()
                .unwrap()
                .notify_scan_complete(zx::Status::OK, 1);
            assert!(runtime.upcalls.lock().unwrap().queue.is_empty());
        }
    }

    #[test]
    fn completed_failure_drains_stale_callbacks_and_allows_successful_retry() {
        let (fake, effects) = Fake::new(0);
        {
            let mut state = effects.lock().unwrap();
            state.simulate_ap = true;
            state.reject_next_auth = true;
            state.retry_cleanup = true;
            state.stale_callback_during_cleanup = true;
        }
        let mut runtime = runtime_with_device_info(fake, retry_device_info());
        assert!(runtime.take_ethernet_device().is_none());

        let failure = futures::executor::block_on(runtime.connect(
            connect_request(),
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        ))
        .unwrap_err();
        assert!(matches!(
            failure,
            ConnectError::Failed(fidl_sme::ConnectResult {
                code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                is_credential_rejected: false,
                is_reconnect: false,
            })
        ));
        assert!(!runtime.revoked);
        assert!(runtime.upcalls.lock().unwrap().queue.is_empty());
        assert_eq!(
            runtime.io.lock().unwrap().ethernet.deliver(&[0; 14]),
            Err(EthernetIngressError::LinkDown)
        );
        {
            let state = effects.lock().unwrap();
            assert_eq!(
                state
                    .calls
                    .iter()
                    .filter(|call| **call == "finish_failed_connect_attempt")
                    .count(),
                1
            );
            assert!(!state.calls.contains(&"reset"));
        }

        let result = futures::executor::block_on(runtime.connect(
            connect_request(),
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        ))
        .unwrap();
        assert_eq!(
            result,
            fidl_sme::ConnectResult {
                code: fidl_ieee80211::StatusCode::Success,
                is_credential_rejected: false,
                is_reconnect: false,
            }
        );
        assert!(runtime.sme().status().is_connected());
        let ethernet = runtime.take_ethernet_device().unwrap();
        assert!(ethernet.properties().is_some());
        assert!(runtime.take_ethernet_device().is_none());
    }

    #[test]
    fn reconnecting_disconnect_retains_the_transaction_stream() {
        let (fake, _) = Fake::new(0);
        let mut runtime = runtime(fake);
        let (events, stream) = mpsc::unbounded();
        runtime.connection = Some(stream);
        events
            .unbounded_send(wlan_sme::client::ConnectTransactionEvent::OnDisconnect {
                info: fidl_sme::DisconnectInfo {
                    is_sme_reconnecting: true,
                    disconnect_source: fidl_sme::DisconnectSource::User(
                        fidl_sme::UserDisconnectReason::FailedToConnect,
                    ),
                },
            })
            .unwrap();
        events
            .unbounded_send(wlan_sme::client::ConnectTransactionEvent::OnConnectResult {
                result: wlan_sme::client::ConnectResult::Success,
                is_reconnect: true,
            })
            .unwrap();

        assert!(matches!(
            runtime.next_connection_event().unwrap(),
            Some(fidl_sme::ConnectTransactionEvent::OnDisconnect {
                info: fidl_sme::DisconnectInfo {
                    is_sme_reconnecting: true,
                    ..
                }
            })
        ));
        assert!(matches!(
            runtime.next_connection_event().unwrap(),
            Some(fidl_sme::ConnectTransactionEvent::OnConnectResult {
                result: fidl_sme::ConnectResult {
                    code: fidl_ieee80211::StatusCode::Success,
                    is_credential_rejected: false,
                    is_reconnect: true,
                },
            })
        ));
    }

    #[test]
    fn retained_connect_attempt_can_be_canceled_and_reused() {
        let (fake, effects) = Fake::new(0);
        {
            let mut state = effects.lock().unwrap();
            state.simulate_ap = true;
            state.suppress_auth_response = true;
            state.retry_cleanup = true;
        }
        let mut runtime = runtime_with_device_info(fake, retry_device_info());
        runtime
            .begin_connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            runtime.begin_connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            ),
            Err(ConnectError::Driver(DriverError::ConnectInProgress))
        );

        let result = futures::executor::block_on(runtime.cancel_connect(
            fidl_sme::UserDisconnectReason::FailedToConnect,
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        ))
        .unwrap();
        assert_eq!(
            result,
            fidl_sme::ConnectResult {
                code: fidl_ieee80211::StatusCode::Canceled,
                is_credential_rejected: false,
                is_reconnect: false,
            }
        );
        assert!(sme_is_retry_quiescent(&runtime.sme().status()));
        assert!(!runtime.revoked);
        assert!(
            effects
                .lock()
                .unwrap()
                .calls
                .contains(&"finish_failed_connect_attempt")
        );

        effects.lock().unwrap().suppress_auth_response = false;
        runtime
            .begin_connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .unwrap();
        let result = loop {
            if let Some(result) = futures::executor::block_on(runtime.drive_connect_once()).unwrap()
            {
                break result;
            }
        };
        assert_eq!(result.code, fidl_ieee80211::StatusCode::Success);
    }

    #[test]
    fn canceled_connect_without_certified_cleanup_is_contained() {
        let (fake, effects) = Fake::new(0);
        {
            let mut state = effects.lock().unwrap();
            state.simulate_ap = true;
            state.suppress_auth_response = true;
        }
        let mut runtime = runtime_with_device_info(fake, retry_device_info());
        runtime
            .begin_connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .unwrap();

        assert_eq!(
            futures::executor::block_on(runtime.cancel_connect(
                fidl_sme::UserDisconnectReason::FailedToConnect,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )),
            Err(ConnectError::Driver(DriverError::RetryCleanup))
        );
        assert!(runtime.revoked);
        let state = effects.lock().unwrap();
        assert!(state.calls.contains(&"finish_failed_connect_attempt"));
        assert!(state.calls.contains(&"reset"));
    }

    #[test]
    fn successful_connection_retains_events_and_disconnects_before_reuse() {
        let (fake, effects) = Fake::new(0);
        effects.lock().unwrap().simulate_ap = true;
        let mut runtime = runtime_with_device_info(fake, retry_device_info());
        futures::executor::block_on(runtime.connect(
            connect_request(),
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        ))
        .unwrap();

        futures::executor::block_on(runtime.pump_associated_once()).unwrap();
        assert_eq!(
            futures::executor::block_on(runtime.connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )),
            Err(ConnectError::Driver(DriverError::AlreadyConnected))
        );
        futures::executor::block_on(runtime.disconnect(
            fidl_sme::UserDisconnectReason::FailedToConnect,
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        ))
        .unwrap();
        assert!(matches!(
            runtime.next_connection_event().unwrap(),
            Some(fidl_sme::ConnectTransactionEvent::OnDisconnect { .. })
        ));
        assert!(sme_is_retry_quiescent(&runtime.sme().status()));

        futures::executor::block_on(runtime.connect(
            connect_request(),
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        ))
        .unwrap();
        runtime.stop().unwrap();
        assert_eq!(
            runtime.next_connection_event(),
            Err(ConnectError::Driver(DriverError::Stopped))
        );
    }

    #[test]
    fn only_idle_sme_is_retry_quiescent() {
        assert!(sme_is_retry_quiescent(
            &wlan_sme::client::ClientSmeStatus::Idle
        ));
        assert!(!sme_is_retry_quiescent(
            &wlan_sme::client::ClientSmeStatus::Roaming([1; 6].into())
        ));
    }

    #[test]
    fn link_up_after_hup_publishes_a_fresh_ethernet_generation() {
        let mac = [2, 0, 0, 0, 0, 1];
        let capacity = 3;
        let (old_host, ethernet) = ethernet_port(mac, capacity).unwrap();
        let (fake, effects) = Fake::new(0);
        let device = Arc::new(Mutex::new(StartedDevice {
            device: fake,
            stop_pending: false,
        }));
        let io = Arc::new(Mutex::new(HostIo {
            ethernet,
            unpublished_ethernet_device: None,
            pending_ethernet_devices: VecDeque::new(),
            ethernet_mac_address: mac,
            ethernet_queue_capacity: capacity,
            minstrel: None,
        }));
        let mut host_device = HostMlmeDevice::new(device, io.clone());

        futures::executor::block_on(host_device.set_ethernet_status(LinkStatus::UP)).unwrap();
        futures::executor::block_on(host_device.set_ethernet_status(LinkStatus::DOWN)).unwrap();
        assert!(io.lock().unwrap().ethernet.is_closed());
        assert_eq!(old_host.properties().unwrap().mac_address, mac);

        effects.lock().unwrap().link_failure = true;
        assert_eq!(
            futures::executor::block_on(host_device.set_ethernet_status(LinkStatus::UP)),
            Err(zx::Status::IO)
        );
        assert!(io.lock().unwrap().ethernet.is_closed());
        assert!(io.lock().unwrap().pending_ethernet_devices.is_empty());

        effects.lock().unwrap().link_failure = false;
        futures::executor::block_on(host_device.set_ethernet_status(LinkStatus::UP)).unwrap();
        let mut state = io.lock().unwrap();
        assert!(!state.ethernet.is_closed());
        assert_eq!(state.pending_ethernet_devices.len(), 1);
        assert_eq!(old_host.properties(), None);
        let replacement = state.pending_ethernet_devices.pop_front().unwrap();
        assert_eq!(replacement.properties().unwrap().mac_address, mac);
        drop(state);

        futures::executor::block_on(host_device.set_ethernet_status(LinkStatus::UP)).unwrap();
        assert!(io.lock().unwrap().pending_ethernet_devices.is_empty());
    }

    #[test]
    fn link_down_revokes_unpublished_and_pending_ethernet_generations() {
        let mac = [2, 0, 0, 0, 0, 1];
        let make_host = || {
            let (host, ethernet) = ethernet_port(mac, 3).unwrap();
            let (fake, _) = Fake::new(0);
            let device = Arc::new(Mutex::new(StartedDevice {
                device: fake,
                stop_pending: false,
            }));
            let io = Arc::new(Mutex::new(HostIo {
                ethernet,
                unpublished_ethernet_device: Some(host),
                pending_ethernet_devices: VecDeque::new(),
                ethernet_mac_address: mac,
                ethernet_queue_capacity: 3,
                minstrel: None,
            }));
            (HostMlmeDevice::new(device, io.clone()), io)
        };

        let (mut before_up, before_up_io) = make_host();
        futures::executor::block_on(before_up.set_ethernet_status(LinkStatus::DOWN)).unwrap();
        let before_up_io = before_up_io.lock().unwrap();
        assert!(before_up_io.unpublished_ethernet_device.is_none());
        assert!(before_up_io.pending_ethernet_devices.is_empty());
        assert!(before_up_io.ethernet.is_closed());
        drop(before_up_io);

        let (mut while_pending, while_pending_io) = make_host();
        futures::executor::block_on(while_pending.set_ethernet_status(LinkStatus::UP)).unwrap();
        assert_eq!(
            while_pending_io
                .lock()
                .unwrap()
                .pending_ethernet_devices
                .len(),
            1
        );
        futures::executor::block_on(while_pending.set_ethernet_status(LinkStatus::DOWN)).unwrap();
        let while_pending_io = while_pending_io.lock().unwrap();
        assert!(while_pending_io.unpublished_ethernet_device.is_none());
        assert!(while_pending_io.pending_ethernet_devices.is_empty());
        assert!(while_pending_io.ethernet.is_closed());
    }

    #[test]
    fn completed_failure_without_retry_safe_driver_cleanup_is_terminal() {
        let (fake, effects) = Fake::new(0);
        {
            let mut state = effects.lock().unwrap();
            state.simulate_ap = true;
            state.reject_next_auth = true;
        }
        let mut runtime = runtime_with_device_info(fake, retry_device_info());

        assert_eq!(
            futures::executor::block_on(runtime.connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )),
            Err(ConnectError::Driver(DriverError::RetryCleanup))
        );
        assert!(runtime.revoked);
        let state = effects.lock().unwrap();
        assert!(state.calls.contains(&"finish_failed_connect_attempt"));
        assert!(state.calls.contains(&"reset"));
    }

    #[test]
    fn failed_stop_is_retried_but_successful_stop_is_not() {
        let (fake, effects) = Fake::new(1);
        let mut runtime_instance = runtime(fake);
        assert_eq!(runtime_instance.stop(), Err(zx::Status::IO));
        runtime_instance.stop().unwrap();
        drop(runtime_instance);
        assert_eq!(
            effects
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|call| **call == "stop")
                .count(),
            2
        );

        let (fake, effects) = Fake::new(1);
        let mut runtime_after_failure = runtime(fake);
        assert_eq!(runtime_after_failure.stop(), Err(zx::Status::IO));
        drop(runtime_after_failure);
        assert_eq!(
            effects
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|call| **call == "stop")
                .count(),
            2
        );
    }

    #[test]
    fn successful_and_failed_stop_make_connect_and_pump_terminal() {
        let (fake, effects) = Fake::new(0);
        let mut stopped = runtime(fake);
        stopped.stop().unwrap();
        let calls_after_stop = effects.lock().unwrap().calls.clone();
        assert_eq!(
            futures::executor::block_on(stopped.connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )),
            Err(ConnectError::Driver(DriverError::Stopped))
        );
        assert_eq!(*effects.lock().unwrap().calls, calls_after_stop);

        let (fake, effects) = Fake::new(1);
        let mut stop_failed = runtime(fake);
        assert_eq!(stop_failed.stop(), Err(zx::Status::IO));
        let calls_after_stop = effects.lock().unwrap().calls.clone();
        assert_eq!(
            futures::executor::block_on(stop_failed.pump_associated_once()),
            Err(ConnectError::Driver(DriverError::Stopped))
        );
        assert_eq!(*effects.lock().unwrap().calls, calls_after_stop);
    }
}
