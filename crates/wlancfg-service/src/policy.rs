// SPDX-License-Identifier: GPL-2.0-only

//! Single-interface wlancfg policy vertical slice.
//!
//! This is intentionally not an interface-manager monitor. It loads the
//! saved-network store, runs the pinned Fuchsia selector once for an idle
//! interface, and gives that exact selection to the pinned client state
//! machine for the lifetime of one Wi-Fi generation.

use crate::{HostControlClient, PreparedHostControlClient};
use anyhow::{Context as _, anyhow};
use async_trait::async_trait;
use fidl_fuchsia_wlan_sme as sme;
use futures::channel::mpsc;
use std::{fs::File, os::fd::OwnedFd, rc::Rc, sync::Arc};
use wlancfg_selection::{
    client::{
        connection_selection::{ConnectionSelector, ConnectionSelectorApi as _},
        roaming::local_roam_manager::RoamManager,
        scan::{ScanReason, ScanRequestApi, selection_scan_results},
        state_machine,
        types::{self, ConnectSelection},
    },
    config_management::{SavedNetworksManager, SavedNetworksManagerApi},
    mode_management::{ClientSmeTransport, Defect, iface_manager_api::SmeForClientStateMachine},
    telemetry::{TelemetryEvent, TelemetrySender},
    util::state_machine::status_publisher_and_reader,
    wlan_metrics_registry::PolicyConnectionAttemptMigratedMetricDimensionReason as ConnectReason,
};

struct ControlScan {
    control: HostControlClient,
}

#[async_trait(?Send)]
impl ScanRequestApi for ControlScan {
    async fn perform_scan(
        &self,
        _reason: ScanReason,
        ssids: Vec<types::Ssid>,
        channels: Vec<types::WlanChan>,
    ) -> Result<Vec<types::ScanResult>, types::ScanError> {
        let channels = channels
            .into_iter()
            .map(|channel| channel.primary)
            .collect();
        let target_ssids = ssids.clone();
        let request = if ssids.is_empty() {
            sme::ScanRequest::Passive(sme::PassiveScanRequest { channels })
        } else {
            sme::ScanRequest::Active(sme::ActiveScanRequest {
                ssids: ssids.into_iter().map(|ssid| ssid.to_vec()).collect(),
                channels,
            })
        };
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
                // Match pinned wlancfg scan semantics: a hostile malformed BSS
                // is dropped without suppressing other valid candidates.
                .filter_map(|result| result.try_into().ok())
                .collect();
        Ok(selection_scan_results(results, &target_ssids))
    }
}

/// Load persisted policy and serve one already-locked-down interface
/// generation. The caller must not invoke this before `LockedDown::run`.
pub fn serve_one_generation(
    prepared: PreparedHostControlClient,
    state_directory: OwnedFd,
) -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .context("construct wlancfg policy executor")?
        .block_on(async move {
            let (telemetry_tx, _telemetry_rx) = mpsc::channel::<TelemetryEvent>(100);
            let telemetry = TelemetrySender::new(telemetry_tx);
            // Construction performs all metadata validation, cleanup, file reads,
            // and parsing. Keeping it here is the startup order's security hinge.
            let saved: Arc<dyn SavedNetworksManagerApi> = Arc::new(
                SavedNetworksManager::new_with_directory(
                    File::from(state_directory),
                    telemetry.clone(),
                )
                .await
                .context("load saved networks")?,
            );
            // Start the first possible IPC receive only after persistence metadata,
            // stale-temp cleanup, load, and parsing have all completed.
            let control = prepared
                .start_after_lockdown()
                .context("start WLAN control owner")?;

            let scan: Arc<dyn ScanRequestApi> = Arc::new(ControlScan {
                control: control.clone(),
            });
            let inspector = fuchsia_inspect::Inspector::default();
            let selector = ConnectionSelector::new(
                saved.clone(),
                scan,
                inspector.root().create_child("selection"),
                telemetry.clone(),
            );
            let target = selector
                .find_and_select_connection_candidate(None, ConnectReason::IdleInterfaceAutoconnect)
                .await
                .ok_or_else(|| anyhow!("no saved network candidate found"))?;
            let selection = ConnectSelection {
                target,
                reason: ConnectReason::IdleInterfaceAutoconnect,
            };

            let event_stream = control.take_event_stream();
            let transport: Rc<dyn ClientSmeTransport> = Rc::new(control);
            let (request_tx, request_rx) = mpsc::channel(4);
            let _state_client = state_machine::Client::new(request_tx);
            let (listener_tx, _listener_rx) = mpsc::unbounded();
            let (defect_tx, _defect_rx) = mpsc::channel::<Defect>(10);
            let (roam_tx, _roam_rx) = mpsc::unbounded();
            let (status_tx, _status_rx) = status_publisher_and_reader();

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
            // Retain every receiver while the one-generation machine runs so its
            // bounded reporting channels remain live without inventing a system-UI
            // protocol for this vertical slice.
            machine.await;
            Ok(())
        })
}
