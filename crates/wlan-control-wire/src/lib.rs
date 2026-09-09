// SPDX-License-Identifier: GPL-2.0-only

//! Project-owned, bounded WLAN control wire values and codec.
//!
//! This crate deliberately owns no transport. Each encoded value is one complete
//! packet suitable for (among other transports) an inherited Unix seqpacket fd.
//! The value types retain the pinned Fuchsia SME schema semantics, while this
//! codec is independent of FIDL and Zircon transports.
//!
//! A scan reply is one atomic packet containing the complete result vector;
//! [`encode`] returns [`Error::PacketTooLarge`] rather than truncating results.
//! SME [`sme::ScanErrorCode`] values are policy outcomes in that reply, while
//! runtime-terminal failures use [`Message::GenerationEnd`]. This policy seam
//! never carries file descriptors; the Ethernet data-plane capability belongs
//! to the distinct Wi-Fi-to-supervisor lifecycle seam.
//!
//! Header `request_id` is the sending endpoint's packet sequence, not an echo:
//! each direction has an independent strictly increasing sequence. Reply bodies
//! carry [`Reply::in_reply_to`] to identify the request packet they complete,
//! so unsolicited events may safely interleave and requests may overlap.

use fidl_fuchsia_wlan_common::{ScanType, WlanMacRole};
use fidl_fuchsia_wlan_ieee80211 as ieee;
use fidl_fuchsia_wlan_internal as internal;
use fidl_fuchsia_wlan_sme as sme;
use std::fmt;

pub const MAGIC: [u8; 4] = *b"WLCP";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 36;
pub const MAX_PACKET: usize = 8192;
pub const MAX_BSS_IE_LEN: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectReply {
    /// SME completed the attempt. `result` preserves its exact status and
    /// credential/reconnect classification (including non-success statuses).
    Completed(sme::ConnectResult),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandReply {
    Success,
    Busy,
    NotConnected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerationEndReason {
    Shutdown,
    Timeout,
    DriverFault,
    ContainmentFault,
    Backpressure,
    ProtocolViolation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reply<T> {
    /// Header `request_id` of the request packet completed by this reply.
    pub in_reply_to: u64,
    pub result: T,
}

#[derive(Clone, Eq, PartialEq)]
pub enum Message {
    Ready,
    Scan(sme::ScanRequest),
    ScanReply(Reply<Result<Vec<sme::ScanResult>, sme::ScanErrorCode>>),
    Connect(sme::ConnectRequest),
    ConnectReply(Reply<ConnectReply>),
    Disconnect(sme::UserDisconnectReason),
    DisconnectReply(Reply<CommandReply>),
    Roam(sme::RoamRequest),
    RoamReply(Reply<CommandReply>),
    Event(sme::ConnectTransactionEvent),
    /// Terminal for the entire Wi-Fi service generation. On receipt, a
    /// transport must fail all pending requests, close every event stream, and
    /// reject further sends; runtime-terminal failures are never command
    /// replies.
    GenerationEnd(GenerationEndReason),
}

// Do not derive Debug: ConnectRequest authentication may contain credentials.
impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => f.write_str("Ready"),
            Self::Scan(request) => f.debug_tuple("Scan").field(request).finish(),
            Self::ScanReply(reply) => f.debug_tuple("ScanReply").field(reply).finish(),
            Self::Connect(request) => f
                .debug_struct("Connect")
                .field("ssid_len", &request.ssid.len())
                .field("bssid", &request.bss_description.bssid)
                .field("authentication", &"<redacted>")
                .finish(),
            Self::ConnectReply(reply) => f.debug_tuple("ConnectReply").field(reply).finish(),
            Self::Disconnect(reason) => f.debug_tuple("Disconnect").field(reason).finish(),
            Self::DisconnectReply(reply) => f.debug_tuple("DisconnectReply").field(reply).finish(),
            Self::Roam(request) => f
                .debug_struct("Roam")
                .field("bssid", &request.bss_description.bssid)
                .finish(),
            Self::RoamReply(reply) => f.debug_tuple("RoamReply").field(reply).finish(),
            Self::Event(event) => f.debug_tuple("Event").field(event).finish(),
            Self::GenerationEnd(reason) => f.debug_tuple("GenerationEnd").field(reason).finish(),
        }
    }
}

impl Message {
    pub const fn in_reply_to(&self) -> Option<u64> {
        match self {
            Self::ScanReply(reply) => Some(reply.in_reply_to),
            Self::ConnectReply(reply) => Some(reply.in_reply_to),
            Self::DisconnectReply(reply) => Some(reply.in_reply_to),
            Self::RoamReply(reply) => Some(reply.in_reply_to),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Packet {
    pub generation: [u8; 16],
    pub request_id: u64,
    pub message: Message,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    PacketTooLarge,
    Truncated,
    BadMagic,
    UnsupportedVersion(u16),
    UnknownKind(u16),
    InvalidLength,
    TrailingBytes,
    ZeroRequestId,
    WrongGeneration,
    NonIncreasingRequestId,
    UnknownDiscriminant(&'static str, u64),
    BoundExceeded(&'static str),
    InvalidValue(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// Validates packets received from one peer. Use a separate instance for each
/// direction; `request_id` is that peer's packet sequence in this generation.
#[derive(Clone, Debug)]
pub struct SessionValidator {
    generation: [u8; 16],
    last_request_id: u64,
}
impl SessionValidator {
    pub const fn new(generation: [u8; 16]) -> Self {
        Self {
            generation,
            last_request_id: 0,
        }
    }
    pub const fn generation(&self) -> [u8; 16] {
        self.generation
    }
    pub const fn last_request_id(&self) -> u64 {
        self.last_request_id
    }
    pub fn validate(&mut self, packet: &Packet) -> Result<(), Error> {
        if packet.generation != self.generation {
            return Err(Error::WrongGeneration);
        }
        if packet.request_id == 0 {
            return Err(Error::ZeroRequestId);
        }
        if packet.request_id <= self.last_request_id {
            return Err(Error::NonIncreasingRequestId);
        }
        self.last_request_id = packet.request_id;
        Ok(())
    }
}

const READY: u16 = 1;
const CONNECT: u16 = 2;
const CONNECT_REPLY: u16 = 3;
const DISCONNECT: u16 = 4;
const DISCONNECT_REPLY: u16 = 5;
const ROAM: u16 = 6;
const ROAM_REPLY: u16 = 7;
const EVENT: u16 = 8;
const GENERATION_END: u16 = 9;
const SCAN: u16 = 11;
const SCAN_REPLY: u16 = 12;

/// Number of file descriptors the policy transport must attach to this message.
/// Policy transport bindings must use kernel operations without an ancillary
/// data interface, so descriptors can be neither imported nor exported.
pub const fn required_fd_count(_message: &Message) -> usize {
    0
}

pub fn encode(packet: &Packet) -> Result<Vec<u8>, Error> {
    if packet.request_id == 0 {
        return Err(Error::ZeroRequestId);
    }
    let (kind, body) = encode_message(&packet.message)?;
    let total = HEADER_LEN
        .checked_add(body.len())
        .ok_or(Error::PacketTooLarge)?;
    if total > MAX_PACKET {
        return Err(Error::PacketTooLarge);
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC);
    put_u16(&mut out, VERSION);
    put_u16(&mut out, kind);
    put_u32(&mut out, body.len() as u32);
    out.extend_from_slice(&packet.generation);
    put_u64(&mut out, packet.request_id);
    debug_assert_eq!(out.len(), HEADER_LEN);
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn decode(bytes: &[u8]) -> Result<Packet, Error> {
    if bytes.len() > MAX_PACKET {
        return Err(Error::PacketTooLarge);
    }
    if bytes.len() < HEADER_LEN {
        return Err(Error::Truncated);
    }
    if bytes[..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let mut h = Reader::new(&bytes[4..HEADER_LEN]);
    let version = h.u16()?;
    if version != VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let kind = h.u16()?;
    let body_len = h.u32()? as usize;
    let generation = h.array::<16>()?;
    let request_id = h.u64()?;
    if request_id == 0 {
        return Err(Error::ZeroRequestId);
    }
    if body_len != bytes.len() - HEADER_LEN {
        return Err(Error::InvalidLength);
    }
    let mut body = Reader::new(&bytes[HEADER_LEN..]);
    let message = decode_message(kind, &mut body)?;
    if !body.done() {
        return Err(Error::TrailingBytes);
    }
    Ok(Packet {
        generation,
        request_id,
        message,
    })
}

fn encode_message(message: &Message) -> Result<(u16, Vec<u8>), Error> {
    let mut w = Vec::new();
    let kind = match message {
        Message::Ready => READY,
        Message::Scan(v) => {
            enc_scan(&mut w, v)?;
            SCAN
        }
        Message::ScanReply(v) => {
            enc_reply(&mut w, v, enc_scan_reply)?;
            SCAN_REPLY
        }
        Message::Connect(v) => {
            enc_connect(&mut w, v)?;
            CONNECT
        }
        Message::ConnectReply(v) => {
            enc_reply(&mut w, v, |w, v| {
                enc_connect_reply(w, v);
                Ok(())
            })?;
            CONNECT_REPLY
        }
        Message::Disconnect(v) => {
            put_u32(&mut w, *v as u32);
            DISCONNECT
        }
        Message::DisconnectReply(v) => {
            enc_reply(&mut w, v, |w, v| {
                w.push(command_reply(*v));
                Ok(())
            })?;
            DISCONNECT_REPLY
        }
        Message::Roam(v) => {
            enc_bss(&mut w, &v.bss_description)?;
            ROAM
        }
        Message::RoamReply(v) => {
            enc_reply(&mut w, v, |w, v| {
                w.push(command_reply(*v));
                Ok(())
            })?;
            ROAM_REPLY
        }
        Message::Event(v) => {
            enc_event(&mut w, v)?;
            EVENT
        }
        Message::GenerationEnd(v) => {
            w.push(generation_end(*v));
            GENERATION_END
        }
    };
    Ok((kind, w))
}

fn decode_message(kind: u16, r: &mut Reader<'_>) -> Result<Message, Error> {
    Ok(match kind {
        READY => Message::Ready,
        SCAN => Message::Scan(dec_scan(r)?),
        SCAN_REPLY => Message::ScanReply(dec_reply(r, dec_scan_reply)?),
        CONNECT => Message::Connect(dec_connect(r)?),
        CONNECT_REPLY => Message::ConnectReply(dec_reply(r, dec_connect_reply)?),
        DISCONNECT => Message::Disconnect(dec_disconnect_reason(r.u32()?)?),
        DISCONNECT_REPLY => Message::DisconnectReply(dec_reply(r, |r| dec_command_reply(r.u8()?))?),
        ROAM => Message::Roam(sme::RoamRequest {
            bss_description: dec_bss(r)?,
        }),
        ROAM_REPLY => Message::RoamReply(dec_reply(r, |r| dec_command_reply(r.u8()?))?),
        EVENT => Message::Event(dec_event(r)?),
        GENERATION_END => Message::GenerationEnd(dec_generation_end(r.u8()?)?),
        other => return Err(Error::UnknownKind(other)),
    })
}

fn enc_connect(w: &mut Vec<u8>, v: &sme::ConnectRequest) -> Result<(), Error> {
    put_vec(w, &v.ssid, 32, "SSID")?;
    enc_bss(w, &v.bss_description)?;
    put_bool(w, v.multiple_bss_candidates);
    enc_auth(w, &v.authentication)?;
    put_u32(w, v.deprecated_scan_type as u32);
    Ok(())
}
fn dec_connect(r: &mut Reader<'_>) -> Result<sme::ConnectRequest, Error> {
    let ssid = r.vec(32, "SSID")?;
    let bss_description = dec_bss(r)?;
    let multiple_bss_candidates = r.bool()?;
    let authentication = dec_auth(r)?;
    let deprecated_scan_type = match r.u32()? {
        1 => ScanType::Active,
        2 => ScanType::Passive,
        n => return Err(Error::UnknownDiscriminant("ScanType", n.into())),
    };
    Ok(sme::ConnectRequest {
        ssid,
        bss_description,
        multiple_bss_candidates,
        authentication,
        deprecated_scan_type,
    })
}

fn enc_reply<T>(
    w: &mut Vec<u8>,
    reply: &Reply<T>,
    encode_result: impl FnOnce(&mut Vec<u8>, &T) -> Result<(), Error>,
) -> Result<(), Error> {
    if reply.in_reply_to == 0 {
        return Err(Error::ZeroRequestId);
    }
    put_u64(w, reply.in_reply_to);
    encode_result(w, &reply.result)
}

fn dec_reply<T>(
    r: &mut Reader<'_>,
    decode_result: impl FnOnce(&mut Reader<'_>) -> Result<T, Error>,
) -> Result<Reply<T>, Error> {
    let in_reply_to = r.u64()?;
    if in_reply_to == 0 {
        return Err(Error::ZeroRequestId);
    }
    Ok(Reply {
        in_reply_to,
        result: decode_result(r)?,
    })
}

fn enc_scan(w: &mut Vec<u8>, v: &sme::ScanRequest) -> Result<(), Error> {
    match v {
        sme::ScanRequest::Active(v) => {
            w.push(1);
            put_u32(w, v.ssids.len() as u32);
            for ssid in &v.ssids {
                put_vec(w, ssid, 32, "SSID")?;
            }
            put_vec(w, &v.channels, 256, "scan channels")?;
        }
        sme::ScanRequest::Passive(v) => {
            w.push(2);
            put_vec(w, &v.channels, 256, "scan channels")?;
        }
    }
    Ok(())
}
fn dec_scan(r: &mut Reader<'_>) -> Result<sme::ScanRequest, Error> {
    Ok(match r.u8()? {
        1 => {
            let count = r.u32()? as usize;
            if count > 84 {
                return Err(Error::BoundExceeded("scan SSIDs"));
            }
            let mut ssids = Vec::with_capacity(count);
            for _ in 0..count {
                ssids.push(r.vec(32, "SSID")?);
            }
            sme::ScanRequest::Active(sme::ActiveScanRequest {
                ssids,
                channels: r.vec(256, "scan channels")?,
            })
        }
        2 => sme::ScanRequest::Passive(sme::PassiveScanRequest {
            channels: r.vec(256, "scan channels")?,
        }),
        n => return Err(Error::UnknownDiscriminant("ScanRequest", n.into())),
    })
}

fn enc_scan_reply(
    w: &mut Vec<u8>,
    v: &Result<Vec<sme::ScanResult>, sme::ScanErrorCode>,
) -> Result<(), Error> {
    match v {
        Ok(results) => {
            w.push(1);
            put_u32(w, results.len() as u32);
            for result in results {
                enc_scan_result(w, result)?;
            }
        }
        Err(error) => {
            w.push(2);
            put_u32(w, *error as u32);
        }
    }
    Ok(())
}
fn dec_scan_reply(
    r: &mut Reader<'_>,
) -> Result<Result<Vec<sme::ScanResult>, sme::ScanErrorCode>, Error> {
    Ok(match r.u8()? {
        1 => {
            let count = r.u32()? as usize;
            // Even zero-sized records exceed this bound in a valid packet.
            if count > MAX_PACKET {
                return Err(Error::BoundExceeded("scan results"));
            }
            let mut results = Vec::with_capacity(count);
            for _ in 0..count {
                results.push(dec_scan_result(r)?);
            }
            Ok(results)
        }
        2 => Err(dec_scan_error(r.u32()?)?),
        n => return Err(Error::UnknownDiscriminant("ScanReply", n.into())),
    })
}
fn enc_scan_result(w: &mut Vec<u8>, v: &sme::ScanResult) -> Result<(), Error> {
    match &v.compatibility {
        sme::Compatibility::Compatible(c) => {
            w.push(1);
            put_u32(w, c.mutual_security_protocols.len() as u32);
            for p in &c.mutual_security_protocols {
                put_u32(w, p.into_primitive());
            }
        }
        sme::Compatibility::Incompatible(i) => {
            w.push(2);
            put_vec(
                w,
                i.description.as_bytes(),
                MAX_PACKET,
                "compatibility description",
            )?;
            match &i.disjoint_security_protocols {
                None => w.push(0),
                Some(values) => {
                    w.push(1);
                    put_u32(w, values.len() as u32);
                    for v in values {
                        put_u32(w, v.protocol.into_primitive());
                        put_u32(w, v.role.into_primitive());
                    }
                }
            }
        }
    }
    w.extend_from_slice(&v.timestamp_nanos.to_le_bytes());
    enc_bss(w, &v.bss_description)
}
fn dec_scan_result(r: &mut Reader<'_>) -> Result<sme::ScanResult, Error> {
    let compatibility = match r.u8()? {
        1 => {
            let count = r.u32()? as usize;
            if count > MAX_PACKET / 4 {
                return Err(Error::BoundExceeded("security protocols"));
            }
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                let n = r.u32()?;
                values.push(
                    internal::Protocol::from_primitive(n)
                        .ok_or(Error::UnknownDiscriminant("Protocol", n.into()))?,
                );
            }
            sme::Compatibility::Compatible(sme::Compatible {
                mutual_security_protocols: values,
            })
        }
        2 => {
            let bytes = r.vec(MAX_PACKET, "compatibility description")?;
            let description = String::from_utf8(bytes)
                .map_err(|_| Error::InvalidValue("compatibility description UTF-8"))?;
            let disjoint_security_protocols = match r.u8()? {
                0 => None,
                1 => {
                    let count = r.u32()? as usize;
                    if count > MAX_PACKET / 8 {
                        return Err(Error::BoundExceeded("disjoint protocols"));
                    }
                    let mut values = Vec::with_capacity(count);
                    for _ in 0..count {
                        let n = r.u32()?;
                        let protocol = internal::Protocol::from_primitive(n)
                            .ok_or(Error::UnknownDiscriminant("Protocol", n.into()))?;
                        let n = r.u32()?;
                        let role = WlanMacRole::from_primitive(n)
                            .ok_or(Error::UnknownDiscriminant("WlanMacRole", n.into()))?;
                        values.push(sme::DisjointSecurityProtocol { protocol, role });
                    }
                    Some(values)
                }
                n => return Err(Error::UnknownDiscriminant("Option", n.into())),
            };
            sme::Compatibility::Incompatible(sme::Incompatible {
                description,
                disjoint_security_protocols,
            })
        }
        n => return Err(Error::UnknownDiscriminant("Compatibility", n.into())),
    };
    let timestamp_nanos = i64::from_le_bytes(r.array()?);
    Ok(sme::ScanResult {
        compatibility,
        timestamp_nanos,
        bss_description: dec_bss(r)?,
    })
}
fn dec_scan_error(n: u32) -> Result<sme::ScanErrorCode, Error> {
    Ok(match n {
        1 => sme::ScanErrorCode::NotSupported,
        2 => sme::ScanErrorCode::InternalError,
        3 => sme::ScanErrorCode::InternalMlmeError,
        4 => sme::ScanErrorCode::ShouldWait,
        5 => sme::ScanErrorCode::CanceledByDriverOrFirmware,
        _ => return Err(Error::UnknownDiscriminant("ScanErrorCode", n.into())),
    })
}

fn enc_auth(w: &mut Vec<u8>, v: &internal::Authentication) -> Result<(), Error> {
    put_u32(w, v.protocol.into_primitive());
    match &v.credentials {
        None => w.push(0),
        Some(c) => {
            w.push(1);
            match c.as_ref() {
                internal::Credentials::Wep(c) => {
                    w.push(1);
                    put_vec(w, &c.key, 32, "WEP key")?;
                }
                internal::Credentials::Wpa(internal::WpaCredentials::Psk(psk)) => {
                    w.push(2);
                    w.extend_from_slice(psk);
                }
                internal::Credentials::Wpa(internal::WpaCredentials::Passphrase(p)) => {
                    w.push(3);
                    put_vec(w, p, 63, "WPA passphrase")?;
                }
                internal::Credentials::Wpa(internal::WpaCredentials::__Unknown { .. })
                | internal::Credentials::__Unknown { .. } => {
                    return Err(Error::InvalidValue("unknown credentials union"));
                }
            }
        }
    }
    Ok(())
}
fn dec_auth(r: &mut Reader<'_>) -> Result<internal::Authentication, Error> {
    let raw = r.u32()?;
    let protocol = internal::Protocol::from_primitive(raw)
        .ok_or(Error::UnknownDiscriminant("Protocol", raw.into()))?;
    let credentials = match r.u8()? {
        0 => None,
        1 => Some(Box::new(match r.u8()? {
            1 => internal::Credentials::Wep(internal::WepCredentials {
                key: r.vec(32, "WEP key")?,
            }),
            2 => internal::Credentials::Wpa(internal::WpaCredentials::Psk(r.array()?)),
            3 => internal::Credentials::Wpa(internal::WpaCredentials::Passphrase(
                r.vec(63, "WPA passphrase")?,
            )),
            n => return Err(Error::UnknownDiscriminant("Credentials", n.into())),
        })),
        n => return Err(Error::UnknownDiscriminant("Option<Credentials>", n.into())),
    };
    Ok(internal::Authentication {
        protocol,
        credentials,
    })
}

fn enc_bss(w: &mut Vec<u8>, b: &ieee::BssDescription) -> Result<(), Error> {
    w.extend_from_slice(&b.bssid);
    put_u32(w, b.bss_type.into_primitive());
    put_u16(w, b.beacon_period);
    put_u16(w, b.capability_info);
    put_vec(w, &b.ies, MAX_BSS_IE_LEN, "BSS IEs")?;
    enc_channel(w, b.primary);
    put_u32(w, b.bandwidth.into_primitive());
    enc_channel(w, b.vht_secondary_80_channel);
    w.push(b.rssi_dbm as u8);
    w.push(b.snr_db as u8);
    Ok(())
}
fn dec_bss(r: &mut Reader<'_>) -> Result<ieee::BssDescription, Error> {
    let bssid = r.array()?;
    let raw = r.u32()?;
    let bss_type = ieee::BssType::from_primitive(raw)
        .ok_or(Error::UnknownDiscriminant("BssType", raw.into()))?;
    let beacon_period = r.u16()?;
    let capability_info = r.u16()?;
    let ies = r.vec(MAX_BSS_IE_LEN, "BSS IEs")?;
    let primary = dec_channel(r)?;
    let raw = r.u32()?;
    let bandwidth = ieee::ChannelBandwidth::from_primitive(raw)
        .ok_or(Error::UnknownDiscriminant("ChannelBandwidth", raw.into()))?;
    let vht_secondary_80_channel = dec_channel(r)?;
    let rssi_dbm = r.u8()? as i8;
    let snr_db = r.u8()? as i8;
    Ok(ieee::BssDescription {
        bssid,
        bss_type,
        beacon_period,
        capability_info,
        ies,
        primary,
        bandwidth,
        vht_secondary_80_channel,
        rssi_dbm,
        snr_db,
    })
}
fn enc_channel(w: &mut Vec<u8>, c: ieee::ChannelNumber) {
    w.push(c.band.into_primitive());
    w.push(c.number);
}
fn dec_channel(r: &mut Reader<'_>) -> Result<ieee::ChannelNumber, Error> {
    let raw = r.u8()?;
    let band = ieee::WlanBand::from_primitive(raw)
        .ok_or(Error::UnknownDiscriminant("WlanBand", raw.into()))?;
    Ok(ieee::ChannelNumber {
        band,
        number: r.u8()?,
    })
}

fn enc_connect_result(w: &mut Vec<u8>, v: sme::ConnectResult) {
    put_u16(w, v.code.into_primitive());
    put_bool(w, v.is_credential_rejected);
    put_bool(w, v.is_reconnect);
}
fn dec_connect_result(r: &mut Reader<'_>) -> Result<sme::ConnectResult, Error> {
    let raw = r.u16()?;
    let code = ieee::StatusCode::from_primitive(raw)
        .ok_or(Error::UnknownDiscriminant("StatusCode", raw.into()))?;
    Ok(sme::ConnectResult {
        code,
        is_credential_rejected: r.bool()?,
        is_reconnect: r.bool()?,
    })
}
fn enc_connect_reply(w: &mut Vec<u8>, v: &ConnectReply) {
    match v {
        ConnectReply::Completed(r) => {
            w.push(1);
            enc_connect_result(w, *r);
        }
    }
}
fn dec_connect_reply(r: &mut Reader<'_>) -> Result<ConnectReply, Error> {
    Ok(match r.u8()? {
        1 => ConnectReply::Completed(dec_connect_result(r)?),
        n => return Err(Error::UnknownDiscriminant("ConnectReply", n.into())),
    })
}

fn enc_event(w: &mut Vec<u8>, v: &sme::ConnectTransactionEvent) -> Result<(), Error> {
    match v {
        sme::ConnectTransactionEvent::OnConnectResult { result } => {
            w.push(1);
            enc_connect_result(w, *result);
        }
        sme::ConnectTransactionEvent::OnDisconnect { info } => {
            w.push(2);
            enc_disconnect_info(w, info);
        }
        sme::ConnectTransactionEvent::OnRoamResult { result } => {
            w.push(3);
            enc_roam_result(w, result)?;
        }
        sme::ConnectTransactionEvent::OnSignalReport { ind } => {
            w.push(4);
            w.push(ind.rssi_dbm as u8);
            w.push(ind.snr_db as u8);
        }
        sme::ConnectTransactionEvent::OnChannelSwitched { info } => {
            w.push(5);
            enc_channel(w, info.new_primary_channel);
            put_u32(w, info.bandwidth.into_primitive());
            enc_channel(w, info.vht_secondary_80_channel);
        }
    }
    Ok(())
}
fn dec_event(r: &mut Reader<'_>) -> Result<sme::ConnectTransactionEvent, Error> {
    Ok(match r.u8()? {
        1 => sme::ConnectTransactionEvent::OnConnectResult {
            result: dec_connect_result(r)?,
        },
        2 => sme::ConnectTransactionEvent::OnDisconnect {
            info: dec_disconnect_info(r)?,
        },
        3 => sme::ConnectTransactionEvent::OnRoamResult {
            result: dec_roam_result(r)?,
        },
        4 => sme::ConnectTransactionEvent::OnSignalReport {
            ind: internal::SignalReportIndication {
                rssi_dbm: r.u8()? as i8,
                snr_db: r.u8()? as i8,
            },
        },
        5 => {
            let new_primary_channel = dec_channel(r)?;
            let raw = r.u32()?;
            let bandwidth = ieee::ChannelBandwidth::from_primitive(raw)
                .ok_or(Error::UnknownDiscriminant("ChannelBandwidth", raw.into()))?;
            let vht_secondary_80_channel = dec_channel(r)?;
            sme::ConnectTransactionEvent::OnChannelSwitched {
                info: internal::ChannelSwitchInfo {
                    new_primary_channel,
                    bandwidth,
                    vht_secondary_80_channel,
                },
            }
        }
        n => {
            return Err(Error::UnknownDiscriminant(
                "ConnectTransactionEvent",
                n.into(),
            ));
        }
    })
}

fn enc_disconnect_info(w: &mut Vec<u8>, v: &sme::DisconnectInfo) {
    put_bool(w, v.is_sme_reconnecting);
    match v.disconnect_source {
        sme::DisconnectSource::Ap(c) => {
            w.push(1);
            enc_cause(w, c);
        }
        sme::DisconnectSource::User(r) => {
            w.push(2);
            put_u32(w, r as u32);
        }
        sme::DisconnectSource::Mlme(c) => {
            w.push(3);
            enc_cause(w, c);
        }
    }
}
fn dec_disconnect_info(r: &mut Reader<'_>) -> Result<sme::DisconnectInfo, Error> {
    let is_sme_reconnecting = r.bool()?;
    let disconnect_source = match r.u8()? {
        1 => sme::DisconnectSource::Ap(dec_cause(r)?),
        2 => sme::DisconnectSource::User(dec_disconnect_reason(r.u32()?)?),
        3 => sme::DisconnectSource::Mlme(dec_cause(r)?),
        n => return Err(Error::UnknownDiscriminant("DisconnectSource", n.into())),
    };
    Ok(sme::DisconnectInfo {
        is_sme_reconnecting,
        disconnect_source,
    })
}
fn enc_cause(w: &mut Vec<u8>, c: sme::DisconnectCause) {
    put_u32(w, c.mlme_event_name as u32);
    put_u16(w, c.reason_code.into_primitive());
}
fn dec_cause(r: &mut Reader<'_>) -> Result<sme::DisconnectCause, Error> {
    let n = r.u32()?;
    let mlme_event_name = match n {
        1 => sme::DisconnectMlmeEventName::DeauthenticateIndication,
        2 => sme::DisconnectMlmeEventName::DisassociateIndication,
        3 => sme::DisconnectMlmeEventName::RoamStartIndication,
        4 => sme::DisconnectMlmeEventName::RoamResultIndication,
        5 => sme::DisconnectMlmeEventName::SaeHandshakeResponse,
        6 => sme::DisconnectMlmeEventName::RoamRequest,
        7 => sme::DisconnectMlmeEventName::RoamConfirmation,
        _ => {
            return Err(Error::UnknownDiscriminant(
                "DisconnectMlmeEventName",
                n.into(),
            ));
        }
    };
    let raw = r.u16()?;
    let reason_code = ieee::ReasonCode::from_primitive(raw)
        .ok_or(Error::UnknownDiscriminant("ReasonCode", raw.into()))?;
    Ok(sme::DisconnectCause {
        mlme_event_name,
        reason_code,
    })
}

fn enc_roam_result(w: &mut Vec<u8>, v: &sme::RoamResult) -> Result<(), Error> {
    w.extend_from_slice(&v.bssid);
    put_u16(w, v.status_code.into_primitive());
    put_bool(w, v.original_association_maintained);
    enc_option(w, v.bss_description.as_deref(), enc_bss)?;
    enc_option(w, v.disconnect_info.as_deref(), |w, i| {
        enc_disconnect_info(w, i);
        Ok(())
    })?;
    put_bool(w, v.is_credential_rejected);
    Ok(())
}
fn dec_roam_result(r: &mut Reader<'_>) -> Result<sme::RoamResult, Error> {
    let bssid = r.array()?;
    let raw = r.u16()?;
    let status_code = ieee::StatusCode::from_primitive(raw)
        .ok_or(Error::UnknownDiscriminant("StatusCode", raw.into()))?;
    let original_association_maintained = r.bool()?;
    let bss_description = dec_option(r, dec_bss)?.map(Box::new);
    let disconnect_info = dec_option(r, dec_disconnect_info)?.map(Box::new);
    let is_credential_rejected = r.bool()?;
    Ok(sme::RoamResult {
        bssid,
        status_code,
        original_association_maintained,
        bss_description,
        disconnect_info,
        is_credential_rejected,
    })
}

fn enc_option<T>(
    w: &mut Vec<u8>,
    v: Option<&T>,
    f: impl FnOnce(&mut Vec<u8>, &T) -> Result<(), Error>,
) -> Result<(), Error> {
    match v {
        None => w.push(0),
        Some(v) => {
            w.push(1);
            f(w, v)?;
        }
    }
    Ok(())
}
fn dec_option<T>(
    r: &mut Reader<'_>,
    f: impl FnOnce(&mut Reader<'_>) -> Result<T, Error>,
) -> Result<Option<T>, Error> {
    match r.u8()? {
        0 => Ok(None),
        1 => Ok(Some(f(r)?)),
        n => Err(Error::UnknownDiscriminant("Option", n.into())),
    }
}

fn dec_disconnect_reason(n: u32) -> Result<sme::UserDisconnectReason, Error> {
    Ok(match n {
        0 => sme::UserDisconnectReason::Unknown,
        1 => sme::UserDisconnectReason::FailedToConnect,
        2 => sme::UserDisconnectReason::FidlConnectRequest,
        3 => sme::UserDisconnectReason::FidlStopClientConnectionsRequest,
        4 => sme::UserDisconnectReason::ProactiveNetworkSwitch,
        5 => sme::UserDisconnectReason::DisconnectDetectedFromSme,
        6 => sme::UserDisconnectReason::RegulatoryRegionChange,
        7 => sme::UserDisconnectReason::Startup,
        8 => sme::UserDisconnectReason::NetworkUnsaved,
        9 => sme::UserDisconnectReason::NetworkConfigUpdated,
        10 => sme::UserDisconnectReason::Recovery,
        124 => sme::UserDisconnectReason::WlanstackUnitTesting,
        125 => sme::UserDisconnectReason::WlanSmeUnitTesting,
        126 => sme::UserDisconnectReason::WlanServiceUtilTesting,
        127 => sme::UserDisconnectReason::WlanDevTool,
        _ => return Err(Error::UnknownDiscriminant("UserDisconnectReason", n.into())),
    })
}
fn command_reply(v: CommandReply) -> u8 {
    match v {
        CommandReply::Success => 1,
        CommandReply::Busy => 2,
        CommandReply::NotConnected => 3,
    }
}
fn dec_command_reply(n: u8) -> Result<CommandReply, Error> {
    Ok(match n {
        1 => CommandReply::Success,
        2 => CommandReply::Busy,
        3 => CommandReply::NotConnected,
        _ => return Err(Error::UnknownDiscriminant("CommandReply", n.into())),
    })
}
fn generation_end(v: GenerationEndReason) -> u8 {
    match v {
        GenerationEndReason::Shutdown => 1,
        GenerationEndReason::Timeout => 2,
        GenerationEndReason::DriverFault => 3,
        GenerationEndReason::ContainmentFault => 4,
        GenerationEndReason::Backpressure => 5,
        GenerationEndReason::ProtocolViolation => 6,
    }
}
fn dec_generation_end(n: u8) -> Result<GenerationEndReason, Error> {
    Ok(match n {
        1 => GenerationEndReason::Shutdown,
        2 => GenerationEndReason::Timeout,
        3 => GenerationEndReason::DriverFault,
        4 => GenerationEndReason::ContainmentFault,
        5 => GenerationEndReason::Backpressure,
        6 => GenerationEndReason::ProtocolViolation,
        _ => return Err(Error::UnknownDiscriminant("GenerationEndReason", n.into())),
    })
}

fn put_bool(w: &mut Vec<u8>, v: bool) {
    w.push(u8::from(v));
}
fn put_u16(w: &mut Vec<u8>, v: u16) {
    w.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(w: &mut Vec<u8>, v: u32) {
    w.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(w: &mut Vec<u8>, v: u64) {
    w.extend_from_slice(&v.to_le_bytes());
}
fn put_vec(w: &mut Vec<u8>, v: &[u8], max: usize, name: &'static str) -> Result<(), Error> {
    if v.len() > max {
        return Err(Error::BoundExceeded(name));
    }
    put_u32(w, v.len() as u32);
    w.extend_from_slice(v);
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn done(&self) -> bool {
        self.pos == self.bytes.len()
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        let v = self.bytes.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(v)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.take(N)?.try_into().expect("length checked"))
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn bool(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            n => Err(Error::UnknownDiscriminant("bool", n.into())),
        }
    }
    fn vec(&mut self, max: usize, name: &'static str) -> Result<Vec<u8>, Error> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(Error::BoundExceeded(name));
        }
        Ok(self.take(n)?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(number: u8) -> ieee::ChannelNumber {
        ieee::ChannelNumber {
            band: ieee::WlanBand::FiveGhz,
            number,
        }
    }
    fn bss(ies: Vec<u8>) -> ieee::BssDescription {
        ieee::BssDescription {
            bssid: [1, 2, 3, 4, 5, 6],
            bss_type: ieee::BssType::Infrastructure,
            beacon_period: 100,
            capability_info: 0x1234,
            ies,
            primary: channel(36),
            bandwidth: ieee::ChannelBandwidth::Cbw80,
            vht_secondary_80_channel: channel(0),
            rssi_dbm: -42,
            snr_db: 31,
        }
    }
    fn connect() -> sme::ConnectRequest {
        sme::ConnectRequest {
            ssid: b"secret-network".to_vec(),
            bss_description: bss(vec![0, 2, b'a', b'p']),
            multiple_bss_candidates: true,
            authentication: internal::Authentication {
                protocol: internal::Protocol::Wpa3Personal,
                credentials: Some(Box::new(internal::Credentials::Wpa(
                    internal::WpaCredentials::Passphrase(b"do-not-log-me".to_vec()),
                ))),
            },
            deprecated_scan_type: ScanType::Active,
        }
    }
    fn info() -> sme::DisconnectInfo {
        sme::DisconnectInfo {
            is_sme_reconnecting: true,
            disconnect_source: sme::DisconnectSource::Ap(sme::DisconnectCause {
                mlme_event_name: sme::DisconnectMlmeEventName::DeauthenticateIndication,
                reason_code: ieee::ReasonCode::MicFailure,
            }),
        }
    }
    fn packet(message: Message, id: u64) -> Packet {
        Packet {
            generation: [9; 16],
            request_id: id,
            message,
        }
    }
    fn reply<T>(result: T) -> Reply<T> {
        Reply {
            in_reply_to: 42,
            result,
        }
    }

    fn roundtrip(message: Message) {
        let expected = packet(message, 7);
        let encoded = encode(&expected).unwrap();
        assert!(encoded.len() <= MAX_PACKET);
        assert_eq!(&encoded[..4], b"WLCP");
        assert_eq!(
            encoded.len(),
            HEADER_LEN + u32::from_le_bytes(encoded[8..12].try_into().unwrap()) as usize
        );
        assert_eq!(decode(&encoded).unwrap(), expected);
    }

    #[test]
    fn roundtrips_every_top_level_kind_and_policy_messages_are_fd_free() {
        let scan_result = sme::ScanResult {
            compatibility: sme::Compatibility::Compatible(sme::Compatible {
                mutual_security_protocols: vec![
                    internal::Protocol::Wpa2Personal,
                    internal::Protocol::Wpa3Personal,
                ],
            }),
            timestamp_nanos: -123,
            bss_description: bss(vec![1, 2, 3]),
        };
        let messages = vec![
            Message::Ready,
            Message::Scan(sme::ScanRequest::Active(sme::ActiveScanRequest {
                ssids: vec![b"one".to_vec(), b"two".to_vec()],
                channels: vec![1, 36],
            })),
            Message::Scan(sme::ScanRequest::Passive(sme::PassiveScanRequest {
                channels: vec![],
            })),
            Message::ScanReply(reply(Ok(vec![scan_result]))),
            Message::ScanReply(reply(Err(sme::ScanErrorCode::ShouldWait))),
            Message::Connect(connect()),
            Message::ConnectReply(reply(ConnectReply::Completed(sme::ConnectResult {
                code: ieee::StatusCode::EstablishRsnaFailure,
                is_credential_rejected: true,
                is_reconnect: false,
            }))),
            Message::Disconnect(sme::UserDisconnectReason::ProactiveNetworkSwitch),
            Message::DisconnectReply(reply(CommandReply::Success)),
            Message::Roam(sme::RoamRequest {
                bss_description: bss(vec![]),
            }),
            Message::RoamReply(reply(CommandReply::NotConnected)),
            Message::GenerationEnd(GenerationEndReason::Shutdown),
            Message::GenerationEnd(GenerationEndReason::Timeout),
            Message::GenerationEnd(GenerationEndReason::DriverFault),
            Message::GenerationEnd(GenerationEndReason::ContainmentFault),
            Message::GenerationEnd(GenerationEndReason::Backpressure),
            Message::GenerationEnd(GenerationEndReason::ProtocolViolation),
        ];
        for message in messages {
            assert_eq!(required_fd_count(&message), 0);
            roundtrip(message);
        }
    }

    #[test]
    fn roundtrips_all_exact_sme_event_variants() {
        let roam = sme::RoamResult {
            bssid: [8; 6],
            status_code: ieee::StatusCode::RefusedTemporarily,
            original_association_maintained: true,
            bss_description: Some(Box::new(bss(vec![7; 20]))),
            disconnect_info: Some(Box::new(info())),
            is_credential_rejected: false,
        };
        for event in [
            sme::ConnectTransactionEvent::OnConnectResult {
                result: sme::ConnectResult {
                    code: ieee::StatusCode::Success,
                    is_credential_rejected: false,
                    is_reconnect: true,
                },
            },
            sme::ConnectTransactionEvent::OnDisconnect { info: info() },
            sme::ConnectTransactionEvent::OnRoamResult { result: roam },
            sme::ConnectTransactionEvent::OnSignalReport {
                ind: internal::SignalReportIndication {
                    rssi_dbm: -70,
                    snr_db: -2,
                },
            },
            sme::ConnectTransactionEvent::OnChannelSwitched {
                info: internal::ChannelSwitchInfo {
                    new_primary_channel: channel(149),
                    bandwidth: ieee::ChannelBandwidth::Cbw40Below,
                    vht_secondary_80_channel: channel(0),
                },
            },
        ] {
            roundtrip(Message::Event(event));
        }
    }

    #[test]
    fn exact_header_layout_is_little_endian_and_unpadded() {
        let bytes = encode(&Packet {
            generation: [0xab; 16],
            request_id: 0x0102_0304_0506_0708,
            message: Message::Ready,
        })
        .unwrap();
        assert_eq!(bytes.len(), 36);
        assert_eq!(&bytes[4..6], &[1, 0]);
        assert_eq!(&bytes[6..8], &[1, 0]);
        assert_eq!(&bytes[8..12], &[0; 4]);
        assert_eq!(&bytes[12..28], &[0xab; 16]);
        assert_eq!(&bytes[28..36], &[8, 7, 6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn rejects_lengths_trailing_data_magic_version_kind_and_zero_id() {
        let valid = encode(&packet(Message::Ready, 1)).unwrap();
        assert_eq!(decode(&valid[..35]), Err(Error::Truncated));
        let mut bad = valid.clone();
        bad[0] = 0;
        assert_eq!(decode(&bad), Err(Error::BadMagic));
        let mut bad = valid.clone();
        bad[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(decode(&bad), Err(Error::UnsupportedVersion(2)));
        let mut bad = valid.clone();
        bad[6..8].copy_from_slice(&99u16.to_le_bytes());
        assert_eq!(decode(&bad), Err(Error::UnknownKind(99)));
        let mut bad = valid.clone();
        bad[28..36].fill(0);
        assert_eq!(decode(&bad), Err(Error::ZeroRequestId));
        let mut bad = valid.clone();
        bad.push(0);
        assert_eq!(decode(&bad), Err(Error::InvalidLength));
        let mut bad = valid.clone();
        bad[8..12].copy_from_slice(&1u32.to_le_bytes());
        bad.push(0);
        assert_eq!(decode(&bad), Err(Error::TrailingBytes));
        let oversized = vec![0; MAX_PACKET + 1];
        assert_eq!(decode(&oversized), Err(Error::PacketTooLarge));
    }

    #[test]
    fn rejects_unknown_nested_discriminants_and_noncanonical_bool() {
        let mut bytes = encode(&packet(
            Message::Disconnect(sme::UserDisconnectReason::Startup),
            1,
        ))
        .unwrap();
        bytes[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&11u32.to_le_bytes());
        assert!(matches!(
            decode(&bytes),
            Err(Error::UnknownDiscriminant("UserDisconnectReason", 11))
        ));
        let mut bytes = encode(&packet(Message::Connect(connect()), 1)).unwrap();
        // SSID length + SSID, BSS through the two signed signal bytes, then boolean.
        let bool_at =
            HEADER_LEN + 4 + b"secret-network".len() + 6 + 4 + 2 + 2 + 4 + 4 + 2 + 4 + 2 + 2;
        bytes[bool_at] = 2;
        assert_eq!(decode(&bytes), Err(Error::UnknownDiscriminant("bool", 2)));
    }

    #[test]
    fn enforces_ie_and_packet_bounds_without_truncation() {
        let too_many_ies = Message::Roam(sme::RoamRequest {
            bss_description: bss(vec![0; MAX_BSS_IE_LEN + 1]),
        });
        assert_eq!(
            encode(&packet(too_many_ies, 1)),
            Err(Error::BoundExceeded("BSS IEs"))
        );
        let maximum = Message::Roam(sme::RoamRequest {
            bss_description: bss(vec![0; MAX_BSS_IE_LEN]),
        });
        roundtrip(maximum);
        let many = (0..3)
            .map(|_| sme::ScanResult {
                compatibility: sme::Compatibility::Compatible(sme::Compatible {
                    mutual_security_protocols: vec![],
                }),
                timestamp_nanos: 0,
                bss_description: bss(vec![0; MAX_BSS_IE_LEN]),
            })
            .collect();
        assert_eq!(
            encode(&packet(Message::ScanReply(reply(Ok(many))), 1)),
            Err(Error::PacketTooLarge)
        );
    }

    #[test]
    fn session_enforces_generation_and_request_order() {
        let mut session = SessionValidator::new([9; 16]);
        session.validate(&packet(Message::Ready, 1)).unwrap();
        assert_eq!(
            session.validate(&packet(Message::Ready, 1)),
            Err(Error::NonIncreasingRequestId)
        );
        assert_eq!(
            session.validate(&Packet {
                generation: [8; 16],
                request_id: 2,
                message: Message::Ready
            }),
            Err(Error::WrongGeneration)
        );
        session.validate(&packet(Message::Ready, 2)).unwrap();
    }

    #[test]
    fn replies_correlate_explicitly_while_events_interleave() {
        let mut inbound_from_server = SessionValidator::new([9; 16]);
        let first_reply = packet(
            Message::DisconnectReply(Reply {
                in_reply_to: 40,
                result: CommandReply::Success,
            }),
            1,
        );
        let event = packet(
            Message::Event(sme::ConnectTransactionEvent::OnSignalReport {
                ind: internal::SignalReportIndication {
                    rssi_dbm: -55,
                    snr_db: 20,
                },
            }),
            2,
        );
        let second_reply = packet(
            Message::RoamReply(Reply {
                in_reply_to: 41,
                result: CommandReply::Success,
            }),
            3,
        );
        for packet in [&first_reply, &event, &second_reply] {
            inbound_from_server.validate(packet).unwrap();
        }
        assert_eq!(first_reply.message.in_reply_to(), Some(40));
        assert_eq!(event.message.in_reply_to(), None);
        assert_eq!(second_reply.message.in_reply_to(), Some(41));

        assert_eq!(
            encode(&packet(
                Message::DisconnectReply(Reply {
                    in_reply_to: 0,
                    result: CommandReply::Success,
                }),
                4,
            )),
            Err(Error::ZeroRequestId)
        );
    }

    #[test]
    fn debug_redacts_credentials() {
        let rendered = format!("{:?}", Message::Connect(connect()));
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("do-not-log-me"));
    }

    #[test]
    fn every_closed_protocol_discriminant_roundtrips() {
        for reason in [
            sme::UserDisconnectReason::Unknown,
            sme::UserDisconnectReason::FailedToConnect,
            sme::UserDisconnectReason::FidlConnectRequest,
            sme::UserDisconnectReason::FidlStopClientConnectionsRequest,
            sme::UserDisconnectReason::ProactiveNetworkSwitch,
            sme::UserDisconnectReason::DisconnectDetectedFromSme,
            sme::UserDisconnectReason::RegulatoryRegionChange,
            sme::UserDisconnectReason::Startup,
            sme::UserDisconnectReason::NetworkUnsaved,
            sme::UserDisconnectReason::NetworkConfigUpdated,
            sme::UserDisconnectReason::Recovery,
            sme::UserDisconnectReason::WlanstackUnitTesting,
            sme::UserDisconnectReason::WlanSmeUnitTesting,
            sme::UserDisconnectReason::WlanServiceUtilTesting,
            sme::UserDisconnectReason::WlanDevTool,
        ] {
            roundtrip(Message::Disconnect(reason));
        }
        for reply in [
            CommandReply::Success,
            CommandReply::Busy,
            CommandReply::NotConnected,
        ] {
            roundtrip(Message::DisconnectReply(self::reply(reply)));
            roundtrip(Message::RoamReply(self::reply(reply)));
        }
        for error in [
            sme::ScanErrorCode::NotSupported,
            sme::ScanErrorCode::InternalError,
            sme::ScanErrorCode::InternalMlmeError,
            sme::ScanErrorCode::ShouldWait,
            sme::ScanErrorCode::CanceledByDriverOrFirmware,
        ] {
            roundtrip(Message::ScanReply(reply(Err(error))));
        }
    }

    #[test]
    fn credentials_and_incompatible_scan_values_preserve_exact_shapes() {
        let mut request = connect();
        request.authentication = internal::Authentication {
            protocol: internal::Protocol::Wep,
            credentials: Some(Box::new(internal::Credentials::Wep(
                internal::WepCredentials {
                    key: vec![1, 2, 3, 4, 5],
                },
            ))),
        };
        roundtrip(Message::Connect(request));
        let mut request = connect();
        request.authentication.credentials = Some(Box::new(internal::Credentials::Wpa(
            internal::WpaCredentials::Psk([0xa5; 32]),
        )));
        roundtrip(Message::Connect(request));
        let result = sme::ScanResult {
            compatibility: sme::Compatibility::Incompatible(sme::Incompatible {
                description: "enterprise role mismatch".into(),
                disjoint_security_protocols: Some(vec![sme::DisjointSecurityProtocol {
                    protocol: internal::Protocol::Wpa3Enterprise,
                    role: WlanMacRole::Client,
                }]),
            }),
            timestamp_nanos: i64::MAX,
            bss_description: bss(vec![]),
        };
        roundtrip(Message::ScanReply(reply(Ok(vec![result]))));
    }
}
