// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host-portable extraction of pinned Fuchsia SoftMAC client MLME boundaries.

mod open_client;
mod sae;

pub use open_client::*;
pub use sae::*;

#[path = "../../mlme/rust/src/client/convert_beacon.rs"]
mod pinned_convert_beacon;

pub use pinned_convert_beacon::construct_bss_description;

pub use fidl_fuchsia_wlan_ieee80211::{
    BssDescription, ChannelBandwidth, ChannelNumber, StatusCode, WlanBand, WlanPhyType,
};
pub use fidl_fuchsia_wlan_mlme::{ScanEnd, ScanRequest, ScanResult, ScanResultCode, ScanTypes};
pub use fidl_fuchsia_wlan_softmac::{
    DiscoverySupport, ScanOffloadExtension, WlanRxInfo, WlanRxInfoFlags, WlanRxInfoValid,
    WlanSoftmacBandCapability, WlanSoftmacBaseCancelScanRequest, WlanSoftmacBaseSetChannelRequest,
    WlanSoftmacBaseStartPassiveScanRequest, WlanSoftmacBaseStartPassiveScanResponse,
    WlanSoftmacQueryResponse,
};
pub use ieee80211::Bssid;
pub use wlan_common::{TimeUnit, mac::CapabilityInfo};

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

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
    fn cancel_scan(&mut self, request: WlanSoftmacBaseCancelScanRequest)
    -> Result<(), Self::Error>;
    fn next_scan_event(&mut self) -> Result<Option<HardwareScanEvent>, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConservativeRegulatoryPolicy {
    pub alpha2: [u8; 2],
    pub indoor: bool,
    pub special_unii_mask: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegulatoryError {
    NonWorldDomain,
    OutdoorEnvironment,
    InvalidSpecialUniiMask,
    MissingBandCapabilities,
}

/// Apply the pinned Fuchsia SME passive-scan channel policy to hardware-
/// reported channels under the temporary world/indoor CLC authorization.
///
/// The pinned policy's candidate universe is 2.4 GHz 1-14 and 5 GHz 36-165.
/// It has no 6 GHz `WlanBand` and deliberately omits UNII-4 channels 169-177,
/// so the firmware special-UNII mask can only restrict future policy; it never
/// expands this conservative milestone's permissions.
pub fn allowed_passive_channels(
    query: &WlanSoftmacQueryResponse,
    policy: ConservativeRegulatoryPolicy,
) -> Result<Vec<ChannelNumber>, RegulatoryError> {
    const FUCHSIA_5GHZ: [u8; 25] = [
        36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144,
        149, 153, 157, 161, 165,
    ];
    if policy.alpha2 != *b"00" {
        return Err(RegulatoryError::NonWorldDomain);
    }
    if !policy.indoor {
        return Err(RegulatoryError::OutdoorEnvironment);
    }
    if policy.special_unii_mask & !0x1f != 0 {
        return Err(RegulatoryError::InvalidSpecialUniiMask);
    }
    let bands = query
        .band_caps
        .as_ref()
        .ok_or(RegulatoryError::MissingBandCapabilities)?;
    let supports = |band: WlanBand, number: u8| {
        bands.iter().any(|capability| {
            capability.band == Some(band)
                && capability
                    .primary_channels
                    .as_ref()
                    .is_some_and(|channels| {
                        channels
                            .iter()
                            .any(|channel| channel.band == band && channel.number == number)
                    })
        })
    };
    let mut allowed = Vec::new();
    for number in 1..=14 {
        if supports(WlanBand::TwoGhz, number) {
            allowed.push(ChannelNumber {
                band: WlanBand::TwoGhz,
                number,
            });
        }
    }
    for number in FUCHSIA_5GHZ {
        if supports(WlanBand::FiveGhz, number) {
            allowed.push(ChannelNumber {
                band: WlanBand::FiveGhz,
                number,
            });
        }
    }
    Ok(allowed)
}

/// Valid only while retained by its originating authorizer. Production must
/// feed observations from the active hardware scan path, not caller-built
/// `ScanObservation` values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BeaconHintAuthorization {
    owner_id: NonZeroU64,
    epoch: u64,
    channel: ChannelNumber,
    bssid: [u8; 6],
    scan_generation: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub struct BeaconHintAuthorizer {
    owner_id: NonZeroU64,
    alpha2: [u8; 2],
    channel: Option<ChannelNumber>,
    target_bssid: [u8; 6],
    target_ssid: Vec<u8>,
    epoch: u64,
    scan_generation: Option<u64>,
    authorization: Option<BeaconHintAuthorization>,
}

impl BeaconHintAuthorizer {
    pub fn new(target_bssid: [u8; 6], target_ssid: Vec<u8>) -> Self {
        Self {
            owner_id: next_beacon_authorizer_id(),
            alpha2: *b"00",
            channel: None,
            target_bssid,
            target_ssid,
            epoch: 0,
            scan_generation: None,
            authorization: None,
        }
    }

    /// A channel transition invalidates the previous beacon hint even when
    /// returning to the same channel later in the run.
    pub fn set_channel(&mut self, channel: ChannelNumber) {
        self.epoch = self.epoch.wrapping_add(1);
        self.authorization = None;
        self.channel = Some(channel);
        self.scan_generation = None;
    }

    pub fn begin_passive_scan(&mut self, generation: u64, channel: ChannelNumber) {
        self.set_channel(channel);
        self.scan_generation = Some(generation);
    }

    pub fn set_regulatory_domain(&mut self, alpha2: [u8; 2]) {
        self.epoch = self.epoch.wrapping_add(1);
        self.authorization = None;
        self.alpha2 = alpha2;
        self.scan_generation = None;
    }

    pub fn reset(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.authorization = None;
        self.channel = None;
        self.scan_generation = None;
    }

    /// Port the narrow `regulatory_hint_found_beacon` case used here: direct
    /// ESS beacon, world roaming domain, exact non-radar channel 36, and the
    /// configured BSSID/SSID. Probe responses and AP Country IEs cannot mint
    /// this authorization.
    pub fn observe(
        &mut self,
        scan_generation: u64,
        observation: &ScanObservation,
    ) -> Option<BeaconHintAuthorization> {
        let channel = ChannelNumber {
            band: WlanBand::FiveGhz,
            number: 36,
        };
        if self.alpha2 != *b"00"
            || self.channel != Some(channel)
            || self.scan_generation != Some(scan_generation)
            || observation.kind != AdvertisementKind::Beacon
            || observation.bss.primary != channel
            || observation.bss.bssid != self.target_bssid
            || observation.bss.capability_info & 1 == 0
            || ssid_from_ies(&observation.bss.ies) != Some(self.target_ssid.as_slice())
        {
            return None;
        }
        let authorization = BeaconHintAuthorization {
            owner_id: self.owner_id,
            epoch: self.epoch,
            channel,
            bssid: self.target_bssid,
            scan_generation,
        };
        self.authorization = Some(authorization);
        Some(authorization)
    }

    /// Must be checked at the sole management-TX publish point. A copied token
    /// cannot survive a channel, regulatory-domain, or reset transition.
    pub fn permits(&self, authorization: &BeaconHintAuthorization) -> bool {
        self.authorization.as_ref() == Some(authorization)
            && authorization.owner_id == self.owner_id
            && authorization.epoch == self.epoch
            && self.channel == Some(authorization.channel)
            && authorization.bssid == self.target_bssid
            && self.scan_generation == Some(authorization.scan_generation)
    }
}

fn next_beacon_authorizer_id() -> NonZeroU64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("beacon authorizer identity space exhausted");
    NonZeroU64::new(id).expect("beacon authorizer identities start at one")
}

fn ssid_from_ies(ies: &[u8]) -> Option<&[u8]> {
    let mut offset = 0usize;
    while offset < ies.len() {
        let header = ies.get(offset..offset + 2)?;
        let len = usize::from(header[1]);
        offset += 2;
        let body = ies.get(offset..offset.checked_add(len)?)?;
        if header[0] == 0 {
            return (len <= 32).then_some(body);
        }
        offset += len;
    }
    None
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

    /// Request cancellation without discarding identity. The scanner remains
    /// busy until hardware reports completion for the matching device scan.
    pub fn cancel<H: SoftmacHardware>(
        &mut self,
        hardware: &mut H,
    ) -> Result<(), ScanError<H::Error>> {
        let Some(scan) = self.ongoing_scan else {
            return Ok(());
        };
        hardware
            .cancel_scan(WlanSoftmacBaseCancelScanRequest {
                scan_id: Some(scan.device_scan_id),
            })
            .map_err(ScanError::Hardware)
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
    cancel_scan_requests: Vec<WlanSoftmacBaseCancelScanRequest>,
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
            cancel_scan_requests: Vec::new(),
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

    pub fn cancel_scan_requests(&self) -> &[WlanSoftmacBaseCancelScanRequest] {
        &self.cancel_scan_requests
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

    fn cancel_scan(
        &mut self,
        request: WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), Self::Error> {
        self.cancel_scan_requests.push(request);
        Ok(())
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

    fn channel_5ghz(number: u8) -> ChannelNumber {
        ChannelNumber {
            band: WlanBand::FiveGhz,
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

    // Matches pinned SME `get_primary_channels_for_scan`: passive scans use
    // the intersection of its fixed candidates and hardware primary channels.
    #[test]
    fn world_indoor_policy_never_expands_from_clc_or_country_ie() {
        let query = WlanSoftmacQueryResponse {
            band_caps: Some(vec![
                WlanSoftmacBandCapability {
                    band: Some(WlanBand::TwoGhz),
                    primary_channels: Some(vec![channel(1), channel(14)]),
                    ..Default::default()
                },
                WlanSoftmacBandCapability {
                    band: Some(WlanBand::FiveGhz),
                    primary_channels: Some(vec![
                        channel_5ghz(36),
                        channel_5ghz(165),
                        channel_5ghz(169),
                        channel_5ghz(177),
                    ]),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        assert_eq!(
            allowed_passive_channels(
                &query,
                ConservativeRegulatoryPolicy {
                    alpha2: *b"00",
                    indoor: true,
                    special_unii_mask: 0x1f,
                }
            ),
            Ok(vec![
                channel(1),
                channel(14),
                channel_5ghz(36),
                channel_5ghz(165)
            ])
        );
        assert_eq!(
            allowed_passive_channels(
                &query,
                ConservativeRegulatoryPolicy {
                    alpha2: *b"US",
                    indoor: true,
                    special_unii_mask: 0x1f,
                }
            ),
            Err(RegulatoryError::NonWorldDomain)
        );
    }

    #[test]
    fn direct_target_beacon_mints_only_run_scoped_channel_36_authorization() {
        let target = [6; 6];
        let channel = channel_5ghz(36);
        let observation = ScanObservation {
            kind: AdvertisementKind::Beacon,
            timestamp_nanos: 42,
            bss: BssDescription {
                bssid: target,
                bss_type: BssType::Infrastructure,
                beacon_period: 100,
                capability_info: 1,
                ies: vec![0, 3, b'p', b'h', b'1'],
                primary: channel,
                bandwidth: ChannelBandwidth::Cbw20,
                vht_secondary_80_channel: channel_5ghz(0),
                rssi_dbm: -30,
                snr_db: 0,
            },
        };
        let mut authorizer = BeaconHintAuthorizer::new(target, b"ph1".to_vec());
        authorizer.begin_passive_scan(1, channel);
        let authorization = authorizer.observe(1, &observation).unwrap();
        assert!(authorizer.permits(&authorization));

        let mut other_authorizer = BeaconHintAuthorizer::new(target, b"ph1".to_vec());
        other_authorizer.begin_passive_scan(1, channel);
        let other_authorization = other_authorizer.observe(1, &observation).unwrap();
        assert!(other_authorizer.permits(&other_authorization));
        assert!(!authorizer.permits(&other_authorization));
        assert!(!other_authorizer.permits(&authorization));

        let mut wrong = observation.clone();
        wrong.kind = AdvertisementKind::ProbeResponse;
        assert_eq!(authorizer.observe(1, &wrong), None);
        wrong = observation.clone();
        wrong.bss.bssid = [7; 6];
        assert_eq!(authorizer.observe(1, &wrong), None);
        wrong = observation.clone();
        wrong.bss.ies = vec![0, 3, b'n', b'o', b'p'];
        assert_eq!(authorizer.observe(1, &wrong), None);

        authorizer.set_channel(channel_5ghz(40));
        assert!(!authorizer.permits(&authorization));
        authorizer.begin_passive_scan(2, channel);
        assert!(!authorizer.permits(&authorization));
        assert_eq!(authorizer.observe(1, &observation), None);
        let authorization = authorizer.observe(2, &observation).unwrap();
        authorizer.set_regulatory_domain(*b"IN");
        assert!(!authorizer.permits(&authorization));
        authorizer.set_regulatory_domain(*b"00");
        authorizer.begin_passive_scan(1, channel);
        let authorization = authorizer.observe(1, &observation).unwrap();
        authorizer.reset();
        assert!(!authorizer.permits(&authorization));
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

    #[test]
    fn cancellation_keeps_identity_until_matching_completion() {
        let mut hardware = fake();
        let mut scanner = PassiveScanner::default();
        scanner.start(&mut hardware, passive_request()).unwrap();
        scanner.cancel(&mut hardware).unwrap();
        assert_eq!(
            hardware.cancel_scan_requests(),
            &[WlanSoftmacBaseCancelScanRequest { scan_id: Some(1) }]
        );
        assert!(scanner.is_scanning());

        hardware.queue_event(HardwareScanEvent::Complete {
            scan_id: 1,
            success: false,
        });
        assert_eq!(
            scanner.poll(&mut hardware).unwrap(),
            Some(MlmeScanEvent::End(ScanEnd {
                txn_id: 1337,
                code: ScanResultCode::InternalError,
            }))
        );
        assert!(!scanner.is_scanning());
    }
}
