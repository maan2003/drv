// SPDX-License-Identifier: GPL-2.0-only

//! Offline-capable MT7921 mechanics adapter for the pinned Fuchsia SoftMAC API.
//!
//! This crate contains no device, register, DMA, IRQ, firmware-loading, or host
//! networking implementation. A caller must provide an explicit transport.

use fuchsia_softmac_port::{
    AdvertisementKind, Bssid, CapabilityInfo, ChannelBandwidth, ChannelNumber, DiscoverySupport,
    HardwareScanEvent, ScanObservation, SoftmacHardware, TimeUnit, WlanBand, WlanPhyType,
    WlanRxInfo, WlanRxInfoFlags, WlanRxInfoValid, WlanSoftmacBandCapability,
    WlanSoftmacBaseCancelScanRequest, WlanSoftmacBaseSetChannelRequest,
    WlanSoftmacBaseStartPassiveScanRequest, WlanSoftmacBaseStartPassiveScanResponse,
    WlanSoftmacQueryResponse, construct_bss_description,
};
use mt7921_port_spike::{
    CandidateChannel, NicCapability, PhysicalBand, candidate_channels as capability_channels,
};
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PassiveScanCommand {
    pub scan_id: u64,
    pub channels: Vec<CandidateChannel>,
    pub min_channel_time_nanos: i64,
    pub max_channel_time_nanos: i64,
}

/// Raw beacon/probe fields and receive metadata supplied by the MT7921 RX edge.
/// IE interpretation remains owned by pinned Fuchsia code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawAdvertisement {
    pub scan_id: u64,
    pub kind: AdvertisementKind,
    pub timestamp_nanos: i64,
    pub bssid: [u8; 6],
    pub beacon_interval_tu: u16,
    pub capability_info: u16,
    pub ies: Vec<u8>,
    pub channel: CandidateChannel,
    pub rssi_dbm: i8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportEvent {
    Advertisement(RawAdvertisement),
    Complete { scan_id: u64, success: bool },
}

/// Narrow MT7921 MCU/RX mechanics needed by the passive milestone.
///
/// Implementations execute mechanics only. Regulatory, SME, mac80211, cfg80211,
/// and Linux networking policy do not belong behind this interface.
pub trait Mt7921PassiveTransport {
    type Error: Error + 'static;

    fn set_channel(&mut self, channel: CandidateChannel) -> Result<(), Self::Error>;
    fn start_passive_scan(&mut self, command: PassiveScanCommand) -> Result<(), Self::Error>;
    fn cancel_passive_scan(&mut self, scan_id: u64) -> Result<(), Self::Error>;
    fn next_event(&mut self) -> Result<Option<TransportEvent>, Self::Error>;
}

#[derive(Debug, Eq, PartialEq)]
pub enum AdapterError<E> {
    InvalidCapabilityChannels,
    UnsupportedAuthorizedChannel(ChannelNumber),
    UnauthorizedChannel(ChannelNumber),
    InvalidRequest,
    UnsupportedChannelWidth,
    ActiveScanUnsupported,
    Busy,
    NotScanning,
    ScanIdMismatch { expected: u64, actual: u64 },
    TimestampRegression { previous: i64, actual: i64 },
    InvalidAdvertisement,
    Poisoned,
    ScanIdExhausted,
    Transport(E),
}

impl<E: fmt::Display> fmt::Display for AdapterError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapabilityChannels => {
                f.write_str("candidate channels disagree with NIC capabilities")
            }
            Self::UnsupportedAuthorizedChannel(channel) => {
                write!(f, "unsupported authorized channel {channel:?}")
            }
            Self::UnauthorizedChannel(channel) => {
                write!(f, "channel is not authorized: {channel:?}")
            }
            Self::InvalidRequest => f.write_str("invalid passive scan request"),
            Self::UnsupportedChannelWidth => {
                f.write_str("only conservative 20 MHz operation is supported")
            }
            Self::ActiveScanUnsupported => f.write_str("active scan semantics are not supported"),
            Self::Busy => f.write_str("a scan is already in progress"),
            Self::NotScanning => f.write_str("no scan is in progress"),
            Self::ScanIdMismatch { expected, actual } => {
                write!(f, "scan id mismatch: expected {expected}, got {actual}")
            }
            Self::TimestampRegression { previous, actual } => write!(
                f,
                "monotonic timestamp regressed from {previous} to {actual}"
            ),
            Self::InvalidAdvertisement => {
                f.write_str("pinned Fuchsia beacon conversion rejected advertisement")
            }
            Self::Poisoned => {
                f.write_str("adapter is fail-closed after a transport or input failure")
            }
            Self::ScanIdExhausted => f.write_str("scan id space exhausted"),
            Self::Transport(error) => write!(f, "MT7921 transport failed: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for AdapterError<E> {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScanState {
    Idle,
    Scanning { scan_id: u64 },
    Cancelling { scan_id: u64 },
    Poisoned,
}

/// Real state-machine adapter. Physical authority, if any, is confined to `T`.
pub struct Mt7921SoftmacAdapter<T> {
    transport: T,
    query_response: WlanSoftmacQueryResponse,
    discovery_support: DiscoverySupport,
    candidates: Vec<CandidateChannel>,
    authorized: Vec<ChannelNumber>,
    next_scan_id: u64,
    last_timestamp_nanos: Option<i64>,
    state: ScanState,
}

impl<T: Mt7921PassiveTransport> Mt7921SoftmacAdapter<T> {
    pub fn new(
        transport: T,
        nic_capability: NicCapability,
        candidates: Vec<CandidateChannel>,
        authorized: Vec<ChannelNumber>,
    ) -> Result<Self, AdapterError<T::Error>> {
        let discovered = capability_channels(nic_capability);
        if candidates
            .iter()
            .any(|candidate| !discovered.contains(candidate))
        {
            return Err(AdapterError::InvalidCapabilityChannels);
        }
        for channel in &authorized {
            if channel_to_candidate(*channel, &candidates).is_none() {
                return Err(AdapterError::UnsupportedAuthorizedChannel(*channel));
            }
        }

        let query_response = query_from_capabilities(nic_capability, &candidates);
        Ok(Self {
            transport,
            query_response,
            discovery_support: DiscoverySupport {
                scan_offload: Some(fuchsia_softmac_port::ScanOffloadExtension {
                    supported: Some(true),
                    scan_cancel_supported: Some(true),
                }),
                ..Default::default()
            },
            candidates,
            authorized,
            next_scan_id: 1,
            last_timestamp_nanos: None,
            state: ScanState::Idle,
        })
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Explicit rejection surface for callers that otherwise have active scan
    /// request material. No transport operation is attempted.
    pub fn start_active_scan(&mut self) -> Result<u64, AdapterError<T::Error>> {
        Err(AdapterError::ActiveScanUnsupported)
    }

    fn ensure_live(&self) -> Result<(), AdapterError<T::Error>> {
        if self.state == ScanState::Poisoned {
            Err(AdapterError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn transport_failure(&mut self, error: T::Error) -> AdapterError<T::Error> {
        self.state = ScanState::Poisoned;
        AdapterError::Transport(error)
    }

    fn active_scan_id(&self) -> Option<u64> {
        match self.state {
            ScanState::Scanning { scan_id } | ScanState::Cancelling { scan_id } => Some(scan_id),
            ScanState::Idle | ScanState::Poisoned => None,
        }
    }

    fn convert_advertisement(
        &mut self,
        raw: RawAdvertisement,
    ) -> Result<HardwareScanEvent, AdapterError<T::Error>> {
        let Some(expected) = self.active_scan_id() else {
            return Err(AdapterError::NotScanning);
        };
        if raw.scan_id != expected {
            return Err(AdapterError::ScanIdMismatch {
                expected,
                actual: raw.scan_id,
            });
        }
        if !self
            .authorized
            .contains(&to_fuchsia_channel(raw.channel).ok_or(AdapterError::InvalidAdvertisement)?)
        {
            self.state = ScanState::Poisoned;
            return Err(AdapterError::InvalidAdvertisement);
        }
        if let Some(previous) = self.last_timestamp_nanos {
            if raw.timestamp_nanos < previous {
                self.state = ScanState::Poisoned;
                return Err(AdapterError::TimestampRegression {
                    previous,
                    actual: raw.timestamp_nanos,
                });
            }
        }
        let primary = to_fuchsia_channel(raw.channel).ok_or(AdapterError::InvalidAdvertisement)?;
        let phy = self
            .query_response
            .supported_phys
            .as_ref()
            .and_then(|phys| phys.last().copied())
            .unwrap_or(WlanPhyType::Ofdm);
        let bss = construct_bss_description(
            Bssid::from(raw.bssid),
            TimeUnit(raw.beacon_interval_tu),
            CapabilityInfo(raw.capability_info),
            &raw.ies,
            WlanRxInfo {
                rx_flags: WlanRxInfoFlags::empty(),
                valid_fields: WlanRxInfoValid::PHY | WlanRxInfoValid::RSSI,
                phy,
                data_rate: 0,
                primary,
                bandwidth: ChannelBandwidth::Cbw20,
                vht_secondary_80_channel: ChannelNumber {
                    band: primary.band,
                    number: 0,
                },
                mcs: 0,
                rssi_dbm: raw.rssi_dbm,
                snr_dbh: 0,
            },
        )
        .map_err(|_| {
            self.state = ScanState::Poisoned;
            AdapterError::InvalidAdvertisement
        })?;
        self.last_timestamp_nanos = Some(raw.timestamp_nanos);
        Ok(HardwareScanEvent::Observation(ScanObservation {
            kind: raw.kind,
            timestamp_nanos: raw.timestamp_nanos,
            bss,
        }))
    }
}

impl<T: Mt7921PassiveTransport> SoftmacHardware for Mt7921SoftmacAdapter<T> {
    type Error = AdapterError<T::Error>;

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
        self.ensure_live()?;
        let primary = request.primary.ok_or(AdapterError::InvalidRequest)?;
        if request.bandwidth != Some(ChannelBandwidth::Cbw20)
            || request
                .vht_secondary_80_channel
                .is_some_and(|secondary| secondary.number != 0)
        {
            return Err(AdapterError::UnsupportedChannelWidth);
        }
        if !self.authorized.contains(&primary) {
            return Err(AdapterError::UnauthorizedChannel(primary));
        }
        let candidate = channel_to_candidate(primary, &self.candidates)
            .ok_or(AdapterError::UnauthorizedChannel(primary))?;
        self.transport
            .set_channel(candidate)
            .map_err(|error| self.transport_failure(error))
    }

    fn start_passive_scan(
        &mut self,
        request: WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<WlanSoftmacBaseStartPassiveScanResponse, Self::Error> {
        self.ensure_live()?;
        if self.state != ScanState::Idle {
            return Err(AdapterError::Busy);
        }
        let channels = request.channels.ok_or(AdapterError::InvalidRequest)?;
        let min = request
            .min_channel_time
            .ok_or(AdapterError::InvalidRequest)?;
        let max = request
            .max_channel_time
            .ok_or(AdapterError::InvalidRequest)?;
        if channels.is_empty() || min < 0 || max < min || request.min_home_time != Some(0) {
            return Err(AdapterError::InvalidRequest);
        }
        let mut transport_channels = Vec::with_capacity(channels.len());
        for channel in channels {
            if !self.authorized.contains(&channel) {
                return Err(AdapterError::UnauthorizedChannel(channel));
            }
            transport_channels.push(
                channel_to_candidate(channel, &self.candidates)
                    .ok_or(AdapterError::UnauthorizedChannel(channel))?,
            );
        }
        let scan_id = self.next_scan_id;
        let next = scan_id
            .checked_add(1)
            .ok_or(AdapterError::ScanIdExhausted)?;
        let command = PassiveScanCommand {
            scan_id,
            channels: transport_channels,
            min_channel_time_nanos: min,
            max_channel_time_nanos: max,
        };
        self.transport
            .start_passive_scan(command)
            .map_err(|error| self.transport_failure(error))?;
        self.next_scan_id = next;
        self.state = ScanState::Scanning { scan_id };
        Ok(WlanSoftmacBaseStartPassiveScanResponse {
            scan_id: Some(scan_id),
        })
    }

    fn cancel_scan(
        &mut self,
        request: WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), Self::Error> {
        self.ensure_live()?;
        let requested = request.scan_id.ok_or(AdapterError::InvalidRequest)?;
        let expected = self.active_scan_id().ok_or(AdapterError::NotScanning)?;
        if requested != expected {
            return Err(AdapterError::ScanIdMismatch {
                expected,
                actual: requested,
            });
        }
        if matches!(self.state, ScanState::Cancelling { .. }) {
            return Ok(());
        }
        self.transport
            .cancel_passive_scan(expected)
            .map_err(|error| self.transport_failure(error))?;
        self.state = ScanState::Cancelling { scan_id: expected };
        Ok(())
    }

    fn next_scan_event(&mut self) -> Result<Option<HardwareScanEvent>, Self::Error> {
        self.ensure_live()?;
        let event = self
            .transport
            .next_event()
            .map_err(|error| self.transport_failure(error))?;
        let Some(event) = event else { return Ok(None) };
        match event {
            TransportEvent::Advertisement(raw)
                if matches!(self.state, ScanState::Cancelling { .. }) =>
            {
                if Some(raw.scan_id) != self.active_scan_id() {
                    return Ok(None);
                }
                Ok(None)
            }
            TransportEvent::Advertisement(raw) => self.convert_advertisement(raw).map(Some),
            TransportEvent::Complete { scan_id, success } => {
                let Some(expected) = self.active_scan_id() else {
                    return Ok(None);
                };
                if scan_id != expected {
                    return Ok(None);
                }
                let cancelled = matches!(self.state, ScanState::Cancelling { .. });
                self.state = ScanState::Idle;
                Ok(Some(HardwareScanEvent::Complete {
                    scan_id,
                    success: success && !cancelled,
                }))
            }
        }
    }
}

fn channel_to_candidate(
    channel: ChannelNumber,
    candidates: &[CandidateChannel],
) -> Option<CandidateChannel> {
    candidates
        .iter()
        .copied()
        .find(|candidate| to_fuchsia_channel(*candidate) == Some(channel))
}

fn to_fuchsia_channel(channel: CandidateChannel) -> Option<ChannelNumber> {
    let band = match channel.band {
        PhysicalBand::Ghz2 => WlanBand::TwoGhz,
        PhysicalBand::Ghz5 => WlanBand::FiveGhz,
        PhysicalBand::Ghz6 => return None,
    };
    let number = u8::try_from(channel.number).ok()?;
    Some(ChannelNumber { band, number })
}

fn query_from_capabilities(
    nic: NicCapability,
    candidates: &[CandidateChannel],
) -> WlanSoftmacQueryResponse {
    let channels_for = |physical_band, wlan_band| {
        candidates
            .iter()
            .copied()
            .filter(|channel| channel.band == physical_band)
            .filter_map(to_fuchsia_channel)
            .map(|channel| ChannelNumber {
                band: wlan_band,
                number: channel.number,
            })
            .collect::<Vec<_>>()
    };
    let mut band_caps = Vec::new();
    for (physical, wlan) in [
        (PhysicalBand::Ghz2, WlanBand::TwoGhz),
        (PhysicalBand::Ghz5, WlanBand::FiveGhz),
    ] {
        let primary_channels = channels_for(physical, wlan);
        if !primary_channels.is_empty() {
            band_caps.push(WlanSoftmacBandCapability {
                band: Some(wlan),
                primary_channels: Some(primary_channels),
                ..Default::default()
            });
        }
    }
    let mut phys = vec![WlanPhyType::Ofdm];
    if let Some(phy) = nic.phy {
        if phy.ht {
            phys.push(WlanPhyType::Ht);
        }
        if phy.vht {
            phys.push(WlanPhyType::Vht);
        }
        if phy.he {
            phys.push(WlanPhyType::He);
        }
    }
    WlanSoftmacQueryResponse {
        sta_addr: nic.mac_address,
        factory_addr: nic.mac_address,
        supported_phys: Some(phys),
        band_caps: Some(band_caps),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ScriptError(&'static str);

    impl fmt::Display for ScriptError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }
    impl Error for ScriptError {}

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Call {
        Set(CandidateChannel),
        Start(PassiveScanCommand),
        Cancel(u64),
    }

    #[derive(Default)]
    struct ScriptedTransport {
        calls: Vec<Call>,
        events: VecDeque<Result<Option<TransportEvent>, ScriptError>>,
        fail_set: bool,
        fail_start: bool,
        fail_cancel: bool,
    }

    impl Mt7921PassiveTransport for ScriptedTransport {
        type Error = ScriptError;
        fn set_channel(&mut self, channel: CandidateChannel) -> Result<(), Self::Error> {
            if self.fail_set {
                return Err(ScriptError("set"));
            }
            self.calls.push(Call::Set(channel));
            Ok(())
        }
        fn start_passive_scan(&mut self, command: PassiveScanCommand) -> Result<(), Self::Error> {
            if self.fail_start {
                return Err(ScriptError("start"));
            }
            self.calls.push(Call::Start(command));
            Ok(())
        }
        fn cancel_passive_scan(&mut self, scan_id: u64) -> Result<(), Self::Error> {
            if self.fail_cancel {
                return Err(ScriptError("cancel"));
            }
            self.calls.push(Call::Cancel(scan_id));
            Ok(())
        }
        fn next_event(&mut self) -> Result<Option<TransportEvent>, Self::Error> {
            self.events.pop_front().unwrap_or(Ok(None))
        }
    }

    fn nic() -> NicCapability {
        NicCapability {
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

    fn channel(number: u8) -> ChannelNumber {
        ChannelNumber {
            band: WlanBand::TwoGhz,
            number,
        }
    }

    fn new_adapter(authorized: Vec<ChannelNumber>) -> Mt7921SoftmacAdapter<ScriptedTransport> {
        let capability = nic();
        Mt7921SoftmacAdapter::new(
            ScriptedTransport::default(),
            capability,
            capability_channels(capability),
            authorized,
        )
        .unwrap()
    }

    fn request(channels: Vec<ChannelNumber>) -> WlanSoftmacBaseStartPassiveScanRequest {
        WlanSoftmacBaseStartPassiveScanRequest {
            channels: Some(channels),
            min_channel_time: Some(10),
            max_channel_time: Some(20),
            min_home_time: Some(0),
        }
    }

    #[test]
    fn capabilities_cross_the_seam_but_authority_is_separate() {
        let mut adapter = new_adapter(vec![channel(1)]);
        assert_eq!(adapter.query_response().sta_addr, nic().mac_address);
        assert_eq!(
            adapter.query_response().band_caps.as_ref().unwrap().len(),
            2
        );
        assert_eq!(
            adapter.set_channel(WlanSoftmacBaseSetChannelRequest {
                primary: Some(channel(6)),
                bandwidth: Some(ChannelBandwidth::Cbw20),
                vht_secondary_80_channel: Some(channel(0)),
            }),
            Err(AdapterError::UnauthorizedChannel(channel(6)))
        );
        assert!(adapter.transport.calls.is_empty());
        assert_eq!(
            adapter.start_active_scan(),
            Err(AdapterError::ActiveScanUnsupported)
        );
        assert!(adapter.transport.calls.is_empty());
    }

    #[test]
    fn rejects_authority_not_present_in_typed_candidates() {
        let capability = nic();
        let error = Mt7921SoftmacAdapter::new(
            ScriptedTransport::default(),
            capability,
            capability_channels(capability),
            vec![ChannelNumber {
                band: WlanBand::FiveGhz,
                number: 200,
            }],
        )
        .err()
        .unwrap();
        assert_eq!(
            error,
            AdapterError::UnsupportedAuthorizedChannel(ChannelNumber {
                band: WlanBand::FiveGhz,
                number: 200
            })
        );
    }

    #[test]
    fn preserves_ids_and_uses_pinned_beacon_conversion() {
        let mut adapter = new_adapter(vec![channel(1), channel(11)]);
        let response = adapter
            .start_passive_scan(request(vec![channel(1)]))
            .unwrap();
        assert_eq!(response.scan_id, Some(1));
        let candidate = channel_to_candidate(channel(1), &adapter.candidates).unwrap();
        adapter.transport.events.extend([
            Ok(Some(TransportEvent::Advertisement(RawAdvertisement {
                scan_id: 1,
                kind: AdvertisementKind::Beacon,
                timestamp_nanos: 100,
                bssid: [3; 6],
                beacon_interval_tu: 100,
                capability_info: 1,
                // SSID "foo" and DSSS channel 11. The resulting channel proves
                // the pinned converter, rather than a local IE parser, ran.
                ies: vec![0, 3, b'f', b'o', b'o', 3, 1, 11],
                channel: candidate,
                rssi_dbm: -42,
            }))),
            Ok(Some(TransportEvent::Complete {
                scan_id: 99,
                success: true,
            })),
            Ok(Some(TransportEvent::Complete {
                scan_id: 1,
                success: true,
            })),
        ]);
        let HardwareScanEvent::Observation(observation) =
            adapter.next_scan_event().unwrap().unwrap()
        else {
            panic!()
        };
        assert_eq!(observation.timestamp_nanos, 100);
        assert_eq!(observation.bss.primary, channel(11));
        assert_eq!(observation.bss.bssid, [3; 6]);
        assert_eq!(adapter.next_scan_event().unwrap(), None);
        assert_eq!(
            adapter.next_scan_event().unwrap(),
            Some(HardwareScanEvent::Complete {
                scan_id: 1,
                success: true
            })
        );
        assert_eq!(
            adapter
                .start_passive_scan(request(vec![channel(1)]))
                .unwrap()
                .scan_id,
            Some(2)
        );
    }

    #[test]
    fn cancellation_waits_for_matching_completion_and_drops_observations() {
        let mut adapter = new_adapter(vec![channel(1)]);
        adapter
            .start_passive_scan(request(vec![channel(1)]))
            .unwrap();
        adapter
            .cancel_scan(WlanSoftmacBaseCancelScanRequest { scan_id: Some(1) })
            .unwrap();
        let candidate = channel_to_candidate(channel(1), &adapter.candidates).unwrap();
        adapter.transport.events.extend([
            Ok(Some(TransportEvent::Advertisement(RawAdvertisement {
                scan_id: 1,
                kind: AdvertisementKind::ProbeResponse,
                timestamp_nanos: 1,
                bssid: [1; 6],
                beacon_interval_tu: 100,
                capability_info: 1,
                ies: vec![],
                channel: candidate,
                rssi_dbm: -30,
            }))),
            Ok(Some(TransportEvent::Complete {
                scan_id: 2,
                success: true,
            })),
            Ok(Some(TransportEvent::Complete {
                scan_id: 1,
                success: true,
            })),
        ]);
        assert_eq!(adapter.next_scan_event().unwrap(), None);
        assert_eq!(adapter.next_scan_event().unwrap(), None);
        assert_eq!(
            adapter.next_scan_event().unwrap(),
            Some(HardwareScanEvent::Complete {
                scan_id: 1,
                success: false
            })
        );
        assert_eq!(adapter.transport.calls.last(), Some(&Call::Cancel(1)));
    }

    #[test]
    fn cancellation_failure_poisoning_is_fail_closed() {
        let mut adapter = new_adapter(vec![channel(1)]);
        adapter
            .start_passive_scan(request(vec![channel(1)]))
            .unwrap();
        adapter.transport.fail_cancel = true;
        assert_eq!(
            adapter.cancel_scan(WlanSoftmacBaseCancelScanRequest { scan_id: Some(1) }),
            Err(AdapterError::Transport(ScriptError("cancel")))
        );
        assert_eq!(adapter.next_scan_event(), Err(AdapterError::Poisoned));
        assert_eq!(
            adapter.start_passive_scan(request(vec![channel(1)])),
            Err(AdapterError::Poisoned)
        );
    }

    #[test]
    fn regressing_timestamp_poisoning_is_fail_closed() {
        let mut adapter = new_adapter(vec![channel(1)]);
        adapter
            .start_passive_scan(request(vec![channel(1)]))
            .unwrap();
        let candidate = channel_to_candidate(channel(1), &adapter.candidates).unwrap();
        for timestamp_nanos in [10, 9] {
            adapter
                .transport
                .events
                .push_back(Ok(Some(TransportEvent::Advertisement(RawAdvertisement {
                    scan_id: 1,
                    kind: AdvertisementKind::Beacon,
                    timestamp_nanos,
                    bssid: [1; 6],
                    beacon_interval_tu: 100,
                    capability_info: 1,
                    ies: vec![],
                    channel: candidate,
                    rssi_dbm: -30,
                }))));
        }
        assert!(matches!(
            adapter.next_scan_event(),
            Ok(Some(HardwareScanEvent::Observation(_)))
        ));
        assert_eq!(
            adapter.next_scan_event(),
            Err(AdapterError::TimestampRegression {
                previous: 10,
                actual: 9
            })
        );
        assert_eq!(adapter.next_scan_event(), Err(AdapterError::Poisoned));
    }

    #[test]
    fn transport_start_failure_does_not_fabricate_scan_identity() {
        let mut adapter = new_adapter(vec![channel(1)]);
        adapter.transport.fail_start = true;
        assert_eq!(
            adapter.start_passive_scan(request(vec![channel(1)])),
            Err(AdapterError::Transport(ScriptError("start")))
        );
        assert_eq!(adapter.next_scan_event(), Err(AdapterError::Poisoned));
    }
}
