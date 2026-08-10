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
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum Credential {
        None(Empty),
        Password(Vec<u8>),
        Psk(Vec<u8>),
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
        Unknown,
        FidlConnectRequest,
        ProactiveNetworkSwitch,
        IdleInterfaceAutoconnect,
        RetryAfterFailedConnectAttempt,
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
}

pub mod telemetry {
    use crate::client::types::{ConnectReason, ScannedCandidate};
    use futures::channel::mpsc;

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
}

pub mod mode_management {
    pub mod iface_manager_api {
        use crate::client::types;
        use crate::config_management::Credential;

        #[derive(Clone)]
        pub struct ConnectAttemptRequest {
            pub network: types::NetworkIdentifier,
            pub credential: Credential,
            pub reason: types::ConnectReason,
            pub attempts: u8,
        }
    }
}
