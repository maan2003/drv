// SPDX-License-Identifier: GPL-2.0-only

//! Mechanical MT7921 effect adapter for the pinned production client MLME.
//!
//! This module owns no connection, retry, timer, regulatory, or credential
//! policy. The effect implementation is injected; this crate has no physical
//! transport implementation.

use fdf::ArenaStaticBox;
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_driver as fidl_driver;
use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
use fidl_fuchsia_wlan_mlme as fidl_mlme;
use fidl_fuchsia_wlan_sme as fidl_sme;
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use futures::channel::mpsc;
use futures::{FutureExt, Stream, StreamExt};
use std::pin::Pin;
#[cfg(test)]
use std::sync::MutexGuard;
use std::sync::{Arc, Mutex};
use wlan_mlme::MlmeImpl;
use wlan_mlme::device::{DeviceOps, LinkStatus};
use wlan_sme::Station;
use wlan_softmac_class_support::{LifecycleAuthorization, PublicWlanIdentity};

use crate::Mt7921SoftmacAdapter;
use crate::ethernet::{
    EthernetIngressError, EthernetPortConfigError, MlmeEthernetSink, Mt7921EthernetDevice,
    Mt7921EthernetTx, ethernet_port,
};
use fuchsia_softmac_port::{HardwareScanEvent, SoftmacHardware};

#[derive(Clone, Copy)]
struct SafeAuthStage {
    algorithm: u16,
    transaction: u16,
    status: u16,
    rejected_group: Option<u16>,
}

fn safe_auth_stage(bytes: &[u8]) -> Option<SafeAuthStage> {
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
    Some(SafeAuthStage {
        algorithm,
        transaction,
        status,
        rejected_group,
    })
}

fn safe_sae_group(frame: &fidl_mlme::SaeFrame) -> Option<u16> {
    (frame.seq_num == 1 && frame.sae_fields.len() >= 2)
        .then(|| u16::from_le_bytes([frame.sae_fields[0], frame.sae_fields[1]]))
}

/// Immutable values reported through the pinned `DeviceOps` query seams.
#[derive(Clone)]
pub struct ClientSupport {
    pub query: fidl_softmac::WlanSoftmacQueryResponse,
    pub discovery: fidl_softmac::DiscoverySupport,
    pub mac_sublayer: fidl_common::MacSublayerSupport,
    pub security: fidl_common::SecuritySupport,
    pub spectrum_management: fidl_common::SpectrumManagementSupport,
}

/// One received frame and the source-exact receive status delivered with it.
///
/// This intentionally has no `Debug` implementation: an 802.11 frame can
/// contain SAE, RSN, EAPOL, or other secret-adjacent material.
pub struct ClientRxFrame {
    pub bytes: Vec<u8>,
    pub status: fidl_softmac::WlanRxInfo,
    pub security: Option<ClientRxSecurity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientRxSecurity {
    pub wcid: u16,
    pub tid: u8,
    pub key_id: u8,
    pub security_mode: u8,
    pub cm: bool,
    pub clm: bool,
    pub icv_error: bool,
    pub mic_error: bool,
    pub fcs_error: bool,
    pub pn: Option<[u8; 6]>,
}

/// Synchronous, transport-owned client I/O boundary.  DeviceOps holds the
/// composed backend lock while calling this interface, so an implementation
/// owns the real mechanics directly and must not defer work or retain loader
/// pointers. Successful UNI/TX returns include the matching firmware/device
/// completion.
pub trait Mt7921ClientIo {
    fn submit_uni(&mut self, expected_cid: u8, encoded: &[u8]) -> Result<(), zx::Status>;
    fn submit_edca(&mut self, encoded: &[u8]) -> Result<(), zx::Status>;
    fn submit_ce_no_ack(&mut self, encoded: &[u8]) -> Result<(), zx::Status>;
    fn diagnostic_association_snapshot(&mut self, _: u64) -> Result<(), zx::Status> {
        Ok(())
    }
    fn transmit_client(
        &mut self,
        bytes: &[u8],
        flags: fidl_softmac::WlanTxInfoFlags,
    ) -> Result<(), zx::Status>;
    fn next_client_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status>;
}

/// Hardware-owned association/WCID and controlled-port ordering state.
///
/// This retains only public metadata. Traffic-key bytes are consumed by the
/// injected effect and must never be copied into this state.
#[derive(Default)]
pub struct Mt7921AssociationState {
    peer: Option<[u8; 6]>,
    wcid: Option<u16>,
    mfp_required: bool,
    pairwise_key: bool,
    group_key: bool,
    integrity_group_key: bool,
    link_up: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssociationTeardown {
    pub peer: [u8; 6],
    pub wcid: u16,
    pub close_link: bool,
    pub remove_pairwise_key: bool,
    pub remove_group_key: bool,
    pub remove_integrity_group_key: bool,
}

impl Mt7921AssociationState {
    pub fn program(
        &mut self,
        configuration: &fidl_softmac::WlanAssociationConfig,
        wcid: u16,
        mfp_required: bool,
    ) -> Result<(), zx::Status> {
        if self.wcid.is_some() || wcid >= 20 {
            return Err(zx::Status::BAD_STATE);
        }
        let peer = PublicWlanIdentity::new(configuration.bssid.ok_or(zx::Status::INVALID_ARGS)?)
            .map_err(|_| zx::Status::INVALID_ARGS)?
            .bytes();
        if configuration.aid.is_none_or(|aid| aid == 0) {
            return Err(zx::Status::INVALID_ARGS);
        }
        self.peer = Some(peer);
        self.wcid = Some(wcid);
        self.mfp_required = mfp_required;
        Ok(())
    }

    /// Validate one key after the firmware effect has installed it, then
    /// publish only its non-secret readiness bit.
    pub fn key_installed(
        &mut self,
        configuration: &fidl_softmac::WlanKeyConfiguration,
    ) -> Result<(), zx::Status> {
        let peer = self.peer.ok_or(zx::Status::BAD_STATE)?;
        if configuration.key.as_ref().is_none_or(Vec::is_empty)
            || configuration.protection != Some(fidl_softmac::WlanProtection::RxTx)
        {
            return Err(zx::Status::INVALID_ARGS);
        }
        match configuration.key_type.ok_or(zx::Status::INVALID_ARGS)? {
            fidl_ieee80211::KeyType::Pairwise => {
                if configuration.peer_addr != Some(peer) || configuration.key_idx != Some(0) {
                    return Err(zx::Status::INVALID_ARGS);
                }
                self.pairwise_key = true;
            }
            fidl_ieee80211::KeyType::Group => {
                if configuration.peer_addr != Some([0xff; 6]) {
                    return Err(zx::Status::INVALID_ARGS);
                }
                self.group_key = true;
            }
            fidl_ieee80211::KeyType::Igtk => {
                if configuration.peer_addr != Some([0xff; 6]) {
                    return Err(zx::Status::INVALID_ARGS);
                }
                self.integrity_group_key = true;
            }
            _ => return Err(zx::Status::NOT_SUPPORTED),
        }
        Ok(())
    }

    pub fn set_link_up(&mut self) -> Result<(), zx::Status> {
        if !self.pairwise_key || !self.group_key || (self.mfp_required && !self.integrity_group_key)
        {
            return Err(zx::Status::BAD_STATE);
        }
        self.link_up = true;
        Ok(())
    }

    pub fn wcid(&self) -> Option<u16> {
        self.wcid
    }

    /// Revoke readiness synchronously and return the exact hardware teardown
    /// effects in close-port, remove-keys, remove-WCID order.
    pub fn clear(&mut self) -> Option<AssociationTeardown> {
        let teardown = AssociationTeardown {
            peer: self.peer?,
            wcid: self.wcid?,
            close_link: self.link_up,
            remove_pairwise_key: self.pairwise_key,
            remove_group_key: self.group_key,
            remove_integrity_group_key: self.integrity_group_key,
        };
        *self = Self::default();
        Some(teardown)
    }
}

/// Firmware/DMA effects retained by the offline client boundary.
///
/// Errors are already-mapped Zircon statuses. The adapter forwards them
/// unchanged and never retries or interprets them.
pub trait Mt7921ClientEffects {
    /// Immediately poison scan-derived authority in every shared TX handle.
    fn revoke_scan(&mut self);

    /// Enter a newly constructed runtime without carrying TX authorization.
    /// Externally selected BSS evidence may remain retained for Connect.
    fn prepare_runtime_handoff(&mut self) -> ClientRuntimeScanState {
        self.revoke_scan();
        ClientRuntimeScanState::Revoked
    }

    /// Immediately and durably poison every shared TX handle for lifecycle.
    fn revoke_lifecycle(&mut self);

    /// Resolve the protocol request against the driver's physical channel
    /// context. `Current` means no second channel-switch command is allowed.
    fn ensure_channel(
        &self,
        _: fidl_ieee80211::ChannelNumber,
        _: fidl_ieee80211::ChannelBandwidth,
        _: fidl_ieee80211::ChannelNumber,
    ) -> Result<ClientChannelEnsure, zx::Status> {
        Ok(ClientChannelEnsure::TransitionRequired)
    }

    fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        vht_secondary_80_channel: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status>;
    fn join_bss(&mut self, request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status>;
    fn send_wlan_frame(
        &mut self,
        bytes: &[u8],
        flags: fidl_softmac::WlanTxInfoFlags,
        io: &mut dyn Mt7921ClientIo,
    ) -> Result<(), zx::Status>;
    fn install_key(
        &mut self,
        configuration: &fidl_softmac::WlanKeyConfiguration,
        io: &mut dyn Mt7921ClientIo,
    ) -> Result<(), zx::Status>;
    fn notify_association_complete(
        &mut self,
        configuration: &fidl_softmac::WlanAssociationConfig,
        io: &mut dyn Mt7921ClientIo,
    ) -> Result<(), zx::Status>;
    fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
        io: &mut dyn Mt7921ClientIo,
    ) -> Result<(), zx::Status>;
    fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status>;
    fn next_rx(&mut self, io: &mut dyn Mt7921ClientIo)
    -> Result<Option<ClientRxFrame>, zx::Status>;

    /// Begin one device-owned passive scan transaction.
    fn begin_passive_scan(
        &mut self,
        scan_id: u64,
        channels: &[fidl_ieee80211::ChannelNumber],
    ) -> Result<(), zx::Status>;

    /// Observe one source-preserving passive scan advertisement.
    fn observe_passive_scan(
        &mut self,
        scan_id: u64,
        observation: &fuchsia_softmac_port::ScanObservation,
    ) -> Result<(), zx::Status>;

    /// Complete or revoke scan-derived authorization in the TX backend.
    fn complete_passive_scan(&mut self, scan_id: u64, success: bool) -> Result<(), zx::Status>;

    /// Revoke all run-scoped TX state after reset.
    fn reset(&mut self) -> Result<(), zx::Status>;

    /// Revoke all run-scoped TX state after stop.
    fn stop(&mut self) -> Result<(), zx::Status>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientChannelEnsure {
    Current,
    TransitionRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientRuntimeScanState {
    Revoked,
    ExternalSelection,
}

trait Mt7921ClientScan: Mt7921ClientIo {
    fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status>;
    fn start_passive_scan(
        &mut self,
        request: fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status>;
    fn cancel_scan(
        &mut self,
        request: fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status>;
}

struct NoClientScan;

impl Mt7921ClientIo for NoClientScan {
    fn submit_uni(&mut self, _: u8, _: &[u8]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn submit_edca(&mut self, _: &[u8]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn submit_ce_no_ack(&mut self, _: &[u8]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn transmit_client(
        &mut self,
        _: &[u8],
        _: fidl_softmac::WlanTxInfoFlags,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn next_client_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
        Ok(None)
    }
}

impl Mt7921ClientScan for NoClientScan {
    fn set_channel(
        &mut self,
        _: fidl_ieee80211::ChannelNumber,
        _: fidl_ieee80211::ChannelBandwidth,
        _: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn start_passive_scan(
        &mut self,
        _: fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn cancel_scan(
        &mut self,
        _: fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
}

impl<T: crate::Mt7921PassiveTransport> Mt7921ClientScan for Mt7921SoftmacAdapter<T> {
    fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        SoftmacHardware::set_channel(
            self,
            crate::set_channel_request(primary, bandwidth, Some(secondary)),
        )
        .map_err(|_| zx::Status::IO)
    }
    fn start_passive_scan(
        &mut self,
        request: fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        SoftmacHardware::start_passive_scan(self, request).map_err(|_| zx::Status::IO)
    }
    fn cancel_scan(
        &mut self,
        request: fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        SoftmacHardware::cancel_scan(self, request).map_err(|_| zx::Status::IO)
    }
}

impl<T: crate::Mt7921PassiveTransport> Mt7921ClientIo for Mt7921SoftmacAdapter<T> {
    fn submit_uni(&mut self, expected_cid: u8, encoded: &[u8]) -> Result<(), zx::Status> {
        self.with_transport_mut(|transport| transport.submit_client_uni(expected_cid, encoded))
    }
    fn submit_edca(&mut self, encoded: &[u8]) -> Result<(), zx::Status> {
        self.with_transport_mut(|transport| transport.submit_client_edca(encoded))
    }
    fn submit_ce_no_ack(&mut self, encoded: &[u8]) -> Result<(), zx::Status> {
        self.with_transport_mut(|transport| transport.submit_client_ce_no_ack(encoded))
    }
    fn diagnostic_association_snapshot(&mut self, generation: u64) -> Result<(), zx::Status> {
        self.with_transport_mut(|transport| transport.diagnostic_association_snapshot(generation))
    }
    fn transmit_client(
        &mut self,
        bytes: &[u8],
        flags: fidl_softmac::WlanTxInfoFlags,
    ) -> Result<(), zx::Status> {
        self.with_transport_mut(|transport| transport.transmit_client(bytes, flags))
    }
    fn next_client_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
        self.with_transport_mut(|transport| transport.next_client_rx())
    }
}

/// Cloneable physical scan edge retained after the device enters `ClientMlme`.
/// Polling updates the same TX backend owned by the device; a beacon alone
/// cannot authorize TX without its matching successful completion.
pub struct Mt7921ScanRunner<E, T> {
    backend: Arc<Mutex<ComposedBackend<E, Mt7921SoftmacAdapter<T>>>>,
}

impl<E, T> Clone for Mt7921ScanRunner<E, T> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
        }
    }
}

impl<E: Mt7921ClientEffects, T: crate::Mt7921PassiveTransport> Mt7921ScanRunner<E, T> {
    /// Borrow the already-connected pinned MLME as the only associated data
    /// plane. The returned pump cannot outlive either the MLME or this runner.
    pub fn associated_data_pump<'a>(
        &'a self,
        mlme: &'a mut wlan_mlme::client::ClientMlme<Mt7921ClientDevice<E, Mt7921SoftmacAdapter<T>>>,
    ) -> crate::ethernet::PinnedAssociatedDataPump<'a, E, T> {
        crate::ethernet::PinnedAssociatedDataPump::new(mlme, self)
    }

    /// Run one short-lived physical operation without transferring the
    /// DeviceOps-owned backend or its revocation state.
    pub fn with_physical<R>(&self, operation: impl FnOnce(&mut Mt7921SoftmacAdapter<T>) -> R) -> R {
        operation(&mut self.backend.lock().unwrap().scan)
    }
    pub fn poll(&self) -> Result<Option<HardwareScanEvent>, zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        let event = match backend.scan.next_scan_event() {
            Ok(event) => event,
            Err(_) => {
                backend.authorization.invalidate_scan();
                backend.effects.revoke_scan();
                if let Some(scan_id) = backend.active_scan_id.take() {
                    let _ = backend.effects.complete_passive_scan(scan_id, false);
                }
                return Err(zx::Status::IO);
            }
        };
        if let Some(event) = &event {
            match event {
                HardwareScanEvent::Observation(observation) => {
                    let Some(scan_id) = backend.active_scan_id else {
                        backend.authorization.invalidate_scan();
                        backend.effects.revoke_scan();
                        return Err(zx::Status::BAD_STATE);
                    };
                    if let Err(status) = backend.effects.observe_passive_scan(scan_id, observation)
                    {
                        backend.authorization.invalidate_scan();
                        backend.effects.revoke_scan();
                        backend.active_scan_id = None;
                        let _ = backend.effects.complete_passive_scan(scan_id, false);
                        return Err(status);
                    }
                }
                HardwareScanEvent::Complete { scan_id, success } => {
                    if backend.active_scan_id != Some(*scan_id) {
                        backend.authorization.invalidate_scan();
                        backend.effects.revoke_scan();
                        return Err(zx::Status::BAD_STATE);
                    }
                    if !success {
                        backend.authorization.invalidate_scan();
                        backend.effects.revoke_scan();
                    }
                    if let Err(status) = backend.effects.complete_passive_scan(*scan_id, *success) {
                        backend.authorization.invalidate_scan();
                        backend.effects.revoke_scan();
                        backend.active_scan_id = None;
                        return Err(status);
                    }
                    backend.active_scan_id = None;
                    if *success {
                        backend.authorization.authorize_scan();
                    } else {
                        backend.authorization.invalidate_scan();
                    }
                    if !backend.authorization.permits_tx() {
                        backend.effects.revoke_scan();
                    }
                }
            }
        }
        Ok(event)
    }

    /// Deliver at most one descriptor-validated raw 802.11 frame from the
    /// run-scoped effects owner into the pinned client MLME. This remains
    /// callable after `Mt7921ClientDevice` has moved into `ClientMlme`.
    pub async fn pump_client_rx(
        &self,
        mlme: &mut wlan_mlme::client::ClientMlme<Mt7921ClientDevice<E, Mt7921SoftmacAdapter<T>>>,
    ) -> Result<bool, zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        if let Some(status) = backend.association_activation_failure {
            return Err(status);
        }
        let ComposedBackend { effects, scan, .. } = &mut *backend;
        let frame = effects.next_rx(scan)?;
        drop(backend);
        let Some(ClientRxFrame { bytes, status, .. }) = frame else {
            return Ok(false);
        };
        let status = wlan_softmac_class_support::rx_carrier(&bytes, status, false)
            .map_or(status, |carrier| carrier.status);
        if let Some(auth) = safe_auth_stage(&bytes) {
            println!(
                "client_mlme_rx stage=adapter_return algorithm={} transaction={} status={} rejected_group={:?} band={:?} primary={}",
                auth.algorithm,
                auth.transaction,
                auth.status,
                auth.rejected_group,
                status.primary.band,
                status.primary.number
            );
        }
        let eapol = bytes
            .windows(8)
            .any(|window| window == [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
        if eapol {
            println!("client_eapol_stage=adapter_return controlled_port_independent=true");
        }
        wlan_mlme::MlmeImpl::handle_mac_frame_rx(mlme, &bytes, status, fuchsia_trace::Id::new())
            .await;
        if let Some(auth) = safe_auth_stage(&bytes) {
            // ClientMlme does not expose its private state/disposition. This
            // marker means the receive future completed, not that MLME
            // accepted the frame; the following event marker proves that.
            println!(
                "client_mlme_rx stage=handle_complete algorithm={} transaction={} status={} rejected_group={:?}",
                auth.algorithm, auth.transaction, auth.status, auth.rejected_group
            );
        }
        if eapol {
            println!("client_eapol_stage=mlme_handle_complete");
        }
        Ok(true)
    }

    /// Notify the supplied backend that its hardware reset completed and
    /// abandon any in-process scan transaction.
    pub fn reset(&self) -> Result<(), zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        backend.authorization.invalidate_lifecycle();
        backend.effects.revoke_lifecycle();
        backend.active_scan_id = None;
        if let Some(ethernet) = backend.ethernet.as_mut() {
            ethernet.teardown();
        }
        backend.effects.reset()
    }

    /// Notify the supplied backend that it stopped and abandon any in-process
    /// scan transaction.
    pub fn stop(&self) -> Result<(), zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        backend.authorization.invalidate_lifecycle();
        backend.effects.revoke_lifecycle();
        backend.active_scan_id = None;
        if let Some(ethernet) = backend.ethernet.as_mut() {
            ethernet.teardown();
        }
        backend.effects.stop()
    }
}

type SmeTimerAction = Box<dyn FnOnce(&mut wlan_sme::client::ClientSme)>;
type MlmeTimerAction = wlan_mlme::common::timer::Event<wlan_mlme::client::TimedEvent>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinnedConnectError {
    Timeout,
    Failed,
    Driver(PinnedDriverError),
    Containment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinnedDriverError {
    MlmeRequest { name: &'static str, detail: String },
    ClientRx(zx::Status),
    RequestStreamClosed,
    EventStreamClosed,
}

/// Bounded production owner for SME, MLME, timers, device events, and MT7921
/// RX. No host-portable association state can be attached to this runtime.
pub struct PinnedClientRuntime<E, T> {
    sme: wlan_sme::client::ClientSme,
    mlme: wlan_mlme::client::ClientMlme<Mt7921ClientDevice<E, Mt7921SoftmacAdapter<T>>>,
    runner: Mt7921ScanRunner<E, T>,
    requests: wlan_sme::MlmeStream,
    events: mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>,
    sme_timers: Pin<Box<dyn Stream<Item = SmeTimerAction>>>,
    mlme_timers: Pin<Box<dyn Stream<Item = MlmeTimerAction>>>,
    timer_runtime: tokio::runtime::Runtime,
}

impl<E, T> PinnedClientRuntime<E, T>
where
    E: Mt7921ClientEffects,
    T: crate::Mt7921PassiveTransport,
{
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        mut device: Mt7921ClientDevice<E, Mt7921SoftmacAdapter<T>>,
        runner: Mt7921ScanRunner<E, T>,
        sme_config: wlan_sme::client::ClientConfig,
        device_info: fidl_mlme::DeviceInfo,
        security: fidl_common::SecuritySupport,
        spectrum: fidl_common::SpectrumManagementSupport,
        inspector: fuchsia_inspect::Inspector,
    ) -> Result<Self, anyhow::Error> {
        let events = device
            .take_mlme_event_stream()
            .ok_or_else(|| anyhow::anyhow!("MLME event stream was already taken"))?;
        let (mlme_timer, mlme_timer_stream) = wlan_mlme::common::timer::create_timer();
        let mlme =
            wlan_mlme::client::ClientMlme::new(Default::default(), device, mlme_timer).await?;
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
        let timer_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        Ok(Self {
            sme,
            mlme,
            runner,
            requests,
            events,
            sme_timers,
            mlme_timers,
            timer_runtime,
        })
    }

    pub fn sme(&self) -> &wlan_sme::client::ClientSme {
        &self.sme
    }

    pub fn associated_data_pump(&mut self) -> crate::ethernet::PinnedAssociatedDataPump<'_, E, T> {
        self.runner.associated_data_pump(&mut self.mlme)
    }

    async fn drain_control(&mut self, budget: usize) -> Result<(bool, bool), PinnedConnectError> {
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
                            safe_sae_group(frame)
                        );
                    }
                    if eapol_tx {
                        println!("client_eapol_stage=sme_tx_request");
                    }
                    let name = request.name();
                    wlan_mlme::MlmeImpl::handle_mlme_request(&mut self.mlme, request)
                        .await
                        .map_err(|error| {
                            PinnedConnectError::Driver(PinnedDriverError::MlmeRequest {
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
                    return Err(PinnedConnectError::Driver(
                        PinnedDriverError::RequestStreamClosed,
                    ));
                }
            }
            match self.events.try_recv() {
                Ok(event) => {
                    if let fidl_mlme::MlmeEvent::OnSaeFrameRx { frame } = &event {
                        println!(
                            "client_sae_stage=mlme_sae_frame_rx algorithm=3 transaction={} status={} group={:?}",
                            frame.seq_num,
                            frame.status_code.into_primitive(),
                            safe_sae_group(frame)
                        );
                    }
                    if matches!(&event, fidl_mlme::MlmeEvent::EapolInd { .. }) {
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
                    return Err(PinnedConnectError::Driver(
                        PinnedDriverError::EventStreamClosed,
                    ));
                }
            }
            if !cycle_progressed {
                return Ok((progressed, true));
            }
        }
        Ok((progressed, false))
    }

    async fn pump_once(&mut self) -> Result<bool, PinnedConnectError> {
        const CONTROL_BUDGET: usize = 64;

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
            return Ok(progressed);
        }

        if self
            .runner
            .pump_client_rx(&mut self.mlme)
            .await
            .map_err(|status| PinnedConnectError::Driver(PinnedDriverError::ClientRx(status)))?
        {
            progressed = true;
            let (_, quiescent) = self.drain_control(CONTROL_BUDGET).await?;
            if !quiescent {
                println!(
                    "client_runtime_control stage=post_rx_budget_exhausted budget={CONTROL_BUDGET} rx_dequeued=false"
                );
            }
        }
        Ok(progressed)
    }

    pub async fn connect(
        &mut self,
        request: fidl_sme::ConnectRequest,
        deadline: std::time::Instant,
    ) -> Result<(), PinnedConnectError> {
        let result = self.connect_inner(request, deadline).await;
        if result.is_err() {
            self.runner
                .reset()
                .map_err(|_| PinnedConnectError::Containment)?;
        }
        result
    }

    async fn connect_inner(
        &mut self,
        request: fidl_sme::ConnectRequest,
        deadline: std::time::Instant,
    ) -> Result<(), PinnedConnectError> {
        let mut transaction = self.sme.on_connect_command(request);
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(PinnedConnectError::Timeout);
            }
            let progressed = self.pump_once().await?;
            loop {
                match transaction.try_recv() {
                    Ok(wlan_sme::client::ConnectTransactionEvent::OnConnectResult {
                        result,
                        ..
                    }) => {
                        if result != wlan_sme::client::ConnectResult::Success {
                            return Err(PinnedConnectError::Failed);
                        }
                        self.pump_once().await?;
                        return self
                            .sme
                            .status()
                            .is_connected()
                            .then_some(())
                            .ok_or(PinnedConnectError::Failed);
                    }
                    Ok(_) => {}
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Closed) => return Err(PinnedConnectError::Failed),
                }
            }
            if !progressed {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}

/// One-way adapter from production `ClientMlme` effects to MT7921 mechanics.
///
/// Construction composes caller-supplied mechanics but grants no TX authority;
/// the backend validates current physical state at every submission.
pub struct Mt7921ClientDevice<E, S> {
    backend: Arc<Mutex<ComposedBackend<E, S>>>,
    support: ClientSupport,
    event_sink: mpsc::UnboundedSender<fidl_mlme::MlmeEvent>,
    event_stream: Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>,
    minstrel: Option<wlan_mlme::MinstrelWrapper>,
}

struct ComposedBackend<E, S> {
    effects: E,
    scan: S,
    active_scan_id: Option<u64>,
    authorization: LifecycleAuthorization,
    association_activation_failure: Option<zx::Status>,
    ethernet: Option<MlmeEthernetSink>,
}

impl<E, S> Mt7921ClientDevice<E, S> {
    fn from_parts(backend: Arc<Mutex<ComposedBackend<E, S>>>, support: ClientSupport) -> Self {
        let (event_sink, event_stream) = mpsc::unbounded();
        Self {
            backend,
            support,
            event_sink,
            event_stream: Some(event_stream),
            minstrel: None,
        }
    }

    #[cfg(test)]
    fn backend(&self) -> MutexGuard<'_, ComposedBackend<E, S>> {
        self.backend.lock().unwrap()
    }
}

impl<E> Mt7921ClientDevice<E, NoClientScan> {
    #[cfg(test)]
    fn new_offline_fake(effects: E, support: ClientSupport) -> Self {
        Self::from_parts(
            Arc::new(Mutex::new(ComposedBackend {
                effects,
                scan: NoClientScan,
                active_scan_id: None,
                authorization: LifecycleAuthorization::new(true),
                association_activation_failure: None,
                ethernet: None,
            })),
            support,
        )
    }
}

impl<E: Mt7921ClientEffects, T: crate::Mt7921PassiveTransport>
    Mt7921ClientDevice<E, Mt7921SoftmacAdapter<T>>
{
    /// Construct the usable production boundary. TX authorization is checked
    /// by `effects` at every submission; construction grants no authority.
    pub fn new(
        mut effects: E,
        scan: Mt7921SoftmacAdapter<T>,
        support: ClientSupport,
    ) -> (Self, Mt7921ScanRunner<E, T>) {
        let scan_state = effects.prepare_runtime_handoff();
        let backend = Arc::new(Mutex::new(ComposedBackend {
            effects,
            scan,
            active_scan_id: None,
            authorization: LifecycleAuthorization::new(
                scan_state != ClientRuntimeScanState::Revoked,
            ),
            association_activation_failure: None,
            ethernet: None,
        }));
        let runner = Mt7921ScanRunner {
            backend: backend.clone(),
        };
        (Self::from_parts(backend, support), runner)
    }

    /// Construct the production client boundary with the existing Netstack3
    /// Ethernet-II port attached. The query MAC is the single address source.
    pub fn new_with_ethernet(
        mut effects: E,
        scan: Mt7921SoftmacAdapter<T>,
        support: ClientSupport,
        queue_capacity: usize,
    ) -> Result<
        (
            Self,
            Mt7921ScanRunner<E, T>,
            Mt7921EthernetDevice,
            Mt7921EthernetTx,
        ),
        EthernetPortConfigError,
    > {
        let mac = support
            .query
            .sta_addr
            .ok_or(EthernetPortConfigError::InvalidMacAddress)?;
        let mac = PublicWlanIdentity::new(mac)
            .map_err(|_| EthernetPortConfigError::InvalidMacAddress)?
            .bytes();
        let (ethernet_device, ethernet_tx, ethernet_sink) = ethernet_port(mac, queue_capacity)?;
        let scan_state = effects.prepare_runtime_handoff();
        let backend = Arc::new(Mutex::new(ComposedBackend {
            effects,
            scan,
            active_scan_id: None,
            authorization: LifecycleAuthorization::new(
                scan_state != ClientRuntimeScanState::Revoked,
            ),
            association_activation_failure: None,
            ethernet: Some(ethernet_sink),
        }));
        let runner = Mt7921ScanRunner {
            backend: backend.clone(),
        };
        Ok((
            Self::from_parts(backend, support),
            runner,
            ethernet_device,
            ethernet_tx,
        ))
    }
}

impl<E: Mt7921ClientEffects, S: Mt7921ClientIo> Mt7921ClientDevice<E, S> {
    /// Pop exactly one frame/status pair from the injected RX effect queue.
    pub fn next_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        let ComposedBackend { effects, scan, .. } = &mut *backend;
        effects.next_rx(scan)
    }
}

impl<E: Mt7921ClientEffects, S: Mt7921ClientScan> DeviceOps for Mt7921ClientDevice<E, S> {
    async fn wlan_softmac_query_response(
        &mut self,
    ) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status> {
        Ok(self.support.query.clone())
    }

    async fn discovery_support(&mut self) -> Result<fidl_softmac::DiscoverySupport, zx::Status> {
        Ok(self.support.discovery.clone())
    }

    async fn mac_sublayer_support(
        &mut self,
    ) -> Result<fidl_common::MacSublayerSupport, zx::Status> {
        Ok(self.support.mac_sublayer.clone())
    }

    async fn security_support(&mut self) -> Result<fidl_common::SecuritySupport, zx::Status> {
        Ok(self.support.security.clone())
    }

    async fn spectrum_management_support(
        &mut self,
    ) -> Result<fidl_common::SpectrumManagementSupport, zx::Status> {
        Ok(self.support.spectrum_management.clone())
    }

    fn deliver_eth_frame(&mut self, packet: &[u8]) -> Result<(), zx::Status> {
        self.backend
            .lock()
            .unwrap()
            .ethernet
            .as_mut()
            .ok_or(zx::Status::NOT_SUPPORTED)?
            .deliver(packet)
            .map_err(|error| match error {
                EthernetIngressError::Closed => zx::Status::CANCELED,
                EthernetIngressError::LinkDown => zx::Status::BAD_STATE,
                EthernetIngressError::Backpressure => zx::Status::SHOULD_WAIT,
                EthernetIngressError::InvalidFrame(_) => zx::Status::IO_DATA_INTEGRITY,
            })
    }

    fn send_wlan_frame(
        &mut self,
        buffer: ArenaStaticBox<[u8]>,
        tx_flags: fidl_softmac::WlanTxInfoFlags,
        _async_id: Option<fuchsia_trace::Id>,
    ) -> Result<(), zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        if !backend.authorization.permits_tx() {
            return Err(zx::Status::ACCESS_DENIED);
        }
        let associated = backend.authorization.permits_tx();
        let ComposedBackend { effects, scan, .. } = &mut *backend;
        let tx_flags = wlan_softmac_class_support::tx_carrier(&buffer, tx_flags, associated)
            .map_or(tx_flags, |carrier| carrier.flags);
        effects.send_wlan_frame(&buffer, tx_flags, scan)
    }

    async fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        let up = status == LinkStatus::UP;
        backend.effects.set_link_up(up)?;
        if let Some(ethernet) = backend.ethernet.as_mut() {
            ethernet.set_link(up);
        }
        Ok(())
    }

    async fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        vht_secondary_80_channel: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        let current =
            backend
                .effects
                .ensure_channel(primary, bandwidth, vht_secondary_80_channel)?
                == ClientChannelEnsure::Current;
        let tuned = if current {
            Ok(())
        } else {
            backend
                .scan
                .set_channel(primary, bandwidth, vht_secondary_80_channel)
        };
        match tuned {
            Err(zx::Status::NOT_SUPPORTED) => {}
            Err(status) => return Err(status),
            Ok(()) => {}
        }
        // Publish the channel to the TX backend only after physical tuning
        // succeeds (or when this explicitly has no physical scan backend).
        backend
            .effects
            .set_channel(primary, bandwidth, vht_secondary_80_channel)
    }

    async fn set_mac_address(&mut self, _mac_addr: [u8; 6]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn start_passive_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        backend.authorization.invalidate_scan();
        backend.effects.revoke_scan();
        let response = backend.scan.start_passive_scan(request.clone())?;
        let scan_id = response.scan_id.ok_or(zx::Status::IO_INVALID)?;
        backend.active_scan_id = Some(scan_id);
        if let Err(status) = backend
            .effects
            .begin_passive_scan(scan_id, request.channels.as_deref().unwrap_or_default())
        {
            backend.authorization.invalidate_scan();
            let _ = backend
                .scan
                .cancel_scan(fidl_softmac::WlanSoftmacBaseCancelScanRequest {
                    scan_id: Some(scan_id),
                });
            let _ = backend.effects.complete_passive_scan(scan_id, false);
            backend.active_scan_id = None;
            return Err(status);
        }
        Ok(response)
    }

    async fn start_active_scan(
        &mut self,
        _request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn cancel_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        self.backend
            .lock()
            .unwrap()
            .scan
            .cancel_scan(request.clone())
    }

    async fn join_bss(&mut self, request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        self.backend.lock().unwrap().effects.join_bss(request)
    }

    async fn enable_beaconing(
        &mut self,
        _request: fidl_softmac::WlanSoftmacBaseEnableBeaconingRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn disable_beaconing(&mut self) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn install_key(
        &mut self,
        configuration: &fidl_softmac::WlanKeyConfiguration,
    ) -> Result<(), zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        let ComposedBackend { effects, scan, .. } = &mut *backend;
        effects.install_key(configuration, scan)
    }

    async fn notify_association_complete(
        &mut self,
        configuration: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        let result = {
            let ComposedBackend { effects, scan, .. } = &mut *backend;
            effects.notify_association_complete(&configuration, scan)
        };
        if let Err(status) = result {
            backend.association_activation_failure = Some(status);
            println!(
                "client_runtime_control stage=association_activation_failed status={status} rx_dequeued=false"
            );
        }
        result
    }

    async fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        let ComposedBackend { effects, scan, .. } = &mut *backend;
        effects.clear_association(request, scan)
    }

    async fn update_wmm_parameters(
        &mut self,
        _request: &fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    fn take_mlme_event_stream(&mut self) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
        self.event_stream.take()
    }

    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
        self.event_sink
            .unbounded_send(event)
            .map_err(|_| anyhow::anyhow!("MLME event queue closed"))
    }

    fn set_minstrel(&mut self, minstrel: wlan_mlme::MinstrelWrapper) {
        self.minstrel = Some(minstrel);
    }

    fn minstrel(&mut self) -> Option<wlan_mlme::MinstrelWrapper> {
        self.minstrel.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_auth_telemetry_exposes_only_fixed_public_fields() {
        let mut frame = vec![0; 32];
        frame[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        frame[24..26].copy_from_slice(&3u16.to_le_bytes());
        frame[26..28].copy_from_slice(&1u16.to_le_bytes());
        frame[28..30].copy_from_slice(&77u16.to_le_bytes());
        frame[30..32].copy_from_slice(&20u16.to_le_bytes());

        let stage = safe_auth_stage(&frame).unwrap();
        assert_eq!(stage.algorithm, 3);
        assert_eq!(stage.transaction, 1);
        assert_eq!(stage.status, 77);
        assert_eq!(stage.rejected_group, Some(20));
        assert!(safe_auth_stage(&frame[..31]).is_none());
        frame[0] = 0x08;
        assert!(safe_auth_stage(&frame).is_none());
    }
    use futures::StreamExt;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use wlan_mlme::MlmeImpl;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum PassiveCall {
        Channel(mt7921_port_spike::CandidateChannel),
        Start(crate::PassiveScanCommand),
        Cancel(u64),
    }

    #[derive(Clone, Default)]
    struct FakePassiveTransport(Arc<Mutex<Vec<PassiveCall>>>);

    #[derive(Debug)]
    struct FakePassiveError;

    impl std::fmt::Display for FakePassiveError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("fake passive transport error")
        }
    }

    impl std::error::Error for FakePassiveError {}

    impl crate::Mt7921PassiveTransport for FakePassiveTransport {
        type Error = FakePassiveError;

        fn set_channel(
            &mut self,
            channel: mt7921_port_spike::CandidateChannel,
        ) -> Result<(), Self::Error> {
            self.0.lock().unwrap().push(PassiveCall::Channel(channel));
            Ok(())
        }

        fn start_passive_scan(
            &mut self,
            command: crate::PassiveScanCommand,
        ) -> Result<(), Self::Error> {
            self.0.lock().unwrap().push(PassiveCall::Start(command));
            Ok(())
        }

        fn cancel_passive_scan(&mut self, scan_id: u64) -> Result<(), Self::Error> {
            self.0.lock().unwrap().push(PassiveCall::Cancel(scan_id));
            Ok(())
        }

        fn next_event(&mut self) -> Result<Option<crate::TransportEvent>, Self::Error> {
            Ok(None)
        }
    }

    fn nic() -> mt7921_port_spike::NicCapability {
        mt7921_port_spike::NicCapability {
            element_count: 2,
            mac_address: Some([2, 0, 0, 0, 0, 1]),
            phy: Some(mt7921_port_spike::NicPhyCapability {
                ht: true,
                vht: true,
                has_5ghz: true,
                max_bandwidth: 2,
                spatial_streams: 2,
                hardware_path: 3,
                he: true,
            }),
            has_6ghz: Some(false),
            chip_capability: None,
            unknown_elements: 0,
        }
    }

    const BSSID: [u8; 6] = [2, 4, 6, 8, 10, 12];
    // Public, synthetic test pattern. This is not key material from a network.
    const FAKE_KEY: [u8; 16] = [0xa5; 16];

    fn open_connect_request() -> fidl_sme::ConnectRequest {
        fidl_sme::ConnectRequest {
            ssid: b"test".to_vec(),
            bss_description: fidl_ieee80211::BssDescription {
                bssid: BSSID,
                bss_type: fidl_ieee80211::BssType::Infrastructure,
                beacon_period: 100,
                capability_info: 1,
                ies: vec![0, 4, b't', b'e', b's', b't', 1, 2, 0x82, 0x84],
                primary: channel(36),
                bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw20,
                vht_secondary_80_channel: channel(0),
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

    fn live_shape_wpa3_connect_request() -> fidl_sme::ConnectRequest {
        let mut request = open_connect_request();
        request.bss_description.capability_info = 0x11;
        request.bss_description.ies = vec![
            0, 4, b't', b'e', b's', b't', 1, 2, 0x82, 0x84, 48, 20, 1, 0, 0, 0x0f, 0xac, 4, 1, 0,
            0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 8, 0xcc, 0,
        ];
        request.authentication = fidl_fuchsia_wlan_internal::Authentication {
            protocol: fidl_fuchsia_wlan_internal::Protocol::Wpa3Personal,
            credentials: Some(Box::new(fidl_fuchsia_wlan_internal::Credentials::Wpa(
                fidl_fuchsia_wlan_internal::WpaCredentials::Passphrase(
                    b"synthetic-password".to_vec(),
                ),
            ))),
        };
        request
    }

    fn wpa2_connect_request() -> fidl_sme::ConnectRequest {
        let mut request = open_connect_request();
        request.bss_description.capability_info = 0x11;
        request.bss_description.ies = vec![
            0, 4, b't', b'e', b's', b't', 1, 2, 0x82, 0x84, 48, 20, 1, 0, 0, 0x0f, 0xac, 4, 1, 0,
            0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 2, 0, 0,
        ];
        request.authentication = fidl_fuchsia_wlan_internal::Authentication {
            protocol: fidl_fuchsia_wlan_internal::Protocol::Wpa2Personal,
            credentials: Some(Box::new(fidl_fuchsia_wlan_internal::Credentials::Wpa(
                fidl_fuchsia_wlan_internal::WpaCredentials::Psk([1; 32]),
            ))),
        };
        request
    }

    fn wpa2_message_1() -> ClientRxFrame {
        let mut bytes = vec![0x08, 0x02, 0, 0];
        bytes.extend_from_slice(&nic().mac_address.unwrap());
        bytes.extend_from_slice(&BSSID);
        bytes.extend_from_slice(&BSSID);
        bytes.extend_from_slice(&[0, 0, 0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
        bytes.extend_from_slice(&[1, 3, 0, 95, 2, 0, 0x8a, 0, 16]);
        bytes.extend_from_slice(&1u64.to_be_bytes());
        bytes.extend_from_slice(&[0x11; 32]);
        bytes.extend_from_slice(&[0; 16 + 8 + 8 + 16]);
        bytes.extend_from_slice(&[0, 0]);
        ClientRxFrame {
            bytes,
            status: rx_status(-30),
            security: None,
        }
    }

    fn wpa2_association_response() -> ClientRxFrame {
        open_response(
            0x01,
            &[
                1, 0, 0, 0, 42, 0, 1, 2, 0x82, 0x84, 48, 20, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f,
                0xac, 4, 1, 0, 0, 0x0f, 0xac, 2, 0, 0,
            ],
        )
    }

    fn open_response(subtype: u8, body: &[u8]) -> ClientRxFrame {
        let mut bytes = vec![0u8; 24];
        bytes[0] = subtype << 4;
        bytes[4..10].copy_from_slice(&nic().mac_address.unwrap());
        bytes[10..16].copy_from_slice(&BSSID);
        bytes[16..22].copy_from_slice(&BSSID);
        bytes.extend_from_slice(body);
        ClientRxFrame {
            bytes,
            status: rx_status(-30),
            security: None,
        }
    }

    fn source_exact_140_byte_open_response(subtype: u8, body: &[u8]) -> ClientRxFrame {
        let metadata_len = 24 + 16 + 16 + 8 + 8 + 2;
        let reported_len = metadata_len + 24 + body.len();
        let mut envelope = vec![0xcc; 140];
        envelope[0..4].copy_from_slice(&((2u32 << 27) | reported_len as u32).to_le_bytes());
        envelope[4..8]
            .copy_from_slice(&((1u32 << 14) | (1 << 11) | (1 << 12) | (1 << 13)).to_le_bytes());
        envelope[8..12].copy_from_slice(&(1u32 << 14).to_le_bytes());
        envelope[12..16].copy_from_slice(&(36u32 << 8).to_le_bytes());
        envelope[24..40].fill(0xa5);
        envelope[40..46].copy_from_slice(&[6, 5, 4, 3, 2, 1]);
        envelope[68..72].copy_from_slice(&0x7878u32.to_le_bytes());
        let frame = &mut envelope[metadata_len..reported_len];
        frame.fill(0);
        frame[0] = subtype << 4;
        frame[4..10].copy_from_slice(&nic().mac_address.unwrap());
        frame[10..16].copy_from_slice(&BSSID);
        frame[16..22].copy_from_slice(&BSSID);
        frame[24..].copy_from_slice(body);
        let parsed = mt7921_port_spike::parse_connac2_rx_frame(&envelope).unwrap();
        assert_eq!(parsed.pn, Some([1, 2, 3, 4, 5, 6]));
        ClientRxFrame {
            bytes: parsed.bytes,
            status: rx_status(parsed.rssi_dbm),
            security: None,
        }
    }

    fn runtime_device_info() -> fidl_mlme::DeviceInfo {
        fidl_mlme::DeviceInfo {
            sta_addr: nic().mac_address.unwrap(),
            factory_addr: nic().mac_address.unwrap(),
            role: fidl_common::WlanMacRole::Client,
            bands: vec![fidl_mlme::BandCapability {
                band: fidl_ieee80211::WlanBand::FiveGhz,
                basic_rates: vec![0x82, 0x84],
                ht_cap: None,
                vht_cap: None,
                primary_channels: vec![channel(36)],
            }],
            softmac_hardware_capability: 0,
            qos_capable: false,
        }
    }

    #[derive(Default)]
    struct FakeEffects {
        order: Vec<&'static str>,
        channel: Option<(
            fidl_ieee80211::ChannelNumber,
            fidl_ieee80211::ChannelBandwidth,
            fidl_ieee80211::ChannelNumber,
        )>,
        join: Option<fidl_driver::JoinBssRequest>,
        frame: Option<Vec<u8>>,
        frames: Vec<Vec<u8>>,
        flags: Option<fidl_softmac::WlanTxInfoFlags>,
        key: Option<fidl_softmac::WlanKeyConfiguration>,
        association: Option<fidl_softmac::WlanAssociationConfig>,
        clear: Option<fidl_softmac::WlanSoftmacBaseClearAssociationRequest>,
        link_up: Option<bool>,
        rx: VecDeque<ClientRxFrame>,
        rx_dequeued: usize,
        fail_on: Option<&'static str>,
        reuse_channel: bool,
    }

    // Deliberately redacted: frames and keys may contain SAE/RSN material.
    impl std::fmt::Debug for FakeEffects {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FakeEffects")
                .field("order", &self.order)
                .field("channel", &self.channel)
                .field("join", &self.join)
                .field("frame", &self.frame.as_ref().map(|_| "<redacted>"))
                .field("flags", &self.flags)
                .field("key", &self.key.as_ref().map(|_| "<redacted>"))
                .field("association", &self.association)
                .field("clear", &self.clear)
                .field("link_up", &self.link_up)
                .field("rx", &format_args!("{} queued", self.rx.len()))
                .finish()
        }
    }

    impl FakeEffects {
        fn hit(&mut self, name: &'static str) -> Result<(), zx::Status> {
            if self.fail_on == Some(name) {
                return Err(zx::Status::IO_REFUSED);
            }
            self.order.push(name);
            Ok(())
        }
    }

    impl Mt7921ClientEffects for FakeEffects {
        fn revoke_scan(&mut self) {}

        fn revoke_lifecycle(&mut self) {}

        fn ensure_channel(
            &self,
            primary: fidl_ieee80211::ChannelNumber,
            bandwidth: fidl_ieee80211::ChannelBandwidth,
            secondary: fidl_ieee80211::ChannelNumber,
        ) -> Result<ClientChannelEnsure, zx::Status> {
            Ok(
                if self.reuse_channel && self.channel == Some((primary, bandwidth, secondary)) {
                    ClientChannelEnsure::Current
                } else {
                    ClientChannelEnsure::TransitionRequired
                },
            )
        }

        fn set_channel(
            &mut self,
            primary: fidl_ieee80211::ChannelNumber,
            bandwidth: fidl_ieee80211::ChannelBandwidth,
            secondary: fidl_ieee80211::ChannelNumber,
        ) -> Result<(), zx::Status> {
            self.hit("channel")?;
            self.channel = Some((primary, bandwidth, secondary));
            Ok(())
        }

        fn join_bss(&mut self, request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
            self.hit("join")?;
            self.join = Some(request.clone());
            Ok(())
        }

        fn send_wlan_frame(
            &mut self,
            bytes: &[u8],
            flags: fidl_softmac::WlanTxInfoFlags,
            _: &mut dyn Mt7921ClientIo,
        ) -> Result<(), zx::Status> {
            self.hit("frame")?;
            self.frame = Some(bytes.to_vec());
            self.frames.push(bytes.to_vec());
            self.flags = Some(flags);
            Ok(())
        }

        fn install_key(
            &mut self,
            configuration: &fidl_softmac::WlanKeyConfiguration,
            _: &mut dyn Mt7921ClientIo,
        ) -> Result<(), zx::Status> {
            self.hit("key")?;
            self.key = Some(configuration.clone());
            Ok(())
        }

        fn notify_association_complete(
            &mut self,
            configuration: &fidl_softmac::WlanAssociationConfig,
            _: &mut dyn Mt7921ClientIo,
        ) -> Result<(), zx::Status> {
            self.hit("association")?;
            self.association = Some(configuration.clone());
            Ok(())
        }

        fn clear_association(
            &mut self,
            request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
            _: &mut dyn Mt7921ClientIo,
        ) -> Result<(), zx::Status> {
            self.hit("clear")?;
            self.clear = Some(request.clone());
            Ok(())
        }

        fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status> {
            self.hit(if up { "link-up" } else { "link-down" })?;
            self.link_up = Some(up);
            Ok(())
        }

        fn next_rx(
            &mut self,
            _: &mut dyn Mt7921ClientIo,
        ) -> Result<Option<ClientRxFrame>, zx::Status> {
            if self.fail_on == Some("rx") {
                return Err(zx::Status::IO_REFUSED);
            }
            let frame = self.rx.pop_front();
            if frame.is_some() {
                self.order.push("rx");
                self.rx_dequeued += 1;
            }
            Ok(frame)
        }

        fn begin_passive_scan(
            &mut self,
            _: u64,
            _: &[fidl_ieee80211::ChannelNumber],
        ) -> Result<(), zx::Status> {
            Ok(())
        }

        fn observe_passive_scan(
            &mut self,
            _: u64,
            _: &fuchsia_softmac_port::ScanObservation,
        ) -> Result<(), zx::Status> {
            Ok(())
        }

        fn complete_passive_scan(&mut self, _: u64, _: bool) -> Result<(), zx::Status> {
            Ok(())
        }

        fn reset(&mut self) -> Result<(), zx::Status> {
            Ok(())
        }

        fn stop(&mut self) -> Result<(), zx::Status> {
            Ok(())
        }
    }

    #[test]
    fn client_mlme_replays_one_exact_physical_channel_context() {
        futures::executor::block_on(async {
            let transport = FakePassiveTransport::default();
            let calls = transport.0.clone();
            let capability = nic();
            let passive = Mt7921SoftmacAdapter::new(
                transport,
                capability,
                mt7921_port_spike::candidate_channels(capability),
                vec![channel(36)],
            )
            .unwrap();
            let (mut device, runner) =
                Mt7921ClientDevice::new(FakeEffects::default(), passive, support());
            runner
                .backend
                .lock()
                .unwrap()
                .authorization
                .authorize_scan();
            DeviceOps::set_channel(
                &mut device,
                channel(36),
                fidl_ieee80211::ChannelBandwidth::Cbw20,
                channel(0),
            )
            .await
            .unwrap();
            assert_eq!(calls.lock().unwrap().len(), 1);

            runner.backend.lock().unwrap().effects.reuse_channel = true;
            DeviceOps::set_channel(
                &mut device,
                channel(36),
                fidl_ieee80211::ChannelBandwidth::Cbw20,
                channel(0),
            )
            .await
            .unwrap();
            assert_eq!(calls.lock().unwrap().len(), 1);

            DeviceOps::set_channel(
                &mut device,
                channel(36),
                fidl_ieee80211::ChannelBandwidth::Cbw40,
                channel(0),
            )
            .await
            .unwrap();
            assert_eq!(calls.lock().unwrap().len(), 2);

            DeviceOps::set_channel(
                &mut device,
                channel(36),
                fidl_ieee80211::ChannelBandwidth::Cbw40,
                channel(0),
            )
            .await
            .unwrap();
            assert_eq!(calls.lock().unwrap().len(), 2);

            assert!(
                DeviceOps::set_channel(
                    &mut device,
                    channel(40),
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    channel(0),
                )
                .await
                .is_err()
            );
        });
    }

    fn support() -> ClientSupport {
        ClientSupport {
            query: fidl_softmac::WlanSoftmacQueryResponse {
                sta_addr: Some([1, 2, 3, 4, 5, 6]),
                hardware_capability: Some(0x420),
                ..Default::default()
            },
            discovery: fidl_softmac::DiscoverySupport {
                scan_offload: Some(fidl_softmac::ScanOffloadExtension {
                    supported: Some(false),
                    scan_cancel_supported: Some(false),
                }),
                ..Default::default()
            },
            mac_sublayer: fidl_common::MacSublayerSupport {
                device: Some(fidl_common::DeviceExtension {
                    mac_implementation_type: Some(fidl_common::MacImplementationType::Softmac),
                    ..Default::default()
                }),
                ..Default::default()
            },
            security: fidl_common::SecuritySupport {
                sae: Some(fidl_common::SaeFeature {
                    driver_handler_supported: Some(false),
                    sme_handler_supported: Some(true),
                    hash_to_element_supported: Some(false),
                }),
                ..Default::default()
            },
            spectrum_management: Default::default(),
        }
    }

    fn rx_status(rssi_dbm: i8) -> fidl_softmac::WlanRxInfo {
        fidl_softmac::WlanRxInfo {
            rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
            valid_fields: fidl_softmac::WlanRxInfoValid::CHAN_WIDTH
                | fidl_softmac::WlanRxInfoValid::RSSI,
            phy: fidl_ieee80211::WlanPhyType::Erp,
            data_rate: 12,
            primary: channel(36),
            bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw20,
            vht_secondary_80_channel: channel(0),
            mcs: 3,
            rssi_dbm,
            snr_dbh: 42,
        }
    }

    fn channel(number: u8) -> fidl_ieee80211::ChannelNumber {
        fidl_ieee80211::ChannelNumber {
            band: fidl_ieee80211::WlanBand::FiveGhz,
            number,
        }
    }

    #[test]
    fn forwards_exact_values_bytes_flags_and_order() {
        futures::executor::block_on(async {
            let mut device =
                Mt7921ClientDevice::new_offline_fake(FakeEffects::default(), support());
            assert_eq!(
                device
                    .wlan_softmac_query_response()
                    .await
                    .unwrap()
                    .hardware_capability,
                Some(0x420)
            );
            assert_eq!(
                device
                    .discovery_support()
                    .await
                    .unwrap()
                    .scan_offload
                    .unwrap()
                    .supported,
                Some(false)
            );
            assert_eq!(
                device.mac_sublayer_support().await.unwrap(),
                support().mac_sublayer
            );
            assert_eq!(device.security_support().await.unwrap(), support().security);
            assert_eq!(
                device.spectrum_management_support().await.unwrap(),
                support().spectrum_management
            );

            device
                .set_channel(
                    channel(36),
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    channel(0),
                )
                .await
                .unwrap();
            let join = fidl_driver::JoinBssRequest {
                bssid: Some(BSSID),
                beacon_period: Some(100),
                ..Default::default()
            };
            device.join_bss(&join).await.unwrap();
            let bytes = vec![0xb0, 0x00, 0, 0, 6, 6, 6, 6, 6, 6, 1, 2, 3, 4];
            let flags = fidl_softmac::WlanTxInfoFlags::PROTECTED
                | fidl_softmac::WlanTxInfoFlags::FAVOR_RELIABILITY;
            device
                .send_wlan_frame(bytes.clone().into(), flags, None)
                .unwrap();
            let key = fidl_softmac::WlanKeyConfiguration {
                peer_addr: Some(BSSID),
                key_idx: Some(2),
                key: Some(FAKE_KEY.to_vec()),
                ..Default::default()
            };
            device.install_key(&key).await.unwrap();
            let association = fidl_softmac::WlanAssociationConfig {
                bssid: Some(BSSID),
                aid: Some(7),
                primary: Some(channel(36)),
                ..Default::default()
            };
            device
                .notify_association_complete(association.clone())
                .await
                .unwrap();
            let clear = fidl_softmac::WlanSoftmacBaseClearAssociationRequest {
                peer_addr: Some(BSSID),
            };
            device.clear_association(&clear).await.unwrap();
            device.set_ethernet_status(LinkStatus::UP).await.unwrap();
            device.set_ethernet_status(LinkStatus::DOWN).await.unwrap();

            let effects = device.backend();
            let effects = &effects.effects;
            assert_eq!(
                effects.order,
                [
                    "channel",
                    "join",
                    "frame",
                    "key",
                    "association",
                    "clear",
                    "link-up",
                    "link-down"
                ]
            );
            assert_eq!(
                effects.channel,
                Some((
                    channel(36),
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    channel(0)
                ))
            );
            assert_eq!(effects.join.as_ref(), Some(&join));
            assert_eq!(effects.frame.as_deref(), Some(bytes.as_slice()));
            assert_eq!(effects.flags, Some(flags));
            assert_eq!(effects.key.as_ref(), Some(&key));
            assert_eq!(effects.association.as_ref(), Some(&association));
            assert_eq!(effects.clear.as_ref(), Some(&clear));
            assert_eq!(effects.link_up, Some(false));
            let debug = format!("{effects:?}");
            assert!(!debug.contains("165"));
            assert!(debug.contains("<redacted>"));
        });
    }

    #[test]
    fn forwards_failure_status_without_retry_or_later_effect() {
        futures::executor::block_on(async {
            let effects = FakeEffects {
                fail_on: Some("key"),
                ..Default::default()
            };
            let mut device = Mt7921ClientDevice::new_offline_fake(effects, support());
            let key = fidl_softmac::WlanKeyConfiguration {
                key: Some(FAKE_KEY.to_vec()),
                ..Default::default()
            };
            assert_eq!(device.install_key(&key).await, Err(zx::Status::IO_REFUSED));
            assert!(device.backend().effects.order.is_empty());
            assert!(device.backend().effects.key.is_none());
        });
    }

    #[test]
    fn queues_mlme_events_and_exact_rx_status_once() {
        futures::executor::block_on(async {
            let status = rx_status(-47);
            let mut effects = FakeEffects::default();
            effects.rx.push_back(ClientRxFrame {
                bytes: vec![8, 1, 2, 3],
                status,
                security: None,
            });
            let mut device = Mt7921ClientDevice::new_offline_fake(effects, support());
            let mut stream = device.take_mlme_event_stream().unwrap();
            assert!(device.take_mlme_event_stream().is_none());
            let event = fidl_mlme::MlmeEvent::OnScanEnd {
                end: fidl_mlme::ScanEnd {
                    txn_id: 99,
                    code: fidl_mlme::ScanResultCode::CanceledByDriverOrFirmware,
                },
            };
            device.send_mlme_event(event.clone()).unwrap();
            assert_eq!(stream.next().await, Some(event));
            let rx = device.next_rx().unwrap().unwrap();
            assert_eq!(rx.bytes, [8, 1, 2, 3]);
            assert_eq!(rx.status, status);
            assert!(device.next_rx().unwrap().is_none());
        });
    }

    #[test]
    fn run_scoped_runner_pumps_rx_after_device_moves_into_mlme() {
        futures::executor::block_on(async {
            let mut effects = FakeEffects::default();
            effects.rx.push_back(ClientRxFrame {
                // A deliberately incomplete data frame is enough to prove the
                // ownership path; the pinned MLME safely rejects its payload.
                bytes: vec![8, 1, 2, 3],
                status: rx_status(-47),
                security: None,
            });
            let transport = FakePassiveTransport::default();
            let capability = nic();
            let passive = Mt7921SoftmacAdapter::new(
                transport,
                capability,
                mt7921_port_spike::candidate_channels(capability),
                vec![channel(36)],
            )
            .unwrap();
            let (device, runner) = Mt7921ClientDevice::new(effects, passive, support());
            let (timer, _timer_stream) = wlan_mlme::common::timer::create_timer();
            let mut mlme = wlan_mlme::client::ClientMlme::new(Default::default(), device, timer)
                .await
                .unwrap();

            {
                use crate::ethernet::{AssociatedDataPump, AssociatedSoftmacTx};
                let mut pump = runner.associated_data_pump(&mut mlme);
                assert!(pump.transmit_ethernet(&[0; 14]).is_err());
                assert!(matches!(
                    pump.pump_receive(std::time::Instant::now()),
                    Err(crate::ethernet::PinnedDataPumpError::Rx(
                        zx::Status::TIMED_OUT
                    ))
                ));
            }
            assert!(runner.pump_client_rx(&mut mlme).await.unwrap());
            assert!(!runner.pump_client_rx(&mut mlme).await.unwrap());
            runner.backend.lock().unwrap().effects.fail_on = Some("rx");
            assert_eq!(
                runner.pump_client_rx(&mut mlme).await,
                Err(zx::Status::IO_REFUSED)
            );
        });
    }

    #[test]
    fn pinned_runtime_reports_live_shape_connect_driver_stage_without_credentials() {
        futures::executor::block_on(async {
            let effects = FakeEffects {
                fail_on: Some("channel"),
                ..Default::default()
            };
            let capability = nic();
            let passive = Mt7921SoftmacAdapter::new(
                FakePassiveTransport::default(),
                capability,
                mt7921_port_spike::candidate_channels(capability),
                vec![channel(36)],
            )
            .unwrap();
            let mut device_support = support();
            device_support.query.sta_addr = nic().mac_address;
            device_support.query.factory_addr = nic().mac_address;
            device_support.query.mac_role = Some(fidl_common::WlanMacRole::Client);
            device_support.query.band_caps = Some(vec![fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_ieee80211::WlanBand::FiveGhz),
                basic_rates: Some(vec![0x82, 0x84]),
                primary_channels: Some(vec![channel(36)]),
                ..Default::default()
            }]);
            let (device, runner) = Mt7921ClientDevice::new(effects, passive, device_support);
            runner
                .backend
                .lock()
                .unwrap()
                .authorization
                .authorize_scan();
            let mut support_info = support();
            support_info.security.mfp = Some(fidl_common::MfpFeature {
                supported: Some(true),
            });
            support_info
                .security
                .sae
                .as_mut()
                .unwrap()
                .hash_to_element_supported = Some(true);
            let mut sme_config = wlan_sme::client::ClientConfig::default();
            sme_config.wpa3_supported = true;
            let mut runtime = PinnedClientRuntime::new(
                device,
                runner,
                sme_config,
                runtime_device_info(),
                support_info.security,
                support_info.spectrum_management,
                fuchsia_inspect::Inspector::default(),
            )
            .await
            .unwrap();

            let error = runtime
                .connect(
                    live_shape_wpa3_connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap_err();
            let PinnedConnectError::Driver(PinnedDriverError::MlmeRequest { name, detail }) = error
            else {
                panic!("expected stage-specific MLME request failure: {error:?}")
            };
            assert_eq!(name, "Connect");
            assert!(detail.contains("IO_REFUSED"), "{detail}");
            assert!(!detail.contains("synthetic-password"));
            assert!(
                !runtime
                    .runner
                    .backend
                    .lock()
                    .unwrap()
                    .authorization
                    .is_live()
            );
        });
    }

    #[test]
    fn pinned_runtime_owns_open_association_authorization_and_timeout_containment() {
        futures::executor::block_on(async {
            let mut effects = FakeEffects::default();
            effects
                .rx
                .push_back(open_response(0x0b, &[0, 0, 2, 0, 0, 0]));
            effects.rx.push_back(source_exact_140_byte_open_response(
                0x01,
                &[1, 0, 0, 0, 42, 0, 1, 2, 0x82, 0x84],
            ));
            let capability = nic();
            let passive = Mt7921SoftmacAdapter::new(
                FakePassiveTransport::default(),
                capability,
                mt7921_port_spike::candidate_channels(capability),
                vec![channel(36)],
            )
            .unwrap();
            let mut device_support = support();
            device_support.query.sta_addr = nic().mac_address;
            device_support.query.factory_addr = nic().mac_address;
            device_support.query.mac_role = Some(fidl_common::WlanMacRole::Client);
            device_support.query.band_caps = Some(vec![fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_ieee80211::WlanBand::FiveGhz),
                basic_rates: Some(vec![0x82, 0x84]),
                primary_channels: Some(vec![channel(36)]),
                ..Default::default()
            }]);
            let (device, runner, mut ethernet, _tx) =
                Mt7921ClientDevice::new_with_ethernet(effects, passive, device_support, 8).unwrap();
            runner
                .backend
                .lock()
                .unwrap()
                .authorization
                .authorize_scan();
            let support_info = support();
            let inspector = fuchsia_inspect::Inspector::default();
            let mut runtime = PinnedClientRuntime::new(
                device,
                runner,
                wlan_sme::client::ClientConfig::default(),
                runtime_device_info(),
                support_info.security,
                support_info.spectrum_management,
                inspector,
            )
            .await
            .unwrap();
            runtime
                .connect(
                    open_connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert!(runtime.sme().status().is_connected());
            assert_eq!(
                runtime.runner.backend.lock().unwrap().effects.link_up,
                Some(true)
            );
            assert!(
                runtime
                    .runner
                    .backend
                    .lock()
                    .unwrap()
                    .effects
                    .association
                    .is_some()
            );

            {
                use crate::ethernet::AssociatedSoftmacTx;
                let mut pump = runtime.associated_data_pump();
                let mut ethernet_tx = vec![0x0a, 2, 3, 4, 5, 6];
                ethernet_tx.extend_from_slice(&nic().mac_address.unwrap());
                ethernet_tx.extend_from_slice(&[0x08, 0x00, 1, 2, 3, 4]);
                pump.transmit_ethernet(&ethernet_tx).unwrap();
            }
            let transmitted = runtime
                .runner
                .backend
                .lock()
                .unwrap()
                .effects
                .frame
                .clone()
                .unwrap();
            assert_eq!(
                u16::from_le_bytes([transmitted[0], transmitted[1]]) & 0x000c,
                0x0008
            );

            let mut data = vec![0x08, 0x02, 0, 0];
            data.extend_from_slice(&nic().mac_address.unwrap());
            data.extend_from_slice(&BSSID);
            data.extend_from_slice(&[0x0a, 2, 3, 4, 5, 6]);
            data.extend_from_slice(&[0, 0, 0xaa, 0xaa, 3, 0, 0, 0, 0x08, 0x00, 9, 8, 7]);
            runtime
                .runner
                .backend
                .lock()
                .unwrap()
                .effects
                .rx
                .push_back(ClientRxFrame {
                    bytes: data,
                    status: rx_status(-30),
                    security: None,
                });
            assert!(
                runtime
                    .runner
                    .pump_client_rx(&mut runtime.mlme)
                    .await
                    .unwrap()
            );
            use netstack3_port_spike::EthernetDevice as _;
            let delivered = ethernet.receive().expect("pinned MLME Ethernet delivery");
            assert_eq!(&delivered.as_bytes()[..6], &nic().mac_address.unwrap());
            assert_eq!(&delivered.as_bytes()[6..12], &[0x0a, 2, 3, 4, 5, 6]);

            runtime
                .runner
                .backend
                .lock()
                .unwrap()
                .effects
                .rx
                .push_back(open_response(0x0c, &3u16.to_le_bytes()));
            for _ in 0..4 {
                runtime.pump_once().await.unwrap();
            }
            assert!(!runtime.sme().status().is_connected());
            assert!(
                runtime
                    .runner
                    .backend
                    .lock()
                    .unwrap()
                    .effects
                    .clear
                    .is_some()
            );

            {
                let mut backend = runtime.runner.backend.lock().unwrap();
                backend
                    .effects
                    .rx
                    .push_back(open_response(0x0b, &[0, 0, 2, 0, 0, 0]));
                backend
                    .effects
                    .rx
                    .push_back(open_response(0x01, &[1, 0, 0, 0, 42, 0, 1, 2, 0x82, 0x84]));
            }
            runtime
                .connect(
                    open_connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert!(runtime.sme().status().is_connected());

            // A second runtime with no peer responses must contain lifecycle
            // authority at its caller-owned deadline.
            let capability = nic();
            let passive = Mt7921SoftmacAdapter::new(
                FakePassiveTransport::default(),
                capability,
                mt7921_port_spike::candidate_channels(capability),
                vec![channel(36)],
            )
            .unwrap();
            let (device, runner) =
                Mt7921ClientDevice::new(FakeEffects::default(), passive, support());
            runner
                .backend
                .lock()
                .unwrap()
                .authorization
                .authorize_scan();
            let support_info = support();
            let inspector = fuchsia_inspect::Inspector::default();
            let mut timed = PinnedClientRuntime::new(
                device,
                runner,
                wlan_sme::client::ClientConfig::default(),
                runtime_device_info(),
                support_info.security,
                support_info.spectrum_management,
                inspector,
            )
            .await
            .unwrap();
            assert_eq!(
                timed
                    .connect(open_connect_request(), std::time::Instant::now())
                    .await,
                Err(PinnedConnectError::Timeout)
            );
            assert!(!timed.runner.backend.lock().unwrap().authorization.is_live());
        });
    }

    #[test]
    fn pinned_runtime_quiesces_association_before_queued_eapol_rx() {
        futures::executor::block_on(async {
            let capability = nic();
            let make_runtime = |effects| async move {
                let passive = Mt7921SoftmacAdapter::new(
                    FakePassiveTransport::default(),
                    capability,
                    mt7921_port_spike::candidate_channels(capability),
                    vec![channel(36)],
                )
                .unwrap();
                let mut device_support = support();
                device_support.query.sta_addr = nic().mac_address;
                device_support.query.factory_addr = nic().mac_address;
                device_support.query.mac_role = Some(fidl_common::WlanMacRole::Client);
                device_support.query.band_caps =
                    Some(vec![fidl_softmac::WlanSoftmacBandCapability {
                        band: Some(fidl_ieee80211::WlanBand::FiveGhz),
                        basic_rates: Some(vec![0x82, 0x84]),
                        primary_channels: Some(vec![channel(36)]),
                        ..Default::default()
                    }]);
                let (device, runner) = Mt7921ClientDevice::new(effects, passive, device_support);
                runner
                    .backend
                    .lock()
                    .unwrap()
                    .authorization
                    .authorize_scan();
                let support_info = support();
                PinnedClientRuntime::new(
                    device,
                    runner,
                    wlan_sme::client::ClientConfig::default(),
                    runtime_device_info(),
                    support_info.security,
                    support_info.spectrum_management,
                    fuchsia_inspect::Inspector::default(),
                )
                .await
                .unwrap()
            };

            let mut effects = FakeEffects::default();
            effects.rx.extend([
                open_response(0x0b, &[0, 0, 2, 0, 0, 0]),
                wpa2_association_response(),
                wpa2_message_1(),
            ]);
            let mut runtime = make_runtime(effects).await;
            let _transaction = runtime.sme.on_connect_command(wpa2_connect_request());
            for _ in 0..16 {
                runtime.pump_once().await.unwrap();
                let backend = runtime.runner.backend.lock().unwrap();
                if backend.effects.rx_dequeued == 3 && backend.effects.frames.len() >= 3 {
                    break;
                }
            }
            let backend = runtime.runner.backend.lock().unwrap();
            assert_eq!(backend.effects.rx_dequeued, 3);
            assert!(
                backend.effects.association.is_some(),
                "effects={:?}",
                backend.effects
            );
            let association = backend
                .effects
                .order
                .iter()
                .position(|effect| *effect == "association")
                .unwrap();
            let third_rx = backend
                .effects
                .order
                .iter()
                .enumerate()
                .filter(|(_, effect)| **effect == "rx")
                .nth(2)
                .unwrap()
                .0;
            assert!(
                association < third_rx,
                "M1 dequeued before association activation"
            );
            assert!(backend.effects.frames.iter().any(|frame| {
                frame
                    .get(..2)
                    .is_some_and(|fc| u16::from_le_bytes([fc[0], fc[1]]) & 0x000c == 0x0008)
                    && frame
                        .windows(8)
                        .any(|window| window == [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e])
            }));
            drop(backend);

            let mut effects = FakeEffects {
                fail_on: Some("association"),
                ..Default::default()
            };
            effects.rx.extend([
                open_response(0x0b, &[0, 0, 2, 0, 0, 0]),
                wpa2_association_response(),
                wpa2_message_1(),
            ]);
            let mut failed = make_runtime(effects).await;
            let failure = failed
                .connect(
                    wpa2_connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await;
            assert!(
                failure.is_err(),
                "association activation failure must be terminal"
            );
            let backend = failed.runner.backend.lock().unwrap();
            assert_eq!(backend.effects.rx_dequeued, 2);
            assert_eq!(
                backend.effects.rx.len(),
                1,
                "queued M1 carrier was consumed"
            );
            assert!(!backend.effects.frames.iter().any(|frame| {
                frame
                    .windows(8)
                    .any(|window| window == [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e])
            }));
        });
    }

    #[test]
    fn pinned_runtime_drives_comeback_timer_and_disconnect_cancels_it() {
        futures::executor::block_on(async {
            let mut effects = FakeEffects::default();
            effects
                .rx
                .push_back(open_response(0x0b, &[0, 0, 2, 0, 0, 0]));
            effects.rx.push_back(open_response(
                0x01,
                &[1, 0, 30, 0, 1, 0, 56, 5, 3, 20, 0, 0, 0],
            ));
            let capability = nic();
            let passive = Mt7921SoftmacAdapter::new(
                FakePassiveTransport::default(),
                capability,
                mt7921_port_spike::candidate_channels(capability),
                vec![channel(36)],
            )
            .unwrap();
            let mut device_support = support();
            device_support.query.sta_addr = nic().mac_address;
            device_support.query.factory_addr = nic().mac_address;
            device_support.query.mac_role = Some(fidl_common::WlanMacRole::Client);
            device_support.query.band_caps = Some(vec![fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_ieee80211::WlanBand::FiveGhz),
                basic_rates: Some(vec![0x82, 0x84]),
                primary_channels: Some(vec![channel(36)]),
                ..Default::default()
            }]);
            let (device, runner) = Mt7921ClientDevice::new(effects, passive, device_support);
            runner
                .backend
                .lock()
                .unwrap()
                .authorization
                .authorize_scan();
            let backend = Arc::clone(&runner.backend);
            let support_info = support();
            let mut runtime = PinnedClientRuntime::new(
                device,
                runner,
                wlan_sme::client::ClientConfig::default(),
                runtime_device_info(),
                support_info.security,
                support_info.spectrum_management,
                fuchsia_inspect::Inspector::default(),
            )
            .await
            .unwrap();
            let responder = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
                loop {
                    {
                        let mut backend = backend.lock().unwrap();
                        if backend.effects.frames.len() >= 3 {
                            backend.effects.rx.push_back(open_response(
                                0x01,
                                &[1, 0, 0, 0, 0x2a, 0xc0, 1, 2, 0x82, 0x84],
                            ));
                            return;
                        }
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "association retry not emitted"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            });
            let started = std::time::Instant::now();
            runtime
                .connect(
                    open_connect_request(),
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
            responder.join().unwrap();
            assert!(started.elapsed() >= std::time::Duration::from_millis(15));
            let backend = runtime.runner.backend.lock().unwrap();
            assert_eq!(backend.effects.frames.len(), 3);
            assert_ne!(
                &backend.effects.frames[1][22..24],
                &backend.effects.frames[2][22..24]
            );
            assert_eq!(
                &backend.effects.frames[1][24..],
                &backend.effects.frames[2][24..]
            );
            assert_eq!(backend.effects.association.as_ref().unwrap().aid, Some(42));
            drop(backend);

            let mut effects = FakeEffects::default();
            effects
                .rx
                .push_back(open_response(0x0b, &[0, 0, 2, 0, 0, 0]));
            effects.rx.push_back(open_response(
                0x01,
                &[1, 0, 30, 0, 1, 0, 56, 5, 3, 20, 0, 0, 0],
            ));
            let passive = Mt7921SoftmacAdapter::new(
                FakePassiveTransport::default(),
                capability,
                mt7921_port_spike::candidate_channels(capability),
                vec![channel(36)],
            )
            .unwrap();
            let mut device_support = support();
            device_support.query.sta_addr = nic().mac_address;
            device_support.query.factory_addr = nic().mac_address;
            device_support.query.mac_role = Some(fidl_common::WlanMacRole::Client);
            device_support.query.band_caps = Some(vec![fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_ieee80211::WlanBand::FiveGhz),
                basic_rates: Some(vec![0x82, 0x84]),
                primary_channels: Some(vec![channel(36)]),
                ..Default::default()
            }]);
            let (device, runner) = Mt7921ClientDevice::new(effects, passive, device_support);
            runner
                .backend
                .lock()
                .unwrap()
                .authorization
                .authorize_scan();
            let support_info = support();
            let mut canceled = PinnedClientRuntime::new(
                device,
                runner,
                wlan_sme::client::ClientConfig::default(),
                runtime_device_info(),
                support_info.security,
                support_info.spectrum_management,
                fuchsia_inspect::Inspector::default(),
            )
            .await
            .unwrap();
            let _transaction = canceled.sme.on_connect_command(open_connect_request());
            for _ in 0..8 {
                canceled.pump_once().await.unwrap();
                if canceled.runner.backend.lock().unwrap().effects.frames.len() == 2 {
                    break;
                }
            }
            assert_eq!(
                canceled.runner.backend.lock().unwrap().effects.frames.len(),
                2
            );
            canceled.sme.on_disconnect_command(
                fidl_sme::UserDisconnectReason::WlanSmeUnitTesting,
                Default::default(),
            );
            for _ in 0..4 {
                canceled.pump_once().await.unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
            for _ in 0..4 {
                canceled.pump_once().await.unwrap();
            }
            assert_eq!(
                canceled.runner.backend.lock().unwrap().effects.frames.len(),
                2,
                "disconnect must cancel the generation-bound comeback timer"
            );
        });
    }

    #[test]
    fn closed_event_queue_error_redacts_sae_fields() {
        let mut device = Mt7921ClientDevice::new_offline_fake(FakeEffects::default(), support());
        drop(device.take_mlme_event_stream().unwrap());
        let sae_marker = vec![0xde, 0xad, 0xbe, 0xef];
        let error = device
            .send_mlme_event(fidl_mlme::MlmeEvent::OnSaeFrameRx {
                frame: fidl_mlme::SaeFrame {
                    peer_sta_address: BSSID,
                    status_code: fidl_ieee80211::StatusCode::Success,
                    seq_num: 1,
                    sae_fields: sae_marker,
                },
            })
            .unwrap_err();
        assert_eq!(format!("{error}"), "MLME event queue closed");
        assert_eq!(format!("{error:?}"), "MLME event queue closed");
    }

    #[test]
    fn device_ops_composes_existing_passive_mechanics_for_scan_and_cancel() {
        futures::executor::block_on(async {
            let transport = FakePassiveTransport::default();
            let calls = Arc::clone(&transport.0);
            let capability = nic();
            let passive = Mt7921SoftmacAdapter::new(
                transport,
                capability,
                mt7921_port_spike::candidate_channels(capability),
                vec![channel(36)],
            )
            .unwrap();
            let mut support = support();
            support.discovery.scan_offload = Some(fidl_softmac::ScanOffloadExtension {
                supported: Some(true),
                scan_cancel_supported: Some(true),
            });
            let (mut device, _runner) =
                Mt7921ClientDevice::new(FakeEffects::default(), passive, support);

            assert_eq!(
                device
                    .set_channel(
                        channel(40),
                        fidl_ieee80211::ChannelBandwidth::Cbw20,
                        channel(0),
                    )
                    .await,
                Err(zx::Status::IO)
            );
            assert!(device.backend().effects.order.is_empty());
            device
                .set_channel(
                    channel(36),
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    channel(0),
                )
                .await
                .unwrap();
            let response = device
                .start_passive_scan(&fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest {
                    channels: Some(vec![channel(36)]),
                    min_channel_time: Some(10),
                    max_channel_time: Some(20),
                    min_home_time: Some(0),
                })
                .await
                .unwrap();
            assert_eq!(response.scan_id, Some(1));
            device
                .cancel_scan(&fidl_softmac::WlanSoftmacBaseCancelScanRequest {
                    scan_id: response.scan_id,
                })
                .await
                .unwrap();

            assert!(matches!(
                calls.lock().unwrap().as_slice(),
                [
                    PassiveCall::Channel(_),
                    PassiveCall::Start(crate::PassiveScanCommand { scan_id: 1, .. }),
                    PassiveCall::Cancel(1),
                ]
            ));
            assert_eq!(device.backend().effects.order, ["channel"]);
        });
    }

    #[test]
    fn all_unretained_operations_are_unsupported() {
        futures::executor::block_on(async {
            let mut device =
                Mt7921ClientDevice::new_offline_fake(FakeEffects::default(), support());
            assert_eq!(
                device.deliver_eth_frame(&[1, 2]),
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.set_mac_address([0; 6]).await,
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.start_passive_scan(&Default::default()).await,
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.start_active_scan(&Default::default()).await,
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.cancel_scan(&Default::default()).await,
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.disable_beaconing().await,
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.update_wmm_parameters(&Default::default()).await,
                Err(zx::Status::NOT_SUPPORTED)
            );
        });
    }

    #[test]
    fn association_state_orders_wcid_keys_port_and_zero_state_teardown() {
        let mut state = Mt7921AssociationState::default();
        state
            .program(
                &fidl_softmac::WlanAssociationConfig {
                    bssid: Some(BSSID),
                    aid: Some(42),
                    ..Default::default()
                },
                7,
                true,
            )
            .unwrap();
        assert_eq!(state.wcid(), Some(7));
        assert_eq!(state.set_link_up(), Err(zx::Status::BAD_STATE));

        let key = |key_type, peer_addr, key_idx| fidl_softmac::WlanKeyConfiguration {
            protection: Some(fidl_softmac::WlanProtection::RxTx),
            cipher_oui: Some([0, 15, 172]),
            cipher_type: Some(4),
            key_type: Some(key_type),
            peer_addr: Some(peer_addr),
            key_idx: Some(key_idx),
            key: Some(FAKE_KEY.to_vec()),
            rsc: Some(0),
        };
        state
            .key_installed(&key(fidl_ieee80211::KeyType::Pairwise, BSSID, 0))
            .unwrap();
        assert_eq!(state.set_link_up(), Err(zx::Status::BAD_STATE));
        state
            .key_installed(&key(fidl_ieee80211::KeyType::Group, [0xff; 6], 1))
            .unwrap();
        assert_eq!(state.set_link_up(), Err(zx::Status::BAD_STATE));
        state
            .key_installed(&key(fidl_ieee80211::KeyType::Igtk, [0xff; 6], 4))
            .unwrap();
        state.set_link_up().unwrap();

        assert_eq!(
            state.clear(),
            Some(AssociationTeardown {
                peer: BSSID,
                wcid: 7,
                close_link: true,
                remove_pairwise_key: true,
                remove_group_key: true,
                remove_integrity_group_key: true,
            })
        );
        assert_eq!(state.wcid(), None);
        assert_eq!(
            state.key_installed(&key(fidl_ieee80211::KeyType::Pairwise, BSSID, 0)),
            Err(zx::Status::BAD_STATE)
        );
    }
}

#[cfg(test)]
#[path = "production_boundary_test.rs"]
mod production_boundary_test;
