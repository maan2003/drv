// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use fidl_fuchsia_wlan_ieee80211::{
    BssDescription, BssType, ChannelBandwidth, ChannelNumber, StatusCode, WlanBand,
};
use fidl_fuchsia_wlan_sme as fidl_sme;
use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use wlan_common::channel::{Bandwidth, Channel};
use wlan_common::scan::Compatible;
use wlan_common::security::{SecurityAuthenticator, SecurityDescriptor};
use wlan_common::sequestered::Sequestered;
use wlancfg_selection::client::roaming::local_roam_manager::RoamManager;
use wlancfg_selection::client::state_machine::{self, ClientApi};
use wlancfg_selection::client::types::{
    self, Bss, ConnectSelection, InternalSavedNetworkData, NetworkIdentifier, ScanObservation,
    SecurityType, Signal,
};
use wlancfg_selection::config_management::{
    Credential, HistoricalListsByBssid, NetworkConfig, NetworkConfigError, PastConnectionData,
    PastConnectionList, SavedNetworksManagerApi,
};
use wlancfg_selection::mode_management::iface_manager_api::SmeForClientStateMachine;
use wlancfg_selection::mode_management::{
    ClientSmeEventStream, ClientSmeScanResult, ClientSmeTransport, ConnectTransactionEventStream,
    Defect,
};
use wlancfg_selection::telemetry::{TelemetryEvent, TelemetrySender};
use wlancfg_selection::util::state_machine::status_publisher_and_reader;
use wlancfg_selection::wlan_metrics_registry::PolicyConnectionAttemptMigratedMetricDimensionReason as ConnectReason;

struct Saved(NetworkConfig);
#[async_trait(?Send)]
impl SavedNetworksManagerApi for Saved {
    async fn remove(&self, _: NetworkIdentifier) -> Result<bool, NetworkConfigError> {
        unreachable!()
    }
    async fn known_network_count(&self) -> usize {
        1
    }
    async fn lookup(&self, id: &NetworkIdentifier) -> Option<NetworkConfig> {
        (self.0.ssid == id.ssid && self.0.security_type == id.security_type).then(|| self.0.clone())
    }
    async fn lookup_compatible(
        &self,
        _: &types::Ssid,
        _: fidl_sme::Protection,
    ) -> Vec<NetworkConfig> {
        vec![]
    }
    async fn store(
        &self,
        _: NetworkIdentifier,
        _: Credential,
    ) -> Result<Option<NetworkConfig>, NetworkConfigError> {
        unreachable!()
    }
    async fn record_connect_result(
        &self,
        _: NetworkIdentifier,
        _: &Credential,
        _: types::Bssid,
        _: fidl_sme::ConnectResult,
        _: ScanObservation,
    ) {
    }
    async fn record_disconnect(
        &self,
        _: &NetworkIdentifier,
        _: &Credential,
        _: PastConnectionData,
    ) {
    }
    async fn record_periodic_metrics(&self) {}
    async fn record_scan_result(
        &self,
        _: Vec<types::Ssid>,
        _: &HashMap<types::NetworkIdentifierDetailed, Vec<Bss>>,
    ) {
    }
    async fn is_network_single_bss(
        &self,
        _: &NetworkIdentifier,
        _: &Credential,
    ) -> Result<bool, anyhow::Error> {
        Ok(true)
    }
    async fn get_networks(&self) -> Vec<NetworkConfig> {
        vec![self.0.clone()]
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

struct Sim {
    results: RefCell<VecDeque<fidl_sme::ConnectResult>>,
    attempts: RefCell<Vec<(Instant, fidl_sme::ConnectRequest)>>,
    events: RefCell<
        Vec<mpsc::UnboundedSender<Result<fidl_sme::ConnectTransactionEvent, anyhow::Error>>>,
    >,
    disconnects: RefCell<Vec<fidl_sme::UserDisconnectReason>>,
}
impl Sim {
    fn new(results: impl IntoIterator<Item = fidl_sme::ConnectResult>) -> Rc<Self> {
        Rc::new(Self {
            results: RefCell::new(results.into_iter().collect()),
            attempts: RefCell::new(vec![]),
            events: RefCell::new(vec![]),
            disconnects: RefCell::new(vec![]),
        })
    }
}
#[async_trait(?Send)]
impl ClientSmeTransport for Sim {
    async fn connect(
        &self,
        request: &fidl_sme::ConnectRequest,
    ) -> Result<(fidl_sme::ConnectResult, ConnectTransactionEventStream), anyhow::Error> {
        self.attempts
            .borrow_mut()
            .push((Instant::now(), request.clone()));
        let (tx, rx) = mpsc::unbounded();
        self.events.borrow_mut().push(tx);
        Ok((
            self.results
                .borrow_mut()
                .pop_front()
                .expect("unexpected connect"),
            rx.boxed_local().fuse(),
        ))
    }
    async fn disconnect(
        &self,
        reason: fidl_sme::UserDisconnectReason,
    ) -> Result<(), anyhow::Error> {
        self.disconnects.borrow_mut().push(reason);
        Ok(())
    }
    fn roam(&self, _: &fidl_sme::RoamRequest) -> Result<(), anyhow::Error> {
        Ok(())
    }
    async fn scan(&self, _: &fidl_sme::ScanRequest) -> Result<ClientSmeScanResult, anyhow::Error> {
        unreachable!()
    }
    fn take_event_stream(&self) -> ClientSmeEventStream {
        futures::stream::pending().boxed_local().fuse()
    }
}

fn result(code: StatusCode, credential: bool) -> fidl_sme::ConnectResult {
    fidl_sme::ConnectResult {
        code,
        is_credential_rejected: credential,
        is_reconnect: false,
    }
}
fn selection() -> (ConnectSelection, NetworkConfig) {
    let ssid = types::Ssid::from_bytes_unchecked(b"host-state".to_vec());
    let id = NetworkIdentifier::new(ssid.clone(), SecurityType::None);
    let desc = BssDescription {
        bssid: [1, 2, 3, 4, 5, 6],
        bss_type: BssType::Infrastructure,
        beacon_period: 100,
        capability_info: 1,
        ies: vec![
            0, 10, b'h', b'o', b's', b't', b'-', b's', b't', b'a', b't', b'e', 1, 1, 2,
        ],
        primary: ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 6,
        },
        bandwidth: ChannelBandwidth::Cbw20,
        vht_secondary_80_channel: ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 0,
        },
        rssi_dbm: -40,
        snr_db: 30,
    };
    let candidate = types::ScannedCandidate {
        network: id.clone(),
        security_type_detailed: fidl_sme::Protection::Open,
        credential: Credential::None,
        bss: Bss {
            bssid: desc.bssid.into(),
            signal: Signal {
                rssi_dbm: -40,
                snr_db: 30,
            },
            channel: Channel {
                primary: 6,
                bandwidth: Bandwidth::Cbw20,
                band: WlanBand::TwoGhz,
            },
            timestamp: zx::MonotonicInstant::now(),
            observation: ScanObservation::Active,
            compatibility: Compatible::expect_ok([SecurityDescriptor::OPEN]),
            bss_description: Sequestered::from(desc),
        },
        network_has_multiple_bss: false,
        authenticator: SecurityAuthenticator::Open,
        saved_network_info: InternalSavedNetworkData {
            has_ever_connected: true,
            recent_failures: vec![],
            past_connections: HistoricalListsByBssid::new(),
        },
    };
    let config = NetworkConfig::new(id, Credential::None, true, Some(0.0)).unwrap();
    (
        ConnectSelection {
            target: candidate,
            reason: ConnectReason::FidlConnectRequest,
        },
        config,
    )
}

async fn run(
    sim: Rc<Sim>,
    selected: ConnectSelection,
    saved: NetworkConfig,
    driver: impl std::future::Future<Output = ()>,
) {
    let (req_tx, req_rx) = mpsc::channel(4);
    let (listener_tx, _listener_rx) = mpsc::unbounded();
    let (telemetry_tx, _telemetry_rx) = mpsc::channel::<TelemetryEvent>(100);
    let (defect_tx, _defect_rx) = mpsc::channel::<Defect>(10);
    let (roam_tx, _roam_rx) = mpsc::unbounded();
    let (status_tx, _) = status_publisher_and_reader();
    let client = state_machine::Client::new(req_tx);
    let machine = state_machine::serve(
        7,
        SmeForClientStateMachine::new(sim.clone()),
        sim.take_event_stream(),
        req_rx,
        listener_tx,
        Arc::new(Saved(saved)),
        Some(selected),
        TelemetrySender::new(telemetry_tx),
        defect_tx,
        RoamManager::new(roam_tx),
        status_tx,
    );
    futures::join!(machine, async move {
        let _client = client;
        driver.await;
    });
}

fn execute(f: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn exact_four_attempt_linear_backoff() {
    execute(async {
        let sim = Sim::new([result(StatusCode::RefusedReasonUnspecified, false); 4]);
        let (selected, saved) = selection();
        let started = Instant::now();
        run(sim.clone(), selected, saved, async {}).await;
        let calls = sim.attempts.borrow();
        assert_eq!(calls.len(), 4);
        let elapsed: Vec<_> = calls
            .iter()
            .map(|(t, _)| t.duration_since(started).as_millis())
            .collect();
        let delays: Vec<_> = elapsed.windows(2).map(|pair| pair[1] - pair[0]).collect();
        for (actual, expected) in delays.iter().zip([400, 800, 1200]) {
            assert!(
                (*actual as i128 - expected).abs() < 180,
                "attempts at {elapsed:?}"
            );
        }
        assert!(
            calls
                .windows(2)
                .all(|pair| pair[0].1.authentication == pair[1].1.authentication)
        );
    })
}

#[test]
fn credential_rejection_never_retries() {
    execute(async {
        let sim = Sim::new([result(StatusCode::RefusedReasonUnspecified, true)]);
        let (selected, saved) = selection();
        run(sim.clone(), selected, saved, async {}).await;
        assert_eq!(sim.attempts.borrow().len(), 1);
    })
}

#[test]
fn connected_events_progress_and_manual_disconnect_cancels() {
    execute(async {
        let sim = Sim::new([result(StatusCode::Success, false)]);
        let (selected, saved) = selection();
        let sim_driver = sim.clone();
        let (req_tx, req_rx) = mpsc::channel(4);
        let (listener_tx, mut listener_rx) = mpsc::unbounded();
        let (telemetry_tx, _telemetry_rx) = mpsc::channel::<TelemetryEvent>(100);
        let (defect_tx, _defect_rx) = mpsc::channel::<Defect>(10);
        let (roam_tx, _roam_rx) = mpsc::unbounded();
        let (status_tx, status_rx) = status_publisher_and_reader();
        let mut client = state_machine::Client::new(req_tx);
        let machine = state_machine::serve(
            7,
            SmeForClientStateMachine::new(sim.clone()),
            sim.take_event_stream(),
            req_rx,
            listener_tx,
            Arc::new(Saved(saved)),
            Some(selected),
            TelemetrySender::new(telemetry_tx),
            defect_tx,
            RoamManager::new(roam_tx),
            status_tx,
        );
        let drive = async move {
            while sim_driver.events.borrow().is_empty() {
                tokio::task::yield_now().await;
            }
            sim_driver.events.borrow()[0]
                .unbounded_send(Ok(fidl_sme::ConnectTransactionEvent::OnSignalReport {
                    ind: fidl_fuchsia_wlan_internal::SignalReportIndication {
                        rssi_dbm: -55,
                        snr_db: 20,
                    },
                }))
                .unwrap();
            tokio::task::yield_now().await;
            assert_eq!(
                status_rx.read_status().unwrap(),
                state_machine::Status::Connected {
                    channel: 6,
                    rssi: -55,
                    snr: 20
                }
            );
            let (done_tx, done_rx) = oneshot::channel();
            client
                .disconnect(
                    types::DisconnectReason::FidlStopClientConnectionsRequest,
                    done_tx,
                )
                .unwrap();
            done_rx.await.unwrap();
            assert_eq!(
                status_rx.read_status().unwrap(),
                state_machine::Status::Disconnected
            );
            assert!(listener_rx.next().await.is_some());
        };
        futures::join!(machine, drive);
        assert_eq!(sim.disconnects.borrow().len(), 2);
    })
}
