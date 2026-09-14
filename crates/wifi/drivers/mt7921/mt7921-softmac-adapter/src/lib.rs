// SPDX-License-Identifier: GPL-2.0-only

//! Offline-capable MT7921 mechanics adapter for the pinned Fuchsia SoftMAC API.
//!
//! This crate contains no device, register, DMA, IRQ, firmware-loading, or host
//! networking implementation. A caller must provide an explicit transport.

pub mod client_device;
pub mod ethernet;
mod production_client;
pub mod production_effects;

pub use production_client::Mt7921ProductionClient;

use fidl_fuchsia_wlan_ieee80211::{HtCapabilities, VhtCapabilities};
use fuchsia_softmac_port::{
    AdvertisementKind, Bssid, CapabilityInfo, ChannelBandwidth, ChannelNumber, DiscoverySupport,
    HardwareScanEvent, ScanObservation, SoftmacHardware, TimeUnit, WlanBand, WlanPhyType,
    WlanRxInfo, WlanRxInfoFlags, WlanRxInfoValid, WlanSoftmacBandCapability,
    WlanSoftmacBaseCancelScanRequest, WlanSoftmacBaseSetChannelRequest,
    WlanSoftmacBaseStartPassiveScanRequest, WlanSoftmacBaseStartPassiveScanResponse,
    WlanSoftmacQueryResponse, construct_bss_description,
};
use mt7921_core::{
    CandidateChannel, ChannelSwitchReason, NicCapability, PassiveAdvertisement, PassiveMcuCommand,
    PassiveMcuCommandError, PassiveScanDone, PhysicalBand, RateTxPowerError,
    RegulatoryRatePowerSnapshot, SarFrequencyRange, candidate_channels as capability_channels,
    conservative_channel_domain, encode_passive_mcu_command,
    regulatory_rate_power_channel_skeleton,
};
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

/// One frame/status/provenance tuple carried only through the in-process
/// production client receive path. The provenance value is opaque here: this
/// shape cannot inspect, validate, clone, detach, or serialize it.
///
/// ```compile_fail
/// fn detach<P>(rx: mt7921_softmac_adapter::PinnedClientRx<P>) {
///     let _ = rx.provenance;
/// }
/// ```
///
/// ```compile_fail
/// fn duplicate<P>(rx: mt7921_softmac_adapter::PinnedClientRx<P>) {
///     let _ = rx.clone();
/// }
/// ```
///
/// ```compile_fail
/// fn fake_trusted_admission<P>(
///     rx: mt7921_softmac_adapter::PinnedClientRx<P>,
/// ) -> fidl_fuchsia_wlan_mlme::ScanResult {
///     rx.into()
/// }
/// ```
pub struct PinnedClientRx<P> {
    bytes: Vec<u8>,
    status: fidl_fuchsia_wlan_softmac::WlanRxInfo,
    provenance: P,
}

pub fn pinned_client_rx_from_connac2<P>(
    envelope: &[u8],
    provenance: P,
) -> Result<PinnedClientRx<P>, mt7921_core::PassiveRxError> {
    let frame = mt7921_core::parse_connac2_rx_frame(envelope)?;
    let band = match frame.band {
        PhysicalBand::Ghz2 => fidl_fuchsia_wlan_ieee80211::WlanBand::TwoGhz,
        PhysicalBand::Ghz5 => fidl_fuchsia_wlan_ieee80211::WlanBand::FiveGhz,
        PhysicalBand::Ghz6 => return Err(mt7921_core::PassiveRxError::InvalidChannel),
    };
    let primary = fidl_fuchsia_wlan_ieee80211::ChannelNumber {
        band,
        number: frame.channel,
    };
    Ok(PinnedClientRx::new(
        frame.bytes,
        fidl_fuchsia_wlan_softmac::WlanRxInfo {
            rx_flags: fidl_fuchsia_wlan_softmac::WlanRxInfoFlags::empty(),
            // The pinned client scanner consumes primary and RSSI. PHY, rate,
            // MCS, bandwidth, and SNR are semantic ignores on this path, so
            // their neutral placeholders are deliberately not marked valid.
            valid_fields: fidl_fuchsia_wlan_softmac::WlanRxInfoValid::RSSI,
            phy: fidl_fuchsia_wlan_ieee80211::WlanPhyType::Ofdm,
            data_rate: 0,
            primary,
            bandwidth: fidl_fuchsia_wlan_ieee80211::ChannelBandwidth::Cbw20,
            vht_secondary_80_channel: fidl_fuchsia_wlan_ieee80211::ChannelNumber {
                band,
                number: 0,
            },
            mcs: 0,
            rssi_dbm: frame.rssi_dbm,
            snr_dbh: 0,
        },
        provenance,
    ))
}

impl<P> PinnedClientRx<P> {
    fn new(bytes: Vec<u8>, status: fidl_fuchsia_wlan_softmac::WlanRxInfo, provenance: P) -> Self {
        Self {
            bytes,
            status,
            provenance,
        }
    }
}

/// Apply the purely structural carrier above to the actual pinned
/// `ClientMlme` receive path. Only the caller's observer can interpret `P`.
pub async fn handle_pinned_client_rx<D, P, O>(
    mlme: &mut wlan_mlme::client::ClientMlme<D>,
    rx: PinnedClientRx<P>,
    observer: &mut O,
) where
    D: wlan_mlme::device::DeviceOps,
    O: wlan_mlme::ScanResultObserver<P>,
{
    let PinnedClientRx {
        bytes,
        status,
        provenance,
    } = rx;
    mlme.handle_mac_frame_rx_observed(
        &bytes,
        status,
        fuchsia_trace::Id::new(),
        provenance,
        observer,
    )
    .await;
}

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
    fn set_channel_context(&mut self, context: PhysicalChannelContext) -> Result<(), Self::Error> {
        if context.center_channel != context.channel.number as u8
            || context.bandwidth != 0
            || context.center_channel2 != 0
        {
            return self.set_channel(context.channel);
        }
        self.set_channel(context.channel)
    }
    /// Linux `mt7921_set_channel`: CHANNEL_SWITCH with `CH_SWITCH_NORMAL` on
    /// the association chandef, issued before the JOIN ROC so the radio is
    /// calibrated for the connected channel instead of staying in the scan
    /// (`CH_SWITCH_SCAN_BYPASS_DPD`) form.
    fn establish_client_channel(
        &mut self,
        _: mt7921_core::ClientPhysicalChannel,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn start_passive_scan(&mut self, command: PassiveScanCommand) -> Result<(), Self::Error>;
    fn cancel_passive_scan(&mut self, scan_id: u64) -> Result<(), Self::Error>;
    fn next_event(&mut self) -> Result<Option<TransportEvent>, Self::Error>;

    /// Client operations deliberately use Zircon status rather than the scan
    /// error type: unsupported scan-only fakes remain valid, while a physical
    /// implementation must complete each operation synchronously.
    fn submit_client_uni(&mut self, _: u8, _: &[u8]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn submit_client_edca(&mut self, _: &[u8]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn submit_client_ce_no_ack(&mut self, _: &[u8]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn acquire_client_join_roc(
        &mut self,
        _: mt7921_core::ClientPhysicalChannel,
        _: u64,
        _: u32,
    ) -> Result<u32, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn client_join_roc_active(&mut self, _: u64) -> bool {
        false
    }
    fn abort_client_join_roc(&mut self, _: u64) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn diagnostic_association_snapshot(&mut self, _: u64) -> Result<(), zx::Status> {
        Ok(())
    }
    fn passive_m1_snapshot(
        &mut self,
        _: client_device::PassiveM1SnapshotPoint,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn transmit_client(
        &mut self,
        _: &[u8],
        _: fidl_fuchsia_wlan_softmac::WlanTxInfoFlags,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn next_client_rx(&mut self) -> Result<Option<client_device::ClientRxFrame>, zx::Status> {
        Ok(None)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalChannelContext {
    pub channel: CandidateChannel,
    pub center_channel: u8,
    pub bandwidth: u8,
    pub center_channel2: u8,
    pub switch_reason: ChannelSwitchReason,
}

/// Linux `ieee80211_channel_to_frequency` for the bands MT7921 serves; the
/// client channel carries only band and number.
pub fn client_channel_candidate(
    channel: mt7921_core::ClientPhysicalChannel,
) -> Option<CandidateChannel> {
    let number = channel.primary;
    match channel.band {
        0 if (1..=14).contains(&number) => Some(CandidateChannel {
            band: mt7921_core::PhysicalBand::Ghz2,
            number,
            frequency_mhz: if number == 14 {
                2484
            } else {
                2407 + 5 * number
            },
        }),
        1 if (36..=177).contains(&number) => Some(CandidateChannel {
            band: mt7921_core::PhysicalBand::Ghz5,
            number,
            frequency_mhz: 5000 + 5 * number,
        }),
        _ => None,
    }
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

    /// Current Linux-style MCU message sequence when mechanics joins an
    /// already-bootstrapped command domain. `None` starts a fresh domain.
    fn current_mcu_sequence(&self) -> Option<u8> {
        None
    }

    fn prepare_passive_receive(&mut self) -> Result<PassivePrerequisites, Self::Error>;
    fn command(
        &mut self,
        command: &PassiveMcuCommand,
        encoded: &[u8],
        wait_response: bool,
    ) -> Result<(), Self::Error>;
    /// Linux installs the complete no-ACK rate-power transaction immediately
    /// after the one-time SET_RX_PATH command.
    fn install_rate_tx_power(&mut self, capability: NicCapability) -> Result<(), Self::Error>;
    fn next_event(
        &mut self,
        deadline_nanos: i64,
    ) -> Result<Option<PassiveMechanicsEvent>, Self::Error>;
    fn confirm_scan_done(&mut self, scan_sequence: u8) -> Result<(), Self::Error>;
    fn submit_client_uni(&mut self, _: u8, _: &[u8]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn submit_client_edca(&mut self, _: &[u8]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn submit_client_ce_no_ack(&mut self, _: &[u8]) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn acquire_client_join_roc(
        &mut self,
        _: mt7921_core::ClientPhysicalChannel,
        _: u64,
        _: u32,
    ) -> Result<u32, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn client_join_roc_active(&mut self, _: u64) -> bool {
        false
    }
    fn abort_client_join_roc(&mut self, _: u64) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn diagnostic_association_snapshot(&mut self, _: u64) -> Result<(), zx::Status> {
        Ok(())
    }
    fn passive_m1_snapshot(
        &mut self,
        _: client_device::PassiveM1SnapshotPoint,
    ) -> Result<(), zx::Status> {
        Ok(())
    }
    fn transmit_client(
        &mut self,
        _: &[u8],
        _: fidl_fuchsia_wlan_softmac::WlanTxInfoFlags,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn next_client_rx(&mut self) -> Result<Option<client_device::ClientRxFrame>, zx::Status> {
        Ok(None)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum SourceExactTransportError<E> {
    MissingNicIdentity,
    UnsupportedSpatialStreams,
    MandatoryDependency(PassivePrerequisites),
    InvalidSequence,
    InvalidChannelDomain,
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
    capability: NicCapability,
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
        let mcu_sequence = mechanics.current_mcu_sequence().unwrap_or(0);
        if mcu_sequence > 15 {
            return Err(SourceExactTransportError::InvalidSequence);
        }
        Ok(Self {
            mechanics,
            capability,
            mac,
            antenna_mask: 3,
            mcu_sequence,
            scan_sequence: 0,
            selected: None,
            receive_prepared: false,
            initialized: false,
            active: None,
            delivery: VecDeque::new(),
        })
    }

    /// Override the interface MAC published by DEV_INFO_ACTIVE during
    /// initialization. Linux programs DEV_INFO exactly once per interface-up
    /// with the interface address mac80211 was given, so a client session
    /// must present its own identity here rather than the EEPROM address;
    /// firmware keeps the first address it saw in the RMAC own-MAC table.
    pub fn with_interface_mac(mut self, mac: [u8; 6]) -> Self {
        self.mac = mac;
        self
    }

    pub fn into_mechanics(self) -> M {
        self.mechanics
    }

    pub fn mechanics_mut(&mut self) -> &mut M {
        &mut self.mechanics
    }

    /// Execute mandatory receive preparation without replaying loader-owned
    /// EEPROM/protection commands.
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
        let template_sequence = self.mcu_sequence % 15 + 1;
        let encoded = encode_passive_mcu_command(&command, template_sequence)
            .map_err(SourceExactTransportError::Encode)?;
        let wait = command.expects_response();
        self.mechanics
            .command(&command, &encoded, wait)
            .map_err(SourceExactTransportError::Mechanics)?;
        self.mcu_sequence = self
            .mechanics
            .current_mcu_sequence()
            .unwrap_or(template_sequence);
        Ok(())
    }

    fn install_rate_tx_power(&mut self) -> Result<(), SourceExactTransportError<M::Error>> {
        self.mechanics
            .install_rate_tx_power(self.capability)
            .map_err(SourceExactTransportError::Mechanics)?;
        if let Some(sequence) = self.mechanics.current_mcu_sequence() {
            if !(1..=15).contains(&sequence) {
                return Err(SourceExactTransportError::InvalidSequence);
            }
            self.mcu_sequence = sequence;
        }
        Ok(())
    }
}

impl<M: SourceExactPassiveMechanics> Mt7921PassiveTransport for SourceExactPassiveTransport<M> {
    type Error = SourceExactTransportError<M::Error>;

    fn submit_client_uni(&mut self, expected_cid: u8, encoded: &[u8]) -> Result<(), zx::Status> {
        self.mechanics.submit_client_uni(expected_cid, encoded)
    }
    fn submit_client_edca(&mut self, encoded: &[u8]) -> Result<(), zx::Status> {
        self.mechanics.submit_client_edca(encoded)
    }
    fn submit_client_ce_no_ack(&mut self, encoded: &[u8]) -> Result<(), zx::Status> {
        self.mechanics.submit_client_ce_no_ack(encoded)
    }
    fn acquire_client_join_roc(
        &mut self,
        channel: mt7921_core::ClientPhysicalChannel,
        generation: u64,
        duration_ms: u32,
    ) -> Result<u32, zx::Status> {
        self.mechanics
            .acquire_client_join_roc(channel, generation, duration_ms)
    }
    fn establish_client_channel(
        &mut self,
        channel: mt7921_core::ClientPhysicalChannel,
    ) -> Result<(), zx::Status> {
        let candidate = client_channel_candidate(channel).ok_or(zx::Status::INVALID_ARGS)?;
        if channel.center > u16::from(u8::MAX) || channel.center2 > u16::from(u8::MAX) {
            return Err(zx::Status::INVALID_ARGS);
        }
        self.set_channel_context(PhysicalChannelContext {
            channel: candidate,
            center_channel: channel.center as u8,
            bandwidth: channel.bandwidth,
            center_channel2: channel.center2 as u8,
            switch_reason: ChannelSwitchReason::Normal,
        })
        .map_err(|_| zx::Status::IO)
    }
    fn client_join_roc_active(&mut self, generation: u64) -> bool {
        self.mechanics.client_join_roc_active(generation)
    }
    fn abort_client_join_roc(&mut self, generation: u64) -> Result<(), zx::Status> {
        self.mechanics.abort_client_join_roc(generation)
    }
    fn diagnostic_association_snapshot(&mut self, generation: u64) -> Result<(), zx::Status> {
        self.mechanics.diagnostic_association_snapshot(generation)
    }
    fn passive_m1_snapshot(
        &mut self,
        point: client_device::PassiveM1SnapshotPoint,
    ) -> Result<(), zx::Status> {
        self.mechanics.passive_m1_snapshot(point)
    }

    fn transmit_client(
        &mut self,
        bytes: &[u8],
        flags: fidl_fuchsia_wlan_softmac::WlanTxInfoFlags,
    ) -> Result<(), zx::Status> {
        self.mechanics.transmit_client(bytes, flags)
    }

    fn next_client_rx(&mut self) -> Result<Option<client_device::ClientRxFrame>, zx::Status> {
        self.mechanics.next_client_rx()
    }

    fn set_channel(&mut self, channel: CandidateChannel) -> Result<(), Self::Error> {
        self.set_channel_context(PhysicalChannelContext {
            channel,
            center_channel: channel.number as u8,
            bandwidth: 0,
            center_channel2: 0,
            switch_reason: ChannelSwitchReason::ScanBypassDpd,
        })
    }

    fn set_channel_context(&mut self, context: PhysicalChannelContext) -> Result<(), Self::Error> {
        let channel = context.channel;
        if !self.initialized {
            self.prepare_receive_only()?;
            // Registration's regulatory notifier publishes one complete SKU
            // batch before runtime-power and PHY start.
            self.install_rate_tx_power()?;
            self.issue(PassiveMcuCommand::KeepFullPower)?;
            self.issue(PassiveMcuCommand::MacEnable)?;
            let domain = conservative_channel_domain(self.capability, *b"00", true, 0)
                .map_err(|_| SourceExactTransportError::InvalidChannelDomain)?;
            self.issue(PassiveMcuCommand::SetChannelDomain(domain))?;
            self.issue(PassiveMcuCommand::SetRxPath {
                // Linux starts the PHY with mac80211's initial 2.4 GHz
                // channel definition, then applies the requested channel
                // through CHANNEL_SWITCH.  Do not fold the first requested
                // scan/association channel into this one-time RX-path setup.
                channel: CandidateChannel {
                    band: PhysicalBand::Ghz2,
                    number: 1,
                    frequency_mhz: 2412,
                },
                antenna_mask: self.antenna_mask,
            })?;
            self.install_rate_tx_power()?;
            self.issue(PassiveMcuCommand::RadioLedCtrl { value: 1 })?;
            self.issue(PassiveMcuCommand::RadioLedCtrl { value: 2 })?;
            self.issue(PassiveMcuCommand::AddDevice { mac: self.mac })?;
            self.issue(PassiveMcuCommand::AddBss)?;
            self.issue(PassiveMcuCommand::InitialEdca)?;
            self.issue(PassiveMcuCommand::SetPassiveRxFilter)?;
            self.initialized = true;
        }
        self.issue(PassiveMcuCommand::ChannelSwitch {
            channel,
            center_channel: context.center_channel,
            bandwidth: context.bandwidth,
            center_channel2: context.center_channel2,
            antenna_mask: self.antenna_mask,
            switch_reason: context.switch_reason,
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
            eprintln!(
                "passive_scan_start_rejected reason=invalid_dwell min_channel_time={} max_channel_time={}",
                command.min_channel_time_nanos, command.max_channel_time_nanos
            );
            return Err(SourceExactTransportError::InvalidDwell);
        }
        self.scan_sequence = (self.scan_sequence + 1) & 0x7f;
        if let Err(error) = self.issue(PassiveMcuCommand::StartScan {
            scan_sequence: self.scan_sequence,
            channel,
        }) {
            eprintln!("passive_scan_start_rejected reason=start_scan_command");
            return Err(error);
        }
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
                self.mechanics
                    .confirm_scan_done(scan_sequence)
                    .map_err(SourceExactTransportError::Mechanics)?;
                let success = done.completed_channels == 1 && done.alpha2 == *b"00";
                let mut active = self.active.take().expect("active above");
                if success && let Some(channel) = active.remaining.pop_front() {
                    self.issue(PassiveMcuCommand::ChannelSwitch {
                        channel,
                        center_channel: channel.number as u8,
                        bandwidth: 0,
                        center_channel2: 0,
                        antenna_mask: self.antenna_mask,
                        switch_reason: ChannelSwitchReason::ScanBypassDpd,
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

    /// Borrow the physical edge for one contained operation while retaining
    /// the adapter's scan/lifecycle ownership.
    pub fn with_transport_mut<R>(&mut self, operation: impl FnOnce(&mut T) -> R) -> R {
        operation(&mut self.transport)
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
        let bandwidth = request.bandwidth.ok_or(AdapterError::InvalidRequest)?;
        if !self.authorized.contains(&primary) {
            return Err(AdapterError::UnauthorizedChannel(primary));
        }
        let candidate = channel_to_candidate(primary, &self.candidates)
            .ok_or(AdapterError::UnauthorizedChannel(primary))?;
        let shape =
            LinuxChannelShape::from_fidl(primary, bandwidth, request.vht_secondary_80_channel)
                .ok_or(AdapterError::UnsupportedChannelWidth)?;
        self.transport
            .set_channel_context(PhysicalChannelContext {
                channel: candidate,
                center_channel: shape.center_channel,
                bandwidth: shape.bandwidth,
                center_channel2: shape.center_channel2,
                // Linux mt7921_config -> mt7921_set_channel: the configured
                // (operating) chandef is switched with CH_SWITCH_NORMAL; only
                // the scan-driven switches use CH_SWITCH_SCAN_BYPASS_DPD.
                switch_reason: ChannelSwitchReason::Normal,
            })
            .map_err(|error| self.transport_failure(error))?;
        Ok(())
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

/// Convert Fuchsia's channel notation to Linux v7.1.5 mt7921 MCU chandef
/// fields (`center_ch`, `bw`, `center_ch2`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinuxChannelShape {
    pub center_channel: u8,
    pub bandwidth: u8,
    pub center_channel2: u8,
}

pub fn set_channel_request(
    primary: ChannelNumber,
    bandwidth: ChannelBandwidth,
    secondary80: Option<ChannelNumber>,
) -> WlanSoftmacBaseSetChannelRequest {
    WlanSoftmacBaseSetChannelRequest {
        primary: Some(primary),
        bandwidth: Some(bandwidth),
        vht_secondary_80_channel: secondary80,
    }
}

impl LinuxChannelShape {
    pub fn from_fidl(
        primary: ChannelNumber,
        bandwidth: ChannelBandwidth,
        secondary80: Option<ChannelNumber>,
    ) -> Option<Self> {
        let definition =
            wlan_softmac_class_support::ChannelDefinition::new(primary, bandwidth, secondary80)
                .ok()?;
        let primary_number = definition.primary().number;
        let secondary_number = definition.secondary80().map_or(0, |channel| channel.number);
        let center80 = match primary_number {
            36..=48 => 42,
            52..=64 => 58,
            100..=112 => 106,
            116..=128 => 122,
            132..=144 => 138,
            148..=161 => 155,
            _ => 0,
        };
        let (center_channel, bandwidth, center_channel2) = match bandwidth {
            ChannelBandwidth::Cbw20 => (primary_number, 0, 0),
            ChannelBandwidth::Cbw40 => (primary_number.checked_add(2)?, 1, 0),
            ChannelBandwidth::Cbw40Below => (primary_number.checked_sub(2)?, 1, 0),
            ChannelBandwidth::Cbw80 if center80 != 0 => (center80, 2, 0),
            ChannelBandwidth::Cbw160 => match primary_number {
                36..=64 => (50, 3, 0),
                100..=128 => (114, 3, 0),
                _ => return None,
            },
            ChannelBandwidth::Cbw80P80
                if center80 != 0
                    && matches!(secondary_number, 42 | 58 | 106 | 122 | 138 | 155)
                    && secondary_number != center80 =>
            {
                (center80, 6, secondary_number)
            }
            _ => return None,
        };
        Some(Self {
            center_channel,
            bandwidth,
            center_channel2,
        })
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FuchsiaRegulatoryPower {
    pub channel: ChannelNumber,
    pub max_reg_power_dbm: i8,
}

/// Freeze the Fuchsia-reported channel universe and current authorization into
/// the core's complete MT7921 table. A present, enabled channel without a
/// matching power entry remains explicitly incomplete and will be rejected by
/// the core before any page is sent. The pinned Fuchsia API currently exposes
/// channel identity but not this power value.
pub fn regulatory_rate_power_snapshot_from_fuchsia(
    generation: u64,
    alpha2: [u8; 2],
    capability: NicCapability,
    candidates: &[CandidateChannel],
    authorized: &[ChannelNumber],
    powers: &[FuchsiaRegulatoryPower],
    sar_ranges: Vec<SarFrequencyRange>,
    external_cap_half_dbm: Option<i8>,
) -> Result<RegulatoryRatePowerSnapshot, RateTxPowerError> {
    let mut channels = regulatory_rate_power_channel_skeleton(capability)?;
    for input in &mut channels {
        let candidate = candidates
            .iter()
            .copied()
            .find(|candidate| candidate.band == input.band && candidate.number == input.channel);
        let Some(candidate) = candidate else { continue };
        if candidate.frequency_mhz != input.frequency_mhz {
            return Err(RateTxPowerError::InvalidChannelFrequency);
        }
        input.present = true;
        let fuchsia_channel =
            to_fuchsia_channel(candidate).ok_or(RateTxPowerError::IncompleteSnapshot)?;
        input.disabled = !authorized.contains(&fuchsia_channel);
        input.max_reg_power_dbm = powers
            .iter()
            .find(|power| power.channel == fuchsia_channel)
            .map(|power| power.max_reg_power_dbm);
    }
    Ok(RegulatoryRatePowerSnapshot::new(
        generation,
        alpha2,
        channels,
        sar_ranges,
        external_cap_half_dbm,
    ))
}

pub fn query_from_capabilities(
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
    let phy = nic.phy;
    for (physical, wlan) in [
        (PhysicalBand::Ghz2, WlanBand::TwoGhz),
        (PhysicalBand::Ghz5, WlanBand::FiveGhz),
    ] {
        let primary_channels = channels_for(physical, wlan);
        if !primary_channels.is_empty() {
            let (ht_caps, vht_caps) = phy
                .filter(|phy| phy.ht)
                .map(mt7921_ht_vht_capabilities)
                .unwrap_or((None, None));
            band_caps.push(WlanSoftmacBandCapability {
                band: Some(wlan),
                ht_caps,
                vht_caps: (wlan == WlanBand::FiveGhz).then_some(vht_caps).flatten(),
                primary_channels: Some(primary_channels),
                ..Default::default()
            });
        }
    }
    let mut phys = vec![WlanPhyType::Ofdm];
    if let Some(phy) = phy {
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

// Linux v7.1.5 mt76/mac80211 capability initialization plus the supported
// association-time narrowing performed by mac80211's HT/VHT IE builders. The
// firmware NIC PHY TLV supplies the modes and stream count; this host
// representation deliberately omits HE because the pinned Fuchsia
// BandCapability has no HE field.
fn mt7921_ht_vht_capabilities(
    phy: mt7921_core::NicPhyCapability,
) -> (Option<HtCapabilities>, Option<VhtCapabilities>) {
    let streams = usize::from(phy.spatial_streams.clamp(1, 8));
    let mut ht = [0; 26];
    // mt76 initializes LDPC, 20/40, greenfield, SGI20/40, RX-STBC-1 and max
    // AMSDU. ieee80211_add_ht_ie then writes the disabled SMPS encoding when
    // runtime SMPS is off; leaving the two-bit field zero means static SMPS.
    let mut ht_cap = 0x097fu16;
    if streams > 1 {
        ht_cap |= 0x0080;
    }
    ht[0..2].copy_from_slice(&ht_cap.to_le_bytes());
    ht[2] = 3; // IEEE80211_HT_MAX_AMPDU_64K, default density zero.
    ht[3..3 + streams.min(10)].fill(0xff);
    ht[15] = 1; // IEEE80211_HT_MCS_TX_DEFINED.

    let vht = phy.vht.then(|| {
        let mut bytes = [0; 12];
        // MT7961's association subset: MPDU-11454 | RX-LDPC | SGI80 |
        // RX-STBC-1 | SU beamformee | beamformee STS-3 | max AMPDU exponent |
        // invariant antenna patterns. Registration includes MU beamformee,
        // but ieee80211_add_vht_ie removes it without AP MU-beamformer support.
        let mut cap = 0x3380_7132u32;
        if streams > 1 {
            cap |= 0x0000_0080;
        }
        bytes[0..4].copy_from_slice(&cap.to_le_bytes());
        let mut mcs_map = 0u16;
        for stream in 0..8 {
            let supported = if stream < streams { 2 } else { 3 };
            mcs_map |= supported << (stream * 2);
        }
        bytes[4..6].copy_from_slice(&mcs_map.to_le_bytes());
        bytes[8..10].copy_from_slice(&mcs_map.to_le_bytes());
        // mt76_init_stream_cap sets IEEE80211_VHT_EXT_NSS_BW_CAPABLE after
        // mt792x advertises SUPPORTS_VHT_EXT_NSS_BW.
        bytes[10..12].copy_from_slice(&0x2000u16.to_le_bytes());
        VhtCapabilities { bytes }
    });
    (Some(HtCapabilities { bytes: ht }), vht)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn mt7921_two_stream_5ghz_query_reports_linux_ht_vht_subset() {
        let nic = NicCapability {
            element_count: 1,
            mac_address: Some([2, 0, 0, 0, 0, 1]),
            phy: Some(mt7921_core::NicPhyCapability {
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
        };
        let query = query_from_capabilities(
            nic,
            &[CandidateChannel {
                band: PhysicalBand::Ghz5,
                number: 36,
                frequency_mhz: 5180,
            }],
        );
        let band = &query.band_caps.unwrap()[0];
        let ht = band.ht_caps.unwrap().bytes;
        let vht = band.vht_caps.unwrap().bytes;
        assert_eq!(&ht[0..3], &[0xff, 0x09, 0x03]);
        assert_eq!(&ht[3..15], &[0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(ht[15], 1);
        assert_eq!(&ht[16..], &[0; 10]);
        assert_eq!(
            vht,
            [
                0xb2, 0x71, 0x80, 0x33, 0xfa, 0xff, 0, 0, 0xfa, 0xff, 0, 0x20
            ]
        );
        // MT7961 follows mt76_init_sband + mt7921_register_device's
        // non-MT7922 branch: SGI80 is advertised, SGI160 is not.
        assert_ne!(vht[0] & 0x20, 0);
        assert_eq!(vht[0] & 0x40, 0);
        assert!(query.supported_phys.unwrap().contains(&WlanPhyType::He));
    }

    #[test]
    fn actual_softmac_query_and_pinned_regdb_form_supported_subset_v2() {
        let capability = NicCapability {
            element_count: 1,
            mac_address: Some([2, 0, 0, 0, 0, 1]),
            phy: Some(mt7921_core::NicPhyCapability {
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
        };
        let query =
            query_from_capabilities(capability, &mt7921_core::candidate_channels(capability));
        let database = include_bytes!("../../mt7921-core/tests/fixtures/regulatory.db");
        let regulatory = mt7921_core::regulatory_rate_power_snapshot_from_regdb_v20(
            database, 0, *b"00", capability, [7; 32],
        )
        .unwrap();
        let profile =
            crate::client_device::production_association_profile_from_query_and_regulatory(
                &query,
                WlanBand::FiveGhz,
                36,
                &regulatory,
            )
            .unwrap();
        assert_eq!(
            profile.ht_capabilities.unwrap(),
            [
                0xff, 0x09, 3, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0,
            ]
        );
        assert_eq!(
            profile.vht_capabilities.unwrap(),
            [
                0xb2, 0x71, 0x80, 0x33, 0xfa, 0xff, 0, 0, 0xfa, 0xff, 0, 0x20
            ]
        );
        let regulatory = profile.regulatory.unwrap();
        assert_eq!(
            (regulatory.min_tx_power_dbm, regulatory.max_tx_power_dbm),
            (0, 20)
        );
        assert_eq!(
            regulatory
                .supported_channels
                .iter()
                .map(|range| (range.first, range.count))
                .collect::<Vec<_>>(),
            [
                36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136,
                140, 144, 149, 153, 157, 161, 165,
            ]
            .map(|channel| (channel, 1))
        );
        assert_eq!(profile.station, Default::default());
    }

    fn connac2_envelope(mcu_normal: bool, group5: bool) -> (Vec<u8>, Vec<u8>) {
        let mut frame = vec![0x80, 0, 0, 0];
        frame.extend(0u8..32);
        let mut envelope = vec![0; if group5 { 104 } else { 32 }];
        let len = (envelope.len() + frame.len()) as u32;
        let kind = if mcu_normal {
            (7 << 27) | (1 << 16)
        } else {
            2 << 27
        };
        envelope[0..4].copy_from_slice(&(kind | len).to_le_bytes());
        envelope[4..8].copy_from_slice(&((1u32 << 13) | (u32::from(group5) << 15)).to_le_bytes());
        envelope[12..16].copy_from_slice(&(36u32 << 8).to_le_bytes());
        envelope[28..32].copy_from_slice(&0x7878u32.to_le_bytes());
        if group5 {
            envelope[56..60].copy_from_slice(&0x6464u32.to_le_bytes());
        }
        envelope.extend_from_slice(&frame);
        (envelope, frame)
    }

    #[test]
    fn connac2_data_and_mcu_normal_map_once_to_exact_frame_and_rx_info() {
        for (mcu_normal, group5, expected_rssi) in [
            (false, false, -50),
            (true, false, -50),
            (false, true, -60),
            (true, true, -60),
        ] {
            let (envelope, frame) = connac2_envelope(mcu_normal, group5);
            let rx = pinned_client_rx_from_connac2(&envelope, 9u8).unwrap();
            assert_eq!(rx.bytes, frame);
            assert_eq!(rx.provenance, 9);
            assert_eq!(rx.status.primary.number, 36);
            assert_eq!(
                rx.status.primary.band,
                fidl_fuchsia_wlan_ieee80211::WlanBand::FiveGhz
            );
            assert_eq!(rx.status.rssi_dbm, expected_rssi);
            assert_eq!(
                rx.status.bandwidth,
                fidl_fuchsia_wlan_ieee80211::ChannelBandwidth::Cbw20
            );
            assert_eq!(
                rx.status.valid_fields,
                fidl_fuchsia_wlan_softmac::WlanRxInfoValid::RSSI
            );
        }
    }

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
        rate_power_after_commands: Option<usize>,
        events: VecDeque<PassiveMechanicsEvent>,
        confirmed_scan_sequences: Vec<u8>,
        mcu_sequence: Option<u8>,
        rate_power_sequences: Vec<u8>,
    }

    impl SourceExactPassiveMechanics for ScriptedMechanics {
        type Error = ScriptError;

        fn current_mcu_sequence(&self) -> Option<u8> {
            self.mcu_sequence
        }

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
            if self.mcu_sequence.is_some() {
                self.mcu_sequence = encoded.get(39).copied();
            }
            Ok(())
        }

        fn install_rate_tx_power(&mut self, _: NicCapability) -> Result<(), Self::Error> {
            self.rate_power_after_commands = Some(self.commands.len());
            if let Some(mut sequence) = self.mcu_sequence {
                for _ in 0..8 {
                    sequence = sequence % 15 + 1;
                    self.rate_power_sequences.push(sequence);
                }
                self.mcu_sequence = Some(sequence);
            }
            Ok(())
        }

        fn next_event(
            &mut self,
            _deadline_nanos: i64,
        ) -> Result<Option<PassiveMechanicsEvent>, Self::Error> {
            Ok(self.events.pop_front())
        }

        fn confirm_scan_done(&mut self, scan_sequence: u8) -> Result<(), Self::Error> {
            self.confirmed_scan_sequences.push(scan_sequence);
            Ok(())
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
            phy: Some(mt7921_core::NicPhyCapability {
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

    fn channel5(number: u8) -> ChannelNumber {
        ChannelNumber {
            band: WlanBand::FiveGhz,
            number,
        }
    }

    #[test]
    fn absent_secondary80_maps_to_zero_for_contiguous_widths() {
        for (bandwidth, expected) in [
            (ChannelBandwidth::Cbw20, (36, 0, 0)),
            (ChannelBandwidth::Cbw40, (38, 1, 0)),
            (ChannelBandwidth::Cbw80, (42, 2, 0)),
            (ChannelBandwidth::Cbw160, (50, 3, 0)),
        ] {
            let shape = LinuxChannelShape::from_fidl(channel5(36), bandwidth, None).unwrap();
            assert_eq!(
                (shape.center_channel, shape.bandwidth, shape.center_channel2),
                expected
            );
        }
        assert_eq!(
            LinuxChannelShape::from_fidl(channel5(40), ChannelBandwidth::Cbw40Below, None),
            Some(LinuxChannelShape {
                center_channel: 38,
                bandwidth: 1,
                center_channel2: 0,
            })
        );
    }

    #[test]
    fn secondary80_is_required_only_for_valid_80_plus_80() {
        assert_eq!(
            LinuxChannelShape::from_fidl(
                channel5(36),
                ChannelBandwidth::Cbw80P80,
                Some(channel5(106)),
            ),
            Some(LinuxChannelShape {
                center_channel: 42,
                bandwidth: 6,
                center_channel2: 106,
            })
        );
        assert_eq!(
            LinuxChannelShape::from_fidl(channel5(36), ChannelBandwidth::Cbw80P80, None),
            None
        );
        assert_eq!(
            LinuxChannelShape::from_fidl(
                channel5(36),
                ChannelBandwidth::Cbw80P80,
                Some(channel5(0)),
            ),
            None
        );
        assert_eq!(
            LinuxChannelShape::from_fidl(
                channel5(36),
                ChannelBandwidth::Cbw80P80,
                Some(channel5(42)),
            ),
            None
        );
    }

    #[test]
    fn contradictory_or_malformed_channel_shapes_are_rejected() {
        assert_eq!(
            LinuxChannelShape::from_fidl(
                channel5(36),
                ChannelBandwidth::Cbw80,
                Some(channel5(106)),
            ),
            None
        );
        assert_eq!(
            LinuxChannelShape::from_fidl(channel5(36), ChannelBandwidth::Cbw20, Some(channel(0)),),
            None
        );
        assert_eq!(
            LinuxChannelShape::from_fidl(
                channel5(36),
                ChannelBandwidth::Cbw80P80,
                Some(channel(106)),
            ),
            None
        );
        assert_eq!(
            LinuxChannelShape::from_fidl(channel5(165), ChannelBandwidth::Cbw80, None),
            None
        );
        assert_eq!(
            LinuxChannelShape::from_fidl(
                channel5(36),
                ChannelBandwidth::from_primitive_allow_unknown(77),
                None,
            ),
            None
        );
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
    fn live_prefix_and_rate_pages_share_one_wrapping_mcu_sequence_domain() {
        let capability = nic();
        let mechanics = ScriptedMechanics {
            mcu_sequence: Some(14),
            ..Default::default()
        };
        let mut transport = SourceExactPassiveTransport::new(mechanics, capability).unwrap();
        transport
            .set_channel(capability_channels(capability)[0])
            .unwrap();
        let mechanics = transport.into_mechanics();
        let command_sequences = mechanics
            .commands
            .iter()
            .map(|(_, encoded, _)| encoded[39])
            .collect::<Vec<_>>();
        // Loader-owned EEPROM/protection commands are not replayed. The first
        // eight-page SKU batch wraps 14 -> 15 -> 1 before runtime commands;
        // the second batch follows SetRxPath and shares that same sequence.
        assert_eq!(mechanics.prepare_after_commands, Some(0));
        assert_eq!(mechanics.rate_power_after_commands, Some(4));
        assert_eq!(command_sequences, [8, 9, 10, 11, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(
            mechanics.rate_power_sequences,
            [15, 1, 2, 3, 4, 5, 6, 7, 12, 13, 14, 15, 1, 2, 3, 4]
        );
    }

    #[test]
    fn real_fuchsia_adapter_drives_source_exact_passive_closure() {
        let capability = nic();
        let transport = SourceExactPassiveTransport::new(
            ScriptedMechanics {
                mcu_sequence: Some(3),
                ..Default::default()
            },
            capability,
        )
        .unwrap();
        let mut adapter = Mt7921SoftmacAdapter::new(
            transport,
            capability,
            capability_channels(capability),
            vec![channel(1)],
        )
        .unwrap();
        adapter
            .set_channel(set_channel_request(
                channel(1),
                ChannelBandwidth::Cbw20,
                None,
            ))
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
        assert_eq!(adapter.transport.mechanics.prepare_after_commands, Some(0));
        assert_eq!(commands.len(), 12);
        assert!(matches!(commands[0].0, PassiveMcuCommand::KeepFullPower));
        assert!(matches!(commands[1].0, PassiveMcuCommand::MacEnable));
        assert!(matches!(
            commands[2].0,
            PassiveMcuCommand::SetChannelDomain(_)
        ));
        assert!(matches!(commands[3].0, PassiveMcuCommand::SetRxPath { .. }));
        assert_eq!(
            adapter.transport.mechanics.rate_power_after_commands,
            Some(4)
        );
        assert_eq!(
            adapter.transport.mechanics.rate_power_sequences,
            [4, 5, 6, 7, 8, 9, 10, 11, 1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert!(matches!(
            commands[3].0,
            PassiveMcuCommand::SetRxPath {
                channel: CandidateChannel {
                    band: PhysicalBand::Ghz2,
                    number: 1,
                    frequency_mhz: 2412,
                },
                antenna_mask: 3,
            }
        ));
        assert!(matches!(
            commands[4].0,
            PassiveMcuCommand::RadioLedCtrl { value: 1 }
        ));
        assert!(matches!(
            commands[5].0,
            PassiveMcuCommand::RadioLedCtrl { value: 2 }
        ));
        assert!(matches!(commands[6].0, PassiveMcuCommand::AddDevice { .. }));
        assert!(matches!(commands[7].0, PassiveMcuCommand::AddBss));
        assert!(matches!(commands[8].0, PassiveMcuCommand::InitialEdca));
        assert!(matches!(
            commands[9].0,
            PassiveMcuCommand::SetPassiveRxFilter
        ));
        assert!(matches!(
            commands[10].0,
            PassiveMcuCommand::ChannelSwitch { .. }
        ));
        assert!(matches!(
            commands[11].0,
            PassiveMcuCommand::StartScan { .. }
        ));
        assert!(!commands[11].2);
        let scan_request = &commands[11].1[64..];
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
        assert_eq!(adapter.transport.mechanics.confirmed_scan_sequences, [1]);
    }

    #[test]
    fn mismatched_hardware_scan_done_is_never_confirmed() {
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
            .start_passive_scan(request(vec![channel(1)]))
            .unwrap();
        adapter
            .transport
            .mechanics
            .events
            .push_back(PassiveMechanicsEvent::ScanDone(PassiveScanDone {
                scan_sequence: 2,
                completed_channels: 1,
                beacon_scan_count: 1,
                alpha2: *b"00",
            }));
        assert!(matches!(
            adapter.next_scan_event(),
            Err(AdapterError::Transport(
                SourceExactTransportError::ScanIdMismatch {
                    expected: 1,
                    actual: 2
                }
            ))
        ));
        assert!(
            adapter
                .transport
                .mechanics
                .confirmed_scan_sequences
                .is_empty()
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
    fn mandatory_passive_dependencies_fail_before_runtime_commands() {
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
            adapter.set_channel(set_channel_request(
                channel(1),
                ChannelBandwidth::Cbw20,
                Some(channel(0)),
            )),
            Err(AdapterError::Transport(
                SourceExactTransportError::MandatoryDependency(_)
            ))
        ));
        assert_eq!(adapter.transport.mechanics.prepare_after_commands, Some(0));
        assert!(adapter.transport.mechanics.commands.is_empty());
        assert_eq!(adapter.transport.mechanics.rate_power_after_commands, None);
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
            adapter.set_channel(set_channel_request(
                channel(6),
                ChannelBandwidth::Cbw20,
                Some(channel(0)),
            )),
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

    #[test]
    fn fuchsia_regulatory_snapshot_preserves_missing_power_as_incomplete() {
        let capability = NicCapability {
            element_count: 0,
            mac_address: Some([2, 0, 0, 0, 0, 1]),
            phy: Some(mt7921_core::NicPhyCapability {
                ht: true,
                vht: true,
                has_5ghz: false,
                max_bandwidth: 1,
                spatial_streams: 2,
                hardware_path: 1,
                he: true,
            }),
            has_6ghz: Some(false),
            chip_capability: None,
            unknown_elements: 0,
        };
        let candidates = capability_channels(capability);
        let authorized = vec![ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 1,
        }];
        let incomplete = regulatory_rate_power_snapshot_from_fuchsia(
            3,
            *b"00",
            capability,
            &candidates,
            &authorized,
            &[],
            Vec::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            mt7921_core::encode_regulatory_rate_tx_power_commands(capability, &incomplete, 3, 1,),
            Err(RateTxPowerError::InvalidRegulatoryLimit)
        );

        let complete = regulatory_rate_power_snapshot_from_fuchsia(
            3,
            *b"00",
            capability,
            &candidates,
            &authorized,
            &[FuchsiaRegulatoryPower {
                channel: authorized[0],
                max_reg_power_dbm: 17,
            }],
            Vec::new(),
            None,
        )
        .unwrap();
        let commands =
            mt7921_core::encode_regulatory_rate_tx_power_commands(capability, &complete, 3, 1)
                .unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(
            complete
                .channels()
                .iter()
                .filter(|channel| channel.present)
                .count(),
            14
        );
        assert_eq!(
            complete
                .channels()
                .iter()
                .filter(|channel| channel.disabled)
                .count(),
            13
        );
    }
}
