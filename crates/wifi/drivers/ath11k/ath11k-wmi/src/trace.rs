//! Normalized differential-oracle trace records.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectReason {
    InvalidArgument,
    Protocol,
    Truncated,
    Unaligned,
}

/// Offsets are measured from byte zero of the command/event TLV body, after
/// the four-byte WMI transport header. Names are stable full source paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceEvent {
    Tlv { tag: u16, len: usize, offset: usize },
    Field { name: &'static str, value: u64 },
    Branch { name: &'static str, taken: bool },
    Reject { reason: RejectReason, offset: usize },
}

pub trait TraceSink {
    fn record(&mut self, event: TraceEvent);
}

/// Useful for generic traced code when the caller does not retain records.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTrace;
impl TraceSink for NoTrace {
    #[inline(always)]
    fn record(&mut self, _event: TraceEvent) {}
}
