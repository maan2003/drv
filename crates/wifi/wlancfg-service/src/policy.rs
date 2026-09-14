// SPDX-License-Identifier: GPL-2.0-only

//! Long-lived single-interface wlancfg policy service.
//!
//! Saved credentials, selection, reconnect, and all SME control remain here.
//! Applications use the bounded application contract and never bypass this
//! policy owner to reach the Wi-Fi service.

use crate::{
    HostControlClient, ParkedHostControlClient,
    application::{
        ApplicationCommand, Association, Network, ParkedApplicationServer, Reply, Request,
        Security, Status,
    },
};
use anyhow::Context as _;
use async_trait::async_trait;
use fidl_fuchsia_wlan_sme as sme;
use futures::{
    StreamExt as _,
    channel::{mpsc, oneshot},
};
use std::{fs::File, os::fd::OwnedFd, rc::Rc, sync::Arc, time::Duration};
use wlancfg_selection::{
    client::{
        connection_selection::{ConnectionSelector, ConnectionSelectorApi as _},
        roaming::local_roam_manager::RoamManager,
        scan::{ScanReason, ScanRequestApi, selection_scan_results},
        state_machine::{self, ClientApi as _},
        types::{self, ConnectSelection},
    },
    config_management::{
        Credential, NetworkIdentifier, SavedNetworksManager, SavedNetworksManagerApi, SecurityType,
    },
    mode_management::{ClientSmeTransport, Defect, iface_manager_api::SmeForClientStateMachine},
    telemetry::{TelemetryEvent, TelemetrySender},
    util::state_machine::{StateMachineStatusReader, status_publisher_and_reader},
    wlan_metrics_registry::PolicyConnectionAttemptMigratedMetricDimensionReason as ConnectReason,
};

#[derive(Clone)]
struct ControlScan {
    control: HostControlClient,
    fixed_target_channel: Option<u8>,
}

#[async_trait(?Send)]
impl ScanRequestApi for ControlScan {
    async fn perform_scan(
        &self,
        _reason: ScanReason,
        ssids: Vec<types::Ssid>,
        channels: Vec<types::WlanChan>,
    ) -> Result<Vec<types::ScanResult>, types::ScanError> {
        let mut channels = channels
            .into_iter()
            .map(|channel| channel.primary)
            .collect::<Vec<_>>();
        if !ssids.is_empty()
            && channels.is_empty()
            && let Some(channel) = self.fixed_target_channel
        {
            channels.push(channel);
        }
        let target_ssids = ssids;
        // The production MT7921 boundary advertises passive offload only.
        // Preserve directed-selection semantics by filtering the returned
        // passive observations below; never turn an SSID hint into probe TX.
        let request = sme::ScanRequest::Passive(sme::PassiveScanRequest { channels });
        let results =
            self.control
                .scan(&request)
                .await
                .map_err(|_| types::ScanError::GeneralError)?
                .map_err(|error| match error {
                    sme::ScanErrorCode::ShouldWait
                    | sme::ScanErrorCode::CanceledByDriverOrFirmware => types::ScanError::Cancelled,
                    _ => types::ScanError::GeneralError,
                })?
                .results
                .into_iter()
                .filter_map(|result| result.try_into().ok())
                .collect();
        Ok(selection_scan_results(results, &target_ssids))
    }
}

struct Machine {
    client: state_machine::Client,
    status: StateMachineStatusReader<state_machine::Status>,
}

fn start_machine(
    control: HostControlClient,
    saved: Arc<dyn SavedNetworksManagerApi>,
    telemetry: TelemetrySender,
    selection: ConnectSelection,
) -> Machine {
    let event_stream = control.take_event_stream();
    let transport: Rc<dyn ClientSmeTransport> = Rc::new(control);
    let (request_tx, request_rx) = mpsc::channel(4);
    let client = state_machine::Client::new(request_tx);
    let (listener_tx, listener_rx) = mpsc::unbounded();
    let (defect_tx, defect_rx) = mpsc::channel::<Defect>(10);
    let (roam_tx, roam_rx) = mpsc::unbounded();
    let (status_tx, status) = status_publisher_and_reader();
    let machine = state_machine::serve(
        1,
        SmeForClientStateMachine::new(transport),
        event_stream,
        request_rx,
        listener_tx,
        saved,
        Some(selection),
        telemetry,
        defect_tx,
        RoamManager::new(roam_tx),
        status_tx,
    );
    tokio::task::spawn_local(async move {
        // These policy-report receivers intentionally remain live. This CLI
        // milestone does not invent a second UI listener/telemetry protocol.
        let _receivers = (listener_rx, defect_rx, roam_rx);
        machine.await;
    });
    Machine { client, status }
}

/// Load persistence before opening either untrusted IPC receive path, then run
/// policy until the Wi-Fi generation ends or the supervisor tears it down.
pub fn serve(
    runtime: tokio::runtime::Runtime,
    parked_control: ParkedHostControlClient,
    parked_applications: ParkedApplicationServer,
    mut commands: mpsc::Receiver<ApplicationCommand>,
    state_directory: OwnedFd,
) -> anyhow::Result<()> {
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let (telemetry_tx, _telemetry_rx) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry = TelemetrySender::new(telemetry_tx);
        let saved: Arc<dyn SavedNetworksManagerApi> = Arc::new(
            SavedNetworksManager::new_with_directory(
                File::from(state_directory),
                telemetry.clone(),
            )
            .await
            .context("load saved networks")?,
        );
        let control = parked_control
            .activate_after_persistence()
            .context("start WLAN control owner")?;
        parked_applications
            .activate_after_persistence()
            .context("start application owner")?;

        let scan = Arc::new(ControlScan {
            control: control.clone(),
            // This service launch is deliberately fixed-target. General
            // network selection/roaming owns a separate future design.
            fixed_target_channel: std::env::var("DRV_SAE_CHANNEL")
                .ok()
                .and_then(|channel| channel.parse().ok()),
        });
        let inspector = fuchsia_inspect::Inspector::default();
        let selector = ConnectionSelector::new(
            saved.clone(),
            scan.clone(),
            inspector.root().create_child("selection"),
            telemetry.clone(),
        );
        let mut current: Option<NetworkIdentifier> = None;
        let mut machine: Option<Machine> = None;

        if let Some(target) = selector
            .find_and_select_connection_candidate(None, ConnectReason::IdleInterfaceAutoconnect)
            .await
        {
            current = Some(target.network.clone());
            machine = Some(start_machine(
                control.clone(),
                saved.clone(),
                telemetry.clone(),
                ConnectSelection {
                    target,
                    reason: ConnectReason::IdleInterfaceAutoconnect,
                },
            ));
        }

        while let Some(command) = commands.next().await {
            let reply = handle(
                command.request,
                &control,
                scan.as_ref(),
                &selector,
                saved.clone(),
                telemetry.clone(),
                &mut machine,
                &mut current,
            )
            .await;
            let _ = command.responder.send(reply);
        }
        Ok(())
    })
}

async fn handle(
    request: Request,
    control: &HostControlClient,
    scan: &ControlScan,
    selector: &ConnectionSelector,
    saved: Arc<dyn SavedNetworksManagerApi>,
    telemetry: TelemetrySender,
    machine: &mut Option<Machine>,
    current: &mut Option<NetworkIdentifier>,
) -> Reply {
    match request {
        Request::Scan => match scan
            .perform_scan(ScanReason::ClientRequest, vec![], vec![])
            .await
        {
            Ok(results) => {
                let mut networks = Vec::new();
                for result in results.into_iter().take(64) {
                    let Some(security) = scan_security(result.security_type_detailed) else {
                        continue;
                    };
                    let rssi_dbm = result
                        .entries
                        .iter()
                        .map(|bss| bss.signal.rssi_dbm)
                        .max()
                        .unwrap_or(i8::MIN);
                    networks.push(Network {
                        ssid: result.ssid.to_vec(),
                        security,
                        rssi_dbm,
                    });
                }
                Reply::Scan(networks)
            }
            Err(_) => Reply::Error("scan failed".into()),
        },
        Request::Connect {
            ssid,
            security,
            credential,
        } => {
            let id = network_id(ssid, security);
            let credential = if security == Security::Open {
                Credential::None
            } else {
                Credential::Password(credential)
            };
            if saved.store(id.clone(), credential).await.is_err() {
                return Reply::Error("could not save network".into());
            }
            let Some(target) = selector
                .find_and_select_connection_candidate(
                    Some(id.clone()),
                    ConnectReason::FidlConnectRequest,
                )
                .await
            else {
                return Reply::Error("saved network is not visible".into());
            };
            let selection = ConnectSelection {
                target,
                reason: ConnectReason::FidlConnectRequest,
            };
            if machine
                .as_ref()
                .is_some_and(|value| value.client.is_alive())
            {
                let (disconnected_tx, disconnected_rx) = oneshot::channel();
                if machine
                    .as_mut()
                    .unwrap()
                    .client
                    .disconnect(types::DisconnectReason::FidlConnectRequest, disconnected_tx)
                    .is_err()
                    || disconnected_rx.await.is_err()
                {
                    return Reply::Error("connection policy unavailable".into());
                }
            }
            *current = None;
            *machine = Some(start_machine(
                control.clone(),
                saved.clone(),
                telemetry,
                selection,
            ));
            // The target belongs to this fresh state-machine generation. Keep
            // it across a bounded request wait so a later policy retry cannot
            // report Connected without the identity it is connecting to.
            *current = Some(id);
            wait_for_connection(machine.as_ref().unwrap()).await
        }
        Request::Status => Reply::Status(policy_status(machine.as_ref(), current)),
        Request::Disconnect => {
            let Some(active) = machine.as_mut().filter(|value| value.client.is_alive()) else {
                *current = None;
                return Reply::Ok;
            };
            let (tx, rx) = oneshot::channel();
            if active
                .client
                .disconnect(
                    types::DisconnectReason::FidlStopClientConnectionsRequest,
                    tx,
                )
                .is_err()
            {
                return Reply::Error("disconnect policy unavailable".into());
            }
            if rx.await.is_err() {
                return Reply::Error("disconnect failed".into());
            }
            *current = None;
            Reply::Ok
        }
        Request::Saved => {
            let values = saved
                .get_networks()
                .await
                .into_iter()
                .filter_map(|config| {
                    config_security(config.security_type)
                        .map(|security| (config.ssid.to_vec(), security))
                })
                .take(64)
                .collect();
            Reply::Saved(values)
        }
        Request::Forget { ssid, security } => {
            let id = network_id(ssid, security);
            if current.as_ref() == Some(&id) {
                if let Some(active) = machine.as_mut().filter(|value| value.client.is_alive()) {
                    let (tx, rx) = oneshot::channel();
                    if active
                        .client
                        .disconnect(types::DisconnectReason::NetworkUnsaved, tx)
                        .is_ok()
                    {
                        let _ = rx.await;
                    }
                }
                *current = None;
            }
            match saved.remove(id).await {
                Ok(true) => Reply::Ok,
                Ok(false) => Reply::Error("network was not saved".into()),
                Err(_) => Reply::Error("could not forget network".into()),
            }
        }
    }
}

async fn wait_for_connection(machine: &Machine) -> Reply {
    let mut saw_progress = false;
    for _ in 0..500 {
        match machine.status.read_status() {
            Ok(state_machine::Status::Connected { .. }) => return Reply::Ok,
            Ok(state_machine::Status::Connecting | state_machine::Status::Disconnecting) => {
                saw_progress = true
            }
            Ok(state_machine::Status::Disconnected)
                if saw_progress && !machine.client.is_alive() =>
            {
                return Reply::Error("connection failed".into());
            }
            Err(_) => return Reply::Error("connection status unavailable".into()),
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Reply::Error("connection timed out".into())
}

fn policy_status(machine: Option<&Machine>, current: &Option<NetworkIdentifier>) -> Status {
    let association = machine
        .and_then(|value| value.status.read_status().ok())
        .map_or(Association::Disconnected, |status| match status {
            state_machine::Status::Disconnected => Association::Disconnected,
            state_machine::Status::Disconnecting => Association::Disconnecting,
            state_machine::Status::Connecting => Association::Connecting,
            state_machine::Status::Connected { channel, rssi, snr } => Association::Connected {
                channel,
                rssi_dbm: rssi,
                snr_db: snr,
            },
        });
    Status {
        association,
        ssid: current.as_ref().map(|id| id.ssid.to_vec()),
    }
}

fn network_id(ssid: Vec<u8>, security: Security) -> NetworkIdentifier {
    NetworkIdentifier::new(
        types::Ssid::from_bytes_unchecked(ssid),
        match security {
            Security::Open => SecurityType::None,
            Security::Wpa2 => SecurityType::Wpa2,
            Security::Wpa3 => SecurityType::Wpa3,
        },
    )
}
fn config_security(security: SecurityType) -> Option<Security> {
    match security {
        SecurityType::None => Some(Security::Open),
        SecurityType::Wpa2 => Some(Security::Wpa2),
        SecurityType::Wpa3 => Some(Security::Wpa3),
        _ => None,
    }
}
fn scan_security(security: sme::Protection) -> Option<Security> {
    match security {
        sme::Protection::Open => Some(Security::Open),
        sme::Protection::Wpa2Personal
        | sme::Protection::Wpa1Wpa2Personal
        | sme::Protection::Wpa2PersonalTkipOnly
        | sme::Protection::Wpa1Wpa2PersonalTkipOnly => Some(Security::Wpa2),
        sme::Protection::Wpa3Personal | sme::Protection::Wpa2Wpa3Personal => Some(Security::Wpa3),
        _ => None,
    }
}
