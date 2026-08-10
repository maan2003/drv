// SPDX-License-Identifier: GPL-2.0-only

use super::*;
use async_trait::async_trait;
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_sme as fidl_sme;
use fuchsia_softmac_port::{MlmeScanEvent, PassiveScanner, ScanRequest};
use futures::StreamExt;
use futures::channel::mpsc;
use futures::lock::Mutex as AsyncMutex;
use mt7921_port_spike::{CandidateChannel, NicCapability, NicPhyCapability, PhysicalBand};
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use wlan_common::channel::{Bandwidth, Channel};
use wlan_common::scan::Compatible;
use wlan_common::security::SecurityDescriptor;
use wlan_common::sequestered::Sequestered;
use wlan_mlme::{MlmeImpl, client::ClientMlme};
use wlan_sme::client::{ClientConfig, ClientSme};
use wlan_sme::{MlmeRequest, Station};
use wlancfg_selection::client::connection_selection::{ConnectionSelector, ConnectionSelectorApi};
use wlancfg_selection::client::scan::{ScanReason, ScanRequestApi};
use wlancfg_selection::client::types::{self, Bss, ScanObservation, ScanResult, Signal};
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

fn physical_scan() -> fidl_ieee80211::BssDescription {
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
    let transport = PhysicalScan {
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
    };
    let mut adapter =
        crate::Mt7921SoftmacAdapter::new(transport, nic, vec![candidate], vec![channel(6)])
            .unwrap();
    let mut scanner = PassiveScanner::default();
    scanner
        .start(
            &mut adapter,
            ScanRequest {
                txn_id: 41,
                scan_type: fidl_mlme::ScanTypes::Passive,
                channel_list: vec![channel(6)],
                ssid_list: vec![],
                probe_delay: 0,
                min_channel_time: 50,
                max_channel_time: 100,
            },
        )
        .unwrap();
    let result = match scanner.poll(&mut adapter).unwrap() {
        Some(MlmeScanEvent::Result { result, .. }) => result,
        other => panic!("physical beacon did not produce a scan result: {other:?}"),
    };
    assert!(matches!(
        scanner.poll(&mut adapter).unwrap(),
        Some(MlmeScanEvent::End(fidl_mlme::ScanEnd {
            txn_id: 41,
            code: fidl_mlme::ScanResultCode::Success,
        }))
    ));
    result.bss
}

struct ScanSource {
    result: AsyncMutex<Option<ScanResult>>,
    reasons: AsyncMutex<Vec<ScanReason>>,
}

#[async_trait(?Send)]
impl ScanRequestApi for ScanSource {
    async fn perform_scan(
        &self,
        reason: ScanReason,
        _: Vec<types::Ssid>,
        _: Vec<types::WlanChan>,
    ) -> Result<Vec<ScanResult>, types::ScanError> {
        self.reasons.lock().await.push(reason);
        self.result
            .lock()
            .await
            .take()
            .map(|result| vec![result])
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
enum Reject {
    AlreadyTransmitted,
    Revoked,
}

#[derive(Default)]
struct BackendState {
    channel: Option<fidl_ieee80211::ChannelNumber>,
    regulatory_channel: Option<fidl_ieee80211::ChannelNumber>,
    sar_power_dbm: Option<i8>,
    transmitted: usize,
    rejects: Vec<Reject>,
}

impl BackendState {
    fn authorize(&mut self) {
        self.regulatory_channel = Some(channel(6));
        self.sar_power_dbm = Some(16);
    }

    fn reset(&mut self) {
        self.regulatory_channel = None;
        self.sar_power_dbm = None;
    }

    fn stop(&mut self) {
        self.reset();
    }
}

#[derive(Clone)]
struct ProductionBackend(Arc<Mutex<BackendState>>);

impl Mt7921ClientEffects for ProductionBackend {
    fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        _: fidl_ieee80211::ChannelBandwidth,
        _: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        self.0.lock().unwrap().channel = Some(primary);
        Ok(())
    }

    fn join_bss(&mut self, _: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        Ok(())
    }

    fn send_wlan_frame(
        &mut self,
        _: &[u8],
        _: fidl_softmac::WlanTxInfoFlags,
    ) -> Result<(), zx::Status> {
        let mut state = self.0.lock().unwrap();
        if state.channel != Some(channel(6))
            || state.regulatory_channel != state.channel
            || state.sar_power_dbm != Some(16)
        {
            state.rejects.push(Reject::Revoked);
            return Err(zx::Status::ACCESS_DENIED);
        }
        if state.transmitted != 0 {
            state.rejects.push(Reject::AlreadyTransmitted);
            return Err(zx::Status::ALREADY_EXISTS);
        }
        state.transmitted += 1;
        Ok(())
    }

    fn install_key(&mut self, _: &fidl_softmac::WlanKeyConfiguration) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn notify_association_complete(
        &mut self,
        _: &fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn clear_association(
        &mut self,
        _: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn set_link_up(&mut self, _: bool) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn next_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
        Ok(None)
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
        discovery: Default::default(),
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
    }
}

#[test]
fn passive_physical_selection_reaches_one_authorized_production_sae_tx() {
    futures::executor::block_on(async {
        let description = physical_scan();
        let scan = Arc::new(ScanSource {
            result: AsyncMutex::new(Some(ScanResult {
                ssid: types::Ssid::from_bytes_unchecked(SSID.to_vec()),
                security_type_detailed: fidl_sme::Protection::Wpa3Personal,
                entries: vec![Bss {
                    bssid: AP.into(),
                    signal: Signal {
                        rssi_dbm: description.rssi_dbm,
                        snr_db: description.snr_db,
                    },
                    channel: Channel {
                        primary: 6,
                        bandwidth: Bandwidth::Cbw20,
                        band: fidl_ieee80211::WlanBand::TwoGhz,
                    },
                    timestamp: zx::MonotonicInstant::from_nanos(10),
                    observation: ScanObservation::Passive,
                    compatibility: Compatible::expect_ok([SecurityDescriptor::WPA3_PERSONAL]),
                    bss_description: Sequestered::from(description),
                }],
                compatibility:
                    wlancfg_selection::fidl_fuchsia_wlan_policy::Compatibility::Supported,
            })),
            reasons: AsyncMutex::new(vec![]),
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
            scan.clone(),
            inspector.root().create_child("selection"),
            TelemetrySender::new(telemetry),
        );
        let selected = selector
            .find_and_select_connection_candidate(Some(network), ConnectReason::FidlConnectRequest)
            .await
            .expect("pinned selector must select the physical WPA3 BSS");
        assert_eq!(*scan.reasons.lock().await, [ScanReason::BssSelection]);

        let inspector = fuchsia_inspect::Inspector::default();
        let mut config = ClientConfig::default();
        config.wpa3_supported = true;
        let (mut sme, _sink, mut requests, _timers) = ClientSme::new(
            config,
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
            },
            inspector.clone(),
            inspector.root().create_child("sme"),
            client_support().security,
            Default::default(),
        );
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

        let state = Arc::new(Mutex::new(BackendState::default()));
        state.lock().unwrap().authorize();
        let mut device = Mt7921ClientDevice::new_offline_fake(
            ProductionBackend(state.clone()),
            client_support(),
        );
        let mut events = device.take_mlme_event_stream().unwrap();
        let (timer, _timer_stream) = wlan_mlme::common::timer::create_timer();
        let mut mlme = ClientMlme::new(Default::default(), device, timer)
            .await
            .unwrap();
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

        mlme.handle_mlme_request(MlmeRequest::SaeFrameTx(sae_tx.clone()))
            .await
            .unwrap();
        assert_eq!(state.lock().unwrap().transmitted, 1);
        let _ = mlme
            .handle_mlme_request(MlmeRequest::SaeFrameTx(sae_tx.clone()))
            .await;
        assert_eq!(
            state.lock().unwrap().rejects,
            [Reject::AlreadyTransmitted],
            "the backend must reject a second TX even though ClientMlme consumes the status"
        );
        {
            let mut state = state.lock().unwrap();
            state.reset();
        }
        let mut backend = ProductionBackend(state.clone());
        assert_eq!(
            backend.send_wlan_frame(&[], fidl_softmac::WlanTxInfoFlags::empty()),
            Err(zx::Status::ACCESS_DENIED)
        );
        assert_eq!(state.lock().unwrap().rejects.last(), Some(&Reject::Revoked));
        {
            let mut state = state.lock().unwrap();
            state.authorize();
            state.stop();
        }
        assert_eq!(
            backend.send_wlan_frame(&[], fidl_softmac::WlanTxInfoFlags::empty()),
            Err(zx::Status::ACCESS_DENIED)
        );
        let state = state.lock().unwrap();
        assert_eq!(state.transmitted, 1);
        assert_eq!(
            state.rejects,
            [Reject::AlreadyTransmitted, Reject::Revoked, Reject::Revoked]
        );
    });
}
