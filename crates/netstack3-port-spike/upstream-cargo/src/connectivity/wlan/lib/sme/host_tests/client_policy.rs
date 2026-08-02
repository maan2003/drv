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
