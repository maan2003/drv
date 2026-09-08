#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
use alloc::vec::Vec;
use ath11k_hal::RingId;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServiceId(pub u16);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TxFrame {
    pub service: ServiceId,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RxFrame {
    pub service: ServiceId,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CeError {
    NoCredits,
    InvalidFrame,
    DeviceFault,
}
pub trait Transport {
    fn bind_service(&mut self, service: ServiceId, tx: RingId, rx: RingId) -> Result<(), CeError>;
    fn send(&mut self, frame: TxFrame) -> Result<(), CeError>;
    fn receive(&mut self, deadline_ns: u64) -> Result<Option<RxFrame>, CeError>;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_identity_is_typed() {
        assert_ne!(ServiceId(1), ServiceId(2));
    }
}
