// PORT-MAP: local-seam
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
use alloc::vec::Vec;
use ath11k_hal::RingId;

pub mod dma;
pub mod golden;
pub mod htt;
pub mod lifecycle;
pub mod reo;
pub mod rx;
pub mod transport;
pub mod tx;
pub use lifecycle::{AllocatedDpRing, DpAllocationError, DpRingOps, DpRingSpec, Wcn6750DpRings};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerId(pub u16);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttHostMessage(pub Vec<u8>);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttTargetMessage(pub Vec<u8>);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TxPacket {
    pub peer: PeerId,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RxPacket {
    pub peer: Option<PeerId>,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataRings {
    pub tcl: RingId,
    pub reo: RingId,
    pub wbm: RingId,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DpError {
    MalformedHtt,
    MalformedDescriptor,
    UnsupportedVersion,
    InvalidFrame,
    UnsupportedDescriptor,
    Timeout,
    NoResources,
    DeviceFault,
    WrongState,
}
pub trait HttControl {
    fn send(&mut self, message: HttHostMessage) -> Result<(), DpError>;
    fn receive(&mut self, deadline_ns: u64) -> Result<Option<HttTargetMessage>, DpError>;
}
pub trait DataPath {
    fn configure(&mut self, rings: DataRings) -> Result<(), DpError>;
    fn transmit(&mut self, packet: TxPacket) -> Result<(), DpError>;
    fn receive(&mut self) -> Result<Option<RxPacket>, DpError>;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peer_id_is_not_a_ring_id() {
        assert_eq!(PeerId(7).0, 7);
    }
}
