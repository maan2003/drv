// SPDX-License-Identifier: GPL-2.0-only

//! Mechanical MT7921 effect adapter for the pinned production client MLME.
//!
//! This module owns no connection, retry, timer, regulatory, or credential
//! policy. The effect implementation is injected; this crate has no physical
//! transport implementation.

use fdf::ArenaStaticBox;
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_driver as fidl_driver;
use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
use fidl_fuchsia_wlan_mlme as fidl_mlme;
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use futures::channel::mpsc;
use wlan_mlme::device::{DeviceOps, LinkStatus};

use crate::Mt7921SoftmacAdapter;
use fuchsia_softmac_port::SoftmacHardware;

/// Immutable values reported through the pinned `DeviceOps` query seams.
#[derive(Clone)]
pub struct ClientSupport {
    pub query: fidl_softmac::WlanSoftmacQueryResponse,
    pub discovery: fidl_softmac::DiscoverySupport,
    pub mac_sublayer: fidl_common::MacSublayerSupport,
    pub security: fidl_common::SecuritySupport,
    pub spectrum_management: fidl_common::SpectrumManagementSupport,
}

/// One received frame and the source-exact receive status delivered with it.
///
/// This intentionally has no `Debug` implementation: an 802.11 frame can
/// contain SAE, RSN, EAPOL, or other secret-adjacent material.
pub struct ClientRxFrame {
    pub bytes: Vec<u8>,
    pub status: fidl_softmac::WlanRxInfo,
}

/// Firmware/DMA effects retained by the offline client boundary.
///
/// Errors are already-mapped Zircon statuses. The adapter forwards them
/// unchanged and never retries or interprets them.
pub trait Mt7921ClientEffects {
    fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        vht_secondary_80_channel: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status>;
    fn join_bss(&mut self, request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status>;
    fn send_wlan_frame(
        &mut self,
        bytes: &[u8],
        flags: fidl_softmac::WlanTxInfoFlags,
    ) -> Result<(), zx::Status>;
    fn install_key(
        &mut self,
        configuration: &fidl_softmac::WlanKeyConfiguration,
    ) -> Result<(), zx::Status>;
    fn notify_association_complete(
        &mut self,
        configuration: &fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status>;
    fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status>;
    fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status>;
    fn next_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status>;
}

trait Mt7921ClientScan {
    fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status>;
    fn start_passive_scan(
        &mut self,
        request: fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status>;
    fn cancel_scan(
        &mut self,
        request: fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status>;
}

struct NoClientScan;

impl Mt7921ClientScan for NoClientScan {
    fn set_channel(
        &mut self,
        _: fidl_ieee80211::ChannelNumber,
        _: fidl_ieee80211::ChannelBandwidth,
        _: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn start_passive_scan(
        &mut self,
        _: fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn cancel_scan(
        &mut self,
        _: fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
}

impl<T: crate::Mt7921PassiveTransport> Mt7921ClientScan for Mt7921SoftmacAdapter<T> {
    fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        SoftmacHardware::set_channel(
            self,
            fidl_softmac::WlanSoftmacBaseSetChannelRequest {
                primary: Some(primary),
                bandwidth: Some(bandwidth),
                vht_secondary_80_channel: Some(secondary),
            },
        )
        .map_err(|_| zx::Status::IO)
    }
    fn start_passive_scan(
        &mut self,
        request: fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        SoftmacHardware::start_passive_scan(self, request).map_err(|_| zx::Status::IO)
    }
    fn cancel_scan(
        &mut self,
        request: fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        SoftmacHardware::cancel_scan(self, request).map_err(|_| zx::Status::IO)
    }
}

/// Explicit boundary for the live TX prerequisite.
///
/// UNIMPLEMENTED: production construction requires both a live beacon-derived
/// channel authorization and completed MT7921 rate/SAR power authorization.
/// This offline gate cannot mint that capability.
pub struct LiveBeaconPowerAuthorization {
    _private: (),
}

pub fn acquire_live_beacon_power_authorization() -> Result<LiveBeaconPowerAuthorization, zx::Status>
{
    Err(zx::Status::NOT_SUPPORTED)
}

/// One-way adapter from production `ClientMlme` effects to MT7921 mechanics.
///
/// Construction is deliberately offline-only. It grants no VFIO, MMIO, DMA,
/// doorbell, TX-enablement, physical-transport, or live authorization access.
pub struct Mt7921ClientDevice<E, S> {
    effects: E,
    scan: S,
    support: ClientSupport,
    event_sink: mpsc::UnboundedSender<fidl_mlme::MlmeEvent>,
    event_stream: Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>,
    minstrel: Option<wlan_mlme::MinstrelWrapper>,
}

impl<E, S> Mt7921ClientDevice<E, S> {
    fn new(effects: E, scan: S, support: ClientSupport) -> Self {
        let (event_sink, event_stream) = mpsc::unbounded();
        Self {
            effects,
            scan,
            support,
            event_sink,
            event_stream: Some(event_stream),
            minstrel: None,
        }
    }

    /// Construct the production adapter only after the separate live
    /// beacon-and-power gate has supplied its unforgeable capability.
    pub fn new_live(
        effects: E,
        scan: S,
        support: ClientSupport,
        _authorization: LiveBeaconPowerAuthorization,
    ) -> Self {
        Self::new(effects, scan, support)
    }

    pub fn effects(&self) -> &E {
        &self.effects
    }
}

impl<E> Mt7921ClientDevice<E, NoClientScan> {
    #[cfg(test)]
    fn new_offline_fake(effects: E, support: ClientSupport) -> Self {
        Self::new(effects, NoClientScan, support)
    }
}

impl<E, T: crate::Mt7921PassiveTransport> Mt7921ClientDevice<E, Mt7921SoftmacAdapter<T>> {
    #[cfg(test)]
    fn new_offline_with_passive(
        effects: E,
        scan: Mt7921SoftmacAdapter<T>,
        support: ClientSupport,
    ) -> Self {
        Self::new(effects, scan, support)
    }
}

impl<E: Mt7921ClientEffects, S> Mt7921ClientDevice<E, S> {
    /// Pop exactly one frame/status pair from the injected RX effect queue.
    pub fn next_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
        self.effects.next_rx()
    }
}

impl<E: Mt7921ClientEffects, S: Mt7921ClientScan> DeviceOps for Mt7921ClientDevice<E, S> {
    async fn wlan_softmac_query_response(
        &mut self,
    ) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status> {
        Ok(self.support.query.clone())
    }

    async fn discovery_support(&mut self) -> Result<fidl_softmac::DiscoverySupport, zx::Status> {
        Ok(self.support.discovery.clone())
    }

    async fn mac_sublayer_support(
        &mut self,
    ) -> Result<fidl_common::MacSublayerSupport, zx::Status> {
        Ok(self.support.mac_sublayer.clone())
    }

    async fn security_support(&mut self) -> Result<fidl_common::SecuritySupport, zx::Status> {
        Ok(self.support.security.clone())
    }

    async fn spectrum_management_support(
        &mut self,
    ) -> Result<fidl_common::SpectrumManagementSupport, zx::Status> {
        Ok(self.support.spectrum_management.clone())
    }

    fn deliver_eth_frame(&mut self, _packet: &[u8]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    fn send_wlan_frame(
        &mut self,
        buffer: ArenaStaticBox<[u8]>,
        tx_flags: fidl_softmac::WlanTxInfoFlags,
        _async_id: Option<fuchsia_trace::Id>,
    ) -> Result<(), zx::Status> {
        self.effects.send_wlan_frame(&buffer, tx_flags)
    }

    async fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
        self.effects.set_link_up(status == LinkStatus::UP)
    }

    async fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        vht_secondary_80_channel: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        match self
            .scan
            .set_channel(primary, bandwidth, vht_secondary_80_channel)
        {
            Err(zx::Status::NOT_SUPPORTED) => {
                self.effects
                    .set_channel(primary, bandwidth, vht_secondary_80_channel)
            }
            result => result,
        }
    }

    async fn set_mac_address(&mut self, _mac_addr: [u8; 6]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn start_passive_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        self.scan.start_passive_scan(request.clone())
    }

    async fn start_active_scan(
        &mut self,
        _request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn cancel_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        self.scan.cancel_scan(request.clone())
    }

    async fn join_bss(&mut self, request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        self.effects.join_bss(request)
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
        configuration: &fidl_softmac::WlanKeyConfiguration,
    ) -> Result<(), zx::Status> {
        self.effects.install_key(configuration)
    }

    async fn notify_association_complete(
        &mut self,
        configuration: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        self.effects.notify_association_complete(&configuration)
    }

    async fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        self.effects.clear_association(request)
    }

    async fn update_wmm_parameters(
        &mut self,
        _request: &fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    fn take_mlme_event_stream(&mut self) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
        self.event_stream.take()
    }

    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
        self.event_sink
            .unbounded_send(event)
            .map_err(|_| anyhow::anyhow!("MLME event queue closed"))
    }

    fn set_minstrel(&mut self, minstrel: wlan_mlme::MinstrelWrapper) {
        self.minstrel = Some(minstrel);
    }

    fn minstrel(&mut self) -> Option<wlan_mlme::MinstrelWrapper> {
        self.minstrel.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum PassiveCall {
        Channel(mt7921_port_spike::CandidateChannel),
        Start(crate::PassiveScanCommand),
        Cancel(u64),
    }

    #[derive(Clone, Default)]
    struct FakePassiveTransport(Arc<Mutex<Vec<PassiveCall>>>);

    #[derive(Debug)]
    struct FakePassiveError;

    impl std::fmt::Display for FakePassiveError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("fake passive transport error")
        }
    }

    impl std::error::Error for FakePassiveError {}

    impl crate::Mt7921PassiveTransport for FakePassiveTransport {
        type Error = FakePassiveError;

        fn set_channel(
            &mut self,
            channel: mt7921_port_spike::CandidateChannel,
        ) -> Result<(), Self::Error> {
            self.0.lock().unwrap().push(PassiveCall::Channel(channel));
            Ok(())
        }

        fn start_passive_scan(
            &mut self,
            command: crate::PassiveScanCommand,
        ) -> Result<(), Self::Error> {
            self.0.lock().unwrap().push(PassiveCall::Start(command));
            Ok(())
        }

        fn cancel_passive_scan(&mut self, scan_id: u64) -> Result<(), Self::Error> {
            self.0.lock().unwrap().push(PassiveCall::Cancel(scan_id));
            Ok(())
        }

        fn next_event(&mut self) -> Result<Option<crate::TransportEvent>, Self::Error> {
            Ok(None)
        }
    }

    fn nic() -> mt7921_port_spike::NicCapability {
        mt7921_port_spike::NicCapability {
            element_count: 2,
            mac_address: Some([2, 0, 0, 0, 0, 1]),
            phy: Some(mt7921_port_spike::NicPhyCapability {
                ht: true,
                vht: true,
                has_5ghz: true,
                max_bandwidth: 2,
                spatial_streams: 2,
                hardware_path: 3,
                he: true,
            }),
            has_6ghz: Some(false),
            chip_capability: None,
            unknown_elements: 0,
        }
    }

    const BSSID: [u8; 6] = [2, 4, 6, 8, 10, 12];
    // Public, synthetic test pattern. This is not key material from a network.
    const FAKE_KEY: [u8; 16] = [0xa5; 16];

    #[derive(Default)]
    struct FakeEffects {
        order: Vec<&'static str>,
        channel: Option<(
            fidl_ieee80211::ChannelNumber,
            fidl_ieee80211::ChannelBandwidth,
            fidl_ieee80211::ChannelNumber,
        )>,
        join: Option<fidl_driver::JoinBssRequest>,
        frame: Option<Vec<u8>>,
        flags: Option<fidl_softmac::WlanTxInfoFlags>,
        key: Option<fidl_softmac::WlanKeyConfiguration>,
        association: Option<fidl_softmac::WlanAssociationConfig>,
        clear: Option<fidl_softmac::WlanSoftmacBaseClearAssociationRequest>,
        link_up: Option<bool>,
        rx: VecDeque<ClientRxFrame>,
        fail_on: Option<&'static str>,
    }

    // Deliberately redacted: frames and keys may contain SAE/RSN material.
    impl std::fmt::Debug for FakeEffects {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FakeEffects")
                .field("order", &self.order)
                .field("channel", &self.channel)
                .field("join", &self.join)
                .field("frame", &self.frame.as_ref().map(|_| "<redacted>"))
                .field("flags", &self.flags)
                .field("key", &self.key.as_ref().map(|_| "<redacted>"))
                .field("association", &self.association)
                .field("clear", &self.clear)
                .field("link_up", &self.link_up)
                .field("rx", &format_args!("{} queued", self.rx.len()))
                .finish()
        }
    }

    impl FakeEffects {
        fn hit(&mut self, name: &'static str) -> Result<(), zx::Status> {
            if self.fail_on == Some(name) {
                return Err(zx::Status::IO_REFUSED);
            }
            self.order.push(name);
            Ok(())
        }
    }

    impl Mt7921ClientEffects for FakeEffects {
        fn set_channel(
            &mut self,
            primary: fidl_ieee80211::ChannelNumber,
            bandwidth: fidl_ieee80211::ChannelBandwidth,
            secondary: fidl_ieee80211::ChannelNumber,
        ) -> Result<(), zx::Status> {
            self.hit("channel")?;
            self.channel = Some((primary, bandwidth, secondary));
            Ok(())
        }

        fn join_bss(&mut self, request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
            self.hit("join")?;
            self.join = Some(request.clone());
            Ok(())
        }

        fn send_wlan_frame(
            &mut self,
            bytes: &[u8],
            flags: fidl_softmac::WlanTxInfoFlags,
        ) -> Result<(), zx::Status> {
            self.hit("frame")?;
            self.frame = Some(bytes.to_vec());
            self.flags = Some(flags);
            Ok(())
        }

        fn install_key(
            &mut self,
            configuration: &fidl_softmac::WlanKeyConfiguration,
        ) -> Result<(), zx::Status> {
            self.hit("key")?;
            self.key = Some(configuration.clone());
            Ok(())
        }

        fn notify_association_complete(
            &mut self,
            configuration: &fidl_softmac::WlanAssociationConfig,
        ) -> Result<(), zx::Status> {
            self.hit("association")?;
            self.association = Some(configuration.clone());
            Ok(())
        }

        fn clear_association(
            &mut self,
            request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
        ) -> Result<(), zx::Status> {
            self.hit("clear")?;
            self.clear = Some(request.clone());
            Ok(())
        }

        fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status> {
            self.hit(if up { "link-up" } else { "link-down" })?;
            self.link_up = Some(up);
            Ok(())
        }

        fn next_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
            if self.fail_on == Some("rx") {
                return Err(zx::Status::IO_REFUSED);
            }
            Ok(self.rx.pop_front())
        }
    }

    fn support() -> ClientSupport {
        ClientSupport {
            query: fidl_softmac::WlanSoftmacQueryResponse {
                sta_addr: Some([1, 2, 3, 4, 5, 6]),
                hardware_capability: Some(0x420),
                ..Default::default()
            },
            discovery: fidl_softmac::DiscoverySupport {
                scan_offload: Some(fidl_softmac::ScanOffloadExtension {
                    supported: Some(false),
                    scan_cancel_supported: Some(false),
                }),
                ..Default::default()
            },
            mac_sublayer: fidl_common::MacSublayerSupport {
                device: Some(fidl_common::DeviceExtension {
                    mac_implementation_type: Some(fidl_common::MacImplementationType::Softmac),
                    ..Default::default()
                }),
                ..Default::default()
            },
            security: fidl_common::SecuritySupport {
                sae: Some(fidl_common::SaeFeature {
                    driver_handler_supported: Some(false),
                    sme_handler_supported: Some(true),
                    hash_to_element_supported: Some(false),
                }),
                ..Default::default()
            },
            spectrum_management: Default::default(),
        }
    }

    fn rx_status(rssi_dbm: i8) -> fidl_softmac::WlanRxInfo {
        fidl_softmac::WlanRxInfo {
            rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
            valid_fields: fidl_softmac::WlanRxInfoValid::CHAN_WIDTH
                | fidl_softmac::WlanRxInfoValid::RSSI,
            phy: fidl_ieee80211::WlanPhyType::Erp,
            data_rate: 12,
            primary: channel(36),
            bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw20,
            vht_secondary_80_channel: channel(0),
            mcs: 3,
            rssi_dbm,
            snr_dbh: 42,
        }
    }

    fn channel(number: u8) -> fidl_ieee80211::ChannelNumber {
        fidl_ieee80211::ChannelNumber {
            band: fidl_ieee80211::WlanBand::FiveGhz,
            number,
        }
    }

    #[test]
    fn forwards_exact_values_bytes_flags_and_order() {
        futures::executor::block_on(async {
            let mut device =
                Mt7921ClientDevice::new_offline_fake(FakeEffects::default(), support());
            assert_eq!(
                device
                    .wlan_softmac_query_response()
                    .await
                    .unwrap()
                    .hardware_capability,
                Some(0x420)
            );
            assert_eq!(
                device
                    .discovery_support()
                    .await
                    .unwrap()
                    .scan_offload
                    .unwrap()
                    .supported,
                Some(false)
            );
            assert_eq!(
                device.mac_sublayer_support().await.unwrap(),
                support().mac_sublayer
            );
            assert_eq!(device.security_support().await.unwrap(), support().security);
            assert_eq!(
                device.spectrum_management_support().await.unwrap(),
                support().spectrum_management
            );

            device
                .set_channel(
                    channel(36),
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    channel(0),
                )
                .await
                .unwrap();
            let join = fidl_driver::JoinBssRequest {
                bssid: Some(BSSID),
                beacon_period: Some(100),
                ..Default::default()
            };
            device.join_bss(&join).await.unwrap();
            let bytes = vec![0xb0, 0x00, 0, 0, 6, 6, 6, 6, 6, 6, 1, 2, 3, 4];
            let flags = fidl_softmac::WlanTxInfoFlags::PROTECTED
                | fidl_softmac::WlanTxInfoFlags::FAVOR_RELIABILITY;
            device
                .send_wlan_frame(bytes.clone().into(), flags, None)
                .unwrap();
            let key = fidl_softmac::WlanKeyConfiguration {
                peer_addr: Some(BSSID),
                key_idx: Some(2),
                key: Some(FAKE_KEY.to_vec()),
                ..Default::default()
            };
            device.install_key(&key).await.unwrap();
            let association = fidl_softmac::WlanAssociationConfig {
                bssid: Some(BSSID),
                aid: Some(7),
                primary: Some(channel(36)),
                ..Default::default()
            };
            device
                .notify_association_complete(association.clone())
                .await
                .unwrap();
            let clear = fidl_softmac::WlanSoftmacBaseClearAssociationRequest {
                peer_addr: Some(BSSID),
            };
            device.clear_association(&clear).await.unwrap();
            device.set_ethernet_status(LinkStatus::UP).await.unwrap();
            device.set_ethernet_status(LinkStatus::DOWN).await.unwrap();

            let effects = device.effects();
            assert_eq!(
                effects.order,
                [
                    "channel",
                    "join",
                    "frame",
                    "key",
                    "association",
                    "clear",
                    "link-up",
                    "link-down"
                ]
            );
            assert_eq!(
                effects.channel,
                Some((
                    channel(36),
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    channel(0)
                ))
            );
            assert_eq!(effects.join.as_ref(), Some(&join));
            assert_eq!(effects.frame.as_deref(), Some(bytes.as_slice()));
            assert_eq!(effects.flags, Some(flags));
            assert_eq!(effects.key.as_ref(), Some(&key));
            assert_eq!(effects.association.as_ref(), Some(&association));
            assert_eq!(effects.clear.as_ref(), Some(&clear));
            assert_eq!(effects.link_up, Some(false));
            let debug = format!("{effects:?}");
            assert!(!debug.contains("165"));
            assert!(debug.contains("<redacted>"));
        });
    }

    #[test]
    fn forwards_failure_status_without_retry_or_later_effect() {
        futures::executor::block_on(async {
            let effects = FakeEffects {
                fail_on: Some("key"),
                ..Default::default()
            };
            let mut device = Mt7921ClientDevice::new_offline_fake(effects, support());
            let key = fidl_softmac::WlanKeyConfiguration {
                key: Some(FAKE_KEY.to_vec()),
                ..Default::default()
            };
            assert_eq!(device.install_key(&key).await, Err(zx::Status::IO_REFUSED));
            assert!(device.effects().order.is_empty());
            assert!(device.effects().key.is_none());
        });
    }

    #[test]
    fn queues_mlme_events_and_exact_rx_status_once() {
        futures::executor::block_on(async {
            let status = rx_status(-47);
            let mut effects = FakeEffects::default();
            effects.rx.push_back(ClientRxFrame {
                bytes: vec![8, 1, 2, 3],
                status,
            });
            let mut device = Mt7921ClientDevice::new_offline_fake(effects, support());
            let mut stream = device.take_mlme_event_stream().unwrap();
            assert!(device.take_mlme_event_stream().is_none());
            let event = fidl_mlme::MlmeEvent::OnScanEnd {
                end: fidl_mlme::ScanEnd {
                    txn_id: 99,
                    code: fidl_mlme::ScanResultCode::CanceledByDriverOrFirmware,
                },
            };
            device.send_mlme_event(event.clone()).unwrap();
            assert_eq!(stream.next().await, Some(event));
            let rx = device.next_rx().unwrap().unwrap();
            assert_eq!(rx.bytes, [8, 1, 2, 3]);
            assert_eq!(rx.status, status);
            assert!(device.next_rx().unwrap().is_none());
        });
    }

    #[test]
    fn closed_event_queue_error_redacts_sae_fields() {
        let mut device = Mt7921ClientDevice::new_offline_fake(FakeEffects::default(), support());
        drop(device.take_mlme_event_stream().unwrap());
        let sae_marker = vec![0xde, 0xad, 0xbe, 0xef];
        let error = device
            .send_mlme_event(fidl_mlme::MlmeEvent::OnSaeFrameRx {
                frame: fidl_mlme::SaeFrame {
                    peer_sta_address: BSSID,
                    status_code: fidl_ieee80211::StatusCode::Success,
                    seq_num: 1,
                    sae_fields: sae_marker,
                },
            })
            .unwrap_err();
        assert_eq!(format!("{error}"), "MLME event queue closed");
        assert_eq!(format!("{error:?}"), "MLME event queue closed");
    }

    #[test]
    fn device_ops_composes_existing_passive_mechanics_for_scan_and_cancel() {
        futures::executor::block_on(async {
            let transport = FakePassiveTransport::default();
            let calls = Arc::clone(&transport.0);
            let capability = nic();
            let passive = Mt7921SoftmacAdapter::new(
                transport,
                capability,
                mt7921_port_spike::candidate_channels(capability),
                vec![channel(36)],
            )
            .unwrap();
            let mut support = support();
            support.discovery.scan_offload = Some(fidl_softmac::ScanOffloadExtension {
                supported: Some(true),
                scan_cancel_supported: Some(true),
            });
            let mut device = Mt7921ClientDevice::new_offline_with_passive(
                FakeEffects::default(),
                passive,
                support,
            );

            device
                .set_channel(
                    channel(36),
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    channel(0),
                )
                .await
                .unwrap();
            let response = device
                .start_passive_scan(&fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest {
                    channels: Some(vec![channel(36)]),
                    min_channel_time: Some(10),
                    max_channel_time: Some(20),
                    min_home_time: Some(0),
                })
                .await
                .unwrap();
            assert_eq!(response.scan_id, Some(1));
            device
                .cancel_scan(&fidl_softmac::WlanSoftmacBaseCancelScanRequest {
                    scan_id: response.scan_id,
                })
                .await
                .unwrap();

            assert!(matches!(
                calls.lock().unwrap().as_slice(),
                [
                    PassiveCall::Channel(_),
                    PassiveCall::Start(crate::PassiveScanCommand { scan_id: 1, .. }),
                    PassiveCall::Cancel(1),
                ]
            ));
            assert!(device.effects().order.is_empty());
        });
    }

    #[test]
    fn live_prerequisite_and_all_unretained_operations_are_unsupported() {
        futures::executor::block_on(async {
            assert_eq!(
                acquire_live_beacon_power_authorization().err(),
                Some(zx::Status::NOT_SUPPORTED)
            );
            let mut device =
                Mt7921ClientDevice::new_offline_fake(FakeEffects::default(), support());
            assert_eq!(
                device.deliver_eth_frame(&[1, 2]),
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.set_mac_address([0; 6]).await,
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.start_passive_scan(&Default::default()).await,
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.start_active_scan(&Default::default()).await,
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.cancel_scan(&Default::default()).await,
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.disable_beaconing().await,
                Err(zx::Status::NOT_SUPPORTED)
            );
            assert_eq!(
                device.update_wmm_parameters(&Default::default()).await,
                Err(zx::Status::NOT_SUPPORTED)
            );
        });
    }
}
