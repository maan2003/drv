//! Provider ABI v2: unambiguous POSIX socket operations for a host adapter.
//!
//! Version 1 remains available only as an experimental, incompatible spike.
//! V2 is deliberately a new wire version: a v1 endpoint must reject it.

use crate::provider_transport::{
    MAX_PROVIDER_PAYLOAD, PROVIDER_HEADER_LEN, ProviderFrameError, ProviderFrameType,
    ProviderIdentity,
};

pub const PROVIDER_ABI_V2: u16 = 2;
const MAGIC: [u8; 4] = *b"NS3P";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ProviderOpcodeV2 {
    OpenClient = 1,
    CloseClient = 2,
    OpenSocket = 3,
    Bind = 4,
    Connect = 5,
    Disconnect = 6,
    Listen = 7,
    Accept = 8,
    SendMsg = 9,
    RecvMsg = 10,
    Shutdown = 11,
    GetName = 12,
    GetSocketError = 13,
    SetOption = 14,
    GetOption = 15,
    Readiness = 16,
    Close = 17,
    ReadinessChanged = 18,
}

impl TryFrom<u16> for ProviderOpcodeV2 {
    type Error = ProviderFrameError;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Ok(match value {
            1 => Self::OpenClient,
            2 => Self::CloseClient,
            3 => Self::OpenSocket,
            4 => Self::Bind,
            5 => Self::Connect,
            6 => Self::Disconnect,
            7 => Self::Listen,
            8 => Self::Accept,
            9 => Self::SendMsg,
            10 => Self::RecvMsg,
            11 => Self::Shutdown,
            12 => Self::GetName,
            13 => Self::GetSocketError,
            14 => Self::SetOption,
            15 => Self::GetOption,
            16 => Self::Readiness,
            17 => Self::Close,
            18 => Self::ReadinessChanged,
            _ => return Err(ProviderFrameError::UnknownOpcode),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderFrameV2 {
    pub frame_type: ProviderFrameType,
    pub opcode: ProviderOpcodeV2,
    pub request_id: u64,
    pub payload: Vec<u8>,
}

pub struct ProviderFramedEndpointV2 {
    identity: ProviderIdentity,
    max_payload: usize,
}

impl ProviderFramedEndpointV2 {
    pub fn new(identity: ProviderIdentity, max_payload: usize) -> Result<Self, ProviderFrameError> {
        if max_payload > MAX_PROVIDER_PAYLOAD {
            return Err(ProviderFrameError::PayloadTooLarge);
        }
        Ok(Self {
            identity,
            max_payload,
        })
    }

    pub fn identity(&self) -> ProviderIdentity {
        self.identity
    }

    /// Constructs the identity-fixed endpoint for one frame received from a
    /// multiplexed provider device. The complete frame is validated before
    /// the header identity is accepted.
    pub fn from_received_frame(
        bytes: &[u8],
        max_payload: usize,
    ) -> Result<Self, ProviderFrameError> {
        if bytes.len() < PROVIDER_HEADER_LEN {
            return Err(ProviderFrameError::Truncated);
        }
        let u64_at = |offset| {
            u64::from_le_bytes(
                bytes[offset..offset + 8]
                    .try_into()
                    .expect("header length checked"),
            )
        };
        let endpoint = Self::new(
            ProviderIdentity {
                namespace: crate::provider_transport::ProviderNamespaceId::from_raw(u64_at(12)),
                client: crate::SocketClientId::from_raw(u64_at(20)),
            },
            max_payload,
        )?;
        endpoint.decode(bytes)?;
        Ok(endpoint)
    }

    pub fn encode(&self, frame: &ProviderFrameV2) -> Result<Vec<u8>, ProviderFrameError> {
        if frame.payload.len() > self.max_payload {
            return Err(ProviderFrameError::PayloadTooLarge);
        }
        let mut bytes = Vec::with_capacity(PROVIDER_HEADER_LEN + frame.payload.len());
        bytes.extend(MAGIC);
        bytes.extend(PROVIDER_ABI_V2.to_le_bytes());
        bytes.extend((frame.frame_type as u16).to_le_bytes());
        bytes.extend((frame.opcode as u16).to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        bytes.extend(self.identity.namespace.into_raw().to_le_bytes());
        bytes.extend(self.identity.client.into_raw().to_le_bytes());
        bytes.extend(frame.request_id.to_le_bytes());
        bytes.extend((frame.payload.len() as u32).to_le_bytes());
        bytes.extend(&frame.payload);
        Ok(bytes)
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<ProviderFrameV2, ProviderFrameError> {
        if bytes.len() < PROVIDER_HEADER_LEN {
            return Err(ProviderFrameError::Truncated);
        }
        if bytes[..4] != MAGIC {
            return Err(ProviderFrameError::BadMagic);
        }
        let u16_at = |o| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
        let u32_at = |o| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let u64_at = |o| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
        if u16_at(4) != PROVIDER_ABI_V2 {
            return Err(ProviderFrameError::UnsupportedVersion);
        }
        let frame_type = ProviderFrameType::try_from(u16_at(6))?;
        let opcode = ProviderOpcodeV2::try_from(u16_at(8))?;
        if u16_at(10) != 0 {
            return Err(ProviderFrameError::ReservedBits);
        }
        if u64_at(12) != self.identity.namespace.into_raw()
            || u64_at(20) != self.identity.client.into_raw()
        {
            return Err(ProviderFrameError::IdentityMismatch);
        }
        let len = u32_at(36) as usize;
        if len > self.max_payload {
            return Err(ProviderFrameError::PayloadTooLarge);
        }
        if bytes.len() != PROVIDER_HEADER_LEN + len {
            return Err(ProviderFrameError::LengthMismatch);
        }
        Ok(ProviderFrameV2 {
            frame_type,
            opcode,
            request_id: u64_at(28),
            payload: bytes[PROVIDER_HEADER_LEN..].to_vec(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProviderSocketState {
    Unbound = 1,
    Bound = 2,
    Connecting = 3,
    Connected = 4,
    Listening = 5,
    ReadClosed = 6,
    WriteClosed = 7,
    Closed = 8,
}

pub struct ProviderOperationState {
    pub opcode: ProviderOpcodeV2,
    pub allowed: &'static [ProviderSocketState],
    pub next: Option<ProviderSocketState>,
}

use ProviderSocketState as S;
pub const PROVIDER_V2_STATE_TABLE: &[ProviderOperationState] = &[
    ProviderOperationState {
        opcode: ProviderOpcodeV2::OpenSocket,
        allowed: &[],
        next: Some(S::Unbound),
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::Bind,
        allowed: &[S::Unbound],
        next: Some(S::Bound),
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::Connect,
        allowed: &[S::Unbound, S::Bound],
        next: Some(S::Connecting),
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::Disconnect,
        allowed: &[S::Connected],
        next: Some(S::Bound),
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::Listen,
        allowed: &[S::Unbound, S::Bound],
        next: Some(S::Listening),
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::Accept,
        allowed: &[S::Listening],
        next: None,
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::SendMsg,
        allowed: &[S::Unbound, S::Bound, S::Connected, S::ReadClosed],
        next: None,
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::RecvMsg,
        allowed: &[S::Bound, S::Connected, S::WriteClosed],
        next: None,
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::Shutdown,
        allowed: &[S::Connected, S::ReadClosed, S::WriteClosed],
        next: None,
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::GetName,
        allowed: &[
            S::Unbound,
            S::Bound,
            S::Connecting,
            S::Connected,
            S::Listening,
            S::ReadClosed,
            S::WriteClosed,
        ],
        next: None,
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::GetSocketError,
        allowed: &[
            S::Unbound,
            S::Bound,
            S::Connecting,
            S::Connected,
            S::Listening,
            S::ReadClosed,
            S::WriteClosed,
        ],
        next: None,
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::SetOption,
        allowed: &[
            S::Unbound,
            S::Bound,
            S::Connecting,
            S::Connected,
            S::Listening,
        ],
        next: None,
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::GetOption,
        allowed: &[
            S::Unbound,
            S::Bound,
            S::Connecting,
            S::Connected,
            S::Listening,
            S::ReadClosed,
            S::WriteClosed,
        ],
        next: None,
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::Readiness,
        allowed: &[
            S::Unbound,
            S::Bound,
            S::Connecting,
            S::Connected,
            S::Listening,
            S::ReadClosed,
            S::WriteClosed,
        ],
        next: None,
    },
    ProviderOperationState {
        opcode: ProviderOpcodeV2::Close,
        allowed: &[
            S::Unbound,
            S::Bound,
            S::Connecting,
            S::Connected,
            S::Listening,
            S::ReadClosed,
            S::WriteClosed,
        ],
        next: Some(S::Closed),
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderReadinessV2(pub u16);
impl ProviderReadinessV2 {
    pub const READABLE: u16 = 1 << 0;
    pub const WRITABLE: u16 = 1 << 1;
    pub const INCOMING: u16 = 1 << 2;
    pub const READ_CLOSED: u16 = 1 << 3;
    pub const WRITE_CLOSED: u16 = 1 << 4;
    pub const ERROR: u16 = 1 << 5;
    pub const CONNECTED: u16 = 1 << 6;
    pub const CONNECT_FAILED: u16 = 1 << 7;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderSocketAddressV2 {
    pub address: crate::RemoteIpAddress,
    pub port: u16,
}

impl ProviderSocketAddressV2 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self.address {
            crate::RemoteIpAddress::V4(a) => {
                out.push(4);
                out.extend(a);
            }
            crate::RemoteIpAddress::V6(a) => {
                out.push(6);
                out.extend(a);
            }
        }
        out.extend(self.port.to_le_bytes());
    }
}

/// Successful RecvMsg result. Absence is represented by the normal WouldBlock
/// error envelope, never by this type, so an empty datagram remains observable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRecvMsgV2 {
    pub source: Option<ProviderSocketAddressV2>,
    pub original_len: u32,
    pub flags: u32,
    pub eof: bool,
    pub data: Vec<u8>,
}

impl ProviderRecvMsgV2 {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(self.eof.into());
        out.extend(self.flags.to_le_bytes());
        out.extend(self.original_len.to_le_bytes());
        match &self.source {
            None => out.push(0),
            Some(source) => source.encode(&mut out),
        }
        out.extend(&self.data);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SocketClientId,
        provider_transport::{
            ProviderFrame, ProviderFramedEndpoint, ProviderNamespaceId, ProviderOpcode,
        },
    };

    fn identity() -> ProviderIdentity {
        ProviderIdentity {
            namespace: ProviderNamespaceId::from_raw(0x0102_0304_0506_0708),
            client: SocketClientId::from_raw(0x1112_1314_1516_1718),
        }
    }

    #[test]
    fn v2_golden_header_and_readiness_payload_are_stable() {
        let ep = ProviderFramedEndpointV2::new(identity(), 64).unwrap();
        let bytes = ep
            .encode(&ProviderFrameV2 {
                frame_type: ProviderFrameType::ReadinessEvent,
                opcode: ProviderOpcodeV2::ReadinessChanged,
                request_id: 9,
                payload: vec![0x41, 0],
            })
            .unwrap();
        assert_eq!(
            &bytes[..12],
            &[b'N', b'S', b'3', b'P', 2, 0, 3, 0, 18, 0, 0, 0]
        );
        assert_eq!(ep.decode(&bytes).unwrap().payload, vec![0x41, 0]);
    }

    #[test]
    fn v1_and_v2_reject_each_other() {
        let v1 = ProviderFramedEndpoint::new(identity(), 64).unwrap();
        let v2 = ProviderFramedEndpointV2::new(identity(), 64).unwrap();
        let old = v1
            .encode(&ProviderFrame {
                frame_type: ProviderFrameType::Request,
                opcode: ProviderOpcode::UdpSocket,
                request_id: 1,
                payload: vec![4],
            })
            .unwrap();
        let new = v2
            .encode(&ProviderFrameV2 {
                frame_type: ProviderFrameType::Request,
                opcode: ProviderOpcodeV2::OpenSocket,
                request_id: 1,
                payload: vec![1, 4],
            })
            .unwrap();
        assert_eq!(v2.decode(&old), Err(ProviderFrameError::UnsupportedVersion));
        assert_eq!(v1.decode(&new), Err(ProviderFrameError::UnsupportedVersion));
    }

    #[test]
    fn malformed_v2_identity_reserved_and_length_are_rejected() {
        let ep = ProviderFramedEndpointV2::new(identity(), 64).unwrap();
        let mut bytes = ep
            .encode(&ProviderFrameV2 {
                frame_type: ProviderFrameType::Request,
                opcode: ProviderOpcodeV2::Bind,
                request_id: 2,
                payload: vec![],
            })
            .unwrap();
        bytes[10] = 1;
        assert_eq!(ep.decode(&bytes), Err(ProviderFrameError::ReservedBits));
        bytes[10] = 0;
        bytes[12] ^= 1;
        assert_eq!(ep.decode(&bytes), Err(ProviderFrameError::IdentityMismatch));
        bytes[12] ^= 1;
        bytes[36] = 1;
        assert_eq!(ep.decode(&bytes), Err(ProviderFrameError::LengthMismatch));
    }

    #[test]
    fn empty_udp_datagram_is_not_no_datagram() {
        let empty = ProviderRecvMsgV2 {
            source: Some(ProviderSocketAddressV2 {
                address: crate::RemoteIpAddress::V4([10, 0, 0, 1]),
                port: 53,
            }),
            original_len: 0,
            flags: 0,
            eof: false,
            data: vec![],
        }
        .encode();
        assert!(!empty.is_empty());
        assert_eq!(empty[0], 0);
        // No message is the stable WouldBlock error response [error, code=7].
        assert_ne!(empty, vec![1, 7]);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProviderSocketKindV2 {
    Udp = 1,
    Tcp = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProviderShutdownV2 {
    Read = 1,
    Write = 2,
    ReadWrite = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProviderNameV2 {
    Local = 1,
    Peer = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProviderOptionV2 {
    ReuseAddress = 1,
    ReusePort = 2,
    Broadcast = 3,
    KeepAlive = 4,
    ReceiveBuffer = 5,
    SendBuffer = 6,
    Linger = 7,
    TcpNoDelay = 8,
    Ipv6Only = 9,
    TcpKeepIdle = 10,
    TcpKeepInterval = 11,
    TcpKeepCount = 12,
}

impl TryFrom<u8> for ProviderOptionV2 {
    type Error = crate::RemoteSocketError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Ok(match value {
            1 => Self::ReuseAddress,
            2 => Self::ReusePort,
            3 => Self::Broadcast,
            4 => Self::KeepAlive,
            5 => Self::ReceiveBuffer,
            6 => Self::SendBuffer,
            7 => Self::Linger,
            8 => Self::TcpNoDelay,
            9 => Self::Ipv6Only,
            10 => Self::TcpKeepIdle,
            11 => Self::TcpKeepInterval,
            12 => Self::TcpKeepCount,
            _ => return Err(crate::RemoteSocketError::NotSupported),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderReadinessSnapshotV2 {
    pub sequence: u64,
    pub readiness: ProviderReadinessV2,
    pub error: Option<crate::RemoteSocketError>,
}
