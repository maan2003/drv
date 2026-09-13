// SPDX-License-Identifier: GPL-2.0-only
//! Connection-attempt ownership, independent of consume-on-read SO_ERROR.
#![forbid(unsafe_code)]
use crate::linux::Address;
use kernel::prelude::*;

#[derive(Clone, Copy)]
pub(crate) struct Names {
    pub(crate) local: Address,
    pub(crate) peer: Address,
}
enum Phase {
    AwaitingAck,
    AwaitingOutcome,
    Complete(Result<Names>),
}
enum Waiter {
    Attached,
    Detached,
}
pub(crate) struct ConnectAttempt {
    id: u64,
    phase: Phase,
    waiter: Waiter,
}
impl ConnectAttempt {
    pub(crate) fn begin(slot: &mut Option<Self>, id: u64, blocking: bool) -> Result {
        if slot.as_ref().is_some_and(Self::blocks_replacement) {
            return Err(EALREADY);
        }
        *slot = Some(Self {
            id,
            phase: Phase::AwaitingAck,
            waiter: if blocking {
                Waiter::Attached
            } else {
                Waiter::Detached
            },
        });
        Ok(())
    }
    pub(crate) fn pending(&self) -> bool {
        !matches!(self.phase, Phase::Complete(_))
    }
    pub(crate) fn blocks_replacement(&self) -> bool {
        self.pending() || matches!(self.waiter, Waiter::Attached)
    }
    pub(crate) fn acknowledge(&mut self, id: u64, tcp: bool, result: Result<Names>) -> Result {
        if id != self.id || !matches!(self.phase, Phase::AwaitingAck) {
            return Err(EPROTO);
        }
        self.phase = if tcp && result.is_ok() {
            Phase::AwaitingOutcome
        } else {
            Phase::Complete(result)
        };
        Ok(())
    }
    pub(crate) fn complete(&mut self, id: u64, result: Result<Names>) -> Result {
        if id != self.id || !matches!(self.phase, Phase::AwaitingOutcome) {
            return Err(EPROTO);
        }
        self.phase = Phase::Complete(result);
        Ok(())
    }
    pub(crate) fn claim(&mut self, id: u64) -> Result<Option<Result<Names>>> {
        if id != self.id || !matches!(self.waiter, Waiter::Attached) {
            return Err(EPROTO);
        }
        match self.phase {
            Phase::Complete(result) => {
                self.waiter = Waiter::Detached;
                Ok(Some(result))
            }
            _ => Ok(None),
        }
    }
    pub(crate) fn detach(&mut self, id: u64) -> Result {
        if id != self.id {
            return Err(EPROTO);
        }
        self.waiter = Waiter::Detached;
        Ok(())
    }
}

#[cfg(CONFIG_KUNIT)]
#[kunit_tests(ns3_connection_attempt)]
mod tests {
    use super::*;
    #[test]
    fn completed_attached_attempt_cannot_be_replaced() {
        let mut slot = None;
        ConnectAttempt::begin(&mut slot, 1, true).unwrap();
        slot.as_mut()
            .unwrap()
            .acknowledge(1, true, Err(ECONNREFUSED))
            .unwrap();
        // A has completed but is deliberately parked before claiming its result.
        assert_eq!(ConnectAttempt::begin(&mut slot, 2, true), Err(EALREADY));
        assert!(matches!(
            slot.as_mut().unwrap().claim(1).unwrap(),
            Some(Err(ECONNREFUSED))
        ));
        ConnectAttempt::begin(&mut slot, 2, true).unwrap();
        assert!(slot.as_mut().unwrap().claim(1).is_err());
    }
    #[test]
    fn detached_attempt_retains_correlation_until_complete() {
        let mut slot = None;
        ConnectAttempt::begin(&mut slot, 1, true).unwrap();
        slot.as_mut().unwrap().detach(1).unwrap();
        assert_eq!(ConnectAttempt::begin(&mut slot, 2, false), Err(EALREADY));
        let names = Names {
            local: Address::default(),
            peer: Address::default(),
        };
        slot.as_mut()
            .unwrap()
            .acknowledge(1, true, Ok(names))
            .unwrap();
        assert_eq!(slot.as_mut().unwrap().complete(2, Ok(names)), Err(EPROTO));
        slot.as_mut().unwrap().complete(1, Ok(names)).unwrap();
        assert_eq!(slot.as_mut().unwrap().complete(1, Ok(names)), Err(EPROTO));
        ConnectAttempt::begin(&mut slot, 2, false).unwrap();
    }
}
