//! Version-1 request payloads and dispatcher for the framed provider ABI.

use std::num::{NonZeroU16, NonZeroUsize};

use crate::provider_transport::{
    ProviderFrame, ProviderFrameError, ProviderFrameType, ProviderFramedEndpoint, ProviderOpcode,
};
use crate::{
    RemoteIpAddress, RemoteIpVersion, RemoteSocketAddress, RemoteSocketError, RemoteSocketHandle,
    RemoteSocketProvider,
};

pub struct ProviderDispatcher<P> {
    endpoint: ProviderFramedEndpoint,
    provider: P,
}

impl<P: RemoteSocketProvider> ProviderDispatcher<P> {
    pub fn new(endpoint: ProviderFramedEndpoint, provider: P) -> Self {
        Self { endpoint, provider }
    }

    pub fn provider(&self) -> &P {
        &self.provider
    }

    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }

    /// Decodes, validates, and executes exactly one request. The returned frame
    /// always echoes the request ID and opcode.
    pub fn dispatch(&mut self, bytes: &[u8]) -> Result<Vec<u8>, ProviderFrameError> {
        let request = self.endpoint.decode(bytes)?;
        if request.frame_type != ProviderFrameType::Request {
            return Err(ProviderFrameError::UnknownFrameType);
        }
        let result = self.execute(request.opcode, &request.payload);
        let payload = match result {
            Ok(payload) => {
                let mut response = vec![0];
                response.extend(payload);
                response
            }
            Err(error) => vec![1, error_code(error)],
        };
        self.endpoint.encode(&ProviderFrame {
            frame_type: ProviderFrameType::Response,
            opcode: request.opcode,
            request_id: request.request_id,
            payload,
        })
    }

    fn execute(
        &mut self,
        opcode: ProviderOpcode,
        payload: &[u8],
    ) -> Result<Vec<u8>, RemoteSocketError> {
        let mut decoder = Decoder::new(payload);
        let client = self.endpoint.identity().client;
        let response = match opcode {
            ProviderOpcode::UdpSocket => {
                let socket = self.provider.udp_socket(client, decoder.version()?)?;
                encode_handle(socket)
            }
            ProviderOpcode::UdpBind => {
                let socket = decoder.handle()?;
                let address = decoder.optional_address()?;
                let port = decoder.port()?;
                decoder.end()?;
                self.provider.udp_bind(socket, address, port)?;
                vec![]
            }
            ProviderOpcode::UdpSendTo => {
                let socket = decoder.handle()?;
                let remote = decoder.socket_address()?;
                let bytes = decoder.rest();
                self.provider.udp_send_to(socket, remote, bytes)?;
                vec![]
            }
            ProviderOpcode::UdpReceive => {
                let socket = decoder.handle()?;
                decoder.end()?;
                self.provider.udp_receive(socket)?.unwrap_or_default()
            }
            ProviderOpcode::TcpSocket => {
                let socket = self.provider.tcp_socket(client, decoder.version()?)?;
                encode_handle(socket)
            }
            ProviderOpcode::TcpBind => {
                let socket = decoder.handle()?;
                let address = decoder.optional_address()?;
                let port = decoder.port()?;
                decoder.end()?;
                self.provider.tcp_bind(socket, address, port)?;
                vec![]
            }
            ProviderOpcode::TcpConnect => {
                let socket = decoder.handle()?;
                let remote = decoder.socket_address()?;
                decoder.end()?;
                self.provider.tcp_connect(socket, remote)?;
                vec![]
            }
            ProviderOpcode::TcpListen => {
                let socket = decoder.handle()?;
                let backlog = decoder.nonzero_usize()?;
                decoder.end()?;
                self.provider.tcp_listen(socket, backlog)?;
                vec![]
            }
            ProviderOpcode::TcpAccept => {
                let socket = decoder.handle()?;
                decoder.end()?;
                encode_handle(self.provider.tcp_accept(socket)?)
            }
            ProviderOpcode::TcpWrite => {
                let socket = decoder.handle()?;
                let bytes = decoder.rest();
                let written = self.provider.tcp_write(socket, bytes)?;
                u32::try_from(written)
                    .map_err(|_| RemoteSocketError::ResourceExhausted)?
                    .to_le_bytes()
                    .to_vec()
            }
            ProviderOpcode::TcpRead => {
                let socket = decoder.handle()?;
                let length = decoder.u32()? as usize;
                decoder.end()?;
                let mut output =
                    vec![0; length.min(crate::provider_transport::MAX_PROVIDER_PAYLOAD)];
                let read = self.provider.tcp_read(socket, &mut output)?;
                output.truncate(read);
                output
            }
            ProviderOpcode::TcpShutdown => {
                let socket = decoder.handle()?;
                decoder.end()?;
                self.provider.tcp_shutdown(socket)?;
                vec![]
            }
            ProviderOpcode::Readiness => {
                let socket = decoder.handle()?;
                decoder.end()?;
                let readiness = self.provider.readiness(socket)?;
                vec![
                    u8::from(readiness.readable),
                    u8::from(readiness.writable),
                    u8::from(readiness.incoming),
                ]
            }
            ProviderOpcode::Close => {
                let socket = decoder.handle()?;
                decoder.end()?;
                self.provider.close(socket)?;
                vec![]
            }
            ProviderOpcode::ReadinessChanged => return Err(RemoteSocketError::InvalidState),
        };
        Ok(response)
    }
}

fn encode_handle(handle: RemoteSocketHandle) -> Vec<u8> {
    handle.into_raw().to_le_bytes().to_vec()
}

fn error_code(error: RemoteSocketError) -> u8 {
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
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(RemoteSocketError::InvalidState)?;
        self.offset = end;
        Ok(bytes)
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

    fn version(&mut self) -> Result<RemoteIpVersion, RemoteSocketError> {
        let version = match self.u8()? {
            4 => RemoteIpVersion::V4,
            6 => RemoteIpVersion::V6,
            _ => return Err(RemoteSocketError::AddressFamilyMismatch),
        };
        self.end()?;
        Ok(version)
    }

    fn address(&mut self) -> Result<RemoteIpAddress, RemoteSocketError> {
        match self.u8()? {
            4 => Ok(RemoteIpAddress::V4(self.take(4)?.try_into().unwrap())),
            6 => Ok(RemoteIpAddress::V6(self.take(16)?.try_into().unwrap())),
            _ => Err(RemoteSocketError::AddressFamilyMismatch),
        }
    }

    fn optional_address(&mut self) -> Result<Option<RemoteIpAddress>, RemoteSocketError> {
        match self.u8()? {
            0 => Ok(None),
            4 => Ok(Some(RemoteIpAddress::V4(self.take(4)?.try_into().unwrap()))),
            6 => Ok(Some(RemoteIpAddress::V6(
                self.take(16)?.try_into().unwrap(),
            ))),
            _ => Err(RemoteSocketError::AddressFamilyMismatch),
        }
    }

    fn port(&mut self) -> Result<NonZeroU16, RemoteSocketError> {
        NonZeroU16::new(self.u16()?).ok_or(RemoteSocketError::InvalidState)
    }

    fn socket_address(&mut self) -> Result<RemoteSocketAddress, RemoteSocketError> {
        Ok(RemoteSocketAddress {
            address: self.address()?,
            port: self.port()?,
        })
    }

    fn handle(&mut self) -> Result<RemoteSocketHandle, RemoteSocketError> {
        Ok(RemoteSocketHandle::from_raw(self.u64()?))
    }

    fn nonzero_usize(&mut self) -> Result<NonZeroUsize, RemoteSocketError> {
        NonZeroUsize::new(self.u32()? as usize).ok_or(RemoteSocketError::InvalidState)
    }

    fn rest(&mut self) -> &'a [u8] {
        let bytes = &self.bytes[self.offset..];
        self.offset = self.bytes.len();
        bytes
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
    use crate::provider_transport::{PROVIDER_HEADER_LEN, ProviderIdentity, ProviderNamespaceId};
    use crate::{RemoteSocketReadiness, SocketClientId};

    struct Fake {
        client: SocketClientId,
        sockets: usize,
    }

    impl RemoteSocketProvider for Fake {
        fn open_client(&mut self, _: NonZeroUsize) -> Result<SocketClientId, RemoteSocketError> {
            unreachable!()
        }
        fn close_client(&mut self, _: SocketClientId) -> Result<(), RemoteSocketError> {
            unreachable!()
        }
        fn udp_socket(
            &mut self,
            client: SocketClientId,
            _: RemoteIpVersion,
        ) -> Result<RemoteSocketHandle, RemoteSocketError> {
            assert_eq!(client, self.client);
            self.sockets += 1;
            Ok(RemoteSocketHandle::from_raw(41))
        }
        fn udp_bind(
            &mut self,
            _: RemoteSocketHandle,
            _: Option<RemoteIpAddress>,
            _: NonZeroU16,
        ) -> Result<(), RemoteSocketError> {
            Ok(())
        }
        fn udp_send_to(
            &mut self,
            _: RemoteSocketHandle,
            _: RemoteSocketAddress,
            _: &[u8],
        ) -> Result<(), RemoteSocketError> {
            Ok(())
        }
        fn udp_receive(
            &mut self,
            _: RemoteSocketHandle,
        ) -> Result<Option<Vec<u8>>, RemoteSocketError> {
            Ok(None)
        }
        fn tcp_socket(
            &mut self,
            _: SocketClientId,
            _: RemoteIpVersion,
        ) -> Result<RemoteSocketHandle, RemoteSocketError> {
            Err(RemoteSocketError::QuotaExceeded)
        }
        fn tcp_bind(
            &mut self,
            _: RemoteSocketHandle,
            _: Option<RemoteIpAddress>,
            _: NonZeroU16,
        ) -> Result<(), RemoteSocketError> {
            Ok(())
        }
        fn tcp_connect(
            &mut self,
            _: RemoteSocketHandle,
            _: RemoteSocketAddress,
        ) -> Result<(), RemoteSocketError> {
            Ok(())
        }
        fn tcp_listen(
            &mut self,
            _: RemoteSocketHandle,
            _: NonZeroUsize,
        ) -> Result<(), RemoteSocketError> {
            Ok(())
        }
        fn tcp_accept(
            &mut self,
            _: RemoteSocketHandle,
        ) -> Result<RemoteSocketHandle, RemoteSocketError> {
            Err(RemoteSocketError::WouldBlock)
        }
        fn tcp_write(
            &mut self,
            _: RemoteSocketHandle,
            b: &[u8],
        ) -> Result<usize, RemoteSocketError> {
            Ok(b.len())
        }
        fn tcp_read(
            &mut self,
            _: RemoteSocketHandle,
            _: &mut [u8],
        ) -> Result<usize, RemoteSocketError> {
            Ok(0)
        }
        fn tcp_shutdown(&mut self, _: RemoteSocketHandle) -> Result<(), RemoteSocketError> {
            Ok(())
        }
        fn readiness(
            &mut self,
            _: RemoteSocketHandle,
        ) -> Result<RemoteSocketReadiness, RemoteSocketError> {
            Ok(RemoteSocketReadiness::default())
        }
        fn close(&mut self, _: RemoteSocketHandle) -> Result<(), RemoteSocketError> {
            Ok(())
        }
    }

    #[test]
    fn dispatcher_preserves_identity_request_id_and_error_codes() {
        let client = SocketClientId::from_raw(7);
        let identity = ProviderIdentity {
            namespace: ProviderNamespaceId::from_raw(9),
            client,
        };
        let endpoint = ProviderFramedEndpoint::new(identity, 64).unwrap();
        let request = endpoint
            .encode(&ProviderFrame {
                frame_type: ProviderFrameType::Request,
                opcode: ProviderOpcode::UdpSocket,
                request_id: 123,
                payload: vec![4],
            })
            .unwrap();
        let mut dispatcher = ProviderDispatcher::new(
            ProviderFramedEndpoint::new(identity, 64).unwrap(),
            Fake { client, sockets: 0 },
        );
        let response = dispatcher.dispatch(&request).unwrap();
        let response = endpoint.decode(&response).unwrap();
        assert_eq!(response.request_id, 123);
        assert_eq!(response.payload, [0, 41, 0, 0, 0, 0, 0, 0, 0]);

        let request = endpoint
            .encode(&ProviderFrame {
                frame_type: ProviderFrameType::Request,
                opcode: ProviderOpcode::TcpSocket,
                request_id: 124,
                payload: vec![4],
            })
            .unwrap();
        let response = dispatcher.dispatch(&request).unwrap();
        assert_eq!(endpoint.decode(&response).unwrap().payload, [1, 2]);
        assert!(response.len() >= PROVIDER_HEADER_LEN);
    }

    #[test]
    fn malformed_payload_becomes_stable_invalid_state_response() {
        let identity = ProviderIdentity {
            namespace: ProviderNamespaceId::from_raw(1),
            client: SocketClientId::from_raw(2),
        };
        let endpoint = ProviderFramedEndpoint::new(identity, 64).unwrap();
        let request = endpoint
            .encode(&ProviderFrame {
                frame_type: ProviderFrameType::Request,
                opcode: ProviderOpcode::TcpConnect,
                request_id: 1,
                payload: vec![0],
            })
            .unwrap();
        let mut dispatcher = ProviderDispatcher::new(
            ProviderFramedEndpoint::new(identity, 64).unwrap(),
            Fake {
                client: identity.client,
                sockets: 0,
            },
        );
        let response = dispatcher.dispatch(&request).unwrap();
        assert_eq!(endpoint.decode(&response).unwrap().payload, [1, 8]);
    }
}
