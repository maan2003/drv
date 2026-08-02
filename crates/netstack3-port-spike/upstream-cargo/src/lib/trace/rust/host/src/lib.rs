// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host observability boundary for Fuchsia's tracing API.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

/// The scope of an instant trace event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    Thread,
    Process,
    Global,
}

/// An identifier correlating asynchronous trace events.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Id(u64);

impl Id {
    pub fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

impl From<u64> for Id {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<Id> for u64 {
    fn from(value: Id) -> Self {
        value.0
    }
}

pub struct Arg<'a>(PhantomData<&'a ()>);

pub trait ArgValue {
    fn of<'a>(_key: &'a str, _value: Self) -> Arg<'a>
    where
        Self: 'a;
}

impl ArgValue for &str {
    fn of<'a>(_key: &'a str, _value: Self) -> Arg<'a>
    where
        Self: 'a,
    {
        Arg(PhantomData)
    }
}

pub struct TraceCategoryContext;

impl TraceCategoryContext {
    pub fn acquire(_category: &'static str) -> Option<Self> {
        None
    }
}

pub fn instant(
    _context: &TraceCategoryContext,
    _name: &'static str,
    _scope: Scope,
    _args: &[Arg<'_>],
) {
}

pub fn async_begin(_id: Id, _category: &'static str, _name: &'static str, _args: &[Arg<'_>]) {}

pub fn async_end(_id: Id, _category: &'static str, _name: &'static str, _args: &[Arg<'_>]) {}

pub fn async_instant(
    _id: Id,
    _context: &TraceCategoryContext,
    _name: &'static str,
    _args: &[Arg<'_>],
) {
}

#[macro_export]
macro_rules! duration_begin {
    ($($arg:tt)*) => {{}};
}

#[macro_export]
macro_rules! duration_end {
    ($($arg:tt)*) => {{}};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip() {
        let first = Id::new();
        let second = Id::new();
        assert_ne!(first, second);
        assert_eq!(u64::from(Id::from(42)), 42);
    }

    #[test]
    fn host_tracing_is_disabled() {
        assert!(TraceCategoryContext::acquire("wlan").is_none());
    }
}
