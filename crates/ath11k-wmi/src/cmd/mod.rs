//! WMI host-to-firmware command encoders.
use crate::{Command, WmiError};
pub trait CommandEncoder {
    type Request;
    fn encode(&self, request: &Self::Request) -> Result<Command, WmiError>;
}
