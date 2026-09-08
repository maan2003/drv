//! HTT carriage over the HTC data-message service.

use ath11k_ce::{RxFrame, ServiceId, Transport, TxFrame};

use crate::{DpError, HttControl, HttHostMessage, HttTargetMessage};

pub const HTT_DATA_MESSAGE_SERVICE: ServiceId = ServiceId::HTT_DATA_MSG;

pub struct HttTransport<T> {
    transport: T,
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

impl<T: Transport> HttControl for HttTransport<T> {
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
