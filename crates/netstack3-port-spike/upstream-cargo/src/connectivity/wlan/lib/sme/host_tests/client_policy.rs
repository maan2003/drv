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
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
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
            |_, _| true,
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
            |_, _| true,
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
            |_, _| true,
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
    let rejected = sme
        .on_trusted_mlme_scan_result(
            fidl_fuchsia_wlan_mlme::ScanResult {
                txn_id: txn_id + 1,
                timestamp_nanos: 0,
                bss: open_bss(),
            },
            Box::new(3),
            &mut state,
        )
        .unwrap_err();
    assert_eq!(
        rejected.error(),
        wlan_sme::client::TrustedScanError::Mismatch
    );
    assert_eq!(state.input_count(), 0);
    let (_, rejected_result, rejected_p) = rejected.into_parts();
    assert_eq!(rejected_result.txn_id, txn_id + 1);
    assert_eq!(*rejected_p, 3);
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
            |_, _| true,
        ),
        Err(wlan_sme::client::TrustedScanError::Failed)
    ));
    assert_eq!(ordinary.try_recv(), Ok(None));
}

#[test]
fn trusted_lineage_distinguishes_initializer_merge_and_exact_echo_drop() {
    struct P(usize);
    let inspector = fuchsia_inspect::Inspector::default();
    let inspect_node = inspector.root().create_child("trusted-lineage");
    let (mut sme, _sink, mut requests, _time) = ClientSme::new(
        ClientConfig::default(),
        device_info(),
        inspector,
        inspect_node,
        SecuritySupport::default(),
        SpectrumManagementSupport::default(),
    );
    let mut state = wlan_sme::client::TrustedScanState::<P>::new(21);
    let txn_id = sme
        .start_trusted_scan(
            &mut state,
            ScanRequest::Passive(PassiveScanRequest {
                channels: vec![1, 6],
            }),
        )
        .unwrap();
    assert!(matches!(requests.try_recv(), Ok(MlmeRequest::Scan(_))));

    let mut descriptions = vec![open_bss(), open_bss(), open_bss(), open_bss()];
    descriptions[1].primary.number = 6;
    descriptions[1].rssi_dbm = -50; // exact weaker-cross-channel drop
    descriptions[2].primary.number = 6;
    descriptions[2].rssi_dbm = -40; // equal RSSI is accepted
    descriptions[3].primary.number = 1;
    descriptions[3].rssi_dbm = -30; // latest accepted representative
    for (encounter, bss) in descriptions.iter().cloned().enumerate() {
        sme.on_trusted_mlme_scan_result(
            fidl_fuchsia_wlan_mlme::ScanResult {
                txn_id,
                timestamp_nanos: encounter as i64,
                bss,
            },
            P(encounter),
            &mut state,
        )
        .unwrap();
    }
    let mut validated_lineage = false;
    let terminal = sme
        .on_trusted_mlme_scan_end(
            ScanEnd {
                txn_id,
                code: ScanResultCode::Success,
            },
            &mut state,
            |inputs, aggregates| {
                assert_eq!(inputs.len(), 4);
                let aggregate = &aggregates[0];
                assert_eq!(aggregate.lineage().merger_input_indices(), &[0, 2, 3]);
                assert_eq!(aggregate.lineage().dropped_input_indices(), &[1]);
                assert_eq!(aggregate.lineage().representative_index(), 3);
                assert_eq!(aggregate.lineage().occupied_predicate_evaluations(), 3);
                assert_eq!(aggregate.fixed_bss().rssi_dbm, -30);
                assert_eq!(aggregate.fixed_bss().primary.number, 1);
                validated_lineage = true;
                true
            },
        )
        .unwrap();
    assert_eq!(
        terminal
            .inputs()
            .iter()
            .map(|input| (
                input.table_index(),
                input.encounter_index(),
                input.provenance().0,
            ))
            .collect::<Vec<_>>(),
        vec![(0, 0, 0), (1, 1, 1), (2, 2, 2), (3, 3, 3)]
    );
    assert!(validated_lineage);
    assert_eq!(terminal.bss_description_list().len(), 1);
}

struct DropProbe {
    invalidated: Arc<AtomicBool>,
    dropped: Arc<AtomicUsize>,
    dropped_before_invalidation: Arc<AtomicUsize>,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        if !self.invalidated.load(Ordering::Acquire) {
            self.dropped_before_invalidation
                .fetch_add(1, Ordering::AcqRel);
        }
        self.dropped.fetch_add(1, Ordering::AcqRel);
    }
}

fn drop_probe(
    invalidated: &Arc<AtomicBool>,
    dropped: &Arc<AtomicUsize>,
    dropped_before_invalidation: &Arc<AtomicUsize>,
) -> DropProbe {
    DropProbe {
        invalidated: Arc::clone(invalidated),
        dropped: Arc::clone(dropped),
        dropped_before_invalidation: Arc::clone(dropped_before_invalidation),
    }
}

#[test]
fn occupied_in_flight_a_and_rejected_late_b_remain_affine_until_invalidation() {
    let inspector = fuchsia_inspect::Inspector::default();
    let node = inspector.root().create_child("occupied-in-flight");
    let (mut sme, _sink, mut requests, _time) = ClientSme::new(
        ClientConfig::default(),
        device_info(),
        inspector,
        node,
        SecuritySupport::default(),
        SpectrumManagementSupport::default(),
    );
    let invalidated = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicUsize::new(0));
    let early = Arc::new(AtomicUsize::new(0));
    let mut state = wlan_sme::client::TrustedScanState::new(31);
    let txn_id = sme
        .start_trusted_scan(
            &mut state,
            ScanRequest::Passive(PassiveScanRequest { channels: vec![1] }),
        )
        .unwrap();
    assert!(matches!(requests.try_recv(), Ok(MlmeRequest::Scan(_))));
    state.inject_result_aggregation_panic_for_test();
    let admitted_a = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = sme.on_trusted_mlme_scan_result(
            fidl_fuchsia_wlan_mlme::ScanResult {
                txn_id,
                timestamp_nanos: 0,
                bss: open_bss(),
            },
            drop_probe(&invalidated, &dropped, &early),
            &mut state,
        );
    }));
    assert!(admitted_a.is_err());
    assert_eq!(state.input_count(), 1);
    let rejected_b = sme
        .on_trusted_mlme_scan_result(
            fidl_fuchsia_wlan_mlme::ScanResult {
                txn_id,
                timestamp_nanos: 1,
                bss: open_bss(),
            },
            drop_probe(&invalidated, &dropped, &early),
            &mut state,
        )
        .unwrap_err();
    assert_eq!(
        rejected_b.error(),
        wlan_sme::client::TrustedScanError::Failed
    );
    assert_eq!(dropped.load(Ordering::Acquire), 0);
    invalidated.store(true, Ordering::Release);
    drop(rejected_b);
    drop(state);
    assert_eq!(dropped.load(Ordering::Acquire), 2);
    assert_eq!(early.load(Ordering::Acquire), 0);
}

#[test]
fn input_4097_is_rejected_intact_and_all_affine_values_wait_for_invalidation() {
    let inspector = fuchsia_inspect::Inspector::default();
    let node = inspector.root().create_child("trusted-capacity");
    let (mut sme, _sink, mut requests, _time) = ClientSme::new(
        ClientConfig::default(),
        device_info(),
        inspector,
        node,
        SecuritySupport::default(),
        SpectrumManagementSupport::default(),
    );
    let invalidated = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicUsize::new(0));
    let early = Arc::new(AtomicUsize::new(0));
    let mut state = wlan_sme::client::TrustedScanState::new(32);
    let txn_id = sme
        .start_trusted_scan(
            &mut state,
            ScanRequest::Passive(PassiveScanRequest { channels: vec![1] }),
        )
        .unwrap();
    assert!(matches!(requests.try_recv(), Ok(MlmeRequest::Scan(_))));
    for encounter in 0..4096 {
        sme.on_trusted_mlme_scan_result(
            fidl_fuchsia_wlan_mlme::ScanResult {
                txn_id,
                timestamp_nanos: encounter,
                bss: open_bss(),
            },
            drop_probe(&invalidated, &dropped, &early),
            &mut state,
        )
        .unwrap();
    }
    let rejected = sme
        .on_trusted_mlme_scan_result(
            fidl_fuchsia_wlan_mlme::ScanResult {
                txn_id,
                timestamp_nanos: 4096,
                bss: open_bss(),
            },
            drop_probe(&invalidated, &dropped, &early),
            &mut state,
        )
        .unwrap_err();
    assert_eq!(
        rejected.error(),
        wlan_sme::client::TrustedScanError::Exhausted
    );
    assert_eq!(state.input_count(), 4096);
    assert_eq!(dropped.load(Ordering::Acquire), 0);
    invalidated.store(true, Ordering::Release);
    drop(rejected);
    drop(state);
    assert_eq!(dropped.load(Ordering::Acquire), 4097);
    assert_eq!(early.load(Ordering::Acquire), 0);
}

#[test]
fn ordinary_and_trusted_aggregation_produce_exactly_equal_terminal_bss() {
    let base = open_bss();
    let mut descriptions = vec![base.clone()];
    // Same-channel weak/equal/strong are all accepted.
    for rssi_dbm in [-50, -40, -30] {
        let mut bss = base.clone();
        bss.rssi_dbm = rssi_dbm;
        descriptions.push(bss);
    }
    // Only cross-channel weaker is dropped; equal is accepted.
    for rssi_dbm in [-60, -30] {
        let mut bss = base.clone();
        bss.primary.number = 6;
        bss.rssi_dbm = rssi_dbm;
        descriptions.push(bss);
    }
    // Repeated overwrite and an identical BSS still carry distinct P values.
    for rssi_dbm in [-20, -10, -10] {
        let mut bss = base.clone();
        bss.rssi_dbm = rssi_dbm;
        descriptions.push(bss);
    }
    // Malformed, conflicting, and duplicate IEs exercise identical merger behavior.
    for ies in [
        vec![0, 5, 1],
        vec![0, 1, b'x'],
        vec![0, 1, b'a', 0, 1, b'b'],
    ] {
        let mut bss = base.clone();
        bss.ies = ies;
        descriptions.push(bss);
    }
    // Repeated changing maximum-size vendor IEs force IesMerger overflow.
    for tag in 0..=255u8 {
        let mut bss = base.clone();
        bss.ies = vec![0xdd, 255];
        bss.ies.extend(std::iter::repeat_n(tag, 255));
        descriptions.push(bss);
    }
    // Alternating BSSIDs ensure equality is paired by key, not HashMap order.
    for suffix in [7, 8, 7, 9, 8] {
        let mut bss = base.clone();
        bss.bssid[5] = suffix;
        descriptions.push(bss);
    }
    let unique_bssids = descriptions
        .iter()
        .map(|bss| bss.bssid)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let expected_occupied = descriptions.len() - unique_bssids;

    let inspector = fuchsia_inspect::Inspector::default();
    let node = inspector.root().create_child("ordinary-differential");
    let (mut ordinary_sme, _sink, mut requests, _time) = ClientSme::new(
        ClientConfig::default(),
        device_info(),
        inspector,
        node,
        SecuritySupport::default(),
        SpectrumManagementSupport::default(),
    );
    let mut ordinary_receiver =
        ordinary_sme.on_scan_command(ScanRequest::Passive(PassiveScanRequest {
            channels: vec![1, 6],
        }));
    let ordinary_txn = match requests.try_recv().unwrap() {
        MlmeRequest::Scan(request) => request.txn_id,
        other => panic!("expected scan request, got {}", other.name()),
    };
    for (encounter, bss) in descriptions.iter().cloned().enumerate() {
        Station::on_mlme_event(
            &mut ordinary_sme,
            MlmeEvent::OnScanResult {
                result: fidl_fuchsia_wlan_mlme::ScanResult {
                    txn_id: ordinary_txn,
                    timestamp_nanos: encounter as i64,
                    bss,
                },
            },
        );
    }
    Station::on_mlme_event(
        &mut ordinary_sme,
        MlmeEvent::OnScanEnd {
            end: ScanEnd {
                txn_id: ordinary_txn,
                code: ScanResultCode::Success,
            },
        },
    );
    let mut ordinary = ordinary_receiver
        .try_recv()
        .unwrap()
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|result| result.bss_description)
        .collect::<Vec<_>>();
    ordinary.sort_by_key(|bss| bss.bssid);

    let inspector = fuchsia_inspect::Inspector::default();
    let node = inspector.root().create_child("trusted-differential");
    let (mut trusted_sme, _sink, mut requests, _time) = ClientSme::new(
        ClientConfig::default(),
        device_info(),
        inspector,
        node,
        SecuritySupport::default(),
        SpectrumManagementSupport::default(),
    );
    let mut state = wlan_sme::client::TrustedScanState::new(41);
    let trusted_txn = trusted_sme
        .start_trusted_scan(
            &mut state,
            ScanRequest::Passive(PassiveScanRequest {
                channels: vec![1, 6],
            }),
        )
        .unwrap();
    assert!(matches!(requests.try_recv(), Ok(MlmeRequest::Scan(_))));
    for (encounter, bss) in descriptions.into_iter().enumerate() {
        trusted_sme
            .on_trusted_mlme_scan_result(
                fidl_fuchsia_wlan_mlme::ScanResult {
                    txn_id: trusted_txn,
                    timestamp_nanos: encounter as i64,
                    bss,
                },
                encounter,
                &mut state,
            )
            .unwrap();
    }
    let trusted = trusted_sme
        .on_trusted_mlme_scan_end(
            ScanEnd {
                txn_id: trusted_txn,
                code: ScanResultCode::Success,
            },
            &mut state,
            |_, aggregates| {
                assert_eq!(
                    aggregates
                        .iter()
                        .map(|aggregate| aggregate.lineage().occupied_predicate_evaluations())
                        .sum::<usize>(),
                    expected_occupied
                );
                true
            },
        )
        .unwrap();
    let mut trusted = trusted.bss_description_list().to_vec();
    trusted.sort_by_key(|bss| bss.bssid);
    assert_eq!(trusted, ordinary);
}
