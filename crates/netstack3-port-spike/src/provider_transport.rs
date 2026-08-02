//! Stable bounded framing for a later host-kernel socket provider.
//!
//! This transport carries capability requests only. It contains no TCP/IP,
//! socket state, fd table, or loopback implementation.

use std::collections::VecDeque;
use std::num::NonZeroUsize;

use crate::{RemoteIpAddress, SocketClientId};

pub const PROVIDER_ABI_VERSION: u16 = 1;
pub const PROVIDER_HEADER_LEN: usize = 40;
pub const MAX_PROVIDER_PAYLOAD: usize = 64 * 1024;
const MAGIC: [u8; 4] = *b"NS3P";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProviderNamespaceId(u64);

impl ProviderNamespaceId {
    pub fn from_raw(id: u64) -> Self {
        Self(id)
    }
    pub fn into_raw(self) -> u64 {
        self.0
    }
}

/// Identity is fixed when an endpoint is constructed and verified on every
/// received frame, preventing a client from switching namespaces or owners.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderIdentity {
    pub namespace: ProviderNamespaceId,
    pub client: SocketClientId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ProviderFrameType {
    Request = 1,
    Response = 2,
    ReadinessEvent = 3,
}

impl TryFrom<u16> for ProviderFrameType {
    type Error = ProviderFrameError;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Request),
            2 => Ok(Self::Response),
            3 => Ok(Self::ReadinessEvent),
            _ => Err(ProviderFrameError::UnknownFrameType),
        }
    }
}

/// Version-1 operation numbers. Request/response payload schemas can evolve
/// only with an ABI version change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ProviderOpcode {
    UdpSocket = 1,
    UdpBind = 2,
    UdpSendTo = 3,
    UdpReceive = 4,
    TcpSocket = 5,
    TcpBind = 6,
    TcpConnect = 7,
    TcpListen = 8,
    TcpAccept = 9,
    TcpWrite = 10,
    TcpRead = 11,
    TcpShutdown = 12,
    Readiness = 13,
    Close = 14,
    ReadinessChanged = 15,
}

impl TryFrom<u16> for ProviderOpcode {
    type Error = ProviderFrameError;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::UdpSocket),
            2 => Ok(Self::UdpBind),
            3 => Ok(Self::UdpSendTo),
            4 => Ok(Self::UdpReceive),
            5 => Ok(Self::TcpSocket),
            6 => Ok(Self::TcpBind),
            7 => Ok(Self::TcpConnect),
            8 => Ok(Self::TcpListen),
            9 => Ok(Self::TcpAccept),
            10 => Ok(Self::TcpWrite),
            11 => Ok(Self::TcpRead),
            12 => Ok(Self::TcpShutdown),
            13 => Ok(Self::Readiness),
            14 => Ok(Self::Close),
            15 => Ok(Self::ReadinessChanged),
            _ => Err(ProviderFrameError::UnknownOpcode),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderFrame {
    pub frame_type: ProviderFrameType,
    pub opcode: ProviderOpcode,
    pub request_id: u64,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFrameError {
    Truncated,
    BadMagic,
    UnsupportedVersion,
    UnknownFrameType,
    UnknownOpcode,
    ReservedBits,
    IdentityMismatch,
    PayloadTooLarge,
    LengthMismatch,
    QueueFull,
}

pub struct ProviderFramedEndpoint {
    identity: ProviderIdentity,
    max_payload: usize,
}

impl ProviderFramedEndpoint {
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

    pub fn encode(&self, frame: &ProviderFrame) -> Result<Vec<u8>, ProviderFrameError> {
        if frame.payload.len() > self.max_payload {
            return Err(ProviderFrameError::PayloadTooLarge);
        }
        let payload_len =
            u32::try_from(frame.payload.len()).map_err(|_| ProviderFrameError::PayloadTooLarge)?;
        let mut bytes = Vec::with_capacity(PROVIDER_HEADER_LEN + frame.payload.len());
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&PROVIDER_ABI_VERSION.to_le_bytes());
        bytes.extend_from_slice(&(frame.frame_type as u16).to_le_bytes());
        bytes.extend_from_slice(&(frame.opcode as u16).to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&self.identity.namespace.into_raw().to_le_bytes());
        bytes.extend_from_slice(&self.identity.client.into_raw().to_le_bytes());
        bytes.extend_from_slice(&frame.request_id.to_le_bytes());
        bytes.extend_from_slice(&payload_len.to_le_bytes());
        bytes.extend_from_slice(&frame.payload);
        Ok(bytes)
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<ProviderFrame, ProviderFrameError> {
        if bytes.len() < PROVIDER_HEADER_LEN {
            return Err(ProviderFrameError::Truncated);
        }
        if bytes[..4] != MAGIC {
            return Err(ProviderFrameError::BadMagic);
        }
        let u16_at = |offset| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        let u32_at = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let u64_at = |offset| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        if u16_at(4) != PROVIDER_ABI_VERSION {
            return Err(ProviderFrameError::UnsupportedVersion);
        }
        let frame_type = ProviderFrameType::try_from(u16_at(6))?;
        let opcode = ProviderOpcode::try_from(u16_at(8))?;
        if u16_at(10) != 0 {
            return Err(ProviderFrameError::ReservedBits);
        }
        if u64_at(12) != self.identity.namespace.into_raw()
            || u64_at(20) != self.identity.client.into_raw()
        {
            return Err(ProviderFrameError::IdentityMismatch);
        }
        let request_id = u64_at(28);
        let payload_len = usize::try_from(u32_at(36)).unwrap();
        if payload_len > self.max_payload {
            return Err(ProviderFrameError::PayloadTooLarge);
        }
        if bytes.len() != PROVIDER_HEADER_LEN + payload_len {
            return Err(ProviderFrameError::LengthMismatch);
        }
        Ok(ProviderFrame {
            frame_type,
            opcode,
            request_id,
            payload: bytes[PROVIDER_HEADER_LEN..].to_vec(),
        })
    }
}

/// Bounded in-memory ABI harness used by a future kernel-provider patch.
pub struct ProviderAbiHarness {
    endpoint: ProviderFramedEndpoint,
    capacity: usize,
    inbound: VecDeque<Vec<u8>>,
    outbound: VecDeque<Vec<u8>>,
}

impl ProviderAbiHarness {
    pub fn new(identity: ProviderIdentity, capacity: NonZeroUsize) -> Self {
        Self {
            endpoint: ProviderFramedEndpoint::new(identity, MAX_PROVIDER_PAYLOAD).unwrap(),
            capacity: capacity.get(),
            inbound: VecDeque::new(),
            outbound: VecDeque::new(),
        }
    }

    pub fn submit_request(&mut self, frame: ProviderFrame) -> Result<(), ProviderFrameError> {
        if frame.frame_type != ProviderFrameType::Request {
            return Err(ProviderFrameError::UnknownFrameType);
        }
        if self.inbound.len() == self.capacity {
            return Err(ProviderFrameError::QueueFull);
        }
        self.inbound.push_back(self.endpoint.encode(&frame)?);
        Ok(())
    }

    pub fn take_request(&mut self) -> Result<Option<ProviderFrame>, ProviderFrameError> {
        self.inbound
            .pop_front()
            .map(|bytes| self.endpoint.decode(&bytes))
            .transpose()
    }

    pub fn submit_response(&mut self, frame: ProviderFrame) -> Result<(), ProviderFrameError> {
        if frame.frame_type == ProviderFrameType::Request {
            return Err(ProviderFrameError::UnknownFrameType);
        }
        if self.outbound.len() == self.capacity {
            return Err(ProviderFrameError::QueueFull);
        }
        self.outbound.push_back(self.endpoint.encode(&frame)?);
        Ok(())
    }

    pub fn take_response(&mut self) -> Result<Option<ProviderFrame>, ProviderFrameError> {
        self.outbound
            .pop_front()
            .map(|bytes| self.endpoint.decode(&bytes))
            .transpose()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderTrafficOwner {
    PrivateLinuxLoopback,
    RemoteNetstack3,
}

/// Selection rule for the future kernel proxy. Only loopback stays in the
/// private Linux namespace; all other destinations use Netstack3.
pub fn traffic_owner(address: RemoteIpAddress) -> ProviderTrafficOwner {
    match address {
        RemoteIpAddress::V4([127, _, _, _]) => ProviderTrafficOwner::PrivateLinuxLoopback,
        RemoteIpAddress::V6(address) if address == std::net::Ipv6Addr::LOCALHOST.octets() => {
            ProviderTrafficOwner::PrivateLinuxLoopback
        }
        RemoteIpAddress::V4(_) | RemoteIpAddress::V6(_) => ProviderTrafficOwner::RemoteNetstack3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ProviderIdentity {
        ProviderIdentity {
            namespace: ProviderNamespaceId::from_raw(0x0102_0304_0506_0708),
            client: SocketClientId::from_raw(0x1112_1314_1516_1718),
        }
    }

    #[test]
    fn version_one_golden_header_is_stable() {
        let endpoint = ProviderFramedEndpoint::new(identity(), 16).unwrap();
        let bytes = endpoint
            .encode(&ProviderFrame {
                frame_type: ProviderFrameType::Request,
                opcode: ProviderOpcode::TcpConnect,
                request_id: 0x2122_2324_2526_2728,
                payload: vec![0xaa, 0xbb],
            })
            .unwrap();
        assert_eq!(
            bytes,
            [
                b'N', b'S', b'3', b'P', 1, 0, 1, 0, 7, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 0x18, 0x17,
                0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 0x28, 0x27, 0x26, 0x25, 0x24, 0x23, 0x22, 0x21,
                2, 0, 0, 0, 0xaa, 0xbb,
            ]
        );
        assert_eq!(
            endpoint.decode(&bytes).unwrap().opcode,
            ProviderOpcode::TcpConnect
        );
    }

    #[test]
    fn identity_is_immutable_and_queues_are_bounded() {
        let mut harness = ProviderAbiHarness::new(identity(), NonZeroUsize::new(1).unwrap());
        let request = ProviderFrame {
            frame_type: ProviderFrameType::Request,
            opcode: ProviderOpcode::UdpSocket,
            request_id: 1,
            payload: vec![],
        };
        harness.submit_request(request.clone()).unwrap();
        assert_eq!(
            harness.submit_request(request),
            Err(ProviderFrameError::QueueFull)
        );

        let other = ProviderFramedEndpoint::new(
            ProviderIdentity {
                namespace: ProviderNamespaceId::from_raw(9),
                client: identity().client,
            },
            16,
        )
        .unwrap();
        let bytes = other
            .encode(&ProviderFrame {
                frame_type: ProviderFrameType::Request,
                opcode: ProviderOpcode::Close,
                request_id: 2,
                payload: vec![],
            })
            .unwrap();
        assert_eq!(
            ProviderFramedEndpoint::new(identity(), 16)
                .unwrap()
                .decode(&bytes),
            Err(ProviderFrameError::IdentityMismatch)
        );
    }

    #[test]
    fn only_private_loopback_bypasses_netstack3() {
        assert_eq!(
            traffic_owner(RemoteIpAddress::V4([127, 9, 8, 7])),
            ProviderTrafficOwner::PrivateLinuxLoopback
        );
        assert_eq!(
            traffic_owner(RemoteIpAddress::V6(std::net::Ipv6Addr::LOCALHOST.octets())),
            ProviderTrafficOwner::PrivateLinuxLoopback
        );
        assert_eq!(
            traffic_owner(RemoteIpAddress::V4([192, 0, 2, 1])),
            ProviderTrafficOwner::RemoteNetstack3
        );
        assert_eq!(
            traffic_owner(RemoteIpAddress::V6([0; 16])),
            ProviderTrafficOwner::RemoteNetstack3
        );
    }
}
