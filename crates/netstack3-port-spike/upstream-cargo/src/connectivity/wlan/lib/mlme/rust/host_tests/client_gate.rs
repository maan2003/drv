// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

use fdf::ArenaStaticBox;
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_driver as fidl_driver;
use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
use fidl_fuchsia_wlan_internal as fidl_internal;
use fidl_fuchsia_wlan_mlme as fidl_mlme;
use fidl_fuchsia_wlan_sme as fidl_sme;
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use futures::channel::mpsc;
use futures::StreamExt;
use fuchsia_sync::Mutex as FuchsiaMutex;
use ieee80211::{MacAddr, Ssid};
use std::sync::{Arc, Mutex};
use wlan_mlme::device::{DeviceOps, LinkStatus};
use wlan_mlme::{MlmeImpl, client::ClientMlme};
use wlan_sme::client::{ClientConfig, ClientSme};
use wlan_sme::{MlmeRequest, Station};
use wlan_rsn::key::{gtk::GtkProvider, igtk::IgtkProvider};
use wlan_rsn::nonce::NonceReader;
use wlan_rsn::rsna::SecAssocUpdate;
use wlan_rsn::{Authenticator, ProtectionInfo};
use wlan_mlme::common::ie::rsn::cipher::{Cipher, CCMP_128};
use wlan_mlme::common::ie::rsn::rsne::{self, Rsne};
use wlan_mlme::common::ie::rsn::suite_filter::DEFAULT_GROUP_MGMT_CIPHER;
use wlan_mlme::common::ie::rsn::suite_selector::OUI;

const CLIENT: [u8; 6] = [7; 6];
const AP: [u8; 6] = [6; 6];

fn channel(number: u8) -> fidl_ieee80211::ChannelNumber {
    fidl_ieee80211::ChannelNumber { band: fidl_ieee80211::WlanBand::TwoGhz, number }
}

fn wpa3_bss() -> fidl_ieee80211::BssDescription {
    fidl_ieee80211::BssDescription {
        bssid: AP,
        bss_type: fidl_ieee80211::BssType::Infrastructure,
        beacon_period: 100,
        capability_info: 0x11,
        ies: vec![
            0, 4, b't', b'e', b's', b't',
            1, 4, 0x82, 0x84, 0x8b, 0x96,
            48, 20, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0,
            0x0f, 0xac, 8, 0xcc, 0,
        ],
        primary: channel(6),
        bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw20,
        vht_secondary_80_channel: channel(0),
        rssi_dbm: -40,
        snr_db: 30,
    }
}

fn device_info() -> fidl_mlme::DeviceInfo {
    fidl_mlme::DeviceInfo {
        sta_addr: CLIENT,
        factory_addr: CLIENT,
        role: fidl_common::WlanMacRole::Client,
        bands: vec![fidl_mlme::BandCapability {
            band: fidl_ieee80211::WlanBand::TwoGhz,
            basic_rates: vec![0x82, 0x84, 0x8b, 0x96],
            ht_cap: None,
            vht_cap: None,
            primary_channels: vec![channel(6)],
        }],
        softmac_hardware_capability: 0,
        qos_capable: false,
    }
}

fn security_support() -> fidl_common::SecuritySupport {
    fidl_common::SecuritySupport {
        mfp: Some(fidl_common::MfpFeature { supported: Some(true) }),
        sae: Some(fidl_common::SaeFeature {
            driver_handler_supported: Some(false),
            sme_handler_supported: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn parse_rsne(bytes: &[u8]) -> Rsne {
    rsne::from_bytes(bytes).expect("valid RSNE").1
}

fn authenticator(supplicant_rsne: &[u8]) -> Authenticator {
    let gtk = GtkProvider::new(Cipher { oui: OUI, suite_type: CCMP_128 }, 1, 0).unwrap();
    let igtk = IgtkProvider::new(DEFAULT_GROUP_MGMT_CIPHER).unwrap();
    let a_rsne = parse_rsne(&wpa3_bss().ies[12..]);
    Authenticator::new_wpa3(
        NonceReader::new(&MacAddr::from(AP)).unwrap(),
        Arc::new(FuchsiaMutex::new(gtk)),
        Arc::new(FuchsiaMutex::new(igtk)),
        Ssid::try_from("test").unwrap(),
        b"password".to_vec(),
        MacAddr::from(CLIENT),
        ProtectionInfo::Rsne(parse_rsne(supplicant_rsne)),
        MacAddr::from(AP),
        ProtectionInfo::Rsne(a_rsne),
    )
    .unwrap()
}

fn rx_info() -> fidl_softmac::WlanRxInfo {
    fidl_softmac::WlanRxInfo {
        rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
        valid_fields: fidl_softmac::WlanRxInfoValid::RSSI,
        phy: fidl_ieee80211::WlanPhyType::Erp,
        data_rate: 0,
        primary: channel(6),
        bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw20,
        vht_secondary_80_channel: channel(0),
        mcs: 0,
        rssi_dbm: -40,
        snr_dbh: 0,
    }
}

fn peer_auth_frame(frame: &fidl_mlme::SaeFrame) -> Vec<u8> {
    let mut bytes = vec![0xb0, 0, 0, 0];
    bytes.extend_from_slice(&CLIENT);
    bytes.extend_from_slice(&AP);
    bytes.extend_from_slice(&AP);
    bytes.extend_from_slice(&[0, 0, 3, 0]);
    bytes.extend_from_slice(&frame.seq_num.to_le_bytes());
    bytes.extend_from_slice(&frame.status_code.into_primitive().to_le_bytes());
    bytes.extend_from_slice(&frame.sae_fields);
    bytes
}

fn peer_assoc_success() -> Vec<u8> {
    let mut bytes = vec![0x10, 0, 0, 0];
    bytes.extend_from_slice(&CLIENT);
    bytes.extend_from_slice(&AP);
    bytes.extend_from_slice(&AP);
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&[0x11, 0, 0, 0, 42, 0]);
    bytes.extend_from_slice(&[1, 4, 0x82, 0x84, 0x8b, 0x96]);
    bytes
}

fn peer_eapol(payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x08, 0x02, 0, 0];
    bytes.extend_from_slice(&CLIENT);
    bytes.extend_from_slice(&AP);
    bytes.extend_from_slice(&AP);
    bytes.extend_from_slice(&[0, 0, 0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
    bytes.extend_from_slice(payload);
    bytes
}

#[derive(Default)]
struct Effects {
    frames: Vec<(Vec<u8>, fidl_softmac::WlanTxInfoFlags)>,
    keys: Vec<fidl_softmac::WlanKeyConfiguration>,
    associations: Vec<fidl_softmac::WlanAssociationConfig>,
    cleared_associations: Vec<fidl_softmac::WlanSoftmacBaseClearAssociationRequest>,
    link_up: bool,
    events: Vec<fidl_mlme::MlmeEvent>,
    order: Vec<&'static str>,
}

struct FakeDeviceOps {
    effects: Arc<Mutex<Effects>>,
    event_sink: mpsc::UnboundedSender<fidl_mlme::MlmeEvent>,
    event_stream: Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>,
    minstrel: Option<wlan_mlme::MinstrelWrapper>,
}

impl FakeDeviceOps {
    fn new() -> (Self, Arc<Mutex<Effects>>) {
        let effects = Arc::new(Mutex::new(Effects::default()));
        let (event_sink, event_stream) = mpsc::unbounded();
        (
            Self {
                effects: effects.clone(),
                event_sink,
                event_stream: Some(event_stream),
                minstrel: None,
            },
            effects,
        )
    }
}

impl DeviceOps for FakeDeviceOps {
    async fn wlan_softmac_query_response(
        &mut self,
    ) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status> {
        Ok(fidl_softmac::WlanSoftmacQueryResponse {
            sta_addr: Some(CLIENT),
            mac_role: Some(fidl_common::WlanMacRole::Client),
            hardware_capability: Some(0),
            band_caps: Some(vec![fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_ieee80211::WlanBand::TwoGhz),
                basic_rates: Some(vec![0x82, 0x84, 0x8b, 0x96]),
                primary_channels: Some(vec![channel(6)]),
                ..Default::default()
            }]),
            ..Default::default()
        })
    }

    async fn discovery_support(
        &mut self,
    ) -> Result<fidl_softmac::DiscoverySupport, zx::Status> {
        Ok(Default::default())
    }

    async fn mac_sublayer_support(
        &mut self,
    ) -> Result<fidl_common::MacSublayerSupport, zx::Status> {
        Ok(fidl_common::MacSublayerSupport {
            device: Some(fidl_common::DeviceExtension {
                mac_implementation_type: Some(fidl_common::MacImplementationType::Softmac),
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    async fn security_support(&mut self) -> Result<fidl_common::SecuritySupport, zx::Status> {
        Ok(security_support())
    }

    async fn spectrum_management_support(
        &mut self,
    ) -> Result<fidl_common::SpectrumManagementSupport, zx::Status> {
        Ok(Default::default())
    }

    fn deliver_eth_frame(&mut self, _packet: &[u8]) -> Result<(), zx::Status> {
        Ok(())
    }

    fn send_wlan_frame(
        &mut self,
        buffer: ArenaStaticBox<[u8]>,
        tx_flags: fidl_softmac::WlanTxInfoFlags,
        _async_id: Option<fuchsia_trace::Id>,
    ) -> Result<(), zx::Status> {
        self.effects.lock().unwrap().frames.push((buffer.to_vec(), tx_flags));
        Ok(())
    }

    async fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
        let mut effects = self.effects.lock().unwrap();
        effects.link_up = status == LinkStatus::UP;
        effects.order.push(if status == LinkStatus::UP { "port-open" } else { "port-close" });
        Ok(())
    }

    async fn set_channel(
        &mut self,
        _primary: fidl_ieee80211::ChannelNumber,
        _bandwidth: fidl_ieee80211::ChannelBandwidth,
        _vht_secondary_80_channel: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        Ok(())
    }

    async fn set_mac_address(&mut self, _mac_addr: [u8; 6]) -> Result<(), zx::Status> {
        Ok(())
    }

    async fn start_passive_scan(
        &mut self,
        _request: &fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn start_active_scan(
        &mut self,
        _request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn cancel_scan(
        &mut self,
        _request: &fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        Ok(())
    }

    async fn join_bss(&mut self, _request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        Ok(())
    }

    async fn enable_beaconing(
        &mut self,
        _request: fidl_softmac::WlanSoftmacBaseEnableBeaconingRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn disable_beaconing(&mut self) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn install_key(
        &mut self,
        key: &fidl_softmac::WlanKeyConfiguration,
    ) -> Result<(), zx::Status> {
        self.effects.lock().unwrap().keys.push(key.clone());
        self.effects.lock().unwrap().order.push("key");
        Ok(())
    }

    async fn notify_association_complete(
        &mut self,
        association: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        self.effects.lock().unwrap().associations.push(association);
        Ok(())
    }

    async fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        self.effects.lock().unwrap().cleared_associations.push(request.clone());
        Ok(())
    }

    async fn update_wmm_parameters(
        &mut self,
        _request: &fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        Ok(())
    }

    fn take_mlme_event_stream(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
        self.event_stream.take()
    }

    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
        self.effects.lock().unwrap().events.push(event.clone());
        self.event_sink.unbounded_send(event).map_err(Into::into)
    }

    fn set_minstrel(&mut self, minstrel: wlan_mlme::MinstrelWrapper) {
        self.minstrel = Some(minstrel);
    }

    fn minstrel(&mut self) -> Option<wlan_mlme::MinstrelWrapper> {
        self.minstrel.clone()
    }
}

#[test]
fn fake_device_is_effect_only_and_advertises_sme_sae() {
    let (mut device, effects) = FakeDeviceOps::new();
    let support = futures::executor::block_on(device.security_support()).unwrap();
    let sae = support.sae.unwrap();
    assert_eq!(sae.sme_handler_supported, Some(true));
    assert_eq!(sae.driver_handler_supported, Some(false));
    assert!(effects.lock().unwrap().frames.is_empty());
    assert_eq!(AP, [6; 6]);
}

#[test]
fn sme_connect_drives_production_sae_tx_and_both_timer_seams() {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
        let inspector = fuchsia_inspect::Inspector::default();
        let inspect_node = inspector.root().create_child("sme");
        let mut client_config = ClientConfig::default();
        client_config.wpa3_supported = true;
        let (mut sme, _mlme_sink, mut requests, mut sme_timers) = ClientSme::new(
            client_config,
            device_info(),
            inspector,
            inspect_node,
            security_support(),
            Default::default(),
        );
        let _transaction = sme.on_connect_command(fidl_sme::ConnectRequest {
            ssid: b"test".to_vec(),
            bss_description: wpa3_bss(),
            multiple_bss_candidates: false,
            authentication: fidl_internal::Authentication {
                protocol: fidl_internal::Protocol::Wpa3Personal,
                credentials: Some(Box::new(fidl_internal::Credentials::Wpa(
                    fidl_internal::WpaCredentials::Passphrase(b"password".to_vec()),
                ))),
            },
            deprecated_scan_type: fidl_common::ScanType::Passive,
        });
        let connect = match requests.try_recv().expect("SME connect request") {
            MlmeRequest::Connect(connect) => connect,
            other => panic!("expected connect, got {}", other.name()),
        };
        assert_eq!(connect.auth_type, fidl_mlme::AuthenticationTypes::Sae);
        let mut peer = authenticator(&connect.security_ie);

        let (device, effects) = FakeDeviceOps::new();
        let (mlme_timer, mut mlme_timers) = wlan_mlme::common::timer::create_timer();
        let mut mlme = ClientMlme::new(Default::default(), device, mlme_timer).await.unwrap();
        mlme.handle_mlme_request(MlmeRequest::Connect(connect)).await.unwrap();

        let handshake_ind = effects.lock().unwrap().events.remove(0);
        assert!(matches!(handshake_ind, fidl_mlme::MlmeEvent::OnSaeHandshakeInd { .. }));
        Station::on_mlme_event(&mut sme, handshake_ind);

        let sae_tx = match requests.try_recv().expect("SME SAE frame request") {
            MlmeRequest::SaeFrameTx(frame) => frame,
            other => panic!("expected SAE TX, got {}", other.name()),
        };
        assert_eq!(sae_tx.peer_sta_address, AP);
        mlme.handle_mlme_request(MlmeRequest::SaeFrameTx(sae_tx.clone())).await.unwrap();

        {
            let effects = effects.lock().unwrap();
            assert_eq!(effects.frames.len(), 1);
            assert_eq!(&effects.frames[0].0[24..28], &[3, 0, 1, 0]);
        }

        let mut stale_sme_timer = None;
        while let Ok((_, event, _)) = sme_timers.try_recv() {
            if format!("{:?}", event.event).starts_with("SaeTimeout(") {
                stale_sme_timer = Some(event);
            }
        }
        let stale_sme_timer = stale_sme_timer.expect("SME SAE retry timer");
        let stale_mlme_timer = mlme_timers.try_recv().expect("MLME connect timer");
        assert!(matches!(
            stale_mlme_timer.1.event,
            wlan_mlme::client::TimedEvent::Connecting
        ));

        let mut peer_updates = vec![];
        peer.on_sae_frame_rx(&mut peer_updates, sae_tx).unwrap();
        let peer_sae_frames: Vec<_> = peer_updates
            .drain(..)
            .filter_map(|update| match update {
                SecAssocUpdate::TxSaeFrame(frame) => Some(frame),
                _ => None,
            })
            .collect();
        assert!(!peer_sae_frames.is_empty());
        for frame in peer_sae_frames {
            mlme.handle_mac_frame_rx(&peer_auth_frame(&frame), rx_info(), 1.into()).await;
            let event = effects.lock().unwrap().events.remove(0);
            assert!(matches!(event, fidl_mlme::MlmeEvent::OnSaeFrameRx { .. }));
            Station::on_mlme_event(&mut sme, event);
        }

        while let Ok(request) = requests.try_recv() {
            match request {
                MlmeRequest::SaeFrameTx(frame) => {
                    mlme.handle_mlme_request(MlmeRequest::SaeFrameTx(frame.clone())).await.unwrap();
                    peer.on_sae_frame_rx(&mut peer_updates, frame).unwrap();
                }
                MlmeRequest::SaeHandshakeResp(response) => {
                    assert_eq!(response.status_code, fidl_ieee80211::StatusCode::Success);
                    mlme.handle_mlme_request(MlmeRequest::SaeHandshakeResp(response)).await.unwrap();
                }
                other => panic!("unexpected SAE request {}", other.name()),
            }
        }
        assert!(effects.lock().unwrap().frames.iter().any(|(frame, _)| frame[0] == 0));

        mlme.handle_mac_frame_rx(&peer_assoc_success(), rx_info(), 2.into()).await;
        let connect_conf = effects.lock().unwrap().events.remove(0);
        assert!(matches!(connect_conf, fidl_mlme::MlmeEvent::ConnectConf { .. }));
        Station::on_mlme_event(&mut sme, connect_conf);

        let msg1 = peer_updates.into_iter().find_map(|update| match update {
            SecAssocUpdate::TxEapolKeyFrame { frame, .. } => Some(frame),
            _ => None,
        }).expect("authenticator EAPOL message 1");
        peer.on_eapol_conf(&mut vec![], fidl_mlme::EapolResultCode::Success).unwrap();
        mlme.handle_mac_frame_rx(&peer_eapol(&msg1), rx_info(), 3.into()).await;
        let eapol_ind = effects.lock().unwrap().events.remove(0);
        assert!(matches!(eapol_ind, fidl_mlme::MlmeEvent::EapolInd { .. }));
        Station::on_mlme_event(&mut sme, eapol_ind);

        let eapol2 = match requests.try_recv().expect("supplicant EAPOL message 2") {
            MlmeRequest::Eapol(request) => request,
            other => panic!("expected EAPOL, got {}", other.name()),
        };
        mlme.handle_mlme_request(MlmeRequest::Eapol(eapol2)).await.unwrap();
        let outgoing = effects.lock().unwrap().frames.last().unwrap().0.clone();
        let mut msg3_updates = vec![];
        peer.on_eapol_frame(
            &mut msg3_updates,
            eapol::Frame::Key(eapol::KeyFrameRx::parse(16, &outgoing[32..]).unwrap()),
        ).unwrap();
        let msg3 = msg3_updates.into_iter().find_map(|update| match update {
            SecAssocUpdate::TxEapolKeyFrame { frame, .. } => Some(frame),
            _ => None,
        }).expect("authenticator EAPOL message 3");
        mlme.handle_mac_frame_rx(&peer_eapol(&msg3), rx_info(), 4.into()).await;
        let eapol_ind = effects.lock().unwrap().events.remove(0);
        Station::on_mlme_event(&mut sme, eapol_ind);

        let mut saw_eapol_conf = false;
        let mut saw_key_conf = false;
        for _ in 0..4 {
            while let Ok(request) = requests.try_recv() {
                mlme.handle_mlme_request(request).await.unwrap();
            }
            let events: Vec<_> = effects.lock().unwrap().events.drain(..).collect();
            for event in events {
                saw_eapol_conf |= matches!(event, fidl_mlme::MlmeEvent::EapolConf { .. });
                saw_key_conf |= matches!(event, fidl_mlme::MlmeEvent::SetKeysConf { .. });
                Station::on_mlme_event(&mut sme, event);
            }
        }
        {
            let effects = effects.lock().unwrap();
            assert_eq!(effects.keys.len(), 3, "PTK, GTK, and IGTK must be installed");
            assert!(effects.keys.iter().any(|key| key.key_type == Some(fidl_ieee80211::KeyType::Pairwise)));
            assert!(effects.keys.iter().any(|key| key.key_type == Some(fidl_ieee80211::KeyType::Group)));
            assert!(effects.keys.iter().any(|key| key.key_type == Some(fidl_ieee80211::KeyType::Igtk)));
            assert!(!effects.associations.is_empty());
            assert!(effects.link_up, "controlled port must open only after key confirmation");
            assert_eq!(effects.order.last(), Some(&"port-open"));
        }
        assert!(saw_eapol_conf && saw_key_conf, "MLME confirmations must return to SME");

        sme.on_disconnect_command(
            fidl_sme::UserDisconnectReason::WlanSmeUnitTesting,
            Default::default(),
        );
        let cancel = requests.try_recv().expect("disconnect must cancel connection state");
        assert!(matches!(cancel, MlmeRequest::Deauthenticate(_)));
        mlme.handle_mlme_request(cancel).await.unwrap();
        effects.lock().unwrap().events.clear();
        let before = {
            let effects = effects.lock().unwrap();
            (effects.frames.len(), effects.keys.len(), effects.cleared_associations.len())
        };

        Station::on_timeout(&mut sme, stale_sme_timer);
        let fired: Vec<_> = wlan_mlme::common::timer::make_async_timed_event_stream(
            futures::stream::iter([stale_mlme_timer]),
        )
        .collect()
        .await;
        assert!(fired.is_empty(), "canceled MLME timer handle must filter stale timeout");
        assert!(requests.try_recv().is_err(), "stale SME timeout must not restart SAE");
        let effects = effects.lock().unwrap();
        assert_eq!(
            (effects.frames.len(), effects.keys.len(), effects.cleared_associations.len()),
            before,
            "stale MLME timeout must not transmit, install keys, or touch association"
        );
        assert!(effects.events.is_empty(), "stale timeout must remain externally silent");
        assert!(!effects.link_up, "cancellation must leave the controlled port closed");
        });
}
