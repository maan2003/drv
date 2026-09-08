// PORT-MAP: local-seam
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

pub mod handshake;
pub mod wire;

#[cfg(feature = "trace")]
pub mod trace;
#[cfg(feature = "trace")]
pub use trace::{RejectReason, TraceEvent, TraceSink};

#[cfg(feature = "proptest")]
pub mod strategies;

pub use handshake::{
    DriverEvent, FirmwareAssets, HandshakeConfig, MemoryProvider, MemoryRegion, Wcn6750Handshake,
};
pub use wire::MessageId;

/// A checked, bounded WLFW QMI message body (the QRTR/QMI header is transport-owned).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    message_id: MessageId,
    bytes: Vec<u8>,
}

impl Request {
    pub(crate) fn from_tlv_bytes(message_id: MessageId, bytes: Vec<u8>) -> Result<Self, QmiError> {
        wire::validate_tlvs(&bytes)?;
        if bytes.len() <= u16::MAX as usize {
            Ok(Self { message_id, bytes })
        } else {
            Err(QmiError::MessageTooLong)
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub const fn message_id(&self) -> MessageId {
        self.message_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response {
    transaction_id: TransactionId,
    message_id: MessageId,
    bytes: Vec<u8>,
}

impl Response {
    pub fn checked(
        transaction_id: TransactionId,
        message_id: MessageId,
        bytes: Vec<u8>,
    ) -> Result<Self, QmiError> {
        wire::validate_tlvs(&bytes)?;
        if bytes.len() > wire::RESPONSE_MAX_LEN {
            return Err(QmiError::MessageTooLong);
        }
        Ok(Self {
            transaction_id,
            message_id,
            bytes,
        })
    }

    #[cfg(feature = "trace")]
    pub fn checked_with_trace(
        transaction_id: TransactionId,
        message_id: MessageId,
        bytes: Vec<u8>,
        trace: Option<&mut dyn trace::TraceSink>,
    ) -> Result<Self, QmiError> {
        if let Some(sink) = trace {
            trace::trace_tlvs(&bytes, sink);
        }
        Self::checked(transaction_id, message_id, bytes)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub const fn message_id(&self) -> MessageId {
        self.message_id
    }
    pub const fn transaction_id(&self) -> TransactionId {
        self.transaction_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransactionId(u16);
impl TransactionId {
    pub const fn new(value: u16) -> Self {
        Self(value)
    }
    pub const fn value(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareReady {
    pub firmware_version: u32,
    pub target_mem_mode: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QmiError {
    Malformed,
    MessageTooLong,
    Protocol(wire::QmiResponse),
    Disconnected,
    Timeout,
    Transport,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawIndication {
    message_id: MessageId,
    bytes: Vec<u8>,
}

impl RawIndication {
    pub fn checked(message_id: MessageId, bytes: Vec<u8>) -> Result<Self, QmiError> {
        wire::validate_tlvs(&bytes)?;
        Ok(Self { message_id, bytes })
    }
    #[cfg(feature = "trace")]
    pub fn checked_with_trace(
        message_id: MessageId,
        bytes: Vec<u8>,
        trace: Option<&mut dyn trace::TraceSink>,
    ) -> Result<Self, QmiError> {
        if let Some(sink) = trace {
            trace::trace_tlvs(&bytes, sink);
        }
        Self::checked(message_id, bytes)
    }
    pub const fn message_id(&self) -> MessageId {
        self.message_id
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Incoming {
    ServerArrived,
    ServerExited,
    Response(Response),
    Indication(RawIndication),
}

/// AF_QIPCRTR and the QMI/QRTR envelope are implemented outside this crate.
pub trait Transport {
    fn start_service(&mut self, version: u32, instance: u32) -> Result<(), QmiError>;
    fn stop_service(&mut self);
    fn send(&mut self, request: Request) -> Result<TransactionId, QmiError>;
    fn now_ns(&self) -> u64;
    /// Wait at most `timeout_ns`; this is a duration, not an absolute timestamp.
    fn receive(&mut self, timeout_ns: u64) -> Result<Incoming, QmiError>;
}

pub trait Handshake {
    fn start(&mut self, transport: &mut dyn Transport) -> Result<FirmwareReady, QmiError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn qmi_length_bound() {
        assert_eq!(
            Request::from_tlv_bytes(MessageId::Capability, vec![0; 65536]),
            Err(QmiError::Malformed)
        );
    }

    #[test]
    fn truncated_tlv_is_rejected_without_panic() {
        for bytes in [vec![1], vec![1, 2], vec![1, 1, 0], vec![1, 2, 0, 4]] {
            assert_eq!(
                Request::from_tlv_bytes(MessageId::Capability, bytes),
                Err(QmiError::Malformed)
            );
        }
    }
}
