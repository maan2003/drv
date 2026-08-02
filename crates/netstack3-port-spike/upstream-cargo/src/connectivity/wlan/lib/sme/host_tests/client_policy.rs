// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

use fidl_fuchsia_wlan_common::{SecuritySupport, SpectrumManagementSupport, WlanMacRole};
use fidl_fuchsia_wlan_ieee80211::{ChannelNumber, WlanBand};
use fidl_fuchsia_wlan_mlme::{
    BandCapability, DeviceInfo, MlmeEvent, ScanEnd, ScanResultCode, ScanTypes,
};
use fidl_fuchsia_wlan_sme::{PassiveScanRequest, ScanRequest};
use wlan_sme::client::{ClientConfig, ClientSme, ClientSmeStatus};
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
