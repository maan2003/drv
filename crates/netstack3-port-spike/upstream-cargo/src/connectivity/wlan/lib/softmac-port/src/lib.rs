// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host-portable extraction of the pinned Fuchsia SoftMAC passive scanner.

pub use fidl_fuchsia_wlan_ieee80211::{BssDescription, ChannelBandwidth, ChannelNumber, WlanBand};
pub use fidl_fuchsia_wlan_mlme::{ScanEnd, ScanRequest, ScanResult, ScanResultCode, ScanTypes};
pub use fidl_fuchsia_wlan_softmac::{
    DiscoverySupport, WlanSoftmacBaseSetChannelRequest, WlanSoftmacBaseStartPassiveScanRequest,
    WlanSoftmacBaseStartPassiveScanResponse, WlanSoftmacQueryResponse,
};

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdvertisementKind {
    Beacon,
    ProbeResponse,
}

/// A hardware observation already converted with Fuchsia's MLME value model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanObservation {
    pub kind: AdvertisementKind,
    pub timestamp_nanos: i64,
    pub bss: BssDescription,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HardwareScanEvent {
    Observation(ScanObservation),
    Complete { scan_id: u64, success: bool },
}

/// Minimal host-independent hardware edge needed by the scan milestone.
///
/// Capability and channel candidates remain the pinned Fuchsia schema types.
/// Implementations own all execution and must not infer regulatory permission
/// from the mere presence of a channel in `query_response`.
pub trait SoftmacHardware {
    type Error: Error;

    fn query_response(&self) -> &WlanSoftmacQueryResponse;
    fn discovery_support(&self) -> &DiscoverySupport;
    fn set_channel(&mut self, request: WlanSoftmacBaseSetChannelRequest)
    -> Result<(), Self::Error>;
    fn start_passive_scan(
        &mut self,
        request: WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<WlanSoftmacBaseStartPassiveScanResponse, Self::Error>;
    fn next_scan_event(&mut self) -> Result<Option<HardwareScanEvent>, Self::Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScanError<E> {
    Busy,
    EmptyChannelList,
    MaxChannelTimeLtMin,
    ScanOffloadNotSupported,
    InvalidResponse,
    Hardware(E),
}

impl<E: fmt::Display> fmt::Display for ScanError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("scanner is busy"),
            Self::EmptyChannelList => f.write_str("invalid arg: empty channel list"),
            Self::MaxChannelTimeLtMin => {
                f.write_str("invalid arg: max_channel_time < min_channel_time")
            }
            Self::ScanOffloadNotSupported => f.write_str("scan offload is not supported"),
            Self::InvalidResponse => f.write_str("invalid scan response"),
            Self::Hardware(error) => write!(f, "hardware scan failed: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for ScanError<E> {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MlmeScanEvent {
    Result {
        kind: AdvertisementKind,
        result: ScanResult,
    },
    End(ScanEnd),
}

/// Passive offload-scan state extracted from Fuchsia's `Scanner`.
#[derive(Debug, Default)]
pub struct PassiveScanner {
    ongoing_scan: Option<OngoingScan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OngoingScan {
    mlme_txn_id: u64,
    device_scan_id: u64,
}

impl PassiveScanner {
    pub fn is_scanning(&self) -> bool {
        self.ongoing_scan.is_some()
    }

    pub fn start<H: SoftmacHardware>(
        &mut self,
        hardware: &mut H,
        request: ScanRequest,
    ) -> Result<(), ScanError<H::Error>> {
        if self.ongoing_scan.is_some() {
            return Err(ScanError::Busy);
        }
        if request.channel_list.is_empty() {
            return Err(ScanError::EmptyChannelList);
        }
        if request.max_channel_time < request.min_channel_time {
            return Err(ScanError::MaxChannelTimeLtMin);
        }
        if request.scan_type != ScanTypes::Passive
            || !hardware
                .discovery_support()
                .scan_offload
                .as_ref()
                .and_then(|support| support.supported)
                .unwrap_or(false)
        {
            return Err(ScanError::ScanOffloadNotSupported);
        }

        // IEEE 802.11 Time Units are 1024 microseconds. This retains the
        // pinned scanner's TimeUnit -> MonotonicDuration conversion.
        let response = hardware
            .start_passive_scan(WlanSoftmacBaseStartPassiveScanRequest {
                channels: Some(request.channel_list),
                min_channel_time: Some(time_units_to_nanos(request.min_channel_time)),
                max_channel_time: Some(time_units_to_nanos(request.max_channel_time)),
                min_home_time: Some(0),
            })
            .map_err(ScanError::Hardware)?;
        let device_scan_id = response.scan_id.ok_or(ScanError::InvalidResponse)?;
        self.ongoing_scan = Some(OngoingScan {
            mlme_txn_id: request.txn_id,
            device_scan_id,
        });
        Ok(())
    }

    pub fn poll<H: SoftmacHardware>(
        &mut self,
        hardware: &mut H,
    ) -> Result<Option<MlmeScanEvent>, ScanError<H::Error>> {
        let Some(event) = hardware.next_scan_event().map_err(ScanError::Hardware)? else {
            return Ok(None);
        };
        let Some(scan) = self.ongoing_scan else {
            return Ok(None);
        };
        match event {
            HardwareScanEvent::Observation(observation) => Ok(Some(MlmeScanEvent::Result {
                kind: observation.kind,
                result: ScanResult {
                    txn_id: scan.mlme_txn_id,
                    timestamp_nanos: observation.timestamp_nanos,
                    bss: observation.bss,
                },
            })),
            HardwareScanEvent::Complete { scan_id, success } if scan_id == scan.device_scan_id => {
                self.ongoing_scan = None;
                Ok(Some(MlmeScanEvent::End(ScanEnd {
                    txn_id: scan.mlme_txn_id,
                    code: if success {
                        ScanResultCode::Success
                    } else {
                        ScanResultCode::InternalError
                    },
                })))
            }
            HardwareScanEvent::Complete { .. } => Ok(None),
        }
    }
}

fn time_units_to_nanos(time_units: u32) -> i64 {
    i64::from(time_units) * 1_024_000
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FakeAdapterError;

impl fmt::Display for FakeAdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("fake adapter rejected operation")
    }
}

impl Error for FakeAdapterError {}

/// Deterministic MT7921-shaped gate with no physical execution path.
#[derive(Clone, Debug)]
pub struct FakeMt7921Adapter {
    query_response: WlanSoftmacQueryResponse,
    discovery_support: DiscoverySupport,
    next_scan_id: u64,
    events: VecDeque<HardwareScanEvent>,
    set_channel_requests: Vec<WlanSoftmacBaseSetChannelRequest>,
    passive_scan_requests: Vec<WlanSoftmacBaseStartPassiveScanRequest>,
}

impl FakeMt7921Adapter {
    pub fn new(
        query_response: WlanSoftmacQueryResponse,
        discovery_support: DiscoverySupport,
    ) -> Self {
        Self {
            query_response,
            discovery_support,
            next_scan_id: 1,
            events: VecDeque::new(),
            set_channel_requests: Vec::new(),
            passive_scan_requests: Vec::new(),
        }
    }

    pub fn queue_event(&mut self, event: HardwareScanEvent) {
        self.events.push_back(event);
    }

    pub fn set_channel_requests(&self) -> &[WlanSoftmacBaseSetChannelRequest] {
        &self.set_channel_requests
    }

    pub fn passive_scan_requests(&self) -> &[WlanSoftmacBaseStartPassiveScanRequest] {
        &self.passive_scan_requests
    }
}

impl SoftmacHardware for FakeMt7921Adapter {
    type Error = FakeAdapterError;

    fn query_response(&self) -> &WlanSoftmacQueryResponse {
        &self.query_response
    }

    fn discovery_support(&self) -> &DiscoverySupport {
        &self.discovery_support
    }

    fn set_channel(
        &mut self,
        request: WlanSoftmacBaseSetChannelRequest,
    ) -> Result<(), Self::Error> {
        self.set_channel_requests.push(request);
        Ok(())
    }

    fn start_passive_scan(
        &mut self,
        request: WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<WlanSoftmacBaseStartPassiveScanResponse, Self::Error> {
        self.passive_scan_requests.push(request);
        let scan_id = self.next_scan_id;
        self.next_scan_id += 1;
        Ok(WlanSoftmacBaseStartPassiveScanResponse {
            scan_id: Some(scan_id),
        })
    }

    fn next_scan_event(&mut self) -> Result<Option<HardwareScanEvent>, Self::Error> {
        Ok(self.events.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_wlan_ieee80211::{BssType, WlanPhyType};
    use fidl_fuchsia_wlan_softmac::{ScanOffloadExtension, WlanSoftmacBandCapability};

    fn channel(number: u8) -> ChannelNumber {
        ChannelNumber {
            band: WlanBand::TwoGhz,
            number,
        }
    }

    fn fake() -> FakeMt7921Adapter {
        FakeMt7921Adapter::new(
            WlanSoftmacQueryResponse {
                band_caps: Some(vec![WlanSoftmacBandCapability {
                    band: Some(WlanBand::TwoGhz),
                    primary_channels: Some(vec![channel(1), channel(6), channel(11)]),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            DiscoverySupport {
                scan_offload: Some(ScanOffloadExtension {
                    supported: Some(true),
                    scan_cancel_supported: Some(false),
                }),
                ..Default::default()
            },
        )
    }

    fn passive_request() -> ScanRequest {
        ScanRequest {
            txn_id: 1337,
            scan_type: ScanTypes::Passive,
            channel_list: vec![channel(6)],
            ssid_list: vec![],
            probe_delay: 0,
            min_channel_time: 100,
            max_channel_time: 300,
        }
    }

    // Derived from upstream client/scanner.rs::test_start_offload_passive_scan_success.
    #[test]
    fn upstream_passive_request_fixture_runs_against_fake() {
        let mut hardware = fake();
        let mut scanner = PassiveScanner::default();
        scanner.start(&mut hardware, passive_request()).unwrap();

        assert_eq!(
            hardware.passive_scan_requests(),
            &[WlanSoftmacBaseStartPassiveScanRequest {
                channels: Some(vec![channel(6)]),
                min_channel_time: Some(102_400_000),
                max_channel_time: Some(307_200_000),
                min_home_time: Some(0),
            }]
        );
    }

    // Uses the BSS shape from upstream scanner advertisement fixtures.
    #[test]
    fn beacon_and_completion_keep_mlme_transaction_identity() {
        let mut hardware = fake();
        let mut scanner = PassiveScanner::default();
        scanner.start(&mut hardware, passive_request()).unwrap();
        let bss = BssDescription {
            bssid: [6; 6],
            bss_type: BssType::Infrastructure,
            beacon_period: 100,
            capability_info: 1,
            ies: vec![0, 3, b'f', b'o', b'o'],
            primary: channel(6),
            bandwidth: ChannelBandwidth::Cbw20,
            vht_secondary_80_channel: channel(0),
            rssi_dbm: -30,
            snr_db: 0,
        };
        hardware.queue_event(HardwareScanEvent::Observation(ScanObservation {
            kind: AdvertisementKind::Beacon,
            timestamp_nanos: 42,
            bss: bss.clone(),
        }));
        hardware.queue_event(HardwareScanEvent::Observation(ScanObservation {
            kind: AdvertisementKind::ProbeResponse,
            timestamp_nanos: 43,
            bss: bss.clone(),
        }));
        hardware.queue_event(HardwareScanEvent::Complete {
            scan_id: 1,
            success: true,
        });

        assert_eq!(
            scanner.poll(&mut hardware).unwrap(),
            Some(MlmeScanEvent::Result {
                kind: AdvertisementKind::Beacon,
                result: ScanResult {
                    txn_id: 1337,
                    timestamp_nanos: 42,
                    bss: bss.clone(),
                },
            })
        );
        assert_eq!(
            scanner.poll(&mut hardware).unwrap(),
            Some(MlmeScanEvent::Result {
                kind: AdvertisementKind::ProbeResponse,
                result: ScanResult {
                    txn_id: 1337,
                    timestamp_nanos: 43,
                    bss,
                },
            })
        );
        assert_eq!(
            scanner.poll(&mut hardware).unwrap(),
            Some(MlmeScanEvent::End(ScanEnd {
                txn_id: 1337,
                code: ScanResultCode::Success,
            }))
        );
        assert!(!scanner.is_scanning());
    }

    #[test]
    fn typed_capabilities_and_channel_requests_cross_the_fake_gate() {
        let mut hardware = fake();
        assert_eq!(
            hardware.query_response().band_caps.as_ref().unwrap()[0].primary_channels,
            Some(vec![channel(1), channel(6), channel(11)])
        );
        let request = WlanSoftmacBaseSetChannelRequest {
            primary: Some(channel(6)),
            bandwidth: Some(ChannelBandwidth::Cbw20),
            vht_secondary_80_channel: Some(channel(0)),
        };
        hardware.set_channel(request.clone()).unwrap();
        assert_eq!(hardware.set_channel_requests(), &[request]);
        let _full_pinned_rx_metadata_type_remains_reachable = WlanPhyType::Ht;
    }

    #[test]
    fn upstream_rejection_fixtures_do_not_reach_hardware() {
        let mut hardware = fake();
        let mut scanner = PassiveScanner::default();
        let empty = ScanRequest {
            channel_list: vec![],
            ..passive_request()
        };
        assert_eq!(
            scanner.start(&mut hardware, empty),
            Err(ScanError::EmptyChannelList)
        );

        let invalid_dwell = ScanRequest {
            min_channel_time: 101,
            max_channel_time: 100,
            ..passive_request()
        };
        assert_eq!(
            scanner.start(&mut hardware, invalid_dwell),
            Err(ScanError::MaxChannelTimeLtMin)
        );
        assert!(hardware.passive_scan_requests().is_empty());
    }

    #[test]
    fn wrong_completion_id_does_not_cancel_scan() {
        let mut hardware = fake();
        let mut scanner = PassiveScanner::default();
        scanner.start(&mut hardware, passive_request()).unwrap();
        hardware.queue_event(HardwareScanEvent::Complete {
            scan_id: 99,
            success: true,
        });
        assert_eq!(scanner.poll(&mut hardware).unwrap(), None);
        assert!(scanner.is_scanning());
    }
}
