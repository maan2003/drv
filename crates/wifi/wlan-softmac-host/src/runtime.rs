// SPDX-License-Identifier: GPL-2.0-only

//! Chip-independent ownership of the pinned Fuchsia client MLME/SME/RSN loop.

use crate::ethernet::{
    DriverEthernetPort, EthernetIngressError, HostEthernetDevice, ethernet_port,
};
use crate::{
    ClientRuntimeDriver, OperationEpoch, WlanSoftmac, WlanSoftmacLifecycle, WlanSoftmacUpcalls,
};
use fdf::ArenaStaticBox;
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_driver as fidl_driver;
use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
use fidl_fuchsia_wlan_mlme as fidl_mlme;
use fidl_fuchsia_wlan_sme as fidl_sme;
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, Stream, StreamExt};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use wlan_mlme::MlmeImpl;
use wlan_mlme::device::{DeviceOps, LinkStatus};
use wlan_sme::Station;

const UPCALL_QUEUE_CAPACITY: usize = 256;
const ETHERNET_QUEUE_CAPACITY: usize = 256;
/// Bounded Ethernet generations created before production lockdown. Exhaustion
/// terminates the runtime cleanly rather than creating a descriptor post-lock.
pub const PREPARED_ETHERNET_GENERATIONS: usize = 4;

struct MlmeExecution {
    epoch: RefCell<OperationEpoch>,
    rejected: Cell<bool>,
}

impl MlmeExecution {
    fn admit(&self) -> Result<(), zx::Status> {
        if self.epoch.borrow().is_live() {
            return Ok(());
        }
        self.rejected.set(true);
        Err(zx::Status::CANCELED)
    }
}

struct StartedDevice<D> {
    device: D,
    // A failed stop remains pending. Later explicit stop calls and Drop retry it.
    stop_pending: bool,
}

struct HostIo {
    ethernet: DriverEthernetPort,
    replacement_ethernet: VecDeque<(HostEthernetDevice, DriverEthernetPort)>,
    unpublished_ethernet_device: Option<HostEthernetDevice>,
    pending_ethernet_devices: VecDeque<HostEthernetDevice>,
    ethernet_mac_address: [u8; 6],
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
    epoch: OperationEpoch,
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
    execution: Rc<MlmeExecution>,
    device: Arc<Mutex<StartedDevice<D>>>,
    io: Arc<Mutex<HostIo>>,
    event_sink: mpsc::UnboundedSender<(OperationEpoch, fidl_mlme::MlmeEvent)>,
    event_stream: Option<mpsc::UnboundedReceiver<(OperationEpoch, fidl_mlme::MlmeEvent)>>,
}

impl<D> HostMlmeDevice<D> {
    fn new(device: Arc<Mutex<StartedDevice<D>>>, io: Arc<Mutex<HostIo>>) -> Self {
        let (event_sink, event_stream) = mpsc::unbounded();
        Self {
            execution: Rc::new(MlmeExecution {
                epoch: RefCell::new(OperationEpoch::new()),
                rejected: Cell::new(false),
            }),
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
        self.execution.admit()?;
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
        self.execution.admit()?;
        if buffer.get(1).is_some_and(|byte| byte & 0x40 != 0) {
            flags |= fidl_softmac::WlanTxInfoFlags::PROTECTED;
        }
        self.device.lock().unwrap().device.queue_tx(&buffer, flags)
    }
    async fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
        self.execution.admit()?;
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
                let (host, driver) = io
                    .replacement_ethernet
                    .pop_front()
                    .ok_or(zx::Status::NO_RESOURCES)?;
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
        self.execution.admit()?;
        eprintln!(
            "client_softmac_channel stage=bridge_enter primary={primary:?} bandwidth={bandwidth:?} secondary={secondary:?}"
        );
        let result = {
            let completion = {
                self.device.lock().unwrap().device.set_channel(
                    fidl_softmac::WlanSoftmacBaseSetChannelRequest {
                        primary: Some(primary),
                        bandwidth: Some(bandwidth),
                        vht_secondary_80_channel: Some(secondary),
                    },
                )
            };
            completion.await
        };
        eprintln!("client_softmac_channel stage=bridge_complete result={result:?}");
        result
    }
    async fn set_mac_address(&mut self, _: [u8; 6]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    async fn start_passive_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        self.execution.admit()?;
        eprintln!(
            "client_softmac_scan stage=bridge_enter kind=passive channel_count={} min_channel_time={:?} max_channel_time={:?} min_home_time={:?}",
            request.channels.as_ref().map_or(0, Vec::len),
            request.min_channel_time,
            request.max_channel_time,
            request.min_home_time,
        );
        let response = {
            let completion = {
                self.device
                    .lock()
                    .unwrap()
                    .device
                    .start_passive_scan(request.clone())
            };
            completion.await
        };
        eprintln!(
            "client_softmac_scan stage=bridge_complete kind=passive status={}",
            if response.is_ok() { "ok" } else { "error" }
        );
        response
    }
    async fn start_active_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
        self.execution.admit()?;
        {
            let completion = {
                self.device
                    .lock()
                    .unwrap()
                    .device
                    .start_active_scan(request.clone())
            };
            completion.await
        }
    }
    async fn cancel_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        self.execution.admit()?;
        {
            let completion = {
                self.device
                    .lock()
                    .unwrap()
                    .device
                    .cancel_scan(request.clone())
            };
            completion.await
        }
    }
    async fn join_bss(&mut self, request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        self.execution.admit()?;
        {
            let completion = { self.device.lock().unwrap().device.join_bss(request.clone()) };
            completion.await
        }
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
        self.execution.admit()?;
        {
            let completion = { self.device.lock().unwrap().device.install_key(key.clone()) };
            completion.await
        }
    }
    async fn notify_association_complete(
        &mut self,
        config: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        self.execution.admit()?;
        eprintln!("client_association stage=configure_enter config={config:?}");
        let result = {
            let completion = {
                self.device
                    .lock()
                    .unwrap()
                    .device
                    .notify_association_complete(config)
            };
            completion.await
        };
        eprintln!("client_association stage=configure_complete result={result:?}");
        result
    }
    async fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        self.execution.admit()?;
        {
            let completion = {
                self.device
                    .lock()
                    .unwrap()
                    .device
                    .clear_association(request.clone())
            };
            completion.await
        }
    }
    async fn update_wmm_parameters(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        self.execution.admit()?;
        {
            let completion = {
                self.device
                    .lock()
                    .unwrap()
                    .device
                    .update_wmm_parameters(request.clone())
            };
            completion.await
        }
    }
    fn take_mlme_event_stream(&mut self) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
        // The owning runtime takes the origin-bearing route directly. An
        // untagged second receiver would erase operation authority.
        None
    }
    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
        if !self.execution.epoch.borrow().is_live() {
            return Ok(());
        }
        self.event_sink
            .unbounded_send((self.execution.epoch.borrow().clone(), event))
            .map_err(Into::into)
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
type MlmeTimerAction =
    wlan_mlme::common::timer::Event<(OperationEpoch, wlan_mlme::client::TimedEvent)>;

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
    MlmeRequest {
        name: &'static str,
        detail: String,
    },
    ClientRx(zx::Status),
    Ethernet(zx::Status),
    RequestStreamClosed,
    MlmeTaskFailed,
    EventStreamClosed,
    ConnectTransactionClosed,
    ConnectStateMismatch,
    AlreadyConnected,
    NotConnected,
    /// Pinned Fuchsia SoftMAC MLME does not implement SME's fullmac Roam
    /// request. Reject it before SME leaves the healthy Associated state.
    RoamUnsupported,
    ConnectInProgress,
    NoConnectInProgress,
    ScanInProgress,
    NoScanInProgress,
    ScanTransactionClosed,
    RetryCleanup,
    ControlBudgetExhausted,
    Stopped,
    UpcallOverflow,
}

enum MlmeInput {
    Request(wlan_sme::MlmeRequest),
    Upcall(Upcall),
    Timeout(wlan_mlme::client::TimedEvent),
    Ethernet(Vec<u8>),
}

/// Owns MLME across awaited device operations; it never borrows ClientRuntime.
/// Scheduled on the service LocalSet; completion wakes the owning protocol loop.
struct MlmeTask {
    sender: mpsc::Sender<(OperationEpoch, MlmeInput)>,
    epoch: OperationEpoch,
    task: Option<tokio::task::JoinHandle<Result<(), ConnectError>>>,
    changed: std::rc::Rc<tokio::sync::Notify>,
    pending: std::rc::Rc<std::cell::Cell<usize>>,
}

impl MlmeTask {
    fn new<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver + 'static>(
        mut mlme: wlan_mlme::client::ClientMlme<HostMlmeDevice<D>>,
        io: Arc<Mutex<HostIo>>,
        execution: Rc<MlmeExecution>,
        mut timers: wlan_common::timer::EventStream<wlan_mlme::client::TimedEvent>,
        timed: mpsc::UnboundedSender<
            wlan_common::timer::ScheduledEvent<(OperationEpoch, wlan_mlme::client::TimedEvent)>,
        >,
    ) -> Self {
        let epoch = execution.epoch.borrow().clone();
        let (sender, mut receiver) =
            mpsc::channel::<(OperationEpoch, MlmeInput)>(UPCALL_QUEUE_CAPACITY);
        let pending = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let work = pending.clone();
        let changed = std::rc::Rc::new(tokio::sync::Notify::new());
        let progress = changed.clone();
        let future = async move {
            while let Some((epoch, input)) = receiver.next().await {
                execution.epoch.replace(epoch.clone());
                execution.rejected.set(false);
                // Completion still retires scanner bookkeeping after revocation,
                // but an old queued request/RX/timer cannot start new work.
                if epoch.is_live()
                    || matches!(&input, MlmeInput::Upcall(Upcall::ScanComplete { .. }))
                {
                    let handler = async {
                        match input {
                            MlmeInput::Request(request) => {
                                let sae_frame_tx =
                                    matches!(&request, wlan_sme::MlmeRequest::SaeFrameTx(_));
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
                                match &request {
                                    wlan_sme::MlmeRequest::SaeHandshakeResp(response) => {
                                        eprintln!("client_sae_handshake response={response:?}");
                                    }
                                    wlan_sme::MlmeRequest::Deauthenticate(request) => {
                                        eprintln!("client_deauthenticate request={request:?}");
                                    }
                                    _ => {}
                                }
                                let name = request.name();
                                // Diagnostic: surface every MLME request the SME issues so the
                                // post-4-way sequence (SetKeys GTK/IGTK, SetCtrlPort, Deauth) is
                                // visible when the connect fails after PTK.
                                println!("client_mlme_request name={name}");
                                if let Err(error) =
                                    wlan_mlme::MlmeImpl::handle_mlme_request(&mut mlme, request)
                                        .await
                                {
                                    let rejected = execution.rejected.get()
                                        && matches!(error.downcast_ref::<wlan_mlme::error::Error>(),
                                    Some(wlan_mlme::error::Error::Status(_, status)) if *status == zx::Status::CANCELED);
                                    if !rejected {
                                        return Err(ConnectError::Driver(
                                            DriverError::MlmeRequest {
                                                name,
                                                detail: error.to_string(),
                                            },
                                        ));
                                    }
                                }
                                println!("client_mlme_request_complete name={name}");
                                if sae_frame_tx {
                                    println!(
                                        "client_sae_stage=mlme_request_complete state={}",
                                        mlme.sae_state_name()
                                    );
                                }
                                if eapol_tx {
                                    println!("client_eapol_stage=mlme_tx_request_complete");
                                }
                            }
                            MlmeInput::Upcall(upcall) => match upcall {
                                Upcall::ScanComplete { status, scan_id } => {
                                    MlmeImpl::handle_scan_complete(&mut mlme, status, scan_id)
                                        .await;
                                }
                                Upcall::TxResult(result) => {
                                    if let Some(minstrel) = io.lock().unwrap().minstrel.clone() {
                                        minstrel.lock().handle_tx_result_report(&result);
                                    }
                                }
                                Upcall::Recv { bytes, info } => {
                                    let auth = safe_auth_stage(&bytes);
                                    let eapol = bytes.windows(8).any(|window| {
                                        window == [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]
                                    });
                                    MlmeImpl::handle_mac_frame_rx(
                                        &mut mlme,
                                        &bytes,
                                        info,
                                        fuchsia_trace::Id::new(),
                                    )
                                    .await;
                                    if let Some((algorithm, transaction, status, rejected_group)) =
                                        auth
                                    {
                                        println!(
                                            "client_mlme_rx stage=handle_complete algorithm={algorithm} transaction={transaction} status={status} rejected_group={rejected_group:?}"
                                        );
                                    }
                                    if eapol {
                                        println!("client_eapol_stage=mlme_handle_complete");
                                    }
                                }
                            },
                            MlmeInput::Timeout(event) => {
                                MlmeImpl::handle_timeout(&mut mlme, event).await;
                            }
                            MlmeInput::Ethernet(bytes) => {
                                if let Err(error) = MlmeImpl::handle_eth_frame_tx(
                                    &mut mlme,
                                    &bytes,
                                    fuchsia_trace::Id::new(),
                                ) {
                                    println!(
                                        "client_data_tx_error stage=ethernet_pump kind=target_rejected error={error}"
                                    );
                                }
                            }
                        }
                        Ok::<(), ConnectError>(())
                    };
                    let mut handler = std::pin::pin!(handler);
                    std::future::poll_fn(|cx| {
                        let result = handler.as_mut().poll(cx);
                        // Capture timer origin on the very poll that scheduled it,
                        // including polls suspended in a driver completion.
                        while let Ok((deadline, event, handle)) = timers.try_recv() {
                            let tagged = wlan_common::timer::Event {
                                id: event.id,
                                event: (epoch.clone(), event.event),
                            };
                            if timed.unbounded_send((deadline, tagged, handle)).is_err() {
                                return std::task::Poll::Ready(Err(ConnectError::Driver(
                                    DriverError::EventStreamClosed,
                                )));
                            }
                        }
                        result
                    })
                    .await?;
                }
                work.set(work.get() - 1);
                // Publish effects/events before reporting completion. An empty
                // input queue alone cannot certify a suspended handler drained.
                progress.notify_one();
                tokio::task::yield_now().await;
            }
            Ok(())
        };
        let finished = changed.clone();
        let task = tokio::task::spawn_local(async move {
            let result = future.await;
            finished.notify_one();
            result
        });
        Self {
            sender,
            epoch,
            task: Some(task),
            changed,
            pending,
        }
    }

    fn enqueue(&mut self, input: MlmeInput) -> Result<(), ConnectError> {
        self.enqueue_for(self.epoch.clone(), input)
    }

    fn enqueue_for(&mut self, epoch: OperationEpoch, input: MlmeInput) -> Result<(), ConnectError> {
        if self.pending.get() == UPCALL_QUEUE_CAPACITY {
            return Err(ConnectError::Driver(DriverError::ControlBudgetExhausted));
        }
        self.sender
            .try_send((epoch, input))
            .map_err(|_| ConnectError::Driver(DriverError::RequestStreamClosed))?;
        self.pending.set(self.pending.get() + 1);
        Ok(())
    }

    fn check(&mut self) -> Result<bool, ConnectError> {
        if let Some(result) = self.task.as_mut().and_then(|task| task.now_or_never()) {
            self.task = None;
            result.map_err(|_| ConnectError::Driver(DriverError::MlmeTaskFailed))??;
            return Err(ConnectError::Driver(DriverError::RequestStreamClosed));
        }
        Ok(self.changed.notified().now_or_never().is_some())
    }

    fn abort(&mut self) {
        self.epoch.revoke();
        self.sender.close_channel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }

    async fn join(&mut self) -> Result<(), zx::Status> {
        let result = match self.task.as_mut() {
            Some(task) => Some(task.await),
            None => None,
        };
        self.task = None;
        if let Some(result) = result {
            match result {
                Ok(Ok(())) => {}
                Err(error) if error.is_cancelled() => {}
                _ => return Err(zx::Status::IO),
            }
        }
        self.pending.set(0);
        Ok(())
    }

    fn is_idle(&self) -> bool {
        self.pending.get() == 0
    }
}

struct ConnectAttempt {
    transaction: wlan_sme::client::ConnectTransactionStream,
    deadline: std::time::Instant,
}

enum Connection {
    Active(wlan_sme::client::ConnectTransactionStream),
    EndedNeedsCleanup,
}

struct Cleanup {
    deadline: std::time::Instant,
    terminal: Option<fidl_sme::ConnectTransactionEvent>,
    transaction_closed: bool,
}

struct ScanAttempt {
    receiver:
        oneshot::Receiver<Result<Vec<wlan_common::scan::ScanResult>, fidl_mlme::ScanResultCode>>,
    deadline: std::time::Instant,
}

/// Bounded production owner for SME, MLME, RSN, timers, device events, and
/// chip-supplied RX. No parallel association state is attached to this owner.
pub struct ClientRuntime<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> {
    device: Arc<Mutex<StartedDevice<D>>>,
    upcalls: Arc<Mutex<UpcallQueue>>,
    io: Arc<Mutex<HostIo>>,
    sme: wlan_sme::client::ClientSme,
    mlme: MlmeTask,
    requests: wlan_sme::MlmeStream,
    events: mpsc::UnboundedReceiver<(OperationEpoch, fidl_mlme::MlmeEvent)>,
    sme_timer_source:
        Pin<Box<dyn Stream<Item = wlan_common::timer::ScheduledEvent<SmeTimerAction>>>>,
    sme_timer_sender: mpsc::UnboundedSender<
        wlan_common::timer::ScheduledEvent<(Option<OperationEpoch>, SmeTimerAction)>,
    >,
    sme_timers: Pin<Box<dyn Stream<Item = (Option<OperationEpoch>, SmeTimerAction)>>>,
    mlme_timers: Pin<Box<dyn Stream<Item = MlmeTimerAction>>>,
    connect_attempt: Option<ConnectAttempt>,
    cleanup: Option<Cleanup>,
    scan_attempt: Option<ScanAttempt>,
    connection: Option<Connection>,
    connection_events: VecDeque<fidl_sme::ConnectTransactionEvent>,
    revoked: bool,
}

/// Inert host runtime capabilities created before process lockdown.
///
/// This owns only prepared Ethernet socketpairs. The Linux entrypoint owns
/// the Tokio runtime, LocalSet, reactor inventory and sandbox registration.
/// Device, firmware, QMI, and RX activation are deliberately absent.
pub struct PreparedRuntimeResources {
    ethernet_device: HostEthernetDevice,
    ethernet: DriverEthernetPort,
    replacement_ethernet: VecDeque<(HostEthernetDevice, DriverEthernetPort)>,
    mac_address: [u8; 6],
}

impl PreparedRuntimeResources {
    pub fn new(mac_address: [u8; 6]) -> Result<Self, anyhow::Error> {
        Self::with_ethernet_capacity(mac_address, ETHERNET_QUEUE_CAPACITY)
    }

    pub fn with_ethernet_capacity(
        mac_address: [u8; 6],
        ethernet_queue_capacity: usize,
    ) -> Result<Self, anyhow::Error> {
        let mut generations = (0..PREPARED_ETHERNET_GENERATIONS)
            .map(|_| ethernet_port(mac_address, ethernet_queue_capacity))
            .collect::<Result<VecDeque<_>, _>>()
            .map_err(|error| anyhow::anyhow!("invalid host Ethernet endpoint: {error:?}"))?;
        let (ethernet_device, ethernet) = generations.pop_front().unwrap();
        Ok(Self {
            ethernet_device,
            ethernet,
            replacement_ethernet: generations,
            mac_address,
        })
    }

    /// The bounded inert Ethernet generations that sandbox setup must retain.
    /// They remain owned by this value and are never duplicated.
    pub fn fd_identities(&self) -> Vec<std::os::fd::RawFd> {
        let mut fds = vec![self.ethernet_device.raw_fd(), self.ethernet.raw_fd()];
        for (host, driver) in &self.replacement_ethernet {
            fds.extend([host.raw_fd(), driver.raw_fd()]);
        }
        fds
    }
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
    ) -> Result<Self, anyhow::Error>
    where
        D: 'static,
    {
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
    ) -> Result<Self, anyhow::Error>
    where
        D: 'static,
    {
        let resources = PreparedRuntimeResources::with_ethernet_capacity(
            device_info.sta_addr,
            ethernet_queue_capacity,
        )?;
        Self::new_with_prepared_resources(
            device,
            sme_config,
            device_info,
            security,
            spectrum,
            inspector,
            resources,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn new_with_prepared_resources(
        device: D,
        sme_config: wlan_sme::client::ClientConfig,
        device_info: fidl_mlme::DeviceInfo,
        security: fidl_common::SecuritySupport,
        spectrum: fidl_common::SpectrumManagementSupport,
        inspector: fuchsia_inspect::Inspector,
        resources: PreparedRuntimeResources,
    ) -> Result<Self, anyhow::Error>
    where
        D: 'static,
    {
        tokio::runtime::Handle::try_current().map_err(|error| {
            anyhow::anyhow!("ClientRuntime requires an owning Tokio runtime: {error}")
        })?;
        if device_info.sta_addr != resources.mac_address {
            return Err(anyhow::anyhow!(
                "prepared Ethernet MAC differs from queried SoftMAC MAC"
            ));
        }
        let PreparedRuntimeResources {
            ethernet_device,
            ethernet,
            replacement_ethernet,
            mac_address,
        } = resources;
        let upcalls = Arc::new(Mutex::new(UpcallQueue {
            epoch: OperationEpoch::new(),
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
            replacement_ethernet,
            unpublished_ethernet_device: Some(ethernet_device),
            pending_ethernet_devices: VecDeque::new(),
            ethernet_mac_address: mac_address,
            minstrel: None,
        }));
        let mut mlme_device = HostMlmeDevice::new(device.clone(), io.clone());
        let events = mlme_device
            .event_stream
            .take()
            .ok_or_else(|| anyhow::anyhow!("MLME event stream was already taken"))?;
        let execution = mlme_device.execution.clone();
        upcalls.lock().unwrap().epoch = execution.epoch.borrow().clone();
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
        let sme_timer_source = Box::pin(sme_timer_stream.map(|(deadline, event, handle)| {
            let id = event.id;
            let action = Box::new(move |sme: &mut wlan_sme::client::ClientSme| {
                Station::on_timeout(sme, event)
            }) as SmeTimerAction;
            (
                deadline,
                wlan_common::timer::Event { id, event: action },
                handle,
            )
        }));
        let (sme_timer_sender, sme_timed) = mpsc::unbounded();
        let sme_timers = Box::pin(
            wlan_common::timer::make_async_timed_event_stream(sme_timed).map(|event| event.event),
        );
        let (timed, timed_receiver) = mpsc::unbounded();
        let mlme_timers = Box::pin(wlan_mlme::common::timer::make_async_timed_event_stream(
            timed_receiver,
        ));
        {
            let mut state = device.lock().unwrap();
            if let Err(status) = state.device.start(Box::new(UpcallSender(upcalls.clone()))) {
                revoke_and_drain(&upcalls);
                return Err(anyhow::anyhow!("SoftMAC start failed: {status}"));
            }
            state.stop_pending = true;
        }
        let mut runtime = Self {
            device,
            upcalls,
            io: io.clone(),
            sme,
            mlme: MlmeTask::new(mlme, io.clone(), execution, mlme_timer_stream, timed),
            requests,
            events,
            sme_timer_source,
            sme_timer_sender,
            sme_timers,
            mlme_timers,
            connect_attempt: None,
            cleanup: None,
            scan_attempt: None,
            connection: None,
            connection_events: VecDeque::new(),
            revoked: false,
        };
        // Constructor maintenance timers are not connection authority.
        runtime
            .capture_sme_outputs(None)
            .map_err(|error| anyhow::anyhow!("capture initial SME outputs: {error:?}"))?;
        Ok(runtime)
    }

    pub fn sme(&self) -> &wlan_sme::client::ClientSme {
        &self.sme
    }

    /// Stable MAC identity published with each Ethernet generation.
    pub fn public_mac(&self) -> [u8; 6] {
        self.io.lock().unwrap().ethernet_mac_address
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
        self.cleanup = None;
        self.mlme.abort();
        self.connect_attempt = None;
        self.scan_attempt = None;
        self.connection = None;
        revoke_and_drain(&self.upcalls);
        self.io.lock().unwrap().ethernet.teardown();
        stop_device(&self.device)
    }

    /// Terminal service shutdown joins the MLME task before its LocalSet is
    /// destroyed. Abandoned device operations remain the driver's resources;
    /// aborting MLME is not evidence of DMA completion.
    pub async fn shutdown(&mut self) -> Result<(), zx::Status> {
        let stopped = self.stop();
        let joined = self.mlme.join().await;
        stopped.and(joined)
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
            let Some(upcall) = upcall else { break };
            let epoch = self.upcalls.lock().unwrap().epoch.clone();
            self.mlme.enqueue_for(epoch, MlmeInput::Upcall(upcall))?;
            progressed = true;
        }
        Ok(progressed)
    }

    fn capture_sme_outputs(&mut self, epoch: Option<OperationEpoch>) -> Result<(), ConnectError> {
        while let Ok(request) = self.requests.try_recv() {
            let epoch = epoch
                .clone()
                .ok_or(ConnectError::Driver(DriverError::RequestStreamClosed))?;
            self.mlme.enqueue_for(epoch, MlmeInput::Request(request))?;
        }
        while let Some((deadline, event, handle)) = self
            .sme_timer_source
            .as_mut()
            .next()
            .now_or_never()
            .flatten()
        {
            let tagged = wlan_common::timer::Event {
                id: event.id,
                event: (epoch.clone(), event.event),
            };
            self.sme_timer_sender
                .unbounded_send((deadline, tagged, handle))
                .map_err(|_| ConnectError::Driver(DriverError::EventStreamClosed))?;
        }
        Ok(())
    }

    async fn drain_control(&mut self, budget: usize) -> Result<(bool, bool), ConnectError> {
        let mut progressed = false;
        for _ in 0..budget {
            let mut cycle_progressed = false;
            match self.events.try_recv() {
                Ok((epoch, event)) => {
                    progressed = true;
                    if !epoch.is_live() {
                        continue;
                    }
                    if let fidl_mlme::MlmeEvent::OnScanEnd { end } = &event {
                        println!(
                            "client_mlme_scan_end txn_id={} code={:?}",
                            end.txn_id, end.code
                        );
                    }
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
                    self.capture_sme_outputs(Some(epoch))?;
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
        let resumed = self.mlme.check()?;
        let (mut progressed, mut control_ready_drained) =
            self.drain_control(CONTROL_BUDGET).await?;
        progressed |= resumed;
        if let Some((epoch, action)) = self.sme_timers.as_mut().next().now_or_never().flatten() {
            if epoch.as_ref().is_none_or(OperationEpoch::is_live) {
                action(&mut self.sme);
                self.capture_sme_outputs(epoch)?;
            }
            progressed = true;
            control_ready_drained = false;
        }
        if let Some(event) = self.mlme_timers.as_mut().next().now_or_never().flatten() {
            println!(
                "client_mlme_timer stage=stream_dequeued timer_id={} event={:?}",
                event.id, event.event.1
            );
            self.mlme
                .enqueue_for(event.event.0, MlmeInput::Timeout(event.event.1))?;
            progressed = true;
            control_ready_drained = false;
        }

        if !control_ready_drained {
            let (control_progressed, quiescent) = self.drain_control(CONTROL_BUDGET).await?;
            progressed |= control_progressed;
            control_ready_drained = quiescent;
        }
        if !control_ready_drained {
            println!(
                "client_runtime_control stage=budget_exhausted budget={CONTROL_BUDGET} rx_dequeued=false"
            );
            return Ok(true);
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
                return Ok(true);
            }
        }
        Ok(progressed)
    }

    /// Advance post-association SME/MLME control, timers, hardware RX, and one
    /// driver-bound Ethernet frame. No backend lock is held across MLME TX.
    pub async fn pump_associated_once(&mut self) -> Result<bool, ConnectError> {
        self.drive_service_once().await
    }

    /// Advance retained connection state and, only while the post-pump
    /// controlled port remains up, one driver-bound Ethernet frame. This is
    /// the service-loop entry point for connected, roaming, and reconnecting
    /// states; connect and scan attempts retain their dedicated drivers.
    pub async fn drive_service_once(&mut self) -> Result<bool, ConnectError> {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        if self.connect_attempt.is_some() || self.scan_attempt.is_some() {
            return Ok(false);
        }
        let mut progressed = self.pump_once().await?;
        let frame = {
            let mut io = self.io.lock().unwrap();
            if !io.ethernet.is_link_up() {
                return Ok(progressed);
            }
            io.ethernet.take_transmit().map_err(|error| {
                let status = ethernet_status(error);
                println!("client_data_seam_error direction=netstack_to_driver status={status}");
                ConnectError::Driver(DriverError::Ethernet(status))
            })?
        };
        if let Some(frame) = frame {
            self.mlme
                .enqueue(MlmeInput::Ethernet(frame.as_bytes().to_vec()))?;
            progressed = true;
        }
        Ok(progressed)
    }

    pub async fn connect(
        &mut self,
        request: fidl_sme::ConnectRequest,
        deadline: std::time::Instant,
    ) -> Result<fidl_sme::ConnectResult, ConnectError> {
        self.begin_connect(request, deadline).await?;
        loop {
            if let Some(result) = self.drive_connect_once().await? {
                return Ok(result);
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    /// Start one policy-selected connect attempt while retaining its SME
    /// transaction in the runtime. This permits a service loop to keep
    /// processing control requests without dropping an in-flight attempt.
    pub async fn begin_connect(
        &mut self,
        request: fidl_sme::ConnectRequest,
        deadline: std::time::Instant,
    ) -> Result<(), ConnectError> {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        self.drain_connection_events()?;
        if matches!(self.connection, Some(Connection::EndedNeedsCleanup)) {
            self.disconnect(fidl_sme::UserDisconnectReason::FailedToConnect, deadline)
                .await?;
        }
        if std::time::Instant::now() >= deadline {
            return Err(ConnectError::Timeout);
        }
        if self.connection.is_some() {
            return Err(ConnectError::Driver(DriverError::AlreadyConnected));
        }
        if self.connect_attempt.is_some() {
            return Err(ConnectError::Driver(DriverError::ConnectInProgress));
        }
        if self.scan_attempt.is_some() {
            return Err(ConnectError::Driver(DriverError::ScanInProgress));
        }
        if self.cleanup.is_some() {
            return Err(ConnectError::Driver(DriverError::ConnectInProgress));
        }
        self.begin_epoch();
        self.connect_attempt = Some(ConnectAttempt {
            transaction: self.sme.on_connect_command(request),
            deadline,
        });
        self.capture_sme_outputs(Some(self.mlme.epoch.clone()))?;
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

    /// Reject explicit roaming without changing the current connection.
    /// Pinned Fuchsia SoftMAC MLME ignores SME's fullmac-only `Roam` request;
    /// entering SME Roaming here would otherwise wedge the association.
    pub fn roam(&mut self, request: fidl_sme::RoamRequest) -> Result<(), ConnectError> {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        if self.connect_attempt.is_some() {
            return Err(ConnectError::Driver(DriverError::ConnectInProgress));
        }
        if self.connection.is_none() || !self.sme.status().is_connected() {
            return Err(ConnectError::Driver(DriverError::NotConnected));
        }
        let _ = request;
        Err(ConnectError::Driver(DriverError::RoamUnsupported))
    }

    /// Start one SME discovery scan while retaining its response in the
    /// runtime for a nonblocking service loop.
    pub async fn begin_scan(
        &mut self,
        request: fidl_sme::ScanRequest,
        deadline: std::time::Instant,
    ) -> Result<(), ConnectError> {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        if self.scan_attempt.is_some() {
            return Err(ConnectError::Driver(DriverError::ScanInProgress));
        }
        if self.connect_attempt.is_some() {
            return Err(ConnectError::Driver(DriverError::ConnectInProgress));
        }
        self.drain_connection_events()?;
        if matches!(self.connection, Some(Connection::EndedNeedsCleanup)) {
            self.disconnect(fidl_sme::UserDisconnectReason::FailedToConnect, deadline)
                .await?;
        }
        if std::time::Instant::now() >= deadline {
            return Err(ConnectError::Timeout);
        }
        if self.cleanup.is_some() {
            return Err(ConnectError::Driver(DriverError::ConnectInProgress));
        }
        if self.connection.is_none() {
            self.begin_epoch();
        }
        self.scan_attempt = Some(ScanAttempt {
            receiver: self.sme.on_scan_command(request),
            deadline,
        });
        self.capture_sme_outputs(Some(self.mlme.epoch.clone()))?;
        Ok(())
    }

    /// Advance a retained discovery scan once. SME scan failures are policy
    /// results; runtime/driver failures remain terminal errors.
    pub async fn drive_scan_once(
        &mut self,
    ) -> Result<Option<Result<Vec<fidl_sme::ScanResult>, fidl_sme::ScanErrorCode>>, ConnectError>
    {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        let Some(attempt) = self.scan_attempt.as_ref() else {
            return Err(ConnectError::Driver(DriverError::NoScanInProgress));
        };
        if std::time::Instant::now() >= attempt.deadline {
            return Err(self.contain_error(ConnectError::Timeout));
        }
        if let Err(error) = self.pump_once().await {
            return Err(if self.revoked {
                error
            } else {
                self.contain_error(error)
            });
        }
        match self
            .scan_attempt
            .as_mut()
            .expect("scan attempt checked above")
            .receiver
            .try_recv()
        {
            Ok(Some(result)) => {
                match &result {
                    Ok(results) => println!(
                        "client_scan_attempt stage=reply_ready success=true result_count={}",
                        results.len()
                    ),
                    Err(error) => println!(
                        "client_scan_attempt stage=reply_ready success=false error={error:?}"
                    ),
                }
                self.scan_attempt = None;
                Ok(Some(wlan_sme::client::convert_scan_result(result)))
            }
            Ok(None) => Ok(None),
            Err(_) => {
                println!("client_scan_attempt stage=reply_closed");
                Err(self.contain_error(ConnectError::Driver(DriverError::ScanTransactionClosed)))
            }
        }
    }

    fn begin_epoch(&mut self) {
        self.mlme.epoch.revoke();
        self.mlme.epoch = OperationEpoch::new();
        // Callers only start a replacement after the preceding driver drain
        // or completed scan. Callback routing changes at that boundary only.
        let mut upcalls = self.upcalls.lock().unwrap();
        upcalls.queue.clear();
        upcalls.raw_queued = 0;
        upcalls.epoch = self.mlme.epoch.clone();
    }

    fn begin_cleanup(
        &mut self,
        reason: fidl_sme::UserDisconnectReason,
        deadline: std::time::Instant,
    ) -> Result<std::time::Instant, ConnectError> {
        if let Some(cleanup) = &self.cleanup {
            return Ok(cleanup.deadline);
        }
        self.cleanup = Some(Cleanup {
            deadline,
            terminal: None,
            transaction_closed: false,
        });
        // Revoke the old continuation before SME emits cleanup. Keep driver
        // callbacks on the old epoch until its drain is certified.
        self.mlme.epoch.revoke();
        self.mlme.epoch = OperationEpoch::new();
        self.io.lock().unwrap().ethernet.set_link(false);
        let result = self.device.lock().unwrap().device.set_link_up(false);
        if let Err(status) = result {
            return Err(self.contain_error(ConnectError::Driver(DriverError::Ethernet(status))));
        }
        self.sme.on_disconnect_command(reason, Default::default());
        self.capture_sme_outputs(Some(self.mlme.epoch.clone()))?;
        Ok(deadline)
    }

    /// Request a policy-owned disconnect and drive the pinned SME/MLME until
    /// it reaches Idle and the driver certifies drain. The reply is the
    /// terminal acknowledgment; no second disconnect event is emitted.
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
        if self.connection.is_none()
            && self.cleanup.is_none()
            && sme_is_retry_quiescent(&self.sme.status())
            && self.mlme.is_idle()
        {
            return Ok(());
        }
        let deadline = self.begin_cleanup(reason, deadline)?;
        let result = loop {
            if std::time::Instant::now() >= deadline {
                break Err(ConnectError::Timeout);
            }
            let progressed = match self.pump_once().await {
                Ok(progressed) => progressed,
                Err(error) => break Err(error),
            };
            if sme_is_retry_quiescent(&self.sme.status()) && self.mlme.is_idle() {
                break Ok(());
            }
            if !progressed {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        };
        match result {
            Ok(()) if self.finish_failed_attempt_cleanup() => {
                self.connection = None;
                Ok(())
            }
            Ok(()) => Err(self.contain_error(ConnectError::Driver(DriverError::RetryCleanup))),
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
        let deadline = self.begin_cleanup(reason, deadline)?;
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
                        self.cleanup.as_mut().unwrap().terminal = Some(
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
                        self.cleanup.as_mut().unwrap().transaction_closed = true;
                        break;
                    }
                }
            }
            let cleanup = self.cleanup.as_ref().unwrap();
            if (cleanup.terminal.is_some() || cleanup.transaction_closed)
                && sme_is_retry_quiescent(&self.sme.status())
                && self.mlme.is_idle()
            {
                break Ok(());
            }
            if !progressed {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
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
        self.scan_attempt = None;
        self.io.lock().unwrap().ethernet.set_link(false);
        let terminal = self.cleanup.as_mut().unwrap().terminal.take();
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
                    self.connection = Some(Connection::Active(attempt.transaction));
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
        eprintln!("client_softmac_connect stage=failed error={error:?}");
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
        // A completed SME failure is retryable only when all owners can
        // prove quiescence. Keep the data plane closed before asking the
        // device to revoke and drain its attempt, then discard callbacks
        // that raced with that device-side drain.
        self.io.lock().unwrap().ethernet.set_link(false);
        let sme_quiescent = sme_is_retry_quiescent(&self.sme.status());
        let device_quiescent = sme_quiescent
            && self.mlme.is_idle()
            && self
                .device
                .lock()
                .unwrap()
                .device
                .finish_failed_connect_attempt()
                .is_ok();
        let drained = device_quiescent && drain_completed_attempt(&self.upcalls);
        if drained {
            self.mlme.epoch.revoke();
            self.cleanup = None;
        }
        drained
    }

    fn contain_error(&mut self, error: ConnectError) -> ConnectError {
        self.revoked = true;
        self.cleanup = None;
        self.mlme.abort();
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
        self.drain_connection_events()?;
        Ok(self.connection_events.pop_front())
    }

    fn drain_connection_events(&mut self) -> Result<(), ConnectError> {
        loop {
            if self.connection_events.len() >= 64 {
                return Err(self.contain_error(ConnectError::Driver(DriverError::UpcallOverflow)));
            }
            let Some(Connection::Active(connection)) = self.connection.as_mut() else {
                return Ok(());
            };
            let result = match connection.try_recv() {
                Ok(event) => {
                    if matches!(
                        &event,
                        wlan_sme::client::ConnectTransactionEvent::OnDisconnect { info }
                            if !info.is_sme_reconnecting
                    ) {
                        self.mlme.epoch.revoke();
                        self.io.lock().unwrap().ethernet.set_link(false);
                        let result = self.device.lock().unwrap().device.set_link_up(false);
                        if let Err(status) = result {
                            return Err(self.contain_error(ConnectError::Driver(
                                DriverError::Ethernet(status),
                            )));
                        }
                        self.connection = Some(Connection::EndedNeedsCleanup);
                    }
                    self.connection_events.push_back(event.into_fidl());
                    Ok(())
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::TryRecvError::Closed) => {
                    Err(self
                        .contain_error(ConnectError::Driver(DriverError::ConnectTransactionClosed)))
                }
            };
            result?;
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
    fn run_local_test(future: impl std::future::Future<Output = ()>) {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&executor, future);
    }

    use super::*;

    #[derive(Default)]
    struct Effects {
        calls: Vec<&'static str>,
        upcalls: Option<Box<dyn WlanSoftmacUpcalls>>,
        stop_failures: usize,
        reset_failure: bool,
        query_failure: bool,
        tx_flags: Vec<fidl_softmac::WlanTxInfoFlags>,
        channels: Vec<fidl_softmac::WlanSoftmacBaseSetChannelRequest>,
        simulate_ap: bool,
        suppress_auth_response: bool,
        reject_next_auth: bool,
        pending_rx: VecDeque<Vec<u8>>,
        retry_cleanup: bool,
        stale_callback_during_cleanup: bool,
        link_failure: bool,
        scan_id: u64,
        scan_offload: bool,
        empty_bands: bool,
        channel_completion: Option<oneshot::Receiver<Result<(), zx::Status>>>,
        clear_completion: Option<oneshot::Receiver<Result<(), zx::Status>>>,
    }

    #[derive(Clone)]
    struct Fake(Arc<Mutex<Effects>>);

    impl Fake {
        fn new(stop_failures: usize) -> (Self, Arc<Mutex<Effects>>) {
            let effects = Arc::new(Mutex::new(Effects {
                stop_failures,
                scan_offload: true,
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
                    band_caps: Some(if self.0.lock().unwrap().empty_bands {
                        vec![]
                    } else {
                        vec![fidl_softmac::WlanSoftmacBandCapability {
                            band: Some(fidl_ieee80211::WlanBand::TwoGhz),
                            basic_rates: Some(vec![0x82, 0x84]),
                            primary_channels: Some(vec![wlan_channel()]),
                            ..Default::default()
                        }]
                    }),
                    ..Default::default()
                }
            )
        }
        fn query_discovery_support(
            &mut self,
        ) -> Result<fidl_softmac::DiscoverySupport, zx::Status> {
            record!(
                self,
                "discovery",
                fidl_softmac::DiscoverySupport {
                    scan_offload: Some(fidl_softmac::ScanOffloadExtension {
                        supported: Some(self.0.lock().unwrap().scan_offload),
                        scan_cancel_supported: Some(true),
                    }),
                    ..Default::default()
                }
            )
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
            request: fidl_softmac::WlanSoftmacBaseSetChannelRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            let completion = {
                let mut effects = self.0.lock().unwrap();
                effects.channels.push(request);
                effects.calls.push("channel");
                effects.channel_completion.take()
            };
            async move {
                match completion {
                    Some(completion) => completion.await.unwrap_or(Err(zx::Status::CANCELED)),
                    None => Ok(()),
                }
            }
        }
        fn join_bss(
            &mut self,
            _: fidl_driver::JoinBssRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready(record!(self, "join", ()))
        }
        fn install_key(
            &mut self,
            _: fidl_softmac::WlanKeyConfiguration,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready(record!(self, "key", ()))
        }
        fn notify_association_complete(
            &mut self,
            _: fidl_softmac::WlanAssociationConfig,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready(record!(self, "assoc", ()))
        }
        fn clear_association(
            &mut self,
            _: fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            let completion = {
                let mut effects = self.0.lock().unwrap();
                effects.calls.push("clear");
                effects.clear_completion.take()
            };
            async move {
                match completion {
                    Some(completion) => completion.await.unwrap_or(Err(zx::Status::CANCELED)),
                    None => Ok(()),
                }
            }
        }
        fn start_passive_scan(
            &mut self,
            _: fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
        ) -> impl std::future::Future<
            Output = Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status>,
        > + 'static {
            std::future::ready({
                let mut effects = self.0.lock().unwrap();
                effects.calls.push("passive");
                effects.scan_id = effects.scan_id.checked_add(1).unwrap();
                Ok(fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse {
                    scan_id: Some(effects.scan_id),
                })
            })
        }
        fn start_active_scan(
            &mut self,
            _: fidl_softmac::WlanSoftmacStartActiveScanRequest,
        ) -> impl std::future::Future<
            Output = Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status>,
        > + 'static {
            std::future::ready(record!(self, "active", Default::default()))
        }
        fn cancel_scan(
            &mut self,
            _: fidl_softmac::WlanSoftmacBaseCancelScanRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready(record!(self, "cancel", ()))
        }
        fn update_wmm_parameters(
            &mut self,
            _: fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready(record!(self, "wmm", ()))
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
            replacement_ethernet: VecDeque::new(),
            unpublished_ethernet_device: None,
            pending_ethernet_devices: VecDeque::new(),
            ethernet_mac_address: [2, 0, 0, 0, 0, 1],
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
    fn deferred_downcall_releases_driver_lock_and_reports_completion_error() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (reply, completion) = oneshot::channel();
            effects.lock().unwrap().channel_completion = Some(completion);
            let (mut bridge, _) = parts(fake);
            let device = bridge.device.clone();
            let mut operation = std::pin::pin!(bridge.set_channel(
                wlan_channel(),
                fidl_ieee80211::ChannelBandwidth::Cbw20,
                wlan_channel(),
            ));
            assert!(operation.as_mut().now_or_never().is_none());
            assert!(
                device.try_lock().is_ok(),
                "downcall must release the driver before Pending"
            );
            assert_eq!(effects.lock().unwrap().calls, ["channel"]);
            reply.send(Err(zx::Status::IO)).unwrap();
            assert_eq!(operation.await, Err(zx::Status::IO));
        });
    }

    #[test]
    fn dropping_completion_does_not_stop_or_revoke_driver_work() {
        run_local_test(async {
            let (mut fake, effects) = Fake::new(0);
            let (reply, completion) = oneshot::channel();
            effects.lock().unwrap().channel_completion = Some(completion);
            let completion = fake.set_channel(Default::default());
            assert_eq!(
                effects.lock().unwrap().channels.len(),
                1,
                "admitted during call"
            );
            drop(completion);
            assert!(
                reply.send(Ok(())).is_err(),
                "only the result waiter was abandoned"
            );
            assert_eq!(effects.lock().unwrap().calls, ["channel"]);
            assert_eq!(effects.lock().unwrap().channels.len(), 1);
            fake.stop().unwrap();
            assert_eq!(effects.lock().unwrap().calls, ["channel", "stop"]);
        });
    }

    #[test]
    fn host_mlme_device_forwards_the_complete_applicable_surface() {
        run_local_test(async {
            let (fake, _) = Fake::new(0);
            let (mut device, effects) = parts(fake);
            (async {
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
            })
            .await;
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
        });
    }

    async fn runtime(fake: Fake) -> ClientRuntime<Fake> {
        runtime_with_device_info(fake, device_info()).await
    }

    async fn runtime_with_device_info(
        fake: Fake,
        device_info: fidl_mlme::DeviceInfo,
    ) -> ClientRuntime<Fake> {
        (ClientRuntime::new(
            fake,
            Default::default(),
            device_info,
            Default::default(),
            Default::default(),
            Default::default(),
        ))
        .await
        .unwrap()
    }

    async fn drain_mlme(runtime: &mut ClientRuntime<Fake>) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !runtime.mlme.is_idle() {
                runtime.mlme.changed.notified().await;
                runtime.mlme.check().unwrap();
            }
        })
        .await
        .expect("MLME completion notification");
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
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let mut runtime = runtime(fake).await;
            {
                let mut effects = effects.lock().unwrap();
                let upcalls = effects.upcalls.as_mut().unwrap();
                upcalls.recv(vec![0, 0], rx_info());
                upcalls.notify_scan_complete(zx::Status::OK, 9);
                upcalls.report_tx_result(tx_result());
            }
            assert!((runtime.pump_upcalls()).await.unwrap());
            assert!(runtime.upcalls.lock().unwrap().queue.is_empty());
        });
    }

    #[test]
    fn owner_can_drive_and_join_shutdown_while_mlme_awaits_driver_completion() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (reply, completion) = oneshot::channel();
            effects.lock().unwrap().channel_completion = Some(completion);
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            runtime
                .begin_connect(
                    connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert_eq!(runtime.drive_connect_once().await.unwrap(), None);
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while !effects.lock().unwrap().calls.contains(&"channel") {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(!runtime.mlme.is_idle());
            assert_eq!(runtime.drive_connect_once().await.unwrap(), None);
            runtime.shutdown().await.unwrap();
            assert!(runtime.mlme.task.is_none());
            assert!(reply.send(Ok(())).is_err());
            let effects = effects.lock().unwrap();
            assert_eq!(
                effects.calls.iter().filter(|call| **call == "stop").count(),
                1
            );
        });
    }

    #[test]
    fn cancel_suspended_join_revokes_followup_and_requires_driver_drain_before_retry() {
        for (certified, terminal_before_drop) in [(false, false), (true, false), (true, true)] {
            run_local_test(async {
                let (fake, effects) = Fake::new(0);
                let (reply, completion) = oneshot::channel();
                {
                    let mut effects = effects.lock().unwrap();
                    effects.channel_completion = Some(completion);
                    effects.retry_cleanup = certified;
                }
                let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
                runtime
                    .begin_connect(
                        connect_request(),
                        std::time::Instant::now() + std::time::Duration::from_secs(2),
                    )
                    .await
                    .unwrap();
                runtime.drive_connect_once().await.unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    while !effects.lock().unwrap().calls.contains(&"channel") {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                if terminal_before_drop {
                    // Model a result already queued when cancellation races with
                    // an outstanding MLME operation. Use a distinct result so
                    // the synthetic cancellation fallback cannot pass this test.
                    let (events, stream) = mpsc::unbounded();
                    events
                        .unbounded_send(
                            wlan_sme::client::ConnectTransactionEvent::OnConnectResult {
                                result: wlan_sme::client::ConnectResult::Success,
                                is_reconnect: true,
                            },
                        )
                        .unwrap();
                    runtime.connect_attempt.as_mut().unwrap().transaction = stream;
                }
                let original_deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(1);
                {
                    let mut cancellation = std::pin::pin!(runtime.cancel_connect(
                        fidl_sme::UserDisconnectReason::WlanSmeUnitTesting,
                        original_deadline,
                    ));
                    assert!(cancellation.as_mut().now_or_never().is_none());
                }
                assert_eq!(
                    runtime.cleanup.as_ref().unwrap().deadline,
                    original_deadline
                );
                assert!(!runtime.mlme.is_idle());
                if terminal_before_drop {
                    let cleanup = runtime.cleanup.as_ref().unwrap();
                    assert!(cleanup.terminal.is_some());
                    assert!(cleanup.transaction_closed);
                }
                assert!(
                    runtime
                        .begin_connect(
                            connect_request(),
                            std::time::Instant::now() + std::time::Duration::from_secs(5),
                        )
                        .await
                        .is_err()
                );
                reply.send(Ok(())).unwrap();
                let result = runtime
                    .cancel_connect(
                        fidl_sme::UserDisconnectReason::WlanSmeUnitTesting,
                        std::time::Instant::now() + std::time::Duration::from_secs(5),
                    )
                    .await;
                {
                    let effects = effects.lock().unwrap();
                    assert!(
                        !effects.calls.contains(&"join"),
                        "stale continuation issued join"
                    );
                    assert!(
                        !effects.calls.contains(&"tx"),
                        "stale continuation transmitted"
                    );
                    assert!(effects.calls.contains(&"finish_failed_connect_attempt"));
                }
                if certified {
                    let result = result.unwrap();
                    assert_eq!(
                        result.code,
                        if terminal_before_drop {
                            fidl_ieee80211::StatusCode::Success
                        } else {
                            fidl_ieee80211::StatusCode::Canceled
                        }
                    );
                    assert_eq!(result.is_reconnect, terminal_before_drop);
                    assert!(runtime.cleanup.is_none());
                    assert!(!runtime.revoked);
                    assert!(runtime.mlme.task.is_some());
                    effects.lock().unwrap().simulate_ap = true;
                    assert_eq!(
                        runtime
                            .connect(
                                connect_request(),
                                std::time::Instant::now() + std::time::Duration::from_secs(1),
                            )
                            .await
                            .unwrap()
                            .code,
                        fidl_ieee80211::StatusCode::Success
                    );
                } else {
                    assert!(result.is_err());
                    assert!(runtime.revoked);
                }
                runtime.shutdown().await.unwrap();
            });
        }
    }

    #[test]
    fn disconnect_without_a_station_acknowledges_without_hardware_effects() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let mut runtime = runtime(fake).await;
            let before = effects.lock().unwrap().calls.clone();
            let peer = [2, 0, 0, 0, 0, 2];
            runtime
                .mlme
                .enqueue(MlmeInput::Request(wlan_sme::MlmeRequest::Deauthenticate(
                    fidl_mlme::DeauthenticateRequest {
                        peer_sta_address: peer,
                        reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                    },
                )))
                .unwrap();
            drain_mlme(&mut runtime).await;
            runtime.mlme.check().unwrap();
            assert!(matches!(
                runtime.events.try_recv().unwrap().1,
                fidl_mlme::MlmeEvent::DeauthenticateConf { resp } if resp.peer_sta_address == peer
            ));
            assert_eq!(effects.lock().unwrap().calls, before);
            runtime.shutdown().await.unwrap();
        });
    }

    #[test]
    fn terminal_shutdown_joins_mlme_without_running_queued_downcalls() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            runtime
                .begin_connect(
                    connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert_eq!(runtime.drive_connect_once().await.unwrap(), None);
            assert!(!runtime.mlme.is_idle());
            runtime.shutdown().await.unwrap();
            assert!(runtime.mlme.task.is_none());
            assert!(runtime.mlme.is_idle());
            assert!(!effects.lock().unwrap().calls.contains(&"channel"));
        });
    }

    #[test]
    fn canceled_shutdown_retains_the_task_until_a_retry_joins_it() {
        run_local_test(async {
            use std::future::Future;
            let (fake, effects) = Fake::new(0);
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            {
                let mut shutdown = std::pin::pin!(runtime.shutdown());
                let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
                assert!(shutdown.as_mut().poll(&mut cx).is_pending());
            }
            assert!(
                runtime.mlme.task.is_some(),
                "cancellation must not detach MLME"
            );
            runtime.shutdown().await.unwrap();
            assert!(runtime.mlme.task.is_none());
            assert!(runtime.mlme.is_idle());
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
        });
    }

    #[test]
    fn mlme_runs_and_signals_completion_without_manual_owner_polling() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            runtime
                .begin_scan(
                    fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest {
                        channels: vec![],
                    }),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert_eq!(runtime.drive_scan_once().await.unwrap(), None);
            assert!(!runtime.mlme.is_idle());
            drain_mlme(&mut runtime).await;
            assert!(effects.lock().unwrap().calls.contains(&"passive"));
            runtime.shutdown().await.unwrap();
        });
    }

    #[test]
    fn control_at_capacity_evicts_oldest_raw_and_preserves_remaining_order() {
        run_local_test(async {
            let state = Arc::new(Mutex::new(UpcallQueue {
                epoch: OperationEpoch::new(),
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
        });
    }

    #[test]
    fn all_control_overflow_is_fatal_and_next_pump_contains_device() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let mut runtime = runtime(fake).await;
            {
                let mut effects = effects.lock().unwrap();
                let upcalls = effects.upcalls.as_mut().unwrap();
                for _ in 0..=UPCALL_QUEUE_CAPACITY {
                    upcalls.report_tx_result(tx_result());
                }
            }
            assert_eq!(
                (runtime.pump_associated_once()).await,
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
        });
    }

    #[test]
    fn mlme_initialization_failure_never_starts_the_device() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().query_failure = true;
            let result = (ClientRuntime::new(
                fake,
                Default::default(),
                device_info(),
                Default::default(),
                Default::default(),
                Default::default(),
            ))
            .await;
            assert!(result.is_err());
            assert_eq!(effects.lock().unwrap().calls, ["query"]);
        });
    }

    #[test]
    fn stop_drains_queued_callbacks_and_excludes_late_callbacks() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let mut runtime = runtime(fake).await;
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
        });
    }

    #[test]
    fn failed_connect_terminally_revokes_host_state_even_when_reset_fails() {
        run_local_test(async {
            for reset_failure in [false, true] {
                let (fake, effects) = Fake::new(0);
                effects.lock().unwrap().reset_failure = reset_failure;
                let mut runtime = runtime(fake).await;
                assert!(runtime.take_ethernet_device().is_none());
                effects
                    .lock()
                    .unwrap()
                    .upcalls
                    .as_mut()
                    .unwrap()
                    .recv(vec![0, 0], rx_info());

                runtime
                    .begin_connect(
                        connect_request(),
                        std::time::Instant::now() + std::time::Duration::from_secs(1),
                    )
                    .await
                    .unwrap();
                runtime.connect_attempt.as_mut().unwrap().deadline = std::time::Instant::now();
                let error = runtime.drive_connect_once().await.unwrap_err();
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
                    (runtime.connect(connect_request(), std::time::Instant::now())).await,
                    Err(ConnectError::Driver(DriverError::Stopped))
                );
                assert_eq!(
                    (runtime.pump_associated_once()).await,
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
        });
    }

    #[test]
    fn completed_failure_drains_stale_callbacks_and_allows_successful_retry() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            {
                let mut state = effects.lock().unwrap();
                state.simulate_ap = true;
                state.reject_next_auth = true;
                state.retry_cleanup = true;
                state.stale_callback_during_cleanup = true;
            }
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            assert!(runtime.take_ethernet_device().is_none());

            let failure = (runtime.connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            ))
            .await
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

            let result = (runtime.connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            ))
            .await
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
        });
    }

    #[test]
    fn abandoned_admission_retains_terminal_event_and_original_cleanup_deadline() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().simulate_ap = true;
            effects.lock().unwrap().retry_cleanup = true;
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            runtime.connect(connect_request(), deadline).await.unwrap();
            let (reply, completion) = oneshot::channel();
            effects.lock().unwrap().clear_completion = Some(completion);
            let (events, stream) = mpsc::unbounded();
            runtime.connection = Some(Connection::Active(stream));
            events
                .unbounded_send(wlan_sme::client::ConnectTransactionEvent::OnDisconnect {
                    info: fidl_sme::DisconnectInfo {
                        is_sme_reconnecting: false,
                        disconnect_source: fidl_sme::DisconnectSource::User(
                            fidl_sme::UserDisconnectReason::FailedToConnect,
                        ),
                    },
                })
                .unwrap();
            drop(events);
            {
                let mut admission =
                    std::pin::pin!(runtime.begin_connect(connect_request(), deadline));
                tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    loop {
                        assert!(admission.as_mut().now_or_never().is_none());
                        if effects.lock().unwrap().calls.contains(&"clear") {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            assert_eq!(runtime.cleanup.as_ref().unwrap().deadline, deadline);
            assert_eq!(runtime.connection_events.len(), 1);
            reply.send(Ok(())).unwrap();
            runtime
                .disconnect(
                    fidl_sme::UserDisconnectReason::FailedToConnect,
                    deadline + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert!(matches!(
                runtime.next_connection_event().unwrap(),
                Some(fidl_sme::ConnectTransactionEvent::OnDisconnect { .. })
            ));
            assert!(runtime.next_connection_event().unwrap().is_none());
            assert!(runtime.connect_attempt.is_none());
            runtime.shutdown().await.unwrap();
        });
    }

    #[test]
    fn terminal_disconnect_requires_certified_cleanup_before_connect_or_scan() {
        for (certified, scan) in [(false, false), (true, false), (false, true), (true, true)] {
            run_local_test(async {
                let (fake, effects) = Fake::new(0);
                effects.lock().unwrap().retry_cleanup = certified;
                let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
                let (events, stream) = mpsc::unbounded();
                runtime.connection = Some(Connection::Active(stream));
                events
                    .unbounded_send(wlan_sme::client::ConnectTransactionEvent::OnDisconnect {
                        info: fidl_sme::DisconnectInfo {
                            is_sme_reconnecting: false,
                            disconnect_source: fidl_sme::DisconnectSource::User(
                                fidl_sme::UserDisconnectReason::FailedToConnect,
                            ),
                        },
                    })
                    .unwrap();
                drop(events);
                // Do not consume the terminal event first: admission must
                // reconcile the authoritative stream itself.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
                let result = if scan {
                    runtime
                        .begin_scan(
                            fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest {
                                channels: vec![],
                            }),
                            deadline,
                        )
                        .await
                } else {
                    runtime.begin_connect(connect_request(), deadline).await
                };
                assert_eq!(result.is_ok(), certified);
                assert!(
                    effects
                        .lock()
                        .unwrap()
                        .calls
                        .contains(&"finish_failed_connect_attempt")
                );
                if certified {
                    assert!(matches!(
                        runtime.next_connection_event().unwrap(),
                        Some(fidl_sme::ConnectTransactionEvent::OnDisconnect { .. })
                    ));
                    assert!(runtime.next_connection_event().unwrap().is_none());
                    assert!(runtime.cleanup.is_none());
                    if scan {
                        assert_eq!(runtime.scan_attempt.as_ref().unwrap().deadline, deadline);
                    } else {
                        assert_eq!(runtime.connect_attempt.as_ref().unwrap().deadline, deadline);
                        effects.lock().unwrap().simulate_ap = true;
                        loop {
                            if let Some(result) = runtime.drive_connect_once().await.unwrap() {
                                assert_eq!(result.code, fidl_ieee80211::StatusCode::Success);
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                    }
                } else {
                    assert!(runtime.revoked);
                }
                runtime.shutdown().await.unwrap();
            });
        }
    }

    #[test]
    fn reconnecting_disconnect_retains_the_transaction_stream() {
        run_local_test(async {
            let (fake, _) = Fake::new(0);
            let mut runtime = runtime(fake).await;
            let (events, stream) = mpsc::unbounded();
            runtime.connection = Some(Connection::Active(stream));
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
        });
    }

    #[test]
    fn retained_connect_attempt_can_be_canceled_and_reused() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            {
                let mut state = effects.lock().unwrap();
                state.simulate_ap = true;
                state.suppress_auth_response = true;
                state.retry_cleanup = true;
            }
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            runtime
                .begin_connect(
                    connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert_eq!(
                runtime
                    .begin_connect(
                        connect_request(),
                        std::time::Instant::now() + std::time::Duration::from_secs(1),
                    )
                    .await,
                Err(ConnectError::Driver(DriverError::ConnectInProgress))
            );

            let result = (runtime.cancel_connect(
                fidl_sme::UserDisconnectReason::FailedToConnect,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            ))
            .await
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
                .await
                .unwrap();
            let result = loop {
                if let Some(result) = (runtime.drive_connect_once()).await.unwrap() {
                    break result;
                }
                tokio::task::yield_now().await;
            };
            assert_eq!(result.code, fidl_ieee80211::StatusCode::Success);
        });
    }

    #[test]
    fn canceled_connect_without_certified_cleanup_is_contained() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            {
                let mut state = effects.lock().unwrap();
                state.simulate_ap = true;
                state.suppress_auth_response = true;
            }
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            runtime
                .begin_connect(
                    connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();

            assert_eq!(
                (runtime.cancel_connect(
                    fidl_sme::UserDisconnectReason::FailedToConnect,
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                ))
                .await,
                Err(ConnectError::Driver(DriverError::RetryCleanup))
            );
            assert!(runtime.revoked);
            let state = effects.lock().unwrap();
            assert!(state.calls.contains(&"finish_failed_connect_attempt"));
            assert!(state.calls.contains(&"reset"));
        });
    }

    #[test]
    fn non_ht_client_joins_wide_bss_on_primary_20mhz() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().simulate_ap = true;
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            let mut request = connect_request();
            request.bss_description.bandwidth = fidl_ieee80211::ChannelBandwidth::Cbw40;
            (runtime.connect(
                request,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            ))
            .await
            .unwrap();
            let state = effects.lock().unwrap();
            assert_eq!(state.channels.len(), 1);
            assert_eq!(state.channels[0].primary, Some(wlan_channel()));
            assert_eq!(
                state.channels[0].bandwidth,
                Some(fidl_ieee80211::ChannelBandwidth::Cbw20)
            );
            assert_eq!(
                state.channels[0].vht_secondary_80_channel.unwrap().number,
                0
            );
            assert!(state.calls.contains(&"assoc"));
        });
    }

    #[test]
    fn successful_connection_retains_events_and_disconnects_before_reuse() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().simulate_ap = true;
            effects.lock().unwrap().retry_cleanup = true;
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            (runtime.connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            ))
            .await
            .unwrap();

            (runtime.pump_associated_once()).await.unwrap();
            assert_eq!(
                (runtime.connect(
                    connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                ))
                .await,
                Err(ConnectError::Driver(DriverError::AlreadyConnected))
            );
            (runtime.disconnect(
                fidl_sme::UserDisconnectReason::FailedToConnect,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            ))
            .await
            .unwrap();
            assert!(runtime.next_connection_event().unwrap().is_none());
            assert!(sme_is_retry_quiescent(&runtime.sme().status()));

            (runtime.connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            ))
            .await
            .unwrap();
            runtime.stop().unwrap();
            assert_eq!(
                runtime.next_connection_event(),
                Err(ConnectError::Driver(DriverError::Stopped))
            );
        });
    }

    #[test]
    fn unsupported_roam_preserves_the_current_connection() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().simulate_ap = true;
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            assert_eq!(
                runtime.roam(fidl_sme::RoamRequest {
                    bss_description: connect_request().bss_description,
                }),
                Err(ConnectError::Driver(DriverError::NotConnected))
            );
            (runtime.connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            ))
            .await
            .unwrap();

            assert_eq!(
                runtime.roam(fidl_sme::RoamRequest {
                    bss_description: connect_request().bss_description,
                }),
                Err(ConnectError::Driver(DriverError::RoamUnsupported))
            );
            assert!(runtime.connection.is_some());
            assert!(runtime.sme().status().is_connected());
            assert_eq!((runtime.drive_service_once()).await, Ok(false));
        });
    }

    #[test]
    fn service_drive_treats_a_revoked_ethernet_generation_as_idle() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().simulate_ap = true;
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            (runtime.connect(
                connect_request(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            ))
            .await
            .unwrap();
            runtime.io.lock().unwrap().ethernet.set_link(false);
            assert_eq!((runtime.drive_service_once()).await, Ok(false));
        });
    }

    #[test]
    fn discovery_scan_result_is_retained_for_service_driving() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            runtime
                .begin_scan(
                    fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest {
                        channels: vec![],
                    }),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert_eq!(
                runtime
                    .begin_scan(
                        fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest {
                            channels: vec![]
                        }),
                        std::time::Instant::now() + std::time::Duration::from_secs(1),
                    )
                    .await,
                Err(ConnectError::Driver(DriverError::ScanInProgress))
            );
            assert_eq!((runtime.drive_scan_once()).await.unwrap(), None);
            drain_mlme(&mut runtime).await;
            let scan_id = effects.lock().unwrap().scan_id;
            effects
                .lock()
                .unwrap()
                .upcalls
                .as_mut()
                .unwrap()
                .notify_scan_complete(zx::Status::OK, scan_id);

            let result = loop {
                if let Some(result) = (runtime.drive_scan_once()).await.unwrap() {
                    break result;
                }
                drain_mlme(&mut runtime).await;
            };
            assert_eq!(result, Ok(vec![]));
            assert_eq!(
                (runtime.drive_scan_once()).await,
                Err(ConnectError::Driver(DriverError::NoScanInProgress))
            );
        });
    }

    #[test]
    fn rejected_discovery_scan_is_retained_as_a_policy_result() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().scan_offload = false;
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            runtime
                .begin_scan(
                    fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest {
                        channels: vec![],
                    }),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert_eq!(runtime.drive_scan_once().await.unwrap(), None);
            drain_mlme(&mut runtime).await;
            assert_eq!(
                (runtime.drive_scan_once()).await.unwrap(),
                Some(Err(fidl_sme::ScanErrorCode::NotSupported))
            );
        });
    }

    #[test]
    fn empty_radio_capabilities_construct_and_reject_scan_without_hardware() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            {
                let mut effects = effects.lock().unwrap();
                effects.empty_bands = true;
                effects.scan_offload = false;
            }
            let info = device_info();
            let resources = PreparedRuntimeResources::new(info.sta_addr).unwrap();
            let mut runtime = (ClientRuntime::new_with_prepared_resources(
                fake,
                Default::default(),
                info,
                Default::default(),
                Default::default(),
                Default::default(),
                resources,
            ))
            .await
            .unwrap();
            runtime
                .begin_scan(
                    fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest {
                        channels: vec![],
                    }),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert_eq!(runtime.drive_scan_once().await.unwrap(), None);
            drain_mlme(&mut runtime).await;
            assert_eq!(
                (runtime.drive_scan_once()).await.unwrap(),
                // Empty channel inventory is rejected by pinned MLME before the
                // driver is called; SME currently maps InvalidArgs to InternalError.
                Some(Err(fidl_sme::ScanErrorCode::InternalError))
            );
            runtime.stop().unwrap();
            let effects = effects.lock().unwrap();
            assert!(!effects.calls.contains(&"passive"));
            assert!(!effects.calls.contains(&"active"));
            assert!(effects.calls.contains(&"stop"));
        });
    }

    #[test]
    fn queued_mlme_work_cannot_be_certified_retry_quiescent() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().retry_cleanup = true;
            let mut runtime = runtime(fake).await;
            runtime
                .mlme
                .enqueue(MlmeInput::Upcall(Upcall::ScanComplete {
                    status: zx::Status::OK,
                    scan_id: 1,
                }))
                .unwrap();
            assert!(!runtime.finish_failed_attempt_cleanup());
            assert!(
                !effects
                    .lock()
                    .unwrap()
                    .calls
                    .contains(&"finish_failed_connect_attempt")
            );
        });
    }

    #[test]
    fn only_idle_sme_is_retry_quiescent() {
        run_local_test(async {
            assert!(sme_is_retry_quiescent(
                &wlan_sme::client::ClientSmeStatus::Idle
            ));
            assert!(!sme_is_retry_quiescent(
                &wlan_sme::client::ClientSmeStatus::Roaming([1; 6].into())
            ));
        });
    }

    #[test]
    fn link_up_after_hup_publishes_a_fresh_ethernet_generation() {
        run_local_test(async {
            let mac = [2, 0, 0, 0, 0, 1];
            let capacity = 3;
            let (old_host, ethernet) = ethernet_port(mac, capacity).unwrap();
            let replacements = (0..2)
                .map(|_| ethernet_port(mac, capacity).unwrap())
                .collect();
            let (fake, effects) = Fake::new(0);
            let device = Arc::new(Mutex::new(StartedDevice {
                device: fake,
                stop_pending: false,
            }));
            let io = Arc::new(Mutex::new(HostIo {
                ethernet,
                replacement_ethernet: replacements,
                unpublished_ethernet_device: None,
                pending_ethernet_devices: VecDeque::new(),
                ethernet_mac_address: mac,
                minstrel: None,
            }));
            let mut host_device = HostMlmeDevice::new(device, io.clone());

            (host_device.set_ethernet_status(LinkStatus::UP))
                .await
                .unwrap();
            (host_device.set_ethernet_status(LinkStatus::DOWN))
                .await
                .unwrap();
            assert!(io.lock().unwrap().ethernet.is_closed());
            assert_eq!(old_host.properties().unwrap().mac_address, mac);

            effects.lock().unwrap().link_failure = true;
            assert_eq!(
                (host_device.set_ethernet_status(LinkStatus::UP)).await,
                Err(zx::Status::IO)
            );
            assert!(io.lock().unwrap().ethernet.is_closed());
            assert!(io.lock().unwrap().pending_ethernet_devices.is_empty());

            effects.lock().unwrap().link_failure = false;
            (host_device.set_ethernet_status(LinkStatus::UP))
                .await
                .unwrap();
            let mut state = io.lock().unwrap();
            assert!(!state.ethernet.is_closed());
            assert_eq!(state.pending_ethernet_devices.len(), 1);
            assert_eq!(old_host.properties(), None);
            let replacement = state.pending_ethernet_devices.pop_front().unwrap();
            assert_eq!(replacement.properties().unwrap().mac_address, mac);
            drop(state);

            (host_device.set_ethernet_status(LinkStatus::UP))
                .await
                .unwrap();
            assert!(io.lock().unwrap().pending_ethernet_devices.is_empty());
            (host_device.set_ethernet_status(LinkStatus::DOWN))
                .await
                .unwrap();
            assert_eq!(
                (host_device.set_ethernet_status(LinkStatus::UP)).await,
                Err(zx::Status::NO_RESOURCES)
            );
        });
    }

    #[test]
    fn link_down_revokes_unpublished_and_pending_ethernet_generations() {
        run_local_test(async {
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
                    replacement_ethernet: VecDeque::new(),
                    unpublished_ethernet_device: Some(host),
                    pending_ethernet_devices: VecDeque::new(),
                    ethernet_mac_address: mac,
                    minstrel: None,
                }));
                (HostMlmeDevice::new(device, io.clone()), io)
            };

            let (mut before_up, before_up_io) = make_host();
            (before_up.set_ethernet_status(LinkStatus::DOWN))
                .await
                .unwrap();
            let before_up_io = before_up_io.lock().unwrap();
            assert!(before_up_io.unpublished_ethernet_device.is_none());
            assert!(before_up_io.pending_ethernet_devices.is_empty());
            assert!(before_up_io.ethernet.is_closed());
            drop(before_up_io);

            let (mut while_pending, while_pending_io) = make_host();
            (while_pending.set_ethernet_status(LinkStatus::UP))
                .await
                .unwrap();
            assert_eq!(
                while_pending_io
                    .lock()
                    .unwrap()
                    .pending_ethernet_devices
                    .len(),
                1
            );
            (while_pending.set_ethernet_status(LinkStatus::DOWN))
                .await
                .unwrap();
            let while_pending_io = while_pending_io.lock().unwrap();
            assert!(while_pending_io.unpublished_ethernet_device.is_none());
            assert!(while_pending_io.pending_ethernet_devices.is_empty());
            assert!(while_pending_io.ethernet.is_closed());
        });
    }

    #[test]
    fn completed_failure_without_retry_safe_driver_cleanup_is_terminal() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            {
                let mut state = effects.lock().unwrap();
                state.simulate_ap = true;
                state.reject_next_auth = true;
            }
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;

            assert_eq!(
                (runtime.connect(
                    connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                ))
                .await,
                Err(ConnectError::Driver(DriverError::RetryCleanup))
            );
            assert!(runtime.revoked);
            let state = effects.lock().unwrap();
            assert!(state.calls.contains(&"finish_failed_connect_attempt"));
            assert!(state.calls.contains(&"reset"));
        });
    }

    #[test]
    fn failed_stop_is_retried_but_successful_stop_is_not() {
        run_local_test(async {
            let (fake, effects) = Fake::new(1);
            let mut runtime_instance = runtime(fake).await;
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
            let mut runtime_after_failure = runtime(fake).await;
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
        });
    }

    #[test]
    fn successful_and_failed_stop_make_connect_and_pump_terminal() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let mut stopped = runtime(fake).await;
            stopped.stop().unwrap();
            let calls_after_stop = effects.lock().unwrap().calls.clone();
            assert_eq!(
                (stopped.connect(
                    connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                ))
                .await,
                Err(ConnectError::Driver(DriverError::Stopped))
            );
            assert_eq!(*effects.lock().unwrap().calls, calls_after_stop);

            let (fake, effects) = Fake::new(1);
            let mut stop_failed = runtime(fake).await;
            assert_eq!(stop_failed.stop(), Err(zx::Status::IO));
            let calls_after_stop = effects.lock().unwrap().calls.clone();
            assert_eq!(
                (stop_failed.pump_associated_once()).await,
                Err(ConnectError::Driver(DriverError::Stopped))
            );
            assert_eq!(*effects.lock().unwrap().calls, calls_after_stop);
        });
    }
}
