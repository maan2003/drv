//! WMI firmware-to-host event decoders.
use crate::{Event, WmiError};
pub trait EventDecoder {
    type Output;
    fn decode(&self, event: Event) -> Result<Self::Output, WmiError>;
}
