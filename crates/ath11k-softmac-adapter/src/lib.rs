// SPDX-License-Identifier: GPL-2.0-only

//! Ath11k client binding for the chip-neutral synchronous SoftMAC contract.

use ath11k_core::{
    ClientRadioControl as _, Device, DeviceState, EventSource as _, Lifecycle as _,
    ModelSubsystems, RadioControl as _, ScanConfig, ScanId, Subsystems, VdevId, WCN6750, WlanEvent,
};
use fidl_fuchsia_wlan_ieee80211::{ChannelBandwidth, ChannelNumber, WlanBand, WlanPhyType};
use wlan_softmac_host::{
    ClientRuntimeDriver, DiscoverySupport, JoinBssRequest, MacSublayerSupport, SecuritySupport,
    SpectrumManagementSupport, WlanAssociationConfig, WlanKeyConfiguration, WlanRxInfo,
    WlanSoftmac, WlanSoftmacBaseCancelScanRequest, WlanSoftmacBaseClearAssociationRequest,
    WlanSoftmacBaseSetChannelRequest, WlanSoftmacBaseStartActiveScanResponse,
    WlanSoftmacBaseStartPassiveScanRequest, WlanSoftmacBaseStartPassiveScanResponse,
    WlanSoftmacBaseUpdateWmmParametersRequest, WlanSoftmacLifecycle, WlanSoftmacQueryResponse,
    WlanSoftmacStartActiveScanRequest, WlanSoftmacUpcalls, WlanTxInfoFlags, WlanTxResult,
};

const SCAN_EVENT_COMPLETED: u32 = 1 << 1;
const DP_WORK_BUDGET: usize = 64;
const DP_RECEIVE_BUDGET: usize = 1;

fn status(error: ath11k_core::CoreError) -> zx::Status {
    match error {
        ath11k_core::CoreError::WrongState => zx::Status::BAD_STATE,
        ath11k_core::CoreError::NoResources => zx::Status::NO_RESOURCES,
        ath11k_core::CoreError::NotFound => zx::Status::NOT_FOUND,
        ath11k_core::CoreError::Protocol => zx::Status::IO_INVALID,
        ath11k_core::CoreError::DeviceFault => zx::Status::IO,
    }
}

fn channel_frequency(channel: ChannelNumber) -> Result<u16, zx::Status> {
    match channel.band {
        WlanBand::TwoGhz if channel.number == 14 => Ok(2484),
        WlanBand::TwoGhz if (1..=13).contains(&channel.number) => {
            Ok(2407 + 5 * u16::from(channel.number))
        }
        WlanBand::FiveGhz if channel.number != 0 => Ok(5000 + 5 * u16::from(channel.number)),
        _ => Err(zx::Status::INVALID_ARGS),
    }
}

fn frequency_channel(frequency: u16) -> ChannelNumber {
    if frequency == 2484 {
        ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 14,
        }
    } else if frequency < 3000 {
        ChannelNumber {
            band: WlanBand::TwoGhz,
            number: frequency.saturating_sub(2407).saturating_div(5) as u8,
        }
    } else {
        ChannelNumber {
            band: WlanBand::FiveGhz,
            number: frequency.saturating_sub(5000).saturating_div(5) as u8,
        }
    }
}

/// Owns an ath11k client device and its installed host callbacks.
///
/// The same type composes the deterministic [`ModelSubsystems`] and the real
/// `Wcn6750Subsystems`; chip-private resources stay below `Device<B>`.
pub struct Ath11kClientDevice<B: Subsystems> {
    device: Device<B>,
    mac: [u8; 6],
    vdev: Option<VdevId>,
    peer: Option<[u8; 6]>,
    upcalls: Option<Box<dyn WlanSoftmacUpcalls>>,
    next_scan_id: u32,
    active_scan: Option<u32>,
    deterministic_scan_completion: bool,
}

impl<B: Subsystems> Ath11kClientDevice<B> {
    pub fn new(device: Device<B>, mac: [u8; 6]) -> Self {
        Self {
            device,
            mac,
            vdev: None,
            peer: None,
            upcalls: None,
            next_scan_id: 1,
            active_scan: None,
            deterministic_scan_completion: false,
        }
    }

    pub fn into_device(self) -> Device<B> {
        self.device
    }

    fn ready_vdev(&self) -> Result<VdevId, zx::Status> {
        if self.upcalls.is_none() {
            return Err(zx::Status::BAD_STATE);
        }
        self.vdev.ok_or(zx::Status::BAD_STATE)
    }
}

impl Ath11kClientDevice<ModelSubsystems> {
    /// Deterministic backend used by the shared host conformance runner.
    pub fn deterministic(mac: [u8; 6]) -> Self {
        let mut device = Self::new(WCN6750.device(ModelSubsystems::default()), mac);
        device.deterministic_scan_completion = true;
        device
    }
}

impl<B: Subsystems> WlanSoftmacLifecycle for Ath11kClientDevice<B> {
    fn start(&mut self, upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status> {
        if self.upcalls.is_some() || self.device.state() != DeviceState::Allocated {
            return Err(zx::Status::BAD_STATE);
        }
        self.device.probe().map_err(status)?;
        self.device.attach_firmware().map_err(status)?;
        self.device.start_radio().map_err(status)?;
        match self.device.create_client_vdev(self.mac) {
            Ok(vdev) => self.vdev = Some(vdev),
            Err(error) => {
                let _ = self.device.stop();
                return Err(status(error));
            }
        }
        self.upcalls = Some(upcalls);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), zx::Status> {
        self.upcalls = None;
        self.active_scan = None;
        self.peer = None;
        self.vdev = None;
        self.device.stop().map_err(status)
    }
}

#[derive(Default)]
struct DpDeliveries {
    rx: Vec<ath11k_dp::tx::HostRxFrame>,
    tx: Vec<ath11k_dp::tx::TxResult>,
}

impl ath11k_dp::tx::DpHost for DpDeliveries {
    fn receive(&mut self, frame: ath11k_dp::tx::HostRxFrame) {
        self.rx.push(frame);
    }

    fn tx_complete(&mut self, result: ath11k_dp::tx::TxResult) {
        self.tx.push(result);
    }
}

impl<B: Subsystems> ClientRuntimeDriver for Ath11kClientDevice<B> {
    fn drive(&mut self) -> Result<bool, zx::Status> {
        self.ready_vdev()?;
        if self.deterministic_scan_completion
            && let Some(scan_id) = self.active_scan.take()
        {
            self.upcalls
                .as_mut()
                .unwrap()
                .notify_scan_complete(zx::Status::OK, u64::from(scan_id));
            return Ok(true);
        }

        let mut deliveries = DpDeliveries::default();
        let serviced = self
            .device
            .service_dp_host(DP_WORK_BUDGET, DP_RECEIVE_BUDGET, &mut deliveries)
            .map_err(status)?;
        let mut progressed = serviced.tx_delivered != 0
            || serviced.tx_malformed != 0
            || serviced.rx_delivered != 0
            || serviced.rx_dropped != Default::default();
        for frame in deliveries.rx {
            let frequency = frame.info.phy_metadata as u16;
            self.upcalls.as_mut().unwrap().recv(
                frame.bytes,
                WlanRxInfo {
                    rx_flags: fidl_fuchsia_wlan_softmac::WlanRxInfoFlags::empty(),
                    valid_fields: fidl_fuchsia_wlan_softmac::WlanRxInfoValid::MCS,
                    phy: WlanPhyType::Ofdm,
                    data_rate: 0,
                    primary: frequency_channel(frequency),
                    bandwidth: ChannelBandwidth::Cbw20,
                    vht_secondary_80_channel: ChannelNumber {
                        band: frequency_channel(frequency).band,
                        number: 0,
                    },
                    mcs: frame.info.mcs,
                    rssi_dbm: 0,
                    snr_dbh: 0,
                },
            );
        }
        for completion in deliveries.tx {
            let peer_addr = self.peer.ok_or(zx::Status::BAD_STATE)?;
            let mut entries = [fidl_fuchsia_wlan_softmac::WlanTxResultEntry {
                tx_vector_idx: 0,
                attempts: 0,
            };
                fidl_fuchsia_wlan_softmac::WLAN_TX_RESULT_MAX_ENTRY as usize];
            entries[0].attempts = 1;
            self.upcalls
                .as_mut()
                .unwrap()
                .report_tx_result(WlanTxResult {
                    tx_result_entry: entries,
                    peer_addr,
                    result_code: if completion.acknowledged {
                        fidl_fuchsia_wlan_softmac::WlanTxResultCode::Success
                    } else {
                        fidl_fuchsia_wlan_softmac::WlanTxResultCode::Failed
                    },
                });
        }

        if let Some(event) = self.device.next_wlan_event().map_err(status)? {
            progressed = true;
            match event {
                WlanEvent::ManagementReceived {
                    channel_mhz,
                    rssi,
                    frame,
                    ..
                } => {
                    let primary = frequency_channel(channel_mhz as u16);
                    self.upcalls.as_mut().unwrap().recv(
                        frame,
                        WlanRxInfo {
                            rx_flags: fidl_fuchsia_wlan_softmac::WlanRxInfoFlags::empty(),
                            valid_fields: fidl_fuchsia_wlan_softmac::WlanRxInfoValid::RSSI,
                            phy: WlanPhyType::Ofdm,
                            data_rate: 0,
                            primary,
                            bandwidth: ChannelBandwidth::Cbw20,
                            vht_secondary_80_channel: ChannelNumber {
                                band: primary.band,
                                number: 0,
                            },
                            mcs: 0,
                            rssi_dbm: rssi.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8,
                            snr_dbh: 0,
                        },
                    );
                }
                WlanEvent::Scan {
                    event_type,
                    reason,
                    scan_id,
                    ..
                } if self.active_scan == Some(scan_id)
                    && event_type & SCAN_EVENT_COMPLETED != 0 =>
                {
                    self.active_scan = None;
                    self.upcalls.as_mut().unwrap().notify_scan_complete(
                        if reason == 0 {
                            zx::Status::OK
                        } else {
                            zx::Status::IO
                        },
                        u64::from(scan_id),
                    );
                }
                _ => {}
            }
        }
        Ok(progressed)
    }

    fn set_link_up(&mut self, _up: bool) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    fn reset(&mut self) -> Result<(), zx::Status> {
        self.stop()
    }
}

impl<B: Subsystems> WlanSoftmac for Ath11kClientDevice<B> {
    fn query(&mut self) -> Result<WlanSoftmacQueryResponse, zx::Status> {
        Ok(WlanSoftmacQueryResponse {
            sta_addr: Some(self.mac),
            ..Default::default()
        })
    }
    fn query_discovery_support(&mut self) -> Result<DiscoverySupport, zx::Status> {
        Ok(Default::default())
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

    fn set_channel(&mut self, request: WlanSoftmacBaseSetChannelRequest) -> Result<(), zx::Status> {
        let primary = request.primary.ok_or(zx::Status::INVALID_ARGS)?;
        if request.bandwidth != Some(ChannelBandwidth::Cbw20)
            || request.vht_secondary_80_channel.is_none()
        {
            return Err(zx::Status::INVALID_ARGS);
        }
        let vdev = self.ready_vdev()?;
        self.device
            .start_vdev(vdev, channel_frequency(primary)?)
            .map_err(status)
    }

    fn join_bss(&mut self, request: JoinBssRequest) -> Result<(), zx::Status> {
        let peer = request.bssid.ok_or(zx::Status::INVALID_ARGS)?;
        self.device
            .create_peer(self.ready_vdev()?, peer)
            .map_err(status)?;
        self.peer = Some(peer);
        Ok(())
    }
    fn install_key(&mut self, _configuration: WlanKeyConfiguration) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn notify_association_complete(
        &mut self,
        _configuration: WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn clear_association(
        &mut self,
        _request: WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    fn start_passive_scan(
        &mut self,
        request: WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        if self.active_scan.is_some() {
            return Err(zx::Status::BAD_STATE);
        }
        let channels = request.channels.ok_or(zx::Status::INVALID_ARGS)?;
        if channels.is_empty() {
            return Err(zx::Status::INVALID_ARGS);
        }
        let channels_mhz = channels
            .into_iter()
            .map(channel_frequency)
            .collect::<Result<Vec<_>, _>>()?;
        let scan_id = self.next_scan_id;
        self.next_scan_id = self
            .next_scan_id
            .checked_add(1)
            .ok_or(zx::Status::NO_RESOURCES)?;
        self.device
            .start_scan(ScanConfig {
                vdev: self.ready_vdev()?,
                id: ScanId(scan_id),
                active: false,
                channels_mhz,
                ssids: Vec::new(),
            })
            .map_err(status)?;
        self.active_scan = Some(scan_id);
        Ok(WlanSoftmacBaseStartPassiveScanResponse {
            scan_id: Some(u64::from(scan_id)),
        })
    }
    fn start_active_scan(
        &mut self,
        _request: WlanSoftmacStartActiveScanRequest,
    ) -> Result<WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn cancel_scan(&mut self, request: WlanSoftmacBaseCancelScanRequest) -> Result<(), zx::Status> {
        let scan_id = u32::try_from(request.scan_id.ok_or(zx::Status::INVALID_ARGS)?)
            .map_err(|_| zx::Status::INVALID_ARGS)?;
        if self.active_scan != Some(scan_id) {
            return Err(zx::Status::NOT_FOUND);
        }
        self.device
            .stop_scan(self.ready_vdev()?, ScanId(scan_id))
            .map_err(status)?;
        self.active_scan = None;
        Ok(())
    }
    fn update_wmm_parameters(
        &mut self,
        _request: WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn queue_tx(&mut self, _bytes: &[u8], _flags: WlanTxInfoFlags) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wlan_softmac_host::conformance::{expected_client_conformance, run_client_conformance};

    const CLIENT: [u8; 6] = [2, 0, 0, 0, 0, 1];

    #[test]
    fn deterministic_ath11k_passes_the_generic_client_contract() {
        let channel = ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 6,
        };
        assert_eq!(
            run_client_conformance(Ath11kClientDevice::deterministic(CLIENT), channel).unwrap(),
            expected_client_conformance(CLIENT, 1)
        );
    }
}
