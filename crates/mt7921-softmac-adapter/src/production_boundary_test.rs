// SPDX-License-Identifier: GPL-2.0-only

use super::*;
use async_trait::async_trait;
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_sme as fidl_sme;
use fuchsia_softmac_port::HardwareScanEvent;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::lock::Mutex as AsyncMutex;
use mt7921_port_spike::{CandidateChannel, NicCapability, NicPhyCapability, PhysicalBand};
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use wlan_common::sequestered::Sequestered;
use wlan_mlme::{MlmeImpl, client::ClientMlme};
use wlan_sme::client::{ClientConfig, ClientSme};
use wlan_sme::{MlmeRequest, Station};
use wlancfg_selection::client::connection_selection::{ConnectionSelector, ConnectionSelectorApi};
use wlancfg_selection::client::scan::{ScanReason, ScanRequestApi, selection_scan_results};
use wlancfg_selection::client::types::{self, Bss, ScanObservation, ScanResult};
use wlancfg_selection::config_management::{
    Credential, NetworkConfig, NetworkConfigError, PastConnectionData, PastConnectionList,
    SavedNetworksManagerApi,
};
use wlancfg_selection::telemetry::{TelemetryEvent, TelemetrySender};
use wlancfg_selection::wlan_metrics_registry::PolicyConnectionAttemptMigratedMetricDimensionReason as ConnectReason;

const CLIENT: [u8; 6] = [7; 6];
const AP: [u8; 6] = [6; 6];
const SSID: &[u8] = b"test";
const PASSPHRASE: &[u8] = b"password";

fn channel(number: u8) -> fidl_ieee80211::ChannelNumber {
    fidl_ieee80211::ChannelNumber {
        band: fidl_ieee80211::WlanBand::TwoGhz,
        number,
    }
}

fn wpa3_ies() -> Vec<u8> {
    vec![
        0, 4, b't', b'e', b's', b't', 1, 4, 0x82, 0x84, 0x8b, 0x96, 48, 20, 1, 0, 0, 0x0f, 0xac, 4,
        1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 8, 0xcc, 0,
    ]
}

struct PhysicalScan {
    events: VecDeque<crate::TransportEvent>,
}

impl crate::Mt7921PassiveTransport for PhysicalScan {
    type Error = Infallible;

    fn set_channel(&mut self, _: CandidateChannel) -> Result<(), Self::Error> {
        Ok(())
    }
    fn start_passive_scan(&mut self, _: crate::PassiveScanCommand) -> Result<(), Self::Error> {
        Ok(())
    }
    fn cancel_passive_scan(&mut self, _: u64) -> Result<(), Self::Error> {
        Ok(())
    }
    fn next_event(&mut self) -> Result<Option<crate::TransportEvent>, Self::Error> {
        Ok(self.events.pop_front())
    }
}

fn physical_adapter() -> crate::Mt7921SoftmacAdapter<PhysicalScan> {
    let candidate = CandidateChannel {
        band: PhysicalBand::Ghz2,
        number: 6,
        frequency_mhz: 2437,
    };
    let nic = NicCapability {
        element_count: 2,
        mac_address: Some(CLIENT),
        phy: Some(NicPhyCapability {
            ht: true,
            vht: true,
            has_5ghz: false,
            max_bandwidth: 0,
            spatial_streams: 1,
            hardware_path: 1,
            he: true,
        }),
        has_6ghz: Some(false),
        chip_capability: None,
        unknown_elements: 0,
    };
    crate::Mt7921SoftmacAdapter::new(
        PhysicalScan {
            events: [
                crate::TransportEvent::Advertisement(crate::RawAdvertisement {
                    scan_id: 1,
                    kind: fuchsia_softmac_port::AdvertisementKind::Beacon,
                    timestamp_nanos: 10,
                    bssid: AP,
                    beacon_interval_tu: 100,
                    capability_info: 0x11,
                    ies: wpa3_ies(),
                    channel: candidate,
                    rssi_dbm: -40,
                }),
                crate::TransportEvent::Complete {
                    scan_id: 1,
                    success: true,
                },
            ]
            .into(),
        },
        nic,
        vec![candidate],
        vec![channel(6)],
    )
    .unwrap()
}

struct ScanSource {
    result: AsyncMutex<Option<Vec<ScanResult>>>,
}

#[async_trait(?Send)]
impl ScanRequestApi for ScanSource {
    async fn perform_scan(
        &self,
        reason: ScanReason,
        _: Vec<types::Ssid>,
        _: Vec<types::WlanChan>,
    ) -> Result<Vec<ScanResult>, types::ScanError> {
        assert_eq!(reason, ScanReason::BssSelection);
        self.result
            .lock()
            .await
            .take()
            .ok_or(wlancfg_selection::fidl_fuchsia_wlan_policy::ScanErrorCode::GeneralError)
    }
}

struct SavedNetwork(NetworkConfig);

#[async_trait(?Send)]
impl SavedNetworksManagerApi for SavedNetwork {
    async fn remove(&self, _: types::NetworkIdentifier) -> Result<bool, NetworkConfigError> {
        panic!("unused saved-network effect")
    }
    async fn known_network_count(&self) -> usize {
        1
    }
    async fn lookup(&self, id: &types::NetworkIdentifier) -> Option<NetworkConfig> {
        (self.0.ssid == id.ssid && self.0.security_type == id.security_type).then(|| self.0.clone())
    }
    async fn lookup_compatible(
        &self,
        ssid: &types::Ssid,
        _: fidl_sme::Protection,
    ) -> Vec<NetworkConfig> {
        if self.0.ssid == *ssid {
            vec![self.0.clone()]
        } else {
            vec![]
        }
    }
    async fn store(
        &self,
        _: types::NetworkIdentifier,
        _: Credential,
    ) -> Result<Option<NetworkConfig>, NetworkConfigError> {
        panic!("unused saved-network effect")
    }
    async fn record_connect_result(
        &self,
        _: types::NetworkIdentifier,
        _: &Credential,
        _: types::Bssid,
        _: fidl_sme::ConnectResult,
        _: ScanObservation,
    ) {
        panic!("unused saved-network effect")
    }
    async fn record_disconnect(
        &self,
        _: &types::NetworkIdentifier,
        _: &Credential,
        _: PastConnectionData,
    ) {
        panic!("unused saved-network effect")
    }
    async fn record_periodic_metrics(&self) {
        panic!("unused saved-network effect")
    }
    async fn record_scan_result(
        &self,
        _: Vec<types::Ssid>,
        _: &HashMap<types::NetworkIdentifierDetailed, Vec<Bss>>,
    ) {
        panic!("unused saved-network effect")
    }
    async fn is_network_single_bss(
        &self,
        _: &types::NetworkIdentifier,
        _: &Credential,
    ) -> Result<bool, anyhow::Error> {
        Ok(true)
    }
    async fn get_networks(&self) -> Vec<NetworkConfig> {
        vec![self.0.clone()]
    }
    async fn get_past_connections(
        &self,
        _: &types::NetworkIdentifier,
        _: &Credential,
        _: &types::Bssid,
    ) -> PastConnectionList {
        PastConnectionList::new(1)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Applied {
    Power(i8),
    Rate(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Reject {
    AlreadyTransmitted,
    Revoked,
}

#[derive(Default)]
struct BackendState {
    scan_poisoned: bool,
    lifecycle_poisoned: bool,
    active_scan_id: Option<u64>,
    pending_scan_channel: Option<fidl_ieee80211::ChannelNumber>,
    regulatory_channel: Option<fidl_ieee80211::ChannelNumber>,
    current_channel: Option<fidl_ieee80211::ChannelNumber>,
    regulatory_max_dbm: Option<i8>,
    sar_cap_dbm: Option<i8>,
    programmed_power_dbm: Option<i8>,
    authorized_rate_mbps: Option<u16>,
    programmed_rate_mbps: Option<u16>,
    applied: Vec<Applied>,
    frames: Vec<Vec<u8>>,
    rejects: Vec<Reject>,
    fail_reset: bool,
    fail_stop: bool,
}

struct ProductionBackend(Arc<Mutex<BackendState>>);

#[derive(Clone)]
struct BackendProgrammer(Arc<Mutex<BackendState>>);

impl BackendProgrammer {
    fn set_regulatory_max(&self, power_dbm: i8) {
        let mut state = self.0.lock().unwrap();
        state.regulatory_max_dbm = Some(power_dbm);
    }

    fn set_sar_cap(&self, power_dbm: i8) {
        self.0.lock().unwrap().sar_cap_dbm = Some(power_dbm);
    }

    fn complete_power_programming(&self, power_dbm: i8) {
        let mut state = self.0.lock().unwrap();
        state.programmed_power_dbm = Some(power_dbm);
        state.applied.push(Applied::Power(power_dbm));
    }

    fn complete_rate_programming(&self, rate_mbps: u16) {
        let mut state = self.0.lock().unwrap();
        state.programmed_rate_mbps = Some(rate_mbps);
        state.applied.push(Applied::Rate(rate_mbps));
    }

    fn authorize_rate(&self, rate_mbps: u16) {
        self.0.lock().unwrap().authorized_rate_mbps = Some(rate_mbps);
    }
}

impl Mt7921ClientEffects for ProductionBackend {
    fn revoke_scan(&mut self) {
        self.0.lock().unwrap().scan_poisoned = true;
    }

    fn revoke_lifecycle(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.lifecycle_poisoned = true;
        state.scan_poisoned = true;
        state.regulatory_channel = None;
        state.current_channel = None;
        state.regulatory_max_dbm = None;
        state.sar_cap_dbm = None;
        state.programmed_power_dbm = None;
        state.authorized_rate_mbps = None;
        state.programmed_rate_mbps = None;
    }

    fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        _: fidl_ieee80211::ChannelBandwidth,
        _: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        self.0.lock().unwrap().current_channel = Some(primary);
        Ok(())
    }
    fn join_bss(&mut self, _: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        Ok(())
    }
    fn send_wlan_frame(
        &mut self,
        bytes: &[u8],
        _: fidl_softmac::WlanTxInfoFlags,
        _: &mut dyn crate::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        let mut state = self.0.lock().unwrap();
        let power_authorized = state.programmed_power_dbm.is_some_and(|power| {
            state.regulatory_max_dbm.is_some_and(|limit| power <= limit)
                && state.sar_cap_dbm.is_some_and(|limit| power <= limit)
        });
        if state.scan_poisoned
            || state.lifecycle_poisoned
            || state.current_channel != Some(channel(6))
            || state.regulatory_channel != state.current_channel
            || !power_authorized
            || state.programmed_rate_mbps != state.authorized_rate_mbps
            || !state.authorized_rate_mbps.is_some_and(|rate| rate > 0)
        {
            state.rejects.push(Reject::Revoked);
            return Err(zx::Status::ACCESS_DENIED);
        }
        if !state.frames.is_empty() {
            state.rejects.push(Reject::AlreadyTransmitted);
            return Err(zx::Status::ALREADY_EXISTS);
        }
        state.frames.push(bytes.to_vec());
        Ok(())
    }
    fn install_key(
        &mut self,
        _: &fidl_softmac::WlanKeyConfiguration,
        _: &mut dyn crate::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn notify_association_complete(
        &mut self,
        _: &fidl_softmac::WlanAssociationConfig,
        _: &mut dyn crate::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn clear_association(
        &mut self,
        _: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
        _: &mut dyn crate::client_device::Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn set_link_up(&mut self, _: bool) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn next_rx(
        &mut self,
        _: &mut dyn crate::client_device::Mt7921ClientIo,
    ) -> Result<Option<ClientRxFrame>, zx::Status> {
        Ok(None)
    }
    fn begin_passive_scan(
        &mut self,
        scan_id: u64,
        _: &[fidl_ieee80211::ChannelNumber],
    ) -> Result<(), zx::Status> {
        self.0.lock().unwrap().active_scan_id = Some(scan_id);
        Ok(())
    }
    fn observe_passive_scan(
        &mut self,
        scan_id: u64,
        observation: &fuchsia_softmac_port::ScanObservation,
    ) -> Result<(), zx::Status> {
        let mut state = self.0.lock().unwrap();
        if state.active_scan_id != Some(scan_id)
            || observation.kind != fuchsia_softmac_port::AdvertisementKind::Beacon
        {
            return Err(zx::Status::ACCESS_DENIED);
        }
        state.pending_scan_channel = Some(observation.bss.primary);
        Ok(())
    }
    fn complete_passive_scan(&mut self, scan_id: u64, success: bool) -> Result<(), zx::Status> {
        let mut state = self.0.lock().unwrap();
        state.regulatory_channel =
            if state.active_scan_id == Some(scan_id) && success && !state.lifecycle_poisoned {
                state.pending_scan_channel
            } else {
                None
            };
        state.active_scan_id = None;
        state.pending_scan_channel = None;
        if success && !state.lifecycle_poisoned {
            state.scan_poisoned = false;
        }
        Ok(())
    }
    fn reset(&mut self) -> Result<(), zx::Status> {
        let mut state = self.0.lock().unwrap();
        if state.fail_reset {
            return Err(zx::Status::IO);
        }
        state.active_scan_id = None;
        Ok(())
    }
    fn stop(&mut self) -> Result<(), zx::Status> {
        if self.0.lock().unwrap().fail_stop {
            return Err(zx::Status::IO);
        }
        self.reset()
    }
}

fn client_support() -> ClientSupport {
    ClientSupport {
        query: fidl_softmac::WlanSoftmacQueryResponse {
            sta_addr: Some(CLIENT),
            factory_addr: Some(CLIENT),
            mac_role: Some(fidl_common::WlanMacRole::Client),
            hardware_capability: Some(0),
            band_caps: Some(vec![fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_ieee80211::WlanBand::TwoGhz),
                basic_rates: Some(vec![0x82, 0x84, 0x8b, 0x96]),
                primary_channels: Some(vec![channel(6)]),
                ..Default::default()
            }]),
            ..Default::default()
        },
        discovery: fidl_softmac::DiscoverySupport {
            scan_offload: Some(fidl_softmac::ScanOffloadExtension {
                supported: Some(true),
                scan_cancel_supported: Some(true),
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
            mfp: Some(fidl_common::MfpFeature {
                supported: Some(true),
            }),
            sae: Some(fidl_common::SaeFeature {
                driver_handler_supported: Some(false),
                sme_handler_supported: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        spectrum_management: Default::default(),
        association: None,
    }
}

fn device_info() -> fidl_mlme::DeviceInfo {
    fidl_mlme::DeviceInfo {
        sta_addr: CLIENT,
        factory_addr: CLIENT,
        role: fidl_common::WlanMacRole::Client,
        bands: vec![fidl_mlme::BandCapability {
            band: fidl_ieee80211::WlanBand::TwoGhz,
            basic_rates: vec![0x82, 0x84, 0x8b, 0x96],
            ht_cap: None,
            vht_cap: None,
            primary_channels: vec![channel(6)],
        }],
        softmac_hardware_capability: 0,
        qos_capable: false,
    }
}

#[test]
fn passive_physical_selection_reaches_one_authorized_production_sae_tx() {
    futures::executor::block_on(async {
        let state = Arc::new(Mutex::new(BackendState::default()));
        let programmer = BackendProgrammer(state.clone());
        let backend = ProductionBackend(state.clone());
        let (mut device, runner) =
            Mt7921ClientDevice::new(backend, physical_adapter(), client_support());
        let mut events = device.take_mlme_event_stream().unwrap();
        let (timer, _timer_stream) = wlan_mlme::common::timer::create_timer();
        let mut mlme = ClientMlme::new(Default::default(), device, timer)
            .await
            .unwrap();

        let inspector = fuchsia_inspect::Inspector::default();
        let mut config = ClientConfig::default();
        config.wpa3_supported = true;
        let (mut sme, _sink, mut requests, _timers) = ClientSme::new(
            config,
            device_info(),
            inspector.clone(),
            inspector.root().create_child("sme"),
            client_support().security,
            Default::default(),
        );
        let mut scanned = sme.on_scan_command(fidl_sme::ScanRequest::Passive(
            fidl_sme::PassiveScanRequest { channels: vec![6] },
        ));
        let scan_request = requests.try_recv().expect("SME passive scan request");
        let txn_id = match &scan_request {
            MlmeRequest::Scan(request) => request.txn_id,
            other => panic!("expected scan, got {}", other.name()),
        };
        mlme.handle_mlme_request(scan_request).await.unwrap();

        let observation = match runner.poll().unwrap() {
            Some(HardwareScanEvent::Observation(observation)) => observation,
            other => panic!("expected physical observation, got {other:?}"),
        };
        Station::on_mlme_event(
            &mut sme,
            fidl_mlme::MlmeEvent::OnScanResult {
                result: fidl_mlme::ScanResult {
                    txn_id,
                    timestamp_nanos: observation.timestamp_nanos,
                    bss: observation.bss,
                },
            },
        );
        let (scan_id, success) = match runner.poll().unwrap() {
            Some(HardwareScanEvent::Complete { scan_id, success }) => (scan_id, success),
            other => panic!("expected matching physical completion, got {other:?}"),
        };
        assert!(success);
        mlme.handle_scan_complete(zx::Status::OK, scan_id).await;
        Station::on_mlme_event(
            &mut sme,
            events.next().await.expect("ClientMlme scan completion"),
        );
        let sme_results = scanned.try_recv().unwrap().unwrap().unwrap();
        let converted = selection_scan_results(sme_results, &[]);
        assert_eq!(converted.len(), 1);
        assert_eq!(
            converted[0].security_type_detailed,
            fidl_sme::Protection::Wpa3Personal
        );
        assert_eq!(
            converted[0].entries[0].observation,
            ScanObservation::Passive
        );

        let scan = Arc::new(ScanSource {
            result: AsyncMutex::new(Some(converted)),
        });
        let network = types::NetworkIdentifier::new(
            types::Ssid::from_bytes_unchecked(SSID.to_vec()),
            types::SecurityType::Wpa3,
        );
        let saved = Arc::new(SavedNetwork(
            NetworkConfig::new(
                network.clone(),
                Credential::Password(PASSPHRASE.to_vec()),
                true,
                Some(0.0),
            )
            .unwrap(),
        ));
        let inspector = fuchsia_inspect::Inspector::default();
        let (telemetry, _) = mpsc::channel::<TelemetryEvent>(8);
        let selector = ConnectionSelector::new(
            saved,
            scan,
            inspector.root().create_child("selection"),
            TelemetrySender::new(telemetry),
        );
        let selected = selector
            .find_and_select_connection_candidate(Some(network), ConnectReason::FidlConnectRequest)
            .await
            .expect("pinned selector must select the physical WPA3 BSS");

        programmer.set_regulatory_max(18);
        programmer.set_sar_cap(16);
        programmer.complete_power_programming(16);
        programmer.authorize_rate(6);
        programmer.complete_rate_programming(6);
        let _transaction = sme.on_connect_command(fidl_sme::ConnectRequest {
            ssid: SSID.to_vec(),
            bss_description: Sequestered::release(selected.bss.bss_description),
            multiple_bss_candidates: selected.network_has_multiple_bss,
            authentication: selected.authenticator.into(),
            deprecated_scan_type: fidl_common::ScanType::Passive,
        });
        let connect = match requests.try_recv().expect("SME connect request") {
            MlmeRequest::Connect(connect) => connect,
            other => panic!("expected connect, got {}", other.name()),
        };
        assert_eq!(connect.auth_type, fidl_mlme::AuthenticationTypes::Sae);
        mlme.handle_mlme_request(MlmeRequest::Connect(connect))
            .await
            .unwrap();
        let handshake = events.next().await.expect("MLME SAE handshake indication");
        assert!(matches!(
            handshake,
            fidl_mlme::MlmeEvent::OnSaeHandshakeInd { .. }
        ));
        Station::on_mlme_event(&mut sme, handshake);
        let sae_tx = match requests.try_recv().expect("ordinary SME SAE frame") {
            MlmeRequest::SaeFrameTx(frame) => frame,
            other => panic!("expected SAE frame, got {}", other.name()),
        };
        mlme.handle_mlme_request(MlmeRequest::SaeFrameTx(sae_tx))
            .await
            .unwrap();

        {
            let state = state.lock().unwrap();
            assert_eq!(state.applied, [Applied::Power(16), Applied::Rate(6)]);
            assert_eq!(state.frames.len(), 1);
            let frame = &state.frames[0];
            assert_eq!(&frame[0..2], &[0xb0, 0]);
            assert_eq!(&frame[4..10], &AP);
            assert_eq!(&frame[10..16], &CLIENT);
            assert_eq!(&frame[16..22], &AP);
            assert_eq!(&frame[24..28], &[3, 0, 1, 0]);
        }

        runner.reset().unwrap();
        let state = state.lock().unwrap();
        assert!(state.lifecycle_poisoned);
        assert!(state.regulatory_max_dbm.is_none());
        assert!(state.sar_cap_dbm.is_none());
        assert!(state.programmed_power_dbm.is_none());
        assert!(state.authorized_rate_mbps.is_none());
        assert!(state.programmed_rate_mbps.is_none());
    });
}

#[test]
fn production_tx_requires_independent_power_and_rate_authorization() {
    futures::executor::block_on(async {
        let state = Arc::new(Mutex::new(BackendState {
            regulatory_channel: Some(channel(6)),
            regulatory_max_dbm: Some(18),
            sar_cap_dbm: Some(16),
            programmed_power_dbm: Some(16),
            authorized_rate_mbps: Some(6),
            ..Default::default()
        }));
        let programmer = BackendProgrammer(state.clone());
        let mut device = Mt7921ClientDevice::new_offline_fake(
            ProductionBackend(state.clone()),
            client_support(),
        );
        device
            .set_channel(
                channel(6),
                fidl_ieee80211::ChannelBandwidth::Cbw20,
                channel(0),
            )
            .await
            .unwrap();

        assert_eq!(
            device.send_wlan_frame(vec![1].into(), fidl_softmac::WlanTxInfoFlags::empty(), None),
            Err(zx::Status::ACCESS_DENIED)
        );
        programmer.complete_rate_programming(0);
        assert_eq!(
            device.send_wlan_frame(vec![2].into(), fidl_softmac::WlanTxInfoFlags::empty(), None),
            Err(zx::Status::ACCESS_DENIED)
        );
        programmer.complete_rate_programming(12);
        assert_eq!(
            device.send_wlan_frame(vec![3].into(), fidl_softmac::WlanTxInfoFlags::empty(), None),
            Err(zx::Status::ACCESS_DENIED)
        );
        programmer.complete_rate_programming(6);
        programmer.complete_power_programming(17);
        assert_eq!(
            device.send_wlan_frame(vec![4].into(), fidl_softmac::WlanTxInfoFlags::empty(), None),
            Err(zx::Status::ACCESS_DENIED)
        );
        programmer.complete_power_programming(16);
        device
            .send_wlan_frame(vec![5].into(), fidl_softmac::WlanTxInfoFlags::empty(), None)
            .unwrap();
        assert_eq!(
            device.send_wlan_frame(vec![6].into(), fidl_softmac::WlanTxInfoFlags::empty(), None),
            Err(zx::Status::ALREADY_EXISTS)
        );

        let state = state.lock().unwrap();
        assert_eq!(state.frames, [vec![5]]);
        assert_eq!(
            state.rejects,
            [
                Reject::Revoked,
                Reject::Revoked,
                Reject::Revoked,
                Reject::Revoked,
                Reject::AlreadyTransmitted
            ]
        );
    });
}

#[test]
fn reset_failure_revokes_before_first_frame_and_survives_later_scan() {
    futures::executor::block_on(async {
        let state = Arc::new(Mutex::new(BackendState {
            regulatory_channel: Some(channel(6)),
            current_channel: Some(channel(6)),
            regulatory_max_dbm: Some(18),
            sar_cap_dbm: Some(16),
            programmed_power_dbm: Some(16),
            authorized_rate_mbps: Some(6),
            programmed_rate_mbps: Some(6),
            fail_reset: true,
            ..Default::default()
        }));
        let (mut device, runner) = Mt7921ClientDevice::new(
            ProductionBackend(state.clone()),
            physical_adapter(),
            client_support(),
        );

        assert!(state.lock().unwrap().frames.is_empty());
        assert_eq!(runner.reset(), Err(zx::Status::IO));
        let request = fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest {
            channels: Some(vec![channel(6)]),
            min_channel_time: Some(10),
            max_channel_time: Some(20),
            min_home_time: Some(0),
        };
        DeviceOps::start_passive_scan(&mut device, &request)
            .await
            .unwrap();
        assert!(matches!(
            runner.poll(),
            Ok(Some(HardwareScanEvent::Observation(_)))
        ));
        assert!(matches!(
            runner.poll(),
            Ok(Some(HardwareScanEvent::Complete { success: true, .. }))
        ));

        let backend = runner.backend.lock().unwrap();
        assert!(!backend.authorization.permits_tx());
        assert!(!backend.authorization.is_live());
        let state = state.lock().unwrap();
        assert!(state.frames.is_empty());
        assert!(state.lifecycle_poisoned);
        assert!(state.regulatory_channel.is_none());
        assert!(state.current_channel.is_none());
        assert!(state.regulatory_max_dbm.is_none());
        assert!(state.sar_cap_dbm.is_none());
        assert!(state.programmed_power_dbm.is_none());
        assert!(state.authorized_rate_mbps.is_none());
        assert!(state.programmed_rate_mbps.is_none());
    });
}

#[test]
fn stop_failure_revokes_and_clears_evidence_before_first_frame() {
    let state = Arc::new(Mutex::new(BackendState {
        regulatory_channel: Some(channel(6)),
        current_channel: Some(channel(6)),
        regulatory_max_dbm: Some(18),
        sar_cap_dbm: Some(16),
        programmed_power_dbm: Some(16),
        authorized_rate_mbps: Some(6),
        programmed_rate_mbps: Some(6),
        fail_stop: true,
        ..Default::default()
    }));
    let (_device, runner) = Mt7921ClientDevice::new(
        ProductionBackend(state.clone()),
        physical_adapter(),
        client_support(),
    );

    assert!(state.lock().unwrap().frames.is_empty());
    assert_eq!(runner.stop(), Err(zx::Status::IO));
    let backend = runner.backend.lock().unwrap();
    assert!(!backend.authorization.permits_tx());
    assert!(!backend.authorization.is_live());
    let state = state.lock().unwrap();
    assert!(state.frames.is_empty());
    assert!(state.lifecycle_poisoned);
    assert!(state.regulatory_channel.is_none());
    assert!(state.current_channel.is_none());
    assert!(state.regulatory_max_dbm.is_none());
    assert!(state.sar_cap_dbm.is_none());
    assert!(state.programmed_power_dbm.is_none());
    assert!(state.authorized_rate_mbps.is_none());
    assert!(state.programmed_rate_mbps.is_none());
}
