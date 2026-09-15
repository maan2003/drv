// SPDX-License-Identifier: GPL-2.0-only

//! Ath11k client binding for the chip-neutral synchronous SoftMAC contract.

use ath11k_core::{
    AssociationBandwidth, Cipher, ClientRadioControl as _, Device, DeviceState, KeyConfig, KeyKind,
    KeyProtection, Lifecycle as _, ManagementFrame, ModelSubsystems, PeerAssociation,
    RadioControl as _, ScanConfig, ScanId, Subsystems, VdevId, WCN6750, WlanEvent,
    WmmAccessCategory, WmmConfig,
};
use fidl_fuchsia_wlan_common::WlanMacRole;
use fidl_fuchsia_wlan_ieee80211::{
    BssType, ChannelBandwidth, ChannelNumber, WlanBand, WlanPhyType,
};
use fidl_fuchsia_wlan_softmac::WlanSoftmacBandCapability;
use wlan_softmac_class_support::{
    ClientRuntimeDriver, DiscoverySupport, JoinBssRequest, MacSublayerSupport, SecuritySupport,
    SpectrumManagementSupport, WlanAssociationConfig, WlanKeyConfiguration, WlanRxInfo,
    WlanSoftmac, WlanSoftmacBaseCancelScanRequest, WlanSoftmacBaseClearAssociationRequest,
    WlanSoftmacBaseSetChannelRequest, WlanSoftmacBaseStartActiveScanResponse,
    WlanSoftmacBaseStartPassiveScanRequest, WlanSoftmacBaseStartPassiveScanResponse,
    WlanSoftmacBaseUpdateWmmParametersRequest, WlanSoftmacLifecycle, WlanSoftmacQueryResponse,
    WlanSoftmacStartActiveScanRequest, WlanSoftmacUpcalls, WlanTxInfoFlags, WlanTxResult,
};

const SCAN_EVENT_COMPLETED: u32 = 1 << 1;
// Pinned wmi.h: prefix 0xA000 marks a scan initiated by the host.
const HOST_SCAN_ID_START: u32 = 0xa000;
const HOST_SCAN_ID_END: u32 = 0xafff;
const DP_WORK_BUDGET: usize = 64;
const DP_RECEIVE_BUDGET: usize = 1;
const MGMT_TX_PENDING_MAX: u32 = 512;
const ACTIVE_SCAN_CHANNEL_MAX: usize = 256;
const ACTIVE_SCAN_SSID_MAX: usize = 16;
const SSID_BYTE_MAX: usize = 32;
const TWO_GHZ_RATES: &[u8] = &[2, 4, 11, 22, 12, 18, 24, 36, 48, 72, 96, 108];
const FIVE_GHZ_RATES: &[u8] = &[12, 18, 24, 36, 48, 72, 96, 108];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingAssociationSecurity {
    peer: [u8; 6],
    need_ptk_4_way: bool,
    need_gtk_2_way: bool,
    pmf: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Igtk {
    key_id: u16,
    key: [u8; 16],
    receive_ipn: u64,
    transmit_ipn: u64,
}

fn aes_cmac_128(key: &[u8; 16], message: &[u8]) -> [u8; 16] {
    let mut result = [0u8; 16];
    // SAFETY: all pointers refer to live slices of the exact lengths supplied.
    let ok = unsafe {
        bssl_sys::AES_CMAC(
            result.as_mut_ptr(),
            key.as_ptr(),
            key.len(),
            message.as_ptr(),
            message.len(),
        )
    };
    assert_eq!(ok, 1, "AES-CMAC rejected fixed-size BIP inputs");
    result
}

fn robust_management(frame: &[u8]) -> bool {
    let subtype = u16::from_le_bytes([frame[0], frame[1]]) & 0x00f0;
    matches!(subtype, 0xa0 | 0xc0)
        || (subtype == 0xd0 && !matches!(frame.get(24), Some(4) | Some(7) | Some(15)))
}

fn bip_mic(key: &[u8; 16], frame: &[u8]) -> Result<[u8; 8], zx::Status> {
    if frame.len() < 24 {
        return Err(zx::Status::INVALID_ARGS);
    }
    let mut authenticated = Vec::with_capacity(frame.len() - 4);
    let mut fc = u16::from_le_bytes([frame[0], frame[1]]);
    fc &= !0x3800;
    authenticated.extend_from_slice(&fc.to_le_bytes());
    authenticated.extend_from_slice(&frame[4..22]);
    authenticated.extend_from_slice(&frame[24..]);
    Ok(aes_cmac_128(key, &authenticated)[..8].try_into().unwrap())
}

fn protect_group_management(frame: &[u8], igtk: &mut Igtk) -> Result<Vec<u8>, zx::Status> {
    igtk.transmit_ipn = igtk
        .transmit_ipn
        .checked_add(1)
        .ok_or(zx::Status::BAD_STATE)?;
    if igtk.transmit_ipn > 0x0000_ffff_ffff_ffff {
        return Err(zx::Status::BAD_STATE);
    }
    let mut protected = frame.to_vec();
    protected[1] |= 0x40;
    protected.extend_from_slice(&[76, 16]);
    protected.extend_from_slice(&igtk.key_id.to_le_bytes());
    protected.extend_from_slice(&igtk.transmit_ipn.to_le_bytes()[..6]);
    protected.extend_from_slice(&[0; 8]);
    let mic = bip_mic(&igtk.key, &protected)?;
    let offset = protected.len() - 8;
    protected[offset..].copy_from_slice(&mic);
    Ok(protected)
}

fn verify_group_management(frame: &[u8], igtk: &mut Igtk) -> bool {
    if frame.len() < 42 || frame[frame.len() - 18..frame.len() - 16] != [76, 16] {
        return false;
    }
    let mmie = frame.len() - 16;
    if u16::from_le_bytes(frame[mmie..mmie + 2].try_into().unwrap()) != igtk.key_id {
        return false;
    }
    let mut ipn_bytes = [0u8; 8];
    ipn_bytes[..6].copy_from_slice(&frame[mmie + 2..mmie + 8]);
    let ipn = u64::from_le_bytes(ipn_bytes);
    if ipn <= igtk.receive_ipn {
        return false;
    }
    let mut checked = frame.to_vec();
    let received = checked[checked.len() - 8..].to_vec();
    let offset = checked.len() - 8;
    checked[offset..].fill(0);
    let Ok(expected) = bip_mic(&igtk.key, &checked) else {
        return false;
    };
    let equal = received
        .iter()
        .zip(expected)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0;
    if equal {
        igtk.receive_ipn = ipn;
    }
    equal
}

fn association_security(bytes: &[u8]) -> Result<PendingAssociationSecurity, zx::Status> {
    if bytes.len() < 28 {
        return Err(zx::Status::INVALID_ARGS);
    }
    let peer = bytes[4..10].try_into().unwrap();
    let mut rsne = None;
    let mut wpa = false;
    let mut offset = 28;
    while offset < bytes.len() {
        let length = usize::from(*bytes.get(offset + 1).ok_or(zx::Status::INVALID_ARGS)?);
        let body = bytes
            .get(offset + 2..offset + 2 + length)
            .ok_or(zx::Status::INVALID_ARGS)?;
        match bytes[offset] {
            48 => {
                if rsne.is_some() || body.len() < 18 || u16::from_le_bytes([body[0], body[1]]) != 1
                {
                    return Err(zx::Status::INVALID_ARGS);
                }
                let pairwise_count = usize::from(u16::from_le_bytes([body[6], body[7]]));
                let akm_count_offset = 8usize
                    .checked_add(
                        pairwise_count
                            .checked_mul(4)
                            .ok_or(zx::Status::INVALID_ARGS)?,
                    )
                    .ok_or(zx::Status::INVALID_ARGS)?;
                let akm_count_bytes = body
                    .get(akm_count_offset..akm_count_offset + 2)
                    .ok_or(zx::Status::INVALID_ARGS)?;
                let akm_count =
                    usize::from(u16::from_le_bytes(akm_count_bytes.try_into().unwrap()));
                let akms = body
                    .get(akm_count_offset + 2..akm_count_offset + 2 + akm_count * 4)
                    .ok_or(zx::Status::INVALID_ARGS)?;
                let capabilities_offset = akm_count_offset + 2 + akm_count * 4;
                let capabilities = body
                    .get(capabilities_offset..capabilities_offset + 2)
                    .map(|caps| u16::from_le_bytes(caps.try_into().unwrap()))
                    .unwrap_or(0);
                let mut optional =
                    capabilities_offset + usize::from(body.len() >= capabilities_offset + 2) * 2;
                if body.len() >= optional + 2 {
                    let pmkid_count = usize::from(u16::from_le_bytes(
                        body[optional..optional + 2].try_into().unwrap(),
                    ));
                    optional = optional
                        .checked_add(
                            2 + pmkid_count
                                .checked_mul(16)
                                .ok_or(zx::Status::INVALID_ARGS)?,
                        )
                        .ok_or(zx::Status::INVALID_ARGS)?;
                }
                if !matches!(body.len().checked_sub(optional), Some(0) | Some(4)) {
                    return Err(zx::Status::INVALID_ARGS);
                }
                if body.len() == optional + 4
                    && body[optional..optional + 4] != [0x00, 0x0f, 0xac, 6]
                {
                    return Err(zx::Status::NOT_SUPPORTED);
                }
                let sae = akms
                    .chunks_exact(4)
                    .any(|suite| suite[..3] == [0x00, 0x0f, 0xac] && suite[3] == 8);
                let pmf = capabilities & ((1 << 6) | (1 << 7)) != 0;
                if sae && capabilities & (1 << 7) == 0 {
                    return Err(zx::Status::INVALID_ARGS);
                }
                rsne = Some(PendingAssociationSecurity {
                    peer,
                    need_ptk_4_way: true,
                    need_gtk_2_way: false,
                    pmf,
                });
            }
            221 if body.starts_with(&[0x00, 0x50, 0xf2, 1]) => {
                if wpa || body.len() < 22 || body[4..6] != [1, 0] {
                    return Err(zx::Status::INVALID_ARGS);
                }
                let pairwise_count = usize::from(u16::from_le_bytes([body[10], body[11]]));
                let akm_count_offset = 12usize
                    .checked_add(
                        pairwise_count
                            .checked_mul(4)
                            .ok_or(zx::Status::INVALID_ARGS)?,
                    )
                    .ok_or(zx::Status::INVALID_ARGS)?;
                let akm_count = body
                    .get(akm_count_offset..akm_count_offset + 2)
                    .map(|count| u16::from_le_bytes(count.try_into().unwrap()) as usize)
                    .ok_or(zx::Status::INVALID_ARGS)?;
                if body.len() != akm_count_offset + 2 + akm_count * 4 {
                    return Err(zx::Status::INVALID_ARGS);
                }
                wpa = true;
            }
            _ => {}
        }
        offset += 2 + length;
    }
    if rsne.is_some() && wpa {
        return Err(zx::Status::INVALID_ARGS);
    }
    Ok(rsne.unwrap_or(PendingAssociationSecurity {
        peer,
        need_ptk_4_way: wpa,
        need_gtk_2_way: wpa,
        pmf: false,
    }))
}

fn status(error: ath11k_core::CoreError) -> zx::Status {
    match error {
        ath11k_core::CoreError::WrongState => zx::Status::BAD_STATE,
        ath11k_core::CoreError::NoResources => zx::Status::NO_RESOURCES,
        ath11k_core::CoreError::NotFound => zx::Status::NOT_FOUND,
        ath11k_core::CoreError::Qmi(_) => zx::Status::IO,
        ath11k_core::CoreError::DpAllocation { .. } => zx::Status::IO,
        ath11k_core::CoreError::HtcControlSend(_)
        | ath11k_core::CoreError::HtcControlReceive(_)
        | ath11k_core::CoreError::HtcControlTimeout { .. }
        | ath11k_core::CoreError::HttVersionTimeout { .. } => zx::Status::IO,
        ath11k_core::CoreError::Protocol
        | ath11k_core::CoreError::ProtocolAt(_)
        | ath11k_core::CoreError::WmiSend(_)
        | ath11k_core::CoreError::WmiWait(_)
        | ath11k_core::CoreError::DpPeerSetup(_)
        | ath11k_core::CoreError::HttPeerMap { .. }
        | ath11k_core::CoreError::HttPeerMapTimeout { .. } => zx::Status::IO_INVALID,
        ath11k_core::CoreError::DeviceFault | ath11k_core::CoreError::DeviceFaultAt(_) => {
            zx::Status::IO
        }
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

fn regulatory_frequency_channel(frequency: u16) -> Option<ChannelNumber> {
    let channel = frequency_channel(frequency);
    let valid = match channel.band {
        WlanBand::TwoGhz => {
            frequency == 2484
                || ((2412..=2472).contains(&frequency) && (frequency - 2407).is_multiple_of(5))
        }
        WlanBand::FiveGhz => {
            (5180..=5825).contains(&frequency) && (frequency - 5000).is_multiple_of(5)
        }
        _ => false,
    };
    valid.then_some(channel)
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
    associated: bool,
    link_up: bool,
    upcalls: Option<Box<dyn WlanSoftmacUpcalls>>,
    next_scan_id: u32,
    active_scan: Option<u32>,
    drive_calls: u32,
    next_mgmt_buffer_id: u32,
    pending_mgmt_tx: Vec<(u32, [u8; 6])>,
    deferred_mgmt_rx: Option<DeferredManagementRx>,
    deterministic_scan_completion: bool,
    regulatory_domain: Option<ath11k_core::RegulatoryDomain>,
    runtime_trace: Option<fn(&'static str, usize)>,
    pending_association_security: Option<PendingAssociationSecurity>,
    igtk: Option<Igtk>,
}

impl<B: Subsystems> Ath11kClientDevice<B> {
    pub fn new(device: Device<B>, mac: [u8; 6]) -> Self {
        Self {
            device,
            mac,
            vdev: None,
            peer: None,
            associated: false,
            link_up: false,
            upcalls: None,
            next_scan_id: HOST_SCAN_ID_START,
            active_scan: None,
            drive_calls: 0,
            next_mgmt_buffer_id: 0,
            pending_mgmt_tx: Vec::new(),
            deferred_mgmt_rx: None,
            deterministic_scan_completion: false,
            regulatory_domain: None,
            runtime_trace: None,
            pending_association_security: None,
            igtk: None,
        }
    }

    /// Install the regulatory domain that startup programs before vdev creation.
    pub fn with_regulatory_domain(mut self, domain: ath11k_core::RegulatoryDomain) -> Self {
        self.regulatory_domain = Some(domain);
        self
    }

    /// Install the bounded physical-runtime checkpoint sink.
    pub fn with_runtime_trace(mut self, trace: fn(&'static str, usize)) -> Self {
        self.runtime_trace = Some(trace);
        self
    }

    fn trace_runtime(&self, stage: &'static str, value: usize) {
        if let Some(trace) = self.runtime_trace {
            trace(stage, value);
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

    fn deliver_management_rx(&mut self, received: DeferredManagementRx) {
        if self.associated
            && received.frame.len() >= 25
            && received.frame[4] & 1 != 0
            && robust_management(&received.frame)
        {
            let Some(igtk) = self.igtk.as_mut() else {
                return;
            };
            if !verify_group_management(&received.frame, igtk) {
                return;
            }
        }
        let primary = frequency_channel(received.channel_mhz as u16);
        self.upcalls.as_mut().unwrap().recv(
            received.frame,
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
                rssi_dbm: received.rssi.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8,
                snr_dbh: 0,
            },
        );
    }
}

impl Ath11kClientDevice<ModelSubsystems> {
    /// Deterministic backend used by the shared host conformance runner.
    pub fn deterministic(mac: [u8; 6]) -> Self {
        let mut device = Self::new(WCN6750.device(ModelSubsystems::default()), mac);
        device.deterministic_scan_completion = true;
        device.regulatory_domain = Some(ath11k_core::RegulatoryDomain {
            alpha2: *b"00",
            channels: vec![ath11k_core::RegulatoryChannel {
                frequency_mhz: 2437,
                max_power_dbm: 0,
                max_reg_power_dbm: 0,
                max_antenna_gain_dbi: 0,
                passive: false,
                radar: false,
                allow_ht: true,
                allow_vht: true,
                allow_he: true,
            }],
        });
        device
    }
}

impl<B: Subsystems> WlanSoftmacLifecycle for Ath11kClientDevice<B> {
    fn start(&mut self, upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status> {
        if self.upcalls.is_some() || self.device.state() != DeviceState::Allocated {
            return Err(zx::Status::BAD_STATE);
        }
        eprintln!("ath11k_softmac_startup=PROBE_ENTER");
        if let Err(error) = self.device.probe() {
            self.device.abort_startup();
            return Err(status(error));
        }
        eprintln!("ath11k_softmac_startup=PROBE_READY");
        eprintln!("ath11k_softmac_startup=FIRMWARE_ATTACH_ENTER");
        if let Err(error) = self.device.attach_firmware() {
            self.device.abort_startup();
            return Err(status(error));
        }
        eprintln!("ath11k_softmac_startup=FIRMWARE_ATTACHED");
        eprintln!("ath11k_softmac_startup=RADIO_START_ENTER");
        if let Err(error) = self.device.start_radio() {
            self.device.abort_startup();
            return Err(status(error));
        }
        eprintln!("ath11k_softmac_startup=RADIO_READY");
        if let Some(domain) = self.regulatory_domain.clone() {
            eprintln!("ath11k_softmac_startup=REGULATORY_ENTER");
            if let Err(error) = self.device.set_regulatory_domain(domain) {
                self.device.abort_startup();
                return Err(status(error));
            }
            eprintln!("ath11k_softmac_startup=REGULATORY_READY");
        }
        eprintln!("ath11k_softmac_startup=CLIENT_VDEV_ENTER");
        match self.device.create_client_vdev(self.mac) {
            Ok(vdev) => self.vdev = Some(vdev),
            Err(error) => {
                self.device.abort_startup();
                return Err(status(error));
            }
        }
        eprintln!("ath11k_softmac_startup=CLIENT_VDEV_READY");
        self.upcalls = Some(upcalls);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), zx::Status> {
        self.upcalls = None;
        self.active_scan = None;
        self.pending_mgmt_tx.clear();
        self.deferred_mgmt_rx = None;
        self.peer = None;
        self.associated = false;
        self.link_up = false;
        self.pending_association_security = None;
        self.igtk = None;
        self.vdev = None;
        self.device.stop().map_err(status)
    }
}

#[derive(Default)]
struct DpDeliveries {
    rx: Vec<ath11k_dp::tx::HostRxFrame>,
    tx: Vec<ath11k_dp::tx::TxResult>,
}

struct DeferredManagementRx {
    channel_mhz: u32,
    rssi: i32,
    frame: Vec<u8>,
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
        let drive_call = self.drive_calls;
        self.drive_calls = self.drive_calls.saturating_add(1);
        self.trace_runtime("drive_enter", drive_call as usize);
        let trace =
            drive_call < 4 || (self.runtime_trace.is_some() && drive_call.is_power_of_two());
        if trace {
            eprintln!("ath11k_softmac_drive stage=enter call={drive_call}");
        }
        self.ready_vdev()?;
        if trace && self.runtime_trace.is_some() {
            eprintln!(
                "ath11k_dp_ring_progress call={drive_call} rings={:?}",
                self.device.backend_mut().dp_ring_progress()
            );
        }
        if self.deterministic_scan_completion
            && let Some(scan_id) = self.active_scan.take()
        {
            self.upcalls
                .as_mut()
                .unwrap()
                .notify_scan_complete(zx::Status::OK, u64::from(scan_id));
            self.trace_runtime("drive_return", drive_call as usize);
            return Ok(true);
        }

        let deferred_mgmt_rx = self.deferred_mgmt_rx.take();
        let mut rx_slot_consumed = deferred_mgmt_rx.is_some();
        if let Some(received) = deferred_mgmt_rx {
            self.deliver_management_rx(received);
        }

        let mut deliveries = DpDeliveries::default();
        if trace {
            eprintln!("ath11k_softmac_drive stage=dp_enter call={drive_call}");
        }
        self.trace_runtime("dp_enter", drive_call as usize);
        let serviced = self
            .device
            .service_dp_host(
                DP_WORK_BUDGET,
                if rx_slot_consumed {
                    0
                } else {
                    DP_RECEIVE_BUDGET
                },
                &mut deliveries,
            )
            .map_err(status)?;
        self.trace_runtime("dp_complete", drive_call as usize);
        if trace || serviced.rx_descriptors != 0 || serviced.rx_dropped != Default::default() {
            eprintln!(
                "ath11k_softmac_drive stage=dp_complete call={drive_call} tx_delivered={} tx_malformed={} rx_delivered={} rx_descriptors={} rx_dropped={:?}",
                serviced.tx_delivered,
                serviced.tx_malformed,
                serviced.rx_delivered,
                serviced.rx_descriptors,
                serviced.rx_dropped
            );
        }
        let mut progressed = rx_slot_consumed
            || serviced.tx_delivered != 0
            || serviced.tx_malformed != 0
            || serviced.rx_delivered != 0
            || serviced.rx_dropped != Default::default();
        rx_slot_consumed |= !deliveries.rx.is_empty();
        for frame in deliveries.rx {
            if self.runtime_trace.is_some() {
                // MAC/LLC headers only, never EAPOL key material or payload.
                eprintln!(
                    "ath11k_dp_rx len={} info={:?} header={:02x?}",
                    frame.bytes.len(),
                    frame.info,
                    &frame.bytes[..frame.bytes.len().min(32)]
                );
            }
            // `ath11k_dp_rx_h_ppdu`: the low byte is a channel number,
            // not MHz. This adapter currently advertises only 2/5 GHz.
            let number = frame.info.phy_metadata as u8;
            let primary = ChannelNumber {
                band: if number <= 14 {
                    WlanBand::TwoGhz
                } else {
                    WlanBand::FiveGhz
                },
                number,
            };
            self.upcalls.as_mut().unwrap().recv(
                frame.bytes,
                WlanRxInfo {
                    rx_flags: fidl_fuchsia_wlan_softmac::WlanRxInfoFlags::empty(),
                    valid_fields: fidl_fuchsia_wlan_softmac::WlanRxInfoValid::MCS,
                    phy: WlanPhyType::Ofdm,
                    data_rate: 0,
                    primary,
                    bandwidth: ChannelBandwidth::Cbw20,
                    vht_secondary_80_channel: ChannelNumber {
                        band: primary.band,
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

        if trace {
            eprintln!("ath11k_softmac_drive stage=control_enter call={drive_call}");
        }
        self.trace_runtime("control_enter", drive_call as usize);
        let (event, control_progressed) = self
            .device
            .poll_wlan_event(DP_WORK_BUDGET)
            .map_err(status)?;
        self.trace_runtime("control_complete", drive_call as usize);
        if trace {
            eprintln!(
                "ath11k_softmac_drive stage=control_complete call={drive_call} event={} progressed={control_progressed}",
                event.is_some()
            );
        }
        progressed |= control_progressed;
        if let Some(event) = event {
            if self.runtime_trace.is_some()
                && let WlanEvent::Scan {
                    event_type,
                    reason,
                    request_id,
                    scan_id,
                    vdev_id,
                    channel_mhz,
                } = &event
            {
                eprintln!(
                    "ath11k_softmac_scan event_type={event_type} reason={reason} request_id={request_id} scan_id={scan_id} vdev_id={vdev_id} channel_mhz={channel_mhz} active_scan={:?}",
                    self.active_scan
                );
            }
            match event {
                WlanEvent::ManagementReceived {
                    channel_mhz,
                    rssi,
                    frame,
                    ..
                } => {
                    let received = DeferredManagementRx {
                        channel_mhz,
                        rssi,
                        frame,
                    };
                    if rx_slot_consumed {
                        self.deferred_mgmt_rx = Some(received);
                    } else {
                        self.deliver_management_rx(received);
                    }
                }
                WlanEvent::ManagementTxCompleted {
                    buffer_id, status, ..
                } => {
                    let Some(index) = self
                        .pending_mgmt_tx
                        .iter()
                        .position(|(pending, _)| *pending == buffer_id)
                    else {
                        self.trace_runtime("drive_return", drive_call as usize);
                        return Ok(true);
                    };
                    let (_, peer_addr) = self.pending_mgmt_tx.remove(index);
                    // WMI completion carries no rate/retry history. Keep every
                    // entry at zero rather than inventing a TX vector.
                    let entries = [fidl_fuchsia_wlan_softmac::WlanTxResultEntry {
                        tx_vector_idx: 0,
                        attempts: 0,
                    };
                        fidl_fuchsia_wlan_softmac::WLAN_TX_RESULT_MAX_ENTRY as usize];
                    self.upcalls
                        .as_mut()
                        .unwrap()
                        .report_tx_result(WlanTxResult {
                            tx_result_entry: entries,
                            peer_addr,
                            result_code: if status == 0 && peer_addr[0] & 1 == 0 {
                                fidl_fuchsia_wlan_softmac::WlanTxResultCode::Success
                            } else {
                                fidl_fuchsia_wlan_softmac::WlanTxResultCode::Failed
                            },
                        });
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
        self.trace_runtime("drive_return", drive_call as usize);
        Ok(progressed)
    }

    fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status> {
        if !self.associated {
            return Err(zx::Status::BAD_STATE);
        }
        if self.link_up == up {
            return Ok(());
        }
        let peer = self.peer.ok_or(zx::Status::BAD_STATE)?;
        let vdev = self.ready_vdev()?;
        self.device
            .set_peer_authorized(vdev, peer, up)
            .map_err(status)?;
        self.link_up = up;
        Ok(())
    }

    fn reset(&mut self) -> Result<(), zx::Status> {
        self.stop()
    }
}

impl<B: Subsystems> WlanSoftmac for Ath11kClientDevice<B> {
    fn query(&mut self) -> Result<WlanSoftmacQueryResponse, zx::Status> {
        let mut band_caps = Vec::new();
        for band in [WlanBand::TwoGhz, WlanBand::FiveGhz] {
            let primary_channels = self
                .regulatory_domain
                .as_ref()
                .into_iter()
                .flat_map(|domain| &domain.channels)
                .filter_map(|channel| regulatory_frequency_channel(channel.frequency_mhz))
                .filter(|channel| channel.band == band)
                .collect::<Vec<_>>();
            if !primary_channels.is_empty() {
                band_caps.push(WlanSoftmacBandCapability {
                    band: Some(band),
                    basic_rates: Some(
                        match band {
                            WlanBand::TwoGhz => TWO_GHZ_RATES,
                            WlanBand::FiveGhz => FIVE_GHZ_RATES,
                            _ => unreachable!(),
                        }
                        .to_vec(),
                    ),
                    primary_channels: Some(primary_channels),
                    // The adapter currently implements only Cbw20.
                    ht_caps: None,
                    vht_caps: None,
                });
            }
        }
        Ok(WlanSoftmacQueryResponse {
            sta_addr: Some(self.mac),
            factory_addr: Some(self.mac),
            mac_role: Some(WlanMacRole::Client),
            supported_phys: Some(vec![WlanPhyType::Ofdm]),
            hardware_capability: Some(0),
            band_caps: Some(band_caps),
        })
    }
    fn query_discovery_support(&mut self) -> Result<DiscoverySupport, zx::Status> {
        Ok(DiscoverySupport {
            scan_offload: Some(fidl_fuchsia_wlan_softmac::ScanOffloadExtension {
                supported: Some(true),
                scan_cancel_supported: Some(true),
            }),
            ..Default::default()
        })
    }
    fn query_mac_sublayer_support(&mut self) -> Result<MacSublayerSupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_security_support(&mut self) -> Result<SecuritySupport, zx::Status> {
        Ok(SecuritySupport {
            sae: Some(fidl_fuchsia_wlan_common::SaeFeature {
                driver_handler_supported: Some(false),
                sme_handler_supported: Some(true),
                hash_to_element_supported: Some(false),
            }),
            mfp: Some(fidl_fuchsia_wlan_common::MfpFeature {
                supported: Some(true),
            }),
            owe: None,
        })
    }
    fn query_spectrum_management_support(
        &mut self,
    ) -> Result<SpectrumManagementSupport, zx::Status> {
        Ok(Default::default())
    }

    fn set_channel(
        &mut self,
        context: wlan_softmac_class_support::OperationContext,
        request: WlanSoftmacBaseSetChannelRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready((|| {
            context.check(std::time::Instant::now())?;
            let primary = request.primary.ok_or(zx::Status::INVALID_ARGS)?;
            if request.bandwidth != Some(ChannelBandwidth::Cbw20)
                || request.vht_secondary_80_channel.is_none()
            {
                return Err(zx::Status::INVALID_ARGS);
            }
            let vdev = self.ready_vdev()?;
            let frequency = channel_frequency(primary)?;
            let channel = self
                .regulatory_domain
                .as_ref()
                .into_iter()
                .flat_map(|domain| &domain.channels)
                .find(|channel| channel.frequency_mhz == frequency)
                .copied()
                .ok_or(zx::Status::NOT_FOUND)?;
            self.device.start_vdev(vdev, channel).map_err(status)
        })())
    }

    fn join_bss(
        &mut self,
        context: wlan_softmac_class_support::OperationContext,
        request: JoinBssRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready((|| {
            context.check(std::time::Instant::now())?;
            self.pending_association_security = None;
            self.igtk = None;
            let peer = request.bssid.ok_or(zx::Status::INVALID_ARGS)?;
            if request.beacon_period.is_none() {
                return Err(zx::Status::INVALID_ARGS);
            }
            if request.bss_type != Some(BssType::Infrastructure) || request.remote != Some(true) {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            if self.peer.is_some() {
                return Err(zx::Status::BAD_STATE);
            }
            let vdev = self.ready_vdev()?;
            if let Err(error) = self.device.create_peer(vdev, peer) {
                if self.runtime_trace.is_some() {
                    eprintln!("ath11k_softmac_peer_create error={error:?}");
                }
                // A failed completion can leave peer creation ambiguous. Only a
                // terminal firmware stop is representable at the current seam.
                let _ = self.stop();
                return Err(status(error));
            }
            self.peer = Some(peer);
            Ok(())
        })())
    }
    fn install_key(
        &mut self,
        context: wlan_softmac_class_support::OperationContext,
        configuration: WlanKeyConfiguration,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        if let Err(status) = context.check(std::time::Instant::now()) {
            return std::future::ready(Err(status));
        }
        std::future::ready((|| {
            if self.runtime_trace.is_some() {
                eprintln!(
                    "ath11k_key_request type={:?} cipher={:?} index={:?} peer={:?} protection={:?} rsc={:?}",
                    configuration.key_type,
                    configuration.cipher_type,
                    configuration.key_idx,
                    configuration.peer_addr,
                    configuration.protection,
                    configuration.rsc
                );
            }
            let vdev = self.ready_vdev()?;
            let associated_peer = self.peer.ok_or(zx::Status::BAD_STATE)?;
            if !self.associated {
                return Err(zx::Status::BAD_STATE);
            }
            let protection = match configuration.protection.ok_or(zx::Status::INVALID_ARGS)? {
                fidl_fuchsia_wlan_softmac::WlanProtection::None => {
                    return Err(zx::Status::INVALID_ARGS);
                }
                fidl_fuchsia_wlan_softmac::WlanProtection::RxTx => KeyProtection::RxTx,
                fidl_fuchsia_wlan_softmac::WlanProtection::Rx => KeyProtection::Rx,
                fidl_fuchsia_wlan_softmac::WlanProtection::Tx => KeyProtection::Tx,
            };
            if configuration.cipher_oui != Some([0x00, 0x0f, 0xac]) {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let (kind, peer) = match configuration.key_type.ok_or(zx::Status::INVALID_ARGS)? {
                fidl_fuchsia_wlan_ieee80211::KeyType::Pairwise => {
                    if configuration.peer_addr != Some(associated_peer) {
                        return Err(zx::Status::INVALID_ARGS);
                    }
                    (KeyKind::Pairwise, associated_peer)
                }
                fidl_fuchsia_wlan_ieee80211::KeyType::Group => {
                    if configuration.peer_addr != Some([0xff; 6]) {
                        return Err(zx::Status::INVALID_ARGS);
                    }
                    // Linux resolves a station GTK's broadcast host identity to
                    // the associated BSSID before WMI and peer-state publication.
                    (KeyKind::Group, associated_peer)
                }
                fidl_fuchsia_wlan_ieee80211::KeyType::Igtk => {
                    if configuration.peer_addr != Some([0xff; 6])
                        || configuration.cipher_type != Some(6)
                        || !matches!(configuration.key_idx, Some(4) | Some(5))
                        || protection != KeyProtection::RxTx
                    {
                        return Err(zx::Status::INVALID_ARGS);
                    }
                    let key: [u8; 16] = configuration
                        .key
                        .ok_or(zx::Status::INVALID_ARGS)?
                        .try_into()
                        .map_err(|_| zx::Status::INVALID_ARGS)?;
                    // SME packs the six wire-order IPN octets into the low
                    // six bytes of a big-endian u64. BIP compares a little-endian
                    // 48-bit packet number.
                    let rsc = configuration
                        .rsc
                        .ok_or(zx::Status::INVALID_ARGS)?
                        .to_be_bytes();
                    if rsc[..2] != [0, 0] {
                        return Err(zx::Status::INVALID_ARGS);
                    }
                    let mut ipn = [0; 8];
                    ipn[..6].copy_from_slice(&rsc[2..]);
                    let receive_ipn = u64::from_le_bytes(ipn);
                    self.igtk = Some(Igtk {
                        key_id: u16::from(configuration.key_idx.unwrap()),
                        key,
                        receive_ipn,
                        transmit_ipn: 0,
                    });
                    return Ok(());
                }
                _ => return Err(zx::Status::NOT_SUPPORTED),
            };
            let cipher = match configuration.cipher_type.ok_or(zx::Status::INVALID_ARGS)? {
                2 => Cipher::Tkip,
                4 => Cipher::Ccmp128,
                8 => Cipher::Gcmp128,
                9 => Cipher::Gcmp256,
                10 => Cipher::Ccmp256,
                _ => return Err(zx::Status::NOT_SUPPORTED),
            };
            let rsc = configuration.rsc.ok_or(zx::Status::INVALID_ARGS)?;
            // EAPOL's parser exposes the RSC octets as a big-endian u64;
            // CCMP/GCMP's wire PN is little-endian. Preserve the packet number,
            // rather than rejecting a valid nonzero GTK RSC as wider than 48 bits.
            let receive_sequence_counter = if kind == KeyKind::Group {
                u64::from_le_bytes(rsc.to_be_bytes())
            } else {
                rsc
            };
            if receive_sequence_counter > 0x0000_ffff_ffff_ffff {
                return Err(zx::Status::INVALID_ARGS);
            }
            let key = KeyConfig {
                vdev,
                peer,
                index: configuration.key_idx.ok_or(zx::Status::INVALID_ARGS)?,
                cipher,
                kind,
                protection,
                receive_sequence_counter,
                bytes: configuration.key.ok_or(zx::Status::INVALID_ARGS)?,
            };
            if let Err(error) = self.device.install_key(key) {
                if self.runtime_trace.is_some() {
                    // Core errors can own the key-install operation: never Debug
                    // the whole error, since that would include key bytes.
                    let phase = match &error {
                        ath11k_core::CoreError::ProtocolAt(operation)
                        | ath11k_core::CoreError::DeviceFaultAt(operation) => {
                            Some(operation.target())
                        }
                        _ => None,
                    };
                    eprintln!(
                        "ath11k_key_install_failed kind={kind:?} status={:?} phase={phase:?}",
                        status(error.clone())
                    );
                }
                // WMI completion may have succeeded before a DP publication
                // failed. Only terminal device teardown makes that state safe.
                let _ = self.stop();
                return Err(status(error));
            }
            Ok(())
        })())
    }
    fn notify_association_complete(
        &mut self,
        context: wlan_softmac_class_support::OperationContext,
        configuration: WlanAssociationConfig,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        if let Err(status) = context.check(std::time::Instant::now()) {
            return std::future::ready(Err(status));
        }
        std::future::ready((|| {
            if self.associated {
                return Err(zx::Status::BAD_STATE);
            }
            let peer = configuration.bssid.ok_or(zx::Status::INVALID_ARGS)?;
            if self.peer != Some(peer) {
                return Err(zx::Status::BAD_STATE);
            }
            let aid = configuration.aid.ok_or(zx::Status::INVALID_ARGS)?;
            if !(1..=2007).contains(&aid) {
                return Err(zx::Status::INVALID_ARGS);
            }
            let listen_interval = configuration
                .listen_interval
                .ok_or(zx::Status::INVALID_ARGS)?;
            let primary = configuration.primary.ok_or(zx::Status::INVALID_ARGS)?;
            let qos = configuration.qos.ok_or(zx::Status::INVALID_ARGS)?;
            let capability_info = configuration
                .capability_info
                .ok_or(zx::Status::INVALID_ARGS)?;
            let evidence = self.pending_association_security.take();
            // MLME's negotiated CapabilityInfo clears Privacy. The transmitted
            // RSNE is authoritative security provenance, not that summary bit.
            let security = if evidence.is_some_and(|security| security.need_ptk_4_way)
                || capability_info & 0x0010 != 0
            {
                evidence
                    .filter(|security| security.peer == peer && security.need_ptk_4_way)
                    .ok_or(zx::Status::BAD_STATE)?
            } else {
                PendingAssociationSecurity {
                    peer,
                    need_ptk_4_way: false,
                    need_gtk_2_way: false,
                    pmf: false,
                }
            };
            if configuration.bandwidth != Some(ChannelBandwidth::Cbw20) {
                // The current vdev-start seam configures only 20 MHz.
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let secondary = configuration
                .vht_secondary_80_channel
                .ok_or(zx::Status::INVALID_ARGS)?;
            if secondary.band != primary.band || secondary.number != 0 {
                return Err(zx::Status::INVALID_ARGS);
            }
            if configuration.ht_cap.is_some() != configuration.ht_op.is_some()
                || configuration.vht_cap.is_some() != configuration.vht_op.is_some()
                || (configuration.vht_cap.is_some() && configuration.ht_cap.is_none())
                || (!qos && (configuration.ht_cap.is_some() || configuration.vht_cap.is_some()))
            {
                return Err(zx::Status::INVALID_ARGS);
            }
            let rates = configuration.rates.ok_or(zx::Status::INVALID_ARGS)?;
            if rates.is_empty() {
                return Err(zx::Status::INVALID_ARGS);
            }
            let allowed: &[u8] = match primary.band {
                WlanBand::TwoGhz => TWO_GHZ_RATES,
                WlanBand::FiveGhz => FIVE_GHZ_RATES,
                _ => return Err(zx::Status::NOT_SUPPORTED),
            };
            if rates.iter().any(|rate| !allowed.contains(&(rate & 0x7f))) {
                return Err(zx::Status::INVALID_ARGS);
            }
            let legacy_rates = allowed
                .iter()
                .copied()
                .filter(|allowed| rates.iter().any(|rate| rate & 0x7f == *allowed))
                .collect();
            let wmm = configuration
                .wmm_params
                .map(|wmm| {
                    // This is the AP's U-APSD capability, not a requirement to
                    // enable it for our station. Our WMM setup keeps U-APSD off.
                    if !qos {
                        return Err(zx::Status::INVALID_ARGS);
                    }
                    macro_rules! convert {
                        ($ac:expr) => {{
                            let ac = $ac;
                            if ac.ecw_min > 15
                                || ac.ecw_max > 15
                                || ac.ecw_min > ac.ecw_max
                                || ac.aifsn > 15
                            {
                                return Err(zx::Status::INVALID_ARGS);
                            }
                            WmmAccessCategory {
                                ecw_min: ac.ecw_min,
                                ecw_max: ac.ecw_max,
                                aifsn: ac.aifsn,
                                txop_limit: ac.txop_limit,
                                admission_control_mandatory: ac.acm,
                            }
                        }};
                    }
                    Ok(WmmConfig {
                        access_categories: [
                            convert!(wmm.ac_be_params),
                            convert!(wmm.ac_bk_params),
                            convert!(wmm.ac_vi_params),
                            convert!(wmm.ac_vo_params),
                        ],
                    })
                })
                .transpose()?;
            let vdev = self.ready_vdev()?;
            let association = PeerAssociation {
                vdev,
                peer,
                aid,
                listen_interval,
                primary_mhz: channel_frequency(primary)?,
                bandwidth: AssociationBandwidth::Bw20,
                capability_info,
                legacy_rates,
                qos,
                ht_capabilities: configuration.ht_cap.map(|cap| cap.bytes),
                vht_capabilities: configuration.vht_cap.map(|cap| cap.bytes),
                wmm,
                need_ptk_4_way: security.need_ptk_4_way,
                need_gtk_2_way: security.need_gtk_2_way,
                pmf: security.pmf,
            };
            if let Err(error) = self.device.associate_peer(association) {
                if self.runtime_trace.is_some() {
                    eprintln!("ath11k_softmac_associate error={error:?}");
                }
                self.peer = None;
                if self.device.delete_peer(vdev, peer).is_err() {
                    let _ = self.stop();
                }
                return Err(status(error));
            }
            if let Err(error) = self.device.up_vdev(vdev, peer, aid) {
                self.peer = None;
                let down = self.device.down_vdev(vdev);
                let deleted = self.device.delete_peer(vdev, peer);
                if down.is_err() || deleted.is_err() {
                    let _ = self.stop();
                }
                return Err(status(error));
            }
            self.associated = true;
            // A protected peer remains unauthorized until the SME completes key
            // installation and opens the controlled port. Open associations are
            // emitted with ath11k's source-derived AUTH peer flag.
            self.link_up = !security.need_ptk_4_way;
            Ok(())
        })())
    }
    fn clear_association(
        &mut self,
        context: wlan_softmac_class_support::OperationContext,
        request: WlanSoftmacBaseClearAssociationRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        if let Err(status) = context.check(std::time::Instant::now()) {
            return std::future::ready(Err(status));
        }
        std::future::ready((|| {
            self.pending_association_security = None;
            self.igtk = None;
            let peer = self.peer.ok_or(zx::Status::BAD_STATE)?;
            if request.peer_addr != Some(peer) {
                return Err(zx::Status::INVALID_ARGS);
            }
            let vdev = self.ready_vdev()?;
            let was_associated = self.associated;
            let was_link_up = self.link_up;
            // Revoke adapter-side authority before the first fallible operation.
            self.peer = None;
            self.associated = false;
            self.link_up = false;
            let mut first_error = None;
            if was_link_up && let Err(error) = self.device.set_peer_authorized(vdev, peer, false) {
                first_error = Some(error);
            }
            if was_associated
                && let Err(error) = self.device.down_vdev(vdev)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
            if let Err(error) = self.device.delete_peer(vdev, peer)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
            if let Some(error) = first_error {
                let _ = self.stop();
                return Err(status(error));
            }
            Ok(())
        })())
    }

    fn start_passive_scan(
        &mut self,
        context: wlan_softmac_class_support::OperationContext,
        request: WlanSoftmacBaseStartPassiveScanRequest,
    ) -> impl std::future::Future<
        Output = Result<WlanSoftmacBaseStartPassiveScanResponse, zx::Status>,
    > + 'static {
        if let Err(status) = context.check(std::time::Instant::now()) {
            return std::future::ready(Err(status));
        }
        std::future::ready((|| {
            eprintln!("ath11k_softmac_scan=PASSIVE_ENTRY");
            self.trace_runtime("passive_scan_enter", 0);
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
            eprintln!(
                "ath11k_softmac_scan=PASSIVE_CHANNELS_READY count={}",
                channels_mhz.len()
            );
            let scan_id = self.next_scan_id;
            self.next_scan_id = self
                .next_scan_id
                .checked_add(1)
                .filter(|next| *next <= HOST_SCAN_ID_END + 1)
                .ok_or(zx::Status::NO_RESOURCES)?;
            eprintln!("ath11k_softmac_scan=PASSIVE_START_ENTER");
            self.trace_runtime("passive_scan_send_enter", scan_id as usize);
            self.device
                .start_scan(ScanConfig {
                    vdev: self.ready_vdev()?,
                    id: ScanId(scan_id),
                    active: false,
                    channels_mhz,
                    ssids: Vec::new(),
                })
                .map_err(status)?;
            self.trace_runtime("passive_scan_send_complete", scan_id as usize);
            eprintln!("ath11k_softmac_scan=PASSIVE_START_READY");
            self.active_scan = Some(scan_id);
            Ok(WlanSoftmacBaseStartPassiveScanResponse {
                scan_id: Some(u64::from(scan_id)),
            })
        })())
    }
    fn start_active_scan(
        &mut self,
        context: wlan_softmac_class_support::OperationContext,
        request: WlanSoftmacStartActiveScanRequest,
    ) -> impl std::future::Future<
        Output = Result<WlanSoftmacBaseStartActiveScanResponse, zx::Status>,
    > + 'static {
        if let Err(status) = context.check(std::time::Instant::now()) {
            return std::future::ready(Err(status));
        }
        std::future::ready((|| {
            if self.active_scan.is_some() {
                return Err(zx::Status::BAD_STATE);
            }
            let channels = request.channels.ok_or(zx::Status::INVALID_ARGS)?;
            let ssids = request.ssids.ok_or(zx::Status::INVALID_ARGS)?;
            if channels.is_empty()
                || channels.len() > ACTIVE_SCAN_CHANNEL_MAX
                || ssids.is_empty()
                || ssids.len() > ACTIVE_SCAN_SSID_MAX
            {
                return Err(zx::Status::INVALID_ARGS);
            }
            let installed_channels = &self
                .regulatory_domain
                .as_ref()
                .ok_or(zx::Status::BAD_STATE)?
                .channels;
            let mut channels_mhz = Vec::with_capacity(channels.len());
            for channel in channels {
                let frequency = channel_frequency(channel)?;
                let Some(installed) = installed_channels
                    .iter()
                    .find(|installed| installed.frequency_mhz == frequency)
                else {
                    return Err(zx::Status::INVALID_ARGS);
                };
                if installed.passive || channels_mhz.contains(&frequency) {
                    return Err(zx::Status::INVALID_ARGS);
                }
                channels_mhz.push(frequency);
            }
            let ssids = ssids
                .into_iter()
                .map(|ssid| {
                    let len = usize::from(ssid.len);
                    if len == 0 || len > SSID_BYTE_MAX {
                        return Err(zx::Status::INVALID_ARGS);
                    }
                    Ok(ssid.data[..len].to_vec())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let scan_id = self.next_scan_id;
            self.next_scan_id = self
                .next_scan_id
                .checked_add(1)
                .filter(|next| *next <= HOST_SCAN_ID_END + 1)
                .ok_or(zx::Status::NO_RESOURCES)?;
            self.device
                .start_scan(ScanConfig {
                    vdev: self.ready_vdev()?,
                    id: ScanId(scan_id),
                    active: true,
                    channels_mhz,
                    ssids,
                })
                .map_err(status)?;
            self.active_scan = Some(scan_id);
            Ok(WlanSoftmacBaseStartActiveScanResponse {
                scan_id: Some(u64::from(scan_id)),
            })
        })())
    }
    fn cancel_scan(
        &mut self,
        request: WlanSoftmacBaseCancelScanRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready((|| {
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
        })())
    }
    fn update_wmm_parameters(
        &mut self,
        _request: WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn queue_tx(
        &mut self,
        context: wlan_softmac_class_support::OperationContext,
        bytes: &[u8],
        flags: WlanTxInfoFlags,
    ) -> Result<(), zx::Status> {
        context.check(std::time::Instant::now())?;
        if bytes.len() < 24 {
            return Err(zx::Status::INVALID_ARGS);
        }
        let frame_control = u16::from_le_bytes([bytes[0], bytes[1]]);
        if frame_control & 0x0003 != 0 {
            return Err(zx::Status::INVALID_ARGS);
        }
        if frame_control & 0x000c == 0x0008 {
            let peer = self.peer.ok_or(zx::Status::BAD_STATE)?;
            if !self.associated || bytes[4..10] != peer || bytes[10..16] != self.mac {
                return Err(zx::Status::BAD_STATE);
            }
            // STA infrastructure frames go to the AP. Do not admit WDS/TDLS
            // or HT-control layouts that this client does not advertise.
            if frame_control & 0x8300 != 0x0100 {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let qos = frame_control & 0x0080 != 0;
            let header_len = if qos { 26 } else { 24 };
            let eapol = bytes.get(header_len..header_len + 8)
                == Some(&[0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
            if !self.link_up && !eapol {
                return Err(zx::Status::BAD_STATE);
            }
            let result = self
                .device
                .transmit_data(
                    self.ready_vdev()?,
                    peer,
                    bytes,
                    ath11k_dp::tx::HostTxFlags {
                        protected: flags.contains(WlanTxInfoFlags::PROTECTED)
                            || frame_control & 0x4000 != 0,
                        favor_reliability: flags.contains(WlanTxInfoFlags::FAVOR_RELIABILITY),
                        qos,
                    },
                )
                .map_err(status);
            if self.runtime_trace.is_some() {
                eprintln!(
                    "ath11k_dp_tx len={} eapol={eapol} result={result:?}",
                    bytes.len()
                );
            }
            return result;
        }
        if frame_control & 0x000c != 0 {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        let protected_requested =
            flags.contains(WlanTxInfoFlags::PROTECTED) || frame_control & 0x4000 != 0;
        if protected_requested && !robust_management(bytes) {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        let frame = if protected_requested && bytes[4] & 1 != 0 && robust_management(bytes) {
            protect_group_management(bytes, self.igtk.as_mut().ok_or(zx::Status::BAD_STATE)?)?
        } else {
            bytes.to_vec()
        };
        let association_security = if frame_control & 0x00fc == 0 {
            let security = association_security(bytes)?;
            if self.peer != Some(security.peer) {
                return Err(zx::Status::BAD_STATE);
            }
            if self
                .pending_association_security
                .is_some_and(|pending| pending != security)
            {
                return Err(zx::Status::BAD_STATE);
            }
            Some(security)
        } else {
            None
        };
        if self.pending_mgmt_tx.len() >= MGMT_TX_PENDING_MAX as usize {
            return Err(zx::Status::NO_RESOURCES);
        }
        let buffer_id = (0..MGMT_TX_PENDING_MAX)
            .map(|offset| (self.next_mgmt_buffer_id + offset) % MGMT_TX_PENDING_MAX)
            .find(|candidate| {
                !self
                    .pending_mgmt_tx
                    .iter()
                    .any(|(pending, _)| pending == candidate)
            })
            .ok_or(zx::Status::NO_RESOURCES)?;
        let peer_addr = bytes[4..10].try_into().unwrap();
        self.device
            .transmit_management(ManagementFrame {
                vdev: self.ready_vdev()?,
                buffer_id,
                bytes: frame,
            })
            .map_err(status)?;
        self.pending_mgmt_tx.push((buffer_id, peer_addr));
        if let Some(security) = association_security {
            self.pending_association_security = Some(security);
        }
        self.next_mgmt_buffer_id = (buffer_id + 1) % MGMT_TX_PENDING_MAX;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    fn operation_context() -> wlan_softmac_class_support::OperationContext {
        wlan_softmac_class_support::conformance::operation_context(
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        )
        .0
    }

    use super::*;
    use ath11k_core::Operation;
    use std::sync::{Arc, Mutex};
    use wlan_softmac_class_support::conformance::{
        expected_client_conformance, run_client_conformance,
    };

    fn scan_context() -> wlan_softmac_class_support::OperationContext {
        wlan_softmac_class_support::conformance::operation_context(
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        )
        .0
    }

    const CLIENT: [u8; 6] = [2, 0, 0, 0, 0, 1];

    #[test]
    fn advertises_implemented_scan_offload_and_cancel() {
        let mut device = Ath11kClientDevice::deterministic(CLIENT);
        let scan = device
            .query_discovery_support()
            .unwrap()
            .scan_offload
            .unwrap();
        assert_eq!(scan.supported, Some(true));
        assert_eq!(scan.scan_cancel_supported, Some(true));
    }

    struct NoopUpcalls;
    impl WlanSoftmacUpcalls for NoopUpcalls {
        fn recv(&mut self, _: Vec<u8>, _: WlanRxInfo) {}
        fn report_tx_result(&mut self, _: WlanTxResult) {}
        fn notify_scan_complete(&mut self, _: zx::Status, _: u64) {}
    }

    #[derive(Default)]
    struct RecordedUpcalls {
        received: Vec<Vec<u8>>,
        received_channels: Vec<ChannelNumber>,
        tx: Vec<([u8; 6], fidl_fuchsia_wlan_softmac::WlanTxResultCode)>,
        scans: Vec<(zx::Status, u64)>,
    }

    struct Recorder(Arc<Mutex<RecordedUpcalls>>);
    impl WlanSoftmacUpcalls for Recorder {
        fn recv(&mut self, bytes: Vec<u8>, info: WlanRxInfo) {
            let mut records = self.0.lock().unwrap();
            records.received.push(bytes);
            records.received_channels.push(info.primary);
        }
        fn report_tx_result(&mut self, result: WlanTxResult) {
            self.0
                .lock()
                .unwrap()
                .tx
                .push((result.peer_addr, result.result_code));
        }
        fn notify_scan_complete(&mut self, status: zx::Status, scan_id: u64) {
            self.0.lock().unwrap().scans.push((status, scan_id));
        }
    }

    struct SimultaneousRxSubsystems {
        model: ModelSubsystems,
        dp_rx: Option<ath11k_dp::tx::HostRxFrame>,
        receive_budgets: Arc<Mutex<Vec<usize>>>,
    }

    impl Subsystems for SimultaneousRxSubsystems {
        fn execute(&mut self, operation: Operation) -> Result<(), ath11k_core::CoreError> {
            self.model.execute(operation)
        }

        fn wait_for_firmware_ready(
            &mut self,
        ) -> Result<ath11k_qmi::FirmwareReady, ath11k_core::CoreError> {
            self.model.wait_for_firmware_ready()
        }

        fn next_wlan_event(&mut self) -> Result<Option<WlanEvent>, ath11k_core::CoreError> {
            self.model.next_wlan_event()
        }

        fn client_nss(&self) -> Result<u8, ath11k_core::CoreError> {
            self.model.client_nss()
        }

        fn service_dp_host<H: ath11k_dp::tx::DpHost>(
            &mut self,
            _work_budget: usize,
            receive_budget: usize,
            host: &mut H,
        ) -> Result<ath11k_dp::tx::HostServiceResult, ath11k_core::CoreError> {
            self.receive_budgets.lock().unwrap().push(receive_budget);
            let delivered = if receive_budget != 0 {
                self.dp_rx.take().map(|frame| host.receive(frame)).is_some()
            } else {
                false
            };
            Ok(ath11k_dp::tx::HostServiceResult {
                rx_descriptors: 0,
                tx_delivered: 0,
                tx_malformed: 0,
                rx_delivered: usize::from(delivered),
                rx_dropped: Default::default(),
            })
        }
    }

    fn failed_start(operation: Operation) -> Device<ModelSubsystems> {
        let mut backend = ModelSubsystems::default();
        backend.fail_once(operation);
        let mut adapter = Ath11kClientDevice::new(WCN6750.device(backend), CLIENT);
        assert!(adapter.start(Box::new(NoopUpcalls)).is_err());
        adapter.into_device()
    }

    #[test]
    fn query_converts_to_mlme_device_info_from_installed_channels() {
        let channel = |frequency_mhz| ath11k_core::RegulatoryChannel {
            frequency_mhz,
            max_power_dbm: 20,
            max_reg_power_dbm: 20,
            max_antenna_gain_dbi: 0,
            passive: false,
            radar: false,
            allow_ht: true,
            allow_vht: true,
            allow_he: true,
        };
        let mut adapter =
            Ath11kClientDevice::new(WCN6750.device(ModelSubsystems::default()), CLIENT)
                .with_regulatory_domain(ath11k_core::RegulatoryDomain {
                    alpha2: *b"US",
                    channels: vec![
                        channel(2400),
                        channel(2437),
                        channel(5955),
                        channel(5180),
                        channel(2462),
                    ],
                });

        let query = adapter.query().unwrap();
        assert_eq!(query.sta_addr, Some(CLIENT));
        assert_eq!(query.factory_addr, Some(CLIENT));
        assert_eq!(query.mac_role, Some(WlanMacRole::Client));
        assert_eq!(query.supported_phys, Some(vec![WlanPhyType::Ofdm]));
        assert_eq!(query.hardware_capability, Some(0));
        assert_eq!(
            query.band_caps,
            Some(vec![
                WlanSoftmacBandCapability {
                    band: Some(WlanBand::TwoGhz),
                    basic_rates: Some(TWO_GHZ_RATES.to_vec()),
                    primary_channels: Some(vec![
                        ChannelNumber {
                            band: WlanBand::TwoGhz,
                            number: 6
                        },
                        ChannelNumber {
                            band: WlanBand::TwoGhz,
                            number: 11
                        },
                    ]),
                    ht_caps: None,
                    vht_caps: None,
                },
                WlanSoftmacBandCapability {
                    band: Some(WlanBand::FiveGhz),
                    basic_rates: Some(FIVE_GHZ_RATES.to_vec()),
                    primary_channels: Some(vec![ChannelNumber {
                        band: WlanBand::FiveGhz,
                        number: 36,
                    }]),
                    ht_caps: None,
                    vht_caps: None,
                },
            ])
        );

        let info = wlan_mlme::mlme_device_info_from_softmac(query).unwrap();
        assert_eq!(info.sta_addr, CLIENT);
        assert_eq!(info.factory_addr, CLIENT);
        assert_eq!(info.role, WlanMacRole::Client);
        assert_eq!(info.softmac_hardware_capability, 0);
        assert_eq!(info.bands.len(), 2);
        assert_eq!(info.bands[0].basic_rates, TWO_GHZ_RATES);
        assert_eq!(info.bands[1].basic_rates, FIVE_GHZ_RATES);
        assert_eq!(info.bands[0].ht_cap, None);
        assert_eq!(info.bands[0].vht_cap, None);
        assert_eq!(info.bands[1].ht_cap, None);
        assert_eq!(info.bands[1].vht_cap, None);
        let security = adapter.query_security_support().unwrap();
        assert_eq!(security.sae.unwrap().sme_handler_supported, Some(true));
        assert_eq!(security.mfp.unwrap().supported, Some(true));
        assert_eq!(
            adapter.query_spectrum_management_support().unwrap(),
            Default::default()
        );
    }

    #[test]
    fn startup_programs_regulatory_domain_before_vdev_creation() {
        let mut adapter = Ath11kClientDevice::deterministic(CLIENT);
        adapter.start(Box::new(NoopUpcalls)).unwrap();
        let operations = adapter.device.backend().operations();
        let radio = operations
            .iter()
            .position(|operation| operation == &Operation::RadioStart)
            .unwrap();
        let country = operations
            .iter()
            .position(|operation| operation == &Operation::WmiSetCurrentCountry { alpha2: *b"00" })
            .unwrap();
        let channels = operations
            .iter()
            .position(|operation| matches!(operation, Operation::WmiScanChannelList { .. }))
            .unwrap();
        let vdev = operations
            .iter()
            .position(|operation| matches!(operation, Operation::WmiVdevCreate { .. }))
            .unwrap();
        assert!(radio < country && country < channels && channels < vdev);
    }

    #[test]
    fn regulatory_programming_failure_aborts_startup_before_vdev_creation() {
        let mut backend = ModelSubsystems::default();
        backend.fail_once(Operation::WaitRegulatoryUpdate {
            pdev: ath11k_core::PdevId(0),
        });
        let mut adapter = Ath11kClientDevice::new(WCN6750.device(backend), CLIENT)
            .with_regulatory_domain(ath11k_core::RegulatoryDomain {
                alpha2: *b"00",
                channels: vec![ath11k_core::RegulatoryChannel {
                    frequency_mhz: 2437,
                    max_power_dbm: 20,
                    max_reg_power_dbm: 20,
                    max_antenna_gain_dbi: 0,
                    passive: false,
                    radar: false,
                    allow_ht: true,
                    allow_vht: true,
                    allow_he: true,
                }],
            });

        assert_eq!(adapter.start(Box::new(NoopUpcalls)), Err(zx::Status::IO));
        assert_eq!(adapter.device.state(), DeviceState::Stopped);
        assert!(
            !adapter
                .device
                .backend()
                .operations()
                .iter()
                .any(|operation| matches!(operation, Operation::WmiVdevCreate { .. }))
        );
    }

    const PEER: [u8; 6] = [2, 0, 0, 0, 0, 2];

    fn join_request() -> JoinBssRequest {
        JoinBssRequest {
            bssid: Some(PEER),
            bss_type: Some(BssType::Infrastructure),
            remote: Some(true),
            beacon_period: Some(100),
        }
    }

    fn ready_adapter() -> Ath11kClientDevice<ModelSubsystems> {
        let mut adapter = Ath11kClientDevice::deterministic(CLIENT);
        adapter.start(Box::new(NoopUpcalls)).unwrap();
        futures::executor::block_on(
            adapter.set_channel(
                wlan_softmac_class_support::conformance::operation_context(
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .0,
                WlanSoftmacBaseSetChannelRequest {
                    primary: Some(ChannelNumber {
                        band: WlanBand::TwoGhz,
                        number: 6,
                    }),
                    bandwidth: Some(ChannelBandwidth::Cbw20),
                    vht_secondary_80_channel: Some(ChannelNumber {
                        band: WlanBand::TwoGhz,
                        number: 0,
                    }),
                },
            ),
        )
        .unwrap();
        adapter.device.backend_mut().clear();
        adapter
    }

    fn active_scan_request(ssids: &[&[u8]]) -> WlanSoftmacStartActiveScanRequest {
        WlanSoftmacStartActiveScanRequest {
            channels: Some(vec![ChannelNumber {
                band: WlanBand::TwoGhz,
                number: 6,
            }]),
            ssids: Some(
                ssids
                    .iter()
                    .map(|bytes| {
                        let mut data = [0; SSID_BYTE_MAX];
                        data[..bytes.len()].copy_from_slice(bytes);
                        fidl_fuchsia_wlan_ieee80211::CSsid {
                            len: bytes.len() as u8,
                            data,
                        }
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn active_scan_maps_exact_channels_ssids_and_scan_id() {
        let mut adapter = Ath11kClientDevice::deterministic(CLIENT);
        adapter.start(Box::new(NoopUpcalls)).unwrap();
        adapter.device.backend_mut().clear();
        let vdev = adapter.vdev.unwrap();

        let response = futures::executor::block_on(
            adapter.start_active_scan(scan_context(), active_scan_request(&[b"redwood", b"lab"])),
        )
        .unwrap();

        assert_eq!(response.scan_id, Some(u64::from(HOST_SCAN_ID_START)));
        assert_eq!(adapter.active_scan, Some(HOST_SCAN_ID_START));
        assert_eq!(
            adapter.device.backend().operations(),
            &[Operation::WmiScanStart(ScanConfig {
                vdev,
                id: ScanId(HOST_SCAN_ID_START),
                active: true,
                channels_mhz: vec![2437],
                ssids: vec![b"redwood".to_vec(), b"lab".to_vec()],
            })]
        );
    }

    #[test]
    fn scan_ids_exhaust_without_leaving_the_host_range() {
        let mut adapter = Ath11kClientDevice::deterministic(CLIENT);
        adapter.start(Box::new(NoopUpcalls)).unwrap();
        adapter.next_scan_id = HOST_SCAN_ID_END;
        let scan = futures::executor::block_on(
            adapter.start_active_scan(scan_context(), active_scan_request(&[b"lab"])),
        )
        .unwrap();
        assert_eq!(scan.scan_id, Some(u64::from(HOST_SCAN_ID_END)));
        futures::executor::block_on(adapter.cancel_scan(WlanSoftmacBaseCancelScanRequest {
            scan_id: scan.scan_id,
        }))
        .unwrap();
        adapter.device.backend_mut().clear();
        assert_eq!(
            futures::executor::block_on(
                adapter.start_active_scan(scan_context(), active_scan_request(&[b"lab"]))
            ),
            Err(zx::Status::NO_RESOURCES)
        );
        assert!(adapter.device.backend().operations().is_empty());
    }

    #[test]
    fn active_scan_rejects_invalid_out_of_domain_and_busy_requests() {
        let mut adapter = Ath11kClientDevice::deterministic(CLIENT);
        adapter.start(Box::new(NoopUpcalls)).unwrap();
        adapter.device.backend_mut().clear();

        let mut missing_channels = active_scan_request(&[b"lab"]);
        missing_channels.channels = None;
        assert_eq!(
            futures::executor::block_on(
                adapter.start_active_scan(scan_context(), missing_channels)
            ),
            Err(zx::Status::INVALID_ARGS)
        );
        assert_eq!(
            futures::executor::block_on(
                adapter.start_active_scan(scan_context(), active_scan_request(&[]))
            ),
            Err(zx::Status::INVALID_ARGS)
        );
        let mut empty_ssid = active_scan_request(&[b"lab"]);
        empty_ssid.ssids.as_mut().unwrap()[0].len = 0;
        assert_eq!(
            futures::executor::block_on(adapter.start_active_scan(scan_context(), empty_ssid)),
            Err(zx::Status::INVALID_ARGS)
        );
        let too_many_ssids = vec![b"lab".as_slice(); ACTIVE_SCAN_SSID_MAX + 1];
        assert_eq!(
            futures::executor::block_on(
                adapter.start_active_scan(scan_context(), active_scan_request(&too_many_ssids))
            ),
            Err(zx::Status::INVALID_ARGS)
        );
        let mut oversized_ssid = active_scan_request(&[b"lab"]);
        oversized_ssid.ssids.as_mut().unwrap()[0].len = (SSID_BYTE_MAX + 1) as u8;
        assert_eq!(
            futures::executor::block_on(adapter.start_active_scan(scan_context(), oversized_ssid)),
            Err(zx::Status::INVALID_ARGS)
        );
        let mut out_of_domain = active_scan_request(&[b"lab"]);
        out_of_domain.channels.as_mut().unwrap()[0].number = 11;
        assert_eq!(
            futures::executor::block_on(adapter.start_active_scan(scan_context(), out_of_domain)),
            Err(zx::Status::INVALID_ARGS)
        );
        adapter.regulatory_domain.as_mut().unwrap().channels[0].passive = true;
        assert_eq!(
            futures::executor::block_on(
                adapter.start_active_scan(scan_context(), active_scan_request(&[b"lab"]))
            ),
            Err(zx::Status::INVALID_ARGS)
        );
        adapter.regulatory_domain.as_mut().unwrap().channels[0].passive = false;
        assert!(adapter.device.backend().operations().is_empty());

        futures::executor::block_on(
            adapter.start_active_scan(scan_context(), active_scan_request(&[b"lab"])),
        )
        .unwrap();
        assert_eq!(
            futures::executor::block_on(
                adapter.start_active_scan(scan_context(), active_scan_request(&[b"other"]))
            ),
            Err(zx::Status::BAD_STATE)
        );
        assert_eq!(adapter.device.backend().operations().len(), 1);
    }

    #[test]
    fn active_scan_completion_and_cancel_close_the_transaction() {
        let records = Arc::new(Mutex::new(RecordedUpcalls::default()));
        let mut adapter = Ath11kClientDevice::deterministic(CLIENT);
        adapter.start(Box::new(Recorder(records.clone()))).unwrap();
        adapter.device.backend_mut().clear();

        let first = futures::executor::block_on(
            adapter.start_active_scan(scan_context(), active_scan_request(&[b"lab"])),
        )
        .unwrap()
        .scan_id
        .unwrap();
        assert!(adapter.drive().unwrap());
        assert_eq!(records.lock().unwrap().scans, vec![(zx::Status::OK, first)]);
        assert_eq!(adapter.active_scan, None);

        let second = futures::executor::block_on(
            adapter.start_active_scan(scan_context(), active_scan_request(&[b"lab"])),
        )
        .unwrap()
        .scan_id
        .unwrap();
        futures::executor::block_on(adapter.cancel_scan(WlanSoftmacBaseCancelScanRequest {
            scan_id: Some(second),
        }))
        .unwrap();
        assert_eq!(second, first + 1);
        assert_eq!(adapter.active_scan, None);
        assert!(matches!(
            adapter.device.backend().operations().last(),
            Some(Operation::WmiScanStop { scan, .. }) if u64::from(scan.0) == second
        ));
        assert_eq!(records.lock().unwrap().scans.len(), 1);
    }

    fn open_association() -> WlanAssociationConfig {
        WlanAssociationConfig {
            bssid: Some(PEER),
            aid: Some(42),
            listen_interval: Some(0),
            primary: Some(ChannelNumber {
                band: WlanBand::TwoGhz,
                number: 6,
            }),
            qos: Some(false),
            rates: Some(vec![0x82, 0x84, 0x8b, 0x96, 12, 18, 24, 36]),
            capability_info: Some(0x0421),
            bandwidth: Some(ChannelBandwidth::Cbw20),
            vht_secondary_80_channel: Some(ChannelNumber {
                band: WlanBand::TwoGhz,
                number: 0,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn join_and_clear_bind_and_delete_exactly_one_peer() {
        let mut adapter = ready_adapter();
        futures::executor::block_on(adapter.join_bss(operation_context(), join_request())).unwrap();
        let vdev = adapter.vdev.unwrap();
        assert_eq!(
            adapter.device.backend().operations(),
            &[
                Operation::WmiPeerCreate {
                    vdev,
                    address: PEER,
                },
                Operation::WaitPeerCreated {
                    vdev,
                    address: PEER,
                },
                Operation::DpPeerSetup {
                    vdev,
                    address: PEER,
                },
            ]
        );
        futures::executor::block_on(adapter.clear_association(
            operation_context(),
            WlanSoftmacBaseClearAssociationRequest {
                peer_addr: Some(PEER),
            },
        ))
        .unwrap();
        assert_eq!(adapter.peer, None);
        assert!(adapter.device.backend().operations().ends_with(&[
            Operation::WmiPeerDelete {
                vdev,
                address: PEER,
            },
            Operation::WaitPeerDeleted {
                vdev,
                address: PEER,
            },
        ]));
        futures::executor::block_on(adapter.join_bss(operation_context(), join_request())).unwrap();
    }

    #[test]
    fn failed_peer_deletion_still_revokes_adapter_peer_authority() {
        let mut adapter = ready_adapter();
        futures::executor::block_on(adapter.join_bss(operation_context(), join_request())).unwrap();
        let vdev = adapter.vdev.unwrap();
        adapter.device.backend_mut().clear();
        adapter
            .device
            .backend_mut()
            .fail_once(Operation::WmiPeerDelete {
                vdev,
                address: PEER,
            });

        assert_eq!(
            futures::executor::block_on(adapter.clear_association(
                operation_context(),
                WlanSoftmacBaseClearAssociationRequest {
                    peer_addr: Some(PEER),
                }
            )),
            Err(zx::Status::IO)
        );
        assert_eq!(adapter.peer, None);
        assert_eq!(adapter.device.state(), DeviceState::Stopped);
        assert_eq!(
            adapter.device.backend().operations().first(),
            Some(&Operation::DpPeerCleanup {
                vdev,
                address: PEER,
            })
        );
        assert!(
            adapter
                .device
                .backend()
                .operations()
                .contains(&Operation::QmiFirmwareStop)
        );
        assert_eq!(
            futures::executor::block_on(adapter.clear_association(
                operation_context(),
                WlanSoftmacBaseClearAssociationRequest {
                    peer_addr: Some(PEER),
                }
            )),
            Err(zx::Status::BAD_STATE)
        );
    }

    #[test]
    fn ambiguous_join_peer_creation_stops_the_device() {
        let mut adapter = ready_adapter();
        let vdev = adapter.vdev.unwrap();
        adapter
            .device
            .backend_mut()
            .fail_once(Operation::WaitPeerCreated {
                vdev,
                address: PEER,
            });

        assert_eq!(
            futures::executor::block_on(adapter.join_bss(operation_context(), join_request())),
            Err(zx::Status::IO)
        );
        assert_eq!(adapter.peer, None);
        assert_eq!(adapter.device.state(), DeviceState::Stopped);
        assert!(adapter.device.backend().operations().starts_with(&[
            Operation::WmiPeerCreate {
                vdev,
                address: PEER,
            },
            Operation::WaitPeerCreated {
                vdev,
                address: PEER,
            },
        ]));
    }

    #[test]
    fn open_association_and_symmetric_link_preserve_operation_order() {
        let mut adapter = ready_adapter();
        futures::executor::block_on(adapter.join_bss(operation_context(), join_request())).unwrap();
        adapter.device.backend_mut().clear();
        let vdev = adapter.vdev.unwrap();

        futures::executor::block_on(
            adapter.notify_association_complete(operation_context(), open_association()),
        )
        .unwrap();
        adapter.set_link_up(true).unwrap();
        adapter.set_link_up(false).unwrap();
        assert_eq!(
            futures::executor::block_on(adapter.install_key(
                operation_context(),
                WlanKeyConfiguration {
                    protection: Some(fidl_fuchsia_wlan_softmac::WlanProtection::RxTx),
                    cipher_oui: Some([0x00, 0x0f, 0xac]),
                    cipher_type: Some(4),
                    key_type: Some(fidl_fuchsia_wlan_ieee80211::KeyType::Pairwise),
                    peer_addr: Some(PEER),
                    key_idx: Some(0),
                    key: Some(vec![0x55; 16]),
                    rsc: Some(0),
                }
            )),
            Ok(())
        );
        assert_eq!(
            adapter.device.backend().operations(),
            &[
                Operation::WmiPeerAssociate(PeerAssociation {
                    vdev,
                    peer: PEER,
                    aid: 42,
                    listen_interval: 0,
                    primary_mhz: 2437,
                    bandwidth: AssociationBandwidth::Bw20,
                    capability_info: 0x0421,
                    legacy_rates: vec![2, 4, 11, 22, 12, 18, 24, 36],
                    qos: false,
                    ht_capabilities: None,
                    vht_capabilities: None,
                    wmm: None,
                    need_ptk_4_way: false,
                    need_gtk_2_way: false,
                    pmf: false,
                }),
                Operation::WaitPeerAssociated {
                    vdev,
                    address: PEER,
                },
                Operation::WmiVdevUp {
                    vdev,
                    bssid: PEER,
                    aid: 42,
                },
                Operation::WmiObssSpatialReuse { vdev },
                Operation::WmiDtimPolicyStick { vdev },
                Operation::WmiPeerAuthorize {
                    vdev,
                    address: PEER,
                    authorized: false,
                },
                Operation::WmiInstallKey(KeyConfig {
                    vdev,
                    peer: PEER,
                    index: 0,
                    cipher: Cipher::Ccmp128,
                    kind: KeyKind::Pairwise,
                    protection: KeyProtection::RxTx,
                    receive_sequence_counter: 0,
                    bytes: vec![0x55; 16],
                }),
                Operation::WaitKeyInstalled { vdev, key_index: 0 },
                Operation::DpInstallPeerKey(KeyConfig {
                    vdev,
                    peer: PEER,
                    index: 0,
                    cipher: Cipher::Ccmp128,
                    kind: KeyKind::Pairwise,
                    protection: KeyProtection::RxTx,
                    receive_sequence_counter: 0,
                    bytes: vec![0x55; 16],
                }),
            ]
        );
    }

    #[test]
    fn group_and_integrity_keys_decode_sme_wire_order_counters() {
        let mut adapter = ready_adapter();
        futures::executor::block_on(adapter.join_bss(operation_context(), join_request())).unwrap();
        futures::executor::block_on(
            adapter.notify_association_complete(operation_context(), open_association()),
        )
        .unwrap();
        let mut key = WlanKeyConfiguration {
            protection: Some(fidl_fuchsia_wlan_softmac::WlanProtection::RxTx),
            cipher_oui: Some([0, 0x0f, 0xac]),
            cipher_type: Some(4),
            key_type: Some(fidl_fuchsia_wlan_ieee80211::KeyType::Group),
            peer_addr: Some([0xff; 6]),
            key_idx: Some(1),
            key: Some(vec![0x55; 16]),
            rsc: Some(u64::from_be_bytes([2, 1, 0, 0, 0, 0, 0, 0])),
            ..Default::default()
        };
        futures::executor::block_on(adapter.install_key(operation_context(), key.clone())).unwrap();
        assert!(matches!(
            adapter.device.backend().operations().last(),
            Some(Operation::DpInstallPeerKey(KeyConfig {
                kind: KeyKind::Group,
                receive_sequence_counter: 0x102,
                ..
            }))
        ));
        key.rsc = Some(u64::from_be_bytes([0, 0, 0, 0, 0, 0, 1, 0]));
        assert_eq!(
            futures::executor::block_on(adapter.install_key(operation_context(), key.clone())),
            Err(zx::Status::INVALID_ARGS)
        );
        key.key_type = Some(fidl_fuchsia_wlan_ieee80211::KeyType::Igtk);
        key.cipher_type = Some(6);
        key.key_idx = Some(4);
        key.rsc = Some(u64::from_be_bytes([0, 0, 6, 5, 4, 3, 2, 1]));
        futures::executor::block_on(adapter.install_key(operation_context(), key)).unwrap();
        assert_eq!(adapter.igtk.as_ref().unwrap().receive_ipn, 0x0102_0304_0506);
    }

    #[test]
    fn ap_uapsd_capability_does_not_require_station_uapsd() {
        for apsd in [false, true] {
            let mut adapter = ready_adapter();
            futures::executor::block_on(adapter.join_bss(operation_context(), join_request()))
                .unwrap();
            let ac = || fidl_fuchsia_wlan_driver::WlanWmmAccessCategoryParameters {
                ecw_min: 4,
                ecw_max: 10,
                aifsn: 3,
                txop_limit: 0,
                acm: false,
            };
            let mut association = open_association();
            association.qos = Some(true);
            association.wmm_params = Some(fidl_fuchsia_wlan_driver::WlanWmmParameters {
                apsd,
                ac_be_params: ac(),
                ac_bk_params: ac(),
                ac_vi_params: ac(),
                ac_vo_params: ac(),
            });
            futures::executor::block_on(
                adapter.notify_association_complete(operation_context(), association),
            )
            .unwrap();
            assert!(adapter.associated);
        }
    }

    #[test]
    fn secure_association_requires_transmitted_rsn_evidence() {
        let mut adapter = ready_adapter();
        futures::executor::block_on(adapter.join_bss(operation_context(), join_request())).unwrap();
        adapter.device.backend_mut().clear();
        let mut association = open_association();
        association.capability_info = Some(0x0431);
        assert_eq!(
            futures::executor::block_on(
                adapter.notify_association_complete(operation_context(), association)
            ),
            Err(zx::Status::BAD_STATE)
        );
        assert!(adapter.device.backend().operations().is_empty());
    }

    #[test]
    fn secure_association_stays_unauthorized_until_controlled_port_up() {
        let mut adapter = ready_adapter();
        futures::executor::block_on(adapter.join_bss(operation_context(), join_request())).unwrap();
        adapter.device.backend_mut().clear();

        let mut request = vec![0; 28];
        request[4..10].copy_from_slice(&PEER);
        request.extend_from_slice(&[
            48, 20, // RSNE
            1, 0, // version
            0, 0x0f, 0xac, 4, // group CCMP-128
            1, 0, 0, 0x0f, 0xac, 4, // one pairwise CCMP-128
            1, 0, 0, 0x0f, 0xac, 2, // one PSK AKM
            0, 0, // no PMF
        ]);
        adapter
            .queue_tx(operation_context(), &request, WlanTxInfoFlags::empty())
            .unwrap();

        let mut association = open_association();
        association.capability_info = Some(0x0421);
        futures::executor::block_on(
            adapter.notify_association_complete(operation_context(), association),
        )
        .unwrap();
        assert!(adapter.associated);
        assert!(!adapter.link_up);
        assert!(
            adapter
                .device
                .backend()
                .operations()
                .iter()
                .any(|operation| {
                    matches!(
                        operation,
                        Operation::WmiPeerAssociate(PeerAssociation {
                            need_ptk_4_way: true,
                            need_gtk_2_way: false,
                            pmf: false,
                            ..
                        })
                    )
                })
        );

        let mut data = vec![0; 24];
        data[..2].copy_from_slice(&0x0108_u16.to_le_bytes());
        data[4..10].copy_from_slice(&PEER);
        data[10..16].copy_from_slice(&CLIENT);
        data[16..22].copy_from_slice(&PEER);
        data.extend_from_slice(&[0xaa, 0xaa, 3, 0, 0, 0, 8, 0]);
        assert_eq!(
            adapter.queue_tx(operation_context(), &data, WlanTxInfoFlags::empty()),
            Err(zx::Status::BAD_STATE)
        );
        data[30..32].copy_from_slice(&[0x88, 0x8e]);
        adapter
            .queue_tx(
                operation_context(),
                &data,
                WlanTxInfoFlags::FAVOR_RELIABILITY,
            )
            .unwrap();
        assert!(matches!(adapter.device.backend().operations().last(),
            Some(Operation::DpTransmitData { flags, .. }) if flags.favor_reliability));

        adapter.set_link_up(true).unwrap();
        assert!(adapter.link_up);
        assert!(matches!(
            adapter.device.backend().operations().last(),
            Some(Operation::WmiPeerAuthorize {
                address: PEER,
                authorized: true,
                ..
            })
        ));
        data[30..32].copy_from_slice(&[8, 0]);
        adapter
            .queue_tx(operation_context(), &data, WlanTxInfoFlags::empty())
            .unwrap();
        data[4] ^= 2;
        assert_eq!(
            adapter.queue_tx(operation_context(), &data, WlanTxInfoFlags::empty()),
            Err(zx::Status::BAD_STATE)
        );
    }

    #[test]
    fn deterministic_ath11k_passes_the_generic_client_contract() {
        let channel = ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 6,
        };
        assert_eq!(
            run_client_conformance(Ath11kClientDevice::deterministic(CLIENT), channel).unwrap(),
            expected_client_conformance(CLIENT, u64::from(HOST_SCAN_ID_START))
        );
    }

    #[test]
    fn management_tx_and_rx_cross_the_host_seam_with_buffer_correlation() {
        let records = Arc::new(Mutex::new(RecordedUpcalls::default()));
        let mut adapter = Ath11kClientDevice::deterministic(CLIENT);
        adapter.start(Box::new(Recorder(records.clone()))).unwrap();
        futures::executor::block_on(
            adapter.set_channel(
                wlan_softmac_class_support::conformance::operation_context(
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                )
                .0,
                WlanSoftmacBaseSetChannelRequest {
                    primary: Some(ChannelNumber {
                        band: WlanBand::TwoGhz,
                        number: 6,
                    }),
                    bandwidth: Some(ChannelBandwidth::Cbw20),
                    vht_secondary_80_channel: Some(ChannelNumber {
                        band: WlanBand::TwoGhz,
                        number: 0,
                    }),
                },
            ),
        )
        .unwrap();
        let mut frame = vec![0; 24];
        frame[0] = 0xb0;
        frame[4..10].copy_from_slice(&[2, 0, 0, 0, 0, 2]);
        adapter
            .queue_tx(operation_context(), &frame, WlanTxInfoFlags::empty())
            .unwrap();
        let vdev = adapter.vdev.unwrap();
        assert!(
            adapter
                .device
                .backend()
                .operations()
                .contains(&Operation::WmiMgmtTx(ManagementFrame {
                    vdev,
                    buffer_id: 0,
                    bytes: frame.clone(),
                }))
        );
        adapter
            .device
            .backend_mut()
            .push_event(WlanEvent::ManagementTxCompleted {
                buffer_id: 0,
                status: 0,
                ack_rssi: 42,
            });
        adapter
            .device
            .backend_mut()
            .push_event(WlanEvent::ManagementReceived {
                pdev_id: 0,
                channel_mhz: 2437,
                snr: 0,
                rssi: -48,
                flags: 0,
                frame: frame.clone(),
            });
        assert!(adapter.drive().unwrap());
        assert!(adapter.drive().unwrap());
        let mut group = frame.clone();
        group[4..10].copy_from_slice(&[0xff; 6]);
        adapter
            .queue_tx(operation_context(), &group, WlanTxInfoFlags::empty())
            .unwrap();
        adapter
            .device
            .backend_mut()
            .push_event(WlanEvent::ManagementTxCompleted {
                buffer_id: 1,
                status: 0,
                ack_rssi: 0,
            });
        assert!(adapter.drive().unwrap());
        let records = records.lock().unwrap();
        assert_eq!(records.received, [frame]);
        assert_eq!(
            records.tx,
            [
                (
                    [2, 0, 0, 0, 0, 2],
                    fidl_fuchsia_wlan_softmac::WlanTxResultCode::Success
                ),
                (
                    [0xff; 6],
                    fidl_fuchsia_wlan_softmac::WlanTxResultCode::Failed
                )
            ]
        );
    }

    #[test]
    fn simultaneous_dp_and_management_rx_share_one_receive_slot() {
        let records = Arc::new(Mutex::new(RecordedUpcalls::default()));
        let receive_budgets = Arc::new(Mutex::new(Vec::new()));
        let dp_frame = vec![0x08, 0x00, 1];
        let mgmt_frame = vec![0x80, 0x00, 2];
        let mut model = ModelSubsystems::default();
        model.push_event(WlanEvent::ManagementReceived {
            pdev_id: 0,
            channel_mhz: 2437,
            snr: 0,
            rssi: -48,
            flags: 0,
            frame: mgmt_frame.clone(),
        });
        let backend = SimultaneousRxSubsystems {
            model,
            dp_rx: Some(ath11k_dp::tx::HostRxFrame {
                bytes: dp_frame.clone(),
                info: ath11k_dp::tx::HostRxInfo {
                    decap_type: ath11k_dp::tx::RxDecapType::Raw,
                    peer: None,
                    tid: 0,
                    decrypt_status: ath11k_dp::tx::RxDecryptStatus::NotDecrypted,
                    phy_metadata: (2437 << 16) | 6,
                    bandwidth: 0,
                    mcs: 0,
                    packet_type: 0,
                    nss: 1,
                    phy_ppdu_id: 0,
                },
            }),
            receive_budgets: receive_budgets.clone(),
        };
        let mut adapter = Ath11kClientDevice::new(WCN6750.device(backend), CLIENT);
        adapter.start(Box::new(Recorder(records.clone()))).unwrap();

        assert!(adapter.drive().unwrap());
        assert_eq!(
            records.lock().unwrap().received.as_slice(),
            std::slice::from_ref(&dp_frame)
        );
        assert!(adapter.drive().unwrap());
        assert_eq!(records.lock().unwrap().received, [dp_frame, mgmt_frame]);
        assert_eq!(*receive_budgets.lock().unwrap(), [1, 0]);
        assert_eq!(
            records.lock().unwrap().received_channels,
            [ChannelNumber {
                band: WlanBand::TwoGhz,
                number: 6
            }; 2]
        );
    }

    #[test]
    fn management_tx_rejects_unsupported_frame_control() {
        let mut adapter = Ath11kClientDevice::deterministic(CLIENT);
        adapter.start(Box::new(NoopUpcalls)).unwrap();
        let mut frame = vec![0; 24];
        frame[0] = 0xb0;

        assert_eq!(
            adapter.queue_tx(operation_context(), &frame, WlanTxInfoFlags::PROTECTED),
            Err(zx::Status::NOT_SUPPORTED)
        );
        frame[1] |= 0x40;
        assert_eq!(
            adapter.queue_tx(operation_context(), &frame, WlanTxInfoFlags::empty()),
            Err(zx::Status::NOT_SUPPORTED)
        );
        frame[1] &= !0x40;
        frame[0] |= 1;
        assert_eq!(
            adapter.queue_tx(operation_context(), &frame, WlanTxInfoFlags::empty()),
            Err(zx::Status::INVALID_ARGS)
        );
    }

    #[test]
    fn failed_firmware_attach_unwinds_probed_device() {
        let device = failed_start(Operation::QmiWaitFirmwareReady);
        assert_eq!(device.state(), DeviceState::Stopped);
        assert!(device.backend().operations().ends_with(&[
            Operation::HifPowerDown,
            Operation::RegFree,
            Operation::QmiDeinitService,
        ]));
    }

    #[test]
    fn failed_hif_power_up_unwinds_allocated_device() {
        let device = failed_start(Operation::HifPowerUp);
        assert_eq!(device.state(), DeviceState::Stopped);
        assert!(
            device
                .backend()
                .operations()
                .contains(&Operation::WmiDetach)
        );
    }

    #[test]
    fn failed_dp_allocation_releases_partial_transport() {
        let device = failed_start(Operation::DpAllocate);
        assert_eq!(device.state(), DeviceState::Stopped);
        assert!(
            device
                .backend()
                .operations()
                .contains(&Operation::WmiDetach)
        );
    }

    #[test]
    fn failed_radio_start_unwinds_ready_device() {
        let device = failed_start(Operation::RadioStart);
        assert_eq!(device.state(), DeviceState::Stopped);
        assert!(
            device
                .backend()
                .operations()
                .contains(&Operation::QmiFirmwareStop)
        );
        assert!(device.backend().operations().ends_with(&[
            Operation::DpFree,
            Operation::RegFree,
            Operation::QmiDeinitService,
        ]));
    }

    #[test]
    fn bip_uses_rfc4493_cmac_and_rejects_replay() {
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let message = [
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a,
        ];
        assert_eq!(
            aes_cmac_128(&key, &message),
            [
                0x07, 0x0a, 0x16, 0xb4, 0x6b, 0x4d, 0x41, 0x44, 0xf7, 0x9b, 0xdd, 0x9d, 0xd0, 0x4a,
                0x28, 0x7c
            ]
        );

        let mut tx = Igtk {
            key_id: 4,
            key,
            receive_ipn: 0,
            transmit_ipn: 0,
        };
        let mut frame = vec![0u8; 26];
        frame[0] = 0xc0;
        frame[4..10].fill(0xff);
        let protected = protect_group_management(&frame, &mut tx).unwrap();
        let mut rx = Igtk {
            key_id: 4,
            key,
            receive_ipn: 0,
            transmit_ipn: 0,
        };
        assert!(verify_group_management(&protected, &mut rx));
        assert!(!verify_group_management(&protected, &mut rx));
        let mut forged = protected;
        forged[24] ^= 1;
        rx.receive_ipn = 0;
        assert!(!verify_group_management(&forged, &mut rx));
    }

    #[test]
    fn association_security_accepts_sae_with_pmf_and_bip_cmac_128() {
        let peer = [2, 0, 0, 0, 0, 2];
        let mut request = vec![0u8; 28];
        request[4..10].copy_from_slice(&peer);
        let rsne = [
            1, 0, 0, 0x0f, 0xac, 4, // version and group CCMP
            1, 0, 0, 0x0f, 0xac, 4, // one pairwise CCMP
            1, 0, 0, 0x0f, 0xac, 8, // one SAE AKM
            0xc0, 0, // MFPC and MFPR
            0, 0, // no PMKIDs
            0, 0x0f, 0xac, 6, // BIP-CMAC-128
        ];
        request.extend_from_slice(&[48, rsne.len() as u8]);
        request.extend_from_slice(&rsne);
        assert_eq!(
            association_security(&request).unwrap(),
            PendingAssociationSecurity {
                peer,
                need_ptk_4_way: true,
                need_gtk_2_way: false,
                pmf: true,
            }
        );
    }
}
