// SPDX-License-Identifier: GPL-2.0-only

//! Chip-independent ownership of the pinned Fuchsia client MLME/SME/RSN loop.

use crate::driver::{Command, DriverActor, DriverHandle, HardwareOwner, OwnerCommand};
use crate::ethernet::{
    DriverEthernetPort, EthernetIngressError, HostEthernetDevice, ethernet_port,
};
use crate::sme::client::{ConnectTransaction, Request as SmeRequest, ScanReceiver};
use crate::{
    ClientRuntimeDriver, OperationContext, OperationEpoch, StationOffloadSupport, WlanSoftmac, WlanSoftmacLifecycle,
    WlanSoftmacUpcalls,
};
use fdf::ArenaStaticBox;
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_driver as fidl_driver;
use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
use fidl_fuchsia_wlan_mlme as fidl_mlme;
use fidl_fuchsia_wlan_sme as fidl_sme;
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use futures::FutureExt;
use futures::channel::{mpsc, oneshot};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wlan_mlme::device::{DeviceOps, LinkStatus};

const UPCALL_QUEUE_CAPACITY: usize = 256;
const ETHERNET_QUEUE_CAPACITY: usize = 256;
/// Bounded Ethernet generations created before production lockdown. Exhaustion
/// terminates the runtime cleanly rather than creating a descriptor post-lock.
pub const PREPARED_ETHERNET_GENERATIONS: usize = 4;

pub(super) struct ScanOperation {
    pub(super) transaction_id: u64,
    pub(super) device_scan_id: Option<u64>,
    pub(super) context: OperationContext,
}

pub(super) struct MlmeExecution {
    pub(super) operation: Arc<Mutex<OperationContext>>,
    pub(super) scan: RefCell<Option<ScanOperation>>,
    pub(super) epoch: RefCell<OperationEpoch>,
    pub(super) rejected: Cell<bool>,
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

pub(super) struct HostIo {
    pub(super) ethernet: DriverEthernetPort,
    pub(super) replacement_ethernet: VecDeque<(HostEthernetDevice, DriverEthernetPort)>,
    pub(super) unpublished_ethernet_device: Option<HostEthernetDevice>,
    pub(super) pending_ethernet_devices: VecDeque<HostEthernetDevice>,
    pub(super) ethernet_mac_address: [u8; 6],
    pub(super) minstrel: Option<wlan_mlme::MinstrelWrapper>,
}

pub(super) enum Upcall {
    ConnectionLoss([u8; 6]),
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

pub(super) struct UpcallQueue {
    pub(super) epoch: OperationEpoch,
    pub(super) live: bool,
    pub(super) overflowed: bool,
    pub(super) raw_queued: usize,
    pub(super) queue: VecDeque<Upcall>,
    pub(super) notify: Arc<tokio::sync::Notify>,
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
                state.notify.notify_one();
                state.queue.clear();
                state.raw_queued = 0;
                return;
            }
        }
        state.queue.push_back(upcall);
        state.notify.notify_one();
    }
}

impl WlanSoftmacUpcalls for UpcallSender {
    fn notify_connection_loss(&mut self, peer: [u8; 6]) {
        self.push_control(Upcall::ConnectionLoss(peer));
    }

    fn recv(&mut self, bytes: Vec<u8>, info: fidl_softmac::WlanRxInfo) {
        let mut state = self.0.lock().unwrap();
        if state.live && state.queue.len() < UPCALL_QUEUE_CAPACITY {
            state.raw_queued += 1;
            state.queue.push_back(Upcall::Recv { bytes, info });
            state.notify.notify_one();
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

fn ethernet_status(error: EthernetIngressError) -> zx::Status {
    match error {
        EthernetIngressError::Closed => zx::Status::CANCELED,
        EthernetIngressError::LinkDown => zx::Status::BAD_STATE,
        EthernetIngressError::Backpressure => zx::Status::SHOULD_WAIT,
        EthernetIngressError::InvalidFrame(_) => zx::Status::IO_DATA_INTEGRITY,
    }
}

struct HostMlmeDevice {
    station_offload: StationOffloadSupport,
    execution: Rc<MlmeExecution>,
    driver: DriverHandle,
    io: Arc<Mutex<HostIo>>,
    event_sink: mpsc::Sender<(OperationEpoch, fidl_mlme::MlmeEvent)>,
    event_stream: Option<mpsc::Receiver<(OperationEpoch, fidl_mlme::MlmeEvent)>>,
    overflow: Arc<std::sync::atomic::AtomicBool>,
}

impl HostMlmeDevice {
    fn new(driver: DriverHandle, io: Arc<Mutex<HostIo>>, station_offload: StationOffloadSupport) -> Self {
        let (event_sink, event_stream) = mpsc::channel(UPCALL_QUEUE_CAPACITY);
        let operation =
            OperationContext::new(std::time::Instant::now() + std::time::Duration::from_secs(3));
        Self {
            station_offload,
            execution: Rc::new(MlmeExecution {
                operation: Arc::new(Mutex::new(operation.clone())),
                scan: RefCell::new(None),
                epoch: RefCell::new(operation.epoch().clone()),
                rejected: Cell::new(false),
            }),
            driver,
            io,
            event_sink,
            event_stream: Some(event_stream),
            overflow: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
    async fn request<T>(
        &mut self,
        make: impl FnOnce(oneshot::Sender<Result<T, zx::Status>>) -> Command,
    ) -> Result<T, zx::Status> {
        let epoch = self.execution.epoch.borrow().clone();
        let (reply, receiver) = oneshot::channel();
        let result = match self.driver.send(epoch.clone(), make(reply)) {
            Ok(()) => receiver.await.unwrap_or(Err(zx::Status::CANCELED)),
            Err(status) => Err(status),
        };
        if matches!(&result, Err(status) if *status == zx::Status::CANCELED) && !epoch.is_live() {
            self.execution.rejected.set(true);
        }
        result
    }
}

impl DeviceOps for HostMlmeDevice {
    fn power_save_offload(&self) -> bool {
        self.station_offload.power_save
    }

    fn connection_monitor_offload(&self) -> bool {
        self.station_offload.connection_monitor
    }

    async fn wlan_softmac_query_response(
        &mut self,
    ) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status> {
        self.request(|reply| Command::Query((), reply)).await
    }
    async fn discovery_support(&mut self) -> Result<fidl_softmac::DiscoverySupport, zx::Status> {
        self.request(|reply| Command::Discovery((), reply)).await
    }
    async fn mac_sublayer_support(
        &mut self,
    ) -> Result<fidl_common::MacSublayerSupport, zx::Status> {
        self.request(|reply| Command::MacSublayer((), reply)).await
    }
    async fn security_support(&mut self) -> Result<fidl_common::SecuritySupport, zx::Status> {
        self.request(|reply| Command::Security((), reply)).await
    }
    async fn spectrum_management_support(
        &mut self,
    ) -> Result<fidl_common::SpectrumManagementSupport, zx::Status> {
        self.request(|reply| Command::Spectrum((), reply)).await
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
        self.driver.send(
            self.execution.epoch.borrow().clone(),
            Command::Transmit(
                self.execution.operation.lock().unwrap().clone(),
                buffer.to_vec(),
                flags,
            ),
        )
    }
    async fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
        self.execution.admit()?;
        if status != LinkStatus::UP {
            {
                let mut io = self.io.lock().unwrap();
                io.pending_ethernet_devices.clear();
                io.unpublished_ethernet_device = None;
                io.ethernet.set_link(false);
            }
            return self.request(|reply| Command::Link(false, reply)).await;
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
        if let Err(status) = self.request(|reply| Command::Link(true, reply)).await {
            let mut io = self.io.lock().unwrap();
            io.pending_ethernet_devices.clear();
            io.unpublished_ethernet_device = None;
            io.ethernet.teardown();
            return Err(status);
        }
        self.execution.admit()?;
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
        let context = self
            .execution
            .scan
            .borrow()
            .as_ref()
            .map(|scan| scan.context.clone())
            .unwrap_or_else(|| self.execution.operation.lock().unwrap().clone());
        let result = self
            .request(|reply| {
                Command::Channel(
                    context,
                    fidl_softmac::WlanSoftmacBaseSetChannelRequest {
                        primary: Some(primary),
                        bandwidth: Some(bandwidth),
                        vht_secondary_80_channel: Some(secondary),
                    },
                    reply,
                )
            })
            .await;
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
        let context = self
            .execution
            .scan
            .borrow()
            .as_ref()
            .ok_or(zx::Status::BAD_STATE)?
            .context
            .clone();
        context.check(std::time::Instant::now())?;
        eprintln!(
            "client_softmac_scan stage=bridge_enter kind=passive channel_count={} min_channel_time={:?} max_channel_time={:?} min_home_time={:?}",
            request.channels.as_ref().map_or(0, Vec::len),
            request.min_channel_time,
            request.max_channel_time,
            request.min_home_time,
        );
        let response = self
            .request(|reply| Command::PassiveScan(context, request.clone(), reply))
            .await;
        eprintln!(
            "client_softmac_scan stage=bridge_complete kind=passive status={}",
            if response.is_ok() { "ok" } else { "error" }
        );
        if let Ok(response) = &response {
            self.execution
                .scan
                .borrow_mut()
                .as_mut()
                .ok_or(zx::Status::BAD_STATE)?
                .device_scan_id = response.scan_id;
        }
        response
    }
    async fn start_active_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
        self.execution.admit()?;
        let context = self
            .execution
            .scan
            .borrow()
            .as_ref()
            .ok_or(zx::Status::BAD_STATE)?
            .context
            .clone();
        context.check(std::time::Instant::now())?;
        let response = self
            .request(|reply| Command::ActiveScan(context, request.clone(), reply))
            .await?;
        self.execution
            .scan
            .borrow_mut()
            .as_mut()
            .ok_or(zx::Status::BAD_STATE)?
            .device_scan_id = response.scan_id;
        Ok(response)
    }
    async fn cancel_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        self.execution.admit()?;
        if let Some(scan) = self.execution.scan.borrow().as_ref()
            && scan.device_scan_id.is_some()
            && scan.device_scan_id == request.scan_id
        {
            scan.context.revoke();
        }
        self.request(|reply| Command::CancelScan(request.clone(), reply))
            .await
    }
    async fn join_bss(&mut self, request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        self.execution.admit()?;
        let context = self.execution.operation.lock().unwrap().clone();
        self.request(|reply| Command::Join(context, request.clone(), reply))
            .await
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
        let context = self.execution.operation.lock().unwrap().clone();
        self.request(|reply| Command::Key(context, key.clone(), reply))
            .await
    }
    async fn notify_association_complete(
        &mut self,
        config: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        self.execution.admit()?;
        let context = self.execution.operation.lock().unwrap().clone();
        eprintln!("client_association stage=configure_enter config={config:?}");
        let result = self
            .request(|reply| Command::Association(context, config, reply))
            .await;
        eprintln!("client_association stage=configure_complete result={result:?}");
        result
    }
    async fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        self.execution.admit()?;
        let context = self.execution.operation.lock().unwrap().clone();
        self.request(|reply| Command::ClearAssociation(context, request.clone(), reply))
            .await
    }
    async fn update_wmm_parameters(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        self.execution.admit()?;
        self.request(|reply| Command::Wmm(request.clone(), reply))
            .await
    }
    fn take_mlme_event_stream(&mut self) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
        // The owning runtime takes the origin-bearing route directly. An
        // untagged second receiver would erase operation authority.
        None
    }
    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
        if let fidl_mlme::MlmeEvent::OnScanEnd { end } = &event {
            let mut scan = self.execution.scan.borrow_mut();
            if scan
                .as_ref()
                .is_some_and(|scan| scan.transaction_id == end.txn_id)
            {
                scan.take().unwrap().context.revoke();
            }
        }
        if !self.execution.epoch.borrow().is_live() {
            return Ok(());
        }
        self.event_sink
            .try_send((self.execution.epoch.borrow().clone(), event))
            .map_err(|error| {
                self.overflow
                    .store(true, std::sync::atomic::Ordering::Release);
                error.into()
            })
    }
    fn set_minstrel(&mut self, minstrel: wlan_mlme::MinstrelWrapper) {
        self.io.lock().unwrap().minstrel = Some(minstrel);
    }
    fn minstrel(&mut self) -> Option<wlan_mlme::MinstrelWrapper> {
        self.io.lock().unwrap().minstrel.clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectError {
    Timeout,
    /// The exact terminal result reported by SME. Policy needs the credential
    /// classification to decide whether a fresh attempt is permitted.
    Failed(fidl_sme::ConnectResult),
    Driver(DriverError),
    Containment,
}

/// Terminal cleanup evidence; queue admission is not a successful disconnect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisconnectOutcome {
    Disconnected,
    ConnectCanceled(fidl_sme::ConnectResult),
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

enum ConnectAdmission {
    Pending(oneshot::Receiver<Option<ConnectTransaction>>),
    Active(ConnectTransaction),
    CanceledBeforeAdmission,
}

struct ConnectAttempt {
    admission: ConnectAdmission,
    result: Option<fidl_sme::ConnectResult>,
    deadline: Instant,
}

enum Connection {
    Active(ConnectTransaction),
    EndedNeedsCleanup,
}

enum ScanAdmission {
    Pending(oneshot::Receiver<ScanReceiver>),
    Active(ScanReceiver),
}

struct ScanAttempt {
    context: OperationContext,
    admission: ScanAdmission,
    deadline: Instant,
}

struct Cleanup {
    deadline: Instant,
    terminal: Option<fidl_sme::ConnectResult>,
    transaction_closed: bool,
    admitted: Option<oneshot::Receiver<()>>,
    link: Option<oneshot::Receiver<Result<(), zx::Status>>>,
    finish: Option<oneshot::Receiver<Result<(), zx::Status>>>,
    finished: bool,
    failed_connect: Option<ConnectError>,
}

/// Native control binding around the pinned Fuchsia serving topology.
/// Protocol and hardware tasks run independently; drive_* only observe retained
/// operation results. They do not schedule SME, MLME, timers or device work.
pub struct ClientRuntime<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> {
    hardware: HardwareOwner<D>,
    upcalls: Arc<Mutex<UpcallQueue>>,
    io: Arc<Mutex<HostIo>>,
    sme: Rc<RefCell<wlan_sme::client::ClientSme>>,
    requests: mpsc::Sender<SmeRequest>,
    #[cfg(test)]
    events: mpsc::Sender<crate::mlme::Event>,
    protocol: Option<tokio::task::JoinHandle<Result<(), zx::Status>>>,
    protocol_result: Option<Result<(), zx::Status>>,
    stop_callbacks: Option<oneshot::Sender<()>>,
    epoch: OperationEpoch,
    service_epoch: OperationEpoch,
    deadline: Arc<Mutex<Option<Instant>>>,
    pending: Arc<AtomicUsize>,
    overflow: Arc<AtomicBool>,
    connect_attempt: Option<ConnectAttempt>,
    scan_attempt: Option<ScanAttempt>,
    power_save: Option<(OperationContext, oneshot::Receiver<Result<(), zx::Status>>)>,
    cleanup: Option<Cleanup>,
    connection: Option<Connection>,
    connection_events: VecDeque<fidl_sme::ConnectTransactionEvent>,
    reset_requested: bool,
    revoked: bool,
}

pub struct PreparedRuntimeResources {
    ethernet_device: HostEthernetDevice,
    ethernet: DriverEthernetPort,
    replacement_ethernet: VecDeque<(HostEthernetDevice, DriverEthernetPort)>,
    mac_address: [u8; 6],
}

impl PreparedRuntimeResources {
    /// Prepare all descriptors and reactor registrations before sandbox lockdown.
    /// Requires an entered Tokio runtime with I/O enabled.
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
        for (_, driver) in &mut generations {
            driver.register_readiness()?;
        }
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
        let epoch = OperationEpoch::new();
        let upcalls = Arc::new(Mutex::new(UpcallQueue {
            epoch: epoch.clone(),
            live: true,
            overflowed: false,
            raw_queued: 0,
            notify: Arc::new(tokio::sync::Notify::new()),
            queue: VecDeque::new(),
        }));
        let io = Arc::new(Mutex::new(HostIo {
            ethernet,
            replacement_ethernet,
            unpublished_ethernet_device: Some(ethernet_device),
            pending_ethernet_devices: VecDeque::new(),
            ethernet_mac_address: mac_address,
            minstrel: None,
        }));
        let station_offload = device.station_offload_support();
        let (mut actor, driver) = DriverActor::new(device);
        let mut mlme_device = HostMlmeDevice::new(driver, io.clone(), station_offload);
        let mlme_events = mlme_device
            .event_stream
            .take()
            .ok_or_else(|| anyhow::anyhow!("MLME event stream already taken"))?;
        let execution = mlme_device.execution.clone();
        let initial_context = execution.operation.lock().unwrap().clone();
        // Constructor/inspection timers belong to the service, not to the first
        // connection attempt. Reconnecting must not silently kill maintenance.
        let service_epoch = initial_context.epoch().clone();
        let overflow = mlme_device.overflow.clone();
        let pending = Arc::new(AtomicUsize::new(0));
        let deadline = Arc::new(Mutex::new(None));
        let (requests, sme_requests) = mpsc::channel(64);
        let (mlme_requests, mlme_request_stream) = mpsc::channel(UPCALL_QUEUE_CAPACITY);
        let (events, driver_events) = mpsc::channel(UPCALL_QUEUE_CAPACITY);
        let (sme, sme_future) = crate::sme::client::serve(
            sme_config,
            device_info,
            security,
            spectrum,
            mlme_events,
            sme_requests,
            mlme_requests,
            inspector,
            initial_context,
            deadline.clone(),
            pending.clone(),
            overflow.clone(),
        );
        actor
            .start(Box::new(UpcallSender(upcalls.clone())))
            .map_err(|status| anyhow::anyhow!("SoftMAC start failed: {status}"))?;
        let (control, owner_commands) = mpsc::channel(4);
        let (hardware_exit, hardware_exited) = oneshot::channel();
        let task = tokio::task::spawn_local(async move {
            let (actor, result) = actor.serve(owner_commands).await;
            let _ = hardware_exit.send(result);
            (actor, result)
        });
        let hardware = HardwareOwner::Running { task, control };
        let (init, initialized) = oneshot::channel();
        let (ready, readiness) = oneshot::channel();
        let (stop_callbacks, callback_stop) = oneshot::channel();
        let mlme = crate::mlme::mlme_main_loop::<wlan_mlme::client::ClientMlme<HostMlmeDevice>>(
            init,
            wlan_mlme::client::ClientConfig {
                ensure_on_channel_time: 500_000_000,
            },
            mlme_device,
            mlme_request_stream,
            driver_events,
            execution,
            deadline.clone(),
            pending.clone(),
            overflow.clone(),
        );
        let callbacks = crate::serve::serve_wlan_softmac_ifc_bridge(
            upcalls.clone(),
            io.clone(),
            events.clone(),
            callback_stop,
            hardware_exited,
            deadline.clone(),
            pending.clone(),
            overflow.clone(),
        );
        let protocol = tokio::task::spawn_local(crate::serve::serve(
            initialized,
            ready,
            callbacks,
            Box::pin(mlme),
            Box::pin(sme_future),
        ));
        // This guard exists before readiness is awaited. Cancellation of the
        // constructor therefore revokes and tears down both owned task trees.
        let mut runtime = Self {
            hardware,
            upcalls,
            io,
            sme,
            requests,
            #[cfg(test)]
            events,
            protocol: Some(protocol),
            protocol_result: None,
            stop_callbacks: Some(stop_callbacks),
            epoch,
            service_epoch,
            deadline,
            pending,
            overflow,
            connect_attempt: None,
            scan_attempt: None,
            power_save: None,
            cleanup: None,
            connection: None,
            connection_events: VecDeque::new(),
            reset_requested: false,
            revoked: false,
        };
        if readiness.await.is_err() {
            let cleanup = runtime.shutdown().await;
            return Err(anyhow::anyhow!(
                "Fuchsia SoftMAC initialization failed; shutdown={cleanup:?}"
            ));
        }
        runtime
            .check_tasks()
            .map_err(|error| anyhow::anyhow!("SoftMAC startup: {error:?}"))?;
        Ok(runtime)
    }

    pub fn sme(&self) -> std::cell::Ref<'_, wlan_sme::client::ClientSme> {
        self.sme.borrow()
    }

    pub fn public_mac(&self) -> [u8; 6] {
        self.io.lock().unwrap().ethernet_mac_address
    }

    pub fn take_ethernet_device(&mut self) -> Option<HostEthernetDevice> {
        self.io.lock().unwrap().pending_ethernet_devices.pop_front()
    }

    /// Revoke/admit shutdown synchronously. This is not cleanup certification.
    pub fn request_stop(&mut self) {
        self.revoked = true;
        self.epoch.revoke();
        self.service_epoch.revoke();
        revoke_and_drain(&self.upcalls);
        self.io.lock().unwrap().ethernet.teardown();
        if let Some(stop) = self.stop_callbacks.take() {
            let _ = stop.send(());
        }
        self.hardware.request_stop(self.reset_requested);
    }

    /// Join by reference so cancellation retains ownership and terminal intent.
    /// Hardware cleanup is always checked, even when protocol shutdown fails.
    pub async fn shutdown(&mut self) -> Result<(), zx::Status> {
        let was_running = matches!(&self.hardware, HardwareOwner::Running { .. });
        self.request_stop();
        self.hardware.join().await;
        if let Some(task) = self.protocol.as_mut() {
            let result = match tokio::time::timeout(Duration::from_secs(3), &mut *task).await {
                Ok(result) => result.unwrap_or(Err(zx::Status::IO)),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    Err(zx::Status::TIMED_OUT)
                }
            };
            self.protocol_result = Some(result);
            self.protocol = None;
        }
        let hardware = if was_running && !self.reset_requested {
            self.hardware.observe().unwrap_or(Err(zx::Status::IO))
        } else {
            self.hardware.certify(self.reset_requested)
        };
        hardware.and(self.protocol_result.unwrap_or(Err(zx::Status::IO)))
    }

    fn check_tasks(&mut self) -> Result<(), ConnectError> {
        if self.revoked {
            return Err(ConnectError::Driver(DriverError::Stopped));
        }
        if self.overflow.load(Ordering::Acquire) || self.upcalls.lock().unwrap().overflowed {
            return Err(self.contain_error(ConnectError::Driver(DriverError::UpcallOverflow)));
        }
        if let Some(result) = self.hardware.observe() {
            let error = ConnectError::Driver(DriverError::ClientRx(
                result.err().unwrap_or(zx::Status::PEER_CLOSED),
            ));
            return Err(self.contain_error(error));
        }
        if let Some(result) = self.protocol.as_mut().and_then(|task| task.now_or_never()) {
            self.protocol_result = Some(result.unwrap_or(Err(zx::Status::IO)));
            self.protocol = None;
            return Err(self.contain_error(ConnectError::Driver(DriverError::MlmeTaskFailed)));
        }
        Ok(())
    }

    fn protocol_idle(&self) -> bool {
        self.pending.load(Ordering::Acquire) == 0
    }

    fn begin_epoch(&mut self) {
        self.epoch.revoke();
        self.epoch = OperationEpoch::new();
        let mut upcalls = self.upcalls.lock().unwrap();
        upcalls.queue.clear();
        upcalls.raw_queued = 0;
        upcalls.epoch = self.epoch.clone();
    }

    fn contain_error(&mut self, error: ConnectError) -> ConnectError {
        self.reset_requested = true;
        self.request_stop();
        error
    }

    pub async fn begin_connect(
        &mut self,
        request: fidl_sme::ConnectRequest,
        deadline: Instant,
    ) -> Result<(), ConnectError> {
        self.check_tasks()?;
        self.drain_connection_events()?;
        if matches!(self.connection, Some(Connection::EndedNeedsCleanup)) {
            tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.disconnect(fidl_sme::UserDisconnectReason::FailedToConnect, deadline),
            )
            .await
            .map_err(|_| ConnectError::Timeout)??;
        }
        if Instant::now() >= deadline {
            return Err(ConnectError::Timeout);
        }
        if self.connection.is_some() {
            return Err(ConnectError::Driver(DriverError::AlreadyConnected));
        }
        if self.connect_attempt.is_some() || self.cleanup.is_some() {
            return Err(ConnectError::Driver(DriverError::ConnectInProgress));
        }
        if self.scan_attempt.is_some() {
            return Err(ConnectError::Driver(DriverError::ScanInProgress));
        }
        self.begin_epoch();
        *self.deadline.lock().unwrap() = Some(deadline);
        let (reply, admission) = oneshot::channel();
        self.requests
            .try_send(SmeRequest::Connect {
                context: self.epoch.context(deadline),
                request,
                reply,
            })
            .map_err(|_| ConnectError::Driver(DriverError::ControlBudgetExhausted))?;
        self.connect_attempt = Some(ConnectAttempt {
            admission: ConnectAdmission::Pending(admission),
            result: None,
            deadline,
        });
        Ok(())
    }

    fn connect_admitted(&mut self) -> Result<bool, ConnectError> {
        let attempt = self
            .connect_attempt
            .as_mut()
            .ok_or(ConnectError::Driver(DriverError::NoConnectInProgress))?;
        if let ConnectAdmission::Pending(receiver) = &mut attempt.admission {
            match receiver.try_recv() {
                Ok(Some(Some(transaction))) => {
                    attempt.admission = ConnectAdmission::Active(transaction)
                }
                Ok(Some(None)) => attempt.admission = ConnectAdmission::CanceledBeforeAdmission,
                Ok(None) => return Ok(false),
                Err(_) => return Err(ConnectError::Driver(DriverError::ConnectTransactionClosed)),
            }
        }
        Ok(true)
    }

    async fn drive_connect_once_inner(
        &mut self,
    ) -> Result<Option<fidl_sme::ConnectResult>, ConnectError> {
        tokio::task::yield_now().await;
        self.check_tasks()?;
        if let Some(failed) = self
            .cleanup
            .as_ref()
            .and_then(|cleanup| cleanup.failed_connect.clone())
        {
            return match self.drive_disconnect_once_inner().await? {
                Some(_) => Err(failed),
                None => Ok(None),
            };
        }
        let deadline = self
            .connect_attempt
            .as_ref()
            .ok_or(ConnectError::Driver(DriverError::NoConnectInProgress))?
            .deadline;
        if Instant::now() >= deadline {
            return Err(ConnectError::Timeout);
        }
        if !self.connect_admitted()? {
            return Ok(None);
        }
        let attempt = self.connect_attempt.as_mut().unwrap();
        if matches!(attempt.admission, ConnectAdmission::CanceledBeforeAdmission) {
            return Err(ConnectError::Driver(DriverError::ConnectTransactionClosed));
        }
        if attempt.result.is_none() {
            let ConnectAdmission::Active(transaction) = &mut attempt.admission else {
                unreachable!()
            };
            loop {
                match transaction.try_recv() {
                    Ok(fidl_sme::ConnectTransactionEvent::OnConnectResult { result }) => {
                        attempt.result = Some(result);
                        break;
                    }
                    Ok(_) => {}
                    Err(mpsc::TryRecvError::Closed) => {
                        return Err(ConnectError::Driver(DriverError::ConnectTransactionClosed));
                    }
                    Err(mpsc::TryRecvError::Empty) => return Ok(None),
                }
            }
        }
        let result = self.connect_attempt.as_ref().unwrap().result.unwrap();
        if result.code != fidl_ieee80211::StatusCode::Success {
            self.begin_cleanup(fidl_sme::UserDisconnectReason::FailedToConnect, deadline)?;
            self.cleanup.as_mut().unwrap().failed_connect = Some(ConnectError::Failed(result));
            return Ok(None);
        }
        if !self.protocol_idle() {
            return Ok(None);
        }
        if !self.sme.borrow().status().is_connected()
            || !self.io.lock().unwrap().ethernet.is_link_up()
        {
            return Err(ConnectError::Driver(DriverError::ConnectStateMismatch));
        }
        let attempt = self.connect_attempt.take().unwrap();
        let ConnectAdmission::Active(transaction) = attempt.admission else {
            unreachable!()
        };
        self.connection = Some(Connection::Active(transaction));
        *self.deadline.lock().unwrap() = None;
        Ok(attempt.result)
    }

    pub async fn drive_connect_once(
        &mut self,
    ) -> Result<Option<fidl_sme::ConnectResult>, ConnectError> {
        match self.drive_connect_once_inner().await {
            Err(error @ ConnectError::Failed(_)) => Err(error),
            Err(error) if !self.revoked => Err(self.contain_error(error)),
            result => result,
        }
    }

    pub async fn connect(
        &mut self,
        request: fidl_sme::ConnectRequest,
        deadline: Instant,
    ) -> Result<fidl_sme::ConnectResult, ConnectError> {
        self.begin_connect(request, deadline).await?;
        loop {
            if let Some(result) = self.drive_connect_once().await? {
                return Ok(result);
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    pub fn roam(&mut self, _request: fidl_sme::RoamRequest) -> Result<(), ConnectError> {
        self.check_tasks()?;
        if self.connect_attempt.is_some() {
            return Err(ConnectError::Driver(DriverError::ConnectInProgress));
        }
        if self.connection.is_none() || !self.sme.borrow().status().is_connected() {
            return Err(ConnectError::Driver(DriverError::NotConnected));
        }
        Err(ConnectError::Driver(DriverError::RoamUnsupported))
    }

    pub async fn begin_scan(
        &mut self,
        request: fidl_sme::ScanRequest,
        deadline: Instant,
    ) -> Result<(), ConnectError> {
        self.check_tasks()?;
        self.drain_connection_events()?;
        if matches!(self.connection, Some(Connection::EndedNeedsCleanup)) {
            tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.disconnect(fidl_sme::UserDisconnectReason::FailedToConnect, deadline),
            )
            .await
            .map_err(|_| ConnectError::Timeout)??;
        }
        if Instant::now() >= deadline {
            return Err(ConnectError::Timeout);
        }
        if self.scan_attempt.is_some() {
            return Err(ConnectError::Driver(DriverError::ScanInProgress));
        }
        if self.connect_attempt.is_some() || self.cleanup.is_some() {
            return Err(ConnectError::Driver(DriverError::ConnectInProgress));
        }
        if self.connection.is_none() {
            self.begin_epoch();
        }
        *self.deadline.lock().unwrap() = Some(deadline);
        let context = OperationContext::child(self.epoch.clone(), deadline);
        let (reply, admission) = oneshot::channel();
        self.requests
            .try_send(SmeRequest::Scan {
                context: self.epoch.context(deadline),
                scan: context.clone(),
                request,
                reply,
            })
            .map_err(|_| ConnectError::Driver(DriverError::ControlBudgetExhausted))?;
        self.scan_attempt = Some(ScanAttempt {
            context,
            admission: ScanAdmission::Pending(admission),
            deadline,
        });
        Ok(())
    }

    pub async fn drive_scan_once(
        &mut self,
    ) -> Result<Option<Result<Vec<fidl_sme::ScanResult>, fidl_sme::ScanErrorCode>>, ConnectError>
    {
        let result = async {
            tokio::task::yield_now().await;
            self.check_tasks()?;
            let scan = self
                .scan_attempt
                .as_mut()
                .ok_or(ConnectError::Driver(DriverError::NoScanInProgress))?;
            if Instant::now() >= scan.deadline {
                return Err(ConnectError::Timeout);
            }
            if let ScanAdmission::Pending(receiver) = &mut scan.admission {
                match receiver.try_recv() {
                    Ok(Some(receiver)) => scan.admission = ScanAdmission::Active(receiver),
                    Ok(None) => return Ok(None),
                    Err(_) => return Err(ConnectError::Driver(DriverError::ScanTransactionClosed)),
                }
            }
            let ScanAdmission::Active(receiver) = &mut scan.admission else {
                unreachable!()
            };
            match receiver.try_recv() {
                Ok(Some(result)) => {
                    scan.context.revoke();
                    self.scan_attempt = None;
                    *self.deadline.lock().unwrap() = None;
                    Ok(Some(wlan_sme::client::convert_scan_result(result)))
                }
                Ok(None) => Ok(None),
                Err(_) => Err(ConnectError::Driver(DriverError::ScanTransactionClosed)),
            }
        }
        .await;
        match result {
            Err(error) if !self.revoked => Err(self.contain_error(error)),
            result => result,
        }
    }

    fn begin_cleanup(
        &mut self,
        reason: fidl_sme::UserDisconnectReason,
        deadline: Instant,
    ) -> Result<(), ConnectError> {
        if self.cleanup.is_some() {
            return Ok(());
        }
        self.epoch.revoke();
        self.epoch = OperationEpoch::new();
        *self.deadline.lock().unwrap() = Some(deadline);
        self.io.lock().unwrap().ethernet.set_link(false);
        let (link_reply, link) = oneshot::channel();
        self.hardware
            .send(OwnerCommand::Link(false, link_reply))
            .map_err(|status| {
                self.contain_error(ConnectError::Driver(DriverError::Ethernet(status)))
            })?;
        let (reply, admitted) = oneshot::channel();
        self.requests
            .try_send(SmeRequest::Disconnect {
                context: self.epoch.context(deadline),
                reason,
                reply,
            })
            .map_err(|_| {
                self.contain_error(ConnectError::Driver(DriverError::ControlBudgetExhausted))
            })?;
        let terminal = self
            .connect_attempt
            .as_mut()
            .and_then(|attempt| attempt.result.take());
        self.cleanup = Some(Cleanup {
            deadline,
            terminal,
            transaction_closed: false,
            admitted: Some(admitted),
            link: Some(link),
            finish: None,
            finished: false,
            failed_connect: None,
        });
        Ok(())
    }

    pub fn begin_disconnect(
        &mut self,
        reason: fidl_sme::UserDisconnectReason,
        deadline: Instant,
    ) -> Result<(), ConnectError> {
        self.check_tasks()?;
        if self.connect_attempt.is_none()
            && self.connection.is_none()
            && self.cleanup.is_none()
            && sme_is_retry_quiescent(&self.sme.borrow().status())
            && self.protocol_idle()
        {
            return Ok(());
        }
        self.begin_cleanup(reason, deadline)
    }

    async fn drive_disconnect_once_inner(
        &mut self,
    ) -> Result<Option<DisconnectOutcome>, ConnectError> {
        tokio::task::yield_now().await;
        self.check_tasks()?;
        let Some(cleanup) = self.cleanup.as_mut() else {
            return Ok(Some(DisconnectOutcome::Disconnected));
        };
        if Instant::now() >= cleanup.deadline {
            return Err(ConnectError::Timeout);
        }
        if let Some(admitted) = cleanup.admitted.as_mut() {
            match admitted.try_recv() {
                Ok(Some(())) => cleanup.admitted = None,
                Ok(None) => return Ok(None),
                Err(_) => return Err(ConnectError::Driver(DriverError::RequestStreamClosed)),
            }
        }
        if let Some(link) = cleanup.link.as_mut() {
            match link.try_recv() {
                Ok(Some(Ok(()))) => cleanup.link = None,
                Ok(None) => return Ok(None),
                _ => return Err(ConnectError::Driver(DriverError::RetryCleanup)),
            }
        }
        if self.connect_attempt.is_some() {
            if !self.connect_admitted()? {
                return Ok(None);
            }
            if let ConnectAdmission::Active(transaction) =
                &mut self.connect_attempt.as_mut().unwrap().admission
            {
                loop {
                    match transaction.try_recv() {
                        Ok(fidl_sme::ConnectTransactionEvent::OnConnectResult { result }) => {
                            self.cleanup.as_mut().unwrap().terminal = Some(result);
                        }
                        Ok(_) => {}
                        Err(mpsc::TryRecvError::Closed) => {
                            self.cleanup.as_mut().unwrap().transaction_closed = true;
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                    }
                }
            } else {
                self.cleanup.as_mut().unwrap().transaction_closed = true;
            }
            let cleanup = self.cleanup.as_ref().unwrap();
            if cleanup.terminal.is_none() && !cleanup.transaction_closed {
                return Ok(None);
            }
        }
        if !sme_is_retry_quiescent(&self.sme.borrow().status()) || !self.protocol_idle() {
            return Ok(None);
        }
        let cleanup = self.cleanup.as_mut().unwrap();
        if !cleanup.finished {
            if cleanup.finish.is_none() {
                let (reply, receiver) = oneshot::channel();
                self.hardware
                    .send(OwnerCommand::FinishAttempt(reply))
                    .map_err(|_| ConnectError::Driver(DriverError::RetryCleanup))?;
                cleanup.finish = Some(receiver);
            }
            match cleanup.finish.as_mut().unwrap().try_recv() {
                Ok(Some(Ok(()))) => {
                    cleanup.finished = true;
                    cleanup.finish = None;
                }
                Ok(Some(Err(zx::Status::SHOULD_WAIT))) => {
                    cleanup.finish = None;
                    return Ok(None);
                }
                Ok(None) => return Ok(None),
                _ => return Err(ConnectError::Driver(DriverError::RetryCleanup)),
            }
        }
        if !self.protocol_idle() {
            return Ok(None);
        }
        if !drain_completed_attempt(&self.upcalls) {
            return Err(ConnectError::Driver(DriverError::RetryCleanup));
        }
        let cleanup = self.cleanup.take().unwrap();
        let outcome = if self.connect_attempt.take().is_some() {
            DisconnectOutcome::ConnectCanceled(cleanup.terminal.unwrap_or(
                fidl_sme::ConnectResult {
                    code: fidl_ieee80211::StatusCode::Canceled,
                    is_credential_rejected: false,
                    is_reconnect: false,
                },
            ))
        } else {
            DisconnectOutcome::Disconnected
        };
        self.scan_attempt = None;
        self.connection = None;
        self.epoch.revoke();
        *self.deadline.lock().unwrap() = None;
        Ok(Some(outcome))
    }

    pub async fn drive_disconnect_once(
        &mut self,
    ) -> Result<Option<DisconnectOutcome>, ConnectError> {
        match self.drive_disconnect_once_inner().await {
            Err(error) if !self.revoked => Err(self.contain_error(error)),
            result => result,
        }
    }

    pub async fn disconnect(
        &mut self,
        reason: fidl_sme::UserDisconnectReason,
        deadline: Instant,
    ) -> Result<(), ConnectError> {
        if self.connect_attempt.is_some() {
            return Err(ConnectError::Driver(DriverError::ConnectInProgress));
        }
        self.begin_disconnect(reason, deadline)?;
        loop {
            if Instant::now() >= deadline {
                return Err(ConnectError::Timeout);
            }
            if self.drive_disconnect_once().await?.is_some() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    pub async fn cancel_connect(
        &mut self,
        reason: fidl_sme::UserDisconnectReason,
        deadline: Instant,
    ) -> Result<fidl_sme::ConnectResult, ConnectError> {
        if self.connect_attempt.is_none() {
            return Err(ConnectError::Driver(DriverError::NoConnectInProgress));
        }
        self.begin_disconnect(reason, deadline)?;
        loop {
            if Instant::now() >= deadline {
                return Err(ConnectError::Timeout);
            }
            if let Some(outcome) = self.drive_disconnect_once().await? {
                let DisconnectOutcome::ConnectCanceled(result) = outcome else {
                    unreachable!("connect attempt retained until cleanup");
                };
                return Ok(result);
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    pub fn begin_power_save(&mut self, enabled: bool, deadline: Instant) -> Result<(), zx::Status> {
        self.check_tasks().map_err(|_| zx::Status::IO)?;
        if Instant::now() >= deadline {
            return Err(zx::Status::TIMED_OUT);
        }
        if self.power_save.is_some() {
            return Err(zx::Status::SHOULD_WAIT);
        }
        if !matches!(self.connection, Some(Connection::Active(_))) || self.cleanup.is_some() {
            return Err(zx::Status::BAD_STATE);
        }
        let context = OperationContext::child(self.epoch.clone(), deadline);
        let (reply, receiver) = oneshot::channel();
        self.hardware
            .send(OwnerCommand::PowerSave(context.clone(), enabled, reply))?;
        self.power_save = Some((context, receiver));
        Ok(())
    }

    pub async fn drive_power_save_once(&mut self) -> Result<Option<()>, zx::Status> {
        self.check_tasks().map_err(|_| zx::Status::IO)?;
        let Some((context, receiver)) = self.power_save.as_mut() else {
            return Ok(None);
        };
        match receiver.try_recv() {
            Ok(Some(Ok(()))) => {
                // Firmware may have acknowledged before disconnect revoked the
                // association, with the reply still buffered in this channel.
                let authority = context.check(Instant::now());
                self.power_save = None;
                authority?;
                if self.cleanup.is_some() || !matches!(self.connection, Some(Connection::Active(_)))
                {
                    return Err(zx::Status::BAD_STATE);
                }
                Ok(Some(()))
            }
            Ok(Some(Err(status))) => {
                self.power_save = None;
                Err(status)
            }
            Ok(None) => Ok(None),
            Err(_) => {
                self.power_save = None;
                Err(zx::Status::IO)
            }
        }
    }

    pub async fn drive_service_once(&mut self) -> Result<bool, ConnectError> {
        tokio::task::yield_now().await;
        self.check_tasks()?;
        if self.connect_attempt.is_some() || self.scan_attempt.is_some() {
            return Ok(false);
        }
        self.drain_connection_events()?;
        if self.cleanup.is_some() {
            let _ = self.drive_disconnect_once().await?;
            return Ok(true);
        }
        Ok(false)
    }

    pub async fn pump_associated_once(&mut self) -> Result<bool, ConnectError> {
        self.drive_service_once().await
    }

    fn drain_connection_events(&mut self) -> Result<(), ConnectError> {
        if self.cleanup.is_some() {
            return Ok(());
        }
        loop {
            if self.connection_events.len() >= 64 {
                return Err(self.contain_error(ConnectError::Driver(DriverError::UpcallOverflow)));
            }
            let Some(Connection::Active(transaction)) = self.connection.as_mut() else {
                return Ok(());
            };
            match transaction.try_recv() {
                Ok(event) => {
                    if matches!(&event, fidl_sme::ConnectTransactionEvent::OnDisconnect { info }
                        if !info.is_sme_reconnecting)
                    {
                        self.connection = Some(Connection::EndedNeedsCleanup);
                        self.begin_cleanup(
                            fidl_sme::UserDisconnectReason::FailedToConnect,
                            Instant::now() + Duration::from_secs(3),
                        )?;
                        self.connection_events.push_back(event);
                        return Ok(());
                    }
                    self.connection_events.push_back(event);
                }
                Err(mpsc::TryRecvError::Closed) => {
                    return Err(self.contain_error(ConnectError::Driver(
                        DriverError::ConnectTransactionClosed,
                    )));
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
            }
        }
    }

    pub fn next_connection_event(
        &mut self,
    ) -> Result<Option<fidl_sme::ConnectTransactionEvent>, ConnectError> {
        self.check_tasks()?;
        self.drain_connection_events()?;
        Ok(self.connection_events.pop_front())
    }
}

impl<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> Drop for ClientRuntime<D> {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(task) = self.protocol.as_ref() {
            task.abort();
        }
        // HardwareOwner::Drop aborts its task; DriverActor::Drop attempts stop.
        // This is a safety backstop, not a synchronous cleanup certificate.
    }
}

#[cfg(test)]
mod tests {
    fn run_local_test(future: impl std::future::Future<Output = ()>) {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&executor, future);
    }

    use super::*;
    use futures::StreamExt;

    #[derive(Default)]
    struct Effects {
        calls: Vec<&'static str>,
        upcalls: Option<Box<dyn WlanSoftmacUpcalls>>,
        stop_failures: usize,
        reset_failure: bool,
        query_failure: bool,
        tx_flags: Vec<fidl_softmac::WlanTxInfoFlags>,
        tx_contexts: Vec<OperationContext>,
        tx_admission_error: Option<zx::Status>,
        channel_contexts: Vec<OperationContext>,
        join_contexts: Vec<OperationContext>,
        association_contexts: Vec<OperationContext>,
        channels: Vec<fidl_softmac::WlanSoftmacBaseSetChannelRequest>,
        simulate_ap: bool,
        ps_polls: usize,
        suppress_auth_response: bool,
        reject_next_auth: bool,
        pending_rx: VecDeque<Vec<u8>>,
        retry_cleanup: bool,
        stale_callback_during_cleanup: bool,
        link_failure: bool,
        scan_id: u64,
        scan_offload: bool,
        station_offload: StationOffloadSupport,
        scan_contexts: Vec<OperationContext>,
        extra_band: Option<fidl_softmac::WlanSoftmacBandCapability>,
        empty_bands: bool,
        channel_completion: Option<oneshot::Receiver<Result<(), zx::Status>>>,
        complete_channel_on_drive: Option<oneshot::Sender<Result<(), zx::Status>>>,
        clear_completion: Option<oneshot::Receiver<Result<(), zx::Status>>>,
        event_driven: bool,
        drive_count: usize,
        wake: Option<std::task::Waker>,
        observation_deadline: Option<Instant>,
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
        fn station_offload_support(&self) -> StationOffloadSupport {
            self.0.lock().unwrap().station_offload
        }
        fn poll_drive(&mut self, cx: &mut std::task::Context<'_>) -> Result<bool, zx::Status> {
            self.0.lock().unwrap().wake = Some(cx.waker().clone());
            self.drive()
        }
        fn next_deadline(&self) -> Option<Instant> {
            let effects = self.0.lock().unwrap();
            if effects.event_driven {
                effects.observation_deadline
            } else {
                Some(Instant::now() + Duration::from_millis(1))
            }
        }
        fn drive(&mut self) -> Result<bool, zx::Status> {
            let mut effects = self.0.lock().unwrap();
            effects.drive_count += 1;
            if effects
                .observation_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                effects.observation_deadline = None;
            }
            if effects.calls.contains(&"channel")
                && let Some(reply) = effects.complete_channel_on_drive.take()
            {
                let _ = reply.send(Ok(()));
                return Ok(true);
            }

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
            let extra_band = self.0.lock().unwrap().extra_band.clone();
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
                        let mut bands = vec![fidl_softmac::WlanSoftmacBandCapability {
                            band: Some(fidl_ieee80211::WlanBand::TwoGhz),
                            basic_rates: Some(vec![0x82, 0x84]),
                            primary_channels: Some(vec![wlan_channel()]),
                            ..Default::default()
                        }];
                        bands.extend(extra_band);
                        bands
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
            context: crate::OperationContext,
            request: fidl_softmac::WlanSoftmacBaseSetChannelRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            let completion = {
                let mut effects = self.0.lock().unwrap();
                effects.channel_contexts.push(context);
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
            context: crate::OperationContext,
            _: fidl_driver::JoinBssRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            self.0.lock().unwrap().join_contexts.push(context);
            std::future::ready(record!(self, "join", ()))
        }
        fn install_key(
            &mut self,
            context: crate::OperationContext,
            _: fidl_softmac::WlanKeyConfiguration,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            self.0.lock().unwrap().association_contexts.push(context);
            std::future::ready(record!(self, "key", ()))
        }
        fn notify_association_complete(
            &mut self,
            context: crate::OperationContext,
            _: fidl_softmac::WlanAssociationConfig,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            self.0.lock().unwrap().association_contexts.push(context);
            std::future::ready(record!(self, "assoc", ()))
        }
        fn clear_association(
            &mut self,
            context: crate::OperationContext,
            _: fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            let completion = {
                let mut effects = self.0.lock().unwrap();
                effects.calls.push("clear");
                effects.association_contexts.push(context);
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
            context: crate::OperationContext,
            _: fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
        ) -> impl std::future::Future<
            Output = Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status>,
        > + 'static {
            std::future::ready({
                let mut effects = self.0.lock().unwrap();
                effects.calls.push("passive");
                effects.scan_contexts.push(context);
                effects.scan_id = effects.scan_id.checked_add(1).unwrap();
                Ok(fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse {
                    scan_id: Some(effects.scan_id),
                })
            })
        }
        fn start_active_scan(
            &mut self,
            context: crate::OperationContext,
            _: fidl_softmac::WlanSoftmacStartActiveScanRequest,
        ) -> impl std::future::Future<
            Output = Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status>,
        > + 'static {
            std::future::ready({
                let mut effects = self.0.lock().unwrap();
                effects.calls.push("active");
                effects.scan_contexts.push(context);
                effects.scan_id += 1;
                Ok(fidl_softmac::WlanSoftmacBaseStartActiveScanResponse {
                    scan_id: Some(effects.scan_id),
                })
            })
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
            context: crate::OperationContext,
            bytes: &[u8],
            flags: fidl_softmac::WlanTxInfoFlags,
        ) -> Result<(), zx::Status> {
            context.check(std::time::Instant::now())?;
            let mut effects = self.0.lock().unwrap();
            if let Some(error) = effects.tx_admission_error {
                return Err(error);
            }
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
                    Some(0xa4) => effects.ps_polls += 1,
                    _ => return Err(zx::Status::NOT_SUPPORTED),
                }
            } else {
                assert_eq!(bytes, [1, 0x40, 3]);
            }
            effects.tx_contexts.push(context);
            effects.tx_flags.push(flags);
            effects.calls.push("tx");
            Ok(())
        }
    }

    fn parts(fake: Fake) -> (HostMlmeDevice, DriverActor<Fake>, Arc<Mutex<Effects>>) {
        let effects = fake.0.clone();
        let (actor, driver) = DriverActor::new(fake);
        let (_, ethernet) = ethernet_port([2, 0, 0, 0, 0, 1], 4).unwrap();
        let io = Arc::new(Mutex::new(HostIo {
            ethernet,
            replacement_ethernet: VecDeque::new(),
            unpublished_ethernet_device: None,
            pending_ethernet_devices: VecDeque::new(),
            ethernet_mac_address: [2, 0, 0, 0, 0, 1],
            minstrel: None,
        }));
        (HostMlmeDevice::new(driver, io, StationOffloadSupport::default()), actor, effects)
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
    fn actor_retains_admitted_completion_after_waiter_drop_and_revocation() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().retry_cleanup = true;
            let (hardware_reply, completion) = oneshot::channel();
            effects.lock().unwrap().channel_completion = Some(completion);
            let (mut actor, handle) = DriverActor::new(fake);
            let epoch = OperationEpoch::new();
            let (reply, receiver) = oneshot::channel();
            handle
                .send(
                    epoch.clone(),
                    Command::Channel(
                        OperationContext::child(
                            epoch.clone(),
                            std::time::Instant::now() + std::time::Duration::from_secs(1),
                        ),
                        Default::default(),
                        reply,
                    ),
                )
                .unwrap();
            actor.drive_once().await.unwrap();
            drop(receiver);
            epoch.revoke();
            assert_eq!(
                actor.finish_failed_connect_attempt(),
                Err(zx::Status::SHOULD_WAIT)
            );
            assert_eq!(effects.lock().unwrap().calls, ["channel"]);
            hardware_reply
                .send(Ok(()))
                .expect("actor retained the hardware completion");
            actor.drive_once().await.unwrap();
            actor.finish_failed_connect_attempt().unwrap();
            assert_eq!(
                effects.lock().unwrap().calls,
                ["channel", "finish_failed_connect_attempt"]
            );
        });
    }

    #[test]
    fn full_actor_mailbox_discards_revoked_unpublished_work_before_cleanup_admission() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (mut actor, handle) = DriverActor::new(fake);
            let epoch = OperationEpoch::new();
            for _ in 0..256 {
                let (reply, _receiver) = oneshot::channel();
                handle
                    .send(epoch.clone(), Command::Link(true, reply))
                    .unwrap();
            }
            let (reply, _receiver) = oneshot::channel();
            assert_eq!(
                handle.send(epoch.clone(), Command::Link(true, reply)),
                Err(zx::Status::NO_RESOURCES)
            );
            assert!(effects.lock().unwrap().calls.is_empty());
            epoch.revoke();
            let (reply, receiver) = oneshot::channel();
            let cleanup = OperationEpoch::new();
            handle
                .send(
                    cleanup.clone(),
                    Command::ClearAssociation(
                        cleanup
                            .context(std::time::Instant::now() + std::time::Duration::from_secs(1)),
                        Default::default(),
                        reply,
                    ),
                )
                .unwrap();
            actor.drive_once().await.unwrap();
            assert_eq!(receiver.await.unwrap(), Ok(()));
            assert_eq!(effects.lock().unwrap().calls, ["clear"]);
        });
    }

    #[test]
    fn scan_queue_checks_child_revocation_and_never_replaces_an_expired_budget() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (mut actor, handle) = DriverActor::new(fake);
            let parent = OperationEpoch::new();
            let now = std::time::Instant::now();
            let expired = OperationContext::child(parent.clone(), now);
            let (reply, _) = oneshot::channel();
            assert_eq!(
                handle.send(
                    parent.clone(),
                    Command::PassiveScan(expired, Default::default(), reply)
                ),
                Err(zx::Status::TIMED_OUT)
            );
            let context =
                OperationContext::child(parent.clone(), now + std::time::Duration::from_secs(1));
            for _ in 0..256 {
                let (reply, _) = oneshot::channel();
                handle
                    .send(
                        parent.clone(),
                        Command::PassiveScan(context.clone(), Default::default(), reply),
                    )
                    .unwrap();
            }
            context.revoke();
            assert!(parent.is_live());
            let (reply, receiver) = oneshot::channel();
            handle
                .send(
                    parent.clone(),
                    Command::ClearAssociation(
                        parent.context(now + std::time::Duration::from_secs(1)),
                        Default::default(),
                        reply,
                    ),
                )
                .unwrap();
            actor.drive_once().await.unwrap();
            assert_eq!(receiver.await.unwrap(), Ok(()));
            assert_eq!(effects.lock().unwrap().calls, ["clear"]);
        });
    }

    #[test]
    fn revoked_link_completion_cannot_publish_an_ethernet_attachment() {
        run_local_test(async {
            let (fake, _) = Fake::new(0);
            let (mut bridge, mut actor, _) = parts(fake);
            let epoch = bridge.execution.epoch.borrow().clone();
            let io = bridge.io.clone();
            {
                let mut operation = std::pin::pin!(bridge.set_ethernet_status(LinkStatus::UP));
                assert!(operation.as_mut().now_or_never().is_none());
                actor.drive_once().await.unwrap();
                epoch.revoke();
                actor.set_link_up(false).unwrap();
                assert_eq!(operation.await, Err(zx::Status::CANCELED));
            }
            let io = io.lock().unwrap();
            assert!(!io.ethernet.is_link_up());
            assert!(io.pending_ethernet_devices.is_empty());
        });
    }

    #[test]
    fn transmit_backpressure_retains_work_without_reviving_revoked_authority() {
        run_local_test(async {
            for revoked in [false, true] {
                let (fake, effects) = Fake::new(0);
                let (mut actor, handle) = DriverActor::new(fake);
                let epoch = OperationEpoch::new();
                let context =
                    OperationContext::child(epoch.clone(), Instant::now() + Duration::from_secs(1));
                effects.lock().unwrap().tx_admission_error = Some(zx::Status::NO_RESOURCES);
                handle
                    .send(
                        epoch.clone(),
                        Command::Transmit(
                            context.clone(),
                            vec![1, 0x40, 3],
                            fidl_softmac::WlanTxInfoFlags::empty(),
                        ),
                    )
                    .unwrap();
                assert!(!actor.drive_once().await.unwrap());
                assert!(!actor.drive_once().await.unwrap());
                assert!(effects.lock().unwrap().tx_contexts.is_empty());
                if revoked {
                    context.revoke();
                }
                effects.lock().unwrap().tx_admission_error = None;
                assert!(actor.drive_once().await.unwrap());
                assert_eq!(
                    effects.lock().unwrap().tx_contexts.len(),
                    usize::from(!revoked)
                );
                assert!(!actor.drive_once().await.unwrap());

                // Only admission exhaustion is retryable; hardware faults
                // must still reach the owner's containment path.
                effects.lock().unwrap().tx_admission_error = Some(zx::Status::IO);
                handle
                    .send(
                        epoch.clone(),
                        Command::Transmit(
                            epoch.context(Instant::now() + Duration::from_secs(1)),
                            vec![1, 0x40, 3],
                            fidl_softmac::WlanTxInfoFlags::empty(),
                        ),
                    )
                    .unwrap();
                assert_eq!(actor.drive_once().await, Err(zx::Status::IO));
            }
        });
    }

    #[test]
    fn transmit_preserves_authority_and_rejects_revoked_or_expired_work() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (mut bridge, mut actor, _) = parts(fake);
            let epoch = bridge.execution.epoch.borrow().clone();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            let context = OperationContext::child(epoch.clone(), deadline);
            *bridge.execution.operation.lock().unwrap() = context.clone();
            bridge
                .send_wlan_frame(
                    vec![1, 0x40, 3].into(),
                    fidl_softmac::WlanTxInfoFlags::empty(),
                    None,
                )
                .unwrap();
            actor.drive_once().await.unwrap();
            assert_eq!(effects.lock().unwrap().tx_contexts[0].deadline(), deadline);
            bridge
                .send_wlan_frame(
                    vec![1, 0x40, 3].into(),
                    fidl_softmac::WlanTxInfoFlags::empty(),
                    None,
                )
                .unwrap();
            context.revoke();
            actor.drive_once().await.unwrap();
            assert_eq!(effects.lock().unwrap().tx_flags.len(), 1);
            assert_eq!(
                effects.lock().unwrap().tx_contexts[0].check(std::time::Instant::now()),
                Err(zx::Status::CANCELED)
            );
            *bridge.execution.operation.lock().unwrap() =
                OperationContext::child(epoch, std::time::Instant::now());
            assert_eq!(
                bridge.send_wlan_frame(
                    vec![1, 0x40, 3].into(),
                    fidl_softmac::WlanTxInfoFlags::empty(),
                    None
                ),
                Err(zx::Status::TIMED_OUT)
            );
            assert_eq!(effects.lock().unwrap().tx_flags.len(), 1);
        });
    }

    #[test]
    fn channel_context_rejects_expiry_and_revocation_before_hardware_dispatch() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (mut actor, handle) = DriverActor::new(fake);
            let epoch = OperationEpoch::new();
            let (reply, _) = oneshot::channel();
            assert_eq!(
                handle.send(
                    epoch.clone(),
                    Command::Channel(
                        OperationContext::child(epoch.clone(), std::time::Instant::now()),
                        Default::default(),
                        reply
                    )
                ),
                Err(zx::Status::TIMED_OUT),
            );
            let context = OperationContext::child(
                epoch.clone(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            );
            let (reply, receiver) = oneshot::channel();
            handle
                .send(
                    epoch,
                    Command::Channel(context.clone(), Default::default(), reply),
                )
                .unwrap();
            context.revoke();
            actor.drive_once().await.unwrap();
            assert_eq!(receiver.await.unwrap(), Err(zx::Status::CANCELED));
            assert!(effects.lock().unwrap().channels.is_empty());
        });
    }

    #[test]
    fn association_mutations_reject_expiry_and_revocation_before_dispatch() {
        run_local_test(async {
            for kind in 0..3 {
                let (fake, effects) = Fake::new(0);
                let (mut actor, handle) = DriverActor::new(fake);
                let epoch = OperationEpoch::new();
                let command = |context, reply| match kind {
                    0 => Command::Key(context, Default::default(), reply),
                    1 => Command::Association(context, Default::default(), reply),
                    _ => Command::ClearAssociation(context, Default::default(), reply),
                };
                let (reply, _) = oneshot::channel();
                assert_eq!(
                    handle.send(
                        epoch.clone(),
                        command(
                            OperationContext::child(epoch.clone(), std::time::Instant::now()),
                            reply
                        )
                    ),
                    Err(zx::Status::TIMED_OUT),
                );
                let context = OperationContext::child(
                    epoch.clone(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                );
                let (reply, receiver) = oneshot::channel();
                handle.send(epoch, command(context.clone(), reply)).unwrap();
                context.revoke();
                actor.drive_once().await.unwrap();
                assert_eq!(receiver.await.unwrap(), Err(zx::Status::CANCELED));
                assert!(effects.lock().unwrap().association_contexts.is_empty());
            }
        });
    }

    #[test]
    fn bridge_preserves_association_authority_and_original_deadline() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (mut bridge, mut actor, _) = parts(fake);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            let context =
                OperationContext::child(bridge.execution.epoch.borrow().clone(), deadline);
            *bridge.execution.operation.lock().unwrap() = context.clone();
            actor
                .run_until(async {
                    bridge.install_key(&Default::default()).await?;
                    bridge
                        .notify_association_complete(Default::default())
                        .await?;
                    bridge.clear_association(&Default::default()).await
                })
                .await
                .unwrap()
                .unwrap();
            context.revoke();
            let effects = effects.lock().unwrap();
            assert_eq!(effects.association_contexts.len(), 3);
            for received in &effects.association_contexts {
                assert_eq!(received.deadline(), deadline);
                assert_eq!(
                    received.check(std::time::Instant::now()),
                    Err(zx::Status::CANCELED)
                );
            }
        });
    }

    #[test]
    fn bridge_preserves_channel_authority_and_original_deadline() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (mut bridge, mut actor, _) = parts(fake);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            let context =
                OperationContext::child(bridge.execution.epoch.borrow().clone(), deadline);
            *bridge.execution.operation.lock().unwrap() = context.clone();
            actor
                .run_until(bridge.set_channel(
                    wlan_channel(),
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    wlan_channel(),
                ))
                .await
                .unwrap()
                .unwrap();
            let effects = effects.lock().unwrap();
            assert_eq!(effects.channel_contexts[0].deadline(), deadline);
            context.revoke();
            assert_eq!(
                effects.channel_contexts[0].check(std::time::Instant::now()),
                Err(zx::Status::CANCELED)
            );
        });
    }

    #[test]
    fn deferred_downcall_leaves_exclusive_actor_available_and_reports_completion_error() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (reply, completion) = oneshot::channel();
            effects.lock().unwrap().channel_completion = Some(completion);
            let (mut bridge, mut actor, _) = parts(fake);
            let mut operation = std::pin::pin!(bridge.set_channel(
                wlan_channel(),
                fidl_ieee80211::ChannelBandwidth::Cbw20,
                wlan_channel(),
            ));
            assert!(operation.as_mut().now_or_never().is_none());
            actor.drive_once().await.unwrap();
            assert_eq!(effects.lock().unwrap().calls, ["channel"]);
            assert!(!actor.drive_once().await.unwrap());
            reply.send(Err(zx::Status::IO)).unwrap();
            assert_eq!(
                actor.run_until(operation).await.unwrap(),
                Err(zx::Status::IO)
            );
        });
    }

    #[test]
    fn independent_hardware_owner_completes_awaited_device_operation() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (completion, receiver) = oneshot::channel();
            effects.lock().unwrap().channel_completion = Some(receiver);
            effects.lock().unwrap().complete_channel_on_drive = Some(completion);
            let (mut bridge, mut actor, _) = parts(fake);
            actor
                .start(Box::new(UpcallSender(Arc::new(Mutex::new(UpcallQueue {
                    epoch: OperationEpoch::new(),
                    live: true,
                    overflowed: false,
                    raw_queued: 0,
                    notify: Arc::new(tokio::sync::Notify::new()),
                    queue: VecDeque::new(),
                })))))
                .unwrap();
            let (mut control, commands) = mpsc::channel(4);
            let task = tokio::task::spawn_local(actor.serve(commands));
            // No runtime pump or actor.drive_once here: the awaited operation
            // only completes if the independently owned hardware keeps moving.
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                bridge.set_channel(
                    wlan_channel(),
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    wlan_channel(),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            control.try_send(crate::driver::OwnerCommand::Stop).unwrap();
            let (_owner, result) = task.await.unwrap();
            result.unwrap();
            assert_eq!(effects.lock().unwrap().calls, ["start", "channel", "stop"]);
        });
    }

    fn independent_owner(fake: Fake) -> (HostMlmeDevice, crate::driver::HardwareOwner<Fake>) {
        let (bridge, mut actor, _) = parts(fake);
        actor
            .start(Box::new(UpcallSender(Arc::new(Mutex::new(UpcallQueue {
                epoch: OperationEpoch::new(),
                live: true,
                overflowed: false,
                raw_queued: 0,
                notify: Arc::new(tokio::sync::Notify::new()),
                queue: VecDeque::new(),
            })))))
            .unwrap();
        let (control, commands) = mpsc::channel(1);
        let task = tokio::task::spawn_local(actor.serve(commands));
        (
            bridge,
            crate::driver::HardwareOwner::Running { task, control },
        )
    }

    #[test]
    fn event_driven_owner_sleeps_until_device_or_mailbox_wakes_it() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().event_driven = true;
            let (mut bridge, mut owner) = independent_owner(fake);
            tokio::task::yield_now().await;
            let idle_count = effects.lock().unwrap().drive_count;
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(effects.lock().unwrap().drive_count, idle_count);

            let wake = effects.lock().unwrap().wake.clone().unwrap();
            wake.wake();
            tokio::task::yield_now().await;
            assert!(effects.lock().unwrap().drive_count > idle_count);
            let after_irq = effects.lock().unwrap().drive_count;
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(effects.lock().unwrap().drive_count, after_irq);

            bridge.wlan_softmac_query_response().await.unwrap();
            assert!(effects.lock().unwrap().calls.contains(&"query"));
            // Releasing the completed request may wake its owner once to
            // discard cancellation bookkeeping; that is not periodic polling.
            tokio::task::yield_now().await;
            let after_mailbox = effects.lock().unwrap().drive_count;
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(effects.lock().unwrap().drive_count, after_mailbox);
            owner.request_stop(false);
            owner.join().await;
            owner.certify(false).unwrap();
        });
    }

    #[test]
    fn owner_deadline_survives_unrelated_wakes_and_returns_to_idle() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().event_driven = true;
            let original = Instant::now() + Duration::from_millis(30);
            effects.lock().unwrap().observation_deadline = Some(original);
            let (mut bridge, mut owner) = independent_owner(fake);
            for _ in 0..3 {
                bridge.wlan_softmac_query_response().await.unwrap();
                assert_eq!(effects.lock().unwrap().observation_deadline, Some(original));
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            tokio::time::timeout(Duration::from_secs(1), async {
                while effects.lock().unwrap().observation_deadline.is_some() {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .unwrap();
            let count = effects.lock().unwrap().drive_count;
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(effects.lock().unwrap().drive_count, count);
            owner.request_stop(false);
            owner.join().await;
            owner.certify(false).unwrap();
        });
    }

    #[test]
    fn canceling_hardware_join_retains_owner_for_stop_retry() {
        run_local_test(async {
            let (fake, effects) = Fake::new(1);
            let (_bridge, mut owner) = independent_owner(fake);
            {
                let mut join = std::pin::pin!(owner.join());
                assert!(join.as_mut().now_or_never().is_none());
            }
            assert!(matches!(
                &owner,
                crate::driver::HardwareOwner::Running { .. }
            ));
            owner.request_stop(false);
            owner.join().await;
            assert!(owner.observe().unwrap().is_err());
            owner.certify(false).unwrap();
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
    fn full_owner_queue_cannot_lose_reset_escalation() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (_bridge, mut owner) = independent_owner(fake);
            loop {
                let (reply, _) = oneshot::channel();
                if owner
                    .send(crate::driver::OwnerCommand::Link(false, reply))
                    .is_err()
                {
                    break;
                }
            }
            owner.request_stop(true);
            owner.join().await;
            // Closing a full queue requests stop; the retained terminal intent
            // still requires reset on the returned owner before certification.
            owner.certify(true).unwrap();
            assert!(effects.lock().unwrap().calls.contains(&"reset"));
        });
    }

    #[test]
    fn dropping_hardware_owner_stops_a_suspended_device_operation() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let (_completion, receiver) = oneshot::channel();
            effects.lock().unwrap().channel_completion = Some(receiver);
            let (mut bridge, owner) = independent_owner(fake);
            let mut operation = std::pin::pin!(bridge.set_channel(
                wlan_channel(),
                fidl_ieee80211::ChannelBandwidth::Cbw20,
                wlan_channel(),
            ));
            assert!(operation.as_mut().now_or_never().is_none());
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while !effects.lock().unwrap().calls.contains(&"channel") {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            drop(owner);
            assert_eq!(operation.await, Err(zx::Status::CANCELED));
            assert!(effects.lock().unwrap().calls.contains(&"stop"));
        });
    }

    #[test]
    fn dropping_completion_does_not_stop_or_revoke_driver_work() {
        run_local_test(async {
            let (mut fake, effects) = Fake::new(0);
            let (reply, completion) = oneshot::channel();
            effects.lock().unwrap().channel_completion = Some(completion);
            let completion = fake.set_channel(
                OperationContext::new(
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                ),
                Default::default(),
            );
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
            let (mut device, mut actor, effects) = parts(fake);
            assert_eq!(
                device.start_passive_scan(&Default::default()).await,
                Err(zx::Status::BAD_STATE)
            );
            let context = OperationContext::child(
                device.execution.epoch.borrow().clone(),
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            );
            device.execution.scan.replace(Some(ScanOperation {
                transaction_id: 1,
                device_scan_id: None,
                context,
            }));
            actor
                .run_until(async {
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
                .await
                .unwrap();
            device
                .send_wlan_frame(
                    vec![1, 0x40, 3].into(),
                    fidl_softmac::WlanTxInfoFlags::empty(),
                    None,
                )
                .unwrap();
            actor.drive_once().await.unwrap();
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
        tokio::time::timeout(Duration::from_secs(1), async {
            // Let the request-serving task accept queued client commands first.
            tokio::task::yield_now().await;
            while !runtime.protocol_idle() || !runtime.upcalls.lock().unwrap().queue.is_empty() {
                runtime.check_tasks().unwrap();
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("autonomous MLME completion");
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
    fn runtime_is_constructible_and_upcalls_are_served_without_a_pump() {
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
            drain_mlme(&mut runtime).await;
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
                    runtime.check_tasks().unwrap();
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(!runtime.protocol_idle());
            assert_eq!(runtime.drive_connect_once().await.unwrap(), None);
            runtime.shutdown().await.unwrap();
            assert!(runtime.protocol.is_none());
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
                        runtime.check_tasks().unwrap();
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                if terminal_before_drop {
                    // Model a result already queued when cancellation races with
                    // an outstanding MLME operation. Use a distinct result so
                    // the synthetic cancellation fallback cannot pass this test.
                    let (mut events, stream) = mpsc::channel(64);
                    events
                        .try_send(
                            (wlan_sme::client::ConnectTransactionEvent::OnConnectResult {
                                result: wlan_sme::client::ConnectResult::Success,
                                is_reconnect: true,
                            })
                            .into_fidl(),
                        )
                        .unwrap();
                    runtime.connect_attempt.as_mut().unwrap().admission =
                        ConnectAdmission::Active(stream);
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
                assert!(!runtime.protocol_idle());
                if terminal_before_drop {
                    tokio::time::timeout(Duration::from_secs(1), async {
                        while runtime.cleanup.as_ref().unwrap().terminal.is_none()
                            || !runtime.cleanup.as_ref().unwrap().transaction_closed
                        {
                            assert_eq!(runtime.drive_disconnect_once().await.unwrap(), None);
                        }
                    })
                    .await
                    .unwrap();
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
                    assert!(runtime.protocol.is_some());
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
            let (mut device, mut owner) = independent_owner(fake);
            let execution = device.execution.clone();
            let context = execution.operation.lock().unwrap().clone();
            let mut indications = device.event_stream.take().unwrap();
            let (mut requests, request_stream) = mpsc::channel(4);
            let (mut events, event_stream) = mpsc::channel(4);
            let (init, initialized) = oneshot::channel();
            let pending = Arc::new(AtomicUsize::new(0));
            let task = tokio::task::spawn_local(crate::mlme::mlme_main_loop::<
                wlan_mlme::client::ClientMlme<HostMlmeDevice>,
            >(
                init,
                Default::default(),
                device,
                request_stream,
                event_stream,
                execution,
                Arc::new(Mutex::new(None)),
                pending.clone(),
                Arc::new(AtomicBool::new(false)),
            ));
            initialized.await.unwrap();
            let before = effects.lock().unwrap().calls.clone();
            let peer = [2, 0, 0, 0, 0, 2];
            pending.fetch_add(1, Ordering::AcqRel);
            requests
                .try_send(crate::mlme::Request {
                    context: context.clone(),
                    scan: None,
                    request: wlan_sme::MlmeRequest::Deauthenticate(
                        fidl_mlme::DeauthenticateRequest {
                            peer_sta_address: peer,
                            reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                        },
                    ),
                })
                .unwrap();
            assert!(matches!(
                indications.next().await.unwrap().1,
                fidl_mlme::MlmeEvent::DeauthenticateConf { resp } if resp.peer_sta_address == peer
            ));
            assert_eq!(effects.lock().unwrap().calls, before);
            let (responder, stopped) = oneshot::channel();
            pending.fetch_add(1, Ordering::AcqRel);
            events
                .try_send(crate::mlme::Event {
                    context,
                    event: crate::mlme::DriverEvent::Stop { responder },
                })
                .unwrap();
            stopped.await.unwrap();
            task.await.unwrap().unwrap();
            owner.request_stop(false);
            owner.join().await;
            owner.certify(false).unwrap();
        });
    }

    #[test]
    fn external_ethernet_peer_must_outlive_protocol_stop() {
        run_local_test(async {
            for close_before_stop in [true, false] {
                let (fake, effects) = Fake::new(0);
                effects.lock().unwrap().simulate_ap = true;
                let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
                runtime
                    .connect(connect_request(), Instant::now() + Duration::from_secs(1))
                    .await
                    .unwrap();
                let ethernet = runtime.take_ethernet_device().unwrap();
                if !close_before_stop {
                    // Retaining the external peer permits ordinary protocol
                    // stop and hardware containment before network revocation.
                    assert_eq!(runtime.shutdown().await, Ok(()));
                    assert!(!runtime.reset_requested);
                    drop(ethernet);
                    continue;
                }
                drop(ethernet);
                let failure = tokio::time::timeout(Duration::from_secs(1), async {
                    loop {
                        if let Err(error) = runtime.check_tasks() {
                            break error;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                assert!(matches!(
                    failure,
                    ConnectError::Driver(DriverError::MlmeTaskFailed)
                ));
                assert!(runtime.reset_requested);
                assert_eq!(runtime.shutdown().await, Err(zx::Status::INTERNAL));
            }
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
            assert!(!runtime.protocol_idle());
            runtime.shutdown().await.unwrap();
            assert!(runtime.protocol.is_none());
            assert!(runtime.protocol_idle());
            assert!(!effects.lock().unwrap().calls.contains(&"channel"));
        });
    }

    #[test]
    fn canceled_shutdown_retains_the_task_until_a_retry_joins_it() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            {
                let mut shutdown = std::pin::pin!(runtime.shutdown());
                let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
                assert!(shutdown.as_mut().poll(&mut cx).is_pending());
            }
            assert!(
                runtime.protocol.is_some(),
                "cancellation must not detach MLME"
            );
            runtime.shutdown().await.unwrap();
            assert!(runtime.protocol.is_none());
            assert!(runtime.protocol_idle());
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
            assert!(!runtime.protocol_idle());
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
                notify: Arc::new(tokio::sync::Notify::new()),
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
    fn all_control_overflow_revokes_and_shutdown_certifies_containment() {
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
            {
                let state = runtime.upcalls.lock().unwrap();
                assert!(!state.live);
                assert!(state.overflowed);
                assert!(state.queue.is_empty());
            }
            assert_eq!(runtime.shutdown().await, Err(zx::Status::INTERNAL));
            assert_eq!(runtime.hardware.observe(), Some(Ok(())));
            assert_eq!(
                effects
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .filter(|call| **call == "reset")
                    .count(),
                1
            );
        });
    }

    #[test]
    fn mlme_initialization_failure_stops_the_started_hardware_owner() {
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
            assert_eq!(
                effects.lock().unwrap().calls,
                ["start", "mac", "query", "stop"]
            );
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
            runtime.shutdown().await.unwrap();
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
                assert_eq!(error, ConnectError::Timeout);
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
                assert_eq!(
                    runtime.shutdown().await,
                    if reset_failure {
                        Err(zx::Status::IO)
                    } else {
                        Ok(())
                    },
                );
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
    fn power_success_cannot_cross_disconnect_revocation() {
        run_local_test(async {
            for acknowledge_first in [false, true] {
                let (fake, _) = Fake::new(0);
                let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
                let (_events, stream) = mpsc::channel(1);
                runtime.connection = Some(Connection::Active(stream));
                let deadline = Instant::now() + Duration::from_secs(2);
                runtime.begin_power_save(true, deadline).unwrap();
                let (context, _) = runtime.power_save.take().unwrap();
                // Control the ACK delivery order while retaining the exact
                // authority selected by real public admission.
                let (reply, receiver) = oneshot::channel();
                runtime.power_save = Some((context, receiver));
                let mut reply = Some(reply);
                if acknowledge_first {
                    reply.take().unwrap().send(Ok(())).unwrap();
                }
                runtime
                    .begin_disconnect(fidl_sme::UserDisconnectReason::FailedToConnect, deadline)
                    .unwrap();
                if let Some(reply) = reply {
                    reply.send(Ok(())).unwrap();
                }
                assert_eq!(
                    runtime.drive_power_save_once().await,
                    Err(zx::Status::CANCELED),
                );
                assert!(runtime.power_save.is_none());
                runtime.shutdown().await.unwrap();
            }
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
            let (mut events, stream) = mpsc::channel(64);
            runtime.connection = Some(Connection::Active(stream));
            events
                .try_send(
                    (wlan_sme::client::ConnectTransactionEvent::OnDisconnect {
                        info: fidl_sme::DisconnectInfo {
                            is_sme_reconnecting: false,
                            disconnect_source: fidl_sme::DisconnectSource::User(
                                fidl_sme::UserDisconnectReason::FailedToConnect,
                            ),
                        },
                    })
                    .into_fidl(),
                )
                .unwrap();
            drop(events);
            runtime.drain_connection_events().unwrap();
            let cleanup_deadline = runtime.cleanup.as_ref().unwrap().deadline;
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
            assert_eq!(runtime.cleanup.as_ref().unwrap().deadline, cleanup_deadline);
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
                let (mut events, stream) = mpsc::channel(64);
                runtime.connection = Some(Connection::Active(stream));
                events
                    .try_send(
                        (wlan_sme::client::ConnectTransactionEvent::OnDisconnect {
                            info: fidl_sme::DisconnectInfo {
                                is_sme_reconnecting: false,
                                disconnect_source: fidl_sme::DisconnectSource::User(
                                    fidl_sme::UserDisconnectReason::FailedToConnect,
                                ),
                            },
                        })
                        .into_fidl(),
                    )
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
            let (mut events, stream) = mpsc::channel(64);
            runtime.connection = Some(Connection::Active(stream));
            events
                .try_send(
                    (wlan_sme::client::ConnectTransactionEvent::OnDisconnect {
                        info: fidl_sme::DisconnectInfo {
                            is_sme_reconnecting: true,
                            disconnect_source: fidl_sme::DisconnectSource::User(
                                fidl_sme::UserDisconnectReason::FailedToConnect,
                            ),
                        },
                    })
                    .into_fidl(),
                )
                .unwrap();
            events
                .try_send(
                    (wlan_sme::client::ConnectTransactionEvent::OnConnectResult {
                        result: wlan_sme::client::ConnectResult::Success,
                        is_reconnect: true,
                    })
                    .into_fidl(),
                )
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
    fn cancel_before_sme_admission_is_explicit_and_keeps_runtime_reusable() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().retry_cleanup = true;
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            let deadline = Instant::now() + Duration::from_secs(1);
            runtime
                .begin_connect(connect_request(), deadline)
                .await
                .unwrap();
            // No yield between admission and revocation: SME has not processed
            // the Connect request. A dropped reply alone must not certify it.
            runtime
                .begin_disconnect(fidl_sme::UserDisconnectReason::WlanSmeUnitTesting, deadline)
                .unwrap();
            let result = runtime
                .cancel_connect(fidl_sme::UserDisconnectReason::WlanSmeUnitTesting, deadline)
                .await
                .unwrap();
            assert_eq!(result.code, fidl_ieee80211::StatusCode::Canceled);
            assert!(!runtime.revoked);
            assert!(
                effects
                    .lock()
                    .unwrap()
                    .calls
                    .contains(&"finish_failed_connect_attempt")
            );
            effects.lock().unwrap().simulate_ap = true;
            assert_eq!(
                runtime
                    .connect(connect_request(), Instant::now() + Duration::from_secs(1),)
                    .await
                    .unwrap()
                    .code,
                fidl_ieee80211::StatusCode::Success
            );
            runtime.shutdown().await.unwrap();
        });
    }

    #[test]
    fn replacement_waiter_deadline_does_not_discard_or_renew_cleanup() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            effects.lock().unwrap().simulate_ap = true;
            effects.lock().unwrap().retry_cleanup = true;
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            runtime
                .connect(connect_request(), Instant::now() + Duration::from_secs(1))
                .await
                .unwrap();
            let (complete, receiver) = oneshot::channel();
            effects.lock().unwrap().clear_completion = Some(receiver);
            let (mut source, stream) = mpsc::channel(4);
            runtime.connection = Some(Connection::Active(stream));
            source
                .try_send(fidl_sme::ConnectTransactionEvent::OnDisconnect {
                    info: fidl_sme::DisconnectInfo {
                        is_sme_reconnecting: false,
                        disconnect_source: fidl_sme::DisconnectSource::User(
                            fidl_sme::UserDisconnectReason::FailedToConnect,
                        ),
                    },
                })
                .unwrap();
            runtime.drain_connection_events().unwrap();
            let cleanup_deadline = runtime.cleanup.as_ref().unwrap().deadline;
            let waiter_deadline = Instant::now() + Duration::from_millis(20);
            assert_eq!(
                runtime
                    .begin_connect(connect_request(), waiter_deadline)
                    .await,
                Err(ConnectError::Timeout)
            );
            assert!(Instant::now() < cleanup_deadline);
            assert_eq!(runtime.cleanup.as_ref().unwrap().deadline, cleanup_deadline);
            assert!(runtime.connect_attempt.is_none());
            assert!(!runtime.revoked);
            complete.send(Ok(())).unwrap();
            runtime
                .disconnect(
                    fidl_sme::UserDisconnectReason::FailedToConnect,
                    Instant::now() + Duration::from_secs(1),
                )
                .await
                .unwrap();
            runtime.shutdown().await.unwrap();
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
            runtime.shutdown().await.unwrap();
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
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            (runtime.connect(request, deadline)).await.unwrap();
            let state = effects.lock().unwrap();
            assert_eq!(state.channels.len(), 1);
            assert_eq!(state.channel_contexts[0].deadline(), deadline);
            assert_eq!(state.join_contexts[0].deadline(), deadline);
            runtime.epoch.revoke();
            assert_eq!(
                state.join_contexts[0].check(std::time::Instant::now()),
                Err(zx::Status::CANCELED)
            );
            assert_eq!(
                state.channel_contexts[0].check(std::time::Instant::now()),
                Err(zx::Status::CANCELED)
            );
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
    fn firmware_power_save_owns_more_data_and_tim_delivery() {
        run_local_test(async {
            for offload in [false, true] {
                let (fake, effects) = Fake::new(0);
                {
                    let mut effects = effects.lock().unwrap();
                    effects.simulate_ap = true;
                    effects.retry_cleanup = true;
                    effects.station_offload.power_save = offload;
                }
                let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
                runtime.connect(
                    connect_request(), Instant::now() + Duration::from_secs(1),
                ).await.unwrap();
                let mut data = stale_data_frame();
                data[1] |= 0x20; // More Data from our associated AP.
                effects.lock().unwrap().upcalls.as_mut().unwrap().recv(data, rx_info());
                drain_mlme(&mut runtime).await;
                let ps_polls = effects.lock().unwrap().ps_polls;
                assert_eq!(ps_polls, usize::from(!offload));

                let peer = connect_request().bss_description.bssid;
                let mut beacon = vec![0x80, 0, 0, 0];
                beacon.extend_from_slice(&[0xff; 6]);
                beacon.extend_from_slice(&peer);
                beacon.extend_from_slice(&peer);
                beacon.extend_from_slice(&[0; 10]); // sequence + TSF
                beacon.extend_from_slice(&[100, 0, 1, 0]);
                // TIM advertises buffered traffic for the fixture's AID 42.
                beacon.extend_from_slice(&[5, 9, 0, 1, 0, 0, 0, 0, 0, 0, 4]);
                effects.lock().unwrap().upcalls.as_mut().unwrap().recv(beacon, rx_info());
                drain_mlme(&mut runtime).await;
                let ps_polls = effects.lock().unwrap().ps_polls;
                assert_eq!(ps_polls, 2 * usize::from(!offload));
                runtime.shutdown().await.unwrap();
            }
        });
    }

    #[test]
    fn ap_deauthentication_keeps_cleanup_authority_until_driver_completion() {
        run_local_test(async {
            let (fake, effects) = Fake::new(0);
            {
                let mut effects = effects.lock().unwrap();
                effects.simulate_ap = true;
                effects.retry_cleanup = true;
            }
            let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
            runtime.connect(
                connect_request(), Instant::now() + Duration::from_secs(1),
            ).await.unwrap();
            let (completion, receiver) = oneshot::channel();
            effects.lock().unwrap().clear_completion = Some(receiver);
            let peer = connect_request().bss_description.bssid;
            let mut deauth = vec![0xc0, 0, 0, 0];
            deauth.extend_from_slice(&device_info().sta_addr);
            deauth.extend_from_slice(&peer);
            deauth.extend_from_slice(&peer);
            deauth.extend_from_slice(&[0, 0, 3, 0]);
            effects.lock().unwrap().upcalls.as_mut().unwrap().recv(deauth, rx_info());
            tokio::time::timeout(Duration::from_secs(1), async {
                while !effects.lock().unwrap().calls.contains(&"clear") {
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
            // Give SME its turns while the real MLME is blocked on the
            // owned device completion. No terminal event may escape yet.
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            let early_event = runtime.next_connection_event().unwrap();
            assert!(early_event.is_none(), "terminal event escaped pending peer cleanup");
            let context = effects.lock().unwrap().association_contexts.last().unwrap().clone();
            assert!(context.check(Instant::now()).is_ok());
            completion.send(Ok(())).unwrap();
            drain_mlme(&mut runtime).await;
            assert!(matches!(
                runtime.next_connection_event().unwrap(),
                Some(fidl_sme::ConnectTransactionEvent::OnDisconnect { .. })
            ));
            runtime.disconnect(
                fidl_sme::UserDisconnectReason::FailedToConnect,
                Instant::now() + Duration::from_secs(1),
            ).await.unwrap();
            runtime.shutdown().await.unwrap();
        });
    }

    #[test]
    fn firmware_connection_loss_is_capability_and_peer_scoped() {
        run_local_test(async {
            for offload in [false, true] {
                let (fake, effects) = Fake::new(0);
                {
                    let mut effects = effects.lock().unwrap();
                    effects.simulate_ap = true;
                    effects.retry_cleanup = true;
                    effects.station_offload.connection_monitor = offload;
                }
                let mut runtime = runtime_with_device_info(fake, retry_device_info()).await;
                let peer = connect_request().bss_description.bssid;
                runtime.connect(
                    connect_request(), Instant::now() + Duration::from_secs(1),
                ).await.unwrap();
                effects.lock().unwrap().calls.clear();
                effects.lock().unwrap().upcalls.as_mut().unwrap()
                    .notify_connection_loss([9; 6]);
                drain_mlme(&mut runtime).await;
                assert!(!effects.lock().unwrap().calls.contains(&"clear"));
                effects.lock().unwrap().upcalls.as_mut().unwrap()
                    .notify_connection_loss(peer);
                drain_mlme(&mut runtime).await;
                assert_eq!(effects.lock().unwrap().calls.contains(&"clear"), offload);
                runtime.shutdown().await.unwrap();
            }
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
            runtime.shutdown().await.unwrap();
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
    fn active_scan_segments_keep_original_authority_and_cancellation_blocks_followup() {
        for cancel in [false, true] {
            run_local_test(async {
                let (fake, effects) = Fake::new(0);
                let five = fidl_ieee80211::ChannelNumber {
                    band: fidl_ieee80211::WlanBand::FiveGhz,
                    number: 36,
                };
                effects.lock().unwrap().extra_band =
                    Some(fidl_softmac::WlanSoftmacBandCapability {
                        band: Some(five.band),
                        basic_rates: Some(vec![12, 24, 48]),
                        primary_channels: Some(vec![five]),
                        ..Default::default()
                    });
                let mut info = retry_device_info();
                info.bands.push(fidl_mlme::BandCapability {
                    band: five.band,
                    basic_rates: vec![12, 24, 48],
                    ht_cap: None,
                    vht_cap: None,
                    primary_channels: vec![five],
                });
                // Pinned SME filters every 5GHz active channel unless DFS
                // support is present, including non-DFS channel 36.
                let mut spectrum = fidl_common::SpectrumManagementSupport::default();
                spectrum.dfs.get_or_insert_default().supported = Some(true);
                let mut runtime = ClientRuntime::new(
                    fake,
                    Default::default(),
                    info,
                    Default::default(),
                    spectrum,
                    Default::default(),
                )
                .await
                .unwrap();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
                runtime
                    .begin_scan(
                        fidl_sme::ScanRequest::Active(fidl_sme::ActiveScanRequest {
                            ssids: vec![],
                            channels: vec![wlan_channel().number, five.number],
                        }),
                        deadline,
                    )
                    .await
                    .unwrap();
                drain_mlme(&mut runtime).await;
                assert_eq!({ effects.lock().unwrap().scan_contexts.len() }, 1);
                let context = runtime.scan_attempt.as_ref().unwrap().context.clone();
                if cancel {
                    context.revoke();
                }
                let first = effects.lock().unwrap().scan_id;
                effects
                    .lock()
                    .unwrap()
                    .upcalls
                    .as_mut()
                    .unwrap()
                    .notify_scan_complete(zx::Status::OK, first);
                let first_result = runtime.drive_scan_once().await.unwrap();
                drain_mlme(&mut runtime).await;
                assert!(
                    runtime.epoch.is_live(),
                    "scan cancellation must not revoke connection"
                );
                if !cancel {
                    assert_eq!({ effects.lock().unwrap().scan_contexts.len() }, 2);
                    let second = effects.lock().unwrap().scan_id;
                    assert_ne!(first, second);
                    effects
                        .lock()
                        .unwrap()
                        .upcalls
                        .as_mut()
                        .unwrap()
                        .notify_scan_complete(zx::Status::OK, second);
                }
                let mut completed = first_result;
                let result = loop {
                    if let Some(result) = completed.take() {
                        break result;
                    }
                    if let Some(result) = runtime.drive_scan_once().await.unwrap() {
                        break result;
                    }
                    drain_mlme(&mut runtime).await;
                };
                assert_eq!(result.is_ok(), !cancel);
                let effects = effects.lock().unwrap();
                assert_eq!(effects.scan_contexts.len(), if cancel { 1 } else { 2 });
                for context in &effects.scan_contexts {
                    assert_eq!(context.deadline(), deadline);
                    assert!(
                        !context.is_live(),
                        "terminal scan revokes all retained segments"
                    );
                }
            });
        }
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
            let result = loop {
                if let Some(result) = runtime.drive_scan_once().await.unwrap() {
                    break result;
                }
            };
            assert_eq!(result, Err(fidl_sme::ScanErrorCode::InternalError));
            runtime.shutdown().await.unwrap();
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
            runtime.pending.fetch_add(1, Ordering::AcqRel);
            runtime
                .events
                .try_send(crate::mlme::Event {
                    context: runtime
                        .epoch
                        .context(Instant::now() + Duration::from_secs(1)),
                    event: crate::mlme::DriverEvent::ScanComplete {
                        status: zx::Status::OK,
                        scan_id: 1,
                    },
                })
                .unwrap();
            assert!(!runtime.protocol_idle());
            runtime
                .begin_disconnect(
                    fidl_sme::UserDisconnectReason::WlanSmeUnitTesting,
                    Instant::now() + Duration::from_secs(1),
                )
                .unwrap();
            assert!(!runtime.cleanup.as_ref().unwrap().finished);
            assert!(
                !effects
                    .lock()
                    .unwrap()
                    .calls
                    .contains(&"finish_failed_connect_attempt")
            );
            runtime.shutdown().await.unwrap();
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
            let (mut actor, driver) = DriverActor::new(fake);
            let io = Arc::new(Mutex::new(HostIo {
                ethernet,
                replacement_ethernet: replacements,
                unpublished_ethernet_device: None,
                pending_ethernet_devices: VecDeque::new(),
                ethernet_mac_address: mac,
                minstrel: None,
            }));
            let mut host_device = HostMlmeDevice::new(driver, io.clone(), StationOffloadSupport::default());

            actor
                .run_until(host_device.set_ethernet_status(LinkStatus::UP))
                .await
                .unwrap()
                .unwrap();
            actor
                .run_until(host_device.set_ethernet_status(LinkStatus::DOWN))
                .await
                .unwrap()
                .unwrap();
            assert!(io.lock().unwrap().ethernet.is_closed());
            assert_eq!(old_host.properties().unwrap().mac_address, mac);

            effects.lock().unwrap().link_failure = true;
            assert_eq!(
                actor
                    .run_until(host_device.set_ethernet_status(LinkStatus::UP))
                    .await
                    .unwrap(),
                Err(zx::Status::IO)
            );
            assert!(io.lock().unwrap().ethernet.is_closed());
            assert!(io.lock().unwrap().pending_ethernet_devices.is_empty());

            effects.lock().unwrap().link_failure = false;
            actor
                .run_until(host_device.set_ethernet_status(LinkStatus::UP))
                .await
                .unwrap()
                .unwrap();
            {
                let mut state = io.lock().unwrap();
                assert!(!state.ethernet.is_closed());
                assert_eq!(state.pending_ethernet_devices.len(), 1);
                assert_eq!(old_host.properties(), None);
                let replacement = state.pending_ethernet_devices.pop_front().unwrap();
                assert_eq!(replacement.properties().unwrap().mac_address, mac);
            }

            actor
                .run_until(host_device.set_ethernet_status(LinkStatus::UP))
                .await
                .unwrap()
                .unwrap();
            assert!(io.lock().unwrap().pending_ethernet_devices.is_empty());
            actor
                .run_until(host_device.set_ethernet_status(LinkStatus::DOWN))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                actor
                    .run_until(host_device.set_ethernet_status(LinkStatus::UP))
                    .await
                    .unwrap(),
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
                let (actor, driver) = DriverActor::new(fake);
                let io = Arc::new(Mutex::new(HostIo {
                    ethernet,
                    replacement_ethernet: VecDeque::new(),
                    unpublished_ethernet_device: Some(host),
                    pending_ethernet_devices: VecDeque::new(),
                    ethernet_mac_address: mac,
                    minstrel: None,
                }));
                (HostMlmeDevice::new(driver, io.clone(), StationOffloadSupport::default()), actor, io)
            };

            let (mut before_up, mut before_actor, before_up_io) = make_host();
            before_actor
                .run_until(before_up.set_ethernet_status(LinkStatus::DOWN))
                .await
                .unwrap()
                .unwrap();
            {
                let before_up_io = before_up_io.lock().unwrap();
                assert!(before_up_io.unpublished_ethernet_device.is_none());
                assert!(before_up_io.pending_ethernet_devices.is_empty());
                assert!(before_up_io.ethernet.is_closed());
            }

            let (mut while_pending, mut pending_actor, while_pending_io) = make_host();
            pending_actor
                .run_until(while_pending.set_ethernet_status(LinkStatus::UP))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                while_pending_io
                    .lock()
                    .unwrap()
                    .pending_ethernet_devices
                    .len(),
                1
            );
            pending_actor
                .run_until(while_pending.set_ethernet_status(LinkStatus::DOWN))
                .await
                .unwrap()
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
            runtime.shutdown().await.unwrap();
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
            assert_eq!(runtime_instance.shutdown().await, Err(zx::Status::IO));
            runtime_instance.shutdown().await.unwrap();
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
            assert_eq!(runtime_after_failure.shutdown().await, Err(zx::Status::IO));
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
            stopped.shutdown().await.unwrap();
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
            assert_eq!(stop_failed.shutdown().await, Err(zx::Status::IO));
            let calls_after_stop = effects.lock().unwrap().calls.clone();
            assert_eq!(
                (stop_failed.pump_associated_once()).await,
                Err(ConnectError::Driver(DriverError::Stopped))
            );
            assert_eq!(*effects.lock().unwrap().calls, calls_after_stop);
        });
    }
}
