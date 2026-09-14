// SPDX-License-Identifier: GPL-2.0-only

//! Direct protocol/driver boundary. Firmware bootstrap and containment are
//! implemented by the owning driver. Radio operations not ported from the
//! retired lab owner fail explicitly; no compatibility transport is retained.

use crate::{Mt7921Driver, SessionLifecycle};
use wlan_softmac_host::*;

impl WlanSoftmacLifecycle for Mt7921Driver {
    fn start(&mut self, upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status> {
        if self.session.lifecycle != SessionLifecycle::FirmwareInitialized {
            return Err(zx::Status::BAD_STATE);
        }
        self.upcalls = Some(upcalls);
        self.session.lifecycle = SessionLifecycle::ProtocolStarted;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), zx::Status> {
        self.upcalls = None;
        self.session
            .contain()
            .map(|_| ())
            .map_err(|_| zx::Status::IO)
    }
}

impl ClientRuntimeDriver for Mt7921Driver {
    fn drive(&mut self) -> Result<bool, zx::Status> {
        if self.session.lifecycle != SessionLifecycle::ProtocolStarted {
            return Err(zx::Status::BAD_STATE);
        }
        let resources = self
            .session
            .resources
            .as_mut()
            .ok_or(zx::Status::BAD_STATE)?;
        let result = (|| {
            let mut progressed = self.mac_initialization.drive(
                resources,
                &mut self.session.mcu.0,
                &mut self.session.receive,
                self.session.start,
                std::time::Instant::now(),
            )?;
            if !matches!(
                self.mac_initialization,
                crate::radio::MacInitialization::Ready
            ) {
                return Ok(progressed);
            }
            progressed |= self.radio_preparation.drive(
                resources,
                &mut self.session.mcu.0,
                &mut self.session.receive,
                self.session.start,
                std::time::Instant::now(),
            )?;
            if !self.radio_preparation.ready() {
                return Ok(progressed);
            }
            if let Some(scan) = self.scan.as_mut() {
                progressed |= scan.drive(
                    resources,
                    &mut self.session.mcu.0,
                    &mut self.session.receive,
                    self.session.start,
                    std::time::Instant::now(),
                )?;
                if !scan.reclaimed {
                    return Ok(progressed);
                }
            }
            let mut views = resources
                .active_mcu_views(&mut self.session.receive, self.session.start)
                .map_err(|_| zx::Status::IO)?;
            progressed |= self
                .session
                .mcu
                .0
                .poll_events(&mut views, &mut ())
                .map_err(|_| zx::Status::IO)?;
            let (data_progress, routes) =
                self.data_rx.poll(resources).map_err(|_| zx::Status::IO)?;
            progressed |= data_progress;
            let upcalls = self.upcalls.as_mut().ok_or(zx::Status::BAD_STATE)?;
            for route in routes {
                if let mt7921_core::McuRxRoute::Normal(bytes) = route {
                    deliver_raw_rx(upcalls.as_mut(), &bytes);
                }
            }
            let mut event_count = 0;
            for _ in 0..64 {
                let Some(event) = self.session.receive.take_event() else {
                    break;
                };
                event_count += 1;
                match event.into_route().map_err(|_| zx::Status::IO)? {
                    mt7921_core::McuRxRoute::Normal(bytes) => {
                        deliver_raw_rx(upcalls.as_mut(), &bytes)
                    }
                    mt7921_core::McuRxRoute::Firmware(bytes) => {
                        if let Ok(done) = mt7921_core::parse_passive_scan_done(&bytes.bytes) {
                            if let Some(scan) = self.scan.as_mut() {
                                if done.scan_sequence == scan.sequence {
                                    scan.done = Some(done);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !data_progress && event_count == 0 {
                if let Some(scan) = self.scan.as_mut() {
                    if scan.reclaimed && scan.done.is_some() {
                        scan.context.check(std::time::Instant::now())?;
                        let done = scan.done.take().expect("checked above");
                        let status = if usize::from(done.completed_channels) == scan.channels.len()
                        {
                            zx::Status::OK
                        } else {
                            zx::Status::IO
                        };
                        upcalls.notify_scan_complete(status, scan.id);
                        self.scan = None;
                        progressed = true;
                    }
                }
            }
            Ok(progressed)
        })();
        if result.is_err() {
            self.session.lifecycle = SessionLifecycle::Closing;
            self.upcalls = None;
        }
        result
    }

    fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status> {
        if up {
            Err(zx::Status::NOT_SUPPORTED)
        } else {
            Ok(())
        }
    }

    fn reset(&mut self) -> Result<(), zx::Status> {
        self.upcalls = None;
        self.session
            .contain()
            .map(|_| ())
            .map_err(|_| zx::Status::IO)
    }
}

impl WlanSoftmac for Mt7921Driver {
    fn query(&mut self) -> Result<WlanSoftmacQueryResponse, zx::Status> {
        let mac = self
            .firmware
            .nic_capability
            .mac_address
            .ok_or(zx::Status::IO_DATA_INTEGRITY)?;
        Ok(WlanSoftmacQueryResponse {
            sta_addr: Some(mac),
            factory_addr: Some(mac),
            band_caps: Some(
                [
                    fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz,
                    fidl_fuchsia_wlan_ieee80211::WlanBand::FiveGhz,
                ]
                .into_iter()
                .filter_map(|band| {
                    let channels: Vec<_> = self
                        .passive_channels()
                        .into_iter()
                        .filter(|channel| {
                            (channel.band == mt7921_core::PhysicalBand::Ghz2)
                                == (band == fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz)
                        })
                        .map(|channel| fidl_fuchsia_wlan_ieee80211::ChannelNumber {
                            band,
                            number: channel.number as u8,
                        })
                        .collect();
                    (!channels.is_empty()).then_some(
                        fidl_fuchsia_wlan_softmac::WlanSoftmacBandCapability {
                            band: Some(band),
                            primary_channels: Some(channels),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ),
            ..Default::default()
        })
    }

    fn query_discovery_support(&mut self) -> Result<DiscoverySupport, zx::Status> {
        Ok(DiscoverySupport {
            scan_offload: Some(fidl_fuchsia_wlan_softmac::ScanOffloadExtension {
                supported: Some(true),
                scan_cancel_supported: Some(false),
            }),
            ..Default::default()
        })
    }
    fn query_mac_sublayer_support(&mut self) -> Result<MacSublayerSupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_security_support(&mut self) -> Result<SecuritySupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_spectrum_management_support(
        &mut self,
    ) -> Result<SpectrumManagementSupport, zx::Status> {
        Ok(Default::default())
    }
    fn set_channel(
        &mut self,
        _: WlanSoftmacBaseSetChannelRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn join_bss(
        &mut self,
        _: JoinBssRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn install_key(
        &mut self,
        _: WlanKeyConfiguration,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn notify_association_complete(
        &mut self,
        _: WlanAssociationConfig,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn clear_association(
        &mut self,
        _: WlanSoftmacBaseClearAssociationRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready({
            // No join/key/TX operation can currently create association state.
            Ok(())
        })
    }
    fn start_passive_scan(
        &mut self,
        context: OperationContext,
        request: WlanSoftmacBaseStartPassiveScanRequest,
    ) -> impl std::future::Future<
        Output = Result<WlanSoftmacBaseStartPassiveScanResponse, zx::Status>,
    > + 'static {
        let result = (|| {
            context.check(std::time::Instant::now())?;
            if self.session.lifecycle != SessionLifecycle::ProtocolStarted {
                return Err(zx::Status::BAD_STATE);
            }
            if self.scan.is_some() {
                return Err(zx::Status::SHOULD_WAIT);
            }
            let requested = request.channels.ok_or(zx::Status::INVALID_ARGS)?;
            if !(1..=64).contains(&requested.len()) {
                return Err(zx::Status::INVALID_ARGS);
            }
            let allowed = self.passive_channels();
            let mut channels = Vec::with_capacity(requested.len());
            for channel in requested {
                let band = match channel.band {
                    fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz => {
                        mt7921_core::PhysicalBand::Ghz2
                    }
                    fidl_fuchsia_wlan_ieee80211::WlanBand::FiveGhz => {
                        mt7921_core::PhysicalBand::Ghz5
                    }
                    _ => return Err(zx::Status::NOT_SUPPORTED),
                };
                channels.push(
                    *allowed
                        .iter()
                        .find(|allowed| {
                            allowed.band == band && allowed.number == u16::from(channel.number)
                        })
                        .ok_or(zx::Status::NOT_SUPPORTED)?,
                );
            }
            let id = self.next_scan_id;
            self.next_scan_id = id.checked_add(1).ok_or(zx::Status::NO_RESOURCES)?;
            let (reply, receiver) = futures_channel::oneshot::channel();
            self.scan = Some(crate::radio::PassiveScan {
                context,
                id,
                sequence: (id & 0x7f) as u8,
                channels,
                reply: Some(reply),
                published: false,
                reclaimed: false,
                done: None,
            });
            Ok(receiver)
        })();
        async move { result?.await.unwrap_or(Err(zx::Status::CANCELED)) }
    }
    fn start_active_scan(
        &mut self,
        context: wlan_softmac_host::OperationContext,
        _: WlanSoftmacStartActiveScanRequest,
    ) -> impl std::future::Future<
        Output = Result<WlanSoftmacBaseStartActiveScanResponse, zx::Status>,
    > + 'static {
        if let Err(status) = context.check(std::time::Instant::now()) {
            return std::future::ready(Err(status));
        }
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn cancel_scan(
        &mut self,
        _: WlanSoftmacBaseCancelScanRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready({
            if self.scan.is_some() {
                Err(zx::Status::NOT_SUPPORTED)
            } else {
                Ok(())
            }
        })
    }
    fn update_wmm_parameters(
        &mut self,
        _: WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn queue_tx(&mut self, _: &[u8], _: WlanTxInfoFlags) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
}

fn deliver_raw_rx(upcalls: &mut dyn WlanSoftmacUpcalls, bytes: &[u8]) {
    use fidl_fuchsia_wlan_ieee80211::{ChannelBandwidth, ChannelNumber, WlanBand, WlanPhyType};
    use fidl_fuchsia_wlan_softmac::{WlanRxInfoFlags, WlanRxInfoValid};
    let Ok(frame) = mt7921_core::parse_connac2_rx_frame(bytes) else {
        return;
    };
    let band = match frame.band {
        mt7921_core::PhysicalBand::Ghz2 => WlanBand::TwoGhz,
        mt7921_core::PhysicalBand::Ghz5 => WlanBand::FiveGhz,
        mt7921_core::PhysicalBand::Ghz6 => return,
    };
    upcalls.recv(
        frame.bytes,
        WlanRxInfo {
            rx_flags: WlanRxInfoFlags::empty(),
            valid_fields: WlanRxInfoValid::RSSI,
            phy: WlanPhyType::Ofdm,
            data_rate: 0,
            primary: ChannelNumber {
                band,
                number: frame.channel,
            },
            bandwidth: ChannelBandwidth::Cbw20,
            vht_secondary_80_channel: ChannelNumber { band, number: 0 },
            mcs: 0,
            rssi_dbm: frame.rssi_dbm,
            snr_dbh: 0,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Upcalls(Vec<(Vec<u8>, WlanRxInfo)>);
    impl WlanSoftmacUpcalls for Upcalls {
        fn recv(&mut self, bytes: Vec<u8>, info: WlanRxInfo) {
            self.0.push((bytes, info));
        }
        fn report_tx_result(&mut self, _: WlanTxResult) {}
        fn notify_scan_complete(&mut self, _: zx::Status, _: u64) {}
    }

    #[test]
    fn raw_rx_preserves_beacon_and_only_marks_observed_signal_valid() {
        let mut bytes = vec![0; 24 + 8 + 36 + 5];
        let length = bytes.len() as u32;
        bytes[..4].copy_from_slice(&((2 << 27) | length).to_le_bytes());
        bytes[4..8].copy_from_slice(&(1u32 << 13).to_le_bytes());
        bytes[12..16].copy_from_slice(&(36u32 << 8).to_le_bytes());
        bytes[28..32].copy_from_slice(&0x7878u32.to_le_bytes());
        bytes[32] = 0x80;
        bytes[68..].copy_from_slice(&[0, 3, b'l', b'a', b'b']);
        let mut upcalls = Upcalls::default();
        deliver_raw_rx(&mut upcalls, &bytes);
        assert_eq!(upcalls.0.len(), 1);
        let (frame, info) = &upcalls.0[0];
        assert_eq!(frame, &bytes[32..]);
        assert_eq!(info.primary.number, 36);
        assert_eq!(
            info.primary.band,
            fidl_fuchsia_wlan_ieee80211::WlanBand::FiveGhz
        );
        assert_eq!(info.rssi_dbm, -50);
        assert_eq!(
            info.valid_fields,
            fidl_fuchsia_wlan_softmac::WlanRxInfoValid::RSSI
        );
        bytes[4..8].copy_from_slice(&((1u32 << 13) | (1 << 28)).to_le_bytes());
        deliver_raw_rx(&mut upcalls, &bytes);
        deliver_raw_rx(&mut upcalls, &[0; 4]);
        assert_eq!(upcalls.0.len(), 1);
    }
}
