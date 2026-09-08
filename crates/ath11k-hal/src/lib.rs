// PORT-MAP: reusable
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;

pub mod descriptors;

use alloc::vec::Vec;
use ath11k_platform_backend::{Backend, Bidirectional, CoherentDma};
pub mod reo;
pub mod srng;
pub use reo::{
    PacketNumberType, ReoCommand, ReoCommandKind, ReoCommandParams, ReoQueueDescriptor,
    ReoResources, ReoStatus, ReoStatusHeader, ReoStatusKind, initialize_command_ring,
    setup_wcn6750,
};
pub use srng::{RingDirection, RingFlags, RingType, Srng, SrngParams, Wcn6750Registers};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RingId(pub u16);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RingKind {
    Ce,
    Tcl,
    Reo,
    Wbm,
    Rxdma,
}
/// Generation-tied coherent ring memory. Its device address can only be
/// programmed through hardware-api's checked DeviceAddress path.
pub struct RingMemory<B: Backend> {
    pub dma: CoherentDma<B, Bidirectional>,
    pub entries: u16,
    pub entry_bytes: u16,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Descriptor(Vec<u8>);
impl Descriptor {
    pub fn new(bytes: Vec<u8>, expected_len: usize) -> Result<Self, HalError> {
        if bytes.len() == expected_len {
            Ok(Self(bytes))
        } else {
            Err(HalError::WrongDescriptorLength)
        }
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HalError {
    WrongDescriptorLength,
    NoResources,
    DeviceFault,
    Unsupported,
}
pub trait Rings<B: Backend> {
    fn create(&mut self, kind: RingKind, memory: RingMemory<B>) -> Result<RingId, HalError>;
    /// Writes the descriptor into coherent ring memory. The caller must first
    /// sync any referenced streaming DMA packet for the device; publishing the
    /// new index is then a release-ordered `write_u32` through `Srng::access_end`.
    fn publish(&mut self, ring: RingId, descriptor: Descriptor) -> Result<(), HalError>;
    fn consume(&mut self, ring: RingId) -> Result<Option<Descriptor>, HalError>;
}
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    #[test]
    fn descriptor_length_is_checked() {
        assert_eq!(
            Descriptor::new(vec![0; 3], 4),
            Err(HalError::WrongDescriptorLength)
        );
    }
}
