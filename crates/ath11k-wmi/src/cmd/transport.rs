//! WMI command/event envelope adaptation over an endpoint-bound HTC service.

use alloc::vec::Vec;
use ath11k_ce::HtcServiceTransport;

use crate::{Command, Event, EventId, Transport, WmiError};

pub struct HtcWmiTransport<T> {
    endpoint: T,
}

impl<T> HtcWmiTransport<T> {
    pub const fn new(endpoint: T) -> Self {
        Self { endpoint }
    }

    pub fn endpoint_mut(&mut self) -> &mut T {
        &mut self.endpoint
    }

    pub fn into_inner(self) -> T {
        self.endpoint
    }
}

impl<T: HtcServiceTransport> Transport for HtcWmiTransport<T> {
    const SEND_ERROR_IS_NON_VISIBLE: bool = true;

    fn send(&mut self, command: Command) -> Result<(), WmiError> {
        let mut payload = Vec::with_capacity(4 + command.tlvs().len());
        payload.extend_from_slice(&(command.id.0 & 0x00ff_ffff).to_le_bytes());
        payload.extend_from_slice(command.tlvs());
        self.endpoint
            .send_payload(&payload)
            .map_err(|_| WmiError::Transport)
    }

    fn receive(&mut self, deadline_ns: u64) -> Result<Option<Event>, WmiError> {
        let Some(payload) = self
            .endpoint
            .receive_payload(deadline_ns)
            .map_err(|_| WmiError::Transport)?
        else {
            return Ok(None);
        };
        let header = payload.get(..4).ok_or(WmiError::Malformed)?;
        let id =
            u32::from_le_bytes(header.try_into().map_err(|_| WmiError::Malformed)?) & 0x00ff_ffff;
        Event::from_tlvs(EventId(id), payload[4..].to_vec()).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommandId;
    use alloc::vec;
    use ath11k_ce::CeError;

    #[derive(Default)]
    struct Endpoint {
        sent: Vec<u8>,
        received: Option<Vec<u8>>,
    }
    impl HtcServiceTransport for Endpoint {
        fn send_payload(&mut self, payload: &[u8]) -> Result<(), CeError> {
            self.sent.extend_from_slice(payload);
            Ok(())
        }
        fn receive_payload(&mut self, _deadline_ns: u64) -> Result<Option<Vec<u8>>, CeError> {
            Ok(self.received.take())
        }
    }

    #[test]
    fn prepends_and_extracts_low_24_bit_wmi_header() {
        let mut transport = HtcWmiTransport::new(Endpoint {
            sent: Vec::new(),
            received: Some(vec![0x34, 0x12, 0xab, 0xff, 0, 0, 0, 0]),
        });
        transport
            .send(Command::from_tlvs(CommandId(0xff12_3456), vec![0; 4]).unwrap())
            .unwrap();
        assert_eq!(
            transport.endpoint_mut().sent,
            [0x56, 0x34, 0x12, 0, 0, 0, 0, 0]
        );
        let event = transport.receive(1).unwrap().unwrap();
        assert_eq!(event.id, EventId(0x00ab_1234));
        assert_eq!(event.tlvs(), &[0; 4]);
    }

    #[test]
    fn rejects_truncated_or_unaligned_rx_without_panicking() {
        for (payload, expected) in [
            (vec![], WmiError::Malformed),
            (vec![1, 2, 3], WmiError::Malformed),
            (vec![1, 0, 0, 0, 5], WmiError::UnalignedTlv),
        ] {
            let mut transport = HtcWmiTransport::new(Endpoint {
                sent: Vec::new(),
                received: Some(payload),
            });
            assert_eq!(transport.receive(0), Err(expected));
        }
    }
}
