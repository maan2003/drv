// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use fidl_fuchsia_wlan_common::{ScanType, SecuritySupport, SpectrumManagementSupport, WlanMacRole};
use fidl_fuchsia_wlan_ieee80211::{
    BssDescription, BssType, ChannelBandwidth, ChannelNumber, WlanBand,
};
use fidl_fuchsia_wlan_internal::{Authentication, Credentials, Protocol, WpaCredentials};
use fidl_fuchsia_wlan_mlme::{BandCapability, DeviceInfo};
use fidl_fuchsia_wlan_sme::ConnectRequest;
use fidl_fuchsia_wlan_sme::Protection;
use futures::channel::mpsc;
use futures::executor::block_on;
use futures::lock::Mutex;
use futures::task::Poll;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use wlan_common::channel::{Bandwidth, Channel};
use wlan_common::scan::{Compatible, Incompatible};
use wlan_common::security::SecurityDescriptor;
use wlan_common::sequestered::Sequestered;
use wlan_sme::MlmeRequest;
use wlan_sme::client::{ClientConfig, ClientSme};
use wlancfg_selection::client::connection_selection::fut_manager::{
    ConnectionSelectionFutures, ConnectionSelectionManager, SelectionIdentifier,
};
use wlancfg_selection::client::connection_selection::{
    ConnectionSelectionRequester, ConnectionSelector, ConnectionSelectorApi,
};
use wlancfg_selection::client::scan::{ScanReason, ScanRequestApi};
use wlancfg_selection::client::types::{
    self, Bss, NetworkIdentifier, ScanObservation, ScanResult, SecurityType, Signal,
};
use wlancfg_selection::config_management::{
    Credential, NetworkConfig, NetworkConfigError, PastConnectionData, PastConnectionList,
    SavedNetworksManagerApi,
};
use wlancfg_selection::service_boundary::WifiConnectCommand;
use wlancfg_selection::telemetry::{TelemetryEvent, TelemetrySender};
use wlancfg_selection::wlan_metrics_registry::PolicyConnectionAttemptMigratedMetricDimensionReason as ConnectReason;

#[derive(Clone)]
struct ScanCall {
    deadline: wlan_control_wire::MonotonicDeadline,
    reason: ScanReason,
    ssids: Vec<types::Ssid>,
    channels: Vec<types::WlanChan>,
}

struct SpyScan {
    calls: Mutex<Vec<ScanCall>>,
    results: Mutex<VecDeque<Result<Vec<ScanResult>, types::ScanError>>>,
}

impl SpyScan {
    fn new(results: Vec<Result<Vec<ScanResult>, types::ScanError>>) -> Self {
        Self {
            calls: Mutex::new(vec![]),
            results: Mutex::new(results.into()),
        }
    }
}

#[async_trait(?Send)]
impl ScanRequestApi for SpyScan {
    async fn perform_scan(
        &self,
        deadline: wlan_control_wire::MonotonicDeadline,
        reason: ScanReason,
        ssids: Vec<types::Ssid>,
        channels: Vec<types::WlanChan>,
    ) -> Result<Vec<ScanResult>, types::ScanError> {
        self.calls.lock().await.push(ScanCall {
            deadline,
            reason,
            ssids,
            channels,
        });
        self.results
            .lock()
            .await
            .pop_front()
            .expect("unexpected scan request")
    }
}

struct SpySavedNetworks {
    configs: Vec<NetworkConfig>,
    compatible_lookups: Mutex<Vec<(types::Ssid, Protection)>>,
}

impl SpySavedNetworks {
    fn new(configs: Vec<NetworkConfig>) -> Self {
        Self {
            configs,
            compatible_lookups: Mutex::new(vec![]),
        }
    }
}

#[async_trait(?Send)]
impl SavedNetworksManagerApi for SpySavedNetworks {
    async fn remove(&self, _: NetworkIdentifier) -> Result<bool, NetworkConfigError> {
        panic!("unused saved-network effect")
    }
    async fn known_network_count(&self) -> usize {
        self.configs.len()
    }
    async fn lookup(&self, id: &NetworkIdentifier) -> Option<NetworkConfig> {
        self.configs
            .iter()
            .find(|c| c.ssid == id.ssid && c.security_type == id.security_type)
            .cloned()
    }
    async fn lookup_compatible(
        &self,
        ssid: &types::Ssid,
        protection: Protection,
    ) -> Vec<NetworkConfig> {
        self.compatible_lookups
            .lock()
            .await
            .push((ssid.clone(), protection));
        self.configs
            .iter()
            .filter(|c| &c.ssid == ssid)
            .cloned()
            .collect()
    }
    async fn store(
        &self,
        _: NetworkIdentifier,
        _: Credential,
    ) -> Result<Option<NetworkConfig>, NetworkConfigError> {
        panic!("unused saved-network effect")
    }
    async fn record_connect_result(
        &self,
        _: NetworkIdentifier,
        _: &Credential,
        _: types::Bssid,
        _: fidl_fuchsia_wlan_sme::ConnectResult,
        _: ScanObservation,
    ) {
        panic!("unused saved-network effect")
    }
    async fn record_disconnect(
        &self,
        _: &NetworkIdentifier,
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
        _: &NetworkIdentifier,
        _: &Credential,
    ) -> Result<bool, anyhow::Error> {
        panic!("unused saved-network effect")
    }
    async fn get_networks(&self) -> Vec<NetworkConfig> {
        self.configs.clone()
    }
    async fn get_past_connections(
        &self,
        _: &NetworkIdentifier,
        _: &Credential,
        _: &types::Bssid,
    ) -> PastConnectionList {
        PastConnectionList::new(1)
    }
}

fn channel(primary: u8) -> Channel {
    Channel {
        primary,
        bandwidth: Bandwidth::Cbw20,
        band: WlanBand::TwoGhz,
    }
}

fn fidl_bss(bssid: [u8; 6], ssid: &[u8], primary: u8, rssi: i8) -> BssDescription {
    let mut ies = vec![0, ssid.len() as u8];
    ies.extend_from_slice(ssid);
    ies.extend_from_slice(&[1, 4, 2, 4, 11, 22]);
    BssDescription {
        bssid,
        bss_type: BssType::Infrastructure,
        beacon_period: 100,
        capability_info: 1,
        ies,
        primary: ChannelNumber {
            band: WlanBand::TwoGhz,
            number: primary,
        },
        bandwidth: ChannelBandwidth::Cbw20,
        vht_secondary_80_channel: ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 0,
        },
        rssi_dbm: rssi,
        snr_db: 30,
    }
}

fn bss(
    bssid: [u8; 6],
    ssid: &[u8],
    primary: u8,
    rssi: i8,
    observation: ScanObservation,
    compatible: bool,
) -> Bss {
    Bss {
        bssid: bssid.into(),
        signal: Signal {
            rssi_dbm: rssi,
            snr_db: 30,
        },
        channel: channel(primary),
        timestamp: zx::MonotonicInstant::from_nanos(1),
        observation,
        compatibility: if compatible {
            Compatible::expect_ok([SecurityDescriptor::OPEN])
        } else {
            Incompatible::unknown()
        },
        bss_description: Sequestered::from(fidl_bss(bssid, ssid, primary, rssi)),
    }
}

fn wpa3_bss(bssid: [u8; 6], ssid: &[u8]) -> Bss {
    Bss {
        bssid: bssid.into(),
        signal: Signal {
            rssi_dbm: -30,
            snr_db: 30,
        },
        channel: channel(1),
        timestamp: zx::MonotonicInstant::from_nanos(1),
        observation: ScanObservation::Active,
        compatibility: Compatible::expect_ok([SecurityDescriptor::WPA3_PERSONAL]),
        bss_description: Sequestered::from(fidl_bss(bssid, ssid, 1, -30)),
    }
}

fn scan_result(ssid: &[u8], entries: Vec<Bss>) -> ScanResult {
    ScanResult {
        ssid: types::Ssid::from_bytes_unchecked(ssid.to_vec()),
        security_type_detailed: Protection::Open,
        entries,
        compatibility: wlancfg_selection::fidl_fuchsia_wlan_policy::Compatibility::Supported,
    }
}

fn open_config(ssid: &[u8]) -> NetworkConfig {
    NetworkConfig::new(
        NetworkIdentifier::new(
            types::Ssid::from_bytes_unchecked(ssid.to_vec()),
            SecurityType::None,
        ),
        Credential::None,
        true,
        Some(0.0),
    )
    .unwrap()
}

fn wpa3_config(ssid: &[u8], password: &[u8]) -> NetworkConfig {
    NetworkConfig::new(
        NetworkIdentifier::new(
            types::Ssid::from_bytes_unchecked(ssid.to_vec()),
            SecurityType::Wpa3,
        ),
        Credential::Password(password.to_vec()),
        true,
        Some(0.0),
    )
    .unwrap()
}

fn selector(
    scan: Arc<SpyScan>,
    saved: Arc<SpySavedNetworks>,
) -> (ConnectionSelector, mpsc::Receiver<TelemetryEvent>) {
    let inspector = fuchsia_inspect::Inspector::default();
    let (sender, receiver) = mpsc::channel(32);
    (
        ConnectionSelector::new(
            saved,
            scan,
            inspector.root().create_child("selection"),
            TelemetrySender::new(sender),
        ),
        receiver,
    )
}

#[test]
fn directed_selection_uses_pinned_filter_and_score_order() {
    block_on(async {
        let ssid = b"open";
        let first = [2, 0, 0, 0, 0, 1];
        let strongest = [2, 0, 0, 0, 0, 2];
        let incompatible = [2, 0, 0, 0, 0, 3];
        let scan = Arc::new(SpyScan::new(vec![Ok(vec![scan_result(
            ssid,
            vec![
                bss(first, ssid, 1, -55, ScanObservation::Active, true),
                bss(strongest, ssid, 1, -35, ScanObservation::Active, true),
                bss(incompatible, ssid, 1, -20, ScanObservation::Active, false),
            ],
        )])]));
        let saved = Arc::new(SpySavedNetworks::new(vec![open_config(ssid)]));
        let (selector, _telemetry) = selector(scan.clone(), saved.clone());
        let target = NetworkIdentifier::new(
            types::Ssid::from_bytes_unchecked(ssid.to_vec()),
            SecurityType::None,
        );

        let selected = selector
            .find_and_select_connection_candidate(
                wlan_control_wire::MonotonicDeadline::after(std::time::Duration::from_secs(30))
                    .unwrap(),
                Some(target),
                ConnectReason::FidlConnectRequest,
            )
            .await
            .expect("pinned selector should choose a compatible BSS");

        assert_eq!(selected.bss.bssid, strongest.into());
        let request = WifiConnectCommand::from_selected(selected).into_sme_request();
        assert_eq!(request.ssid, ssid);
        assert_eq!(request.bss_description.bssid, strongest);
        assert!(request.multiple_bss_candidates);
        assert_eq!(request.authentication.protocol, Protocol::Open);
        assert!(request.authentication.credentials.is_none());
        assert_eq!(request.deprecated_scan_type, ScanType::Active);
        let calls = scan.calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].reason, ScanReason::BssSelection);
        assert_eq!(calls[0].ssids[0].to_vec(), ssid);
        assert!(calls[0].channels.is_empty());
        assert_eq!(saved.compatible_lookups.lock().await.len(), 1);
    });
}

#[test]
fn wifi_command_contains_only_the_selected_active_credential() {
    block_on(async {
        let ssid = b"secured";
        let password = b"synthetic-password";
        let address = [2, 0, 0, 0, 0, 4];
        let scan = Arc::new(SpyScan::new(vec![Ok(vec![ScanResult {
            ssid: types::Ssid::from_bytes_unchecked(ssid.to_vec()),
            security_type_detailed: Protection::Wpa3Personal,
            entries: vec![wpa3_bss(address, ssid)],
            compatibility: wlancfg_selection::fidl_fuchsia_wlan_policy::Compatibility::Supported,
        }])]));
        let saved = Arc::new(SpySavedNetworks::new(vec![wpa3_config(ssid, password)]));
        let (selector, _telemetry) = selector(scan, saved);
        let target = NetworkIdentifier::new(
            types::Ssid::from_bytes_unchecked(ssid.to_vec()),
            SecurityType::Wpa3,
        );

        let selected = selector
            .find_and_select_connection_candidate(
                wlan_control_wire::MonotonicDeadline::after(std::time::Duration::from_secs(30))
                    .unwrap(),
                Some(target),
                ConnectReason::FidlConnectRequest,
            )
            .await
            .expect("pinned selector should choose the saved WPA3 network");
        let request = WifiConnectCommand::from_selected(selected).into_sme_request();

        assert_eq!(request.ssid, ssid);
        assert_eq!(request.bss_description.bssid, address);
        assert_eq!(request.authentication.protocol, Protocol::Wpa3Personal);
        assert_eq!(
            request.authentication.credentials,
            Some(Box::new(Credentials::Wpa(WpaCredentials::Passphrase(
                password.to_vec()
            ))))
        );
    });
}

#[test]
fn passive_winner_is_augmented_by_the_pinned_active_scan() {
    block_on(async {
        let ssid = b"augment";
        let address = [2, 0, 0, 0, 1, 1];
        let passive = bss(address, ssid, 6, -45, ScanObservation::Passive, true);
        let augmented = bss(address, ssid, 6, -41, ScanObservation::Active, true);
        let augmented_description = augmented.bss_description.clone();
        let scan = Arc::new(SpyScan::new(vec![
            Ok(vec![scan_result(ssid, vec![passive])]),
            Ok(vec![scan_result(ssid, vec![augmented])]),
        ]));
        let saved = Arc::new(SpySavedNetworks::new(vec![open_config(ssid)]));
        let (selector, _telemetry) = selector(scan.clone(), saved);

        let deadline =
            wlan_control_wire::MonotonicDeadline::after(std::time::Duration::from_secs(30))
                .unwrap();
        let selected = selector
            .find_and_select_connection_candidate(
                deadline,
                None,
                ConnectReason::IdleInterfaceAutoconnect,
            )
            .await
            .expect("pinned selector should return its augmented candidate");

        assert_eq!(selected.bss.bss_description, augmented_description);
        let calls = scan.calls.lock().await;
        assert_eq!(calls.len(), 2);
        assert!(
            calls.iter().all(|call| call.deadline == deadline),
            "augmentation renewed budget"
        );
        assert_eq!(calls[0].reason, ScanReason::NetworkSelection);
        assert_eq!(calls[1].reason, ScanReason::BssSelectionAugmentation);
        assert_eq!(calls[1].ssids[0].to_vec(), ssid);
        assert_eq!(calls[1].channels, vec![channel(6)]);
    });
}

#[test]
fn equal_scores_preserve_the_pinned_input_order() {
    block_on(async {
        let ssid = b"tie";
        let first = [2, 0, 0, 0, 2, 1];
        let second = [2, 0, 0, 0, 2, 2];
        let scan = Arc::new(SpyScan::new(vec![Ok(vec![scan_result(
            ssid,
            vec![
                bss(first, ssid, 1, -40, ScanObservation::Active, true),
                bss(second, ssid, 1, -40, ScanObservation::Active, true),
            ],
        )])]));
        let saved = Arc::new(SpySavedNetworks::new(vec![open_config(ssid)]));
        let (selector, _telemetry) = selector(scan, saved);
        let target = NetworkIdentifier::new(
            types::Ssid::from_bytes_unchecked(ssid.to_vec()),
            SecurityType::None,
        );
        let selected = selector
            .find_and_select_connection_candidate(
                wlan_control_wire::MonotonicDeadline::after(std::time::Duration::from_secs(30))
                    .unwrap(),
                Some(target),
                ConnectReason::FidlConnectRequest,
            )
            .await
            .unwrap();
        assert_eq!(selected.bss.bssid, first.into());
    });
}

#[test]
fn no_candidates_returns_none_without_local_fallback() {
    block_on(async {
        let scan = Arc::new(SpyScan::new(vec![Ok(vec![])]));
        let saved = Arc::new(SpySavedNetworks::new(vec![]));
        let (selector, mut telemetry) = selector(scan, saved);
        assert!(
            selector
                .find_and_select_connection_candidate(
                    wlan_control_wire::MonotonicDeadline::after(std::time::Duration::from_secs(30))
                        .unwrap(),
                    None,
                    ConnectReason::IdleInterfaceAutoconnect,
                )
                .await
                .is_none()
        );
        assert!(matches!(
            telemetry.try_recv(),
            Ok(TelemetryEvent::ActiveScanRequested {
                num_ssids_requested: 0
            })
        ));
    });
}

#[test]
fn pinned_selection_manager_cancellation_discards_the_result() {
    block_on(async {
        let (sender, mut requests) = mpsc::channel(1);
        let mut manager =
            ConnectionSelectionManager::new(ConnectionSelectionRequester::new(sender));
        manager.initiate_automatic_connection_selection(
            wlan_control_wire::MonotonicDeadline::after(std::time::Duration::from_secs(30))
                .unwrap(),
        );
        assert!(manager.active_selections() == vec![SelectionIdentifier::Automatic]);

        // Keep the production request's responder alive so only the pinned
        // cancellation branch can complete the managed future.
        {
            let mut pending = std::pin::pin!(ConnectionSelectionFutures::new(&mut manager));
            assert!(matches!(futures::poll!(&mut pending), Poll::Pending));
        }
        let pending_request = requests
            .try_recv()
            .expect("selection request must be emitted");
        manager.cancel(&SelectionIdentifier::Automatic);
        let (id, result) = ConnectionSelectionFutures::new(&mut manager).await;
        assert!(id == SelectionIdentifier::Automatic);
        assert!(matches!(result, Ok(None)));
        drop(pending_request);
    });
}

#[test]
fn ordinary_sme_connect_data_cannot_impersonate_wlancfg_selection() {
    block_on(async {
        let scan = Arc::new(SpyScan::new(vec![]));
        let saved = Arc::new(SpySavedNetworks::new(vec![]));
        let (_selector, _telemetry) = selector(scan.clone(), saved.clone());

        let inspector = fuchsia_inspect::Inspector::default();
        let (mut sme, _sink, mut requests, _time) = ClientSme::new(
            ClientConfig::default(),
            DeviceInfo {
                sta_addr: [2, 0, 0, 0, 0, 1],
                factory_addr: [2, 0, 0, 0, 0, 1],
                role: WlanMacRole::Client,
                bands: vec![BandCapability {
                    band: WlanBand::TwoGhz,
                    basic_rates: vec![2, 4, 11, 22],
                    ht_cap: None,
                    vht_cap: None,
                    primary_channels: vec![ChannelNumber {
                        band: WlanBand::TwoGhz,
                        number: 1,
                    }],
                }],
                softmac_hardware_capability: 0,
                qos_capable: false,
            },
            inspector.clone(),
            inspector.root().create_child("sme"),
            SecuritySupport::default(),
            SpectrumManagementSupport::default(),
        );
        let description = fidl_bss([2, 0, 0, 0, 9, 1], b"caller", 1, -30);
        let _transaction = sme.on_connect_command(ConnectRequest {
            ssid: b"caller".to_vec(),
            bss_description: description,
            multiple_bss_candidates: false,
            authentication: Authentication {
                protocol: Protocol::Open,
                credentials: None,
            },
            deprecated_scan_type: ScanType::Passive,
        });
        assert!(matches!(requests.try_recv(), Ok(MlmeRequest::Connect(_))));
        assert!(scan.calls.lock().await.is_empty());
        assert!(saved.compatible_lookups.lock().await.is_empty());
    });
}
