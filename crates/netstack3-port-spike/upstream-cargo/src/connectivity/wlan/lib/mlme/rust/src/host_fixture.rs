// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deterministic host fixture that exercises the production `ClientMlme`.

use crate::device::{DeviceOps, LinkStatus};
use crate::{MlmeImpl, client::ClientMlme};
use fdf::ArenaStaticBox;
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_driver as fidl_driver;
use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
use fidl_fuchsia_wlan_mlme as fidl_mlme;
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use futures::channel::mpsc;
use std::sync::{Arc, Mutex};
use wlan_sme::MlmeRequest;
const CLIENT: [u8; 6] = [0x8a, 0xfd, 0x2a, 0x8b, 0x70, 0x5a];
const AP: [u8; 6] = [0x72, 0xa6, 0xc7, 0x7d, 0x56, 0x93];

fn channel(number: u8) -> fidl_ieee80211::ChannelNumber {
    fidl_ieee80211::ChannelNumber {
        band: fidl_ieee80211::WlanBand::FiveGhz,
        number,
    }
}

fn wpa3_bss() -> fidl_ieee80211::BssDescription {
    fidl_ieee80211::BssDescription {
        bssid: AP,
        bss_type: fidl_ieee80211::BssType::Infrastructure,
        beacon_period: 100,
        capability_info: 0x11,
        ies: vec![
            0, 3, b'p', b'h', b'1', 1, 8, 0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c, 48, 20,
            1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 8, 0xcc, 0, 244,
            1, 0x20,
        ],
        primary: channel(36),
        bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw80,
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
            band: fidl_ieee80211::WlanBand::FiveGhz,
            basic_rates: vec![0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c],
            ht_cap: Some(Box::new(fidl_ieee80211::HtCapabilities {
                bytes: [
                    0xff, 0x09, 3, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0,
                ],
            })),
            vht_cap: Some(Box::new(fidl_ieee80211::VhtCapabilities {
                bytes: [
                    0xb2, 0x71, 0x80, 0x33, 0xfa, 0xff, 0, 0, 0xfa, 0xff, 0, 0x20,
                ],
            })),
            primary_channels: vec![channel(36)],
        }],
        softmac_hardware_capability: 0,
        qos_capable: false,
    }
}

fn security_support() -> fidl_common::SecuritySupport {
    fidl_common::SecuritySupport {
        mfp: Some(fidl_common::MfpFeature {
            supported: Some(true),
        }),
        sae: Some(fidl_common::SaeFeature {
            driver_handler_supported: Some(false),
            sme_handler_supported: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
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
    minstrel: Option<crate::MinstrelWrapper>,
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
                band: Some(fidl_ieee80211::WlanBand::FiveGhz),
                ht_caps: device_info().bands[0].ht_cap.clone().map(|cap| *cap),
                vht_caps: device_info().bands[0].vht_cap.clone().map(|cap| *cap),
                basic_rates: Some(vec![0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c]),
                primary_channels: Some(vec![channel(36)]),
                ..Default::default()
            }]),
            ..Default::default()
        })
    }

    async fn discovery_support(&mut self) -> Result<fidl_softmac::DiscoverySupport, zx::Status> {
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
        self.effects
            .lock()
            .unwrap()
            .frames
            .push((buffer.to_vec(), tx_flags));
        Ok(())
    }

    async fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
        let mut effects = self.effects.lock().unwrap();
        effects.link_up = status == LinkStatus::UP;
        effects.order.push(if status == LinkStatus::UP {
            "port-open"
        } else {
            "port-close"
        });
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
        self.effects
            .lock()
            .unwrap()
            .cleared_associations
            .push(request.clone());
        Ok(())
    }

    async fn update_wmm_parameters(
        &mut self,
        _request: &fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        Ok(())
    }

    fn take_mlme_event_stream(&mut self) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
        self.event_stream.take()
    }

    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
        self.effects.lock().unwrap().events.push(event.clone());
        self.event_sink.unbounded_send(event).map_err(Into::into)
    }

    fn set_minstrel(&mut self, minstrel: crate::MinstrelWrapper) {
        self.minstrel = Some(minstrel);
    }

    fn minstrel(&mut self) -> Option<crate::MinstrelWrapper> {
        self.minstrel.clone()
    }
}

fn connect_request(h2e: bool) -> fidl_mlme::ConnectRequest {
    let mut selected_bss = wpa3_bss();
    if !h2e {
        selected_bss.ies.truncate(selected_bss.ies.len() - 3);
    }
    fidl_mlme::ConnectRequest {
        selected_bss,
        connect_failure_timeout: 20,
        auth_type: fidl_mlme::AuthenticationTypes::Sae,
        sae_password: vec![],
        wep_key: None,
        security_ie: vec![
            48, 20, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 8, 0xcc, 0,
        ],
        owe_public_key: None,
    }
}

async fn association_request(h2e: bool) -> Result<Vec<u8>, &'static str> {
    let (device, effects) = FakeDeviceOps::new();
    let (timer, _) = crate::common::timer::create_timer();
    let mut mlme = ClientMlme::new(Default::default(), device, timer)
        .await
        .map_err(|_| "ClientMlme fixture construction failed")?;
    mlme.handle_mlme_request(MlmeRequest::Connect(connect_request(h2e)))
        .await
        .map_err(|_| "ClientMlme fixture connect failed")?;
    mlme.handle_mlme_request(MlmeRequest::SaeHandshakeResp(
        fidl_mlme::SaeHandshakeResponse {
            peer_sta_address: AP,
            status_code: fidl_ieee80211::StatusCode::Success,
        },
    ))
    .await
    .map_err(|_| "ClientMlme fixture SAE completion failed")?;
    let frame = effects
        .lock()
        .unwrap()
        .frames
        .iter()
        .find(|(frame, _)| frame.first() == Some(&0))
        .map(|(frame, _)| frame.clone())
        .ok_or("ClientMlme did not serialize an association request")?;
    fuchsia_softmac_port::finalize_association_request(
        &frame,
        &fuchsia_softmac_port::linux_61840_oracle_profile(),
    )
    .map_err(|_| "ClientMlme association profile rejected")
}

/// Association requests produced by the production pinned `ClientMlme` from
/// the same selected BSS, with and without SAE-H2E evidence.
pub fn sae_h2e_association_request_fixture() -> Result<(Vec<u8>, Vec<u8>), &'static str> {
    futures::executor::block_on(async {
        Ok((
            association_request(true).await?,
            association_request(false).await?,
        ))
    })
}
