// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host build wiring for the pinned wlancfg connection selector.
//!
//! The modules containing selection, scoring, filtering, augmentation, and
//! cancellation are the pinned production sources.  The modules below provide
//! only the effect and value dependencies that the host closure does not yet
//! package.

pub mod fidl_fuchsia_wlan_policy {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum ConnectionState {
        Failed,
        Disconnected,
        Connecting,
        Connected,
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum WlanClientState {
        ConnectionsDisabled,
        ConnectionsEnabled,
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum DisconnectStatus {
        ConnectionFailed,
        ConnectionStopped,
        CredentialsFailed,
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Compatibility {
        Supported,
        DisallowedNotSupported,
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum ScanErrorCode {
        GeneralError,
        Cancelled,
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum NetworkConfigChangeError {
        GeneralError,
        InvalidSecurityCredentialError,
        CredentialLenError,
        SsidEmptyError,
        NetworkConfigMissingFieldError,
        UnsupportedCredentialError,
        NetworkConfigWriteError,
    }
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub enum SecurityType {
        None,
        Wep,
        Wpa,
        Wpa2,
        Wpa3,
    }
    #[derive(Clone, Eq, PartialEq)]
    pub enum Credential {
        None(Empty),
        Password(Vec<u8>),
        Psk(Vec<u8>),
    }
    impl std::fmt::Debug for Credential {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::None(_) => f.write_str("None"),
                Self::Password(_) => f.write_str("Password(<redacted>)"),
                Self::Psk(_) => f.write_str("Psk(<redacted>)"),
            }
        }
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Empty;
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct NetworkIdentifier {
        pub ssid: Vec<u8>,
        pub type_: SecurityType,
    }
    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct NetworkConfig {
        pub id: Option<NetworkIdentifier>,
        pub credential: Option<Credential>,
    }
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Bss;
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ScanResult;
}

pub mod wlan_metrics_registry {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum PolicyConnectionAttemptMigratedMetricDimensionReason {
        FidlConnectRequest,
        ProactiveNetworkSwitch,
        IdleInterfaceAutoconnect,
        RetryAfterFailedConnectAttempt,
        RetryAfterDisconnectDetected,
        RegulatoryChangeReconnect,
        NewSavedNetworkAutoconnect,
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum PolicyDisconnectionMigratedMetricDimensionReason {
        Unknown,
        FailedToConnect,
        FidlConnectRequest,
        FidlStopClientConnectionsRequest,
        ProactiveNetworkSwitch,
        DisconnectDetectedFromSme,
        RegulatoryRegionChange,
        Startup,
        NetworkUnsaved,
        NetworkConfigUpdated,
    }
}

pub mod regulatory_manager {
    pub type CountryCode = [u8; 2];
}

pub mod util {
    #[path = "historical_list.rs"]
    pub mod historical_list;
    #[path = "pseudo_energy.rs"]
    pub mod pseudo_energy;

    #[path = "state_machine.rs"]
    pub mod state_machine;

    pub mod listener {
        use crate::client::types;
        use futures::channel::mpsc;

        #[derive(Clone, PartialEq)]
        pub struct ClientNetworkState {
            pub id: types::NetworkIdentifier,
            pub state: types::ConnectionState,
            pub status: Option<types::DisconnectStatus>,
        }

        #[derive(Clone, PartialEq)]
        pub struct ClientStateUpdate {
            pub state: crate::fidl_fuchsia_wlan_policy::WlanClientState,
            pub networks: Vec<ClientNetworkState>,
        }

        pub enum Message {
            NotifyListeners(ClientStateUpdate),
        }

        pub type ClientListenerMessageSender = mpsc::UnboundedSender<Message>;
    }
}

pub mod telemetry {
    use crate::client::roaming::lib::PolicyRoamRequest;
    use crate::client::types::{self, ConnectReason, ScannedCandidate};
    use crate::util::historical_list::HistoricalList;
    use futures::channel::mpsc;

    pub const AVERAGE_SCORE_DELTA_MINIMUM_DURATION: zx::MonotonicDuration =
        zx::MonotonicDuration::from_seconds(30);
    pub const METRICS_SHORT_CONNECT_DURATION: zx::MonotonicDuration =
        zx::MonotonicDuration::from_seconds(90);

    #[derive(Clone)]
    pub struct DisconnectInfo {
        pub iface_id: u16,
        pub connected_duration: zx::MonotonicDuration,
        pub is_sme_reconnecting: bool,
        pub disconnect_source: fidl_fuchsia_wlan_sme::DisconnectSource,
        pub previous_connect_reason: types::ConnectReason,
        pub ap_state: types::ApState,
        pub signals: HistoricalList<types::TimestampedSignal>,
    }

    #[derive(Default)]
    pub struct ScanEventInspectData {
        pub unknown_protection_ies: Vec<String>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum NetworkSelectionType {
        Directed,
        Undirected,
    }

    #[derive(Clone)]
    pub struct TelemetrySender(mpsc::Sender<TelemetryEvent>);
    impl TelemetrySender {
        pub fn new(sender: mpsc::Sender<TelemetryEvent>) -> Self {
            Self(sender)
        }
        pub fn send(&self, event: TelemetryEvent) {
            let _ = self.0.clone().try_send(event);
        }
    }

    #[derive(Clone)]
    pub enum TelemetryEvent {
        NetworkSelectionScanInterval {
            time_since_last_scan: zx::MonotonicDuration,
        },
        ActiveScanRequested {
            num_ssids_requested: usize,
        },
        NetworkSelectionDecision {
            network_selection_type: NetworkSelectionType,
            num_candidates: Result<usize, ()>,
            selected_count: usize,
        },
        BssSelectionResult {
            reason: ConnectReason,
            scored_candidates: Vec<(ScannedCandidate, i16)>,
            selected_candidate: Option<(ScannedCandidate, i16)>,
        },
        ConnectionSelectionScanResults {
            saved_network_count: usize,
            bss_count_per_saved_network: Vec<usize>,
            saved_network_count_found_by_active_scan: usize,
        },
        ConnectResult {
            iface_id: u16,
            result: fidl_fuchsia_wlan_sme::ConnectResult,
            policy_connect_reason: Option<types::ConnectReason>,
            multiple_bss_candidates: bool,
            ap_state: types::ApState,
            network_is_likely_hidden: bool,
        },
        Disconnected {
            track_subsequent_downtime: bool,
            info: Option<DisconnectInfo>,
        },
        PostConnectionSignals {
            connect_time: fuchsia_async::MonotonicInstant,
            signal_at_connect: types::Signal,
            signals: HistoricalList<types::TimestampedSignal>,
        },
        LongDurationSignals {
            signals: Vec<types::TimestampedSignal>,
        },
        OnSignalReport {
            ind: fidl_fuchsia_wlan_internal::SignalReportIndication,
        },
        OnChannelSwitched {
            info: fidl_fuchsia_wlan_internal::ChannelSwitchInfo,
        },
        PolicyRoamAttempt {
            request: PolicyRoamRequest,
            connected_duration: zx::MonotonicDuration,
        },
        PolicyInitiatedRoamResult {
            iface_id: u16,
            result: fidl_fuchsia_wlan_sme::RoamResult,
            updated_ap_state: types::ApState,
            original_ap_state: Box<types::ApState>,
            request: Box<PolicyRoamRequest>,
            request_time: fuchsia_async::MonotonicInstant,
            result_time: fuchsia_async::MonotonicInstant,
        },
        SavedNetworkCount {
            saved_network_count: usize,
            config_count_per_saved_network: Vec<usize>,
        },

    }
}

pub mod config_management {
    #[path = "config_manager.rs"]
    mod config_manager;
    #[path = "network_config.rs"]
    pub mod network_config;
    pub use config_manager::*;
    pub use network_config::*;
}

#[path = "host_saved_networks.rs"]
mod host_saved_networks;
pub use host_saved_networks::PersistenceError as SavedNetworksPersistenceError;

pub mod client {
    #[path = "types.rs"]
    pub mod types;

    pub mod scan {
        use super::types;
        use async_trait::async_trait;

        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum ScanReason {
            ClientRequest,
            NetworkSelection,
            BssSelection,
            BssSelectionAugmentation,
            RoamSearch,
        }

        #[async_trait(?Send)]
        pub trait ScanRequestApi {
            async fn perform_scan(
                &self,
                scan_reason: ScanReason,
                ssids: Vec<types::Ssid>,
                channels: Vec<types::WlanChan>,
            ) -> Result<Vec<types::ScanResult>, types::ScanError>;
        }

        #[path = "selection_conversion.rs"]
        mod selection_conversion;

        /// Narrow host entry into the exact pinned production conversion above.
        pub fn selection_scan_results(
            results: Vec<wlan_common::scan::ScanResult>,
            target_ssids: &[types::Ssid],
        ) -> Vec<types::ScanResult> {
            selection_conversion::network_map_to_scan_result(
                selection_conversion::bss_to_network_map(
                    results,
                    target_ssids,
                    &mut crate::telemetry::ScanEventInspectData::default(),
                ),
            )
        }
    }

    #[path = "connection_selection/mod.rs"]
    pub mod connection_selection;

    pub mod roaming {
        pub mod lib {
            use crate::client::types;
            use wlan_common::sequestered::Sequestered;

            pub const ROAMING_CHANNEL_BUFFER_SIZE: usize = 100;

            #[derive(Clone)]
            pub enum RoamReason {
                RssiBelowThreshold,
                SnrBelowThreshold,
            }

            #[derive(Clone)]
            pub struct PolicyRoamRequest {
                pub candidate: types::ScannedCandidate,
                pub reasons: Vec<RoamReason>,
            }

            impl From<PolicyRoamRequest> for fidl_fuchsia_wlan_sme::RoamRequest {
                fn from(request: PolicyRoamRequest) -> Self {
                    Self {
                        bss_description: Sequestered::release(
                            request.candidate.bss.bss_description,
                        ),
                    }
                }
            }
        }

        pub mod roam_monitor {
            use futures::channel::mpsc;

            #[derive(Clone)]
            pub struct RoamDataSender(
                mpsc::Sender<fidl_fuchsia_wlan_internal::SignalReportIndication>,
            );

            impl RoamDataSender {
                pub fn new(
                    sender: mpsc::Sender<fidl_fuchsia_wlan_internal::SignalReportIndication>,
                ) -> Self {
                    Self(sender)
                }

                pub fn send_signal_report_ind(
                    &mut self,
                    ind: fidl_fuchsia_wlan_internal::SignalReportIndication,
                ) -> Result<
                    (),
                    mpsc::TrySendError<fidl_fuchsia_wlan_internal::SignalReportIndication>,
                > {
                    self.0.try_send(ind)
                }
            }
        }

        pub mod local_roam_manager {
            use super::lib::PolicyRoamRequest;
            use super::roam_monitor::RoamDataSender;
            use crate::client::types;
            use crate::config_management::Credential;
            use futures::channel::mpsc;

            pub struct RoamMonitorChannels {
                pub roam_request_sender: mpsc::Sender<PolicyRoamRequest>,
                pub signal_receiver:
                    mpsc::Receiver<fidl_fuchsia_wlan_internal::SignalReportIndication>,
            }

            #[derive(Clone)]
            pub struct RoamManager {
                sender: mpsc::UnboundedSender<RoamMonitorChannels>,
            }

            impl RoamManager {
                pub fn new(sender: mpsc::UnboundedSender<RoamMonitorChannels>) -> Self {
                    Self { sender }
                }

                pub fn initialize_roam_monitor(
                    &mut self,
                    _ap_state: types::ApState,
                    _network_identifier: types::NetworkIdentifier,
                    _credential: Credential,
                    roam_request_sender: mpsc::Sender<PolicyRoamRequest>,
                ) -> RoamDataSender {
                    let (signal_sender, signal_receiver) = mpsc::channel(100);
                    let _ = self.sender.unbounded_send(RoamMonitorChannels {
                        roam_request_sender,
                        signal_receiver,
                    });
                    RoamDataSender::new(signal_sender)
                }
            }
        }
    }

    #[path = "state_machine.rs"]
    pub mod state_machine;
}

pub mod mode_management {
    use async_trait::async_trait;
    use futures::stream::{Fuse, LocalBoxStream};

    pub type ConnectTransactionEventStream = Fuse<
        LocalBoxStream<
            'static,
            Result<fidl_fuchsia_wlan_sme::ConnectTransactionEvent, anyhow::Error>,
        >,
    >;
    pub type ClientSmeEventStream = Fuse<LocalBoxStream<'static, Result<(), anyhow::Error>>>;
    pub type ClientSmeScanResult =
        Result<fidl_fuchsia_wlan_sme::ScanResultVector, fidl_fuchsia_wlan_sme::ScanErrorCode>;

    #[async_trait(?Send)]
    pub trait ClientSmeTransport {
        async fn connect(
            &self,
            request: &fidl_fuchsia_wlan_sme::ConnectRequest,
        ) -> Result<
            (
                fidl_fuchsia_wlan_sme::ConnectResult,
                ConnectTransactionEventStream,
            ),
            anyhow::Error,
        >;
        async fn disconnect(
            &self,
            reason: fidl_fuchsia_wlan_sme::UserDisconnectReason,
        ) -> Result<(), anyhow::Error>;
        fn roam(&self, request: &fidl_fuchsia_wlan_sme::RoamRequest) -> Result<(), anyhow::Error>;
        async fn scan(
            &self,
            request: &fidl_fuchsia_wlan_sme::ScanRequest,
        ) -> Result<ClientSmeScanResult, anyhow::Error>;
        fn take_event_stream(&self) -> ClientSmeEventStream;
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum IfaceFailure {
        ConnectionFailure { iface_id: u16 },
    }
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum Defect {
        Iface(IfaceFailure),
    }

    pub mod iface_manager_api {
        use super::{
            ClientSmeEventStream, ClientSmeScanResult, ClientSmeTransport,
            ConnectTransactionEventStream,
        };
        use crate::client::types;
        use crate::config_management::Credential;
        use std::rc::Rc;

        #[derive(Clone)]
        pub struct ConnectAttemptRequest {
            pub network: types::NetworkIdentifier,
            pub credential: Credential,
            pub reason: types::ConnectReason,
            pub attempts: u8,
        }

        #[derive(Clone)]
        pub struct SmeForClientStateMachine(Rc<dyn ClientSmeTransport>);

        impl SmeForClientStateMachine {
            pub fn new(transport: Rc<dyn ClientSmeTransport>) -> Self {
                Self(transport)
            }
            pub async fn connect(
                &self,
                request: &fidl_fuchsia_wlan_sme::ConnectRequest,
            ) -> Result<
                (
                    fidl_fuchsia_wlan_sme::ConnectResult,
                    ConnectTransactionEventStream,
                ),
                anyhow::Error,
            > {
                self.0.connect(request).await
            }
            pub async fn disconnect(
                &self,
                reason: fidl_fuchsia_wlan_sme::UserDisconnectReason,
            ) -> Result<(), anyhow::Error> {
                self.0.disconnect(reason).await
            }
            pub fn roam(
                &self,
                request: &fidl_fuchsia_wlan_sme::RoamRequest,
            ) -> Result<(), anyhow::Error> {
                self.0.roam(request)
            }
            pub fn take_event_stream(&self) -> ClientSmeEventStream {
                self.0.take_event_stream()
            }
            pub fn sme_for_scan(&self) -> SmeForScan {
                SmeForScan(self.0.clone())
            }
        }

        #[derive(Clone)]
        pub struct SmeForScan(Rc<dyn ClientSmeTransport>);
        impl SmeForScan {
            pub async fn scan(
                &self,
                request: &fidl_fuchsia_wlan_sme::ScanRequest,
            ) -> Result<ClientSmeScanResult, anyhow::Error> {
                self.0.scan(request).await
            }
            pub fn log_aborted_scan_defect(&self) {}
            pub fn log_failed_scan_defect(&self) {}
            pub fn log_empty_scan_defect(&self) {}
        }
    }
}

/// Narrow handoff from wlancfg selection policy to the Wi-Fi service.
///
/// The command contains only the selected BSS and the authentication material
/// for this connection attempt.  Saved-network storage, candidate selection,
/// retry, and roaming policy remain on the wlancfg side; SME/MLME/RSN retain
/// the association and authentication state machines on the Wi-Fi side.
pub mod service_boundary {
    use crate::client::types::ScannedCandidate;
    use fidl_fuchsia_wlan_common::ScanType;
    use fidl_fuchsia_wlan_sme::ConnectRequest;
    use wlan_common::sequestered::Sequestered;

    pub struct WifiConnectCommand(ConnectRequest);

    impl WifiConnectCommand {
        /// Promote the output of the pinned selector into the sole connection
        /// command sent to the Wi-Fi service.
        pub fn from_selected(candidate: ScannedCandidate) -> Self {
            Self(ConnectRequest {
                ssid: candidate.network.ssid.to_vec(),
                bss_description: Sequestered::release(candidate.bss.bss_description),
                multiple_bss_candidates: candidate.network_has_multiple_bss,
                authentication: candidate.authenticator.into(),
                // Preserve pinned Fuchsia wlancfg state-machine behavior. This
                // legacy field does not describe how the winning BSS was seen.
                deprecated_scan_type: ScanType::Active,
            })
        }

        pub fn into_sme_request(self) -> ConnectRequest {
            self.0
        }
    }
}
