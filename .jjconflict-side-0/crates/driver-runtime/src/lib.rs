#![no_std]

//! Device-neutral state machines used by userspace hardware drivers.
//!
//! Bus access belongs in backend crates; register protocols, commands, and
//! descriptors belong in device crates.

extern crate alloc;

use alloc::{format, string::String};

/// A bounded monotonic deadline represented in the clock's native units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Deadline {
    expires_at: u64,
}

impl Deadline {
    pub fn checked(start: u64, timeout: u64) -> Option<Self> {
        Some(Self {
            expires_at: start.checked_add(timeout)?,
        })
    }

    pub const fn expires_at(self) -> u64 {
        self.expires_at
    }

    pub const fn expired(self, now: u64) -> bool {
        now >= self.expires_at
    }

    pub const fn remaining(self, now: u64) -> u64 {
        self.expires_at.saturating_sub(now)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    Partial { completed: u16, total: u16 },
    Complete,
    TimedOut { completed: u16, total: u16 },
}

/// Tracks monotonic progress against one hard deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletionTracker {
    total: u16,
    completed: u16,
    deadline: Deadline,
}

impl CompletionTracker {
    pub fn new(total: u16, start: u64, timeout: u64) -> Option<Self> {
        Some(Self {
            total: (total != 0).then_some(total)?,
            completed: 0,
            deadline: Deadline::checked(start, timeout)?,
        })
    }

    pub fn observe(&mut self, completed: u16, now: u64) -> Option<Completion> {
        if completed < self.completed || completed > self.total {
            return None;
        }
        self.completed = completed;
        if completed == self.total {
            Some(Completion::Complete)
        } else if self.deadline.expired(now) {
            Some(Completion::TimedOut {
                completed,
                total: self.total,
            })
        } else {
            Some(Completion::Partial {
                completed,
                total: self.total,
            })
        }
    }
}

/// Conservative ownership of a resource crossing a hardware publication edge.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PublicationState {
    #[default]
    Local,
    /// Publication completed, but the device has not acknowledged consumption.
    Published,
    Acknowledged,
    /// Publication may have reached hardware; reuse requires containment.
    Uncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationError {
    InvalidTransition,
}

impl PublicationState {
    /// Mark publication ambiguous before entering the operation that exposes it.
    pub fn begin(&mut self) -> Result<(), PublicationError> {
        if *self != Self::Local {
            return Err(PublicationError::InvalidTransition);
        }
        *self = Self::Uncertain;
        Ok(())
    }

    pub fn published(&mut self) -> Result<(), PublicationError> {
        if *self != Self::Uncertain {
            return Err(PublicationError::InvalidTransition);
        }
        *self = Self::Published;
        Ok(())
    }

    pub fn acknowledged(&mut self) -> Result<(), PublicationError> {
        if *self != Self::Published {
            return Err(PublicationError::InvalidTransition);
        }
        *self = Self::Acknowledged;
        Ok(())
    }

    pub const fn requires_containment(self) -> bool {
        matches!(self, Self::Published | Self::Uncertain)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqKind {
    Intx,
    Msi,
    Msix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqCapability {
    pub kind: IrqKind,
    pub count: u32,
    pub eventfd: bool,
}

pub fn select_irq(capabilities: &[IrqCapability]) -> Option<IrqCapability> {
    [IrqKind::Msix, IrqKind::Msi, IrqKind::Intx]
        .into_iter()
        .find_map(|kind| {
            capabilities
                .iter()
                .copied()
                .find(|item| item.kind == kind && item.count != 0 && item.eventfd)
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqLifecycle {
    Uninstalled,
    EventfdInstalled(IrqCapability),
    DeviceSourceEnabled(IrqCapability),
    EventObserved(IrqCapability),
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqLifecycleError {
    InvalidTransition,
    EmptyEvent,
}

impl IrqLifecycle {
    pub fn install(self, capability: IrqCapability) -> Result<Self, IrqLifecycleError> {
        if self != Self::Uninstalled || capability.count == 0 || !capability.eventfd {
            return Err(IrqLifecycleError::InvalidTransition);
        }
        Ok(Self::EventfdInstalled(capability))
    }

    pub fn enable_device_source(self) -> Result<Self, IrqLifecycleError> {
        match self {
            Self::EventfdInstalled(capability) => Ok(Self::DeviceSourceEnabled(capability)),
            _ => Err(IrqLifecycleError::InvalidTransition),
        }
    }

    pub fn observe_event(self, counter: u64) -> Result<Self, IrqLifecycleError> {
        if counter == 0 {
            return Err(IrqLifecycleError::EmptyEvent);
        }
        match self {
            Self::DeviceSourceEnabled(capability) => Ok(Self::EventObserved(capability)),
            _ => Err(IrqLifecycleError::InvalidTransition),
        }
    }

    pub fn disable(self) -> Result<Self, IrqLifecycleError> {
        match self {
            Self::EventfdInstalled(_) | Self::DeviceSourceEnabled(_) | Self::EventObserved(_) => {
                Ok(Self::Disabled)
            }
            _ => Err(IrqLifecycleError::InvalidTransition),
        }
    }

    pub const fn may_unmask_device(self) -> bool {
        matches!(self, Self::EventfdInstalled(_))
    }
}

/// One structured, single-field hardware transcript event.
///
/// Secret values can only be added in redacted form, so callers do not need to
/// retain sensitive data merely to report that a sensitive operation occurred.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptEvent<'a> {
    field: &'a str,
    value: TranscriptValue<'a>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TranscriptValue<'a> {
    Public(&'a str),
    Redacted,
}

impl<'a> TranscriptEvent<'a> {
    pub const fn public(field: &'a str, value: &'a str) -> Self {
        Self {
            field,
            value: TranscriptValue::Public(value),
        }
    }

    pub const fn secret(field: &'a str) -> Self {
        Self {
            field,
            value: TranscriptValue::Redacted,
        }
    }

    pub const fn field(&self) -> &str {
        self.field
    }

    pub fn value(&self) -> &str {
        match self.value {
            TranscriptValue::Public(value) => value,
            TranscriptValue::Redacted => "[redacted]",
        }
    }

    pub fn json(&self) -> String {
        // Existing hardware stage values use an identifier-like vocabulary;
        // escape JSON metacharacters for generic runtime consumers.
        let escaped = self.value().replace('\\', "\\\\").replace('"', "\\\"");
        format!("{{\"{}\":\"{}\"}}", self.field, escaped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_tracks_acknowledged_and_ambiguous_ownership() {
        let mut acknowledged = PublicationState::Local;
        acknowledged.begin().unwrap();
        acknowledged.published().unwrap();
        assert!(acknowledged.requires_containment());
        acknowledged.acknowledged().unwrap();
        assert!(!acknowledged.requires_containment());

        let mut ambiguous = PublicationState::Local;
        ambiguous.begin().unwrap();
        assert_eq!(ambiguous, PublicationState::Uncertain);
        assert!(ambiguous.requires_containment());
    }

    #[test]
    fn deadline_is_hard_and_overflow_is_rejected() {
        let deadline = Deadline::checked(10, 5).unwrap();
        assert_eq!(deadline.remaining(12), 3);
        assert!(!deadline.expired(14));
        assert!(deadline.expired(15));
        assert_eq!(Deadline::checked(u64::MAX, 1), None);
    }

    #[test]
    fn completion_rejects_regression_and_obeys_deadline() {
        let mut tracker = CompletionTracker::new(3, 10, 5).unwrap();
        assert_eq!(
            tracker.observe(1, 14),
            Some(Completion::Partial {
                completed: 1,
                total: 3
            })
        );
        assert_eq!(tracker.observe(0, 14), None);
        assert_eq!(
            tracker.observe(2, 15),
            Some(Completion::TimedOut {
                completed: 2,
                total: 3
            })
        );
    }

    #[test]
    fn transcript_never_retains_or_serializes_secret_values() {
        let secret_value = "sensitive material";
        let event = TranscriptEvent::secret("sensitive_operation");
        assert_eq!(event.value(), "[redacted]");
        assert!(!event.json().contains(secret_value));
        assert_eq!(event.json(), r#"{"sensitive_operation":"[redacted]"}"#);
    }
}
