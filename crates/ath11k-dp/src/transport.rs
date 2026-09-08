// PORT-MAP: local-seam
//! HTT carriage over the HTC data-message service.

use ath11k_ce::{HtcServiceTransport, RxFrame, ServiceId, Transport, TxFrame};

use crate::{DpError, HttControl, HttHostMessage, HttTargetMessage};

pub const HTT_DATA_MESSAGE_SERVICE: ServiceId = ServiceId::HTT_DATA_MSG;

pub struct HttTransport<T> {
    transport: T,
}

/// HTT adapter for an endpoint already bound by the shared HTC router.
pub struct HtcHttTransport<T> {
    endpoint: T,
}

impl<T> HtcHttTransport<T> {
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

impl<T> HttTransport<T> {
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    pub fn into_inner(self) -> T {
        self.transport
    }
}

/// `ath11k_dp_htt_connect`: bind the HTC HTT data service and retain its
/// typed transport adapter.
pub fn ath11k_dp_htt_connect<T: Transport>(
    mut transport: T,
    tx: ath11k_hal::RingId,
    rx: ath11k_hal::RingId,
) -> Result<HttTransport<T>, DpError> {
    transport
        .bind_service(HTT_DATA_MESSAGE_SERVICE, tx, rx)
        .map_err(map_ce_error)?;
    Ok(HttTransport::new(transport))
}

/// Construct the HTT control adapter from a service endpoint issued by
/// `HtcRouter::bind_service`.
pub const fn ath11k_dp_htt_connect_service<T: HtcServiceTransport>(
    endpoint: T,
) -> HtcHttTransport<T> {
    HtcHttTransport::new(endpoint)
}

impl<T: Transport> HttControl for HttTransport<T> {
    const SEND_ERROR_IS_NON_VISIBLE: bool = true;

    fn send(&mut self, message: HttHostMessage) -> Result<(), DpError> {
        self.transport
            .send(TxFrame {
                service: HTT_DATA_MESSAGE_SERVICE,
                bytes: message.0,
            })
            .map_err(map_ce_error)
    }

    fn receive(&mut self, deadline_ns: u64) -> Result<Option<HttTargetMessage>, DpError> {
        self.transport
            .receive(deadline_ns)
            .map_err(map_ce_error)?
            .map(decode_frame)
            .transpose()
    }
}

impl<T: HtcServiceTransport> HttControl for HtcHttTransport<T> {
    const SEND_ERROR_IS_NON_VISIBLE: bool = true;

    fn send(&mut self, message: HttHostMessage) -> Result<(), DpError> {
        validate_htt_payload(&message.0)?;
        self.endpoint.send_payload(&message.0).map_err(map_ce_error)
    }

    fn receive(&mut self, deadline_ns: u64) -> Result<Option<HttTargetMessage>, DpError> {
        let Some(payload) = self
            .endpoint
            .receive_payload(deadline_ns)
            .map_err(map_ce_error)?
        else {
            return Ok(None);
        };
        validate_htt_payload(&payload)?;
        Ok(Some(HttTargetMessage(payload)))
    }
}

fn validate_htt_payload(payload: &[u8]) -> Result<(), DpError> {
    // Every HTT message starts with a complete little-endian info word. Some
    // telemetry messages append byte arrays, so the total need not be aligned.
    if payload.len() < 4 {
        return Err(DpError::MalformedHtt);
    }
    Ok(())
}

fn decode_frame(frame: RxFrame) -> Result<HttTargetMessage, DpError> {
    if frame.service != HTT_DATA_MESSAGE_SERVICE {
        return Err(DpError::MalformedHtt);
    }
    Ok(HttTargetMessage(frame.bytes))
}

fn map_ce_error(error: ath11k_ce::CeError) -> DpError {
    match error {
        ath11k_ce::CeError::NoCredits => DpError::NoResources,
        ath11k_ce::CeError::InvalidFrame => DpError::MalformedHtt,
        ath11k_ce::CeError::DeviceFault => DpError::DeviceFault,
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;
    use ath11k_ce::CeError;

    use super::*;

    #[derive(Default)]
    struct Endpoint {
        sent: Vec<u8>,
        received: Option<Vec<u8>>,
        error: Option<CeError>,
    }

    impl HtcServiceTransport for Endpoint {
        fn send_payload(&mut self, payload: &[u8]) -> Result<(), CeError> {
            if let Some(error) = self.error {
                return Err(error);
            }
            self.sent.extend_from_slice(payload);
            Ok(())
        }

        fn receive_payload(&mut self, _deadline_ns: u64) -> Result<Option<Vec<u8>>, CeError> {
            if let Some(error) = self.error {
                return Err(error);
            }
            Ok(self.received.take())
        }
    }

    #[test]
    fn endpoint_adapter_forwards_complete_htt_words() {
        const {
            assert!(<HtcHttTransport<Endpoint> as HttControl>::SEND_ERROR_IS_NON_VISIBLE);
        }
        let mut transport = ath11k_dp_htt_connect_service(Endpoint {
            received: Some(vec![0, 7, 3, 0]),
            ..Endpoint::default()
        });
        transport.send(HttHostMessage(vec![0, 0, 0, 0])).unwrap();
        assert_eq!(transport.endpoint_mut().sent, [0, 0, 0, 0]);
        assert_eq!(
            transport.receive(1).unwrap(),
            Some(HttTargetMessage(vec![0, 7, 3, 0]))
        );
    }

    #[test]
    fn ce_transport_adapters_guarantee_retry_safe_send_errors() {
        const {
            assert!(<HttTransport<TransportModel> as HttControl>::SEND_ERROR_IS_NON_VISIBLE);
        }
    }

    struct TransportModel;
    impl Transport for TransportModel {
        fn bind_service(
            &mut self,
            _: ServiceId,
            _: ath11k_hal::RingId,
            _: ath11k_hal::RingId,
        ) -> Result<(), CeError> {
            Ok(())
        }
        fn send(&mut self, _: TxFrame) -> Result<(), CeError> {
            Ok(())
        }
        fn receive(&mut self, _: u64) -> Result<Option<RxFrame>, CeError> {
            Ok(None)
        }
    }

    #[test]
    fn endpoint_adapter_rejects_malformed_payloads_and_maps_credit_errors() {
        for bytes in [vec![], vec![1, 2, 3]] {
            let mut transport = ath11k_dp_htt_connect_service(Endpoint {
                received: Some(bytes),
                ..Endpoint::default()
            });
            assert_eq!(transport.receive(0), Err(DpError::MalformedHtt));
        }

        let mut transport = ath11k_dp_htt_connect_service(Endpoint {
            error: Some(CeError::NoCredits),
            ..Endpoint::default()
        });
        assert_eq!(
            transport.send(HttHostMessage(vec![0, 0, 0, 0])),
            Err(DpError::NoResources)
        );
    }
}
