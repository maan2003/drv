// SPDX-License-Identifier: GPL-2.0-only

//! Long-lived single-interface wlancfg policy service.
//!
//! Saved credentials, selection, reconnect, and all SME control remain here.
//! Applications use the bounded application contract and never bypass this
//! policy owner to reach the Wi-Fi service.

use crate::{
    HostControlClient, ParkedHostControlClient,
    application::{
        ApplicationCommand, Association, Network, ParkedApplicationServer, PowerSaveMode, Reply,
        Request, Security, Status,
    },
};
use anyhow::Context as _;
use async_trait::async_trait;
use fidl_fuchsia_wlan_sme as sme;
use futures::{
    FutureExt as _, StreamExt as _,
    channel::{mpsc, oneshot},
};
use std::{
    cell::{Cell, RefCell},
    fs::File,
    os::fd::OwnedFd,
    rc::Rc,
    sync::Arc,
    time::Duration,
};
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
}

#[async_trait(?Send)]
impl ScanRequestApi for ControlScan {
    async fn perform_scan(
        &self,
        deadline: wlan_control_wire::MonotonicDeadline,
        _reason: ScanReason,
        ssids: Vec<types::Ssid>,
        channels: Vec<types::WlanChan>,
    ) -> Result<Vec<types::ScanResult>, types::ScanError> {
        let channels = channels
            .into_iter()
            .map(|channel| channel.primary)
            .collect::<Vec<_>>();
        let target_ssids = ssids;
        // The production MT7921 boundary advertises passive offload only.
        // Preserve directed-selection semantics by filtering the returned
        // passive observations below; never turn an SSID hint into probe TX.
        let request = sme::ScanRequest::Passive(sme::PassiveScanRequest { channels });
        let results =
            self.control
                .scan(deadline, &request)
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
    network: NetworkIdentifier,
}

#[derive(Default)]
struct PolicyState {
    desired: Option<NetworkIdentifier>,
    machine: Option<Machine>,
}

fn start_machine(
    control: HostControlClient,
    saved: Arc<dyn SavedNetworksManagerApi>,
    telemetry: TelemetrySender,
    selection: ConnectSelection,
) -> Machine {
    let network = selection.target.network.clone();
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
    Machine {
        client,
        status,
        network,
    }
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
        });
        let inspector = fuchsia_inspect::Inspector::default();
        let selector = ConnectionSelector::new(
            saved.clone(),
            scan.clone(),
            inspector.root().create_child("selection"),
            telemetry.clone(),
        );
        let state = RefCell::new(PolicyState::default());
        let cancelled = Cell::new(None);
        let startup_deadline =
            wlan_control_wire::MonotonicDeadline::after(Duration::from_secs(30))?;
        let startup = async {
            if let Some(target) = selector
                .find_and_select_connection_candidate(
                    startup_deadline,
                    None,
                    ConnectReason::IdleInterfaceAutoconnect,
                )
                .await
                && cancelled.get().is_none()
            {
                state.borrow_mut().desired = Some(target.network.clone());
                state.borrow_mut().machine = Some(start_machine(
                    control.clone(),
                    saved.clone(),
                    telemetry.clone(),
                    ConnectSelection {
                        deadline: startup_deadline,
                        target,
                        reason: ConnectReason::IdleInterfaceAutoconnect,
                    },
                ));
            }
            Reply::Ok
        };
        let (_, mut queued) =
            drive_operation(startup, startup_deadline, &mut commands, &state, &cancelled).await;
        loop {
            let command = match queued.take() {
                Some(command) => command,
                None => match commands.next().await {
                    Some(command) => command,
                    None => break,
                },
            };
            cancelled.set(None);
            let operation = handle(
                command.request,
                command.deadline,
                &control,
                scan.as_ref(),
                &selector,
                saved.clone(),
                telemetry.clone(),
                &state,
                &cancelled,
            );
            let (reply, next) = drive_operation(
                operation,
                command.deadline,
                &mut commands,
                &state,
                &cancelled,
            )
            .await;
            let _ = command.responder.send(reply);
            queued = next;
        }
        Ok(())
    })
}

/// Keep one mutating operation alive until completion. Cancelling intent does
/// not drop its transport future: scan/connect replies must still be drained.
async fn drive_operation(
    operation: impl std::future::Future<Output = Reply>,
    deadline: wlan_control_wire::MonotonicDeadline,
    commands: &mut mpsc::Receiver<ApplicationCommand>,
    state: &RefCell<PolicyState>,
    cancelled: &Cell<Option<wlan_control_wire::MonotonicDeadline>>,
) -> (Reply, Option<ApplicationCommand>) {
    let operation = operation.fuse();
    futures::pin_mut!(operation);
    let mut queued = None;
    loop {
        futures::select_biased! {
            reply = operation => return (reply, queued),
            command = commands.next().fuse() => match command {
                Some(command) => match command.request {
                    Request::Status => {
                        let _ = command.responder.send(Reply::Status(policy_status(&state.borrow())));
                    }
                    Request::Disconnect => {
                        if cancelled.get().is_none() {
                            cancelled.set(Some(command.deadline));
                        }
                        state.borrow_mut().desired = None;
                        if let Some(previous) = queued.replace(command) {
                            let _ = previous.responder.send(Reply::Error("superseded by disconnect".into()));
                        }
                    }
                    _ if queued.is_none() => queued = Some(command),
                    _ => {
                        let _ = command.responder.send(Reply::Error("policy operation busy".into()));
                    }
                },
                None => {
                    if cancelled.get().is_none() {
                        cancelled.set(Some(deadline));
                    }
                    return (operation.await, queued);
                }
            },
        }
    }
}

async fn disconnect_machine(
    state: &RefCell<PolicyState>,
    deadline: wlan_control_wire::MonotonicDeadline,
    reason: types::DisconnectReason,
) -> Reply {
    let receiver = {
        let mut state = state.borrow_mut();
        let Some(active) = state
            .machine
            .as_mut()
            .filter(|value| value.client.is_alive())
        else {
            return Reply::Ok;
        };
        let (tx, rx) = oneshot::channel();
        if active.client.disconnect(deadline, reason, tx).is_err() {
            return Reply::Error("disconnect policy unavailable".into());
        }
        rx
    };
    match receiver.await {
        Ok(()) => Reply::Ok,
        Err(_) => Reply::Error("disconnect failed".into()),
    }
}

async fn handle(
    request: Request,
    deadline: wlan_control_wire::MonotonicDeadline,
    control: &HostControlClient,
    scan: &ControlScan,
    selector: &ConnectionSelector,
    saved: Arc<dyn SavedNetworksManagerApi>,
    telemetry: TelemetrySender,
    state: &RefCell<PolicyState>,
    cancelled: &Cell<Option<wlan_control_wire::MonotonicDeadline>>,
) -> Reply {
    if deadline
        .remaining(wlan_control_wire::monotonic_time_ns().unwrap_or(u64::MAX))
        .is_none()
    {
        return Reply::Error("policy operation deadline exceeded".into());
    }
    match request {
        Request::Scan => match scan
            .perform_scan(deadline, ScanReason::ClientRequest, vec![], vec![])
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
            state.borrow_mut().desired = Some(id.clone());
            let credential = if security == Security::Open {
                Credential::None
            } else {
                Credential::Password(credential)
            };
            if saved.store(id.clone(), credential).await.is_err() {
                return Reply::Error("could not save network".into());
            }
            if cancelled.get().is_some() {
                return Reply::Error("connection cancelled".into());
            }
            // This adapter cannot scan while joined. Let the Fuchsia machine
            // acknowledge disconnect before selection, retaining the original
            // deadline and never treating failed teardown as quiescence.
            let disconnected =
                disconnect_machine(state, deadline, types::DisconnectReason::FidlConnectRequest)
                    .await;
            if disconnected != Reply::Ok {
                return disconnected;
            }
            if cancelled.get().is_some() {
                return Reply::Error("connection cancelled".into());
            }
            let Some(target) = selector
                .find_and_select_connection_candidate(
                    deadline,
                    Some(id.clone()),
                    ConnectReason::FidlConnectRequest,
                )
                .await
            else {
                return Reply::Error("saved network is not visible".into());
            };
            let selection = ConnectSelection {
                deadline,
                target,
                reason: ConnectReason::FidlConnectRequest,
            };
            if cancelled.get().is_some() {
                return Reply::Error("connection cancelled".into());
            }
            state.borrow_mut().machine = Some(start_machine(
                control.clone(),
                saved.clone(),
                telemetry,
                selection,
            ));
            wait_for_connection(state, cancelled).await
        }
        Request::Status => Reply::Status(policy_status(&state.borrow())),
        Request::PowerSave(mode) => {
            let mode = match mode {
                PowerSaveMode::Performance => wlan_control_wire::PowerSaveMode::Performance,
                PowerSaveMode::Balanced => wlan_control_wire::PowerSaveMode::Balanced,
            };
            match control.set_power_save(deadline, mode).await {
                Ok(()) => Reply::Ok,
                Err(error) => Reply::Error(error.to_string()),
            }
        }
        Request::Disconnect => {
            state.borrow_mut().desired = None;
            disconnect_machine(
                state,
                deadline,
                types::DisconnectReason::FidlStopClientConnectionsRequest,
            )
            .await
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
            let is_current = {
                let state = state.borrow();
                state.desired.as_ref() == Some(&id)
                    || state
                        .machine
                        .as_ref()
                        .is_some_and(|machine| machine.network == id)
            };
            if is_current {
                state.borrow_mut().desired = None;
                let disconnected =
                    disconnect_machine(state, deadline, types::DisconnectReason::NetworkUnsaved)
                        .await;
                if disconnected != Reply::Ok {
                    return disconnected;
                }
            }
            match saved.remove(id).await {
                Ok(true) => Reply::Ok,
                Ok(false) => Reply::Error("network was not saved".into()),
                Err(_) => Reply::Error("could not forget network".into()),
            }
        }
    }
}

async fn wait_for_connection(
    state: &RefCell<PolicyState>,
    cancelled: &Cell<Option<wlan_control_wire::MonotonicDeadline>>,
) -> Reply {
    loop {
        if let Some(cancel_deadline) = cancelled.get() {
            // Keep the state machine and transport/event consumer alive until
            // disconnect acknowledges quiescence, before completing Connect.
            let disconnected = disconnect_machine(
                state,
                cancel_deadline,
                types::DisconnectReason::FidlStopClientConnectionsRequest,
            )
            .await;
            return if disconnected == Reply::Ok {
                Reply::Error("connection cancelled".into())
            } else {
                disconnected
            };
        }
        {
            let state = state.borrow();
            let machine = state.machine.as_ref().expect("connecting machine");
            match machine.status.read_status() {
                Ok(state_machine::Status::Connected { .. }) => return Reply::Ok,
                Ok(state_machine::Status::Disconnected) if !machine.client.is_alive() => {
                    return Reply::Error("connection failed".into());
                }
                Err(_) => return Reply::Error("connection status unavailable".into()),
                _ => {}
            }
        }
        // The actual connection operation owns its timeout. Do not report a
        // second, shorter timeout while its state machine is still connecting.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn policy_status(state: &PolicyState) -> Status {
    let association = state
        .machine
        .as_ref()
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
    let ssid = if association == Association::Disconnected {
        None
    } else {
        state
            .machine
            .as_ref()
            .map(|machine| machine.network.ssid.to_vec())
    };
    Status { association, ssid }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc as sync_mpsc;

    #[test]
    fn reconnect_selection_waits_for_successful_disconnect() {
        use crate::tests::{GENERATION, receive, send_packet, sockets};
        use std::os::fd::AsRawFd as _;
        use wlan_control_wire::{Message, Reply as WireReply};

        for disconnect_succeeds in [false, true] {
            let directory = std::env::temp_dir().join(format!(
                "wlancfg-reconnect-order-{}-{disconnect_succeeds}",
                std::process::id()
            ));
            std::fs::create_dir(&directory).unwrap();
            futures::executor::block_on(async {
                let (telemetry_tx, _telemetry_rx) = mpsc::channel(100);
                let telemetry = TelemetrySender::new(telemetry_tx);
                let saved: Arc<dyn SavedNetworksManagerApi> = Arc::new(
                    SavedNetworksManager::new_with_directory(
                        File::open(&directory).unwrap(),
                        telemetry.clone(),
                    )
                    .await
                    .unwrap(),
                );
                let (client_fd, server_fd) = sockets();
                let control =
                    crate::PreparedHostControlClient::from_inherited_socket(client_fd, GENERATION)
                        .unwrap()
                        .spawn_parked_after_setup()
                        .unwrap()
                        .activate_after_persistence()
                        .unwrap();
                let scan = Arc::new(ControlScan {
                    control: control.clone(),
                });
                let inspector = fuchsia_inspect::Inspector::default();
                let selector = ConnectionSelector::new(
                    saved.clone(),
                    scan.clone(),
                    inspector.root().create_child("selection"),
                    telemetry.clone(),
                );
                let (tx, mut requests) = mpsc::channel(4);
                let (publisher, status) = status_publisher_and_reader();
                publisher.publish_status(state_machine::Status::Connected {
                    channel: 6,
                    rssi: -40,
                    snr: 30,
                });
                let state = RefCell::new(PolicyState {
                    desired: None,
                    machine: Some(Machine {
                        client: state_machine::Client::new(tx),
                        status,
                        network: network_id(b"old-peer".to_vec(), Security::Open),
                    }),
                });
                let cancelled = Cell::new(None);
                let deadline =
                    wlan_control_wire::MonotonicDeadline::after(Duration::from_secs(30)).unwrap();
                let mut connecting = Box::pin(handle(
                    Request::Connect {
                        ssid: b"new-peer".to_vec(),
                        security: Security::Open,
                        credential: vec![],
                    },
                    deadline,
                    &control,
                    scan.as_ref(),
                    &selector,
                    saved,
                    telemetry,
                    &state,
                    &cancelled,
                ));
                assert!(connecting.as_mut().now_or_never().is_none());
                let state_machine::ManualRequest::Disconnect((received, _, ack)) = requests
                    .try_recv()
                    .expect("disconnect must precede selection")
                else {
                    panic!("expected disconnect");
                };
                assert_eq!(received, deadline);
                assert!(connecting.as_mut().now_or_never().is_none());
                if !disconnect_succeeds {
                    drop(ack);
                    assert_eq!(connecting.await, Reply::Error("disconnect failed".into()));
                    // Failure must not issue a scan or a replacement connect.
                    let mut byte = 0u8;
                    assert_eq!(
                        unsafe {
                            libc::recv(
                                server_fd.as_raw_fd(),
                                (&mut byte as *mut u8).cast(),
                                1,
                                libc::MSG_DONTWAIT,
                            )
                        },
                        -1
                    );
                    assert_eq!(
                        std::io::Error::last_os_error().kind(),
                        std::io::ErrorKind::WouldBlock
                    );
                } else {
                    publisher.publish_status(state_machine::Status::Disconnected);
                    ack.send(()).unwrap();
                    let peer = std::thread::spawn(move || {
                        let packet = receive(server_fd.as_raw_fd());
                        assert!(matches!(packet.message, Message::Scan { deadline: d, .. }
                            if d == deadline));
                        send_packet(
                            server_fd.as_raw_fd(),
                            1,
                            Message::ScanReply(WireReply {
                                in_reply_to: packet.request_id,
                                result: Err(sme::ScanErrorCode::InternalError),
                            }),
                            &[],
                        );
                    });
                    assert_eq!(
                        connecting.await,
                        Reply::Error("saved network is not visible".into())
                    );
                    peer.join().unwrap();
                }
            });
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn status_and_disconnect_progress_without_dropping_the_active_operation() {
        let state = RefCell::new(PolicyState::default());
        let cancelled = Cell::new(None);
        let (mut commands, mut incoming) = mpsc::channel(4);
        let (complete, completion) = oneshot::channel();
        let operation = async { completion.await.unwrap() };
        let mut driving = Box::pin(drive_operation(
            operation,
            wlan_control_wire::MonotonicDeadline::after(Duration::from_secs(30)).unwrap(),
            &mut incoming,
            &state,
            &cancelled,
        ));
        let (status_tx, status_rx) = sync_mpsc::sync_channel(1);
        commands
            .try_send(ApplicationCommand {
                deadline: wlan_control_wire::MonotonicDeadline::after(Duration::from_secs(30))
                    .unwrap(),
                request: Request::Status,
                responder: status_tx,
            })
            .unwrap();
        assert!(driving.as_mut().now_or_never().is_none());
        assert!(matches!(status_rx.try_recv().unwrap(), Reply::Status(_)));

        let (disconnect_tx, disconnect_rx) = sync_mpsc::sync_channel(1);
        commands
            .try_send(ApplicationCommand {
                deadline: wlan_control_wire::MonotonicDeadline::after(Duration::from_secs(30))
                    .unwrap(),
                request: Request::Disconnect,
                responder: disconnect_tx,
            })
            .unwrap();
        assert!(driving.as_mut().now_or_never().is_none());
        assert!(cancelled.get().is_some());
        assert!(
            disconnect_rx.try_recv().is_err(),
            "receipt is not quiescence"
        );
        let first_cancel_deadline = cancelled.get().unwrap();
        let (repeat_tx, _repeat_rx) = sync_mpsc::sync_channel(1);
        commands
            .try_send(ApplicationCommand {
                deadline: first_cancel_deadline
                    .checked_add(Duration::from_secs(5))
                    .unwrap(),
                request: Request::Disconnect,
                responder: repeat_tx,
            })
            .unwrap();
        assert!(driving.as_mut().now_or_never().is_none());
        assert_eq!(
            cancelled.get(),
            Some(first_cancel_deadline),
            "repeated cancel extended cleanup"
        );
        complete.send(Reply::Ok).unwrap();
        let (reply, queued) = futures::executor::block_on(driving);
        assert_eq!(reply, Reply::Ok);
        assert!(matches!(queued.unwrap().request, Request::Disconnect));
    }

    #[test]
    fn actual_status_identity_does_not_follow_new_desired_network() {
        let (tx, _rx) = mpsc::channel(4);
        let (publisher, status) = status_publisher_and_reader();
        publisher.publish_status(state_machine::Status::Connected {
            channel: 6,
            rssi: -40,
            snr: 30,
        });
        let state = PolicyState {
            desired: Some(network_id(b"new-intent".to_vec(), Security::Wpa3)),
            machine: Some(Machine {
                client: state_machine::Client::new(tx),
                status,
                network: network_id(b"actual-peer".to_vec(), Security::Wpa3),
            }),
        };
        assert_eq!(policy_status(&state).ssid, Some(b"actual-peer".to_vec()));
        publisher.publish_status(state_machine::Status::Disconnected);
        assert_eq!(policy_status(&state).ssid, None);
    }

    #[test]
    fn cancelled_connect_waits_for_state_machine_disconnect_acknowledgment() {
        let (tx, mut requests) = mpsc::channel(4);
        let (publisher, status) = status_publisher_and_reader();
        publisher.publish_status(state_machine::Status::Connecting);
        let state = RefCell::new(PolicyState {
            desired: None,
            machine: Some(Machine {
                client: state_machine::Client::new(tx),
                status,
                network: network_id(b"ap".to_vec(), Security::Wpa3),
            }),
        });
        let cancelled = Cell::new(Some(
            wlan_control_wire::MonotonicDeadline::after(Duration::from_secs(30)).unwrap(),
        ));
        let mut waiting = Box::pin(wait_for_connection(&state, &cancelled));
        assert!(waiting.as_mut().now_or_never().is_none());
        let state_machine::ManualRequest::Disconnect((_, _, ack)) = requests.try_recv().unwrap()
        else {
            panic!("cancellation must explicitly disconnect");
        };
        assert!(waiting.as_mut().now_or_never().is_none());
        ack.send(()).unwrap();
        assert_eq!(
            futures::executor::block_on(waiting),
            Reply::Error("connection cancelled".into())
        );
    }
}
