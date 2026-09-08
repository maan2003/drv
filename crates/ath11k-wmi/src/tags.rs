//! Shared mechanical transcription target for WMI tags and message IDs.
use crate::{CommandId, EventId};
pub const fn command_id(value: u32) -> CommandId {
    CommandId(value)
}
pub const fn event_id(value: u32) -> EventId {
    EventId(value)
}
