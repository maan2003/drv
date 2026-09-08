// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Owned host allocation for the client MLME frame-building seam.

use std::ops::{Deref, DerefMut, RangeFrom};

pub struct Arena;

impl Arena {
    pub fn new() -> Self {
        Self
    }

    pub fn insert_default_slice<T: Clone + Default>(&self, len: usize) -> ArenaBox<[T]> {
        ArenaBox(vec![T::default(); len].into_boxed_slice())
    }

    pub fn make_static<T: ?Sized>(&self, value: ArenaBox<T>) -> ArenaStaticBox<T> {
        ArenaStaticBox(value.0)
    }
}

pub struct ArenaBox<T: ?Sized>(Box<T>);

impl<T: ?Sized> Deref for ArenaBox<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: ?Sized> DerefMut for ArenaBox<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl ArenaBox<[u8]> {
    /// Transfers the requested suffix into an independently owned frame.
    pub fn into_static_range(self, range: RangeFrom<usize>) -> ArenaStaticBox<[u8]> {
        ArenaStaticBox(self.0[range].to_vec().into_boxed_slice())
    }
}

pub struct ArenaStaticBox<T: ?Sized>(Box<T>);

impl<T: ?Sized> Deref for ArenaStaticBox<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: ?Sized> DerefMut for ArenaStaticBox<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<Vec<u8>> for ArenaStaticBox<[u8]> {
    fn from(value: Vec<u8>) -> Self {
        Self(value.into_boxed_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_transfer_preserves_bytes_and_independent_ownership() {
        let arena = Arena::new();
        let mut allocation = arena.insert_default_slice::<u8>(5);
        allocation.copy_from_slice(&[1, 2, 3, 4, 5]);
        let frame = allocation.into_static_range(2..);
        assert_eq!(&*frame, &[3, 4, 5]);
    }
}
