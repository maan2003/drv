// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

use fidl_fuchsia_wlan_common::{ScanType, SecuritySupport, SpectrumManagementSupport, WlanMacRole};
use fidl_fuchsia_wlan_ieee80211::{
    BssDescription, BssType, ChannelBandwidth, ChannelNumber, StatusCode, WlanBand,
};
use fidl_fuchsia_wlan_internal::{Authentication, Protocol};
use fidl_fuchsia_wlan_mlme::{
    AuthenticationTypes, BandCapability, ConnectConfirm, DeviceInfo, MlmeEvent, ScanEnd,
    ScanResultCode, ScanTypes,
};
use fidl_fuchsia_wlan_sme::{ConnectRequest, PassiveScanRequest, ScanRequest};
use ieee80211::MacAddrBytes as _;
use wlan_sme::client::{
    ClientConfig, ClientSme, ClientSmeStatus, ConnectResult, ConnectTransactionEvent,
};
use wlan_sme::{MlmeRequest, Station};

fn device_info() -> DeviceInfo {
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
    }
}

fn open_bss() -> BssDescription {
    BssDescription {
        bssid: [6, 5, 4, 3, 2, 1],
        bss_type: BssType::Infrastructure,
        beacon_period: 100,
        capability_info: 1,
        // SSID "open", followed by supported rates 1, 2, 5.5, and 11 Mbps.
        ies: vec![0, 4, b'o', b'p', b'e', b'n', 1, 4, 2, 4, 11, 22],
        primary: ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 1,
        },
        bandwidth: ChannelBandwidth::Cbw20,
        vht_secondary_80_channel: ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 0,
        },
        rssi_dbm: -40,
        snr_db: 30,
    }
}

#[test]
fn passive_scan_crosses_pinned_sme_mlme_boundary() {
    let inspector = fuchsia_inspect::Inspector::default();
    let inspect_node = inspector.root().create_child("sme");
    let (mut sme, _mlme_sink, mut mlme_stream, _time_stream) = ClientSme::new(
        ClientConfig::default(),
        device_info(),
        inspector,
        inspect_node,
        SecuritySupport::default(),
        SpectrumManagementSupport::default(),
    );
    assert_eq!(sme.status(), ClientSmeStatus::Idle);

    let mut result = sme.on_scan_command(ScanRequest::Passive(PassiveScanRequest {
        channels: vec![1],
    }));
    let request = mlme_stream
        .try_recv()
        .expect("SME must issue one MLME scan request");
    let scan = match request {
        MlmeRequest::Scan(scan) => scan,
        other => panic!("expected scan request, got {}", other.name()),
    };
    assert_eq!(scan.txn_id, 1);
    assert_eq!(scan.scan_type, ScanTypes::Passive);
    assert_eq!(
        scan.channel_list,
        vec![ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 1
        }]
    );

    Station::on_mlme_event(
        &mut sme,
        MlmeEvent::OnScanEnd {
            end: ScanEnd {
                txn_id: scan.txn_id,
                code: ScanResultCode::Success,
            },
        },
    );
    assert_eq!(result.try_recv(), Ok(Some(Ok(vec![]))));
    assert_eq!(sme.status(), ClientSmeStatus::Idle);
}

#[test]
fn open_connect_crosses_pinned_sme_mlme_boundary() {
    let inspector = fuchsia_inspect::Inspector::default();
    let inspect_node = inspector.root().create_child("sme");
    let (mut sme, _mlme_sink, mut mlme_stream, _time_stream) = ClientSme::new(
        ClientConfig::default(),
        device_info(),
        inspector,
        inspect_node,
        SecuritySupport::default(),
        SpectrumManagementSupport::default(),
    );
    let bss = open_bss();
    let mut transaction = sme.on_connect_command(ConnectRequest {
        ssid: b"open".to_vec(),
        bss_description: bss.clone(),
        multiple_bss_candidates: false,
        authentication: Authentication {
            protocol: Protocol::Open,
            credentials: None,
        },
        deprecated_scan_type: ScanType::Passive,
    });
    assert!(
        matches!(sme.status(), ClientSmeStatus::Connecting(ref ssid) if ssid.to_vec() == b"open")
    );

    let request = mlme_stream
        .try_recv()
        .expect("SME must issue one MLME connect request");
    let connect = match request {
        MlmeRequest::Connect(connect) => connect,
        other => panic!("expected connect request, got {}", other.name()),
    };
    assert_eq!(connect.selected_bss, bss);
    assert_eq!(connect.auth_type, AuthenticationTypes::OpenSystem);
    assert!(connect.security_ie.is_empty());

    Station::on_mlme_event(
        &mut sme,
        MlmeEvent::ConnectConf {
            resp: ConnectConfirm {
                peer_sta_address: bss.bssid,
                result_code: StatusCode::Success,
                association_id: 42,
                association_ies: vec![],
            },
        },
    );
    assert!(matches!(
        transaction.try_recv(),
        Ok(ConnectTransactionEvent::OnConnectResult {
            result: ConnectResult::Success,
            is_reconnect: false
        })
    ));
    assert!(
        matches!(sme.status(), ClientSmeStatus::Connected(ref ap) if ap.bssid.as_array() == &bss.bssid)
    );
}

#[test]
fn trusted_private_carrier_crosses_actual_scheduler_and_seals_once() {
    struct PrivateIndex(u64);

    let inspector = fuchsia_inspect::Inspector::default();
    let inspect_node = inspector.root().create_child("trusted-sme");
    let (mut sme, _mlme_sink, mut mlme_stream, _time_stream) = ClientSme::new(
        ClientConfig::default(),
        device_info(),
        inspector,
        inspect_node,
        SecuritySupport::default(),
        SpectrumManagementSupport::default(),
    );
    let mut state = wlan_sme::client::TrustedScanState::<PrivateIndex>::new(7);
    let txn_id = sme
        .start_trusted_scan(
            &mut state,
            ScanRequest::Passive(PassiveScanRequest { channels: vec![1] }),
        )
        .unwrap();
    assert!(
        matches!(mlme_stream.try_recv(), Ok(MlmeRequest::Scan(ref scan)) if scan.txn_id == txn_id)
    );

    sme.on_trusted_mlme_scan_result(
        fidl_fuchsia_wlan_mlme::ScanResult {
            txn_id,
            timestamp_nanos: 100,
            bss: open_bss(),
        },
        PrivateIndex(41),
        &mut state,
    )
    .unwrap();
    let terminal = sme
        .on_trusted_mlme_scan_end(
            ScanEnd {
                txn_id,
                code: ScanResultCode::Success,
            },
            &mut state,
        )
        .unwrap();
    assert_eq!(terminal.generation(), 7);
    assert_eq!(terminal.txn_id(), txn_id);
    assert_eq!(terminal.bss_description_list().len(), 1);
    let input_provenance = terminal.input_provenance().collect::<Vec<_>>();
    assert_eq!(input_provenance.len(), 1);
    assert_eq!(input_provenance[0].0, 41);
    assert!(matches!(
        sme.on_trusted_mlme_scan_end(
            ScanEnd {
                txn_id,
                code: ScanResultCode::Success
            },
            &mut state,
        ),
        Err(wlan_sme::client::TrustedScanError::DuplicateOrLate)
    ));
    assert!(mlme_stream.try_recv().is_err());
}

#[test]
fn trusted_guard_rejects_ordinary_responder_without_request_or_output() {
    let inspector = fuchsia_inspect::Inspector::default();
    let inspect_node = inspector.root().create_child("trusted-guard");
    let (mut sme, _mlme_sink, mut mlme_stream, _time_stream) = ClientSme::new(
        ClientConfig::default(),
        device_info(),
        inspector,
        inspect_node,
        SecuritySupport::default(),
        SpectrumManagementSupport::default(),
    );
    let mut state = wlan_sme::client::TrustedScanState::<u64>::new(9);
    let txn_id = sme
        .start_trusted_scan(
            &mut state,
            ScanRequest::Passive(PassiveScanRequest { channels: vec![1] }),
        )
        .unwrap();
    assert!(matches!(mlme_stream.try_recv(), Ok(MlmeRequest::Scan(_))));

    let mut ordinary = sme.on_scan_command(ScanRequest::Passive(PassiveScanRequest {
        channels: vec![1],
    }));
    assert_eq!(ordinary.try_recv(), Ok(None));
    assert!(mlme_stream.try_recv().is_err());
    assert!(matches!(
        sme.on_trusted_mlme_scan_end(
            ScanEnd {
                txn_id,
                code: ScanResultCode::Success
            },
            &mut state,
        ),
        Err(wlan_sme::client::TrustedScanError::Failed)
    ));
    assert_eq!(ordinary.try_recv(), Ok(None));
    assert!(mlme_stream.try_recv().is_err());
}

#[test]
fn trusted_mismatch_retains_affine_input_and_emits_nothing() {
    let inspector = fuchsia_inspect::Inspector::default();
    let inspect_node = inspector.root().create_child("trusted-mismatch");
    let (mut sme, _mlme_sink, mut mlme_stream, _time_stream) = ClientSme::new(
        ClientConfig::default(),
        device_info(),
        inspector,
        inspect_node,
        SecuritySupport::default(),
        SpectrumManagementSupport::default(),
    );
    let mut state = wlan_sme::client::TrustedScanState::<Box<u64>>::new(11);
    let txn_id = sme
        .start_trusted_scan(
            &mut state,
            ScanRequest::Passive(PassiveScanRequest { channels: vec![1] }),
        )
        .unwrap();
    assert!(matches!(mlme_stream.try_recv(), Ok(MlmeRequest::Scan(_))));
    assert!(matches!(
        sme.on_trusted_mlme_scan_result(
            fidl_fuchsia_wlan_mlme::ScanResult {
                txn_id: txn_id + 1,
                timestamp_nanos: 0,
                bss: open_bss(),
            },
            Box::new(3),
            &mut state,
        ),
        Err(wlan_sme::client::TrustedScanError::Mismatch)
    ));
    assert!(state.is_failed());
    assert!(mlme_stream.try_recv().is_err());
}

#[test]
fn connecting_shortcut_cannot_bypass_active_trusted_guard() {
    let inspector = fuchsia_inspect::Inspector::default();
    let inspect_node = inspector.root().create_child("trusted-connecting");
    let (mut sme, _mlme_sink, mut mlme_stream, _time_stream) = ClientSme::new(
        ClientConfig::default(),
        device_info(),
        inspector,
        inspect_node,
        SecuritySupport::default(),
        SpectrumManagementSupport::default(),
    );
    let bss = open_bss();
    let _connect_transaction = sme.on_connect_command(ConnectRequest {
        ssid: b"open".to_vec(),
        bss_description: bss,
        multiple_bss_candidates: false,
        authentication: Authentication {
            protocol: Protocol::Open,
            credentials: None,
        },
        deprecated_scan_type: ScanType::Passive,
    });
    assert!(matches!(
        mlme_stream.try_recv(),
        Ok(MlmeRequest::Connect(_))
    ));

    let mut state = wlan_sme::client::TrustedScanState::<u64>::new(13);
    let txn_id = sme
        .start_trusted_scan(
            &mut state,
            ScanRequest::Passive(PassiveScanRequest { channels: vec![1] }),
        )
        .unwrap();
    assert!(matches!(mlme_stream.try_recv(), Ok(MlmeRequest::Scan(_))));
    let mut ordinary = sme.on_scan_command(ScanRequest::Passive(PassiveScanRequest {
        channels: vec![1],
    }));
    assert_eq!(ordinary.try_recv(), Ok(None));
    assert!(mlme_stream.try_recv().is_err());
    assert!(matches!(
        sme.on_trusted_mlme_scan_end(
            ScanEnd {
                txn_id,
                code: ScanResultCode::Success
            },
            &mut state,
        ),
        Err(wlan_sme::client::TrustedScanError::Failed)
    ));
    assert_eq!(ordinary.try_recv(), Ok(None));
}
