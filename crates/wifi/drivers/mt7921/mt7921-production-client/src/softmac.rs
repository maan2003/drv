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
            if let Some(change) = self.channel_change.as_mut() {
                progressed |= change.drive(
                    resources,
                    &mut self.session.mcu.0,
                    &mut self.session.receive,
                    self.session.start,
                    std::time::Instant::now(),
                )?;
                if !change.complete() {
                    return Ok(progressed);
                }
                change.context.check(std::time::Instant::now())?;
                self.current_channel = Some(change.channel);
                if let Some(reply) = change.reply.take() {
                    let _ = reply.send(Ok(()));
                }
                self.channel_change = None;
            }
            if let Some(join) = self.peer_join.as_mut() {
                progressed |= join.drive(
                    resources,
                    &mut self.session.mcu.0,
                    &mut self.session.receive,
                    self.session.start,
                    std::time::Instant::now(),
                )?;
                if !join.complete() {
                    return Ok(progressed);
                }
                join.context.check(std::time::Instant::now())?;
                self.joined = Some(join.bss.clone());
                if let Some(reply) = join.reply.take() {
                    let _ = reply.send(Ok(()));
                }
                self.peer_join = None;
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
            let (io_progress, receive_idle, routes) = drive_radio_io(
                resources,
                &mut self.session.mcu.0,
                &mut self.session.receive,
                &mut self.data_rx,
                &mut self.management_tx,
                self.session.start,
                std::time::Instant::now(),
            )?;
            progressed |= io_progress;
            let upcalls = self.upcalls.as_mut().ok_or(zx::Status::BAD_STATE)?;
            for route in routes {
                match route {
                    mt7921_core::McuRxRoute::Normal(bytes) => {
                        deliver_raw_rx(upcalls.as_mut(), &mut self.observations, &bytes)
                    }
                    mt7921_core::McuRxRoute::TxFree(free) => self.management_tx.tx_free(free)?,
                    mt7921_core::McuRxRoute::TxStatus(status) => {
                        self.management_tx.tx_status(status)?
                    }
                    mt7921_core::McuRxRoute::Firmware(bytes) => {
                        if let Ok(grant) = mt7921_core::parse_client_join_roc_grant(&bytes.bytes) {
                            self.management_tx
                                .roc_grant(grant, std::time::Instant::now())?;
                        }
                        if let Ok(done) = mt7921_core::parse_passive_scan_done(&bytes.bytes) {
                            if let Some(scan) = self.scan.as_mut() {
                                if done.scan_sequence == scan.sequence {
                                    scan.done = Some(done);
                                }
                            }
                        }
                    }
                }
            }
            if receive_idle {
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
        if let Err(status) = result {
            if let Some(change) = self.channel_change.as_mut()
                && let Some(reply) = change.reply.take()
            {
                let _ = reply.send(Err(status));
            }
            if let Some(join) = self.peer_join.as_mut()
                && let Some(reply) = join.reply.take()
            {
                let _ = reply.send(Err(status));
            }
            self.current_channel = None;
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
            mac_role: Some(fidl_fuchsia_wlan_common::WlanMacRole::Client),
            hardware_capability: Some(0),
            supported_phys: Some(vec![fidl_fuchsia_wlan_ieee80211::WlanPhyType::Ofdm]),
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
                            // Non-HT OFDM rates supported by the radio (500 kbit/s).
                            basic_rates: Some(crate::peer::OFDM_RATES.to_vec()),
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
        context: wlan_softmac_host::OperationContext,
        request: WlanSoftmacBaseSetChannelRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        let result = (|| {
            context.check(std::time::Instant::now())?;
            if self.session.lifecycle != SessionLifecycle::ProtocolStarted {
                return Err(zx::Status::BAD_STATE);
            }
            if self.scan.is_some() || self.channel_change.is_some() || self.peer_join.is_some() {
                return Err(zx::Status::SHOULD_WAIT);
            }
            let primary = request.primary.ok_or(zx::Status::INVALID_ARGS)?;
            if request.bandwidth != Some(fidl_fuchsia_wlan_ieee80211::ChannelBandwidth::Cbw20)
                || request
                    .vht_secondary_80_channel
                    .is_none_or(|channel| channel.number != 0)
            {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let band = match primary.band {
                fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz => mt7921_core::PhysicalBand::Ghz2,
                fidl_fuchsia_wlan_ieee80211::WlanBand::FiveGhz => mt7921_core::PhysicalBand::Ghz5,
                _ => return Err(zx::Status::NOT_SUPPORTED),
            };
            let channel = self
                .passive_channels()
                .into_iter()
                .find(|channel| channel.band == band && channel.number == u16::from(primary.number))
                .ok_or(zx::Status::NOT_SUPPORTED)?;
            // This client currently advertises OFDM only; do not tune for
            // association on the regdb's NO_OFDM channel 14.
            if self.regulatory.channels().iter().any(|rule| {
                rule.band == band
                    && rule.channel == channel.number
                    && rule.regulatory_flags & (1 << 6) != 0
            }) {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let (reply, receiver) = futures_channel::oneshot::channel();
            if self.current_channel == Some(channel) {
                let _ = reply.send(Ok(()));
                return Ok(receiver);
            }
            if self.joined.is_some() {
                return Err(zx::Status::BAD_STATE);
            }
            self.channel_change = Some(crate::radio::ChannelChange::new(context, channel, reply)?);
            self.current_channel = None;
            Ok(receiver)
        })();
        async move { result?.await.unwrap_or(Err(zx::Status::CANCELED)) }
    }
    fn join_bss(
        &mut self,
        context: wlan_softmac_host::OperationContext,
        request: JoinBssRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        let result = (|| {
            let now = std::time::Instant::now();
            context.check(now)?;
            if self.session.lifecycle != SessionLifecycle::ProtocolStarted {
                return Err(zx::Status::BAD_STATE);
            }
            if self.scan.is_some() || self.channel_change.is_some() || self.peer_join.is_some() {
                return Err(zx::Status::SHOULD_WAIT);
            }
            if self.joined.is_some() {
                return Err(zx::Status::BAD_STATE);
            }
            if request.remote != Some(true)
                || request.bss_type != Some(fidl_fuchsia_wlan_ieee80211::BssType::Infrastructure)
            {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let bssid = request.bssid.ok_or(zx::Status::INVALID_ARGS)?;
            let channel = self.current_channel.ok_or(zx::Status::BAD_STATE)?;
            let bss = self
                .observations
                .iter()
                .find(|bss| {
                    bss.bssid == bssid
                        && bss.channel == channel
                        && Some(bss.beacon_period) == request.beacon_period
                        && bss.fresh(now)
                })
                .cloned()
                .ok_or(zx::Status::NOT_FOUND)?;
            let (reply, receiver) = futures_channel::oneshot::channel();
            self.peer_join = Some(crate::peer::PeerJoin::new(context, bss, reply)?);
            Ok(receiver)
        })();
        async move { result?.await.unwrap_or(Err(zx::Status::CANCELED)) }
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
            // Firmware peer removal is not implemented yet. Never certify a
            // programmed (or uncertain) peer as cleared; the owner must contain.
            if self.joined.is_some() || self.peer_join.is_some() {
                Err(zx::Status::NOT_SUPPORTED)
            } else {
                Ok(())
            }
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
            if self.scan.is_some() || self.channel_change.is_some() || self.peer_join.is_some() {
                return Err(zx::Status::SHOULD_WAIT);
            }
            if self.joined.is_some() || !self.management_tx.idle() {
                return Err(zx::Status::NOT_SUPPORTED);
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
    fn queue_tx(
        &mut self,
        context: wlan_softmac_host::OperationContext,
        bytes: &[u8],
        flags: WlanTxInfoFlags,
    ) -> Result<(), zx::Status> {
        context.check(std::time::Instant::now())?;
        if self.session.lifecycle != SessionLifecycle::ProtocolStarted {
            return Err(zx::Status::BAD_STATE);
        }
        if self.scan.is_some() || self.channel_change.is_some() || self.peer_join.is_some() {
            return Err(zx::Status::SHOULD_WAIT);
        }
        let peer = self.joined.as_ref().ok_or(zx::Status::BAD_STATE)?;
        let local = self
            .firmware
            .nic_capability
            .mac_address
            .ok_or(zx::Status::BAD_STATE)?;
        if self.current_channel != Some(peer.channel)
            || bytes.get(4..10) != Some(peer.bssid.as_slice())
            || bytes.get(10..16) != Some(local.as_slice())
            || bytes.get(16..22) != Some(peer.bssid.as_slice())
        {
            return Err(zx::Status::INVALID_ARGS);
        }
        if flags.contains(WlanTxInfoFlags::PROTECTED) {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        self.management_tx
            .enqueue(context, bytes, peer.management_rate(), peer.channel)
    }
}

/// One operational radio I/O turn shared by the physical driver and model.
/// The command pump owns correlated MCU RX while busy; already collected
/// events and the independent data RX ring are still serviced every turn.
pub(super) fn drive_radio_io<B: drv_hardware::Backend>(
    resources: &mut crate::OwnedHardwareResources<B>,
    mechanics: &mut mt7921_core::LoaderMechanics,
    receive: &mut crate::receive::RxRouting,
    data_rx: &mut crate::receive::DataRx,
    management_tx: &mut crate::transmit::ManagementTx,
    start: std::time::Instant,
    now: std::time::Instant,
) -> Result<(bool, bool, Vec<mt7921_core::McuRxRoute>), zx::Status> {
    let mut progressed = management_tx.drive(resources, mechanics, receive, start, now)?;
    if mechanics.active_command_slot().is_none() {
        let mut views = resources
            .active_mcu_views(receive, start)
            .map_err(|_| zx::Status::IO)?;
        progressed |= mechanics
            .poll_events(&mut views, &mut ())
            .map_err(|_| zx::Status::IO)?;
    }
    let (data_progress, mut routes) = data_rx.poll(resources).map_err(|_| zx::Status::IO)?;
    progressed |= data_progress;
    let mut event_count = 0;
    for _ in 0..64 {
        let Some(event) = receive.take_event() else {
            break;
        };
        event_count += 1;
        routes.push(event.into_route().map_err(|_| zx::Status::IO)?);
    }
    Ok((
        progressed || event_count != 0,
        !data_progress && event_count == 0,
        routes,
    ))
}

fn deliver_raw_rx(
    upcalls: &mut dyn WlanSoftmacUpcalls,
    observations: &mut std::collections::VecDeque<crate::peer::ObservedBss>,
    bytes: &[u8],
) {
    use fidl_fuchsia_wlan_ieee80211::{ChannelBandwidth, ChannelNumber, WlanBand, WlanPhyType};
    use fidl_fuchsia_wlan_softmac::{WlanRxInfoFlags, WlanRxInfoValid};
    let Ok(frame) = mt7921_core::parse_connac2_rx_frame(bytes) else {
        return;
    };
    if let Some(observation) = crate::peer::ObservedBss::from_rx(&frame, std::time::Instant::now())
    {
        observations
            .retain(|old| old.bssid != observation.bssid || old.channel != observation.channel);
        if observations.len() == crate::peer::OBSERVATION_CAPACITY {
            observations.pop_front();
        }
        observations.push_back(observation);
    }
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
        let mut observations = Default::default();
        deliver_raw_rx(&mut upcalls, &mut observations, &bytes);
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
        deliver_raw_rx(&mut upcalls, &mut observations, &bytes);
        deliver_raw_rx(&mut upcalls, &mut observations, &[0; 4]);
        assert_eq!(upcalls.0.len(), 1);
    }
}
