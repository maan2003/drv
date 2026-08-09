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
    CandidateChannel, NicCapability, PassiveAdvertisement, PassiveMcuCommand,
    PassiveMcuCommandError, PassiveScanDone, PhysicalBand,
    candidate_channels as capability_channels, encode_passive_mcu_command,
};
use std::collections::VecDeque;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PassivePrerequisites {
    pub channel_domain_mask_zero: bool,
    pub mac_mmio_initialized: bool,
    pub data_rx_owned: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PassiveMechanicsEvent {
    Advertisement {
        timestamp_nanos: i64,
        advertisement: PassiveAdvertisement,
    },
    ScanDone(PassiveScanDone),
}

/// Device edge below the real Fuchsia adapter. Implementations own the exact
/// MMIO/data-RX setup and matched MCU completion mechanics, but receive only
/// already encoded source-exact commands.
pub trait SourceExactPassiveMechanics {
    type Error: Error + 'static;

    fn prepare_passive_receive(&mut self) -> Result<PassivePrerequisites, Self::Error>;
    fn command(
        &mut self,
        command: &PassiveMcuCommand,
        encoded: &[u8],
        wait_response: bool,
    ) -> Result<(), Self::Error>;
    fn next_event(
        &mut self,
        deadline_nanos: i64,
    ) -> Result<Option<PassiveMechanicsEvent>, Self::Error>;
}

#[derive(Debug, Eq, PartialEq)]
pub enum SourceExactTransportError<E> {
    MissingNicIdentity,
    UnsupportedSpatialStreams,
    MandatoryDependency(PassivePrerequisites),
    InvalidSequence,
    InvalidDwell,
    UnsupportedMultiChannelScan,
    ChannelNotSelected,
    ScanIdMismatch { expected: u8, actual: u8 },
    Encode(PassiveMcuCommandError),
    Mechanics(E),
}

impl<E: fmt::Display + fmt::Debug> fmt::Display for SourceExactTransportError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "source-exact passive transport failed: {self:?}")
    }
}

impl<E: Error + 'static> Error for SourceExactTransportError<E> {}

/// Concrete transport used by `Mt7921SoftmacAdapter`. It emits initialization,
/// tune, and passive scan commands only; there is no general TX API.
pub struct SourceExactPassiveTransport<M> {
    mechanics: M,
    mac: [u8; 6],
    antenna_mask: u8,
    mcu_sequence: u8,
    scan_sequence: u8,
    selected: Option<CandidateChannel>,
    receive_prepared: bool,
    initialized: bool,
    active: Option<ActivePassiveScan>,
    delivery: VecDeque<TransportEvent>,
}

struct ActivePassiveScan {
    scan_id: u64,
    scan_sequence: u8,
    deadline_nanos: i64,
    remaining: VecDeque<CandidateChannel>,
    observations: Vec<RawAdvertisement>,
}

impl<M: SourceExactPassiveMechanics> SourceExactPassiveTransport<M> {
    pub fn new(
        mechanics: M,
        capability: NicCapability,
    ) -> Result<Self, SourceExactTransportError<M::Error>> {
        let mac = capability
            .mac_address
            .ok_or(SourceExactTransportError::MissingNicIdentity)?;
        if capability.phy.map(|phy| phy.spatial_streams) != Some(2) {
            return Err(SourceExactTransportError::UnsupportedSpatialStreams);
        }
        Ok(Self {
            mechanics,
            mac,
            antenna_mask: 3,
            mcu_sequence: 0,
            scan_sequence: 0,
            selected: None,
            receive_prepared: false,
            initialized: false,
            active: None,
            delivery: VecDeque::new(),
        })
    }

    pub fn into_mechanics(self) -> M {
        self.mechanics
    }

    /// Execute the source-ordered EEPROM-buffer command and mandatory receive
    /// preparation without enabling MAC/channel/scan operation.
    pub fn prepare_receive_only(
        &mut self,
    ) -> Result<PassivePrerequisites, SourceExactTransportError<M::Error>> {
        if self.receive_prepared {
            return Ok(PassivePrerequisites {
                channel_domain_mask_zero: true,
                mac_mmio_initialized: true,
                data_rx_owned: true,
            });
        }
        self.issue(PassiveMcuCommand::EepromBufferMode)?;
        let prerequisites = self
            .mechanics
            .prepare_passive_receive()
            .map_err(SourceExactTransportError::Mechanics)?;
        if prerequisites
            != (PassivePrerequisites {
                channel_domain_mask_zero: true,
                mac_mmio_initialized: true,
                data_rx_owned: true,
            })
        {
            return Err(SourceExactTransportError::MandatoryDependency(
                prerequisites,
            ));
        }
        self.receive_prepared = true;
        Ok(prerequisites)
    }

    fn issue(
        &mut self,
        command: PassiveMcuCommand,
    ) -> Result<(), SourceExactTransportError<M::Error>> {
        self.mcu_sequence = self.mcu_sequence % 15 + 1;
        let encoded = encode_passive_mcu_command(&command, self.mcu_sequence)
            .map_err(SourceExactTransportError::Encode)?;
        let wait = command.expects_response();
        self.mechanics
            .command(&command, &encoded, wait)
            .map_err(SourceExactTransportError::Mechanics)
    }
}

impl<M: SourceExactPassiveMechanics> Mt7921PassiveTransport for SourceExactPassiveTransport<M> {
    type Error = SourceExactTransportError<M::Error>;

    fn set_channel(&mut self, channel: CandidateChannel) -> Result<(), Self::Error> {
        if !self.initialized {
            self.prepare_receive_only()?;
            self.issue(PassiveMcuCommand::MacEnable)?;
            self.issue(PassiveMcuCommand::SetRxPath {
                channel,
                antenna_mask: self.antenna_mask,
            })?;
            self.issue(PassiveMcuCommand::AddDevice { mac: self.mac })?;
            self.issue(PassiveMcuCommand::AddBss)?;
            self.issue(PassiveMcuCommand::SetPassiveRxFilter)?;
            self.initialized = true;
        }
        self.issue(PassiveMcuCommand::ChannelSwitch {
            channel,
            antenna_mask: self.antenna_mask,
        })?;
        self.selected = Some(channel);
        Ok(())
    }

    fn start_passive_scan(&mut self, command: PassiveScanCommand) -> Result<(), Self::Error> {
        let Some((&channel, remaining)) = command.channels.split_first() else {
            return Err(SourceExactTransportError::UnsupportedMultiChannelScan);
        };
        if self.selected != Some(channel) {
            self.set_channel(channel)?;
        }
        if command.min_channel_time_nanos < 0
            || command.max_channel_time_nanos < command.min_channel_time_nanos
            || command.max_channel_time_nanos > 500_000_000
        {
            return Err(SourceExactTransportError::InvalidDwell);
        }
        self.scan_sequence = (self.scan_sequence + 1) & 0x7f;
        self.issue(PassiveMcuCommand::StartScan {
            scan_sequence: self.scan_sequence,
            channel,
        })?;
        self.active = Some(ActivePassiveScan {
            scan_id: command.scan_id,
            scan_sequence: self.scan_sequence,
            deadline_nanos: command.max_channel_time_nanos,
            remaining: remaining.iter().copied().collect(),
            observations: Vec::new(),
        });
        Ok(())
    }

    fn cancel_passive_scan(&mut self, scan_id: u64) -> Result<(), Self::Error> {
        let active = self
            .active
            .as_mut()
            .ok_or(SourceExactTransportError::InvalidSequence)?;
        if scan_id != active.scan_id {
            return Err(SourceExactTransportError::InvalidSequence);
        }
        let scan_sequence = active.scan_sequence;
        active.remaining.clear();
        active.observations.clear();
        self.delivery.clear();
        self.issue(PassiveMcuCommand::CancelScan { scan_sequence })
    }

    fn next_event(&mut self) -> Result<Option<TransportEvent>, Self::Error> {
        if let Some(event) = self.delivery.pop_front() {
            return Ok(Some(event));
        }
        let Some(active) = self.active.as_ref() else {
            return Ok(None);
        };
        let scan_id = active.scan_id;
        let scan_sequence = active.scan_sequence;
        let deadline = active.deadline_nanos;
        match self
            .mechanics
            .next_event(deadline)
            .map_err(SourceExactTransportError::Mechanics)?
        {
            None => Ok(None),
            Some(PassiveMechanicsEvent::Advertisement {
                timestamp_nanos,
                advertisement,
            }) => {
                let raw = RawAdvertisement {
                    scan_id,
                    kind: if advertisement.probe_response {
                        AdvertisementKind::ProbeResponse
                    } else {
                        AdvertisementKind::Beacon
                    },
                    timestamp_nanos,
                    bssid: advertisement.bssid,
                    beacon_interval_tu: advertisement.beacon_interval_tu,
                    capability_info: advertisement.capability_info,
                    ies: advertisement.ies,
                    channel: CandidateChannel {
                        band: advertisement.band,
                        number: advertisement.channel.into(),
                        frequency_mhz: match advertisement.band {
                            PhysicalBand::Ghz2 if advertisement.channel == 14 => 2484,
                            PhysicalBand::Ghz2 => 2407 + 5 * u16::from(advertisement.channel),
                            PhysicalBand::Ghz5 => 5000 + 5 * u16::from(advertisement.channel),
                            PhysicalBand::Ghz6 => 5950 + 5 * u16::from(advertisement.channel),
                        },
                    },
                    rssi_dbm: advertisement.rssi_dbm,
                };
                let observations = &mut self.active.as_mut().expect("active above").observations;
                if let Some(existing) = observations.iter_mut().find(|item| item.bssid == raw.bssid)
                {
                    if raw.rssi_dbm > existing.rssi_dbm {
                        *existing = raw;
                    }
                } else {
                    observations.push(raw);
                }
                Ok(None)
            }
            Some(PassiveMechanicsEvent::ScanDone(done)) => {
                if done.scan_sequence != scan_sequence {
                    return Err(SourceExactTransportError::ScanIdMismatch {
                        expected: scan_sequence,
                        actual: done.scan_sequence,
                    });
                }
                let success = done.completed_channels == 1 && done.alpha2 == *b"00";
                let mut active = self.active.take().expect("active above");
                if success && let Some(channel) = active.remaining.pop_front() {
                    self.issue(PassiveMcuCommand::ChannelSwitch {
                        channel,
                        antenna_mask: self.antenna_mask,
                    })?;
                    self.selected = Some(channel);
                    self.scan_sequence = (self.scan_sequence + 1) & 0x7f;
                    self.issue(PassiveMcuCommand::StartScan {
                        scan_sequence: self.scan_sequence,
                        channel,
                    })?;
                    active.scan_sequence = self.scan_sequence;
                    self.active = Some(active);
                    return Ok(None);
                }
                active
                    .observations
                    .sort_by_key(|observation| observation.timestamp_nanos);
                self.delivery.extend(
                    active
                        .observations
                        .into_iter()
                        .map(TransportEvent::Advertisement),
                );
                self.delivery
                    .push_back(TransportEvent::Complete { scan_id, success });
                Ok(self.delivery.pop_front())
            }
        }
    }
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
        self.ensure_live()?;
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

    #[derive(Default)]
    struct ScriptedMechanics {
        prerequisites: Option<PassivePrerequisites>,
        commands: Vec<(PassiveMcuCommand, Vec<u8>, bool)>,
        prepare_after_commands: Option<usize>,
        events: VecDeque<PassiveMechanicsEvent>,
    }

    impl SourceExactPassiveMechanics for ScriptedMechanics {
        type Error = ScriptError;

        fn prepare_passive_receive(&mut self) -> Result<PassivePrerequisites, Self::Error> {
            self.prepare_after_commands = Some(self.commands.len());
            Ok(self.prerequisites.unwrap_or(PassivePrerequisites {
                channel_domain_mask_zero: true,
                mac_mmio_initialized: true,
                data_rx_owned: true,
            }))
        }

        fn command(
            &mut self,
            command: &PassiveMcuCommand,
            encoded: &[u8],
            wait_response: bool,
        ) -> Result<(), Self::Error> {
            self.commands
                .push((command.clone(), encoded.to_vec(), wait_response));
            Ok(())
        }

        fn next_event(
            &mut self,
            _deadline_nanos: i64,
        ) -> Result<Option<PassiveMechanicsEvent>, Self::Error> {
            Ok(self.events.pop_front())
        }
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
    fn real_fuchsia_adapter_drives_source_exact_passive_closure() {
        let capability = nic();
        let transport =
            SourceExactPassiveTransport::new(ScriptedMechanics::default(), capability).unwrap();
        let mut adapter = Mt7921SoftmacAdapter::new(
            transport,
            capability,
            capability_channels(capability),
            vec![channel(1)],
        )
        .unwrap();
        adapter
            .set_channel(WlanSoftmacBaseSetChannelRequest {
                primary: Some(channel(1)),
                bandwidth: Some(ChannelBandwidth::Cbw20),
                vht_secondary_80_channel: Some(channel(0)),
            })
            .unwrap();
        let response = adapter
            .start_passive_scan(WlanSoftmacBaseStartPassiveScanRequest {
                channels: Some(vec![channel(1)]),
                min_channel_time: Some(50_000_000),
                max_channel_time: Some(120_000_000),
                min_home_time: Some(0),
            })
            .unwrap();
        assert_eq!(response.scan_id, Some(1));
        let commands = &adapter.transport.mechanics.commands;
        assert_eq!(adapter.transport.mechanics.prepare_after_commands, Some(1));
        assert_eq!(commands.len(), 8);
        assert!(matches!(commands[0].0, PassiveMcuCommand::EepromBufferMode));
        assert!(matches!(commands[1].0, PassiveMcuCommand::MacEnable));
        assert!(matches!(commands[2].0, PassiveMcuCommand::SetRxPath { .. }));
        assert!(matches!(commands[3].0, PassiveMcuCommand::AddDevice { .. }));
        assert!(matches!(commands[4].0, PassiveMcuCommand::AddBss));
        assert!(matches!(
            commands[5].0,
            PassiveMcuCommand::SetPassiveRxFilter
        ));
        assert!(matches!(
            commands[6].0,
            PassiveMcuCommand::ChannelSwitch { .. }
        ));
        assert!(matches!(commands[7].0, PassiveMcuCommand::StartScan { .. }));
        assert!(!commands[7].2);
        let scan_request = &commands[7].1[64..];
        assert_eq!(scan_request[2], 0);
        assert_eq!(scan_request[4], 0);
        assert_eq!(scan_request[5], 0);
        assert!(scan_request[224..826].iter().all(|byte| *byte == 0));

        adapter
            .transport
            .mechanics
            .events
            .push_back(PassiveMechanicsEvent::Advertisement {
                timestamp_nanos: 10,
                advertisement: PassiveAdvertisement {
                    probe_response: false,
                    bssid: [1, 2, 3, 4, 5, 6],
                    beacon_interval_tu: 100,
                    capability_info: 0x0431,
                    ies: vec![0, 3, b'a', b'p', b'1'],
                    band: PhysicalBand::Ghz2,
                    channel: 1,
                    rssi_dbm: -50,
                },
            });
        adapter
            .transport
            .mechanics
            .events
            .push_back(PassiveMechanicsEvent::ScanDone(PassiveScanDone {
                scan_sequence: 1,
                completed_channels: 1,
                beacon_scan_count: 1,
                alpha2: *b"00",
            }));
        assert_eq!(adapter.next_scan_event(), Ok(None));
        assert!(matches!(
            adapter.next_scan_event(),
            Ok(Some(HardwareScanEvent::Observation(_)))
        ));
        assert_eq!(
            adapter.next_scan_event(),
            Ok(Some(HardwareScanEvent::Complete {
                scan_id: 1,
                success: true,
            }))
        );
    }

    #[test]
    fn multi_channel_scan_aggregates_strongest_bss_before_sme_delivery() {
        let capability = nic();
        let transport =
            SourceExactPassiveTransport::new(ScriptedMechanics::default(), capability).unwrap();
        let mut adapter = Mt7921SoftmacAdapter::new(
            transport,
            capability,
            capability_channels(capability),
            vec![channel(1), channel(6)],
        )
        .unwrap();
        assert_eq!(
            adapter
                .start_passive_scan(request(vec![channel(1), channel(6)]))
                .unwrap()
                .scan_id,
            Some(1)
        );

        let advertisement =
            |timestamp_nanos, bssid, channel, rssi_dbm| PassiveMechanicsEvent::Advertisement {
                timestamp_nanos,
                advertisement: PassiveAdvertisement {
                    probe_response: false,
                    bssid,
                    beacon_interval_tu: 100,
                    capability_info: 0x0431,
                    ies: vec![0, 1, b'x'],
                    band: PhysicalBand::Ghz2,
                    channel,
                    rssi_dbm,
                },
            };
        adapter.transport.mechanics.events.extend([
            advertisement(10, [1; 6], 1, -70),
            PassiveMechanicsEvent::ScanDone(PassiveScanDone {
                scan_sequence: 1,
                completed_channels: 1,
                beacon_scan_count: 1,
                alpha2: *b"00",
            }),
            advertisement(20, [1; 6], 6, -40),
            advertisement(21, [2; 6], 6, -60),
            PassiveMechanicsEvent::ScanDone(PassiveScanDone {
                scan_sequence: 2,
                completed_channels: 1,
                beacon_scan_count: 2,
                alpha2: *b"00",
            }),
        ]);
        assert_eq!(adapter.next_scan_event(), Ok(None));
        assert_eq!(adapter.next_scan_event(), Ok(None));
        assert_eq!(adapter.next_scan_event(), Ok(None));
        assert_eq!(adapter.next_scan_event(), Ok(None));
        let Some(HardwareScanEvent::Observation(first)) = adapter.next_scan_event().unwrap() else {
            panic!("missing first aggregate")
        };
        assert_eq!(first.bss.bssid, [1; 6]);
        assert_eq!(first.bss.rssi_dbm, -40);
        let Some(HardwareScanEvent::Observation(second)) = adapter.next_scan_event().unwrap()
        else {
            panic!("missing second aggregate")
        };
        assert_eq!(second.bss.bssid, [2; 6]);
        assert_eq!(
            adapter.next_scan_event().unwrap(),
            Some(HardwareScanEvent::Complete {
                scan_id: 1,
                success: true,
            })
        );
    }

    #[test]
    fn aggregated_multi_channel_results_reach_pinned_fuchsia_scanner() {
        let capability = nic();
        let transport =
            SourceExactPassiveTransport::new(ScriptedMechanics::default(), capability).unwrap();
        let mut adapter = Mt7921SoftmacAdapter::new(
            transport,
            capability,
            capability_channels(capability),
            vec![channel(1), channel(6)],
        )
        .unwrap();
        let mut scanner = fuchsia_softmac_port::PassiveScanner::default();
        scanner
            .start(
                &mut adapter,
                fuchsia_softmac_port::ScanRequest {
                    txn_id: 77,
                    scan_type: fuchsia_softmac_port::ScanTypes::Passive,
                    channel_list: vec![channel(1), channel(6)],
                    ssid_list: vec![],
                    probe_delay: 0,
                    min_channel_time: 50,
                    max_channel_time: 120,
                },
            )
            .unwrap();
        let advertisement =
            |timestamp_nanos, channel, rssi_dbm| PassiveMechanicsEvent::Advertisement {
                timestamp_nanos,
                advertisement: PassiveAdvertisement {
                    probe_response: false,
                    bssid: [9; 6],
                    beacon_interval_tu: 100,
                    capability_info: 1,
                    ies: vec![0, 1, b'x'],
                    band: PhysicalBand::Ghz2,
                    channel,
                    rssi_dbm,
                },
            };
        adapter.transport.mechanics.events.extend([
            advertisement(1, 1, -70),
            PassiveMechanicsEvent::ScanDone(PassiveScanDone {
                scan_sequence: 1,
                completed_channels: 1,
                beacon_scan_count: 1,
                alpha2: *b"00",
            }),
            advertisement(2, 6, -40),
            PassiveMechanicsEvent::ScanDone(PassiveScanDone {
                scan_sequence: 2,
                completed_channels: 1,
                beacon_scan_count: 1,
                alpha2: *b"00",
            }),
        ]);
        assert_eq!(scanner.poll(&mut adapter).unwrap(), None);
        assert_eq!(scanner.poll(&mut adapter).unwrap(), None);
        assert_eq!(scanner.poll(&mut adapter).unwrap(), None);
        let Some(fuchsia_softmac_port::MlmeScanEvent::Result { result, .. }) =
            scanner.poll(&mut adapter).unwrap()
        else {
            panic!("missing SME scan result")
        };
        assert_eq!(result.txn_id, 77);
        assert_eq!(result.bss.bssid, [9; 6]);
        assert_eq!(result.bss.rssi_dbm, -40);
        assert!(matches!(
            scanner.poll(&mut adapter).unwrap(),
            Some(fuchsia_softmac_port::MlmeScanEvent::End(_))
        ));
    }

    #[test]
    fn mandatory_passive_dependencies_fail_after_only_source_ordered_eeprom() {
        let capability = nic();
        let mechanics = ScriptedMechanics {
            prerequisites: Some(PassivePrerequisites {
                channel_domain_mask_zero: true,
                mac_mmio_initialized: false,
                data_rx_owned: true,
            }),
            ..Default::default()
        };
        let transport = SourceExactPassiveTransport::new(mechanics, capability).unwrap();
        let mut adapter = Mt7921SoftmacAdapter::new(
            transport,
            capability,
            capability_channels(capability),
            vec![channel(1)],
        )
        .unwrap();
        assert!(matches!(
            adapter.set_channel(WlanSoftmacBaseSetChannelRequest {
                primary: Some(channel(1)),
                bandwidth: Some(ChannelBandwidth::Cbw20),
                vht_secondary_80_channel: Some(channel(0)),
            }),
            Err(AdapterError::Transport(
                SourceExactTransportError::MandatoryDependency(_)
            ))
        ));
        assert_eq!(adapter.transport.mechanics.commands.len(), 1);
        assert!(matches!(
            adapter.transport.mechanics.commands[0].0,
            PassiveMcuCommand::EepromBufferMode
        ));
        assert_eq!(adapter.start_active_scan(), Err(AdapterError::Poisoned));
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
