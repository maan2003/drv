// SPDX-License-Identifier: GPL-2.0-only

//! Production MT7921 client effects shared by physical and test composition.

use crate::LinuxChannelShape;
use crate::client_device::{
    ASSOCIATION_CAPABILITY_INPUT_SOURCE, AssociationCapabilityTransformation, ClientChannelEnsure,
    ClientRuntimeScanState, ClientRxFrame, ClientRxPoll, Mt7921ClientEffects, Mt7921ClientIo,
};
use fidl_fuchsia_wlan_driver as fidl_driver;
use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use fuchsia_softmac_port::{ChannelNumber, WlanBand};
use mt7921_core::{
    ClientChannelContext, ClientDataGeneration, ClientEdcaAc, ClientEdcaParameters,
    ClientFirmwareEffectsState, ClientPhysicalChannel, ClientPhysicalChannelEnsure,
    ClientRxCandidate, ClientScanEvidence, ClientTargetBssLease, ClientWcid, LegacyWmeAssociation,
    classify_preassociation_sae_auth, linux_legacy_rate_context_reference,
};
use num_bigint::BigUint;
use sha2::{Digest as _, Sha256};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Redacted diagnostic event produced by the live effects owner.
pub struct LiveClientEvent {
    message: String,
    direct_json: bool,
}

/// Maximum encoded size of every event delivered to a live client observer.
pub const LIVE_CLIENT_EVENT_MAX_BYTES: usize = 4096;

impl LiveClientEvent {
    fn new(mut message: String, direct_json: bool) -> Self {
        if message.len() > LIVE_CLIENT_EVENT_MAX_BYTES {
            let mut end = LIVE_CLIENT_EVENT_MAX_BYTES;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        Self {
            message,
            direct_json,
        }
    }

    /// Returns the bounded, secret-free event rendering.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether this event retains the legacy direct-JSON output channel.
    pub fn direct_json(&self) -> bool {
        self.direct_json
    }
}

fn notify_observer(observer: LiveClientObserver, event: LiveClientEvent) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (observer.observe)(&event);
    }));
}

fn stage_event(message: &str) -> LiveClientEvent {
    LiveClientEvent::new(message.to_owned(), false)
}

/// Trusted synchronous diagnostic observer supplied by the service entrypoint.
///
/// Events are emitted only after effects locks are released. The callback is
/// panic-contained, returns no value, and can never affect an operation's
/// result. Events are bounded by [`LIVE_CLIENT_EVENT_MAX_BYTES`] and contain
/// only redacted scalar/hash telemetry, never key or frame byte slices.
#[derive(Clone, Copy)]
pub struct LiveClientObserver {
    observe: fn(&LiveClientEvent),
}

fn ignore_event(_: &LiveClientEvent) {}

impl Default for LiveClientObserver {
    fn default() -> Self {
        Self {
            observe: ignore_event,
        }
    }
}

impl LiveClientObserver {
    pub const fn new(observe: fn(&LiveClientEvent)) -> Self {
        Self { observe }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn auth_frame_event(direction: &str, frame: &[u8]) -> LiveClientEvent {
    let word = |offset| {
        frame
            .get(offset..offset + 2)
            .map(|value| u16::from_le_bytes([value[0], value[1]]))
    };
    let control = word(0);
    let sequence_control = word(22);
    let algorithm = word(24);
    let transaction = word(26);
    let status = word(28);
    let fields = frame.get(30..).unwrap_or_default();
    let group = (transaction == Some(1) && fields.len() >= 2)
        .then(|| u16::from_le_bytes([fields[0], fields[1]]));
    let confirm_counter = (transaction == Some(2) && fields.len() >= 2)
        .then(|| u16::from_le_bytes([fields[0], fields[1]]));
    LiveClientEvent::new(
        format!(
            "sae_auth_frame_structure monotonic_ns={{observer_monotonic_ns}} direction={direction} retry={} sequence={} fragment={} algorithm={} transaction={} status={} group={} confirm_counter={} fields_len={} fields_sha256={} fixed_fields_complete={}",
            control.is_some_and(|value| value & 0x0800 != 0),
            sequence_control.map_or_else(|| "unknown".into(), |value| (value >> 4).to_string()),
            sequence_control.map_or_else(|| "unknown".into(), |value| (value & 15).to_string()),
            algorithm.map_or_else(|| "unknown".into(), |value| value.to_string()),
            transaction.map_or_else(|| "unknown".into(), |value| value.to_string()),
            status.map_or_else(|| "unknown".into(), |value| value.to_string()),
            group.map_or_else(|| "none".into(), |value| value.to_string()),
            confirm_counter.map_or_else(|| "none".into(), |value| value.to_string()),
            fields.len(),
            sha256_hex(fields),
            control.is_some()
                && sequence_control.is_some()
                && algorithm.is_some()
                && transaction.is_some()
                && status.is_some(),
        ),
        false,
    )
}

fn validate_sae_commit(frame: &[u8]) -> Result<LiveClientEvent, String> {
    let header = frame
        .get(..32)
        .ok_or("SAE commit is shorter than the authentication header")?;
    let algorithm = u16::from_le_bytes(header[24..26].try_into().unwrap());
    let transaction = u16::from_le_bytes(header[26..28].try_into().unwrap());
    let status = u16::from_le_bytes(header[28..30].try_into().unwrap());
    let group = u16::from_le_bytes(header[30..32].try_into().unwrap());
    if algorithm != 3 || transaction != 1 || status != 126 {
        return Err(format!(
            "unexpected SAE commit header algorithm={algorithm} transaction={transaction} status={status} group={group}"
        ));
    }
    let (scalar_len, element_len, order_hex, p_hex, b_hex) = match group {
        19 => (
            32,
            64,
            b"ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551".as_slice(),
            b"ffffffff00000001000000000000000000000000ffffffffffffffffffffffff".as_slice(),
            b"5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b".as_slice(),
        ),
        20 => (
            48,
            96,
            b"ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973".as_slice(),
            b"fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff".as_slice(),
            b"b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef".as_slice(),
        ),
        _ => return Err(format!("unexpected SAE commit group {group}")),
    };
    let fixed_end = 32 + scalar_len + element_len;
    let fixed = frame
        .get(..fixed_end)
        .ok_or("SAE commit is shorter than the selected group's fixed body")?;
    let scalar = &fixed[32..32 + scalar_len];
    let element = &fixed[32 + scalar_len..fixed_end];
    let scalar_value = BigUint::from_bytes_be(scalar);
    let order = BigUint::parse_bytes(order_hex, 16).unwrap();
    let scalar_range = scalar_value > BigUint::from(1u8) && scalar_value < order;
    let p = BigUint::parse_bytes(p_hex, 16).unwrap();
    let b = BigUint::parse_bytes(b_hex, 16).unwrap();
    let coordinate_len = element_len / 2;
    let x = BigUint::from_bytes_be(&element[..coordinate_len]);
    let y = BigUint::from_bytes_be(&element[coordinate_len..]);
    let three_x = (&x * BigUint::from(3u8)) % &p;
    let rhs = (x.modpow(&BigUint::from(3u8), &p) + (&p - three_x) + b) % &p;
    let element_on_curve = x < p && y < p && y.modpow(&BigUint::from(2u8), &p) == rhs;
    let mut offset = fixed_end;
    let mut tail_ies = Vec::new();
    while offset < frame.len() {
        let header = frame
            .get(offset..offset + 2)
            .ok_or("SAE commit has a truncated tail IE header")?;
        let len = usize::from(header[1]);
        frame
            .get(offset + 2..offset + 2 + len)
            .ok_or("SAE commit has a truncated tail IE body")?;
        if tail_ies.len() < 64 {
            tail_ies.push(format!("{}:{len}", header[0]));
        }
        offset += 2 + len;
    }
    let mac = |offset| {
        frame[offset..offset + 6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    };
    Ok(LiveClientEvent::new(
        format!(
            r#"{{"sae_commit_structure":{{"fc":"0x{:04x}","receiver":"{}","transmitter":"{}","bssid":"{}","seq_control":{},"algorithm":{},"transaction":{},"status":{},"group":{},"body_len":{},"scalar_len":{},"element_len":{},"tail_len":{},"scalar_range":{},"element_on_curve":{},"tail_ies":"{}"}}}}"#,
            u16::from_le_bytes(frame[0..2].try_into().unwrap()),
            mac(4),
            mac(10),
            mac(16),
            u16::from_le_bytes(frame[22..24].try_into().unwrap()),
            algorithm,
            transaction,
            status,
            group,
            frame.len() - 30,
            scalar.len(),
            element.len(),
            frame.len() - fixed_end,
            scalar_range,
            element_on_curve,
            tail_ies.join(","),
        ),
        true,
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TargetBeaconTim<'a> {
    dtim_count: u8,
    dtim_period: u8,
    bitmap_control: u8,
    bitmap_offset: u8,
    partial_virtual_bitmap: &'a [u8],
    multicast_buffered: bool,
    aid_buffered: bool,
}

fn parse_target_beacon_tim(
    ies: &[u8],
    normalized_aid: u16,
) -> Result<Option<TargetBeaconTim<'_>>, &'static str> {
    if !(1..=2007).contains(&normalized_aid) {
        return Err("invalid_normalized_aid");
    }
    let mut offset = 0usize;
    while offset < ies.len() {
        let header = ies.get(offset..offset + 2).ok_or("truncated_ie_header")?;
        let end = offset
            .checked_add(2 + usize::from(header[1]))
            .ok_or("ie_length_overflow")?;
        let body = ies.get(offset + 2..end).ok_or("truncated_ie_body")?;
        offset = end;
        if header[0] != 5 {
            continue;
        }
        if body.len() < 4 {
            return Err("malformed_tim_length");
        }
        if body[1] == 0 {
            return Err("zero_dtim_period");
        }
        // This is the same index/mask calculation as Linux
        // ieee80211_check_tim: AID bits 13:0 select a byte and bit in the
        // partial virtual bitmap; bitmap-control bit 0 is multicast only.
        let aid = normalized_aid & 0x3fff;
        let index = usize::from(aid / 8);
        let mask = 1u8 << (aid & 7);
        let bitmap_offset = body[2] & 0xfe;
        let partial_virtual_bitmap = &body[3..];
        let first = usize::from(bitmap_offset);
        let aid_buffered = index
            .checked_sub(first)
            .and_then(|relative| partial_virtual_bitmap.get(relative))
            .is_some_and(|byte| byte & mask != 0);
        return Ok(Some(TargetBeaconTim {
            dtim_count: body[0],
            dtim_period: body[1],
            bitmap_control: body[2],
            bitmap_offset,
            partial_virtual_bitmap,
            multicast_buffered: body[2] & 1 != 0,
            aid_buffered,
        }));
    }
    Ok(None)
}

#[derive(Default)]
struct TargetBeaconTimTelemetry {
    beacon_count: u64,
    tim_present_count: u64,
    aid_buffered_true_count: u64,
    aid_buffered_true_transition_count: u64,
    previous_aid_buffered: Option<bool>,
}

#[derive(Default)]
struct LiveClientState {
    selection: ClientTargetBssLease,
    channel: ClientChannelContext,
}

/// Revocable authorization half paired with one live effects owner.
///
/// Selection and rate/power evidence can be advanced only through this
/// handle; the effects value keeps the shared state private.
pub struct LiveClientAuthorization {
    state: Arc<Mutex<LiveClientState>>,
    target: [u8; 6],
}

impl LiveClientAuthorization {
    pub fn mark_rate_power_ready(
        &mut self,
        bssid: [u8; 6],
        channel: ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: ChannelNumber,
    ) -> Result<(), String> {
        if bssid != self.target {
            return Err("rate-power readiness does not match the live target".into());
        }
        self.state
            .lock()
            .unwrap()
            .mark_rate_power_ready(bssid, channel, bandwidth, secondary)
    }

    pub fn authorize_sae(
        &mut self,
        bssid: [u8; 6],
        channel: ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: ChannelNumber,
    ) -> Result<u64, String> {
        if bssid != self.target {
            return Err("SAE authorization does not match the live target".into());
        }
        self.state
            .lock()
            .unwrap()
            .authorize_sae(bssid, channel, bandwidth, secondary)
    }
}

impl LiveClientState {
    fn authorize_sae(
        &mut self,
        bssid: [u8; 6],
        channel: ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: ChannelNumber,
    ) -> Result<u64, String> {
        let physical = client_physical_channel(channel, bandwidth, secondary)
            .map_err(|status| status.to_string())?;
        let ClientPhysicalChannelEnsure::Current(channel) = self.channel.ensure_channel(physical)
        else {
            return Err("SAE authorization does not match physical channel".into());
        };
        self.selection.authorize_sae(bssid, channel)?;
        self.channel.authorize_channel(physical)?;
        Ok(channel.generation)
    }

    fn mark_rate_power_ready(
        &mut self,
        bssid: [u8; 6],
        channel: ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: ChannelNumber,
    ) -> Result<(), String> {
        self.selection.mark_rate_power_ready(
            bssid,
            client_physical_channel(channel, bandwidth, secondary)
                .map_err(|status| status.to_string())?,
        )
    }
}

pub fn client_physical_channel(
    channel: ChannelNumber,
    bandwidth: fidl_ieee80211::ChannelBandwidth,
    secondary: ChannelNumber,
) -> Result<ClientPhysicalChannel, zx::Status> {
    let band = match channel.band {
        WlanBand::TwoGhz => 0,
        WlanBand::FiveGhz => 1,
        _ => return Err(zx::Status::INVALID_ARGS),
    };
    let shape = LinuxChannelShape::from_fidl(channel, bandwidth, Some(secondary))
        .ok_or(zx::Status::INVALID_ARGS)?;
    Ok(ClientPhysicalChannel {
        band,
        primary: u16::from(channel.number),
        center: u16::from(shape.center_channel),
        bandwidth: shape.bandwidth,
        center2: u16::from(shape.center_channel2),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub struct ClientDataFrameClassification {
    pub frame_type: u8,
    pub subtype: u8,
    pub to_ds: bool,
    pub from_ds: bool,
    pub protected: bool,
    pub header_offset: usize,
    pub qos: bool,
    pub amsdu: bool,
    pub addr1_is_client: bool,
    pub addr2_is_peer: bool,
    pub addr3_is_bssid: bool,
    pub snap_present: bool,
    pub ether_type: Option<u16>,
    pub llc_result: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub struct ClientManagementFrameClassification {
    pub subtype: u8,
    pub addr1_is_client: bool,
    pub addr2_is_peer: bool,
    pub addr3_is_bssid: bool,
}

#[doc(hidden)]
pub fn classify_client_management_frame(
    bytes: &[u8],
    client: [u8; 6],
    peer: [u8; 6],
) -> ClientManagementFrameClassification {
    let control = bytes
        .get(..2)
        .map(|value| u16::from_le_bytes([value[0], value[1]]))
        .unwrap_or(0);
    ClientManagementFrameClassification {
        subtype: ((control >> 4) & 15) as u8,
        addr1_is_client: bytes.get(4..10) == Some(&client),
        addr2_is_peer: bytes.get(10..16) == Some(&peer),
        addr3_is_bssid: bytes.get(16..22) == Some(&peer),
    }
}

#[doc(hidden)]
pub fn strip_verified_management_ccmp(bytes: &mut Vec<u8>) -> Result<(), ()> {
    if bytes.len() >= 42 {
        let mut plaintext = Vec::with_capacity(bytes.len() - 16);
        plaintext.extend_from_slice(&bytes[..24]);
        plaintext.extend_from_slice(&bytes[32..bytes.len() - 8]);
        *bytes = plaintext;
    }
    (bytes.len() >= 26).then_some(()).ok_or(())
}

#[doc(hidden)]
pub fn management_ie_id_lengths(bytes: &[u8], offset: usize) -> String {
    let mut cursor = offset;
    let mut fields = Vec::new();
    while cursor < bytes.len() {
        let Some(header) = bytes.get(cursor..cursor + 2) else {
            fields.push("malformed".to_string());
            break;
        };
        let length = usize::from(header[1]);
        fields.push(format!("{}:{length}", header[0]));
        let Some(next) = cursor.checked_add(2 + length) else {
            fields.push("overflow".to_string());
            break;
        };
        if next > bytes.len() {
            fields.push("truncated".to_string());
            break;
        }
        cursor = next;
    }
    fields.join(",")
}

#[doc(hidden)]
pub fn association_comeback_interval(bytes: &[u8], offset: usize) -> Option<(u32, u64)> {
    let mut cursor = offset;
    let mut comeback = None;
    while cursor < bytes.len() {
        let header = bytes.get(cursor..cursor.checked_add(2)?)?;
        let length = usize::from(header[1]);
        let next = cursor.checked_add(2 + length)?;
        let body = bytes.get(cursor + 2..next)?;
        if header[0] == 56 {
            if comeback.is_some() || body.len() != 5 || body[0] != 3 {
                return None;
            }
            let tu = u32::from_le_bytes(body[1..5].try_into().ok()?);
            if tu == 0 {
                return None;
            }
            comeback = Some((tu, u64::from(tu) * 1024 / 1000));
        }
        cursor = next;
    }
    comeback
}

#[doc(hidden)]
pub fn classify_client_data_frame(
    bytes: &[u8],
    client: [u8; 6],
    peer: [u8; 6],
) -> ClientDataFrameClassification {
    let control = bytes
        .get(..2)
        .map(|value| u16::from_le_bytes([value[0], value[1]]))
        .unwrap_or(0);
    let frame_type = ((control >> 2) & 3) as u8;
    let subtype = ((control >> 4) & 15) as u8;
    let to_ds = control & 0x0100 != 0;
    let from_ds = control & 0x0200 != 0;
    let protected = control & 0x4000 != 0;
    let addr1_is_client = bytes.get(4..10) == Some(&client);
    let addr2_is_peer = bytes.get(10..16) == Some(&peer);
    let addr3_is_bssid = bytes.get(16..22) == Some(&peer);

    let mut body_offset = 24usize;
    if to_ds && from_ds {
        body_offset += 6;
    }
    let qos = subtype & 8 != 0;
    let amsdu = if qos {
        let value = bytes.get(body_offset..body_offset + 2);
        body_offset += 2;
        value.is_some_and(|value| value[0] & 0x80 != 0)
    } else {
        false
    };
    // Pinned Fuchsia `DataFrame::parse_frame_type_unchecked` consumes HT
    // control whenever FrameControl::htc_order() is set, after Addr4/QoS.
    if control & 0x8000 != 0 {
        body_offset += 4;
    }
    let (llc_result, snap_present, ether_type) = if frame_type != 2 {
        ("not_data", false, None)
    } else if amsdu {
        ("amsdu", false, None)
    } else if bytes.len() < body_offset {
        ("header_truncated", false, None)
    } else if bytes.len() < body_offset + 8 {
        ("llc_truncated", false, None)
    } else {
        let snap = bytes.get(body_offset..body_offset + 6) == Some(&[0xaa, 0xaa, 3, 0, 0, 0]);
        let ether_type = Some(u16::from_be_bytes([
            bytes[body_offset + 6],
            bytes[body_offset + 7],
        ]));
        (if snap { "valid" } else { "non_snap" }, snap, ether_type)
    };
    ClientDataFrameClassification {
        frame_type,
        subtype,
        to_ds,
        from_ds,
        protected,
        header_offset: body_offset,
        qos,
        amsdu,
        addr1_is_client,
        addr2_is_peer,
        addr3_is_bssid,
        snap_present,
        ether_type,
        llc_result,
    }
}

fn is_exact_target_eapol_candidate(classification: &ClientDataFrameClassification) -> bool {
    classification.frame_type == 2
        && !classification.to_ds
        && classification.from_ds
        && classification.addr1_is_client
        && classification.addr2_is_peer
        && classification.addr3_is_bssid
        && classification.snap_present
        && classification.ether_type == Some(0x888e)
        && classification.llc_result == "valid"
}

#[doc(hidden)]
pub fn is_anchored_eapol_data(bytes: &[u8]) -> bool {
    let classification = classify_client_data_frame(bytes, [0; 6], [0; 6]);
    classification.frame_type == 2
        && classification.snap_present
        && classification.ether_type == Some(0x888e)
}

#[derive(Clone, Copy)]
struct RuntimeTargetScan {
    id: u64,
    observation_generation: u64,
    operating_channel: Option<ClientPhysicalChannel>,
    target_channel: Option<ClientPhysicalChannel>,
}

pub struct LiveClientEffects {
    state: Arc<Mutex<LiveClientState>>,
    target: [u8; 6],
    client: [u8; 6],
    rcpi: u8,
    dtim_period: u8,
    preauth_rates: Option<(u16, u16)>,
    firmware: ClientFirmwareEffectsState,
    peer_wcid: Option<ClientWcid>,
    join_roc_generation: Option<u64>,
    established_channel: Option<ClientPhysicalChannel>,
    join_roc_deadline: Option<Instant>,
    post_association_data_wait: Option<Instant>,
    eapol_start_deadline: Option<(Instant, u64)>,
    eapol_start_emitted: bool,
    target_beacon_tim: TargetBeaconTimTelemetry,
    post_assoc_rx_ready_generation: Option<u64>,
    observer: LiveClientObserver,
    runtime_scan: Option<RuntimeTargetScan>,
}

const EAPOL_START_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

#[doc(hidden)]
pub fn eapol_start_frame(client: [u8; 6], peer: [u8; 6], qos: bool) -> Vec<u8> {
    let mut frame = vec![if qos { 0x88 } else { 0x08 }, 0x01, 0, 0];
    frame.extend_from_slice(&peer);
    frame.extend_from_slice(&client);
    frame.extend_from_slice(&[0x01, 0x80, 0xc2, 0x00, 0x00, 0x03]);
    frame.extend_from_slice(&[0, 0]);
    if qos {
        // Linux/mac80211 maps the control-port packet to voice priority 7;
        // the QoS control field is part of the 26-byte 802.11 header.
        frame.extend_from_slice(&[7, 0]);
    }
    frame.extend_from_slice(&[0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
    // The pinned EAPOL stack's IEEE802DOT1X2001 version, Start type, and an
    // empty packet body (IEEE 802.1X).
    frame.extend_from_slice(&[1, 1, 0, 0]);
    frame
}

#[doc(hidden)]
pub fn classify_eapol_key(bytes: &[u8]) -> Option<(u16, &'static str)> {
    let Some(body_offset) = bytes
        .windows(8)
        .position(|window| window == [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e])
        .map(|offset| offset + 8)
    else {
        return None;
    };
    let Some(eapol) = bytes.get(body_offset..) else {
        return None;
    };
    if eapol.len() < 9 || eapol[1] != 3 {
        return None;
    }
    let packet_body_len = usize::from(u16::from_be_bytes([eapol[2], eapol[3]]));
    if packet_body_len < 95 || eapol.len() < 4 + packet_body_len {
        return None;
    }
    let key_info = u16::from_be_bytes([eapol[5], eapol[6]]);
    let descriptor_version = key_info & 0x0007;
    let pairwise = key_info & 0x0008 != 0;
    let install = key_info & 0x0040 != 0;
    let ack = key_info & 0x0080 != 0;
    let mic = key_info & 0x0100 != 0;
    let secure = key_info & 0x0200 != 0;
    let replay_counter = u64::from_be_bytes(eapol[9..17].try_into().ok()?);
    let key_data_len = usize::from(u16::from_be_bytes([eapol[97], eapol[98]]));
    if descriptor_version == 0 || replay_counter == 0 || packet_body_len != 95 + key_data_len {
        return None;
    }
    let class = match (pairwise, install, ack, mic, secure) {
        (true, false, true, false, false) if key_data_len == 0 => "authenticator_m1",
        (true, false, false, true, false) if key_data_len != 0 => "supplicant_m2",
        (true, true, true, true, true) if key_data_len != 0 => "authenticator_m3",
        (true, false, false, true, true) if key_data_len == 0 => "supplicant_m4",
        _ => "other_eapol_key",
    };
    Some((key_info, class))
}

#[doc(hidden)]
pub fn is_authenticator_m1(bytes: &[u8]) -> bool {
    classify_eapol_key(bytes).is_some_and(|(_, class)| class == "authenticator_m1")
}

impl LiveClientEffects {
    fn observe(&self, event: LiveClientEvent) {
        notify_observer(self.observer, event);
    }

    fn observe_stage(&self, message: &str) {
        self.observe(stage_event(message));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        selection: ClientTargetBssLease,
        target: [u8; 6],
        client: [u8; 6],
        rcpi: u8,
        dtim_period: u8,
        preauth_rates: Option<(u16, u16)>,
        observer: LiveClientObserver,
    ) -> (Self, LiveClientAuthorization) {
        let state = Arc::new(Mutex::new(LiveClientState {
            selection,
            channel: ClientChannelContext::default(),
        }));
        (
            Self {
                state: state.clone(),
                target,
                client,
                rcpi,
                dtim_period,
                preauth_rates,
                firmware: ClientFirmwareEffectsState::default(),
                peer_wcid: None,
                join_roc_generation: None,
                established_channel: None,
                join_roc_deadline: None,
                post_association_data_wait: None,
                eapol_start_deadline: None,
                eapol_start_emitted: false,
                target_beacon_tim: TargetBeaconTimTelemetry::default(),
                post_assoc_rx_ready_generation: None,
                observer,
                runtime_scan: None,
            },
            LiveClientAuthorization { state, target },
        )
    }

    pub fn set_preauth_rates(&mut self, rates: (u16, u16)) {
        self.preauth_rates = Some(rates);
    }

    fn acquire_join_roc(
        &mut self,
        io: &mut dyn Mt7921ClientIo,
        channel: mt7921_core::ClientChannelLease,
        duration_ms: u32,
    ) -> Result<(), zx::Status> {
        if self.join_roc_generation == Some(channel.generation) {
            if self
                .join_roc_deadline
                .is_some_and(|deadline| Instant::now() < deadline)
            {
                return Ok(());
            }
            self.abort_join_roc(io)?;
        }
        if self.join_roc_generation.is_some() {
            return Err(zx::Status::BAD_STATE);
        }
        // Linux programs the association chandef with CHANNEL_SWITCH
        // (mt7921_set_channel, CH_SWITCH_NORMAL) before mgd_prepare_tx's JOIN
        // ROC. Without it the radio stays in the scan-time
        // CH_SWITCH_SCAN_BYPASS_DPD form and only decodes basic-rate frames.
        if self.established_channel != Some(channel.channel) {
            match io.establish_client_channel(channel.channel) {
                Ok(()) => {
                    self.observe_stage(&format!(
                        "client_channel_switch reason=normal band={} primary={} center={} bandwidth={} center2={}",
                        channel.channel.band,
                        channel.channel.primary,
                        channel.channel.center,
                        channel.channel.bandwidth,
                        channel.channel.center2,
                    ));
                    self.established_channel = Some(channel.channel);
                }
                Err(zx::Status::NOT_SUPPORTED) => {
                    self.observe_stage("client_channel_switch result=unsupported");
                }
                Err(status) => {
                    self.observe_stage(&format!(
                        "client_channel_switch result=error status={status}"
                    ));
                    return Err(status);
                }
            }
        }
        // Linux JOIN ROC is always a 20 MHz transaction on the selected
        // primary channel, independent of the wider association chandef.
        let roc_channel = ClientPhysicalChannel {
            center: channel.channel.primary,
            bandwidth: 0,
            center2: 0,
            ..channel.channel
        };
        let max_interval_ms =
            match io.acquire_join_roc(roc_channel, channel.generation, duration_ms) {
                Ok(max_interval_ms) => max_interval_ms,
                Err(status) => {
                    if io.join_roc_active(channel.generation) {
                        self.join_roc_generation = Some(channel.generation);
                        self.abort_join_roc(io)?;
                    }
                    return Err(status);
                }
            };
        self.join_roc_generation = Some(channel.generation);
        self.join_roc_deadline =
            Some(Instant::now() + std::time::Duration::from_millis(u64::from(max_interval_ms)));
        Ok(())
    }

    fn abort_join_roc(&mut self, io: &mut dyn Mt7921ClientIo) -> Result<(), zx::Status> {
        let Some(generation) = self.join_roc_generation.take() else {
            return Ok(());
        };
        self.join_roc_deadline = None;
        io.abort_join_roc(generation).map_err(|status| {
            self.firmware.firmware_uncertain = true;
            status
        })
    }

    fn invalidate_association_rx(&mut self) {
        self.post_assoc_rx_ready_generation = None;
    }

    fn observe_target_beacon_tim(&mut self, bytes: &[u8]) {
        let Some(association) = self.firmware.association else {
            return;
        };
        let classification = classify_client_management_frame(bytes, self.client, self.target);
        let control = bytes
            .get(..2)
            .map(|value| u16::from_le_bytes([value[0], value[1]]))
            .unwrap_or(0);
        if control & 0x00fc != 0x0080
            || !classification.addr2_is_peer
            || !classification.addr3_is_bssid
        {
            return;
        }

        self.target_beacon_tim.beacon_count += 1;
        let beacon_count = self.target_beacon_tim.beacon_count;
        let elapsed_ms = self
            .post_association_data_wait
            .map(|started| started.elapsed().as_millis())
            .unwrap_or(0);
        let Some(fixed) = bytes.get(24..36) else {
            self.observe_stage(&format!(
                "target_beacon_tim beacon_count={beacon_count} elapsed_ms={elapsed_ms} normalized_aid={} tim_present=false tim_present_count={} parse=truncated_fixed_fields aid_buffered=unknown buffered_semantics=inconclusive",
                association.aid, self.target_beacon_tim.tim_present_count
            ));
            return;
        };
        let timestamp = u64::from_le_bytes(fixed[0..8].try_into().expect("fixed length"));
        let beacon_interval = u16::from_le_bytes([fixed[8], fixed[9]]);
        let capability = u16::from_le_bytes([fixed[10], fixed[11]]);
        match parse_target_beacon_tim(&bytes[36..], association.aid) {
            Ok(None) => self.observe_stage(&format!(
                "target_beacon_tim beacon_count={beacon_count} elapsed_ms={elapsed_ms} normalized_aid={} timestamp={timestamp} beacon_interval={beacon_interval} capability=0x{capability:04x} tim_present=false tim_present_count={} parse=valid aid_buffered=unknown buffered_semantics=inconclusive",
                association.aid, self.target_beacon_tim.tim_present_count
            )),
            Err(reason) => self.observe_stage(&format!(
                "target_beacon_tim beacon_count={beacon_count} elapsed_ms={elapsed_ms} normalized_aid={} timestamp={timestamp} beacon_interval={beacon_interval} capability=0x{capability:04x} tim_present=unknown tim_present_count={} parse=malformed reason={reason} aid_buffered=unknown buffered_semantics=inconclusive",
                association.aid, self.target_beacon_tim.tim_present_count
            )),
            Ok(Some(tim)) => {
                self.target_beacon_tim.tim_present_count += 1;
                if tim.aid_buffered {
                    self.target_beacon_tim.aid_buffered_true_count += 1;
                }
                let transition = match (
                    self.target_beacon_tim.previous_aid_buffered,
                    tim.aid_buffered,
                ) {
                    (Some(false), true) => "false_to_true",
                    (Some(true), false) => "true_to_false",
                    (None, true) => "initial_true",
                    (None, false) => "initial_false",
                    _ => "unchanged",
                };
                if tim.aid_buffered && self.target_beacon_tim.previous_aid_buffered != Some(true) {
                    self.target_beacon_tim.aid_buffered_true_transition_count += 1;
                }
                self.target_beacon_tim.previous_aid_buffered = Some(tim.aid_buffered);
                self.observe_stage(&format!(
                    "target_beacon_tim beacon_count={beacon_count} elapsed_ms={elapsed_ms} normalized_aid={} timestamp={timestamp} beacon_interval={beacon_interval} capability=0x{capability:04x} tim_present=true tim_present_count={} parse=valid dtim_count={} dtim_period={} bitmap_control=0x{:02x} bitmap_offset={} partial_virtual_bitmap_sha256={} multicast_buffered={} aid_buffered={} aid_buffered_true_count={} aid_buffered_transition={transition} aid_buffered_true_transition_count={} buffered_semantics=ap_queued_unicast_for_aid_not_traffic_type",
                    association.aid,
                    self.target_beacon_tim.tim_present_count,
                    tim.dtim_count,
                    tim.dtim_period,
                    tim.bitmap_control,
                    tim.bitmap_offset,
                    sha256_hex(tim.partial_virtual_bitmap),
                    tim.multicast_buffered,
                    tim.aid_buffered,
                    self.target_beacon_tim.aid_buffered_true_count,
                    self.target_beacon_tim.aid_buffered_true_transition_count,
                ));
            }
        }
    }
}

#[cfg(test)]
mod production_effects_tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        static REENTRANT_AUTHORIZATION: RefCell<Option<LiveClientAuthorization>> =
            const { RefCell::new(None) };
    }

    fn reentrant_observer(_: &LiveClientEvent) {
        REENTRANT_AUTHORIZATION.with(|slot| {
            let mut slot = slot.borrow_mut();
            let authorization = slot.as_mut().expect("authorization retained");
            let channel = ChannelNumber {
                band: WlanBand::FiveGhz,
                number: 36,
            };
            authorization
                .mark_rate_power_ready(
                    [1, 2, 3, 4, 5, 6],
                    channel,
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    ChannelNumber {
                        number: 0,
                        ..channel
                    },
                )
                .unwrap();
        });
    }

    #[derive(Default)]
    struct ChannelIo(Vec<ClientPhysicalChannel>);

    impl Mt7921ClientIo for ChannelIo {
        fn submit_uni(&mut self, _: u8, _: &[u8]) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        fn submit_edca(&mut self, _: &[u8]) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        fn submit_ce_no_ack(&mut self, _: &[u8]) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        fn establish_client_channel(
            &mut self,
            channel: ClientPhysicalChannel,
        ) -> Result<(), zx::Status> {
            self.0.push(channel);
            Ok(())
        }
        fn transmit_client(
            &mut self,
            _: &[u8],
            _: fidl_softmac::WlanTxInfoFlags,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }
        fn next_client_rx(&mut self) -> Result<Option<ClientRxFrame>, zx::Status> {
            Ok(None)
        }
    }

    #[test]
    fn runtime_scan_restores_operating_chandef_after_other_channel_before_reconnect() {
        let target = [1, 2, 3, 4, 5, 6];
        let channel = ChannelNumber {
            band: WlanBand::FiveGhz,
            number: 149,
        };
        let physical = client_physical_channel(
            channel,
            fidl_ieee80211::ChannelBandwidth::Cbw80,
            ChannelNumber {
                number: 0,
                ..channel
            },
        )
        .unwrap();
        let selection = ClientTargetBssLease::retain(ClientScanEvidence {
            scan_id: 1,
            observation_generation: 1,
            observation_timestamp_nanos: 1,
            bssid: target,
            channel: physical,
        })
        .unwrap();
        let (mut effects, authorization) = LiveClientEffects::new(
            selection,
            target,
            [7, 8, 9, 10, 11, 12],
            100,
            2,
            None,
            LiveClientObserver::default(),
        );
        effects
            .set_channel(
                channel,
                fidl_ieee80211::ChannelBandwidth::Cbw80,
                ChannelNumber {
                    number: 0,
                    ..channel
                },
            )
            .unwrap();
        effects.revoke_scan();
        effects.begin_passive_scan(2, &[channel]).unwrap();
        effects
            .observe_passive_scan(
                2,
                &fuchsia_softmac_port::ScanObservation {
                    kind: fuchsia_softmac_port::AdvertisementKind::Beacon,
                    timestamp_nanos: 2,
                    bss: fidl_ieee80211::BssDescription {
                        bssid: target,
                        bss_type: fidl_ieee80211::BssType::Infrastructure,
                        beacon_period: 100,
                        capability_info: 0x11,
                        ies: vec![],
                        primary: channel,
                        bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw80,
                        vht_secondary_80_channel: ChannelNumber {
                            number: 0,
                            ..channel
                        },
                        rssi_dbm: -40,
                        snr_db: 20,
                    },
                },
            )
            .unwrap();
        let other_channel = ChannelNumber {
            band: WlanBand::FiveGhz,
            number: 36,
        };
        effects
            .observe_passive_scan(
                2,
                &fuchsia_softmac_port::ScanObservation {
                    kind: fuchsia_softmac_port::AdvertisementKind::Beacon,
                    timestamp_nanos: 3,
                    bss: fidl_ieee80211::BssDescription {
                        bssid: [9, 8, 7, 6, 5, 4],
                        bss_type: fidl_ieee80211::BssType::Infrastructure,
                        beacon_period: 100,
                        capability_info: 0x11,
                        ies: vec![],
                        primary: other_channel,
                        bandwidth: fidl_ieee80211::ChannelBandwidth::Cbw20,
                        vht_secondary_80_channel: ChannelNumber {
                            number: 0,
                            ..other_channel
                        },
                        rssi_dbm: -30,
                        snr_db: 25,
                    },
                },
            )
            .unwrap();
        let mut io = ChannelIo::default();
        effects.complete_passive_scan(2, true, &mut io).unwrap();
        assert_eq!(io.0, [physical]);

        let state = authorization.state.lock().unwrap();
        let lease = state.channel.authorized_channel().unwrap();
        assert!(state.selection.permits_join(target, lease));
    }

    #[test]
    fn authorization_handle_rejects_a_bssid_other_than_its_fixed_target() {
        let target = [1, 2, 3, 4, 5, 6];
        let (_effects, mut authorization) = LiveClientEffects::new(
            ClientTargetBssLease::default(),
            target,
            [7, 8, 9, 10, 11, 12],
            100,
            2,
            None,
            LiveClientObserver::default(),
        );
        let channel = ChannelNumber {
            band: WlanBand::FiveGhz,
            number: 36,
        };
        let secondary = ChannelNumber {
            number: 0,
            ..channel
        };
        assert!(
            authorization
                .mark_rate_power_ready(
                    [9; 6],
                    channel,
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    secondary
                )
                .is_err()
        );
        assert!(
            authorization
                .authorize_sae(
                    [9; 6],
                    channel,
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    secondary
                )
                .is_err()
        );
    }

    #[test]
    fn every_event_channel_enforces_the_documented_bound() {
        for direct_json in [false, true] {
            let event =
                LiveClientEvent::new("x".repeat(LIVE_CLIENT_EVENT_MAX_BYTES + 1), direct_json);
            assert_eq!(event.message().len(), LIVE_CLIENT_EVENT_MAX_BYTES);
            assert_eq!(event.direct_json(), direct_json);
        }
        let frame = vec![0u8; 2304];
        assert!(auth_frame_event("rx", &frame).message().len() <= LIVE_CLIENT_EVENT_MAX_BYTES);
    }

    #[test]
    fn observer_panics_cannot_change_effects_control_flow() {
        fn panic_observer(_: &LiveClientEvent) {
            panic!("observer failure");
        }
        let (mut effects, authorization) = LiveClientEffects::new(
            ClientTargetBssLease::default(),
            [1, 2, 3, 4, 5, 6],
            [7, 8, 9, 10, 11, 12],
            100,
            2,
            None,
            LiveClientObserver::new(panic_observer),
        );
        let channel = ChannelNumber {
            band: WlanBand::FiveGhz,
            number: 36,
        };
        let secondary = ChannelNumber {
            number: 0,
            ..channel
        };
        effects
            .set_channel(channel, fidl_ieee80211::ChannelBandwidth::Cbw20, secondary)
            .unwrap();
        assert_eq!(
            effects.ensure_channel(channel, fidl_ieee80211::ChannelBandwidth::Cbw20, secondary,),
            Ok(ClientChannelEnsure::Current)
        );
        assert!(
            authorization
                .state
                .lock()
                .unwrap()
                .channel
                .authorized_channel()
                .is_err()
        );
    }

    #[test]
    fn observer_emission_is_outside_authorization_mutex() {
        let target = [1, 2, 3, 4, 5, 6];
        let channel = ChannelNumber {
            band: WlanBand::FiveGhz,
            number: 36,
        };
        let physical = client_physical_channel(
            channel,
            fidl_ieee80211::ChannelBandwidth::Cbw20,
            ChannelNumber {
                number: 0,
                ..channel
            },
        )
        .unwrap();
        let selection = ClientTargetBssLease::retain(mt7921_core::ClientScanEvidence {
            scan_id: 1,
            observation_generation: 1,
            observation_timestamp_nanos: 1,
            bssid: target,
            channel: physical,
        })
        .unwrap();
        let (mut effects, authorization) = LiveClientEffects::new(
            selection,
            target,
            [7, 8, 9, 10, 11, 12],
            100,
            2,
            None,
            LiveClientObserver::new(reentrant_observer),
        );
        effects
            .set_channel(
                channel,
                fidl_ieee80211::ChannelBandwidth::Cbw20,
                ChannelNumber {
                    number: 0,
                    ..channel
                },
            )
            .unwrap();
        REENTRANT_AUTHORIZATION.with(|slot| *slot.borrow_mut() = Some(authorization));
        assert_eq!(
            effects.ensure_channel(
                channel,
                fidl_ieee80211::ChannelBandwidth::Cbw20,
                ChannelNumber {
                    number: 0,
                    ..channel
                },
            ),
            Ok(ClientChannelEnsure::Current)
        );
        let mut authorization =
            REENTRANT_AUTHORIZATION.with(|slot| slot.borrow_mut().take().unwrap());
        assert_eq!(
            authorization
                .authorize_sae(
                    target,
                    channel,
                    fidl_ieee80211::ChannelBandwidth::Cbw20,
                    ChannelNumber {
                        number: 0,
                        ..channel
                    },
                )
                .unwrap(),
            1
        );
        assert!(
            authorization
                .state
                .lock()
                .unwrap()
                .channel
                .authorized_channel()
                .is_ok()
        );
    }
}

impl Mt7921ClientEffects for LiveClientEffects {
    fn association_capability_transformation(
        &mut self,
        evidence: &AssociationCapabilityTransformation,
        final_frame: &[u8],
    ) {
        let mut normalized = final_frame.to_vec();
        normalized[22..24].fill(0);
        self.observe_stage(&format!(
            "association_capability_transformation source={} contract={} base_ht_sha256={} base_vht_sha256={} authoritative_ht_sha256={} authoritative_vht_sha256={} final_ht_sha256={} final_vht_sha256={} normalized_frame_sha256={}",
            ASSOCIATION_CAPABILITY_INPUT_SOURCE,
            "device+pinned-regdb-authoritative-association-v2",
            sha256_hex(&evidence.base_ht),
            sha256_hex(&evidence.base_vht),
            sha256_hex(&evidence.authoritative_ht),
            sha256_hex(&evidence.authoritative_vht),
            sha256_hex(&evidence.final_ht),
            sha256_hex(&evidence.final_vht),
            sha256_hex(&normalized),
        ));
    }

    fn prepare_runtime_handoff(&mut self) -> ClientRuntimeScanState {
        self.invalidate_association_rx();
        self.state.lock().unwrap().channel.revoke_authorization();
        ClientRuntimeScanState::ExternalSelection
    }

    fn revoke_scan(&mut self) {
        self.runtime_scan = None;
        self.invalidate_association_rx();
        let mut state = self.state.lock().unwrap();
        state.selection.invalidate();
        state.channel.revoke_authorization();
    }
    fn revoke_lifecycle(&mut self) {
        self.invalidate_association_rx();
        *self.state.lock().unwrap() = LiveClientState::default();
        // Device reset/stop owns transport containment. Do not issue new DMA
        // after lifecycle revocation; forget only after the owner contained it.
        self.firmware = ClientFirmwareEffectsState::default();
        self.post_association_data_wait = None;
        self.eapol_start_deadline = None;
        self.eapol_start_emitted = false;
    }
    fn ensure_channel(
        &self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: fidl_ieee80211::ChannelNumber,
    ) -> Result<ClientChannelEnsure, zx::Status> {
        let requested = client_physical_channel(primary, bandwidth, secondary)?;
        let state = self.state.lock().unwrap();
        let ensure = state.channel.ensure_channel(requested);
        drop(state);
        self.observe_stage(&format!(
            "channel_context_ensure result={} requested_band={} requested_primary={} protocol_width={bandwidth:?} secondary80={} current={:?}",
            if matches!(ensure, ClientPhysicalChannelEnsure::Current(_)) {
                "current"
            } else {
                "transition_required"
            },
            requested.band,
            requested.primary,
            secondary.number,
            match ensure {
                ClientPhysicalChannelEnsure::Current(current) => Some(current),
                ClientPhysicalChannelEnsure::TransitionRequired { current, .. } => current,
            }
        ));
        Ok(match ensure {
            ClientPhysicalChannelEnsure::Current(_) => ClientChannelEnsure::Current,
            ClientPhysicalChannelEnsure::TransitionRequired { .. } => {
                ClientChannelEnsure::TransitionRequired
            }
        })
    }
    fn set_channel(
        &mut self,
        primary: fidl_ieee80211::ChannelNumber,
        bandwidth: fidl_ieee80211::ChannelBandwidth,
        secondary: fidl_ieee80211::ChannelNumber,
    ) -> Result<(), zx::Status> {
        let physical = client_physical_channel(primary, bandwidth, secondary)?;
        self.invalidate_association_rx();
        let mut state = self.state.lock().unwrap();
        state.selection.channel_changed(physical);
        state
            .channel
            .establish_channel(physical)
            .map_err(|_| zx::Status::NO_RESOURCES)?;
        Ok(())
    }
    fn join_bss(&mut self, request: &fidl_driver::JoinBssRequest) -> Result<(), zx::Status> {
        let bssid = request.bssid.ok_or(zx::Status::INVALID_ARGS)?;
        if bssid != self.target
            || request.bss_type != Some(fidl_ieee80211::BssType::Infrastructure)
            || request.remote != Some(true)
        {
            return Err(zx::Status::INVALID_ARGS);
        }
        let state = self.state.lock().unwrap();
        let channel = state
            .channel
            .authorized_channel()
            .map_err(|_| zx::Status::BAD_STATE)?;
        if !state.selection.permits_join(bssid, channel) {
            return Err(zx::Status::ACCESS_DENIED);
        }
        drop(state);
        self.firmware
            .bind_join(
                bssid,
                channel,
                request.beacon_period.ok_or(zx::Status::INVALID_ARGS)?,
                self.dtim_period,
            )
            .map_err(|_| zx::Status::BAD_STATE)
    }
    fn send_wlan_frame(
        &mut self,
        bytes: &[u8],
        flags: fidl_softmac::WlanTxInfoFlags,
        io: &mut dyn Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        let control = bytes
            .get(..2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .ok_or(zx::Status::INVALID_ARGS)?;
        let management = control & 0x000c == 0;
        let sae = control & 0x00fc == 0x00b0;
        let state = self.state.lock().unwrap();
        let channel = state.channel.authorized_channel();
        if channel.is_err()
            || bytes.get(4..10) != Some(&self.target)
            || bytes.get(10..16) != Some(&self.client)
            || (sae && bytes.get(16..22) != Some(&self.target))
        {
            return Err(zx::Status::ACCESS_DENIED);
        }
        let channel = *channel.as_ref().expect("authorized channel was checked");
        drop(state);
        if management {
            // Diagnostic: identify management frames the SME emits, especially any
            // deauth/disassoc (subtype 12/10) the SME sends to abort the connect
            // after the 4-way handshake -- its reason code says *why* it gave up.
            self.observe_stage(&format!(
                "client_management_tx_intent subtype={} len={} sae={sae} associated={} reason_or_status={:?}",
                (control >> 4) & 0xf,
                bytes.len(),
                self.firmware.association_generation.is_some(),
                bytes.get(24..26).map(|b| u16::from_le_bytes([b[0], b[1]])),
            ));
            if (sae && bytes.get(24..26) != Some(&[3, 0]))
                || flags.contains(fidl_softmac::WlanTxInfoFlags::PROTECTED)
            {
                return Err(zx::Status::ACCESS_DENIED);
            }
            if sae {
                self.observe(auth_frame_event("tx", bytes));
            }
            if sae && bytes.get(26..28) == Some(&[1, 0]) {
                self.observe(
                    validate_sae_commit(bytes).map_err(|_| zx::Status::IO_DATA_INTEGRITY)?,
                );
            }
            if control & 0x00fc == 0 {
                let capability = bytes
                    .get(24..26)
                    .map(|field| u16::from_le_bytes([field[0], field[1]]));
                let listen_interval = bytes
                    .get(26..28)
                    .map(|field| u16::from_le_bytes([field[0], field[1]]));
                let sequence = bytes
                    .get(22..24)
                    .map(|field| u16::from_le_bytes([field[0], field[1]]) >> 4);
                self.observe_stage(&format!(
                    "association_request_structure capability={} listen_interval={} retry={} sequence={} ie_id_lengths={} fixed_fields_complete={}",
                    capability
                        .map_or_else(|| "unknown".to_string(), |value| format!("0x{value:04x}")),
                    listen_interval
                        .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                    control & 0x0800 != 0,
                    sequence.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                    management_ie_id_lengths(bytes, 28),
                    capability.is_some() && listen_interval.is_some() && sequence.is_some(),
                ));
            }
            if sae && self.firmware.preauth_peer.is_none() {
                let peer_wcid = self
                    .firmware
                    .allocate_peer_wcid()
                    .map_err(|_| zx::Status::NO_RESOURCES)?;
                self.peer_wcid = Some(peer_wcid);
                if self
                    .firmware
                    .prepare_preauth_peer(
                        LegacyWmeAssociation {
                            bss_index: 0,
                            peer_wcid,
                            aid: 0,
                            peer: self.target,
                            rcpi: self.rcpi,
                            basic_rates: self.preauth_rates.ok_or(zx::Status::BAD_STATE)?.0,
                            legacy_rates: self.preauth_rates.ok_or(zx::Status::BAD_STATE)?.1,
                            ht_cap: None,
                            vht_cap: None,
                            bandwidth: 0,
                            negotiated_qos: false,
                            mfp_required: false,
                        },
                        channel,
                        |cid, command| {
                            io.submit_uni(cid, command)
                                .map_err(|status| status.to_string())
                        },
                    )
                    .is_err()
                {
                    self.peer_wcid = None;
                    return Err(zx::Status::IO);
                }
                self.observe_stage(&format!(
                    "firmware_wcid_stage stage=preauth peer_wcid={} sta_state=none aid=0 peer_identity=true keys=false port_open=false",
                    peer_wcid.get()
                ));
            }
            if control & 0x00fc == 0
                && (bytes.get(16..22) != Some(&self.target) || self.peer_wcid.is_none())
            {
                return Err(zx::Status::ACCESS_DENIED);
            }
            let roc_duration_ms = if sae { 2_000 } else { 1_000 };
            // JOIN ROC (Linux mgd_prepare_tx) is a pre-association primitive: it
            // parks the radio on the target channel to exchange auth/assoc frames
            // before we belong to the BSS. Once associated we are already parked on
            // the operating channel (CH_SWITCH_NORMAL was programmed during the
            // association-request ROC), and the firmware refuses a fresh JOIN ROC in
            // the associated state -- the grant event never arrives and the acquire
            // times out, which would abort the post-PTK handshake-tail management TX
            // and stall the connect (controlled port never opens, no GTK). So only
            // acquire a ROC while still pre-association; afterwards transmit directly
            // on the already-established operating channel.
            if self.firmware.association_generation.is_none() {
                self.acquire_join_roc(io, channel, roc_duration_ms)?;
            } else {
                self.observe_stage(&format!(
                    "join_roc_skipped reason=associated generation={:?} on_established_channel={} duration_ms={roc_duration_ms}",
                    self.firmware.association_generation,
                    self.established_channel == Some(channel.channel),
                ));
            }
            if let Err(status) = io.transmit_client(bytes, flags) {
                let _ = self.abort_join_roc(io);
                return Err(status);
            }
            if sae {
                self.observe_stage(match bytes.get(26..28) {
                    Some([1, 0]) => "sae_commit_tx_terminal_success sme_callback=success",
                    Some([2, 0]) => "sae_confirm_tx_terminal_success sme_callback=success",
                    _ => "sae_protocol_tx_terminal_success sme_callback=success",
                });
            }
            return Ok(());
        }
        let eapol = is_anchored_eapol_data(bytes);
        self.firmware.tx_generation(eapol).map_err(|reason| {
            self.observe_stage(&format!(
                "client_data_tx_blocked reason=tx_generation eapol={eapol} detail={reason}"
            ));
            zx::Status::ACCESS_DENIED
        })?;
        let association = self
            .firmware
            .association
            .filter(|association| {
                Some(association.peer_wcid) == self.peer_wcid && association.peer == self.target
            })
            .ok_or_else(|| {
                self.observe_stage("client_data_tx_blocked reason=association_identity_mismatch");
                zx::Status::BAD_STATE
            })?;
        let to_ds = control & 0x0100 != 0;
        let from_ds = control & 0x0200 != 0;
        let qos = (control >> 4) & 8 != 0;
        let qos_offset = if to_ds && from_ds { 30 } else { 24 };
        let tid = if qos {
            bytes
                .get(qos_offset)
                .map(|value| value & 15)
                .ok_or(zx::Status::INVALID_ARGS)?
        } else {
            0
        };
        let qos_frame;
        let (bytes, control, qos, tid): (&[u8], u16, bool, u8) = if eapol
            && !qos
            && association.negotiated_qos
        {
            // The Fuchsia MLME deliberately emits EAPOL as a non-QoS data
            // frame (bound.rs send_eapol_frame). Linux's mac80211 always
            // builds control-port EAPOL as a QoS data frame with TID 7 on
            // a WME association (ieee80211_build_hdr with skb priority 7),
            // so promote the MPDU here to keep the on-air bytes equal to
            // the oracle instead of rejecting the frame.
            if to_ds && from_ds {
                return Err(zx::Status::INVALID_ARGS);
            }
            let qos_control = control | 0x0080;
            let mut frame = Vec::with_capacity(bytes.len() + 2);
            frame.extend_from_slice(&qos_control.to_le_bytes());
            frame.extend_from_slice(bytes.get(2..24).ok_or(zx::Status::INVALID_ARGS)?);
            frame.extend_from_slice(&[7, 0]);
            frame.extend_from_slice(bytes.get(24..).ok_or(zx::Status::INVALID_ARGS)?);
            self.observe_stage(&format!(
                "client_eapol_qos_promotion source=mlme_non_qos_data result=qos_data_tid7 linux_reference=mac80211_control_port frame_len_before={} frame_len_after={}",
                bytes.len(),
                frame.len()
            ));
            qos_frame = frame;
            (qos_frame.as_slice(), qos_control, true, 7)
        } else if eapol && qos != association.negotiated_qos {
            self.observe_stage(&format!(
                "client_data_tx_blocked reason=eapol_qos_mismatch qos={qos} negotiated_qos={}",
                association.negotiated_qos
            ));
            return Err(zx::Status::BAD_STATE);
        } else {
            (bytes, control, qos, tid)
        };
        if qos && !self.firmware.qos_tx_ready() {
            self.observe_stage("client_data_tx_blocked reason=edca_not_programmed");
            return Err(zx::Status::BAD_STATE);
        }
        self.observe_stage(&format!(
            "client_data_tx_public fc=0x{control:04x} protected={} to_ds={to_ds} qos={qos} tid={tid} frame_len={} wcid={} qidx={} rate={} addr1_is_bssid={} addr2_is_sta={} addr3_is_pae_group={} ack_ra_unicast={} sequence_owner=hardware fcs_owner=hardware",
            control & 0x4000 != 0,
            bytes.len(),
            association.peer_wcid.get(),
            if eapol { 3 } else { 1 },
            if eapol { "ofdm6" } else { "auto" },
            bytes.get(4..10) == Some(&self.target),
            bytes.get(10..16) == Some(&self.client),
            bytes.get(16..22) == Some(&[0x01, 0x80, 0xc2, 0, 0, 3]),
            bytes.get(4).is_some_and(|byte| byte & 1 == 0),
        ));
        if !eapol && !flags.contains(fidl_softmac::WlanTxInfoFlags::PROTECTED) {
            self.observe_stage(&format!(
                "client_data_tx_blocked reason=unprotected_non_eapol_data fc=0x{control:04x} flags={flags:?}"
            ));
            return Err(zx::Status::ACCESS_DENIED);
        }
        io.transmit_client(bytes, flags)
    }
    fn install_key(
        &mut self,
        configuration: &fidl_softmac::WlanKeyConfiguration,
        io: &mut dyn Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        if configuration.protection != Some(fidl_softmac::WlanProtection::RxTx)
            || configuration.cipher_oui != Some([0, 15, 172])
        {
            return Err(zx::Status::INVALID_ARGS);
        }
        let association = self.firmware.association.ok_or(zx::Status::BAD_STATE)?;
        let key = configuration
            .key
            .as_deref()
            .ok_or(zx::Status::INVALID_ARGS)?;
        let key_id = configuration.key_idx.ok_or(zx::Status::INVALID_ARGS)?;
        let key_type = configuration.key_type.ok_or(zx::Status::INVALID_ARGS)?;
        // Diagnostic: log every SetKeys the SME issues, BEFORE the accept guards,
        // so a GTK/IGTK rejected by key_id/cipher/peer bounds is visible (the
        // guard-reject path returns before the *_installed log otherwise).
        self.observe_stage(&format!(
            "install_key_intent key_type={:?} key_id={key_id} cipher_type={:?} cipher_oui={:?} peer_broadcast={} rsc={:?} key_len={}",
            key_type,
            configuration.cipher_type,
            configuration.cipher_oui,
            configuration.peer_addr == Some([0xff; 6]),
            configuration.rsc,
            key.len(),
        ));
        let io = std::cell::RefCell::new(&mut *io);
        let result = match key_type {
            fidl_ieee80211::KeyType::Pairwise
                if configuration.peer_addr == Some(association.peer)
                    && key_id == 0
                    && configuration.cipher_type == Some(4) =>
            {
                self.firmware.install_ptk(
                    key,
                    configuration.rsc.unwrap_or(0),
                    |cid, command| {
                        io.borrow_mut()
                            .submit_uni(cid, command)
                            .map_err(|status| status.to_string())
                    },
                    |command| {
                        io.borrow_mut()
                            .submit_ce_no_ack(command)
                            .map_err(|status| status.to_string())
                    },
                )
            }
            fidl_ieee80211::KeyType::Group
                if configuration.peer_addr == Some([0xff; 6])
                    && (1..=3).contains(&key_id)
                    && configuration.cipher_type == Some(4) =>
            {
                self.firmware.install_gtk(
                    key_id,
                    key,
                    configuration.rsc.unwrap_or(0),
                    |cid, command| {
                        io.borrow_mut()
                            .submit_uni(cid, command)
                            .map_err(|status| status.to_string())
                    },
                    |command| {
                        io.borrow_mut()
                            .submit_ce_no_ack(command)
                            .map_err(|status| status.to_string())
                    },
                )
            }
            fidl_ieee80211::KeyType::Igtk
                if configuration.peer_addr == Some([0xff; 6])
                    && (4..=5).contains(&key_id)
                    && configuration.cipher_type == Some(6) =>
            {
                self.firmware.install_igtk(
                    key_id,
                    key,
                    |cid, command| {
                        io.borrow_mut()
                            .submit_uni(cid, command)
                            .map_err(|status| status.to_string())
                    },
                    |command| {
                        io.borrow_mut()
                            .submit_ce_no_ack(command)
                            .map_err(|status| status.to_string())
                    },
                )
            }
            _ => return Err(zx::Status::INVALID_ARGS),
        };
        result.map_err(|_| zx::Status::IO)?;
        self.eapol_start_deadline = None;
        self.observe_stage(match key_type {
            fidl_ieee80211::KeyType::Pairwise => "traffic_key_ptk_installed=true",
            fidl_ieee80211::KeyType::Group => "traffic_key_gtk_installed=true",
            fidl_ieee80211::KeyType::Igtk => "traffic_key_igtk_installed=true",
            _ => "traffic_key_installed=true",
        });
        Ok(())
    }
    fn notify_association_complete(
        &mut self,
        configuration: &fidl_softmac::WlanAssociationConfig,
        io: &mut dyn Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        let result = (|| {
            self.post_assoc_rx_ready_generation = None;
            let Some(peer) = configuration.bssid else {
                self.observe_stage(
                    "association_config_validation result=invalid clause=missing_bssid",
                );
                return Err(zx::Status::INVALID_ARGS);
            };
            let Some(aid) = configuration.aid else {
                self.observe_stage(
                    "association_config_validation result=invalid clause=missing_aid",
                );
                return Err(zx::Status::INVALID_ARGS);
            };
            // Client MLME masks the five reserved on-wire AID bits before this
            // FIDL boundary. Requiring their raw 0xc000 form here rejects every
            // successful infrastructure association before firmware activation.
            if !(1..=2007).contains(&aid) {
                self.observe_stage(&format!(
                    "association_config_validation result=invalid clause=normalized_aid_range aid={aid}"
                ));
                return Err(zx::Status::INVALID_ARGS);
            }
            if peer != self.target {
                self.observe_stage(
                    "association_config_validation result=denied clause=foreign_bssid",
                );
                return Err(zx::Status::ACCESS_DENIED);
            }
            let negotiated_qos = configuration.qos.unwrap_or(false);
            if negotiated_qos != configuration.wmm_params.is_some() {
                self.observe_stage(
                    "association_config_validation result=invalid clause=wmm_negotiation",
                );
                return Err(zx::Status::INVALID_ARGS);
            }
            let primary = configuration.primary.ok_or(zx::Status::INVALID_ARGS)?;

            let band = match primary.band {
                fidl_ieee80211::WlanBand::TwoGhz => 0,
                fidl_ieee80211::WlanBand::FiveGhz => 1,
                _ => return Err(zx::Status::NOT_SUPPORTED),
            };
            let (basic_rates, legacy_rates) = linux_legacy_rate_context_reference(
                band,
                configuration
                    .rates
                    .as_deref()
                    .ok_or(zx::Status::INVALID_ARGS)?,
            )
            .map_err(|_| zx::Status::INVALID_ARGS)?;
            let ht_cap = configuration.ht_cap.map(|cap| cap.bytes);
            let vht_cap = configuration.vht_cap.map(|cap| cap.bytes);
            if vht_cap.is_some() && ht_cap.is_none() {
                return Err(zx::Status::INVALID_ARGS);
            }
            let bandwidth = match configuration.bandwidth {
                Some(fidl_ieee80211::ChannelBandwidth::Cbw20) => 0,
                Some(fidl_ieee80211::ChannelBandwidth::Cbw40)
                | Some(fidl_ieee80211::ChannelBandwidth::Cbw40Below) => 1,
                Some(fidl_ieee80211::ChannelBandwidth::Cbw80) => 2,
                Some(fidl_ieee80211::ChannelBandwidth::Cbw160)
                | Some(fidl_ieee80211::ChannelBandwidth::Cbw80P80) => 3,
                _ => return Err(zx::Status::INVALID_ARGS),
            };
            self.observe_stage(&format!(
                "e2e81_linux_sta_context source=association_config basic_rates={basic_rates:#06x} legacy_rates={legacy_rates:#06x} ht_present={} vht_present={} he_present=false he_reason=pinned_api_omission bandwidth={bandwidth} qidx_mapping=3_minus_mac80211_ac tid7_ac=vo qidx=3",
                ht_cap.is_some(),
                vht_cap.is_some(),
            ));
            self.observe_stage(&format!(
                "association_config_validation result=pass bssid_match=true normalized_aid={aid} keys=false port_open=false protected_management=closed"
            ));
            let channel = self
                .state
                .lock()
                .unwrap()
                .channel
                .authorized_channel()
                .map_err(|_| zx::Status::BAD_STATE)?;
            let peer_wcid = self.peer_wcid.ok_or(zx::Status::BAD_STATE)?;
            let io = std::cell::RefCell::new(&mut *io);
            // Keep the core state locally so the Linux-proven post-BSS/pre-STA
            // callback can pump RX through the remaining LiveClientEffects.
            // Restore it before propagating every result.
            let mut firmware = std::mem::take(&mut self.firmware);
            let associate_result = firmware.associate(
                LegacyWmeAssociation {
                    bss_index: 0,
                    peer_wcid,
                    aid,
                    peer,
                    rcpi: self.rcpi,
                    basic_rates,
                    legacy_rates,
                    ht_cap,
                    vht_cap,
                    bandwidth,
                    negotiated_qos,
                    // This FIDL association seam does not carry RSN MFP
                    // negotiation. IGTK installation remains supported but
                    // cannot become a mandatory readiness predicate here.
                    mfp_required: false,
                },
                channel,
                |cid, command| {
                    io.borrow_mut()
                        .submit_uni(cid, command)
                        .map_err(|status| status.to_string())
                },
                || Ok(()),
            );
            self.firmware = firmware;
            associate_result.map_err(|_| zx::Status::IO)?;
            if let Some(wmm) = configuration.wmm_params {
                let convert = |ac: fidl_driver::WlanWmmAccessCategoryParameters| {
                    if ac.ecw_min > 14 || ac.ecw_max > 14 || ac.ecw_max < ac.ecw_min {
                        return Err(zx::Status::INVALID_ARGS);
                    }
                    Ok(ClientEdcaAc {
                        cw_min: (1u16 << ac.ecw_min) - 1,
                        cw_max: (1u16 << ac.ecw_max) - 1,
                        txop: ac.txop_limit,
                        aifs: u16::from(ac.aifsn),
                        acm: ac.acm,
                    })
                };
                let params = ClientEdcaParameters {
                    ac: [
                        convert(wmm.ac_vo_params)?,
                        convert(wmm.ac_vi_params)?,
                        convert(wmm.ac_be_params)?,
                        convert(wmm.ac_bk_params)?,
                    ],
                };
                if let Err(error) = self.firmware.program_edca(params, |command| {
                    io.borrow_mut()
                        .submit_edca(command)
                        .map_err(|status| status.to_string())
                }) {
                    self.observe_stage(&format!("wmm_edca_program result=error reason={error}"));
                    let _ = self.firmware.teardown(
                        |cid, command| {
                            io.borrow_mut()
                                .submit_uni(cid, command)
                                .map_err(|status| status.to_string())
                        },
                        |_| Ok(()),
                    );
                    return Err(zx::Status::IO);
                }
                self.observe_stage(&format!(
                    "wmm_edca_program stage=associated result=complete completion=true readback=transport_owned bss=0 wmm=0 ac_vo=aifs{},cwmin{},cwmax{},txop{},acm{} ac_vi=aifs{},cwmin{},cwmax{},txop{},acm{} ac_be=aifs{},cwmin{},cwmax{},txop{},acm{} ac_bk=aifs{},cwmin{},cwmax{},txop{},acm{} tid7_ac=vo qidx3_programmed=true data_ring=0 shared_with_management=true",
                    params.ac[0].aifs,
                    params.ac[0].cw_min,
                    params.ac[0].cw_max,
                    params.ac[0].txop,
                    params.ac[0].acm,
                    params.ac[1].aifs,
                    params.ac[1].cw_min,
                    params.ac[1].cw_max,
                    params.ac[1].txop,
                    params.ac[1].acm,
                    params.ac[2].aifs,
                    params.ac[2].cw_min,
                    params.ac[2].cw_max,
                    params.ac[2].txop,
                    params.ac[2].acm,
                    params.ac[3].aifs,
                    params.ac[3].cw_min,
                    params.ac[3].cw_max,
                    params.ac[3].txop,
                    params.ac[3].acm,
                ));
            }
            self.firmware
                .complete_post_assoc_interface(
                    |cid, command| {
                        io.borrow_mut()
                            .submit_uni(cid, command)
                            .map_err(|status| status.to_string())
                    },
                    |command| {
                        io.borrow_mut()
                            .submit_ce_no_ack(command)
                            .map_err(|status| status.to_string())
                    },
                )
                .map_err(|error| {
                    self.observe_stage(&format!(
                        "post_assoc_interface_wcid result=error wcid=19 reason={error}"
                    ));
                    zx::Status::IO
                })?;
            self.abort_join_roc(&mut **io.borrow_mut())?;
            self.observe_stage(
                "join_roc_lifecycle phase=association_complete abort=after_post_assoc_tail before_m1",
            );
            self.observe_stage(
                "post_assoc_interface_wcid result=complete wcid=19 operation=reset_and_set tlvs=generic,rx,hdr_trans linux_order=after_edca before_beacon_filter data_tx_gate=closed",
            );
            self.observe_stage(&format!(
                "post_assoc_bss_updates result=complete order=RLM-before-M1-pump-and-STA,BCNFT-disabled,SET_RXFILTER beacon_interval={} dtim={} rx_filter=drop_other_beacon rx_filter_ack=not_requested_linux connection_monitor=host firmware_beacon_loss_route=unimplemented channel={} center={} bandwidth={} data_tx_gate=open",
                self.firmware.joined.expect("join retained").beacon_interval,
                self.dtim_period,
                channel.channel.primary,
                channel.channel.center,
                channel.channel.bandwidth,
            ));
            let generation = self
                .firmware
                .association_generation
                .expect("successful association publishes its generation");
            self.target_beacon_tim = TargetBeaconTimTelemetry::default();
            self.post_association_data_wait = Some(Instant::now());
            self.eapol_start_deadline = Some((Instant::now() + EAPOL_START_WAIT, generation));
            self.eapol_start_emitted = false;
            self.observe_stage(&format!(
                "firmware_wcid_stage stage=associated peer_wcid={} sta_state=assoc normalized_aid={aid} peer_identity=true keys=false port_open=false protected_management=closed",
                peer_wcid.get()
            ));
            self.observe_stage(&format!(
                "association_data_rx_activation bss_active=true bss_idx=0 bmc_wcid=19 peer_wcid={} wtbl_state=assoc no_rx_trans=true association_generation={generation} controlled_port_open=false eapol_ready=true",
                peer_wcid.get()
            ));
            self.observe_stage(
                "association_firmware_configured=true eapol_start_emitted=false supplicant_wait=authenticator_m1",
            );
            if !self.firmware.qos_tx_ready()
                || self.firmware.association_generation != Some(generation)
            {
                self.firmware.firmware_uncertain = true;
                return Err(zx::Status::IO_DATA_INTEGRITY);
            }
            self.post_assoc_rx_ready_generation = Some(generation);
            if let Err(status) = io.borrow_mut().diagnostic_association_snapshot(generation) {
                self.observe_stage(&format!(
                    "fw_state_diagnostic result=failed generation={generation} status={status}"
                ));
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = self.abort_join_roc(io);
        }
        result
    }
    fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
        io: &mut dyn Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        let association = self.firmware.association.ok_or(zx::Status::BAD_STATE)?;
        if request.peer_addr != Some(association.peer) {
            return Err(zx::Status::INVALID_ARGS);
        }
        self.invalidate_association_rx();
        // Clear association eligibility before the first fallible teardown operation.
        let io = std::cell::RefCell::new(io);
        self.firmware
            .teardown(
                |cid, command| {
                    io.borrow_mut()
                        .submit_uni(cid, command)
                        .map_err(|status| status.to_string())
                },
                |command| {
                    io.borrow_mut()
                        .submit_ce_no_ack(command)
                        .map_err(|status| status.to_string())
                },
            )
            .map_err(|_| zx::Status::IO)?;
        self.peer_wcid = None;
        self.post_association_data_wait = None;
        self.eapol_start_deadline = None;
        self.eapol_start_emitted = false;
        self.revoke_scan();
        Ok(())
    }
    fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status> {
        if !up {
            self.invalidate_association_rx();
        }
        self.firmware
            .set_controlled_port(up)
            .map_err(|_| zx::Status::BAD_STATE)?;
        if up {
            self.eapol_start_deadline = None;
        }
        self.observe_stage(if up {
            "controlled_port_open=true"
        } else {
            "controlled_port_open=false"
        });
        Ok(())
    }
    fn finish_failed_connect_attempt(
        &mut self,
        io: &mut dyn Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        // Poison all attempt-derived RX/TX authority before the first
        // fallible firmware operation. Any later error is terminally reset by
        // ClientRuntime rather than reopening the old attempt.
        self.invalidate_association_rx();
        self.firmware
            .set_controlled_port(false)
            .map_err(|_| zx::Status::BAD_STATE)?;
        self.abort_join_roc(io)?;
        let io = std::cell::RefCell::new(io);
        self.firmware
            .teardown(
                |cid, command| {
                    io.borrow_mut()
                        .submit_uni(cid, command)
                        .map_err(|status| status.to_string())
                },
                |command| {
                    io.borrow_mut()
                        .submit_ce_no_ack(command)
                        .map_err(|status| status.to_string())
                },
            )
            .map_err(|_| zx::Status::IO)?;
        self.peer_wcid = None;
        self.post_association_data_wait = None;
        self.eapol_start_deadline = None;
        self.eapol_start_emitted = false;
        self.target_beacon_tim = TargetBeaconTimTelemetry::default();

        // BSS/key teardown prevents new attempt traffic. Drain the bounded RX
        // ring before certifying quiescence so no already-completed descriptor
        // can become an old-attempt callback on the next runtime pump.
        const MAX_STALE_RX: usize = 256;
        for drained in 0..=MAX_STALE_RX {
            match io.borrow_mut().next_client_rx()? {
                None => {
                    self.observe_stage(&format!(
                        "client_attempt_cleanup result=complete stale_rx_drained={drained} keys=false port_open=false"
                    ));
                    return Ok(());
                }
                Some(_) if drained < MAX_STALE_RX => {}
                Some(_) => {
                    self.observe_stage("client_attempt_cleanup result=error reason=rx_drain_bound");
                    return Err(zx::Status::IO_DATA_INTEGRITY);
                }
            }
        }
        unreachable!("bounded stale RX drain returns from every branch")
    }
    fn next_rx(&mut self, io: &mut dyn Mt7921ClientIo) -> Result<ClientRxPoll, zx::Status> {
        use ClientRxPoll;
        if self
            .join_roc_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.abort_join_roc(io)?;
        }
        // Hardware scan observations and ordinary client RX share the data
        // ring. While an offload scan is live, leave the ring exclusively to
        // next_scan_event so beacons become scan results rather than ordinary
        // MLME RX frames.
        if self.runtime_scan.is_some() {
            return Ok(ClientRxPoll::Idle);
        }
        // Drain a successfully committed retained M1 before timers or hardware IO.
        let frame = match io.next_client_rx() {
            Ok(frame) => frame,
            Err(zx::Status::IO_DATA_INTEGRITY) => return Ok(ClientRxPoll::IntegrityDrop),
            Err(status) => return Err(status),
        };
        let Some(mut frame) = frame else {
            if let Some((deadline, generation)) = self.eapol_start_deadline {
                if self.firmware.association_generation != Some(generation)
                    || self.firmware.association.is_none()
                    || self.firmware.ptk_installed
                {
                    self.eapol_start_deadline = None;
                    self.observe_stage(
                        "eapol_liveness type=start timer=cancelled one_shot=suppressed",
                    );
                } else if Instant::now() >= deadline {
                    // Consume the one-shot before entering the synchronous TX
                    // path. A failed completion must not turn this into a
                    // retrying fallback.
                    self.eapol_start_deadline = None;
                    self.eapol_start_emitted = true;
                    self.observe_stage(
                        "eapol_liveness type=start timer=expired one_shot=committed",
                    );
                    let qos = self
                        .firmware
                        .association
                        .expect("generation-checked association")
                        .negotiated_qos;
                    let start = eapol_start_frame(self.client, self.target, qos);
                    self.send_wlan_frame(&start, fidl_softmac::WlanTxInfoFlags::empty(), io)?;
                    // The authenticator's M1 usually arrives right after our
                    // EAPOL-Start. Some APs (notably phone hotspots) send the
                    // first M1 ~1 s later than hostapd — past EAPOL_START_WAIT —
                    // so this one-shot fires, and its associated ROC/TX churn can
                    // leave the post-association RX-ready latch cleared. The
                    // EAPOL-Start only emits once we have confirmed the live
                    // association for `generation` above, so re-assert the latch
                    // for that generation; otherwise the following M1 is dropped
                    // as post_assoc_rx_not_ready and the 4-way handshake stalls.
                    self.post_assoc_rx_ready_generation = Some(generation);
                    self.observe_stage(
                        "eapol_liveness type=start timer=expired one_shot=completed",
                    );
                }
            }
            return Ok(ClientRxPoll::Idle);
        };
        let outcome = (|| -> Result<Option<ClientRxFrame>, zx::Status> {
            let control = frame
                .bytes
                .get(..2)
                .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                .ok_or(zx::Status::IO_DATA_INTEGRITY)?;
            let authentication = control & 0x00fc == 0x00b0;
            if authentication {
                let admitted = {
                    let state = self.state.lock().unwrap();
                    state
                        .channel
                        .authorized_channel()
                        .ok()
                        .filter(|channel| {
                            state.selection.permits_join(self.target, *channel)
                                && channel.channel.band
                                    == match frame.status.primary.band {
                                        WlanBand::TwoGhz => 0,
                                        WlanBand::FiveGhz => 1,
                                        _ => u8::MAX,
                                    }
                                && channel.channel.primary == u16::from(frame.status.primary.number)
                        })
                        .is_some()
                };
                if !admitted {
                    self.observe_stage(
                        "client_rx_filtered reason=stale_or_wrong_channel subtype=auth",
                    );
                    return Ok(None);
                }
                match classify_preassociation_sae_auth(&frame.bytes, self.client, self.target) {
                    Ok(Some(auth)) => {
                        self.observe(auth_frame_event("rx", &frame.bytes));
                        self.observe_stage(&format!(
                            "client_rx_admitted subtype=auth transaction={} status={} address_match=true",
                            auth.transaction, auth.status
                        ));
                    }
                    Ok(None) => unreachable!("authentication subtype was checked"),
                    Err(reason) => {
                        self.observe_stage(&format!(
                            "client_rx_filtered reason={reason} subtype=auth address_match=false"
                        ));
                        return Ok(None);
                    }
                }
                if frame.bytes.get(26..28) == Some(&[2, 0]) {
                    self.abort_join_roc(io)?;
                }
            } else if control & 0x000c == 0 {
                let subtype = ((control >> 4) & 15) as u8;
                let protected = control & 0x4000 != 0;
                if protected && matches!(subtype, 10 | 12) {
                    let association_generation = self.firmware.association_generation;
                    let pmf = self
                        .firmware
                        .association
                        .is_some_and(|association| association.mfp_required);
                    let key_current = self.firmware.ptk_installed
                        && self.firmware.ptk_rx_pn.is_some()
                        && !self.firmware.firmware_uncertain
                        && association_generation.is_some();
                    let decrypted = frame.security.is_some_and(|security| {
                        security.security_mode == 4
                            && !security.cm
                            && !security.clm
                            && !security.icv_error
                            && !security.mic_error
                            && !security.fcs_error
                            && security.pn.is_some()
                    });
                    self.observe_stage(&format!(
                        "protected_management_candidate subtype={subtype} fc_protected=true rx_security={} decrypted={decrypted} key_current={key_current} association_generation={} pmf={pmf}",
                        frame.security.is_some(),
                        association_generation
                            .map_or_else(|| "none".to_string(), |value| value.to_string()),
                    ));
                    let admitted = frame.security.and_then(|security| {
                        association_generation.map(|generation| ClientRxCandidate {
                            generation: ClientDataGeneration::Association(generation),
                            eapol: false,
                            wcid: security.wcid,
                            tid: security.tid,
                            group: frame.bytes.get(4).is_some_and(|byte| byte & 1 != 0),
                            key_id: security.key_id,
                            security_mode: security.security_mode,
                            cm: security.cm,
                            clm: security.clm,
                            icv_error: security.icv_error,
                            mic_error: security.mic_error,
                            fcs_error: security.fcs_error,
                            pn: security.pn.unwrap_or([0; 6]),
                        })
                    });
                    if !decrypted {
                        self.observe_stage(&format!(
                            "client_rx_integrity_validation result=drop reason=protected_not_decrypted subtype={subtype}"
                        ));
                        return Err(zx::Status::IO_DATA_INTEGRITY);
                    }
                    let Some(admitted) = admitted else {
                        self.observe_stage(&format!(
                            "client_rx_filtered reason=protected_stale_or_unassociated subtype={subtype}"
                        ));
                        return Ok(None);
                    };
                    if let Err(reason) = self.firmware.deliver_protected_management_rx(admitted) {
                        self.observe_stage(&format!(
                            "client_rx_integrity_validation result=drop reason={reason} subtype={subtype}"
                        ));
                        return Err(zx::Status::IO_DATA_INTEGRITY);
                    }
                    // Connac2 leaves the CCMP header and MIC in an otherwise
                    // decrypted management MPDU. Strip them only after the
                    // current-key/generation and replay checks above succeed.
                    strip_verified_management_ccmp(&mut frame.bytes)
                        .map_err(|_| zx::Status::IO_DATA_INTEGRITY)?;
                    self.observe_stage(&format!(
                        "protected_management_admitted subtype={subtype} decrypted=true key_generation_current=true"
                    ));
                }
                let classification =
                    classify_client_management_frame(&frame.bytes, self.client, self.target);
                let channel_generation_match = {
                    let state = self.state.lock().unwrap();
                    state
                        .channel
                        .authorized_channel()
                        .ok()
                        .is_some_and(|channel| {
                            self.firmware.joined.is_some_and(|joined| {
                                joined.bssid == self.target
                                    && joined.channel == channel.channel.primary
                                    && joined.channel_generation == channel.generation
                                    && channel.channel.band
                                        == match frame.status.primary.band {
                                            WlanBand::TwoGhz => 0,
                                            WlanBand::FiveGhz => 1,
                                            _ => u8::MAX,
                                        }
                                    && channel.channel.primary
                                        == u16::from(frame.status.primary.number)
                            })
                        })
                };
                if classification.subtype == 1 {
                    let capability = frame
                        .bytes
                        .get(24..26)
                        .map(|field| u16::from_le_bytes([field[0], field[1]]));
                    let status = frame
                        .bytes
                        .get(26..28)
                        .map(|field| u16::from_le_bytes([field[0], field[1]]));
                    let raw_aid = frame
                        .bytes
                        .get(28..30)
                        .map(|field| u16::from_le_bytes([field[0], field[1]]));
                    let sequence = frame
                        .bytes
                        .get(22..24)
                        .map(|field| u16::from_le_bytes([field[0], field[1]]) >> 4);
                    self.observe_stage(&format!(
                        "association_response_structure monotonic_ns={{observer_monotonic_ns}} capability={} status={} raw_aid={} retry={} sequence={} fixed_fields_complete={}",
                        capability.map_or_else(
                            || "unknown".to_string(),
                            |value| format!("0x{value:04x}")
                        ),
                        status.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                        raw_aid.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                        control & 0x0800 != 0,
                        sequence.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                        capability.is_some()
                            && status.is_some()
                            && raw_aid.is_some()
                            && sequence.is_some(),
                    ));
                    self.observe_stage(&format!(
                        "association_response_candidate addr1_is_client={} addr2_is_peer={} addr3_is_bssid={} channel_generation_match={channel_generation_match}",
                        classification.addr1_is_client,
                        classification.addr2_is_peer,
                        classification.addr3_is_bssid,
                    ));
                }
                if !channel_generation_match
                    || !self
                        .firmware
                        .accepts_joined_management(&frame.bytes, self.client)
                {
                    let reason = if !channel_generation_match {
                        "stale_or_wrong_channel"
                    } else if !classification.addr1_is_client {
                        "foreign_receiver"
                    } else {
                        "foreign_bss"
                    };
                    if classification.subtype == 1 {
                        self.observe_stage(&format!(
                            "association_response_drop subreason={reason} channel_generation_match={channel_generation_match}"
                        ));
                    }
                    self.observe_stage(&format!(
                        "client_rx_filtered reason={reason} subtype={}",
                        classification.subtype
                    ));
                    return Ok(None);
                }
                self.observe_target_beacon_tim(&frame.bytes);
                if classification.subtype == 1 {
                    self.observe_stage(
                        "association_response_admitted address_match=true channel_generation_match=true",
                    );
                    self.observe_stage(&format!(
                        "association_response_ies ie_id_lengths={}",
                        management_ie_id_lengths(&frame.bytes, 30)
                    ));
                    let status = frame
                        .bytes
                        .get(26..28)
                        .map(|field| u16::from_le_bytes([field[0], field[1]]));
                    let comeback = association_comeback_interval(&frame.bytes, 30);
                    if let Some((tu, ms)) = comeback {
                        self.observe_stage(&format!(
                            "association_comeback advertised=true valid=true tu={tu} ms={ms}"
                        ));
                    } else if status == Some(30) {
                        self.observe_stage(
                            "association_comeback advertised=unknown valid=false tu=unknown ms=unknown",
                        );
                    }
                    self.observe_stage(&match status {
                    Some(0) => "mlme_association_disposition result=success status=0 retry_supported=true".to_string(),
                    Some(30) if comeback.is_some() => "mlme_association_disposition result=comeback status=30 retry_supported=true".to_string(),
                    Some(status) => format!(
                        "mlme_association_disposition result=failure status={status} retry_supported=false"
                    ),
                    None => "mlme_association_disposition result=malformed status=unknown retry_supported=true".to_string(),
                });
                    if status.is_some_and(|status| status != 0) {
                        self.abort_join_roc(io)?;
                        self.observe_stage(&format!(
                            "join_roc_lifecycle phase=association_response status={} abort=non_success_before_retry",
                            status.expect("non-success status was checked")
                        ));
                    }
                }
                if matches!(classification.subtype, 10 | 12) {
                    self.invalidate_association_rx();
                    self.eapol_start_deadline = None;
                }
            }
            if control & 0x000c == 0x0008 {
                let observation_started = self.post_association_data_wait.take();
                if let Some(started) = observation_started {
                    self.observe_stage(&format!(
                        "post_association_first_data result=observed elapsed_ms={} data_candidate=true",
                        started.elapsed().as_millis()
                    ));
                }
                let classification =
                    classify_client_data_frame(&frame.bytes, self.client, self.target);
                let security = frame.security.ok_or(zx::Status::IO_DATA_INTEGRITY)?;
                let eapol = classification.snap_present
                    && classification.ether_type == Some(0x888e)
                    && classification.llc_result == "valid";
                let generation = self.firmware.tx_generation(eapol);
                let association_generation_match = self
                    .firmware
                    .association_generation
                    .is_some_and(|expected| {
                        generation == Ok(ClientDataGeneration::Association(expected))
                    });
                self.observe_stage(&format!(
                    "client_data_candidate frame_type={} subtype={} to_ds={} from_ds={} protected={} header_offset={} qos={} amsdu={} addr1_is_client={} addr2_is_peer={} addr3_is_bssid={} snap_present={} ether_type={} llc_result={} wcid={} security_mode={} cm={} clm={} icv_error={} mic_error={} fcs_error={} association_generation_match={association_generation_match}",
                    classification.frame_type,
                    classification.subtype,
                    classification.to_ds,
                    classification.from_ds,
                    classification.protected,
                    classification.header_offset,
                    classification.qos,
                    classification.amsdu,
                    classification.addr1_is_client,
                    classification.addr2_is_peer,
                    classification.addr3_is_bssid,
                    classification.snap_present,
                    classification.ether_type.map_or(0, u16::from),
                    classification.llc_result,
                    security.wcid,
                    security.security_mode,
                    security.cm,
                    security.clm,
                    security.icv_error,
                    security.mic_error,
                    security.fcs_error,
                ));
                self.observe_stage(&format!(
                    "firmware_rx_lookup observed_wcid={} peer_wcid={} lookup_match={} firmware_wcid_stage={} association_generation_match={association_generation_match}",
                    security.wcid,
                    self.peer_wcid.map(ClientWcid::get).unwrap_or(0),
                    self.peer_wcid
                        .is_some_and(|wcid| security.wcid == u16::from(wcid.get())),
                    if self.firmware.association.is_some() {
                        "associated"
                    } else if self.firmware.preauth_peer.is_some() {
                        "preauth"
                    } else {
                        "absent"
                    },
                ));
                let observer = self.observer;
                let drop = |subreason| {
                    notify_observer(
                        observer,
                        stage_event(&format!(
                            "client_data_drop subreason={subreason} wcid={} association_generation_match={association_generation_match}",
                            security.wcid
                        )),
                    );
                };
                let firmware_rx_ready =
                    self.firmware
                        .association_generation
                        .is_some_and(|generation| {
                            self.firmware.qos_tx_ready()
                                && self.post_assoc_rx_ready_generation == Some(generation)
                        });
                if self.firmware.association.is_none() {
                    drop("no_association");
                    return Ok(None);
                }
                if eapol && !firmware_rx_ready {
                    self.observe_stage(&format!(
                        "post_assoc_rx_ready_debug qos_tx_ready={} ready_generation={:?} association_generation={:?} eapol_start_emitted={}",
                        self.firmware.qos_tx_ready(),
                        self.post_assoc_rx_ready_generation,
                        self.firmware.association_generation,
                        self.eapol_start_emitted,
                    ));
                    drop("post_assoc_rx_not_ready");
                    return Ok(None);
                }
                if classification.to_ds || !classification.from_ds {
                    drop("direction_not_ap_to_sta");
                    return Ok(None);
                }
                if !classification.addr1_is_client {
                    drop("foreign_receiver");
                    return Ok(None);
                }
                if !classification.addr2_is_peer {
                    drop("foreign_transmitter");
                    return Ok(None);
                }
                if eapol && !classification.addr3_is_bssid {
                    drop("foreign_bssid");
                    return Ok(None);
                }
                let current_channel = {
                    let state = self.state.lock().unwrap();
                    state.channel.authorized_channel().is_ok_and(|channel| {
                        channel.channel.band
                            == match frame.status.primary.band {
                                WlanBand::TwoGhz => 0,
                                WlanBand::FiveGhz => 1,
                                _ => u8::MAX,
                            }
                            && channel.channel.primary == u16::from(frame.status.primary.number)
                    })
                };
                if !current_channel {
                    drop("wrong_channel");
                    return Ok(None);
                }
                if security.wcid == 1023 && classification.llc_result != "valid" {
                    drop(match classification.llc_result {
                        "llc_truncated" | "header_truncated" => "malformed_llc",
                        "amsdu" => "amsdu_unicast_search_miss",
                        _ => "non_snap_sentinel",
                    });
                    return Ok(None);
                }
                if security.wcid == 1023 && !eapol {
                    drop("non_eapol_sentinel");
                    return Ok(None);
                }
                if security.wcid == 1023 && !classification.addr3_is_bssid {
                    drop("foreign_bssid_sentinel");
                    return Ok(None);
                }
                let generation = generation.map_err(|_| {
                    drop("security_generation_gate");
                    zx::Status::ACCESS_DENIED
                })?;
                let validation = self.firmware.deliver_rx(ClientRxCandidate {
                    generation,
                    eapol,
                    wcid: security.wcid,
                    tid: security.tid,
                    group: frame.bytes.get(4).is_some_and(|byte| byte & 1 != 0),
                    key_id: security.key_id,
                    security_mode: security.security_mode,
                    cm: security.cm,
                    clm: security.clm,
                    icv_error: security.icv_error,
                    mic_error: security.mic_error,
                    fcs_error: security.fcs_error,
                    pn: security.pn.unwrap_or([0; 6]),
                });
                if let Err(reason) = validation {
                    drop("security_replay_or_integrity");
                    self.observe_stage(&format!(
                        "client_rx_integrity_validation result=drop reason={reason}"
                    ));
                    return Err(zx::Status::IO_DATA_INTEGRITY);
                }
                let m1 = is_exact_target_eapol_candidate(&classification)
                    && is_authenticator_m1(&frame.bytes);
                if m1 {
                    self.post_association_data_wait = None;
                    self.eapol_start_deadline = None;
                    self.observe_stage(
                        "eapol_liveness type=start timer=cancelled one_shot=suppressed",
                    );
                }
                self.observe_stage(&format!(
                    "client_data_admitted eapol={eapol} wcid={} association_generation_match={association_generation_match}",
                    security.wcid
                ));
            } else if control & 0x000c != 0 {
                return Err(zx::Status::IO_DATA_INTEGRITY);
            } else if authentication {
                let sequence = frame
                    .bytes
                    .get(26..28)
                    .map(|value| u16::from_le_bytes([value[0], value[1]]))
                    .ok_or(zx::Status::IO_DATA_INTEGRITY)?;
                let status = frame
                    .bytes
                    .get(28..30)
                    .map(|value| u16::from_le_bytes([value[0], value[1]]))
                    .ok_or(zx::Status::IO_DATA_INTEGRITY)?;
                self.observe_stage(match sequence {
                    1 => "sae_peer_commit_rx",
                    2 => "sae_peer_confirm_rx",
                    _ => "sae_peer_protocol_rx",
                });
                self.observe(LiveClientEvent::new(
                    format!(r#"{{"sae_peer_status":{{"sequence":{sequence},"status":{status}}}}}"#),
                    true,
                ));
            }
            Ok(Some(frame))
        })();
        match outcome {
            Ok(Some(frame)) => Ok(ClientRxPoll::Frame(frame)),
            Ok(None) => Ok(ClientRxPoll::Idle),
            Err(zx::Status::IO_DATA_INTEGRITY) => Ok(ClientRxPoll::IntegrityDrop),
            Err(status) => Err(status),
        }
    }
    fn begin_passive_scan(
        &mut self,
        scan_id: u64,
        channels: &[fidl_ieee80211::ChannelNumber],
    ) -> Result<(), zx::Status> {
        if scan_id == 0 || channels.is_empty() || self.runtime_scan.is_some() {
            return Err(zx::Status::INVALID_ARGS);
        }
        let operating_channel = {
            let mut state = self.state.lock().unwrap();
            let operating = state.channel.current_channel().map(|lease| lease.channel);
            state.channel.invalidate_current();
            operating
        };
        self.established_channel = None;
        self.runtime_scan = Some(RuntimeTargetScan {
            id: scan_id,
            observation_generation: 0,
            operating_channel,
            target_channel: None,
        });
        Ok(())
    }
    fn observe_passive_scan(
        &mut self,
        scan_id: u64,
        observation: &fuchsia_softmac_port::ScanObservation,
    ) -> Result<(), zx::Status> {
        let scan = self.runtime_scan.as_mut().ok_or(zx::Status::BAD_STATE)?;
        if scan.id != scan_id {
            return Err(zx::Status::BAD_STATE);
        }
        scan.observation_generation = scan
            .observation_generation
            .checked_add(1)
            .ok_or(zx::Status::NO_RESOURCES)?;
        if observation.bss.bssid == self.target {
            let channel = client_physical_channel(
                observation.bss.primary,
                observation.bss.bandwidth,
                observation.bss.vht_secondary_80_channel,
            )?;
            self.state.lock().unwrap().selection =
                ClientTargetBssLease::retain(ClientScanEvidence {
                    scan_id,
                    observation_generation: scan.observation_generation,
                    observation_timestamp_nanos: observation.timestamp_nanos,
                    bssid: self.target,
                    channel,
                })
                .map_err(|_| zx::Status::BAD_STATE)?;
            scan.target_channel = Some(channel);
        }
        Ok(())
    }
    fn complete_passive_scan(
        &mut self,
        scan_id: u64,
        success: bool,
        io: &mut dyn Mt7921ClientIo,
    ) -> Result<(), zx::Status> {
        let scan = self.runtime_scan.take().ok_or(zx::Status::BAD_STATE)?;
        if scan.id != scan_id {
            self.revoke_scan();
            return Err(zx::Status::BAD_STATE);
        }
        let restore_channel = scan.operating_channel.or(scan.target_channel);
        if let Some(channel) = restore_channel {
            if let Err(status) = io.establish_client_channel(channel) {
                self.revoke_scan();
                return Err(status);
            }
            self.state
                .lock()
                .unwrap()
                .channel
                .establish_channel(channel)
                .map_err(|_| zx::Status::NO_RESOURCES)?;
            self.established_channel = Some(channel);
        }
        if !success {
            self.revoke_scan();
            return Ok(());
        }
        let Some(channel) = scan.target_channel else {
            return Ok(());
        };
        let mut state = self.state.lock().unwrap();
        let ClientPhysicalChannelEnsure::Current(lease) = state.channel.ensure_channel(channel)
        else {
            state.selection.invalidate();
            return Err(zx::Status::BAD_STATE);
        };
        state
            .selection
            .mark_rate_power_ready(self.target, channel)
            .and_then(|()| {
                state
                    .selection
                    .authorize_sae(self.target, lease)
                    .map(|_| ())
            })
            .and_then(|()| state.channel.authorize_channel(channel).map(|_| ()))
            .map_err(|_| zx::Status::BAD_STATE)
    }
    fn reset(&mut self) -> Result<(), zx::Status> {
        self.revoke_scan();
        if self.firmware.firmware_uncertain {
            Err(zx::Status::IO)
        } else {
            Ok(())
        }
    }
    fn stop(&mut self) -> Result<(), zx::Status> {
        self.revoke_scan();
        if self.firmware.firmware_uncertain {
            Err(zx::Status::IO)
        } else {
            Ok(())
        }
    }
}
