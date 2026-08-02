//! Typed dispatcher for provider ABI v2.

use crate::provider_transport::{ProviderFrameError, ProviderFrameType};
use crate::provider_transport_v2::{
    ProviderFrameV2, ProviderFramedEndpointV2, ProviderNameV2, ProviderOpcodeV2, ProviderOptionV2,
    ProviderReadinessSnapshotV2, ProviderRecvMsgV2, ProviderShutdownV2, ProviderSocketAddressV2,
    ProviderSocketKindV2,
};
use crate::{
    RemoteIpAddress, RemoteIpVersion, RemoteSocketError, RemoteSocketHandle, SocketClientId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderAcceptV2 {
    pub handle: RemoteSocketHandle,
    pub local: ProviderSocketAddressV2,
    pub peer: ProviderSocketAddressV2,
}

/// Complete semantic surface consumed by the Linux v2 proxy. Implementations
/// adapt these operations to Netstack3; fd tables and errno remain host-side.
pub trait RemoteSocketProviderV2 {
    fn open_client(&mut self, client: SocketClientId, quota: u32) -> Result<(), RemoteSocketError>;
    fn close_client(&mut self, client: SocketClientId) -> Result<(), RemoteSocketError>;
    fn open_socket(
        &mut self,
        client: SocketClientId,
        kind: ProviderSocketKindV2,
        version: RemoteIpVersion,
    ) -> Result<RemoteSocketHandle, RemoteSocketError>;
    fn bind(
        &mut self,
        handle: RemoteSocketHandle,
        local: Option<RemoteIpAddress>,
        port: u16,
    ) -> Result<ProviderSocketAddressV2, RemoteSocketError>;
    fn connect(
        &mut self,
        handle: RemoteSocketHandle,
        peer: ProviderSocketAddressV2,
    ) -> Result<(), RemoteSocketError>;
    fn disconnect(
        &mut self,
        handle: RemoteSocketHandle,
    ) -> Result<ProviderSocketAddressV2, RemoteSocketError>;
    fn listen(
        &mut self,
        handle: RemoteSocketHandle,
        backlog: u32,
    ) -> Result<ProviderSocketAddressV2, RemoteSocketError>;
    fn accept(&mut self, handle: RemoteSocketHandle)
    -> Result<ProviderAcceptV2, RemoteSocketError>;
    fn send_msg(
        &mut self,
        handle: RemoteSocketHandle,
        flags: u32,
        peer: Option<ProviderSocketAddressV2>,
        bytes: &[u8],
    ) -> Result<usize, RemoteSocketError>;
    /// No queued message is `Err(WouldBlock)`. A successful empty datagram is
    /// represented by `ProviderRecvMsgV2 { data: vec![], eof: false, .. }`.
    fn recv_msg(
        &mut self,
        handle: RemoteSocketHandle,
        max_len: u32,
        flags: u32,
    ) -> Result<ProviderRecvMsgV2, RemoteSocketError>;
    fn shutdown(
        &mut self,
        handle: RemoteSocketHandle,
        how: ProviderShutdownV2,
    ) -> Result<(), RemoteSocketError>;
    fn get_name(
        &mut self,
        handle: RemoteSocketHandle,
        which: ProviderNameV2,
    ) -> Result<ProviderSocketAddressV2, RemoteSocketError>;
    /// Returns and consumes the pending socket error.
    fn take_socket_error(
        &mut self,
        handle: RemoteSocketHandle,
    ) -> Result<Option<RemoteSocketError>, RemoteSocketError>;
    fn set_option(
        &mut self,
        handle: RemoteSocketHandle,
        option: ProviderOptionV2,
        value: &[u8],
    ) -> Result<(), RemoteSocketError>;
    fn get_option(
        &mut self,
        handle: RemoteSocketHandle,
        option: ProviderOptionV2,
    ) -> Result<Vec<u8>, RemoteSocketError>;
    fn readiness(
        &mut self,
        handle: RemoteSocketHandle,
    ) -> Result<ProviderReadinessSnapshotV2, RemoteSocketError>;
    fn close(&mut self, handle: RemoteSocketHandle) -> Result<(), RemoteSocketError>;
}

pub struct ProviderDispatcherV2<P> {
    endpoint: ProviderFramedEndpointV2,
    provider: P,
}

impl<P: RemoteSocketProviderV2> ProviderDispatcherV2<P> {
    pub fn new(endpoint: ProviderFramedEndpointV2, provider: P) -> Self {
        Self { endpoint, provider }
    }

    pub fn provider(&self) -> &P {
        &self.provider
    }

    pub fn dispatch(&mut self, bytes: &[u8]) -> Result<Vec<u8>, ProviderFrameError> {
        let request = self.endpoint.decode(bytes)?;
        if request.frame_type != ProviderFrameType::Request {
            return Err(ProviderFrameError::UnknownFrameType);
        }
        let payload = match self.execute(request.opcode, &request.payload) {
            Ok(result) => {
                let mut payload = vec![0];
                payload.extend(result);
                payload
            }
            Err(error) => vec![1, error_code(error)],
        };
        self.endpoint.encode(&ProviderFrameV2 {
            frame_type: ProviderFrameType::Response,
            opcode: request.opcode,
            request_id: request.request_id,
            payload,
        })
    }

    fn execute(
        &mut self,
        opcode: ProviderOpcodeV2,
        payload: &[u8],
    ) -> Result<Vec<u8>, RemoteSocketError> {
        let mut d = Decoder::new(payload);
        let client = self.endpoint.identity().client;
        let output = match opcode {
            ProviderOpcodeV2::OpenClient => {
                let quota = d.u32()?;
                d.end()?;
                if quota == 0 {
                    return Err(RemoteSocketError::InvalidState);
                }
                self.provider.open_client(client, quota)?;
                vec![]
            }
            ProviderOpcodeV2::CloseClient => {
                d.end()?;
                self.provider.close_client(client)?;
                vec![]
            }
            ProviderOpcodeV2::OpenSocket => {
                let kind = match d.u8()? {
                    1 => ProviderSocketKindV2::Udp,
                    2 => ProviderSocketKindV2::Tcp,
                    _ => return Err(RemoteSocketError::WrongSocketKind),
                };
                let version = d.version()?;
                d.end()?;
                self.provider
                    .open_socket(client, kind, version)?
                    .into_raw()
                    .to_le_bytes()
                    .to_vec()
            }
            ProviderOpcodeV2::Bind => {
                let handle = d.handle()?;
                let address = d.optional_ip()?;
                let port = d.u16()?;
                d.end()?;
                encode_address(&self.provider.bind(handle, address, port)?)
            }
            ProviderOpcodeV2::Connect => {
                let handle = d.handle()?;
                let peer = d.address()?;
                d.end()?;
                self.provider.connect(handle, peer)?;
                vec![]
            }
            ProviderOpcodeV2::Disconnect => {
                let handle = d.handle()?;
                d.end()?;
                encode_address(&self.provider.disconnect(handle)?)
            }
            ProviderOpcodeV2::Listen => {
                let handle = d.handle()?;
                let backlog = d.u32()?;
                d.end()?;
                encode_address(&self.provider.listen(handle, backlog)?)
            }
            ProviderOpcodeV2::Accept => {
                let handle = d.handle()?;
                d.end()?;
                let accepted = self.provider.accept(handle)?;
                let mut out = accepted.handle.into_raw().to_le_bytes().to_vec();
                accepted.local.encode(&mut out);
                accepted.peer.encode(&mut out);
                out
            }
            ProviderOpcodeV2::SendMsg => {
                let handle = d.handle()?;
                let flags = d.u32()?;
                let peer = d.optional_address()?;
                let sent = self.provider.send_msg(handle, flags, peer, d.rest())?;
                u32::try_from(sent)
                    .map_err(|_| RemoteSocketError::ResourceExhausted)?
                    .to_le_bytes()
                    .to_vec()
            }
            ProviderOpcodeV2::RecvMsg => {
                let handle = d.handle()?;
                let max_len = d.u32()?;
                let flags = d.u32()?;
                d.end()?;
                let result = self.provider.recv_msg(handle, max_len, flags)?;
                if result.data.len() > max_len as usize {
                    return Err(RemoteSocketError::InvalidState);
                }
                result.encode()
            }
            ProviderOpcodeV2::Shutdown => {
                let handle = d.handle()?;
                let how = match d.u8()? {
                    1 => ProviderShutdownV2::Read,
                    2 => ProviderShutdownV2::Write,
                    3 => ProviderShutdownV2::ReadWrite,
                    _ => return Err(RemoteSocketError::InvalidState),
                };
                d.end()?;
                self.provider.shutdown(handle, how)?;
                vec![]
            }
            ProviderOpcodeV2::GetName => {
                let handle = d.handle()?;
                let which = match d.u8()? {
                    1 => ProviderNameV2::Local,
                    2 => ProviderNameV2::Peer,
                    _ => return Err(RemoteSocketError::InvalidState),
                };
                d.end()?;
                encode_address(&self.provider.get_name(handle, which)?)
            }
            ProviderOpcodeV2::GetSocketError => {
                let handle = d.handle()?;
                d.end()?;
                vec![
                    self.provider
                        .take_socket_error(handle)?
                        .map(error_code)
                        .unwrap_or(0),
                ]
            }
            ProviderOpcodeV2::SetOption => {
                let handle = d.handle()?;
                let option = ProviderOptionV2::try_from(d.u8()?)?;
                let len = d.u32()? as usize;
                let value = d.take(len)?;
                d.end()?;
                self.provider.set_option(handle, option, value)?;
                vec![]
            }
            ProviderOpcodeV2::GetOption => {
                let handle = d.handle()?;
                let option = ProviderOptionV2::try_from(d.u8()?)?;
                d.end()?;
                let value = self.provider.get_option(handle, option)?;
                let len =
                    u32::try_from(value.len()).map_err(|_| RemoteSocketError::ResourceExhausted)?;
                let mut out = len.to_le_bytes().to_vec();
                out.extend(value);
                out
            }
            ProviderOpcodeV2::Readiness => {
                let handle = d.handle()?;
                d.end()?;
                let snapshot = self.provider.readiness(handle)?;
                let mut out = snapshot.sequence.to_le_bytes().to_vec();
                out.extend(snapshot.readiness.0.to_le_bytes());
                out.push(snapshot.error.map(error_code).unwrap_or(0));
                out
            }
            ProviderOpcodeV2::Close => {
                let handle = d.handle()?;
                d.end()?;
                self.provider.close(handle)?;
                vec![]
            }
            ProviderOpcodeV2::ReadinessChanged => {
                return Err(RemoteSocketError::InvalidState);
            }
        };
        Ok(output)
    }
}

fn encode_address(address: &ProviderSocketAddressV2) -> Vec<u8> {
    let mut out = Vec::new();
    address.encode(&mut out);
    out
}

pub(crate) fn error_code(error: RemoteSocketError) -> u8 {
    match error {
        RemoteSocketError::UnknownClient => 1,
        RemoteSocketError::QuotaExceeded => 2,
        RemoteSocketError::StaleHandle => 3,
        RemoteSocketError::WrongSocketKind => 4,
        RemoteSocketError::AddressFamilyMismatch => 5,
        RemoteSocketError::AddressInUse => 6,
        RemoteSocketError::WouldBlock => 7,
        RemoteSocketError::InvalidState => 8,
        RemoteSocketError::PayloadTooLarge => 9,
        RemoteSocketError::NetworkUnreachable => 10,
        RemoteSocketError::HostUnreachable => 11,
        RemoteSocketError::ConnectionRefused => 12,
        RemoteSocketError::InProgress => 13,
        RemoteSocketError::AlreadyConnected => 14,
        RemoteSocketError::TimedOut => 15,
        RemoteSocketError::PermissionDenied => 16,
        RemoteSocketError::NotSupported => 17,
        RemoteSocketError::ResourceExhausted => 18,
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], RemoteSocketError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(RemoteSocketError::InvalidState)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(RemoteSocketError::InvalidState)?;
        self.offset = end;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, RemoteSocketError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, RemoteSocketError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, RemoteSocketError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, RemoteSocketError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn handle(&mut self) -> Result<RemoteSocketHandle, RemoteSocketError> {
        Ok(RemoteSocketHandle::from_raw(self.u64()?))
    }
    fn version(&mut self) -> Result<RemoteIpVersion, RemoteSocketError> {
        match self.u8()? {
            4 => Ok(RemoteIpVersion::V4),
            6 => Ok(RemoteIpVersion::V6),
            _ => Err(RemoteSocketError::AddressFamilyMismatch),
        }
    }
    fn ip(&mut self) -> Result<RemoteIpAddress, RemoteSocketError> {
        match self.u8()? {
            4 => Ok(RemoteIpAddress::V4(self.take(4)?.try_into().unwrap())),
            6 => Ok(RemoteIpAddress::V6(self.take(16)?.try_into().unwrap())),
            _ => Err(RemoteSocketError::AddressFamilyMismatch),
        }
    }
    fn optional_ip(&mut self) -> Result<Option<RemoteIpAddress>, RemoteSocketError> {
        match self.u8()? {
            0 => Ok(None),
            4 => Ok(Some(RemoteIpAddress::V4(self.take(4)?.try_into().unwrap()))),
            6 => Ok(Some(RemoteIpAddress::V6(
                self.take(16)?.try_into().unwrap(),
            ))),
            _ => Err(RemoteSocketError::AddressFamilyMismatch),
        }
    }
    fn address(&mut self) -> Result<ProviderSocketAddressV2, RemoteSocketError> {
        Ok(ProviderSocketAddressV2 {
            address: self.ip()?,
            port: self.u16()?,
        })
    }
    fn optional_address(&mut self) -> Result<Option<ProviderSocketAddressV2>, RemoteSocketError> {
        let tag = *self
            .bytes
            .get(self.offset)
            .ok_or(RemoteSocketError::InvalidState)?;
        if tag == 0 {
            self.offset += 1;
            Ok(None)
        } else {
            self.address().map(Some)
        }
    }
    fn rest(&mut self) -> &'a [u8] {
        let rest = &self.bytes[self.offset..];
        self.offset = self.bytes.len();
        rest
    }
    fn end(&self) -> Result<(), RemoteSocketError> {
        (self.offset == self.bytes.len())
            .then_some(())
            .ok_or(RemoteSocketError::InvalidState)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_transport::{ProviderIdentity, ProviderNamespaceId};
    use crate::provider_transport_v2::{PROVIDER_ABI_V2, ProviderReadinessV2};

    #[derive(Default)]
    struct Fake {
        client: Option<SocketClientId>,
        receives: usize,
    }

    fn address() -> ProviderSocketAddressV2 {
        ProviderSocketAddressV2 {
            address: RemoteIpAddress::V4([192, 0, 2, 1]),
            port: 0,
        }
    }

    impl RemoteSocketProviderV2 for Fake {
        fn open_client(
            &mut self,
            client: SocketClientId,
            quota: u32,
        ) -> Result<(), RemoteSocketError> {
            assert_eq!(quota, 8);
            self.client = Some(client);
            Ok(())
        }
        fn close_client(&mut self, client: SocketClientId) -> Result<(), RemoteSocketError> {
            assert_eq!(self.client, Some(client));
            Ok(())
        }
        fn open_socket(
            &mut self,
            client: SocketClientId,
            _: ProviderSocketKindV2,
            _: RemoteIpVersion,
        ) -> Result<RemoteSocketHandle, RemoteSocketError> {
            assert_eq!(self.client, Some(client));
            Ok(RemoteSocketHandle::from_raw(44))
        }
        fn bind(
            &mut self,
            _: RemoteSocketHandle,
            _: Option<RemoteIpAddress>,
            port: u16,
        ) -> Result<ProviderSocketAddressV2, RemoteSocketError> {
            assert_eq!(port, 0);
            Ok(address())
        }
        fn connect(
            &mut self,
            _: RemoteSocketHandle,
            _: ProviderSocketAddressV2,
        ) -> Result<(), RemoteSocketError> {
            Ok(())
        }
        fn disconnect(
            &mut self,
            _: RemoteSocketHandle,
        ) -> Result<ProviderSocketAddressV2, RemoteSocketError> {
            Ok(address())
        }
        fn listen(
            &mut self,
            _: RemoteSocketHandle,
            backlog: u32,
        ) -> Result<ProviderSocketAddressV2, RemoteSocketError> {
            assert_eq!(backlog, 0);
            Ok(address())
        }
        fn accept(&mut self, _: RemoteSocketHandle) -> Result<ProviderAcceptV2, RemoteSocketError> {
            Ok(ProviderAcceptV2 {
                handle: RemoteSocketHandle::from_raw(45),
                local: address(),
                peer: address(),
            })
        }
        fn send_msg(
            &mut self,
            _: RemoteSocketHandle,
            _: u32,
            _: Option<ProviderSocketAddressV2>,
            bytes: &[u8],
        ) -> Result<usize, RemoteSocketError> {
            Ok(bytes.len())
        }
        fn recv_msg(
            &mut self,
            _: RemoteSocketHandle,
            _: u32,
            _: u32,
        ) -> Result<ProviderRecvMsgV2, RemoteSocketError> {
            self.receives += 1;
            if self.receives == 1 {
                Ok(ProviderRecvMsgV2 {
                    source: Some(address()),
                    original_len: 0,
                    flags: 0,
                    eof: false,
                    data: vec![],
                })
            } else {
                Err(RemoteSocketError::WouldBlock)
            }
        }
        fn shutdown(
            &mut self,
            _: RemoteSocketHandle,
            _: ProviderShutdownV2,
        ) -> Result<(), RemoteSocketError> {
            Ok(())
        }
        fn get_name(
            &mut self,
            _: RemoteSocketHandle,
            _: ProviderNameV2,
        ) -> Result<ProviderSocketAddressV2, RemoteSocketError> {
            Ok(address())
        }
        fn take_socket_error(
            &mut self,
            _: RemoteSocketHandle,
        ) -> Result<Option<RemoteSocketError>, RemoteSocketError> {
            Ok(None)
        }
        fn set_option(
            &mut self,
            _: RemoteSocketHandle,
            _: ProviderOptionV2,
            _: &[u8],
        ) -> Result<(), RemoteSocketError> {
            Ok(())
        }
        fn get_option(
            &mut self,
            _: RemoteSocketHandle,
            _: ProviderOptionV2,
        ) -> Result<Vec<u8>, RemoteSocketError> {
            Ok(vec![1])
        }
        fn readiness(
            &mut self,
            _: RemoteSocketHandle,
        ) -> Result<ProviderReadinessSnapshotV2, RemoteSocketError> {
            Ok(ProviderReadinessSnapshotV2 {
                sequence: 7,
                readiness: crate::provider_transport_v2::ProviderReadinessV2(
                    ProviderReadinessV2::INCOMING,
                ),
                error: None,
            })
        }
        fn close(&mut self, _: RemoteSocketHandle) -> Result<(), RemoteSocketError> {
            Ok(())
        }
    }

    fn endpoint() -> ProviderFramedEndpointV2 {
        ProviderFramedEndpointV2::new(
            ProviderIdentity {
                namespace: ProviderNamespaceId::from_raw(3),
                client: SocketClientId::from_raw(4),
            },
            1024,
        )
        .unwrap()
    }

    fn request(opcode: ProviderOpcodeV2, payload: Vec<u8>) -> Vec<u8> {
        endpoint()
            .encode(&ProviderFrameV2 {
                frame_type: ProviderFrameType::Request,
                opcode,
                request_id: 9,
                payload,
            })
            .unwrap()
    }

    #[test]
    fn v2_dispatch_preserves_identity_zero_values_and_empty_datagram() {
        assert_eq!(PROVIDER_ABI_V2, 2);
        let mut dispatcher = ProviderDispatcherV2::new(endpoint(), Fake::default());
        let response = dispatcher
            .dispatch(&request(
                ProviderOpcodeV2::OpenClient,
                8u32.to_le_bytes().to_vec(),
            ))
            .unwrap();
        assert_eq!(endpoint().decode(&response).unwrap().payload, vec![0]);
        let mut bind = 44u64.to_le_bytes().to_vec();
        bind.push(4);
        bind.extend([192, 0, 2, 1]);
        bind.extend(0u16.to_le_bytes());
        let response = dispatcher
            .dispatch(&request(ProviderOpcodeV2::Bind, bind))
            .unwrap();
        assert_eq!(endpoint().decode(&response).unwrap().payload[0], 0);
        let mut recv = 44u64.to_le_bytes().to_vec();
        recv.extend(16u32.to_le_bytes());
        recv.extend(0u32.to_le_bytes());
        let empty = endpoint()
            .decode(
                &dispatcher
                    .dispatch(&request(ProviderOpcodeV2::RecvMsg, recv.clone()))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(empty.payload[0], 0);
        assert!(empty.payload.len() > 10);
        let none = endpoint()
            .decode(
                &dispatcher
                    .dispatch(&request(ProviderOpcodeV2::RecvMsg, recv))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(none.payload, vec![1, 7]);
    }

    #[test]
    fn malformed_payload_and_event_as_request_are_stable_errors() {
        let mut dispatcher = ProviderDispatcherV2::new(endpoint(), Fake::default());
        let malformed = endpoint()
            .decode(
                &dispatcher
                    .dispatch(&request(ProviderOpcodeV2::Bind, vec![0]))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(malformed.payload, vec![1, 8]);
        let event = endpoint()
            .decode(
                &dispatcher
                    .dispatch(&request(ProviderOpcodeV2::ReadinessChanged, vec![]))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(event.payload, vec![1, 8]);
    }
}
