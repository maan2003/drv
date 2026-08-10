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
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use futures::channel::mpsc;
#[cfg(test)]
use std::sync::MutexGuard;
use std::sync::{Arc, Mutex};
use wlan_mlme::device::{DeviceOps, LinkStatus};

use crate::Mt7921SoftmacAdapter;
use crate::ethernet::{
    EthernetIngressError, EthernetPortConfigError, MlmeEthernetSink, Mt7921EthernetDevice,
    Mt7921EthernetTx, ethernet_port,
};
use fuchsia_softmac_port::{HardwareScanEvent, SoftmacHardware};

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
        let peer = configuration.bssid.ok_or(zx::Status::INVALID_ARGS)?;
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

    /// Immediately and durably poison every shared TX handle for lifecycle.
    fn revoke_lifecycle(&mut self);

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
    ) -> Result<(), zx::Status>;
    fn install_key(
        &mut self,
        configuration: &fidl_softmac::WlanKeyConfiguration,
    ) -> Result<(), zx::Status>;
    fn notify_association_complete(
        &mut self,
        configuration: &fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status>;
    fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status>;
    fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status>;
    fn next_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status>;

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

trait Mt7921ClientScan {
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
            fidl_softmac::WlanSoftmacBaseSetChannelRequest {
                primary: Some(primary),
                bandwidth: Some(bandwidth),
                vht_secondary_80_channel: Some(secondary),
            },
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
                backend.revoked = true;
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
                        backend.revoked = true;
                        backend.effects.revoke_scan();
                        return Err(zx::Status::BAD_STATE);
                    };
                    if let Err(status) = backend.effects.observe_passive_scan(scan_id, observation)
                    {
                        backend.revoked = true;
                        backend.effects.revoke_scan();
                        backend.active_scan_id = None;
                        let _ = backend.effects.complete_passive_scan(scan_id, false);
                        return Err(status);
                    }
                }
                HardwareScanEvent::Complete { scan_id, success } => {
                    if backend.active_scan_id != Some(*scan_id) {
                        backend.revoked = true;
                        backend.effects.revoke_scan();
                        return Err(zx::Status::BAD_STATE);
                    }
                    if !success {
                        backend.revoked = true;
                        backend.effects.revoke_scan();
                    }
                    if let Err(status) = backend.effects.complete_passive_scan(*scan_id, *success) {
                        backend.revoked = true;
                        backend.effects.revoke_scan();
                        backend.active_scan_id = None;
                        return Err(status);
                    }
                    backend.active_scan_id = None;
                    backend.revoked = !success || backend.lifecycle_poisoned;
                    if backend.revoked {
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
        let frame = self.backend.lock().unwrap().effects.next_rx()?;
        let Some(ClientRxFrame { bytes, status }) = frame else {
            return Ok(false);
        };
        wlan_mlme::MlmeImpl::handle_mac_frame_rx(mlme, &bytes, status, fuchsia_trace::Id::new())
            .await;
        Ok(true)
    }

    /// Notify the supplied backend that its hardware reset completed and
    /// abandon any in-process scan transaction.
    pub fn reset(&self) -> Result<(), zx::Status> {
        let mut backend = self.backend.lock().unwrap();
        backend.revoked = true;
        backend.lifecycle_poisoned = true;
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
        backend.revoked = true;
        backend.lifecycle_poisoned = true;
        backend.effects.revoke_lifecycle();
        backend.active_scan_id = None;
        if let Some(ethernet) = backend.ethernet.as_mut() {
            ethernet.teardown();
        }
        backend.effects.stop()
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
    revoked: bool,
    lifecycle_poisoned: bool,
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
                revoked: false,
                lifecycle_poisoned: false,
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
        effects.revoke_scan();
        let backend = Arc::new(Mutex::new(ComposedBackend {
            effects,
            scan,
            active_scan_id: None,
            revoked: true,
            lifecycle_poisoned: false,
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
        let (ethernet_device, ethernet_tx, ethernet_sink) = ethernet_port(mac, queue_capacity)?;
        effects.revoke_scan();
        let backend = Arc::new(Mutex::new(ComposedBackend {
            effects,
            scan,
            active_scan_id: None,
            revoked: true,
            lifecycle_poisoned: false,
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

impl<E: Mt7921ClientEffects, S> Mt7921ClientDevice<E, S> {
    /// Pop exactly one frame/status pair from the injected RX effect queue.
    pub fn next_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
        self.backend.lock().unwrap().effects.next_rx()
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
        if backend.revoked {
            return Err(zx::Status::ACCESS_DENIED);
        }
        backend.effects.send_wlan_frame(&buffer, tx_flags)
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
        let tuned = backend
            .scan
            .set_channel(primary, bandwidth, vht_secondary_80_channel);
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
        backend.revoked = true;
        backend.effects.revoke_scan();
        let response = backend.scan.start_passive_scan(request.clone())?;
        let scan_id = response.scan_id.ok_or(zx::Status::IO_INVALID)?;
        backend.active_scan_id = Some(scan_id);
        if let Err(status) = backend
            .effects
            .begin_passive_scan(scan_id, request.channels.as_deref().unwrap_or_default())
        {
            backend.revoked = true;
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
        self.backend
            .lock()
            .unwrap()
            .effects
            .install_key(configuration)
    }

    async fn notify_association_complete(
        &mut self,
        configuration: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        self.backend
            .lock()
            .unwrap()
            .effects
            .notify_association_complete(&configuration)
    }

    async fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        self.backend
            .lock()
            .unwrap()
            .effects
            .clear_association(request)
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
        flags: Option<fidl_softmac::WlanTxInfoFlags>,
        key: Option<fidl_softmac::WlanKeyConfiguration>,
        association: Option<fidl_softmac::WlanAssociationConfig>,
        clear: Option<fidl_softmac::WlanSoftmacBaseClearAssociationRequest>,
        link_up: Option<bool>,
        rx: VecDeque<ClientRxFrame>,
        fail_on: Option<&'static str>,
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
        ) -> Result<(), zx::Status> {
            self.hit("frame")?;
            self.frame = Some(bytes.to_vec());
            self.flags = Some(flags);
            Ok(())
        }

        fn install_key(
            &mut self,
            configuration: &fidl_softmac::WlanKeyConfiguration,
        ) -> Result<(), zx::Status> {
            self.hit("key")?;
            self.key = Some(configuration.clone());
            Ok(())
        }

        fn notify_association_complete(
            &mut self,
            configuration: &fidl_softmac::WlanAssociationConfig,
        ) -> Result<(), zx::Status> {
            self.hit("association")?;
            self.association = Some(configuration.clone());
            Ok(())
        }

        fn clear_association(
            &mut self,
            request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
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

        fn next_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
            if self.fail_on == Some("rx") {
                return Err(zx::Status::IO_REFUSED);
            }
            Ok(self.rx.pop_front())
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
