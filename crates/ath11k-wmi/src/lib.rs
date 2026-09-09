#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
use alloc::vec::Vec;
pub mod cmd;
pub mod event;
pub mod tags;
pub mod trace;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandId(pub u32);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventId(pub u32);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command {
    pub id: CommandId,
    tlvs: Vec<u8>,
}
impl Command {
    pub fn from_tlvs(id: CommandId, tlvs: Vec<u8>) -> Result<Self, WmiError> {
        check_tlvs(&tlvs)?;
        Ok(Self { id, tlvs })
    }
    pub fn tlvs(&self) -> &[u8] {
        &self.tlvs
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub id: EventId,
    tlvs: Vec<u8>,
}
impl Event {
    pub fn from_tlvs(id: EventId, tlvs: Vec<u8>) -> Result<Self, WmiError> {
        check_tlvs(&tlvs)?;
        Ok(Self { id, tlvs })
    }
    pub fn tlvs(&self) -> &[u8] {
        &self.tlvs
    }
}
fn check_tlvs(bytes: &[u8]) -> Result<(), WmiError> {
    if bytes.len().is_multiple_of(4) {
        Ok(())
    } else {
        Err(WmiError::UnalignedTlv)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WmiError {
    UnalignedTlv,
    Malformed,
    Timeout,
    NoCredits,
    Transport,
}
pub trait Transport {
    /// Whether `send` returning an error guarantees that firmware could not
    /// observe the command. Callers may release DMA referenced by a rejected
    /// command only when this is true.
    const SEND_ERROR_IS_NON_VISIBLE: bool = false;

    fn send(&mut self, command: Command) -> Result<(), WmiError>;
    fn receive(&mut self, deadline_ns: u64) -> Result<Option<Event>, WmiError>;
}
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    #[test]
    fn tlvs_are_word_aligned() {
        assert_eq!(
            Command::from_tlvs(CommandId(1), vec![0; 3]),
            Err(WmiError::UnalignedTlv)
        );
    }
}
