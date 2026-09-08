#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
use alloc::vec::Vec;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request(Vec<u8>);
impl Request {
    pub fn from_tlv_bytes(bytes: Vec<u8>) -> Result<Self, QmiError> {
        if bytes.len() <= u16::MAX as usize {
            Ok(Self(bytes))
        } else {
            Err(QmiError::MessageTooLong)
        }
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response(pub Vec<u8>);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareReady {
    pub firmware_version: u32,
    pub target_mem_mode: u32,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QmiError {
    Malformed,
    MessageTooLong,
    Timeout,
    Transport,
}
/// AF_QIPCRTR is implemented outside this protocol crate.
pub trait Transport {
    fn transact(&mut self, request: Request, deadline_ns: u64) -> Result<Response, QmiError>;
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
            Request::from_tlv_bytes(vec![0; 65536]),
            Err(QmiError::MessageTooLong)
        );
    }
}
