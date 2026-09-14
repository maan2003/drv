// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host monotonic values for client channel-switch deadline calculation.

pub use zx::{MonotonicDuration, MonotonicInstant};

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Host executor binding for Fuchsia's monotonic deadline timer.
pub struct Timer {
    sleep: Pin<Box<tokio::time::Sleep>>,
    terminated: bool,
}

impl Timer {
    pub fn new(deadline: MonotonicInstant) -> Self {
        let remaining = deadline - MonotonicInstant::now();
        let nanos = remaining.into_nanos().max(0) as u64;
        Self {
            sleep: Box::pin(tokio::time::sleep(std::time::Duration::from_nanos(nanos))),
            terminated: false,
        }
    }
}

impl Future for Timer {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let poll = self.sleep.as_mut().poll(cx);
        if poll.is_ready() {
            self.terminated = true;
        }
        poll
    }
}

impl futures::future::FusedFuture for Timer {
    fn is_terminated(&self) -> bool {
        self.terminated
    }
}

pub trait DurationExt {
    fn after_now(self) -> MonotonicInstant;
}

impl DurationExt for MonotonicDuration {
    fn after_now(self) -> MonotonicInstant {
        MonotonicInstant::now() + self
    }
}
